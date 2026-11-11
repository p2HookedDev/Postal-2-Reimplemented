use p2_package::PackageId;
use p2_script::{Expr, Source};
use crate::memory::{Array, Heap, LayoutId, Mem, MemLayouts, Shape};
use crate::object::{Span, Stamp};
use crate::{ClassId, Error, ObjectId, LoaderRef, Value, Vm};
use super::{
    Frame, Hop, MAX_HOPS, Place, Root, Target, block, block_mut, int, linked, member_link, member_of_link, set_link,
    unlink,
};

impl<L: LoaderRef> Vm<L> {
    pub(super) fn assign(&mut self, frame: &mut Frame, target: &Expr, value: Value) -> Result<(), Error> {
        if let Expr::DynArrayLength(array) = target {
            if let Target::Place(place) = self.place(frame, array, frame.this)?
                && let Some((memory, items, elem)) = self.array_mut(frame, &place)
            {
                memory.resize(elem, items, int(&value).max(0) as usize);
            }
            return Ok(());
        }
        match self.place(frame, target, frame.this)? {
            Target::Place(place) => self.write_place(frame, &place, value),
            Target::Nowhere | Target::Constant(_) => {}
        }
        Ok(())
    }

    fn root<'s>(&'s self, frame: &'s Frame, root: Root) -> Option<&'s Mem> {
        Some(match root {
            Root::Local => &frame.locals,
            Root::Instance(object) => self.store.objects.get(object.0 as usize)?.mem(),
            Root::Default(class) => &self.store.classes.get(class.0 as usize)?.defaults,
        })
    }

    pub(super) fn readable(&self, frame: &Frame, place: &Place) -> bool {
        self.root(frame, place.root).and_then(|mem| block(mem, place)).is_some()
    }

    pub(super) fn read_place(&self, frame: &Frame, place: &Place) -> Value {
        let empty = Mem::default();
        let (mem, word, heap) = match self.root(frame, place.root).and_then(|mem| block(mem, place)) {
            Some(mem) => (mem, place.word as usize, place.heap as usize),
            None => (&empty, 0, 0),
        };
        self.store.memory.read(place.shape, place.dim, mem, word, heap)
    }

    /// Inside a dynamic array the whole top-level array counts: its heap slot is the one the class flags.
    fn span(&self, place: &Place) -> Span {
        match place.hops[..place.depth as usize].first() {
            Some(hop) => Span::place(0, hop.heap, (0, 1), 1),
            None => Span::place(place.word, place.heap, self.store.memory.size(place.shape), place.dim),
        }
    }

    pub(super) fn write_place(&mut self, frame: &mut Frame, place: &Place, value: Value) {
        let (span, stamp) = (self.span(place), self.store.stamp);
        let kind = match place.root {
            Root::Instance(object) => self.store.stamp_at(object, span),
            Root::Local | Root::Default(_) => Stamp::Written,
        };
        let memory = &self.store.memory;
        let apply = |mem: &mut Mem| {
            if let Some(mem) = block_mut(memory, mem, place) {
                memory.write(place.shape, place.dim, mem, place.word as usize, place.heap as usize, value);
            }
        };
        match place.root {
            Root::Local => apply(&mut frame.locals),
            Root::Instance(object) => {
                if let Some(object) = self.store.objects.get_mut(object.0 as usize) {
                    object.write(stamp, kind, span, apply);
                }
            }
            Root::Default(class) => {
                if let Some(class) = self.store.classes.get_mut(class.0 as usize) {
                    apply(&mut class.defaults);
                }
            }
        }
    }

    pub(super) fn array_mut<'s>(&'s mut self, frame: &'s mut Frame, place: &Place) -> Option<(&'s MemLayouts, &'s mut Array, LayoutId)> {
        let (Shape::Array(elem), 1) = (place.shape, place.dim) else { return None };
        let kind = match place.root {
            Root::Instance(object) => self.store.stamp_at(object, self.span(place)),
            Root::Local | Root::Default(_) => Stamp::Written,
        };
        let memory = &self.store.memory;
        let stamp = self.store.stamp;
        let mem = match place.root {
            Root::Local => &mut frame.locals,
            Root::Instance(object) => self.store.objects.get_mut(object.0 as usize)?.mem_mut(stamp, kind),
            Root::Default(class) => &mut self.store.classes.get_mut(class.0 as usize)?.defaults,
        };
        match block_mut(memory, mem, place)?.heap.get_mut(place.heap as usize)? {
            Heap::Array(array) => Some((memory, array, elem)),
            Heap::Str(_) => None,
        }
    }

    pub(super) fn array_len(&self, frame: &Frame, place: &Place) -> usize {
        let (Shape::Array(_), 1) = (place.shape, place.dim) else { return 0 };
        match self.root(frame, place.root).and_then(|mem| block(mem, place)).and_then(|mem| mem.heap.get(place.heap as usize)) {
            Some(Heap::Array(array)) => array.len as usize,
            _ => 0,
        }
    }

    /// A static array's index clamps into range, as retail; a dynamic array's is a hop into its elements.
    pub(super) fn element(&self, mut place: Place, index: i32) -> Target {
        match place.shape {
            _ if place.dim > 1 => {
                let index = index.clamp(0, place.dim as i32 - 1) as u32;
                let (words, heaps) = self.store.memory.size(place.shape);
                place.word += index * words;
                place.heap += index * heaps;
                place.dim = 1;
                Target::Place(place)
            }
            Shape::Array(elem) if (place.depth as usize) < MAX_HOPS && index >= 0 => {
                place.hops[place.depth as usize] = Hop { heap: place.heap, index: index as u32, elem };
                place.depth += 1;
                let (words, heaps) = self.store.memory.element_size(elem);
                let field = self.store.memory.get(elem).fields[0];
                place.word = index as u32 * words;
                place.heap = index as u32 * heaps;
                place.shape = field.shape;
                place.dim = field.dim;
                place.grows = true;
                Target::Place(place)
            }
            _ => Target::Nowhere,
        }
    }

    pub(super) fn place(&mut self, frame: &mut Frame, expr: &Expr, context: Option<ObjectId>) -> Result<Target, Error> {
        let package = frame.function.package;
        let property_name = |vm: &Self, reference: i32| {
            vm.loader.package(package).package.object_name(reference).unwrap_or("").to_owned()
        };
        let (expr, link) = unlink(expr);
        Ok(match expr {
            Expr::BoolVariable(inner) => self.place(frame, inner, context)?,
            Expr::Context { object, member, .. } | Expr::ClassContext { object, member, .. } => {
                match self.eval(frame, object, context)? {
                    Value::Object(Some(object)) => self.place(frame, member, Some(object))?,
                    _ => Target::Nowhere,
                }
            }
            Expr::LocalVariable(reference) => {
                let index = match linked(frame, link) {
                    Some(index) => index as usize,
                    None => {
                        let name = property_name(self, *reference);
                        let index = frame.function.layout.properties.iter().position(|p| p.name.eq_ignore_ascii_case(&name));
                        let index = index.ok_or(Error::Unsupported(format!("local {name}")))?;
                        set_link(frame, link, index as u64);
                        index
                    }
                };
                self.local_target(frame, index)
            }
            Expr::InstanceVariable(reference) => {
                let object = context.ok_or_else(|| Error::Unsupported(format!("{} without an object", property_name(self, *reference))))?;
                if let Some(class) = self.store.object(object).class {
                    let index = self.member_index(frame, link, class, package, *reference);
                    let index = index.ok_or_else(|| Error::Unsupported(format!("property {}", property_name(self, *reference))))?;
                    return Ok(self.instance_target(object, class, index));
                }
                Target::Constant(self.stub_member(object, package, *reference))
            }
            Expr::DefaultVariable(reference) => {
                let class = self.context_class(context)?.unwrap_or(frame.class);
                let index = self.member_index(frame, link, class, package, *reference);
                let index = index.ok_or_else(|| Error::Unsupported(format!("default {}", property_name(self, *reference))))?;
                match self.store.memory.get(self.store.class(class).mem).fields.get(index) {
                    Some(field) => Target::Place(Place::new(Root::Default(class), field)),
                    None => Target::Nowhere,
                }
            }
            Expr::ArrayElement { index, array } | Expr::DynArrayElement { index, array } => {
                let element = int(&self.eval(frame, index, frame.this)?);
                match self.place(frame, array, context)? {
                    Target::Place(place) => self.element(place, element),
                    _ => Target::Nowhere,
                }
            }
            Expr::StructMember { member, value } => {
                let index = self.linked_member_index(frame, link, *member)?;
                let target = self.place(frame, value, context)?;
                self.struct_member_target(target, index)
            }
            other => return Err(Error::Unsupported(format!("assign to {other:?}").chars().take(60).collect())),
        })
    }

    fn member_index(&mut self, frame: &Frame, link: Option<u32>, class: ClassId, package: PackageId, reference: i32) -> Option<usize> {
        if let Some(index) = linked(frame, link).and_then(|value| member_of_link(value, class)) {
            return index;
        }
        let key = (class, package, reference);
        let index = match self.script.lookups.members.get(&key) {
            Some(&index) => index,
            None => {
                let index = self.reference_property(class, package, reference);
                self.script.lookups.members.insert(key, index);
                index
            }
        };
        set_link(frame, link, member_link(class, index));
        index
    }

    pub(super) fn local_target(&self, frame: &Frame, index: usize) -> Target {
        match self.store.memory.get(frame.function.mem).fields.get(index) {
            Some(field) => Target::Place(Place::new(Root::Local, field)),
            None => Target::Nowhere,
        }
    }

    pub(super) fn instance_target(&self, object: ObjectId, class: ClassId, index: usize) -> Target {
        match self.store.memory.get(self.store.class(class).mem).fields.get(index) {
            Some(field) => Target::Place(Place::new(Root::Instance(object), field)),
            None => Target::Nowhere,
        }
    }

    pub(super) fn struct_member_target(&self, target: Target, index: usize) -> Target {
        match target {
            Target::Place(mut place) => match (place.shape, place.dim) {
                (Shape::Struct(id), 1) => match self.store.memory.get(id).fields.get(index) {
                    Some(field) => {
                        place.word += field.word;
                        place.heap += field.heap;
                        place.shape = field.shape;
                        place.dim = field.dim;
                        place.grows = false;
                        Target::Place(place)
                    }
                    None => Target::Nowhere,
                },
                _ => Target::Nowhere,
            },
            _ => Target::Nowhere,
        }
    }

    pub(super) fn linked_member_index(&mut self, frame: &Frame, link: Option<u32>, member: i32) -> Result<usize, Error> {
        if let Some(index) = linked(frame, link) {
            return Ok(index as usize);
        }
        let index = self.find_member_index(frame.function.package, member)?;
        set_link(frame, link, index as u64);
        Ok(index)
    }

    fn find_member_index(&mut self, package: PackageId, member: i32) -> Result<usize, Error> {
        let missing = || Error::Unsupported(format!("struct member {member}"));
        let (member_package, member_export) = self.loader.loaded_export(package, member).ok_or_else(missing)?;
        let loaded = self.loader.package(member_package);
        let entry = &loaded.package.exports[member_export];
        let name = loaded.package.name(entry.object_name);
        let struct_export = usize::try_from(entry.package_index - 1).map_err(|_| missing())?;
        let layout = self.layouts.layout(Source { package: &loaded.package, data: &loaded.data }, struct_export)?;
        layout.properties.iter().position(|p| p.name.eq_ignore_ascii_case(name)).ok_or_else(missing)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use p2_package::Loader;

    use super::*;
    use crate::object_store::tests::actor_vm;

    fn stamp_of(vm: &Vm<Arc<Loader>>, actor: ObjectId, target: Target) -> Stamp {
        match target {
            Target::Place(place) => vm.store.stamp_at(actor, vm.span(&place)),
            Target::Nowhere | Target::Constant(_) => panic!("no place"),
        }
    }

    #[test]
    fn script_places_inside_a_drawn_property_are_drawn() {
        let (vm, actor) = actor_vm("drawn-stamp-places");
        let class = vm.store.object(actor).class.unwrap();
        let property = |name: &str| vm.instance_target(actor, class, vm.store.property_index(class, name).unwrap());
        assert_eq!(stamp_of(&vm, actor, vm.struct_member_target(property("Location"), 1)), Stamp::Drawn);
        assert_eq!(stamp_of(&vm, actor, vm.struct_member_target(property("Acceleration"), 1)), Stamp::Written);
        assert_eq!(stamp_of(&vm, actor, property("TimerCounter")), Stamp::Written);
        let Target::Place(skins) = property("Skins") else { panic!("Skins") };
        assert_eq!(stamp_of(&vm, actor, vm.element(skins, 2)), Stamp::Drawn);
    }
}
