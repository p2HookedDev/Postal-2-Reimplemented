use p2_math::{Rotator, Vector};

use crate::interp::{float, int, truthy};

use super::*;

impl MemLayouts {
    pub(crate) fn read_field(&self, field: &Field, mem: &Mem) -> Value {
        self.read(field.shape, field.dim, mem, field.word as usize, field.heap as usize)
    }

    #[inline]
    pub(crate) fn read(&self, shape: Shape, dim: u32, mem: &Mem, word: usize, heap: usize) -> Value {
        if dim == 1 {
            let value = scalar(mem.words.get(word).copied().unwrap_or(0), shape);
            if !matches!(value, Value::Map) {
                return value;
            }
        }
        self.read_compound(shape, dim, mem, word, heap)
    }

    fn read_compound(&self, shape: Shape, dim: u32, mem: &Mem, word: usize, heap: usize) -> Value {
        if dim > 1 {
            let (w, h) = self.size(shape);
            return Value::Array((0..dim as usize).map(|i| self.read(shape, 1, mem, word + i * w as usize, heap + i * h as usize)).collect());
        }
        let at = |i: usize| mem.words.get(word + i).copied().unwrap_or(0);
        match shape {
            Shape::Byte | Shape::Int | Shape::Bool | Shape::Float | Shape::Object | Shape::Name => scalar(at(0), shape),
            Shape::Delegate => Value::Delegate { object: object(at(0)), function: NameId(at(1)) },
            Shape::Str => match mem.heap.get(heap) {
                Some(Heap::Str(text)) => Value::Str(text.clone()),
                _ => Value::Str(String::new()),
            },
            Shape::Array(elem) => match mem.heap.get(heap) {
                Some(Heap::Array(array)) => {
                    let field = self.get(elem).fields[0];
                    let (w, h) = self.element_size(elem);
                    Value::Array(
                        (0..array.len as usize)
                            .map(|i| self.read(field.shape, field.dim, &array.mem, i * w as usize, i * h as usize))
                            .collect(),
                    )
                }
                _ => Value::Array(Vec::new()),
            },
            Shape::Struct(id) => {
                let layout = self.get(id);
                let member = |i: usize| at(layout.fields[i].word as usize);
                match layout.kind {
                    StructKind::Vector => Value::Vector(Vector::new(f32::from_bits(member(0)), f32::from_bits(member(1)), f32::from_bits(member(2)))),
                    StructKind::Rotator => Value::Rotator(Rotator::new(member(0) as i32, member(1) as i32, member(2) as i32)),
                    StructKind::Other => Value::Struct(
                        layout.fields.iter().map(|f| self.read(f.shape, f.dim, mem, word + f.word as usize, heap + f.heap as usize)).collect(),
                    ),
                }
            }
            Shape::Fixed(id) => {
                let f = self.get(id).fields[0];
                Value::Array((0..f.dim as usize).map(|i| self.read(f.shape, 1, mem, word + i * f.words as usize, heap + i * f.heaps as usize)).collect())
            }
            Shape::Map => Value::Map,
        }
    }

    pub(crate) fn write_field(&self, field: &Field, mem: &mut Mem, value: Value) {
        self.write(field.shape, field.dim, mem, field.word as usize, field.heap as usize, value);
    }

    /// Converted to the shape's type: numbers to numbers, anything else of another kind to the zero value (as the engine's
    /// memory holds what a typed copy leaves there).
    pub(crate) fn write(&self, shape: Shape, dim: u32, mem: &mut Mem, word: usize, heap: usize, value: Value) {
        if dim > 1 {
            let (w, h) = self.size(shape);
            let mut items = match value {
                Value::Array(items) => items.into_iter(),
                _ => Vec::new().into_iter(),
            };
            for i in 0..dim as usize {
                let item = items.next().unwrap_or(Value::Map);
                self.write(shape, 1, mem, word + i * w as usize, heap + i * h as usize, item);
            }
            return;
        }
        let set = |mem: &mut Mem, i: usize, bits: u32| {
            if let Some(slot) = mem.words.get_mut(word + i) {
                *slot = bits;
            }
        };
        match shape {
            Shape::Byte => set(mem, 0, u32::from(int(&value) as u8)),
            Shape::Int => set(mem, 0, int(&value) as u32),
            Shape::Bool => set(mem, 0, u32::from(truthy(&value))),
            Shape::Float => set(mem, 0, float(&value).to_bits()),
            Shape::Object => set(mem, 0, match value {
                Value::Object(object) => object_word(object),
                _ => 0,
            }),
            Shape::Name => set(mem, 0, match value {
                Value::Name(name) => name.0,
                _ => 0,
            }),
            Shape::Delegate => {
                let (object, function) = match value {
                    Value::Delegate { object, function } => (object, function),
                    _ => (None, NameId::NONE),
                };
                set(mem, 0, object_word(object));
                set(mem, 1, function.0);
            }
            Shape::Str => {
                if let Some(slot) = mem.heap.get_mut(heap) {
                    *slot = Heap::Str(match value {
                        Value::Str(text) => text,
                        _ => String::new(),
                    });
                }
            }
            Shape::Array(elem) => {
                let items = match value {
                    Value::Array(items) => items,
                    _ => Vec::new(),
                };
                let mut array = Array::default();
                self.resize(elem, &mut array, items.len());
                let field = self.get(elem).fields[0];
                let (w, h) = self.element_size(elem);
                for (i, item) in items.into_iter().enumerate() {
                    self.write(field.shape, field.dim, &mut array.mem, i * w as usize, i * h as usize, item);
                }
                if let Some(slot) = mem.heap.get_mut(heap) {
                    *slot = Heap::Array(array);
                }
            }
            Shape::Struct(id) => {
                let layout = self.get(id);
                match (layout.kind, &value) {
                    (StructKind::Vector, Value::Vector(v)) => {
                        for (f, c) in layout.fields.iter().zip(<[f32; 3]>::from(*v)) {
                            set(mem, f.word as usize, c.to_bits());
                        }
                        return;
                    }
                    (StructKind::Rotator, Value::Rotator(r)) => {
                        for (f, c) in layout.fields.iter().zip(<[i32; 3]>::from(*r)) {
                            set(mem, f.word as usize, c as u32);
                        }
                        return;
                    }
                    _ => {}
                }
                let (three, mut members) = match value {
                    Value::Vector(v) => (Some(<[f32; 3]>::from(v).map(Value::Float)), Vec::new().into_iter()),
                    Value::Rotator(r) => (Some(<[i32; 3]>::from(r).map(Value::Int)), Vec::new().into_iter()),
                    Value::Struct(members) => (None, members.into_iter()),
                    _ => (None, Vec::new().into_iter()),
                };
                let mut three = three.into_iter().flatten();
                for f in &self.get(id).fields {
                    let member = three.next().or_else(|| members.next()).unwrap_or(Value::Map);
                    self.write(f.shape, f.dim, mem, word + f.word as usize, heap + f.heap as usize, member);
                }
            }
            Shape::Fixed(id) => {
                let f = self.get(id).fields[0];
                let mut items = match value {
                    Value::Array(items) => items.into_iter(),
                    _ => Vec::new().into_iter(),
                };
                for i in 0..f.dim as usize {
                    let item = items.next().unwrap_or(Value::Map);
                    self.write(f.shape, 1, mem, word + i * f.words as usize, heap + i * f.heaps as usize, item);
                }
            }
            Shape::Map => {}
        }
    }

    pub(crate) fn write_element(&self, field: &Field, mem: &mut Mem, index: usize, value: Value) {
        match field.shape {
            _ if field.dim > 1 => {
                if index < field.dim as usize {
                    let (word, heap) = (field.word as usize + index * field.words as usize, field.heap as usize + index * field.heaps as usize);
                    self.write(field.shape, 1, mem, word, heap, value);
                }
            }
            Shape::Array(elem) if !matches!(value, Value::Array(_)) => {
                if index >= MAX_TAGGED_ELEMENTS {
                    return;
                }
                let Some(Heap::Array(array)) = mem.heap.get_mut(field.heap as usize) else { return };
                if array.len as usize <= index {
                    self.resize(elem, array, index + 1);
                }
                let f = self.get(elem).fields[0];
                let (w, h) = self.element_size(elem);
                self.write(f.shape, f.dim, &mut array.mem, index * w as usize, index * h as usize, value);
            }
            _ => self.write_field(field, mem, value),
        }
    }
}
