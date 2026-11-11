use p2_math::{Rotator, Vector};
use p2_package::PackageId;
use p2_script::Type;
use rustc_hash::FxHashMap;

use std::sync::atomic::{AtomicBool, Ordering};

use crate::enums::EPhysics;
use crate::interp::{float, int};
use crate::object::{Span, Stamp};
use crate::props::DRAWN;
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
    /// Goes up when a follower of object values has read them ([`ObjectStore::next_stamp`]); every write leaves the
    /// current one on its object.
    pub(crate) stamp: u64,
    drawn_reads: AtomicBool,
}

impl ObjectStore {
    pub fn stamp(&self) -> u64 {
        self.stamp
    }

    /// Called by a follower once it has read what it follows: writes from now on carry a later stamp than its reads.
    pub fn next_stamp(&mut self) {
        self.stamp += 1;
    }

    /// Runs `read`, the host reading what it draws an actor from. In debug builds a read of a property that is not on
    /// the drawn list fails: a write to it would not move `drawn`, and the host would miss the change.
    pub fn reading_drawn<T>(&self, read: impl FnOnce() -> T) -> T {
        self.drawn_reads.store(true, Ordering::Relaxed);
        let result = read();
        self.drawn_reads.store(false, Ordering::Relaxed);
        result
    }

    #[inline]
    fn check_drawn_read(&self, property: impl PropertyName) {
        if cfg!(debug_assertions) && self.drawn_reads.load(Ordering::Relaxed) {
            let prop = property.prop();
            assert!(prop.is_some_and(|prop| DRAWN.contains(&prop)), "the host draws from {prop:?}, which is not on the drawn list");
        }
    }

    pub(crate) fn stamp_at(&self, object: ObjectId, span: Span) -> Stamp {
        match self.objects.get(object.0 as usize).and_then(|object| object.class) {
            Some(class) if !self.class(class).resolved.drawn(span) => Stamp::Written,
            _ => Stamp::Drawn,
        }
    }

    /// `most` is [`Stamp::Written`] for the host's own write-backs: they leave `drawn` as it was.
    fn write_span(&mut self, object: ObjectId, span: Span, most: Stamp, apply: impl FnOnce(&memory::MemLayouts, &mut memory::Mem)) {
        let kind = match most {
            Stamp::Drawn => self.stamp_at(object, span),
            Stamp::Written => Stamp::Written,
        };
        let memory = &self.memory;
        self.objects[object.0 as usize].write(self.stamp, kind, span, |mem| apply(memory, mem));
    }

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
        self.check_drawn_read(property);
        property.field_in(self, self.object(object).class?)
    }

    pub fn get(&self, object: ObjectId, property: impl PropertyName) -> Option<Value> {
        if property.is_resolved()
            && let Some((word, shape)) = self.word(object, property)
        {
            return Some(memory::scalar(word, shape));
        }
        let field = self.field(object, property)?;
        Some(self.memory.read_field(&field, self.object(object).mem()))
    }

    #[inline]
    pub(crate) fn word(&self, object: ObjectId, property: impl PropertyName) -> Option<(u32, memory::Shape)> {
        self.check_drawn_read(property);
        let object_ref = self.object(object);
        let (word, shape) = property.word_in(self, object_ref.class?)?;
        match shape {
            memory::Shape::Byte | memory::Shape::Int | memory::Shape::Bool | memory::Shape::Float | memory::Shape::Object | memory::Shape::Name => {
                Some((*object_ref.mem().words.get(word as usize)?, shape))
            }
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn member_words<const N: usize>(&self, object: ObjectId, property: impl PropertyName) -> Option<[(u32, memory::Shape); N]> {
        const { assert!(N <= 3, "member_words reaches a struct's first three members (`MemLayout::head`)") };
        self.check_drawn_read(property);
        let object_ref = self.object(object);
        let (base, memory::Shape::Struct(id)) = property.word_in(self, object_ref.class?)? else { return None };
        let head = &self.memory.get(id).head;
        let words = &object_ref.mem().words;
        Some(std::array::from_fn(|i| match head.get(i) {
            Some(&(word, shape)) if shape != memory::Shape::Map => (words.get((base + word) as usize).copied().unwrap_or(0), shape),
            _ => (0, memory::Shape::Map),
        }))
    }

    /// R, G, B, A (`Object.uc` declares B, G, R, A), read in place. All four: [`Self::member_words`] reaches only the first
    /// three.
    pub fn color_of(&self, object: ObjectId, property: impl PropertyName) -> Option<[u8; 4]> {
        let field = self.field(object, property)?;
        let (memory::Shape::Struct(id), 1) = (field.shape, field.dim) else { return None };
        let (members, words) = (&self.memory.get(id).fields, &self.object(object).mem().words);
        let byte = |i: usize| members.get(i).and_then(|m| words.get((field.word + m.word) as usize)).map(|&word| word as u8);
        Some([byte(2)?, byte(1)?, byte(0)?, byte(3)?])
    }

    pub(crate) fn members<const N: usize>(&self, object: ObjectId, property: impl PropertyName) -> Option<[Value; N]> {
        Some(self.member_words::<N>(object, property)?.map(|(word, shape)| memory::scalar(word, shape)))
    }

    pub(crate) fn set_members<const N: usize>(&mut self, object: ObjectId, property: impl PropertyName, values: [Value; N]) -> bool {
        let Some(field) = self.field(object, property) else { return false };
        let (memory::Shape::Struct(id), 1) = (field.shape, field.dim) else { return false };
        self.write_span(object, Span::of(&field), Stamp::Drawn, |memory, mem| {
            for (m, value) in memory.get(id).fields.iter().zip(values) {
                memory.write(m.shape, m.dim, mem, (field.word + m.word) as usize, (field.heap + m.heap) as usize, value);
            }
        });
        true
    }

    pub(crate) fn array_words(&self, object: ObjectId, property: impl PropertyName) -> Option<&[u32]> {
        let field = self.field(object, property)?;
        let (memory::Shape::Array(elem), 1) = (field.shape, field.dim) else { return None };
        if self.memory.element_size(elem) != (1, 0) {
            return None;
        }
        match self.object(object).mem().heap.get(field.heap as usize)? {
            memory::Heap::Array(array) => array.mem.words.get(..array.len as usize),
            memory::Heap::Str(_) => None,
        }
    }

    pub fn objects_of(&self, object: ObjectId, property: impl PropertyName) -> impl Iterator<Item = Option<ObjectId>> + '_ {
        self.array_words(object, property).unwrap_or_default().iter().map(|&word| memory::object(word))
    }

    pub(crate) fn element_of(&self, object: ObjectId, property: impl PropertyName, index: usize) -> Option<Value> {
        let field = self.field(object, property)?;
        let mem = self.object(object).mem();
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
        let (word, heap) = (field.word + index as u32 * field.words, field.heap + index as u32 * field.heaps);
        let span = Span::place(word, heap, (field.words, field.heaps), 1);
        self.write_span(object, span, Stamp::Drawn, |memory, mem| memory.write(field.shape, 1, mem, word as usize, heap as usize, value));
        true
    }

    pub(crate) fn object_list(&self, object: ObjectId, property: impl PropertyName) -> Vec<ObjectId> {
        self.array_words(object, property).map_or_else(Vec::new, |words| words.iter().filter_map(|&w| memory::object(w)).collect())
    }

    pub fn properties(&self, object: ObjectId) -> Vec<Value> {
        let object = self.object(object);
        let Some(class) = object.class else { return Vec::new() };
        self.memory.get(self.class(class).mem).fields.iter().map(|field| self.memory.read_field(field, object.mem())).collect()
    }

    pub fn property_at(&self, object: ObjectId, index: usize) -> Option<Value> {
        let object_ref = self.object(object);
        let field = *self.memory.get(self.class(object_ref.class?).mem).fields.get(index)?;
        Some(self.memory.read_field(&field, object_ref.mem()))
    }

    pub(crate) fn set_property_at(&mut self, object: ObjectId, index: usize, value: Value) -> bool {
        let Some(class) = self.object(object).class else { return false };
        let Some(&field) = self.memory.get(self.class(class).mem).fields.get(index) else { return false };
        self.write_span(object, Span::of(&field), Stamp::Drawn, |memory, mem| memory.write_field(&field, mem, value));
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
        Some(self.memory.read(shape, dim, self.object(object).mem(), word, heap))
    }

    pub fn set_path(&mut self, object: ObjectId, path: &[&str], value: Value) -> bool {
        let Some((first, members)) = path.split_first() else { return false };
        let Some(class) = self.object(object).class else { return false };
        let Some((shape, dim, word, heap)) = self.member_place(class, first, members) else { return false };
        let span = Span::place(word as u32, heap as u32, self.memory.size(shape), dim);
        self.write_span(object, span, Stamp::Drawn, |memory, mem| memory.write(shape, dim, mem, word, heap, value));
        true
    }

    /// See [`Object::mem_mut_unstamped`].
    pub(crate) fn set_unstamped(&mut self, object: ObjectId, property: impl PropertyName, value: Value) -> bool {
        let Some(field) = self.field(object, property) else { return false };
        self.memory.write_field(&field, self.objects[object.0 as usize].mem_mut_unstamped(), value);
        true
    }

    pub fn set(&mut self, object: ObjectId, property: impl PropertyName, value: Value) -> bool {
        let Some(field) = self.field(object, property) else { return false };
        self.write_span(object, Span::of(&field), Stamp::Drawn, |memory, mem| memory.write_field(&field, mem, value));
        true
    }

    /// For the host's write-backs of what it drew: they move `written` but not `drawn`.
    pub(crate) fn set_undrawn(&mut self, object: ObjectId, property: impl PropertyName, value: Value) -> bool {
        let Some(field) = self.field(object, property) else { return false };
        self.write_span(object, Span::of(&field), Stamp::Written, |memory, mem| memory.write_field(&field, mem, value));
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

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use p2_package::Loader;
    use p2_test_package::{PackageBuilder, PropertyType};

    use crate::{ObjectId, Prop, Value, Vm};

    pub(crate) fn actor_vm(dir: &str) -> (Vm<Arc<Loader>>, ObjectId) {
        let mut package = PackageBuilder::new("Fixture");
        let vector = package.struct_type("Vector");
        for member in ["X", "Y", "Z"] {
            package.property(vector, member, PropertyType::Float, 0);
        }
        let actor = package.class("Actor", None);
        for name in ["Location", "Acceleration"] {
            package.property(actor, name, PropertyType::Struct(vector), 0);
        }
        for name in ["TimerCounter", "MoveTimer", "OldZ"] {
            package.property(actor, name, PropertyType::Float, 0);
        }
        package.array_property(actor, "Skins", PropertyType::Object, 0);
        let dir = package.write_to_temp(dir).unwrap();
        let mut loader = Loader::new(&dir, &["*.u".into()]).unwrap();
        loader.load_scripts().unwrap();
        let mut vm = Vm::new(Arc::new(loader));
        let class = vm.find_class("Fixture", "Actor").unwrap();
        let object = vm.new_object(class, None);
        vm.store.next_stamp();
        (vm, object)
    }

    fn stamps(vm: &Vm<Arc<Loader>>, object: ObjectId) -> (u64, u64) {
        let object = vm.store.object(object);
        (object.written(), object.drawn())
    }

    #[test]
    fn only_a_changed_drawn_property_moves_drawn() {
        let (mut vm, actor) = actor_vm("drawn-stamp-store");
        for name in [Prop::TimerCounter, Prop::MoveTimer, Prop::OldZ] {
            assert!(vm.store.set(actor, name, Value::Float(2.0)));
        }
        vm.store.set_vector(actor, Prop::Acceleration, [1.0, 2.0, 3.0].into());
        vm.store.set_vector(actor, Prop::Location, [0.0; 3].into());
        assert_eq!(stamps(&vm, actor), (1, 0));
        vm.store.next_stamp();
        vm.store.set_vector(actor, Prop::Location, [0.0, 5.0, 0.0].into());
        assert_eq!(stamps(&vm, actor), (2, 2));
        vm.store.next_stamp();
        assert!(vm.store.set_path(actor, &["Location", "Z"], Value::Float(1.0)));
        assert_eq!(stamps(&vm, actor), (3, 3));
        vm.store.next_stamp();
        assert!(vm.store.set_undrawn(actor, Prop::Location, Value::Vector([7.0; 3].into())));
        assert_eq!(stamps(&vm, actor), (4, 3));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "not on the drawn list")]
    fn a_host_read_off_the_drawn_list_fails_in_debug_builds() {
        let (vm, actor) = actor_vm("drawn-stamp-guard");
        vm.store.reading_drawn(|| vm.store.location(actor));
        vm.store.reading_drawn(|| vm.store.float_of(actor, Prop::TimerCounter));
    }
}
