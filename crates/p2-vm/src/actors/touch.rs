use crate::{Error, Kind, ObjectId, LoaderRef, Prop, Value, Vm};
use super::{OverlapSide, cylinders_meet, overlap_allowed, same_members};

impl<L: LoaderRef> Vm<L> {
    fn overlap_side(&self, actor: ObjectId) -> OverlapSide {
        OverlapSide {
            brush: self.store.object_of(actor, Prop::Brush).is_some() && self.store.is_a(actor, Kind::Brush),
            encroacher: self.is_encroacher(actor),
            only_affect_pawns: self.store.flag_of(actor, Prop::bOnlyAffectPawns),
            pawn: self.store.is_a(actor, Kind::Pawn),
            blocks_extent: self.store.flag_of(actor, Prop::bBlockNonZeroExtentTraces),
        }
    }

    pub(crate) fn may_overlap(&self, actor: ObjectId, other: ObjectId) -> bool {
        self.may_overlap_from(self.overlap_side(actor), actor, other)
    }

    fn may_overlap_from(&self, side: OverlapSide, actor: ObjectId, other: ObjectId) -> bool {
        overlap_allowed(side, self.overlap_side(other), false)
            && self.store.object_of(actor, Prop::Owner) != Some(other)
            && actor != other
            && !self.is_based_on(actor, other)
            && !self.is_based_on(other, actor)
    }

    /// Begins and ends touches for `actor` where it stands; both blocking, they bump instead. Both actors
    /// get the event, `actor` first. An unchanged touched set writes nothing and builds no lists.
    pub fn update_touching(&mut self, actor: ObjectId) -> Result<(), Error> {
        let flag = |vm: &Self, object: ObjectId, name: Prop| vm.store.flag_of(object, name);
        let touching = |vm: &Self, object: ObjectId| vm.store.object_list(object, Prop::Touching);
        let collides = flag(self, actor, Prop::bCollideActors) && !self.store.is_deleted(actor);
        let now: Vec<ObjectId> = if collides {
            let at = self.store.vector(actor, Prop::Location);
            let (radius, height) = (self.store.float_of(actor, Prop::CollisionRadius), self.store.float_of(actor, Prop::CollisionHeight));
            let blocks = flag(self, actor, Prop::bBlockActors);
            let side = self.overlap_side(actor);
            let by_primitive = self.overlaps_by_primitive(actor);
            let cylinders = |vm: &Self, other: ObjectId| {
                let (r, h) = (vm.store.float_of(other, Prop::CollisionRadius), vm.store.float_of(other, Prop::CollisionHeight));
                cylinders_meet(at, vm.store.vector(other, Prop::Location), radius + r, height + h)
            };
            let can_touch = |vm: &Self, other: ObjectId| {
                other != actor
                    && !vm.store.is_deleted(other)
                    && vm.store.is_a(other, Kind::Actor)
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
            .store.array_words(actor, Prop::Touching)
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

    pub(super) fn touches(&self, actor: ObjectId, other: ObjectId) -> bool {
        self.store.array_words(actor, Prop::Touching).is_some_and(|words| words.iter().any(|&w| crate::memory::object(w) == Some(other)))
    }

    pub(super) fn drop_touching(&mut self, actor: ObjectId, other: ObjectId) {
        let kept = self.store.object_list(actor, Prop::Touching).into_iter().filter(|&o| o != other).map(|o| Value::Object(Some(o))).collect();
        self.store.set(actor, Prop::Touching, Value::Array(kept));
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
        for other in self.store.object_list(actor, Prop::Touching) {
            if self.store.is_deleted(other) || !self.may_overlap(actor, other) || !self.shapes_overlap(actor, other) {
                self.end_touch(actor, other)?;
            }
        }
        Ok(())
    }

    /// `other`'s side runs only if `other` is still on `actor`'s list afterwards, as retail. An `other`
    /// destroyed during `actor`'s `Touch` gets none: its destroy emptied the lists.
    pub(crate) fn begin_touch(&mut self, actor: ObjectId, other: ObjectId) -> Result<(), Error> {
        if self.touch_side(actor, other)? && !self.store.is_deleted(other) {
            self.touch_side(other, actor)?;
        }
        Ok(())
    }

    fn touch_side(&mut self, actor: ObjectId, other: ObjectId) -> Result<bool, Error> {
        if self.touches(actor, other) {
            return Ok(true);
        }
        let mut list = match self.store.get(actor, Prop::Touching) {
            Some(Value::Array(list)) => list,
            _ => Vec::new(),
        };
        list.push(Value::Object(Some(other)));
        self.store.set(actor, Prop::Touching, Value::Array(list));
        self.link_static(actor);
        self.event(actor, "Touch", [Value::Object(Some(other))])?;
        Ok(!self.store.is_deleted(actor) && self.touches(actor, other))
    }

    /// Overlap test as the hash query would find them (the query's own filter is unverified).
    pub(super) fn overlapping(&mut self, actor: ObjectId) -> Vec<ObjectId> {
        let at = self.store.vector(actor, Prop::Location);
        let nearby = self.colliders_near([at[0], at[1]], [at[0], at[1]], self.store.float_of(actor, Prop::CollisionRadius));
        let overlapping = nearby
            .iter()
            .map(|e| e.actor)
            .filter(|&other| {
                other != actor
                    && self.store.is_a(other, Kind::Actor)
                    && !self.store.is_deleted(other)
                    && self.store.flag_of(other, Prop::bCollideActors)
                    && self.may_overlap(actor, other)
                    && self.shapes_overlap(actor, other)
            })
            .collect();
        self.give_entries(nearby);
        overlapping
    }

    fn touch_overlapped(&mut self, actor: ObjectId) -> Result<(), Error> {
        for other in self.overlapping(actor) {
            if self.store.is_deleted(actor) {
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
            for other in self.store.object_list(actor, Prop::Touching) {
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
}
