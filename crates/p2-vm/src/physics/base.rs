use p2_math::{Rotator, Vector};
use crate::enums::EPhysics;
use crate::volumes::{OldZone, ZoneEvents};
use crate::{Error, LoaderRef, ObjectId, Prop, Value, Vm, Kind};
use super::{
    Bases, FIND_BASE_DEPTH, MAX_BASE_CHAIN, MAX_OWNER_CHAIN, Sweep, SweepAgainst, attached_turn, carried_motion,
    keeps_base, stops,
};

impl<L: LoaderRef> Vm<L> {
    /// Retail reads neither `AttachmentBone` nor `bHardAttach` here, nor the base's `bCollideActors`.
    /// A refusal moves actors carried before it back by `delta`, not turned back; it stays where it went.
    pub(crate) fn carry_attached(&mut self, base: ObjectId, old_location: Vector, delta: Vector, old_rotation: Rotator, rotation: Rotator) -> Result<bool, Error> {
        let attached = self.store.object_list(base, Prop::Attached);
        let turn = attached_turn(old_rotation, rotation);
        for (i, &other) in attached.iter().enumerate() {
            if self.store.is_deleted(other) {
                continue;
            }
            if rotation != old_rotation
                && let Some(controller) = self.store.object_of(other, Prop::Controller).filter(|_| self.store.is_a(other, Kind::Pawn))
            {
                let view = self.store.rotator_of(controller, Prop::Rotation);
                let mut swung = [0, 1, 2].map(|c| view[c].wrapping_add(turn[c]));
                if !self.store.flag_of(other, Prop::bCrawler) {
                    swung[2] = view.roll;
                }
                self.store.set(controller, Prop::Rotation, Value::Rotator(swung.into()));
            }
            let motion = carried_motion(delta, old_rotation, rotation, self.store.vector(other, Prop::RelativeLocation));
            let own = self.store.rotator_of(other, Prop::Rotation);
            self.store.set(other, Prop::Rotation, Value::Rotator([0, 1, 2].map(|c| own[c].wrapping_add(turn[c])).into()));
            self.move_actor(other, motion, Bases::Ignore)?;
            if self.is_blocked_by(other, base)
                && self.primitive_overlaps(base, other)
                && self.event_bool(base, crate::states::Event::EncroachingOn, [Value::Object(Some(other))])?
            {
                self.set_vector(base, Prop::Location, old_location);
                self.store.set(base, Prop::Rotation, Value::Rotator(old_rotation));
                self.relocate(base);
                for &earlier in &attached[..i] {
                    self.move_actor(earlier, delta * (-1.0), Bases::Ignore)?;
                }
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn update_relative_location(&mut self, actor: ObjectId) {
        if let Some(base) = self.store.object_of(actor, Prop::Base)
            && !self.on_bone(actor)
        {
            let offset = self.store.vector(actor, Prop::Location) - self.store.vector(base, Prop::Location);
            self.set_vector(actor, Prop::RelativeLocation, offset);
        }
    }

    /// Retail's `SetRotation` is a zero-delta move; with nothing attached it only sets the rotation.
    /// An encroacher's encroachment check on this path is not modelled.
    pub(crate) fn turn_attached(&mut self, actor: ObjectId, old_rotation: Rotator) -> Result<(), Error> {
        if self.store.object_list(actor, Prop::Attached).is_empty() {
            return Ok(());
        }
        let (location, rotation) = (self.store.vector(actor, Prop::Location), self.store.rotator_of(actor, Prop::Rotation));
        if !self.carry_attached(actor, location, Vector::ZERO, old_rotation, rotation)? {
            return Ok(());
        }
        if self.store.flag_of(actor, Prop::bCollideActors) && !self.store.is_deleted(actor) {
            self.end_touches(actor)?;
        }
        self.set_zone(actor, ZoneEvents::Sent, OldZone::Region)?;
        Ok(())
    }

    pub(crate) fn is_based_on(&self, actor: ObjectId, other: ObjectId) -> bool {
        std::iter::successors(self.store.object_of(actor, Prop::Base), |&b| self.store.object_of(b, Prop::Base)).take(MAX_BASE_CHAIN).any(|b| b == other)
    }

    pub(super) fn in_owner_chain(&self, actor: ObjectId, other: ObjectId) -> bool {
        std::iter::successors(self.store.object_of(actor, Prop::Owner), |&o| self.store.object_of(o, Prop::Owner)).take(MAX_OWNER_CHAIN).any(|o| o == other)
    }

    /// Relative rotation is not updated. Without a base, or on world geometry, the actor leaves any bone.
    pub(crate) fn set_base(&mut self, actor: ObjectId, base: Option<ObjectId>, floor: Vector) -> Result<(), Error> {
        if self.store.is_a(actor, Kind::Pawn) {
            self.set_vector(actor, Prop::Floor, floor);
        }
        if base == Some(actor) || base.is_some_and(|b| self.is_based_on(b, actor)) {
            return Ok(());
        }
        let old = self.store.object_of(actor, Prop::Base);
        let world = |vm: &Self, b: ObjectId| Some(b) == vm.level || vm.store.flag_of(b, Prop::bWorldGeometry);
        if old != base {
            if let Some(old) = old.filter(|&o| !world(self, o)) {
                if let Some(Value::Array(attached)) = self.store.get(old, Prop::Attached) {
                    let kept = attached.into_iter().filter(|a| *a != Value::Object(Some(actor))).collect();
                    self.store.set(old, Prop::Attached, Value::Array(kept));
                }
                self.event(old, crate::states::Event::Detach, [Value::Object(Some(actor))])?;
            }
            self.store.set(actor, Prop::Base, Value::Object(base));
            if let Some(new) = base
                && !self.on_bone(actor)
            {
                let offset = self.store.vector(actor, Prop::Location) - self.store.vector(new, Prop::Location);
                self.set_vector(actor, Prop::RelativeLocation, offset);
            }
            if let Some(new) = base.filter(|&b| !world(self, b)) {
                if let Some(Value::Array(mut attached)) = self.store.get(new, Prop::Attached) {
                    attached.push(Value::Object(Some(actor)));
                    self.store.set(new, Prop::Attached, Value::Array(attached));
                }
                self.event(new, crate::states::Event::Attach, [Value::Object(Some(actor))])?;
            }
            self.event(actor, crate::states::Event::BaseChange, Vec::new())?;
        }
        let base = self.store.object_of(actor, Prop::Base);
        if base.is_none_or(|b| world(self, b)) && self.on_bone(actor) {
            self.store.set(actor, Prop::AttachmentBone, Value::Name(crate::NameId::NONE));
        }
        Ok(())
    }

    pub(crate) fn on_bone(&self, actor: ObjectId) -> bool {
        matches!(self.store.get(actor, Prop::AttachmentBone), Some(Value::Name(bone)) if bone != crate::NameId::NONE)
    }

    /// Also for actors on a bone or `bHardAttach`: one put on a bone and then set to a base-keeping mode
    /// stays on only if the base is right below it.
    pub(crate) fn set_physics(&mut self, actor: ObjectId, physics: EPhysics, floor: Option<(Option<ObjectId>, Vector)>) -> Result<(), Error> {
        self.set_physics_byte(actor, physics as u8, floor)
    }

    pub(crate) fn set_physics_byte(&mut self, actor: ObjectId, physics: u8, floor: Option<(Option<ObjectId>, Vector)>) -> Result<(), Error> {
        let old = self.store.byte_of(actor, Prop::Physics);
        if old == physics {
            return Ok(());
        }
        self.store.set(actor, Prop::Physics, Value::Byte(physics));
        self.karma_physics_changed(actor, EPhysics::from_byte(old));
        let mode = EPhysics::from_byte(physics);
        if mode.is_some_and(keeps_base) {
            match floor {
                Some((Some(base), normal)) => {
                    if self.store.object_of(actor, Prop::Base) != Some(base) {
                        self.set_base(actor, Some(base), normal)?;
                    }
                }
                _ => self.find_base(actor)?,
            }
        } else if self.store.object_of(actor, Prop::Base).is_some() {
            self.set_base(actor, None, [0.0, 0.0, 1.0].into())?;
        }
        if mode.is_some_and(stops) {
            self.set_vector(actor, Prop::Velocity, Vector::ZERO);
            self.set_vector(actor, Prop::Acceleration, Vector::ZERO);
        }
        if let Some(volume) = self.store.object_of(actor, Prop::PhysicsVolume).filter(|&v| self.store.object(v).class.is_some()) {
            self.event(volume, crate::states::Event::PhysicsChangedFor, [Value::Object(Some(actor))])?;
        }
        Ok(())
    }

    fn find_base(&mut self, actor: ObjectId) -> Result<(), Error> {
        let at = self.store.vector(actor, Prop::Location);
        let (radius, height) = self.store.collision_size(actor);
        let hit = self.sweep(actor, Sweep { start: at, delta: [0.0, 0.0, -FIND_BASE_DEPTH].into(), radius, height }, SweepAgainst::WORLD_AND_ACTORS, None);
        let base = hit.actor;
        if self.store.object_of(actor, Prop::Base) != base {
            self.set_base(actor, base, hit.normal)?;
        }
        Ok(())
    }

    pub(super) fn notify_bump(&mut self, actor: ObjectId, other: ObjectId) -> Result<(), Error> {
        if self.store.is_a(actor, Kind::Pawn)
            && let Some(controller) = self.store.object_of(actor, Prop::Controller)
            && self.event_bool(controller, crate::states::Event::NotifyBump, [Value::Object(Some(other))])?
        {
            return Ok(());
        }
        self.event(actor, crate::states::Event::Bump, [Value::Object(Some(other))])
    }
}
