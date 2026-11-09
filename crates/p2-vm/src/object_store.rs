use p2_math::{Rotator, Vector};
use p2_package::PackageId;
use p2_script::Type;
use rustc_hash::FxHashMap;

use crate::enums::EPhysics;
use crate::interp::{float, int};
use crate::{Class, ClassId, ClassName, NameId, Object, ObjectId, Prop, PropertyName, Value, actors, lower_lookup, lowercase_index, memory, profiling};

#[derive(Default)]
pub struct ObjectStore {
    pub(crate) names: Vec<String>,
    pub(crate) name_ids: FxHashMap<String, NameId>,
    pub(crate) objects: Vec<Object>,
    pub(crate) object_ids: FxHashMap<String, ObjectId>,
    pub(crate) actor_ids: std::collections::BTreeSet<ObjectId>,
    pub(crate) dynamic_ids: actors::DynamicActors,
    pub(crate) classes: Vec<Class>,
    pub(crate) class_ids: FxHashMap<(PackageId, usize), ClassId>,
    pub(crate) class_by_name: FxHashMap<NameId, ClassId>,
    pub(crate) class_exports: FxHashMap<PackageId, FxHashMap<String, usize>>,
    pub(crate) name_counters: FxHashMap<ClassId, u32>,
    pub(crate) class_redirects: Vec<(ClassId, ClassId)>,
    pub(crate) memory: memory::MemLayouts,
    pub(crate) work: profiling::Counters,
    /// `Diagnostics::count_work` as of the start of the last level tick: the store cannot see `Vm`'s diagnostics.
    pub(crate) count_work: bool,
}

impl ObjectStore {
    #[inline]
    pub fn name(&self, id: NameId) -> &str {
        &self.names[id.0 as usize]
    }

    #[inline]
    pub fn object(&self, id: ObjectId) -> &Object {
        &self.objects[id.0 as usize]
    }

    #[inline]
    pub fn class(&self, id: ClassId) -> &Class {
        &self.classes[id.0 as usize]
    }

    pub fn property_index(&self, class: ClassId, name: &str) -> Option<usize> {
        self.count_property(name);
        let class = self.class(class);
        class.index.get(name).copied().or_else(|| {
            self.count(profiling::Work::PropertyScan);
            lower_lookup(class.lower_index.get_or_init(|| lowercase_index(&class.layout)), name)
        })
    }

    pub(crate) fn field(&self, object: ObjectId, property: impl PropertyName) -> Option<memory::Field> {
        property.field_in(self, self.object(object).class?)
    }

    pub fn get(&self, object: ObjectId, property: impl PropertyName) -> Option<Value> {
        if property.is_resolved()
            && let Some((word, shape)) = self.word(object, property)
        {
            return Some(memory::scalar(word, shape));
        }
        let field = self.field(object, property)?;
        Some(self.memory.read_field(&field, &self.object(object).mem))
    }

    #[inline]
    pub(crate) fn word(&self, object: ObjectId, property: impl PropertyName) -> Option<(u32, memory::Shape)> {
        let object_ref = self.object(object);
        let (word, shape) = property.word_in(self, object_ref.class?)?;
        match shape {
            memory::Shape::Byte | memory::Shape::Int | memory::Shape::Bool | memory::Shape::Float | memory::Shape::Object | memory::Shape::Name => {
                Some((*object_ref.mem.words.get(word as usize)?, shape))
            }
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn member_words<const N: usize>(&self, object: ObjectId, property: impl PropertyName) -> Option<[(u32, memory::Shape); N]> {
        const { assert!(N <= 3, "member_words reaches a struct's first three members (`MemLayout::head`)") };
        let object_ref = self.object(object);
        let (base, memory::Shape::Struct(id)) = property.word_in(self, object_ref.class?)? else { return None };
        let head = &self.memory.get(id).head;
        let words = &object_ref.mem.words;
        Some(std::array::from_fn(|i| match head.get(i) {
            Some(&(word, shape)) if shape != memory::Shape::Map => (words.get((base + word) as usize).copied().unwrap_or(0), shape),
            _ => (0, memory::Shape::Map),
        }))
    }

    /// R, G, B, A (`Object.uc` declares B, G, R, A), read in place. All four: [`Self::member_words`] reaches only the first
    /// three.
    pub(crate) fn color_of(&self, object: ObjectId, property: impl PropertyName) -> Option<[u8; 4]> {
        let field = self.field(object, property)?;
        let (memory::Shape::Struct(id), 1) = (field.shape, field.dim) else { return None };
        let (members, words) = (&self.memory.get(id).fields, &self.object(object).mem.words);
        let byte = |i: usize| members.get(i).and_then(|m| words.get((field.word + m.word) as usize)).map(|&word| word as u8);
        Some([byte(2)?, byte(1)?, byte(0)?, byte(3)?])
    }

    pub(crate) fn members<const N: usize>(&self, object: ObjectId, property: impl PropertyName) -> Option<[Value; N]> {
        Some(self.member_words::<N>(object, property)?.map(|(word, shape)| memory::scalar(word, shape)))
    }

    pub(crate) fn set_members<const N: usize>(&mut self, object: ObjectId, property: impl PropertyName, values: [Value; N]) -> bool {
        let Some(field) = self.field(object, property) else { return false };
        let (memory::Shape::Struct(id), 1) = (field.shape, field.dim) else { return false };
        let members = &self.memory.get(id).fields;
        let mem = &mut self.objects[object.0 as usize].mem;
        for (m, value) in members.iter().zip(values) {
            self.memory.write(m.shape, m.dim, mem, (field.word + m.word) as usize, (field.heap + m.heap) as usize, value);
        }
        true
    }

    pub(crate) fn array_words(&self, object: ObjectId, property: impl PropertyName) -> Option<&[u32]> {
        let field = self.field(object, property)?;
        let (memory::Shape::Array(elem), 1) = (field.shape, field.dim) else { return None };
        if self.memory.element_size(elem) != (1, 0) {
            return None;
        }
        match self.object(object).mem.heap.get(field.heap as usize)? {
            memory::Heap::Array(array) => array.mem.words.get(..array.len as usize),
            memory::Heap::Str(_) => None,
        }
    }

    pub fn objects_of(&self, object: ObjectId, property: impl PropertyName) -> impl Iterator<Item = Option<ObjectId>> + '_ {
        self.array_words(object, property).unwrap_or_default().iter().map(|&word| memory::object(word))
    }

    pub(crate) fn element_of(&self, object: ObjectId, property: impl PropertyName, index: usize) -> Option<Value> {
        let field = self.field(object, property)?;
        let mem = &self.object(object).mem;
        if field.dim > 1 {
            if index >= field.dim as usize {
                return None;
            }
            let (word, heap) = (field.word as usize + index * field.words as usize, field.heap as usize + index * field.heaps as usize);
            return Some(self.memory.read(field.shape, 1, mem, word, heap));
        }
        let memory::Shape::Array(elem) = field.shape else { return None };
        let memory::Heap::Array(array) = mem.heap.get(field.heap as usize)? else { return None };
        if index >= array.len as usize {
            return None;
        }
        let f = self.memory.get(elem).fields[0];
        let (w, h) = self.memory.element_size(elem);
        Some(self.memory.read(f.shape, f.dim, &array.mem, index * w as usize, index * h as usize))
    }

    /// An element of a static array (`var T X[N]`).
    pub(crate) fn set_element(&mut self, object: ObjectId, property: impl PropertyName, index: usize, value: Value) -> bool {
        let Some(field) = self.field(object, property) else { return false };
        if index >= field.dim as usize {
            return false;
        }
        let (word, heap) = (field.word as usize + index * field.words as usize, field.heap as usize + index * field.heaps as usize);
        self.memory.write(field.shape, 1, &mut self.objects[object.0 as usize].mem, word, heap, value);
        true
    }

    pub(crate) fn object_list(&self, object: ObjectId, property: impl PropertyName) -> Vec<ObjectId> {
        self.array_words(object, property).map_or_else(Vec::new, |words| words.iter().filter_map(|&w| memory::object(w)).collect())
    }

    pub fn properties(&self, object: ObjectId) -> Vec<Value> {
        let object = self.object(object);
        let Some(class) = object.class else { return Vec::new() };
        self.memory.get(self.class(class).mem).fields.iter().map(|field| self.memory.read_field(field, &object.mem)).collect()
    }

    pub fn property_at(&self, object: ObjectId, index: usize) -> Option<Value> {
        let object_ref = self.object(object);
        let field = *self.memory.get(self.class(object_ref.class?).mem).fields.get(index)?;
        Some(self.memory.read_field(&field, &object_ref.mem))
    }

    pub(crate) fn set_property_at(&mut self, object: ObjectId, index: usize, value: Value) -> bool {
        let Some(class) = self.object(object).class else { return false };
        let Some(&field) = self.memory.get(self.class(class).mem).fields.get(index) else { return false };
        self.memory.write_field(&field, &mut self.objects[object.0 as usize].mem, value);
        true
    }

    fn member_place(&self, class: ClassId, first: &str, members: &[&str]) -> Option<(memory::Shape, u32, usize, usize)> {
        let index = self.property_index(class, first)?;
        let field = *self.memory.get(self.class(class).mem).fields.get(index)?;
        let mut ty = &self.class(class).layout.properties[index].ty;
        let (mut shape, mut dim, mut word, mut heap) = (field.shape, field.dim, field.word as usize, field.heap as usize);
        for member in members {
            let (Type::Struct(layout), memory::Shape::Struct(id), 1) = (ty, shape, dim) else { return None };
            let index = layout.properties.iter().position(|p| p.name.eq_ignore_ascii_case(member))?;
            let f = *self.memory.get(id).fields.get(index)?;
            ty = &layout.properties[index].ty;
            (shape, dim, word, heap) = (f.shape, f.dim, word + f.word as usize, heap + f.heap as usize);
        }
        Some((shape, dim, word, heap))
    }

    pub fn get_path(&self, object: ObjectId, path: &[&str]) -> Option<Value> {
        let (first, members) = path.split_first()?;
        let (shape, dim, word, heap) = self.member_place(self.object(object).class?, first, members)?;
        Some(self.memory.read(shape, dim, &self.object(object).mem, word, heap))
    }

    pub fn set_path(&mut self, object: ObjectId, path: &[&str], value: Value) -> bool {
        let Some((first, members)) = path.split_first() else { return false };
        let Some(class) = self.object(object).class else { return false };
        let Some((shape, dim, word, heap)) = self.member_place(class, first, members) else { return false };
        self.memory.write(shape, dim, &mut self.objects[object.0 as usize].mem, word, heap, value);
        true
    }

    pub fn set(&mut self, object: ObjectId, property: impl PropertyName, value: Value) -> bool {
        let Some(field) = self.field(object, property) else { return false };
        self.memory.write_field(&field, &mut self.objects[object.0 as usize].mem, value);
        true
    }

    pub fn default_value(&self, class: ClassId, property: impl PropertyName) -> Option<Value> {
        let field = property.field_in(self, class)?;
        Some(self.memory.read_field(&field, &self.class(class).defaults))
    }

    pub fn default_flag(&self, class: ClassId, property: impl PropertyName) -> bool {
        matches!(self.default_value(class, property), Some(Value::Bool(true)))
    }

    pub fn float_of(&self, object: ObjectId, property: impl PropertyName) -> f32 {
        match self.word(object, property) {
            Some((word, memory::Shape::Float)) => f32::from_bits(word),
            Some((word, memory::Shape::Int)) => word as i32 as f32,
            Some((word, memory::Shape::Byte | memory::Shape::Bool)) => word as f32,
            _ => 0.0,
        }
    }

    pub fn int_of(&self, object: ObjectId, property: impl PropertyName) -> i32 {
        match self.word(object, property) {
            Some((word, memory::Shape::Int)) => word as i32,
            Some((word, memory::Shape::Byte)) => i32::from(word as u8),
            Some((word, memory::Shape::Bool)) => i32::from(word != 0),
            Some((word, memory::Shape::Float)) => f32::from_bits(word) as i32,
            _ => 0,
        }
    }

    pub fn name_of(&self, object: ObjectId, property: impl PropertyName) -> NameId {
        match self.word(object, property) {
            Some((word, memory::Shape::Name)) => NameId(word),
            _ => NameId::NONE,
        }
    }

    pub fn collision_size(&self, object: ObjectId) -> (f32, f32) {
        (self.float_of(object, Prop::CollisionRadius), self.float_of(object, Prop::CollisionHeight))
    }

    pub fn flag_of(&self, object: ObjectId, property: impl PropertyName) -> bool {
        matches!(self.word(object, property), Some((word, memory::Shape::Bool)) if word != 0)
    }

    pub fn object_of(&self, object: ObjectId, property: impl PropertyName) -> Option<ObjectId> {
        match self.word(object, property) {
            Some((word, memory::Shape::Object)) => memory::object(word),
            _ => None,
        }
    }

    pub fn byte_of(&self, object: ObjectId, property: impl PropertyName) -> u8 {
        match self.word(object, property) {
            Some((word, memory::Shape::Byte | memory::Shape::Bool | memory::Shape::Int)) => word as u8,
            Some((word, memory::Shape::Float)) => f32::from_bits(word) as i32 as u8,
            _ => 0,
        }
    }

    pub fn rotator_of(&self, object: ObjectId, property: impl PropertyName) -> Rotator {
        self.members::<3>(object, property).map_or([0; 3].into(), |members| members.map(|m| int(&m)).into())
    }

    pub(crate) fn set_vector(&mut self, object: ObjectId, property: impl PropertyName, value: Vector) {
        self.set_members(object, property, <[f32; 3]>::from(value).map(Value::Float));
    }

    pub fn vector(&self, object: ObjectId, property: impl PropertyName) -> Vector {
        self.member_words::<3>(object, property).map_or(Vector::ZERO, |members| {
            members.map(|m| match m {
                (word, memory::Shape::Float) => f32::from_bits(word),
                _ => 0.0,
            }).into()
        })
    }

    pub(crate) fn vec3_of(&self, object: ObjectId, property: impl PropertyName) -> Vector {
        self.members::<3>(object, property).map_or(Vector::ZERO, |members| members.map(|m| float(&m)).into())
    }

    #[inline]
    pub fn location(&self, actor: ObjectId) -> Vector {
        self.vector(actor, Prop::Location)
    }

    #[inline]
    pub fn rotation(&self, actor: ObjectId) -> Rotator {
        self.member_words::<3>(actor, Prop::Rotation).map_or([0; 3].into(), |members| {
            members.map(|m| match m {
                (word, memory::Shape::Int) => word as i32,
                _ => 0,
            }).into()
        })
    }

    pub fn is_a(&self, object: ObjectId, class_name: impl ClassName) -> bool {
        self.object(object).class.is_some_and(|class| class_name.is_base_of(self, class))
    }

    pub(crate) fn class_has_name(&self, class: ClassId, name: NameId) -> bool {
        let mut current = Some(class);
        while let Some(class) = current {
            self.count(profiling::Work::ClassTestStep);
            if self.class(class).name == name {
                return true;
            }
            current = self.class(class).super_class;
        }
        false
    }

    pub(crate) fn class_is_named(&self, class: ClassId, class_name: &str) -> bool {
        self.count(profiling::Work::ClassTestByName);
        if class_name.eq_ignore_ascii_case("Actor") {
            return self.class(class).is_actor();
        }
        let mut current = Some(class);
        while let Some(class) = current {
            self.count(profiling::Work::ClassTestStep);
            if self.name(self.class(class).name).eq_ignore_ascii_case(class_name) {
                return true;
            }
            current = self.class(class).super_class;
        }
        false
    }

    #[inline]
    pub(crate) fn physics_of(&self, actor: ObjectId) -> Option<EPhysics> {
        EPhysics::from_byte(self.byte_of(actor, Prop::Physics))
    }

    #[inline]
    pub fn is_deleted(&self, actor: ObjectId) -> bool {
        self.flag_of(actor, Prop::bDeleteMe)
    }
}
