use std::sync::Arc;

use p2_script::{StructLayout, Type};

use rustc_hash::FxHashMap;

use crate::{NameId, ObjectId, Value};

mod values;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutId(u32);

impl LayoutId {
    pub(crate) const NONE: Self = Self(u32::MAX);
}

/// An array tag past this index is treated as bad and dropped; the limit is ours (unverified).
pub(crate) const MAX_TAGGED_ELEMENTS: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shape {
    Byte,
    Int,
    Bool,
    Float,
    /// 0 = None, else the object's id + 1.
    Object,
    Name,
    /// Two words: the object (as [`Shape::Object`]) and the function name.
    Delegate,
    Str,
    Array(LayoutId),
    Struct(LayoutId),
    Fixed(LayoutId),
    Map,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StructKind {
    Vector,
    Rotator,
    Other,
}

impl StructKind {
    pub(crate) fn of(layout: &StructLayout) -> Self {
        let three = |scalar: fn(&Type) -> bool| {
            layout.properties.len() == 3 && layout.properties.iter().all(|p| p.array_dim <= 1 && scalar(&p.ty))
        };
        if layout.name.eq_ignore_ascii_case("Vector") && three(|ty| matches!(ty, Type::Float)) {
            Self::Vector
        } else if layout.name.eq_ignore_ascii_case("Rotator") && three(|ty| matches!(ty, Type::Int)) {
            Self::Rotator
        } else {
            Self::Other
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub word: u32,
    pub heap: u32,
    pub dim: u32,
    pub shape: Shape,
    pub words: u32,
    pub heaps: u32,
}

#[derive(Debug)]
pub(crate) struct MemLayout {
    pub fields: Vec<Field>,
    pub words: u32,
    pub heaps: u32,
    /// Word offsets and shapes of the first three members when they are one-word scalars (else `Shape::Map`): a
    /// vector's X, Y, Z, a rotator's pitch, yaw, roll, read without going through `fields`.
    pub head: [(u32, Shape); 3],
    pub kind: StructKind,
    zero_heap: Vec<Heap>,
    _source: Option<Arc<StructLayout>>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mem {
    pub(crate) words: Vec<u32>,
    pub(crate) heap: Vec<Heap>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Heap {
    Str(String),
    Array(Array),
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Array {
    pub len: u32,
    pub mem: Mem,
}

#[derive(Debug, Default)]
pub(crate) struct MemLayouts {
    list: Vec<MemLayout>,
    by_source: FxHashMap<usize, LayoutId>,
    scalar_elements: FxHashMap<Shape, LayoutId>,
}

impl MemLayouts {
    pub(crate) fn get(&self, id: LayoutId) -> &MemLayout {
        &self.list[id.0 as usize]
    }

    pub(crate) fn of(&mut self, layout: &Arc<StructLayout>) -> LayoutId {
        self.of_class(layout, None)
    }

    /// The last properties of `layout` are the superclass's (`base`, of `base_count` properties):
    /// they keep its offsets and the class's own come after them.
    pub(crate) fn of_class(&mut self, layout: &Arc<StructLayout>, base: Option<(LayoutId, usize)>) -> LayoutId {
        let key = Arc::as_ptr(layout) as usize;
        if let Some(&id) = self.by_source.get(&key) {
            return id;
        }
        let own_count = base.map_or(layout.properties.len(), |(_, count)| layout.properties.len().saturating_sub(count));
        let (mut words, mut heaps) = base.map_or((0, 0), |(id, _)| (self.get(id).words, self.get(id).heaps));
        let mut fields = Vec::with_capacity(layout.properties.len());
        for property in &layout.properties[..own_count] {
            let shape = self.shape(&property.ty);
            let (w, h) = self.size(shape);
            let dim = property.array_dim.max(1);
            fields.push(Field { word: words, heap: heaps, dim, shape, words: w, heaps: h });
            words += w * dim;
            heaps += h * dim;
        }
        if let Some((id, _)) = base {
            fields.extend(self.get(id).fields.iter().copied());
        }
        let id = self.push(fields, words, heaps, Some(Arc::clone(layout)));
        self.by_source.insert(key, id);
        id
    }

    fn push(&mut self, fields: Vec<Field>, words: u32, heaps: u32, source: Option<Arc<StructLayout>>) -> LayoutId {
        let mut zero_heap = vec![Heap::Str(String::new()); heaps as usize];
        for field in &fields {
            for element in 0..field.dim {
                let at = (field.heap + element * field.heaps) as usize;
                if let Some(slots) = zero_heap.get_mut(at..at + field.heaps as usize) {
                    self.zero_heap_of(field.shape, slots);
                }
            }
        }
        let head = std::array::from_fn(|i| match fields.get(i) {
            Some(f) if f.dim == 1 && (f.words, f.heaps) == (1, 0) => (f.word, f.shape),
            _ => (0, Shape::Map),
        });
        let kind = source.as_deref().map_or(StructKind::Other, StructKind::of);
        let id = LayoutId(self.list.len() as u32);
        self.list.push(MemLayout { fields, words, heaps, head, kind, zero_heap, _source: source });
        id
    }

    fn zero_heap_of(&self, shape: Shape, slots: &mut [Heap]) {
        match shape {
            Shape::Str => slots[0] = Heap::Str(String::new()),
            Shape::Array(_) => slots[0] = Heap::Array(Array::default()),
            Shape::Struct(id) | Shape::Fixed(id) => slots.clone_from_slice(&self.get(id).zero_heap),
            _ => {}
        }
    }

    fn shape(&mut self, ty: &Type) -> Shape {
        match ty {
            Type::Byte => Shape::Byte,
            Type::Int => Shape::Int,
            Type::Bool => Shape::Bool,
            Type::Float => Shape::Float,
            Type::Object | Type::Class => Shape::Object,
            Type::Name => Shape::Name,
            Type::Delegate => Shape::Delegate,
            Type::Str | Type::String => Shape::Str,
            Type::Map => Shape::Map,
            Type::Struct(layout) => Shape::Struct(self.of(layout)),
            Type::Array(inner) => Shape::Array(self.element(inner, 1)),
            Type::FixedArray(inner, count) => Shape::Fixed(self.element(inner, *count)),
        }
    }

    fn element(&mut self, ty: &Type, dim: u32) -> LayoutId {
        let shape = self.shape(ty);
        let scalar = dim == 1
            && matches!(shape, Shape::Byte | Shape::Int | Shape::Bool | Shape::Float | Shape::Object | Shape::Name | Shape::Str | Shape::Delegate);
        if scalar && let Some(id) = self.scalar_elements.get(&shape) {
            return *id;
        }
        let (words, heaps) = self.size(shape);
        let dim = dim.max(1);
        let id = self.push(vec![Field { word: 0, heap: 0, dim, shape, words, heaps }], words * dim, heaps * dim, None);
        if scalar {
            self.scalar_elements.insert(shape, id);
        }
        id
    }

    pub(crate) fn size(&self, shape: Shape) -> (u32, u32) {
        match shape {
            Shape::Byte | Shape::Int | Shape::Bool | Shape::Float | Shape::Object | Shape::Name => (1, 0),
            Shape::Delegate => (2, 0),
            Shape::Str | Shape::Array(_) => (0, 1),
            Shape::Struct(id) | Shape::Fixed(id) => (self.get(id).words, self.get(id).heaps),
            Shape::Map => (0, 0),
        }
    }

    pub(crate) fn zero(&self, id: LayoutId) -> Mem {
        let layout = self.get(id);
        Mem { words: vec![0; layout.words as usize], heap: layout.zero_heap.clone() }
    }

    pub(crate) fn reset(&self, id: LayoutId, mem: &mut Mem) {
        let layout = self.get(id);
        mem.words.clear();
        mem.words.resize(layout.words as usize, 0);
        mem.heap.clone_from(&layout.zero_heap);
    }

    pub(crate) fn element_size(&self, elem: LayoutId) -> (u32, u32) {
        let layout = self.get(elem);
        (layout.words, layout.heaps)
    }

    pub(crate) fn resize(&self, elem: LayoutId, array: &mut Array, len: usize) {
        let layout = self.get(elem);
        let (words, heaps) = (layout.words as usize, layout.heaps as usize);
        array.mem.words.resize(len * words, 0);
        if heaps > 0 {
            let old = array.len as usize;
            array.mem.heap.truncate(len * heaps);
            for _ in old.min(len)..len {
                array.mem.heap.extend(layout.zero_heap.iter().cloned());
            }
        }
        array.len = len as u32;
    }

    pub(crate) fn insert(&self, elem: LayoutId, array: &mut Array, at: usize, count: usize) {
        let layout = self.get(elem);
        let (words, heaps) = (layout.words as usize, layout.heaps as usize);
        let at = at.min(array.len as usize);
        array.mem.words.splice(at * words..at * words, std::iter::repeat_n(0, count * words));
        if heaps > 0 {
            let fill: Vec<Heap> = (0..count).flat_map(|_| layout.zero_heap.iter().cloned()).collect();
            array.mem.heap.splice(at * heaps..at * heaps, fill);
        }
        array.len += count as u32;
    }

    pub(crate) fn remove(&self, elem: LayoutId, array: &mut Array, at: usize, count: usize) {
        let layout = self.get(elem);
        let (words, heaps) = (layout.words as usize, layout.heaps as usize);
        let len = array.len as usize;
        let at = at.min(len);
        let end = (at + count).min(len);
        array.mem.words.drain(at * words..end * words);
        array.mem.heap.drain(at * heaps..end * heaps);
        array.len = (len - (end - at)) as u32;
    }

    pub(crate) fn forget(&self, id: LayoutId, mem: &mut Mem, gone: &impl Fn(ObjectId) -> bool) {
        self.visit_refs(id, mem, &mut |shape, word| {
            if shape == Shape::Object && object(*word).is_some_and(gone) {
                *word = 0;
            }
        });
    }

    /// A save file's lengths are not trusted.
    pub(crate) fn arrays_fit(&self, id: LayoutId, mem: &Mem) -> bool {
        self.fits_at(Shape::Struct(id), mem, 0, 0)
    }

    fn fits_at(&self, shape: Shape, mem: &Mem, word: usize, heap: usize) -> bool {
        match shape {
            Shape::Struct(id) | Shape::Fixed(id) => self.get(id).fields.iter().all(|field| {
                (0..field.dim as usize).all(|element| {
                    let w = word + field.word as usize + element * field.words as usize;
                    let h = heap + field.heap as usize + element * field.heaps as usize;
                    self.fits_at(field.shape, mem, w, h)
                })
            }),
            Shape::Array(elem) => {
                let Some(Heap::Array(array)) = mem.heap.get(heap) else { return true };
                let (w, h) = self.element_size(elem);
                let (w, h, len) = (w as usize, h as usize, array.len as usize);
                array.mem.words.len() >= len * w && array.mem.heap.len() >= len * h && (0..len).all(|i| self.fits_at(Shape::Struct(elem), &array.mem, i * w, i * h))
            }
            _ => true,
        }
    }

    pub(crate) fn visit_refs(&self, id: LayoutId, mem: &mut Mem, f: &mut impl FnMut(Shape, &mut u32)) {
        self.visit_at(Shape::Struct(id), mem, 0, 0, f);
    }

    fn visit_at(&self, shape: Shape, mem: &mut Mem, word: usize, heap: usize, f: &mut impl FnMut(Shape, &mut u32)) {
        match shape {
            Shape::Object | Shape::Name => {
                if let Some(slot) = mem.words.get_mut(word) {
                    f(shape, slot);
                }
            }
            Shape::Delegate => {
                self.visit_at(Shape::Object, mem, word, heap, f);
                self.visit_at(Shape::Name, mem, word + 1, heap, f);
            }
            Shape::Struct(id) | Shape::Fixed(id) => {
                for field in &self.get(id).fields {
                    for element in 0..field.dim as usize {
                        let w = word + field.word as usize + element * field.words as usize;
                        let h = heap + field.heap as usize + element * field.heaps as usize;
                        self.visit_at(field.shape, mem, w, h, f);
                    }
                }
            }
            Shape::Array(elem) => {
                let Some(Heap::Array(array)) = mem.heap.get_mut(heap) else { return };
                let (w, h) = self.element_size(elem);
                for i in 0..array.len as usize {
                    self.visit_at(Shape::Struct(elem), &mut array.mem, i * w as usize, i * h as usize, f);
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn scalar(word: u32, shape: Shape) -> Value {
    match shape {
        Shape::Byte => Value::Byte(word as u8),
        Shape::Int => Value::Int(word as i32),
        Shape::Bool => Value::Bool(word != 0),
        Shape::Float => Value::Float(f32::from_bits(word)),
        Shape::Object => Value::Object(object(word)),
        Shape::Name => Value::Name(NameId(word)),
        _ => Value::Map,
    }
}

pub(crate) fn object(word: u32) -> Option<ObjectId> {
    word.checked_sub(1).map(ObjectId)
}

pub(crate) fn object_word(object: Option<ObjectId>) -> u32 {
    object.map_or(0, |o| o.0 + 1)
}

#[cfg(test)]
mod tests {
    use p2_math::{Rotator, Vector};
    use p2_script::FieldLayout;

    use super::*;

    fn field(name: &str, ty: Type, array_dim: u32) -> FieldLayout {
        FieldLayout { name: Arc::from(name), array_dim, flags: 0, ty }
    }

    fn vector() -> Arc<StructLayout> {
        let properties = ["X", "Y", "Z"].map(|n| field(n, Type::Float, 1)).to_vec();
        Arc::new(StructLayout::new("Vector".to_owned(), properties, 0, None))
    }

    fn sample() -> Arc<StructLayout> {
        let inner = Arc::new(StructLayout::new("Item".to_owned(), vec![field("Label", Type::Str, 1), field("At", Type::Struct(vector()), 1), field("Tags", Type::Array(Box::new(Type::Name)), 1)], 0, None));
        Arc::new(StructLayout::new("Sample".to_owned(), vec![
                field("Count", Type::Int, 1),
                field("Flags", Type::Bool, 3),
                field("Location", Type::Struct(vector()), 1),
                field("Name", Type::Str, 1),
                field("Items", Type::Array(Box::new(Type::Struct(inner))), 1),
                field("Owner", Type::Object, 1),
                field("Speed", Type::Float, 1),
            ], 0, None))
    }

    #[test]
    fn offsets_follow_one_another_with_structs_inline() {
        let mut layouts = MemLayouts::default();
        let id = layouts.of(&sample());
        let layout = layouts.get(id);
        let offsets: Vec<(u32, u32)> = layout.fields.iter().map(|f| (f.word, f.heap)).collect();
        assert_eq!(offsets, [(0, 0), (1, 0), (4, 0), (7, 0), (7, 1), (7, 2), (8, 2)]);
        assert_eq!((layout.words, layout.heaps), (9, 2));
        let zero = layouts.zero(id);
        assert_eq!(zero.words, vec![0; 9]);
        assert_eq!(zero.heap, [Heap::Str(String::new()), Heap::Array(Array::default())]);
    }

    #[test]
    fn values_round_trip() {
        let mut layouts = MemLayouts::default();
        let id = layouts.of(&sample());
        let mut mem = layouts.zero(id);
        let item = |label: &str, x: f32, tags: &[u32]| {
            Value::Struct(vec![
                Value::Str(label.to_owned()),
                Value::Vector(Vector::new(x, 2.0, -3.5)),
                Value::Array(tags.iter().map(|&t| Value::Name(NameId(t))).collect()),
            ])
        };
        let values = [
            Value::Int(-7),
            Value::Array(vec![Value::Bool(true), Value::Bool(false), Value::Bool(true)]),
            Value::Vector(Vector::new(1.5, -0.0, 1e30)),
            Value::Str("dude".to_owned()),
            Value::Array(vec![item("a", 1.0, &[3, 4]), item("b", 9.0, &[])]),
            Value::Object(Some(ObjectId(0))),
            Value::Float(f32::MIN_POSITIVE),
        ];
        let fields = layouts.get(id).fields.clone();
        for (field, value) in fields.iter().zip(values.clone()) {
            layouts.write_field(field, &mut mem, value);
        }
        for (field, value) in fields.iter().zip(values) {
            assert_eq!(layouts.read_field(field, &mem), value);
        }
    }

    #[test]
    fn writes_convert_to_the_declared_type() {
        let mut layouts = MemLayouts::default();
        let id = layouts.of(&sample());
        let mut mem = layouts.zero(id);
        let fields = layouts.get(id).fields.clone();
        layouts.write_field(&fields[6], &mut mem, Value::Int(3));
        assert_eq!(layouts.read_field(&fields[6], &mem), Value::Float(3.0));
        layouts.write_field(&fields[0], &mut mem, Value::Float(2.9));
        assert_eq!(layouts.read_field(&fields[0], &mem), Value::Int(2));
        layouts.write_field(&fields[2], &mut mem, Value::Int(0));
        assert_eq!(layouts.read_field(&fields[2], &mem), Value::Vector(Vector::ZERO));
        layouts.write_field(&fields[5], &mut mem, Value::Int(0));
        assert_eq!(layouts.read_field(&fields[5], &mem), Value::Object(None));
    }

    #[test]
    fn vectors_and_rotators_read_inline() {
        let mut layouts = MemLayouts::default();
        let ints = ["Pitch", "Yaw", "Roll"].map(|n| field(n, Type::Int, 1)).to_vec();
        let rotator = Arc::new(StructLayout::new("Rotator".to_owned(), ints.clone(), 0, None));
        let other = Arc::new(StructLayout::new("Triple".to_owned(), ints, 0, None));
        let outer = Arc::new(StructLayout::new("Outer".to_owned(), vec![field("At", Type::Struct(vector()), 1), field("Facing", Type::Struct(rotator), 1), field("Other", Type::Struct(other), 1)], 0, None));
        let id = layouts.of(&outer);
        let mut mem = layouts.zero(id);
        let fields = layouts.get(id).fields.clone();
        layouts.write_field(&fields[0], &mut mem, Value::Struct(vec![Value::Float(1.0), Value::Int(2), Value::Float(3.5)]));
        assert_eq!(layouts.read_field(&fields[0], &mem), Value::Vector(Vector::new(1.0, 2.0, 3.5)));
        layouts.write_field(&fields[1], &mut mem, Value::Rotator(Rotator::new(-1, 16384, 70000)));
        assert_eq!(layouts.read_field(&fields[1], &mem), Value::Rotator(Rotator::new(-1, 16384, 70000)));
        layouts.write_field(&fields[1], &mut mem, Value::Vector(Vector::new(1.9, -2.5, 3.0)));
        assert_eq!(layouts.read_field(&fields[1], &mem), Value::Rotator(Rotator::new(1, -2, 3)));
        layouts.write_field(&fields[2], &mut mem, Value::Rotator(Rotator::new(4, 5, 6)));
        assert_eq!(layouts.read_field(&fields[2], &mem), Value::Struct(vec![Value::Int(4), Value::Int(5), Value::Int(6)]));
    }

    #[test]
    fn vec3_and_vector_round_trip() {
        let v = Vector::new(1.5, -2.0, 1e30);
        assert_eq!(crate::natives::vec3(Some(Value::Vector(v))), v);
        let r = Rotator::new(-5, 65535, 1 << 25);
        assert_eq!(crate::natives::rot3(Some(Value::Rotator(r))), r);
        assert_eq!(crate::natives::vec3(Some(Value::Struct(vec![Value::Float(1.0), Value::Int(2), Value::Float(3.0)]))), Vector::new(1.0, 2.0, 3.0));
        const { assert!(size_of::<Value>() <= 32) };
    }

    #[test]
    fn arrays_grow_shrink_and_forget() {
        let mut layouts = MemLayouts::default();
        let id = layouts.of(&sample());
        let mut mem = layouts.zero(id);
        let items = layouts.get(id).fields[4];
        let Shape::Array(elem) = items.shape else { panic!("array") };
        let Heap::Array(array) = &mut mem.heap[items.heap as usize] else { panic!("array heap") };
        layouts.resize(elem, array, 2);
        layouts.insert(elem, array, 1, 2);
        assert_eq!((array.len, array.mem.words.len(), array.mem.heap.len()), (4, 12, 8));
        layouts.remove(elem, array, 0, 3);
        assert_eq!((array.len, array.mem.words.len(), array.mem.heap.len()), (1, 3, 2));
        let owner = layouts.get(id).fields[5];
        layouts.write_field(&owner, &mut mem, Value::Object(Some(ObjectId(4))));
        layouts.forget(id, &mut mem, &|o| o == ObjectId(4));
        assert_eq!(layouts.read_field(&owner, &mem), Value::Object(None));
    }

    #[test]
    fn arrays_fit_checks_lengths() {
        let mut layouts = MemLayouts::default();
        let id = layouts.of(&sample());
        let mut mem = layouts.zero(id);
        let items = layouts.get(id).fields[4];
        let Shape::Array(elem) = items.shape else { panic!("array") };
        let Heap::Array(array) = &mut mem.heap[items.heap as usize] else { panic!("array heap") };
        layouts.resize(elem, array, 2);
        assert!(layouts.arrays_fit(id, &mem));
        let Heap::Array(array) = &mut mem.heap[items.heap as usize] else { panic!("array heap") };
        array.len = u32::MAX;
        assert!(!layouts.arrays_fit(id, &mem));
    }

    #[test]
    fn subclass_keeps_the_superclass_offsets() {
        let mut layouts = MemLayouts::default();
        let base = vector();
        let base_id = layouts.of(&base);
        let mut properties = vec![field("W", Type::Float, 1)];
        properties.extend(base.properties.iter().cloned());
        let derived = Arc::new(StructLayout::new("Plane".to_owned(), properties, 0, None));
        let id = layouts.of_class(&derived, Some((base_id, 3)));
        let words: Vec<u32> = layouts.get(id).fields.iter().map(|f| f.word).collect();
        assert_eq!(words, [3, 0, 1, 2]);
    }
}
