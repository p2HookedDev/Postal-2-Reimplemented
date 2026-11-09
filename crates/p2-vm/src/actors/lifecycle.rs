use p2_math::Vector;
use crate::volumes::{OldZone, ZoneEvents};
use crate::{ClassId, Error, Kind, ObjectId, LoaderRef, Prop, Value, Vm};
use super::{BEGUN_EVENTS, SPAWN_EVENTS};

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn dynamic_actors(&mut self) -> Vec<ObjectId> {
        let mut dynamic = std::mem::take(&mut self.store.dynamic_ids);
        let actors = dynamic.walk(|actor| self.store.flag_of(actor, Prop::bStatic));
        self.store.dynamic_ids = dynamic;
        actors
    }

    fn next_destroy_walk_actor(&mut self, after: Option<ObjectId>) -> Option<ObjectId> {
        if !self.store.dynamic_ids.unsorted.is_empty() {
            let mut dynamic = std::mem::take(&mut self.store.dynamic_ids);
            dynamic.sort_new(|actor| self.store.flag_of(actor, Prop::bStatic));
            self.store.dynamic_ids = dynamic;
        }
        self.store.dynamic_ids.next_after(after)
    }

    fn list_linked_statics(&mut self) {
        if self.store.dynamic_ids.linked_statics.is_none() {
            let holds_links = |actor: ObjectId| self.store.object_of(actor, Prop::Owner).is_some() || self.store.array_words(actor, Prop::Touching).is_some_and(|words| !words.is_empty());
            let linked = self.actors().filter(|&actor| self.store.flag_of(actor, Prop::bStatic) && holds_links(actor)).collect();
            self.store.dynamic_ids.linked_statics = Some(linked);
        }
    }

    pub(super) fn link_static(&mut self, actor: ObjectId) {
        let is_static = self.store.flag_of(actor, Prop::bStatic);
        if is_static && let Some(list) = &mut self.store.dynamic_ids.linked_statics {
            list.insert(actor);
        }
    }

    pub fn set_level(&mut self, level: ObjectId) {
        self.level = Some(level);
    }

    pub fn level(&self) -> Option<ObjectId> {
        self.level
    }

    /// `placement` holds `Vector` / `Rotator` struct values. `None` if a class colliding with the world
    /// does not fit, if `EncroachingOn` refused an overlapped actor, or if the actor destroyed itself.
    pub fn spawn(&mut self, class: ClassId, owner: Option<ObjectId>, placement: [Option<Value>; 2]) -> Result<Option<ObjectId>, Error> {
        self.spawn_actor(class, owner, None, placement)
    }

    pub(crate) fn spawn_actor(
        &mut self,
        class: ClassId,
        owner: Option<ObjectId>,
        instigator: Option<ObjectId>,
        mut placement: [Option<Value>; 2],
    ) -> Result<Option<ObjectId>, Error> {
        // Before the actor is constructed, `FindSpot` may move it, else the spawn fails with no object made.
        // A spawn without a location (the host's own) has none to fit.
        if let Some(value) = &placement[0] {
            let default_float = |vm: &Self, name: Prop| match vm.store.default_value(class, name) {
                Some(Value::Float(value)) => value,
                _ => 0.0,
            };
            if self.store.default_flag(class, Prop::bCollideWorld) || self.store.default_flag(class, Prop::bCollideWhenPlacing) {
                let (radius, height) = (default_float(self, Prop::CollisionRadius), default_float(self, Prop::CollisionHeight));
                let mut location = crate::natives::vec3(Some(value));
                if !crate::find_spot::find_spot(self, Vector::new(radius, radius, height), &mut location) {
                    let name = self.store.name(self.store.class(class).name).to_owned();
                    self.diagnostics.log.push(format!("SpawnActor failed because {name} won't fit at the specified location"));
                    return Ok(None);
                }
                placement[0] = Some(Value::Vector(location));
            }
        }
        let actor = self.new_object(class, None);
        self.instance_emitters(actor);
        if self.store.is_a(actor, self.script.lookups.class_names.hud) {
            self.construct_hud_movies(actor)?;
        }
        self.store.set(actor, Prop::Level, Value::Object(self.level));
        self.store.set(actor, Prop::Tag, Value::Name(self.store.class(class).name));
        for (property, value) in [Prop::Location, Prop::Rotation].into_iter().zip(placement) {
            if let Some(value @ (Value::Struct(_) | Value::Vector(_) | Value::Rotator(_))) = value {
                self.store.set(actor, property, value);
            }
        }
        // Whether the hash add below comes before or after `GainedChild` is unverified.
        self.set_owner(actor, owner)?;
        self.store.set(actor, Prop::Instigator, Value::Object(instigator));
        self.relocate(actor);
        for event in SPAWN_EVENTS {
            self.event(actor, event, Vec::new())?;
            if self.store.is_deleted(actor) {
                return Ok(None);
            }
        }
        self.set_zone(actor, ZoneEvents::Sent, OldZone::Level)?;
        if self.store.is_deleted(actor) {
            return Ok(None);
        }
        let at = self.store.vector(actor, Prop::Location);
        if self.actor_encroachment(actor, at)? {
            self.diagnostics.log.push(format!("SpawnActor failed because {} was encroaching", self.store.object(actor).path));
            self.destroy(actor)?;
            return Ok(None);
        }
        for event in BEGUN_EVENTS {
            if self.store.is_deleted(actor) {
                return Ok(None);
            }
            self.event(actor, event, Vec::new())?;
        }
        Ok((!self.store.is_deleted(actor)).then_some(actor))
    }

    /// No check that the owner changed, nor for a destroy between.
    pub(crate) fn set_owner(&mut self, actor: ObjectId, owner: Option<ObjectId>) -> Result<(), Error> {
        if let Some(old) = self.store.object_of(actor, Prop::Owner) {
            self.event(old, crate::states::Event::LostChild, [Value::Object(Some(actor))])?;
        }
        self.store.set(actor, Prop::Owner, Value::Object(owner));
        self.link_static(actor);
        if let Some(new) = owner {
            self.event(new, crate::states::Event::GainedChild, [Value::Object(Some(actor))])?;
        }
        Ok(())
    }

    /// `Emitters` is an `export editinline` object array: constructing an object at run time instances
    /// such subobjects (unverified), so one fire's script `+=` stays its own.
    fn instance_emitters(&mut self, actor: ObjectId) {
        if !self.store.is_a(actor, Kind::Emitter) {
            return;
        }
        let Some(Value::Array(templates)) = self.store.get(actor, Prop::Emitters) else { return };
        let emitters = templates
            .into_iter()
            .map(|template| match template {
                Value::Object(Some(template)) => Value::Object(Some(self.duplicate_object(template))),
                other => other,
            })
            .collect();
        self.store.set(actor, Prop::Emitters, Value::Array(emitters));
    }

    /// In retail order. `Attached` detach by a live index walk, so retail skips every other one. After
    /// every event a nested destroy that finished ends this one. The object and its lists stay, as retail.
    pub fn destroy(&mut self, actor: ObjectId) -> Result<bool, Error> {
        if self.store.flag_of(actor, Prop::bStatic) || self.store.flag_of(actor, Prop::bNoDelete) {
            return Ok(false);
        }
        if self.store.is_deleted(actor) {
            return Ok(true);
        }
        self.event(actor, crate::states::Event::EndState, Vec::new())?;
        if self.store.is_deleted(actor) {
            return Ok(true);
        }
        self.event(actor, crate::states::Event::Destroyed, Vec::new())?;
        if self.store.object_of(actor, Prop::Base).is_some() {
            self.set_base(actor, None, [0.0, 0.0, 1.0].into())?;
            if self.store.is_deleted(actor) {
                return Ok(true);
            }
        }
        let mut i = 0;
        while let Some(word) = self.store.array_words(actor, Prop::Attached).and_then(|words| words.get(i).copied()) {
            if let Some(attached) = crate::memory::object(word) {
                self.set_base(attached, None, [0.0, 0.0, 1.0].into())?;
            }
            i += 1;
        }
        if self.store.is_deleted(actor) {
            return Ok(true);
        }
        // The walk sees actors spawned during its events (retail's list grows at the end).
        self.list_linked_statics();
        let mut at = None;
        while let Some(other) = self.next_destroy_walk_actor(at) {
            at = Some(other);
            if self.store.object_of(other, Prop::Owner) == Some(actor) {
                self.event(actor, crate::states::Event::LostChild, [Value::Object(Some(other))])?;
                self.store.set(other, Prop::Owner, Value::Object(None));
                if self.store.is_deleted(actor) {
                    return Ok(true);
                }
            }
            if self.touches(other, actor) {
                self.drop_touching(actor, other);
                self.event(other, crate::states::Event::UnTouch, [Value::Object(Some(actor))])?;
                self.drop_touching(other, actor);
                if self.store.is_deleted(actor) {
                    return Ok(true);
                }
            }
        }
        if let Some(owner) = self.store.object_of(actor, Prop::Owner) {
            self.event(owner, crate::states::Event::LostChild, [Value::Object(Some(actor))])?;
            if self.store.is_deleted(actor) {
                return Ok(true);
            }
        }
        self.script.states.leave(actor);
        self.store.set(actor, Prop::bDeleteMe, Value::Bool(true));
        self.store.actor_ids.remove(&actor);
        self.store.dynamic_ids.removed(actor);
        self.relocate(actor);
        self.forget_placed_shape(actor);
        Ok(true)
    }
}
