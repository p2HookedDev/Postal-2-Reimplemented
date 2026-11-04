use p2_math::Vector;
use crate::enums::EPhysics;
use crate::{ClassId, Error, HostCommand, Kind, ObjectId, LoaderRef, Prop, Value, Vm};

fn add_time(profile: &mut std::collections::HashMap<String, std::time::Duration>, name: &str, time: std::time::Duration) {
    match profile.get_mut(name) {
        Some(total) => *total += time,
        None => {
            profile.insert(name.to_owned(), time);
        }
    }
}

const SPAWN_EVENTS: [&str; 3] = ["Spawned", "PreBeginPlay", "BeginPlay"];
const BEGUN_EVENTS: [&str; 3] = ["PostBeginPlay", "SetInitialState", "PostNetBeginPlay"];
const PROBE_TICK: u32 = 36;
const MIN_TICK: f32 = 0.0005;
const MAX_TICK: f32 = 0.4;

/// Live non-static actors in creation order: what a level tick and the collision hash rebuild walk.
/// `bStatic` is `const` in script, so a new actor is sorted once, on the first walk after it was made.
#[derive(Debug, Default)]
pub(crate) struct DynamicActors {
    ids: Vec<ObjectId>,
    unsorted: Vec<ObjectId>,
    /// All [`Vm::destroy`]'s walk needs besides `ids`. `None` until the first destroy and after a level or a save is
    /// loaded. `Owner` and `Touching` are `const` in script (unverified), so only natives write them.
    linked_statics: Option<std::collections::BTreeSet<ObjectId>>,
}

impl DynamicActors {
    pub(crate) fn added(&mut self, actor: ObjectId) {
        self.unsorted.push(actor);
    }

    pub(crate) fn removed(&mut self, actor: ObjectId) {
        if let Ok(at) = self.ids.binary_search(&actor) {
            self.ids.remove(at);
        }
        self.unsorted.retain(|&a| a != actor);
        if let Some(list) = &mut self.linked_statics {
            list.remove(&actor);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.ids.clear();
        self.unsorted.clear();
        self.linked_statics = None;
    }

    pub(crate) fn forget_linked_statics(&mut self) {
        self.linked_statics = None;
    }

    fn walk(&mut self, is_static: impl Fn(ObjectId) -> bool) -> Vec<ObjectId> {
        self.sort_new(is_static);
        self.ids.clone()
    }

    fn sort_new(&mut self, is_static: impl Fn(ObjectId) -> bool) {
        for actor in self.unsorted.drain(..) {
            if !is_static(actor)
                && let Err(at) = self.ids.binary_search(&actor)
            {
                self.ids.insert(at, actor);
            }
        }
    }

    fn next_after(&self, after: Option<ObjectId>) -> Option<ObjectId> {
        let at = after.map_or(0, |after| self.ids.partition_point(|&id| id <= after));
        let linked = self.linked_statics.as_ref().and_then(|list| next_level_actor(list, after));
        self.ids.get(at).copied().into_iter().chain(linked).min()
    }
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn dynamic_actors(&mut self) -> Vec<ObjectId> {
        let mut dynamic = std::mem::take(&mut self.store.dynamic_ids);
        let actors = dynamic.walk(|actor| self.flag_of(actor, Prop::bStatic));
        self.store.dynamic_ids = dynamic;
        actors
    }

    fn next_destroy_walk_actor(&mut self, after: Option<ObjectId>) -> Option<ObjectId> {
        if !self.store.dynamic_ids.unsorted.is_empty() {
            let mut dynamic = std::mem::take(&mut self.store.dynamic_ids);
            dynamic.sort_new(|actor| self.flag_of(actor, Prop::bStatic));
            self.store.dynamic_ids = dynamic;
        }
        self.store.dynamic_ids.next_after(after)
    }

    fn list_linked_statics(&mut self) {
        if self.store.dynamic_ids.linked_statics.is_none() {
            let holds_links = |actor: ObjectId| self.object_of(actor, Prop::Owner).is_some() || self.array_words(actor, Prop::Touching).is_some_and(|words| !words.is_empty());
            let linked = self.actors().filter(|&actor| self.flag_of(actor, Prop::bStatic) && holds_links(actor)).collect();
            self.store.dynamic_ids.linked_statics = Some(linked);
        }
    }

    fn link_static(&mut self, actor: ObjectId) {
        let is_static = self.flag_of(actor, Prop::bStatic);
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
            let default_flag = |vm: &Self, name: Prop| matches!(vm.default_value(class, name), Some(Value::Bool(true)));
            let default_float = |vm: &Self, name: Prop| match vm.default_value(class, name) {
                Some(Value::Float(value)) => value,
                _ => 0.0,
            };
            if default_flag(self, Prop::bCollideWorld) || default_flag(self, Prop::bCollideWhenPlacing) {
                let (radius, height) = (default_float(self, Prop::CollisionRadius), default_float(self, Prop::CollisionHeight));
                let mut location = crate::natives::vec3(Some(value));
                if !crate::find_spot::find_spot(self, Vector::new(radius, radius, height), &mut location) {
                    let name = self.name(self.class(class).name).to_owned();
                    self.diagnostics.log.push(format!("SpawnActor failed because {name} won't fit at the specified location"));
                    return Ok(None);
                }
                placement[0] = Some(Value::Vector(location));
            }
        }
        let actor = self.new_object(class, None);
        self.instance_emitters(actor);
        if self.is_a(actor, self.script.lookups.class_names.hud) {
            self.construct_hud_movies(actor)?;
        }
        self.set(actor, Prop::Level, Value::Object(self.level));
        self.set(actor, Prop::Tag, Value::Name(self.class(class).name));
        for (property, value) in [Prop::Location, Prop::Rotation].into_iter().zip(placement) {
            if let Some(value @ (Value::Struct(_) | Value::Vector(_) | Value::Rotator(_))) = value {
                self.set(actor, property, value);
            }
        }
        // Whether the hash add below comes before or after `GainedChild` is unverified.
        self.set_owner(actor, owner)?;
        self.set(actor, Prop::Instigator, Value::Object(instigator));
        self.relocate(actor);
        for event in SPAWN_EVENTS {
            self.event(actor, event, Vec::new())?;
            if self.is_deleted(actor) {
                return Ok(None);
            }
        }
        self.set_zone(actor, false, true)?;
        if self.is_deleted(actor) {
            return Ok(None);
        }
        let at = self.vector(actor, Prop::Location);
        if self.actor_encroachment(actor, at)? {
            self.diagnostics.log.push(format!("SpawnActor failed because {} was encroaching", self.object(actor).path));
            self.destroy(actor)?;
            return Ok(None);
        }
        for event in BEGUN_EVENTS {
            if self.is_deleted(actor) {
                return Ok(None);
            }
            self.event(actor, event, Vec::new())?;
        }
        Ok((!self.is_deleted(actor)).then_some(actor))
    }

    /// No check that the owner changed, nor for a destroy between.
    pub(crate) fn set_owner(&mut self, actor: ObjectId, owner: Option<ObjectId>) -> Result<(), Error> {
        if let Some(old) = self.object_of(actor, Prop::Owner) {
            self.event(old, "LostChild", [Value::Object(Some(actor))])?;
        }
        self.set(actor, Prop::Owner, Value::Object(owner));
        self.link_static(actor);
        if let Some(new) = owner {
            self.event(new, "GainedChild", [Value::Object(Some(actor))])?;
        }
        Ok(())
    }

    /// With the touch notify on. Only the query and overlap test run at `at`: events see `actor` where it
    /// stands, as retail, which places it afterwards. An encroacher's push is zero here, moving nothing.
    fn actor_encroachment(&mut self, actor: ObjectId, at: Vector) -> Result<bool, Error> {
        let encroacher = self.is_encroacher(actor);
        let tested = encroachment_tested([
            self.flag_of(actor, Prop::bCollideActors),
            self.flag_of(actor, Prop::bBlockActors),
            self.flag_of(actor, Prop::bBlockPlayers),
            encroacher,
        ]);
        if !tested {
            return Ok(false);
        }
        let found: Vec<ObjectId> = self
            .at_location(actor, at, |vm| vm.encroachment_found(actor))
            .into_iter()
            .filter(|&other| Some(other) != self.level && !self.is_based_on(other, actor))
            .collect();
        for &other in &found {
            if self.is_deleted(other) || !self.is_blocked_by(actor, other) {
                continue;
            }
            if encroacher && !self.is_encroacher(other) {
                let still = self.at_location(actor, at, |vm| if vm.is_a(other, Kind::Volume) { vm.shapes_overlap(actor, other) } else { vm.primitive_overlaps(actor, other) });
                if !still {
                    continue;
                }
            }
            if self.event_bool(actor, "EncroachingOn", [Value::Object(Some(other))])? {
                return Ok(true);
            }
        }
        self.end_touches(actor)?;
        for other in found {
            if self.is_deleted(actor) {
                break;
            }
            if self.is_deleted(other) {
                continue;
            }
            if self.is_blocked_by(actor, other) {
                self.event(other, "EncroachedBy", [Value::Object(Some(actor))])?;
            } else {
                self.begin_touch(actor, other)?;
            }
        }
        Ok(false)
    }

    fn at_location<T>(&mut self, actor: ObjectId, at: Vector, f: impl FnOnce(&mut Self) -> T) -> T {
        let from = self.vector(actor, Prop::Location);
        self.set_vector(actor, Prop::Location, at);
        let result = f(self);
        self.set_vector(actor, Prop::Location, from);
        result
    }

    fn encroachment_found(&mut self, actor: ObjectId) -> Vec<ObjectId> {
        let encroacher = self.is_encroacher(actor);
        self.overlapping(actor)
            .into_iter()
            .filter(|&other| {
                self.should_trace(other, Some(actor), crate::world::trace::ALL_COLLIDING)
                    && !(encroacher && self.flag_of(other, Prop::bWorldGeometry))
            })
            .collect()
    }

    /// `Emitters` is an `export editinline` object array: constructing an object at run time instances
    /// such subobjects (unverified), so one fire's script `+=` stays its own.
    fn instance_emitters(&mut self, actor: ObjectId) {
        if !self.is_a(actor, Kind::Emitter) {
            return;
        }
        let Some(Value::Array(templates)) = self.get(actor, Prop::Emitters) else { return };
        let emitters = templates
            .into_iter()
            .map(|template| match template {
                Value::Object(Some(template)) => Value::Object(Some(self.duplicate_object(template))),
                other => other,
            })
            .collect();
        self.set(actor, Prop::Emitters, Value::Array(emitters));
    }

    pub(crate) fn encroached(&mut self, actor: ObjectId) -> Option<ObjectId> {
        let blocks = |vm: &Self, object: ObjectId| {
            vm.flag_of(object, Prop::bCollideActors) && vm.flag_of(object, Prop::bBlockActors) && !vm.is_deleted(object)
        };
        if !blocks(self, actor) {
            return None;
        }
        let at = self.vector(actor, Prop::Location);
        let nearby = self.colliders_near([at[0], at[1]], [at[0], at[1]], self.float_of(actor, Prop::CollisionRadius));
        let found = nearby
            .iter()
            .map(|e| e.actor)
            .find(|&other| other != actor && self.is_a(other, Kind::Actor) && blocks(self, other) && self.shapes_overlap(actor, other));
        self.give_entries(nearby);
        found
    }

    pub(crate) fn cylinders_overlap(&self, a: ObjectId, b: ObjectId) -> bool {
        let float = |object: ObjectId, name: Prop| self.float_of(object, name);
        let (here, there) = (self.vector(a, Prop::Location), self.vector(b, Prop::Location));
        cylinders_meet(
            here,
            there,
            float(a, Prop::CollisionRadius) + float(b, Prop::CollisionRadius),
            float(a, Prop::CollisionHeight) + float(b, Prop::CollisionHeight),
        )
    }

    /// For the engine's own moves: tests off, never refused, touches nothing. Script's `SetLocation` is
    /// [`Vm::far_move_actor`].
    pub fn set_location(&mut self, actor: ObjectId, location: Vector) -> Result<(), Error> {
        self.place_far_moved(actor, location, true)
    }

    /// `no_check` is retail's `bIgnoreEncroachment`. No kill here: `EncroachedBy` and `EncroachingOn` are
    /// script's (`Pawn`: the telefrag, and `bWarping` refusing it).
    pub(crate) fn far_move_actor(&mut self, actor: ObjectId, location: Vector, no_check: bool) -> Result<bool, Error> {
        if self.flag_of(actor, Prop::bStatic) || !self.flag_of(actor, Prop::bMovable) {
            return Ok(false);
        }
        let flags = [Prop::bCollideWorld, Prop::bCollideWhenPlacing, Prop::bSkipEncroachmentCheck].map(|name| self.flag_of(actor, name));
        let (fit, encroach) = far_move_tests(no_check, flags);
        let mut to = location;
        if fit {
            let (radius, height) = (self.float_of(actor, Prop::CollisionRadius), self.float_of(actor, Prop::CollisionHeight));
            if !crate::find_spot::find_spot(self, Vector::new(radius, radius, height), &mut to) {
                return Ok(false);
            }
        }
        if encroach && self.actor_encroachment(actor, to)? {
            return Ok(false);
        }
        if !self.is_deleted(actor) {
            self.place_far_moved(actor, to, no_check)?;
        }
        Ok(true)
    }

    /// `bJustTeleported` makes a warp zone's `ActorEntered` skip the actor, so one warped into the other
    /// side's zone is not sent back. `Attached` actors move on their own, result ignored; not turned.
    fn place_far_moved(&mut self, actor: ObjectId, location: Vector, no_check: bool) -> Result<(), Error> {
        self.set(actor, Prop::bJustTeleported, Value::Bool(true));
        let from = self.vector(actor, Prop::Location);
        for other in self.object_list(actor, Prop::Attached) {
            if !self.is_deleted(other) {
                let to = (self.vector(other, Prop::Location) + location) - from;
                self.far_move_actor(other, to, no_check)?;
            }
        }
        self.set_vector(actor, Prop::Location, location);
        self.relocate(actor);
        self.set_zone(actor, false, false)?;
        Ok(())
    }

    fn overlap_side(&self, actor: ObjectId) -> OverlapSide {
        OverlapSide {
            brush: self.object_of(actor, Prop::Brush).is_some() && self.is_a(actor, Kind::Brush),
            encroacher: self.is_encroacher(actor),
            only_affect_pawns: self.flag_of(actor, Prop::bOnlyAffectPawns),
            pawn: self.is_a(actor, Kind::Pawn),
            blocks_extent: self.flag_of(actor, Prop::bBlockNonZeroExtentTraces),
        }
    }

    pub(crate) fn may_overlap(&self, actor: ObjectId, other: ObjectId) -> bool {
        self.may_overlap_from(self.overlap_side(actor), actor, other)
    }

    fn may_overlap_from(&self, side: OverlapSide, actor: ObjectId, other: ObjectId) -> bool {
        overlap_allowed(side, self.overlap_side(other), false)
            && self.object_of(actor, Prop::Owner) != Some(other)
            && actor != other
            && !self.is_based_on(actor, other)
            && !self.is_based_on(other, actor)
    }

    /// Begins and ends touches for `actor` where it stands; both blocking, they bump instead. Both actors
    /// get the event, `actor` first. An unchanged touched set writes nothing and builds no lists.
    pub fn update_touching(&mut self, actor: ObjectId) -> Result<(), Error> {
        let flag = |vm: &Self, object: ObjectId, name: Prop| vm.flag_of(object, name);
        let touching = |vm: &Self, object: ObjectId| vm.object_list(object, Prop::Touching);
        let collides = flag(self, actor, Prop::bCollideActors) && !self.is_deleted(actor);
        let now: Vec<ObjectId> = if collides {
            let at = self.vector(actor, Prop::Location);
            let (radius, height) = (self.float_of(actor, Prop::CollisionRadius), self.float_of(actor, Prop::CollisionHeight));
            let blocks = flag(self, actor, Prop::bBlockActors);
            let side = self.overlap_side(actor);
            let by_primitive = self.overlaps_by_primitive(actor);
            let cylinders = |vm: &Self, other: ObjectId| {
                let (r, h) = (vm.float_of(other, Prop::CollisionRadius), vm.float_of(other, Prop::CollisionHeight));
                cylinders_meet(at, vm.vector(other, Prop::Location), radius + r, height + h)
            };
            let can_touch = |vm: &Self, other: ObjectId| {
                other != actor
                    && !vm.is_deleted(other)
                    && vm.is_a(other, Kind::Actor)
                    && flag(vm, other, Prop::bCollideActors)
                    && !(blocks && flag(vm, other, Prop::bBlockActors))
                    && (by_primitive || cylinders(vm, other) || vm.overlaps_by_primitive(other))
                    && vm.may_overlap_from(side, actor, other)
            };
            let nearby = self.colliders_near([at[0], at[1]], [at[0], at[1]], radius);
            self.count(crate::profiling::Work::Touch);
            self.count_n(crate::profiling::Work::TouchCandidate, nearby.len());
            let overlapping: Vec<ObjectId> = nearby.iter().map(|e| e.actor).filter(|&other| can_touch(self, other) && self.shapes_overlap(actor, other)).collect();
            self.give_entries(nearby);
            overlapping
        } else {
            Vec::new()
        };
        let unchanged = self
            .array_words(actor, Prop::Touching)
            .map_or(now.is_empty(), |words| same_members(words.iter().filter_map(|&w| crate::memory::object(w)), &now));
        if unchanged {
            return Ok(());
        }
        let before: Vec<ObjectId> = touching(self, actor);
        // Retail's order: `EndTouch` the ones left, then `BeginTouch` the new ones.
        for &other in before.iter().filter(|o| !now.contains(o)) {
            self.end_touch(actor, other)?;
        }
        for &other in now.iter().filter(|o| !before.contains(o)) {
            self.begin_touch(actor, other)?;
        }
        Ok(())
    }

    fn touches(&self, actor: ObjectId, other: ObjectId) -> bool {
        self.array_words(actor, Prop::Touching).is_some_and(|words| words.iter().any(|&w| crate::memory::object(w) == Some(other)))
    }

    fn drop_touching(&mut self, actor: ObjectId, other: ObjectId) {
        let kept = self.object_list(actor, Prop::Touching).into_iter().filter(|&o| o != other).map(|o| Value::Object(Some(o))).collect();
        self.set(actor, Prop::Touching, Value::Array(kept));
    }

    fn end_touch(&mut self, actor: ObjectId, other: ObjectId) -> Result<(), Error> {
        if self.touches(actor, other) {
            self.event(actor, "UnTouch", [Value::Object(Some(other))])?;
        }
        self.drop_touching(actor, other);
        if self.touches(other, actor) {
            self.event(other, "UnTouch", [Value::Object(Some(actor))])?;
            self.drop_touching(other, actor);
        }
        Ok(())
    }

    pub(crate) fn end_touches(&mut self, actor: ObjectId) -> Result<(), Error> {
        for other in self.object_list(actor, Prop::Touching) {
            if self.is_deleted(other) || !self.may_overlap(actor, other) || !self.shapes_overlap(actor, other) {
                self.end_touch(actor, other)?;
            }
        }
        Ok(())
    }

    /// `other`'s side runs only if `other` is still on `actor`'s list afterwards, as retail. An `other`
    /// destroyed during `actor`'s `Touch` gets none: its destroy emptied the lists.
    pub(crate) fn begin_touch(&mut self, actor: ObjectId, other: ObjectId) -> Result<(), Error> {
        if self.touch_side(actor, other)? && !self.is_deleted(other) {
            self.touch_side(other, actor)?;
        }
        Ok(())
    }

    fn touch_side(&mut self, actor: ObjectId, other: ObjectId) -> Result<bool, Error> {
        if self.touches(actor, other) {
            return Ok(true);
        }
        let mut list = match self.get(actor, Prop::Touching) {
            Some(Value::Array(list)) => list,
            _ => Vec::new(),
        };
        list.push(Value::Object(Some(other)));
        self.set(actor, Prop::Touching, Value::Array(list));
        self.link_static(actor);
        self.event(actor, "Touch", [Value::Object(Some(other))])?;
        Ok(!self.is_deleted(actor) && self.touches(actor, other))
    }

    /// Overlap test as the hash query would find them (the query's own filter is unverified).
    fn overlapping(&mut self, actor: ObjectId) -> Vec<ObjectId> {
        let at = self.vector(actor, Prop::Location);
        let nearby = self.colliders_near([at[0], at[1]], [at[0], at[1]], self.float_of(actor, Prop::CollisionRadius));
        let overlapping = nearby
            .iter()
            .map(|e| e.actor)
            .filter(|&other| {
                other != actor
                    && self.is_a(other, Kind::Actor)
                    && !self.is_deleted(other)
                    && self.flag_of(other, Prop::bCollideActors)
                    && self.may_overlap(actor, other)
                    && self.shapes_overlap(actor, other)
            })
            .collect();
        self.give_entries(nearby);
        overlapping
    }

    fn touch_overlapped(&mut self, actor: ObjectId) -> Result<(), Error> {
        for other in self.overlapping(actor) {
            if self.is_deleted(actor) {
                break;
            }
            if !self.is_blocked_by(actor, other) {
                self.begin_touch(actor, other)?;
            }
        }
        Ok(())
    }

    pub(crate) fn set_collision_before(&mut self, actor: ObjectId, was: bool, now: bool) -> Result<(), Error> {
        if was && !now {
            for other in self.object_list(actor, Prop::Touching) {
                self.end_touch(other, actor)?;
            }
        }
        Ok(())
    }

    /// After the new flags are set: only collision turned on from off touches.
    pub(crate) fn set_collision_after(&mut self, actor: ObjectId, was: bool, now: bool) -> Result<(), Error> {
        if !was && now {
            self.touch_overlapped(actor)?;
        }
        Ok(())
    }

    pub fn mark_rendered(&mut self, actor: ObjectId) -> Result<(), Error> {
        let Some(level) = self.level else { return Ok(()) };
        let time = self.get(level, Prop::TimeSeconds).unwrap_or(Value::Float(0.0));
        self.set(actor, Prop::LastRenderTime, time);
        Ok(())
    }

    /// `axes` in world directions. No touch events: bone attachments move only when their base is drawn,
    /// so scripts read where they were last drawn (a `Head`'s `Location` for head shots).
    pub fn place_attachment(&mut self, actor: ObjectId, location: Vector, axes: [Vector; 3]) {
        if self.is_deleted(actor) || self.object_of(actor, Prop::Base).is_none() || !self.on_bone(actor) {
            return;
        }
        self.set(actor, Prop::Location, Value::Vector(location));
        self.set(actor, Prop::Rotation, Value::Rotator(crate::natives::orthogonal_rotation(axes[0], axes[1], axes[2])));
        self.volume_moved(actor);
        self.relocate(actor);
    }

    /// In retail order. `Attached` detach by a live index walk, so retail skips every other one. After
    /// every event a nested destroy that finished ends this one. The object and its lists stay, as retail.
    pub fn destroy(&mut self, actor: ObjectId) -> Result<bool, Error> {
        if self.flag_of(actor, Prop::bStatic) || self.flag_of(actor, Prop::bNoDelete) {
            return Ok(false);
        }
        if self.is_deleted(actor) {
            return Ok(true);
        }
        self.event(actor, "EndState", Vec::new())?;
        if self.is_deleted(actor) {
            return Ok(true);
        }
        self.event(actor, "Destroyed", Vec::new())?;
        if self.object_of(actor, Prop::Base).is_some() {
            self.set_base(actor, None, [0.0, 0.0, 1.0].into())?;
            if self.is_deleted(actor) {
                return Ok(true);
            }
        }
        let mut i = 0;
        while let Some(word) = self.array_words(actor, Prop::Attached).and_then(|words| words.get(i).copied()) {
            if let Some(attached) = crate::memory::object(word) {
                self.set_base(attached, None, [0.0, 0.0, 1.0].into())?;
            }
            i += 1;
        }
        if self.is_deleted(actor) {
            return Ok(true);
        }
        // The walk sees actors spawned during its events (retail's list grows at the end).
        self.list_linked_statics();
        let mut at = None;
        while let Some(other) = self.next_destroy_walk_actor(at) {
            at = Some(other);
            if self.object_of(other, Prop::Owner) == Some(actor) {
                self.event(actor, "LostChild", [Value::Object(Some(other))])?;
                self.set(other, Prop::Owner, Value::Object(None));
                if self.is_deleted(actor) {
                    return Ok(true);
                }
            }
            if self.touches(other, actor) {
                self.drop_touching(actor, other);
                self.event(other, "UnTouch", [Value::Object(Some(actor))])?;
                self.drop_touching(other, actor);
                if self.is_deleted(actor) {
                    return Ok(true);
                }
            }
        }
        if let Some(owner) = self.object_of(actor, Prop::Owner) {
            self.event(owner, "LostChild", [Value::Object(Some(actor))])?;
            if self.is_deleted(actor) {
                return Ok(true);
            }
        }
        self.script.states.leave(actor);
        self.set(actor, Prop::bDeleteMe, Value::Bool(true));
        self.store.actor_ids.remove(&actor);
        self.store.dynamic_ids.removed(actor);
        self.relocate(actor);
        self.forget_placed_shape(actor);
        Ok(true)
    }

    pub(crate) fn profile_start(&self) -> Option<std::time::Instant> {
        self.diagnostics.tick_profile.is_some().then(std::time::Instant::now)
    }

    pub(crate) fn profile_phase(&mut self, name: &str, start: Option<std::time::Instant>) {
        if let (Some(start), Some(profile)) = (start, &mut self.diagnostics.tick_profile) {
            add_time(profile, name, start.elapsed());
        }
    }

    #[inline]
    pub fn is_deleted(&self, actor: ObjectId) -> bool {
        self.store.is_deleted(actor)
    }

    /// In retail's order: animation, `Tick` event, state code, timer and `LifeSpan`, physics.
    pub fn tick_actor(&mut self, actor: ObjectId, delta: f32) -> Result<(), Error> {
        if self.is_deleted(actor) {
            return Ok(());
        }
        if self.rests(actor)? {
            self.age(actor, delta)?;
            return Ok(());
        }
        let Some(delta) = self.fps_pawn_tick(actor, delta)? else { return Ok(()) };
        let phase = self.profile_start();
        self.update_animation(actor, delta)?;
        self.profile_phase("phase: animation", phase);
        if self.is_deleted(actor) {
            return Ok(());
        }
        // A player controller gets no `Tick` event; the local player's reads input and gets `PlayerTick`.
        let phase = self.profile_start();
        if !self.is_a(actor, Kind::PlayerController) {
            self.event(actor, "Tick", [Value::Float(delta)])?;
        } else if self.player() == Some(actor) {
            self.player_tick(actor, delta)?;
        }
        self.profile_phase("phase: timer and Tick", phase);
        let phase = self.profile_start();
        self.tick(actor, delta)?;
        self.profile_phase("phase: state code", phase);
        // An actor whose `LifeSpan` runs out is destroyed without its physics this frame.
        if self.is_deleted(actor) {
            return Ok(());
        }
        if let Some(Value::Float(rate)) = self.get(actor, Prop::TimerRate)
            && rate > 0.0
        {
            let counter = match self.get(actor, Prop::TimerCounter) {
                Some(Value::Float(counter)) => counter + delta,
                _ => delta,
            };
            if counter >= rate {
                if self.flag_of(actor, Prop::bTimerLoop) {
                    self.set(actor, Prop::TimerCounter, Value::Float(counter - rate));
                } else {
                    self.set(actor, Prop::TimerRate, Value::Float(0.0));
                    self.set(actor, Prop::TimerCounter, Value::Float(0.0));
                }
                self.event(actor, "Timer", Vec::new())?;
            } else {
                self.set(actor, Prop::TimerCounter, Value::Float(counter));
            }
        }
        if self.age(actor, delta)? || self.is_deleted(actor) {
            return Ok(());
        }
        if self.is_a(actor, Kind::Pawn) {
            let phase = self.profile_start();
            let result = self.pawn_physics(actor, delta);
            self.profile_phase("phase: pawn physics", phase);
            result?;
            if self.physics_of(actor) != Some(EPhysics::None) {
                let acceleration = self.vector(actor, Prop::Acceleration);
                self.set_vector(actor, Prop::OldAcceleration, acceleration);
            }
            if self.is_deleted(actor) {
                return Ok(());
            }
            let phase = self.profile_start();
            let result = self.pawn_tick_special(actor, delta);
            self.profile_phase("phase: movement animation", phase);
            result
        } else if self.is_a(actor, Kind::Mover) {
            // Retail's mover physics turns movers too, after their move.
            self.mover_physics(actor, delta)?;
            self.actor_rotation(actor, delta)
        } else {
            let phase = self.profile_start();
            self.actor_physics(actor, delta)?;
            if self.is_deleted(actor) {
                return Ok(());
            }
            self.actor_rotation(actor, delta)?;
            self.profile_phase("phase: actor physics", phase);
            if self.is_a(actor, Kind::Controller) {
                let phase = self.profile_start();
                if self.is_a(actor, Kind::PlayerController) {
                    self.player_sight(actor, delta)?;
                } else {
                    self.controller_tick(actor, delta)?;
                }
                self.profile_phase("phase: controller tick", phase);
            }
            if self.is_a(actor, Kind::SceneManager) {
                self.scene_tick(actor, delta)?;
            }
            Ok(())
        }
    }

    fn age(&mut self, actor: ObjectId, delta: f32) -> Result<bool, Error> {
        let life = self.float_of(actor, Prop::LifeSpan);
        if life == 0.0 {
            return Ok(false);
        }
        let left = life - delta;
        self.set(actor, Prop::LifeSpan, Value::Float(left));
        if left > 1e-4 {
            return Ok(false);
        }
        self.destroy(actor)?;
        Ok(true)
    }

    fn rests(&mut self, actor: ObjectId) -> Result<bool, Error> {
        if [Kind::Pawn, Kind::Controller, Kind::Mover, Kind::SceneManager].into_iter().any(|kind| self.is_a(actor, kind))
            || self.has_animation(actor)
            || self.physics_of(actor) != Some(EPhysics::None)
            || self.float_of(actor, Prop::TimerRate) > 0.0
        {
            return Ok(false);
        }
        if let Some(state) = self.script.states.objects.get(&actor)
            && (state.code.is_some()
                || state.sleep > 0.0
                || state.finish_anim.is_some()
                || state.finish_interpolation.is_some()
                || state.latent_move.is_some())
        {
            return Ok(false);
        }
        if self.rotator_of(actor, Prop::RotationRate) != [0; 3].into()
            && !self.flag_of(actor, Prop::bInterpolating)
            && (self.flag_of(actor, Prop::bRotateToDesired) || self.flag_of(actor, Prop::bFixedRotationDir))
        {
            return Ok(false);
        }
        Ok(!self.is_probing(actor, PROBE_TICK)?)
    }

    /// Runs before the stock tick. Pawns owned by a player's controller, or with `bIgnoreTimeDilation`,
    /// tick in real time (`delta / TimeDilation`). `None`: the controller stays in stasis, no tick.
    fn fps_pawn_tick(&mut self, pawn: ObjectId, delta: f32) -> Result<Option<f32>, Error> {
        if !self.is_a(pawn, Kind::FPSPawn) {
            return Ok(Some(delta));
        }
        let now = self.level.map_or(0.0, |level| self.float_of(level, Prop::TimeSeconds));
        let rendered = self.float_of(pawn, Prop::LastRenderTime);
        if let Some(controller) = self.object_of(pawn, Prop::Controller) {
            if self.flag_of(controller, Prop::bStasis) {
                if !self.flag_of(pawn, Prop::bSliderStasis) && rendered + delta >= now {
                    self.event(controller, "ComeOutOfStasis", [Value::Bool(false)])?;
                }
            } else {
                let wait = self.float_of(pawn, Prop::TimeTillStasis);
                if self.flag_of(pawn, Prop::bAllowStasis) && wait > 0.0 && now - rendered > wait && !self.flag_of(controller, Prop::bPendingStasis) {
                    self.set(controller, Prop::bPendingStasis, Value::Bool(true));
                }
            }
            if self.flag_of(controller, Prop::bStasis) {
                return Ok(None);
            }
        }
        let owner = self.object_of(pawn, Prop::Owner);
        let real_time = self.flag_of(pawn, Prop::bIgnoreTimeDilation) || owner.is_some_and(|o| self.is_a(o, Kind::PlayerController) || self.is_player(o));
        let dilation = self.level.map_or(1.0, |level| self.float_of(level, Prop::TimeDilation));
        Ok(Some(if real_time && dilation > 0.0 { delta / dilation } else { delta }))
    }

    pub fn tick_actors(&mut self, delta: f32) -> Result<(), Error> {
        self.world.frame += 1;
        let actors: Vec<ObjectId> = self.actors().filter(|&o| self.is_a(o, Kind::Actor)).collect();
        for actor in actors {
            self.tick_actor(actor, delta)?;
        }
        Ok(())
    }

    /// `options` are a URL's (`?Name=Value...`), with the portal after a `#`; the `[DefaultPlayer]`
    /// options of `User.ini` it lacks are added (`Name=` is how `Login` finds a saved game's player).
    pub fn login(&mut self, options: &str) -> Result<Option<(ObjectId, Option<ObjectId>)>, Error> {
        let Some(level) = self.level() else { return Ok(None) };
        let Some(Value::Object(Some(game))) = self.get(level, Prop::Game) else { return Ok(None) };
        let Some(class) = self.object(game).class else { return Ok(None) };
        let Some(login) = self.find_object_function(game, "Login")? else { return Ok(None) };
        let mut url = crate::travel::TravelUrl::parse(options);
        self.add_default_player_options(&mut url);
        let args = vec![Value::Str(url.portal.clone()), Value::Str(url.options_text()), Value::Str(String::new())];
        let Some(Value::Object(Some(player))) = self.call(&login, Some(game), class, args)? else { return Ok(None) };
        self.set_player(player)?;
        if let Some(post_login) = self.find_object_function(game, "PostLogin")? {
            self.call(&post_login, Some(game), class, vec![Value::Object(Some(player))])?;
        }
        let pawn = match self.get(player, Prop::Pawn) {
            Some(Value::Object(pawn)) => pawn,
            _ => None,
        };
        if let Some(pawn) = pawn {
            let mut accepted = vec![player, pawn];
            accepted.extend(self.arrive(pawn)?);
            for &actor in &accepted {
                self.event(actor, "TravelPreAccept", Vec::new())?;
            }
            self.event(game, "AcceptInventory", [Value::Object(Some(pawn))])?;
            for &actor in &accepted {
                self.event(actor, "TravelPostAccept", Vec::new())?;
            }
        }
        Ok(Some((player, pawn)))
    }

    pub(crate) fn load_level(&mut self, map: p2_package::PackageId) -> Result<Vec<ObjectId>, Error> {
        let mut actors = Vec::new();
        for export in 0..self.loader.package(map).package.exports.len() {
            let object = self.load_object(map, export)?;
            if self.is_a(object, Kind::LevelInfo) && self.level.is_none() {
                self.level = Some(object);
            }
            if self.is_a(object, Kind::Actor) {
                actors.push(object);
            }
            if self.is_a(object, Kind::SceneManager) {
                self.construct_scene_manager(object);
            }
        }
        self.build_world(map);
        self.map_name = self.loader.package(map).name.clone();
        Ok(actors)
    }

    pub fn begin_play(
        &mut self,
        map: p2_package::PackageId,
        game_class: ClassId,
        options: &str,
    ) -> Result<Vec<(String, &'static str, Error)>, Error> {
        let actors = self.load_level(map)?;
        let level = self.level.ok_or(Error::Unsupported("map without a LevelInfo".to_owned()))?;
        self.url_options = crate::TravelUrl::parse(options).options;
        crate::natives::update_time(self, level);
        let mut errors = Vec::new();
        // `LevelInfo.bStartup` is set while the map's actors begin play (pawns placed in the map spawn their
        // controllers in `PostBeginPlay` only then).
        self.set(level, Prop::bStartup, Value::Bool(true));
        let game = self.spawn(game_class, None, [None, None])?;
        self.set(level, Prop::Game, Value::Object(game));
        if let Some(game) = game
            && let Err(e) = self.event(game, "InitGame", [Value::Str(options.to_owned()), Value::Str(String::new())])
        {
            errors.push((self.object(game).path.clone(), "InitGame", e));
        }
        for event in ["PreBeginPlay", "BeginPlay", "PostBeginPlay", "SetInitialState"] {
            if event == "PostBeginPlay" {
                for &actor in &actors {
                    if let Err(e) = self.set_zone(actor, true, true) {
                        errors.push((self.object(actor).path.clone(), "SetZone", e));
                    }
                }
            }
            for &actor in &actors {
                if self.is_deleted(actor) {
                    continue;
                }
                if let Err(e) = self.event(actor, event, Vec::new()) {
                    errors.push((self.object(actor).path.clone(), event, e));
                }
                if event == "PostBeginPlay" && self.is_a(actor, Kind::SceneManager) && !self.is_deleted(actor) {
                    self.prepare_path(actor);
                }
            }
        }
        self.set(level, Prop::bStartup, Value::Bool(false));
        Ok(errors)
    }

    pub fn paused(&self) -> bool {
        self.level.is_some_and(|level| matches!(self.get(level, Prop::Pauser), Some(Value::Object(Some(_)))))
    }

    pub fn tick_level(&mut self, real: f32) -> Vec<(String, Error)> {
        self.store.count_work = self.diagnostics.count_work;
        self.world.frame += 1;
        let mut delta = real;
        let mut paused = false;
        if let Some(level) = self.level {
            crate::natives::update_time(self, level);
            let dilation = match self.get(level, Prop::TimeDilation) {
                Some(Value::Float(dilation)) => dilation,
                _ => 1.0,
            };
            delta = real * dilation;
            paused = self.object_of(level, Prop::Pauser).is_some();
            if !paused {
                self.set(level, Prop::TimeSeconds, Value::Float(self.float_of(level, Prop::TimeSeconds) + delta));
            }
            self.set(level, Prop::TimeSecondsAlways, Value::Float(self.float_of(level, Prop::TimeSecondsAlways) + delta));
            // `ServerTravel` set `NextURL`: the host travels, from `Vm::travel`, once `NextSwitchCountdown` runs out.
            if let Some(Value::Str(url)) = self.get(level, Prop::NextURL)
                && !url.is_empty()
            {
                let countdown = match self.get(level, Prop::NextSwitchCountdown) {
                    Some(Value::Float(countdown)) => countdown - delta,
                    _ => 0.0,
                };
                self.set(level, Prop::NextSwitchCountdown, Value::Float(countdown));
                if countdown <= 0.0 {
                    self.host.commands.push(HostCommand::Travel(url));
                    self.set(level, Prop::NextURL, Value::Str(String::new()));
                }
            }
        }
        let phase = self.profile_start();
        let dynamic = self.dynamic_actors();
        let actors = if paused { dynamic.into_iter().filter(|&o| self.flag_of(o, Prop::bAlwaysTick)).collect() } else { dynamic };
        self.profile_phase("actor list", phase);
        let delta = delta.clamp(MIN_TICK, MAX_TICK);
        let phase = self.profile_start();
        let mut errors = if paused { Vec::new() } else { self.karma_tick(delta) };
        self.profile_phase("karma", phase);
        for actor in actors {
            let started = self.profile_start();
            if let Err(e) = self.tick_actor(actor, delta) {
                errors.push((self.object(actor).path.clone(), e));
            }
            if let Some(started) = started {
                let class = self.store.names[self.object(actor).class_name.0 as usize].as_str();
                add_time(self.diagnostics.tick_profile.get_or_insert_default(), class, started.elapsed());
            }
        }
        if let Err(e) = self.subtitle_tick() {
            errors.push(("subtitles".to_owned(), e));
        }
        let phase = self.profile_start();
        if let Err(e) = self.client_tick(delta) {
            errors.push(("interactions".to_owned(), e));
        }
        self.profile_phase("interactions", phase);
        errors
    }

    pub fn class_of_object(&mut self, object: ObjectId) -> Result<Option<ClassId>, Error> {
        if let Some(&class) = self.script.lookups.class_objects.get(&object) {
            return Ok(class);
        }
        let Some((package, name)) = self.object(object).path.split_once('.') else { return Ok(None) };
        let (package, name) = (package.to_owned(), name.to_owned());
        // Not found is kept too (each miss scanned the package's exports by name, ~18 us a `ClassIsChildOf`): the
        // loaded packages only change with the loader, which clears the cache (`travel.rs`).
        let class = match self.find_class(&package, &name) {
            Ok(class) => Some(class),
            Err(Error::ClassNotFound(_) | Error::PackageNotLoaded(_)) => None,
            Err(e) => return Err(e),
        };
        self.script.lookups.class_objects.insert(object, class);
        Ok(class)
    }
}

/// Both strictly less, compared squared, height first, as retail.
pub(crate) fn cylinders_meet(here: Vector, there: Vector, radii: f32, heights: f32) -> bool {
    let (dx, dy, dz) = (here[0] - there[0], here[1] - there[1], here[2] - there[2]);
    dz * dz < heights * heights && dy * dy + dx * dx < radii * radii
}

#[expect(clippy::struct_excessive_bools, reason = "the actor flags retail's IsOverlapping reads, one field each")]
#[derive(Clone, Copy, Default)]
struct OverlapSide {
    brush: bool,
    encroacher: bool,
    only_affect_pawns: bool,
    pawn: bool,
    blocks_extent: bool,
}

/// Retail's overlap rejects, before its shape test. Retail tests Owner one way only, and reads
/// `bBlockNonZeroExtentTraces` here, not `bCollideActors`.
fn overlap_allowed(a: OverlapSide, b: OverlapSide, related: bool) -> bool {
    !(a.brush && b.brush)
        && !related
        && !(a.encroacher && b.encroacher)
        && (!a.only_affect_pawns || b.pawn)
        && (!b.only_affect_pawns || a.pawn)
        && a.blocks_extent
        && b.blocks_extent
}

fn encroachment_tested([collide_actors, block_actors, block_players, encroacher]: [bool; 4]) -> bool {
    collide_actors || block_actors || block_players || encroacher
}

fn next_level_actor(actors: &std::collections::BTreeSet<ObjectId>, after: Option<ObjectId>) -> Option<ObjectId> {
    match after {
        Some(after) => actors.range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded)).next().copied(),
        None => actors.first().copied(),
    }
}

fn same_members(before: impl Iterator<Item = ObjectId> + Clone, now: &[ObjectId]) -> bool {
    before.clone().all(|old| now.contains(&old)) && now.iter().all(|&new| before.clone().any(|old| old == new))
}

/// What a far move tests, given `bCollideWorld`, `bCollideWhenPlacing` and `bSkipEncroachmentCheck`
/// (the first two names unverified).
fn far_move_tests(no_check: bool, [collide_world, collide_when_placing, skip_encroachment]: [bool; 3]) -> (bool, bool) {
    (!no_check && (collide_world || collide_when_placing), !no_check && !skip_encroachment)
}

#[cfg(test)]
mod tests {
    use super::{
        BEGUN_EVENTS, DynamicActors, OverlapSide, SPAWN_EVENTS, cylinders_meet, encroachment_tested,
        next_level_actor, overlap_allowed, same_members,
    };
    use crate::ObjectId;

    #[test]
    fn tick_probe_bit_is_the_tick_name_s() {
        assert_eq!(crate::states::probe_bit("Tick"), Some(super::PROBE_TICK));
    }

    #[test]
    fn destroy_level_walk() {
        let mut actors: std::collections::BTreeSet<ObjectId> = [1, 3, 5].map(ObjectId).into();
        assert_eq!(next_level_actor(&actors, None), Some(ObjectId(1)));
        actors.insert(ObjectId(9));
        actors.remove(&ObjectId(3));
        assert_eq!(next_level_actor(&actors, Some(ObjectId(1))), Some(ObjectId(5)));
        assert_eq!(next_level_actor(&actors, Some(ObjectId(5))), Some(ObjectId(9)));
        assert_eq!(next_level_actor(&actors, Some(ObjectId(9))), None);
        assert_eq!(next_level_actor(&std::collections::BTreeSet::new(), None), None);
    }

    #[test]
    fn far_move_test_gate() {
        assert_eq!(super::far_move_tests(true, [true, true, false]), (false, false));
        assert_eq!(super::far_move_tests(false, [false, false, false]), (false, true));
        assert_eq!(super::far_move_tests(false, [true, false, false]), (true, true));
        assert_eq!(super::far_move_tests(false, [false, true, true]), (true, false));
    }

    #[test]
    fn encroachment_test_gate() {
        assert!(!encroachment_tested([false, false, false, false]));
        assert!(encroachment_tested([true, false, false, false]));
        assert!(encroachment_tested([false, true, false, false]));
        assert!(encroachment_tested([false, false, true, false]));
        assert!(encroachment_tested([false, false, false, true]));
    }

    #[test]
    fn spawn_event_order() {
        assert_eq!(SPAWN_EVENTS, ["Spawned", "PreBeginPlay", "BeginPlay"]);
        assert_eq!(BEGUN_EVENTS, ["PostBeginPlay", "SetInitialState", "PostNetBeginPlay"]);
    }

    #[test]
    fn overlap_rejects() {
        let plain = OverlapSide { blocks_extent: true, ..OverlapSide::default() };
        let allowed = overlap_allowed;
        assert!(allowed(plain, plain, false));
        assert!(!allowed(plain, plain, true));
        assert!(!allowed(plain, plain, true));
        assert!(!allowed(plain, plain, true));
        let brush = OverlapSide { brush: true, ..plain };
        assert!(!allowed(brush, brush, false));
        assert!(allowed(brush, plain, false));
        let encroacher = OverlapSide { encroacher: true, ..plain };
        assert!(!allowed(encroacher, encroacher, false));
        assert!(allowed(encroacher, plain, false));
        let pawns_only = OverlapSide { only_affect_pawns: true, ..plain };
        let pawn = OverlapSide { pawn: true, ..plain };
        assert!(!allowed(pawns_only, plain, false));
        assert!(!allowed(plain, pawns_only, false));
        assert!(allowed(pawns_only, pawn, false));
        assert!(allowed(pawn, pawns_only, false));
        let no_extent = OverlapSide::default();
        assert!(!allowed(no_extent, plain, false));
        assert!(!allowed(plain, no_extent, false));
    }

    #[test]
    fn cylinders_meet_strictly_inside() {
        let at = |x: f32, y: f32, z: f32| p2_math::Vector::from([x, y, z]);
        assert!(cylinders_meet(at(0.0, 0.0, 0.0), at(3.0, 4.0, 0.0), 5.5, 1.0));
        assert!(!cylinders_meet(at(0.0, 0.0, 0.0), at(3.0, 4.0, 0.0), 5.0, 1.0));
        assert!(cylinders_meet(at(0.0, 0.0, 10.0), at(0.0, 0.0, 0.5), 1.0, 10.0));
        assert!(!cylinders_meet(at(0.0, 0.0, 0.0), at(0.0, 0.0, -10.0), 1.0, 10.0));
    }

    #[test]
    fn same_members_ignores_order() {
        let ids = |list: &[u32]| list.iter().map(|&i| ObjectId(i)).collect::<Vec<_>>();
        let (a, b) = (ids(&[1, 2, 3]), ids(&[3, 1, 2]));
        assert!(same_members(a.iter().copied(), &b));
        assert!(same_members(std::iter::empty(), &[]));
        assert!(!same_members(a.iter().copied(), &ids(&[1, 2])));
        assert!(!same_members(ids(&[1, 2]).into_iter(), &a));
        assert!(!same_members(std::iter::empty(), &ids(&[4])));
    }

    #[test]
    fn dynamic_actors_keep_creation_order() {
        let mut list = DynamicActors::default();
        let is_static = |actor: ObjectId| actor == ObjectId(3);
        for id in [1, 2, 3, 4] {
            list.added(ObjectId(id));
        }
        assert_eq!(list.walk(is_static), [ObjectId(1), ObjectId(2), ObjectId(4)]);
        list.removed(ObjectId(2));
        list.added(ObjectId(5));
        list.added(ObjectId(6));
        list.removed(ObjectId(6));
        assert_eq!(list.walk(is_static), [ObjectId(1), ObjectId(4), ObjectId(5)]);
        list.added(ObjectId(5));
        assert_eq!(list.walk(is_static), [ObjectId(1), ObjectId(4), ObjectId(5)]);
        list.added(ObjectId(7));
        list.clear();
        assert!(list.walk(is_static).is_empty());
    }

    #[test]
    fn the_destroy_walk_merges_linked_statics_in_level_order() {
        let mut list = DynamicActors::default();
        for id in [1, 3, 4, 7] {
            list.added(ObjectId(id));
        }
        list.sort_new(|actor| actor == ObjectId(3) || actor == ObjectId(4));
        list.linked_statics = Some([ObjectId(3), ObjectId(9)].into_iter().collect());
        let mut walked = Vec::new();
        let mut at = None;
        while let Some(actor) = list.next_after(at) {
            walked.push(actor);
            at = Some(actor);
            if actor == ObjectId(3) {
                list.removed(ObjectId(7));
                list.added(ObjectId(8));
                list.sort_new(|_| false);
            }
        }
        assert_eq!(walked, [ObjectId(1), ObjectId(3), ObjectId(8), ObjectId(9)]);
    }
}
