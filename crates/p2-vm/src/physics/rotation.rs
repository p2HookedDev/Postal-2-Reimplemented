use crate::enums::EPhysics;
use crate::{Error, LoaderRef, ObjectId, Prop, Value, Vm, Kind};
use super::{Toward, fixed_turn};

impl<L: LoaderRef> Vm<L> {
    /// Banking from velocity changes is not modelled. Human pawns use the pawn's rate, not retail's.
    pub(super) fn pawn_rotation(&mut self, pawn: ObjectId, delta: f32) {
        let controller = self.store.object_of(pawn, Prop::Controller);
        let human = controller.is_some_and(|c| self.store.is_a(c, Kind::PlayerController));
        let rotate = match controller {
            Some(c) if human => self.store.flag_of(c, Prop::bRotateToDesired),
            other => other.is_some(),
        };
        self.store.set(pawn, Prop::bRotateToDesired, Value::Bool(rotate));
        if let Some(c) = controller.filter(|_| human && rotate) {
            let own = self.store.rotator_of(c, Prop::DesiredRotation);
            let mut desired = self.store.rotator_of(pawn, Prop::DesiredRotation);
            desired.pitch = own.pitch;
            desired.yaw = own.yaw;
            self.store.set(pawn, Prop::DesiredRotation, Value::Rotator(desired));
        }
        let physics = self.store.physics_of(pawn);
        let fixed = self.store.flag_of(pawn, Prop::bFixedRotationDir);
        let rate = self.store.rotator_of(pawn, Prop::RotationRate);
        let turn = (rate.yaw as f32 * delta).round() as i32;
        let mut rotation = self.store.rotator_of(pawn, Prop::Rotation);
        let mut desired = self.store.rotator_of(pawn, Prop::DesiredRotation);
        if let Some(controller) = controller.filter(|_| rotate) {
            if desired.yaw != rotation.yaw {
                rotation.yaw = fixed_turn(rotation.yaw, desired.yaw, turn, Toward::Desired, fixed);
            }
            if matches!(physics, Some(EPhysics::Walking | EPhysics::Falling | EPhysics::Ladder)) && desired.pitch != 0 {
                desired.pitch = 0;
                self.store.set(pawn, Prop::DesiredRotation, Value::Rotator(desired));
            }
            if !(self.store.flag_of(pawn, Prop::bCrawler) && physics == Some(EPhysics::Walking)) && desired.pitch != rotation.pitch {
                rotation.pitch = fixed_turn(rotation.pitch, desired.pitch, turn, Toward::Desired, fixed);
            }
            if self.store.flag_of(controller, Prop::bRotateToDesired) {
                let mut own = self.store.rotator_of(controller, Prop::Rotation);
                own.yaw = rotation.yaw;
                let own_desired = self.store.rotator_of(controller, Prop::DesiredRotation);
                own.pitch = fixed_turn(own.pitch, own_desired.pitch, turn, Toward::Desired, self.store.flag_of(controller, Prop::bFixedRotationDir));
                self.store.set(controller, Prop::Rotation, Value::Rotator(own));
            }
        }
        if rate.roll < 1 {
            rotation.roll = 0;
        } else {
            let ease = (delta * 8.0).min(1.0);
            let roll = rotation.roll & 0xffff;
            rotation.roll = if roll < 0x8000 { ((1.0 - ease) * roll as f32) as i32 } else { (roll as f32 + (0x10000 - roll) as f32 * ease) as i32 };
        }
        if rotation != self.store.rotator_of(pawn, Prop::Rotation) {
            self.store.set(pawn, Prop::Rotation, Value::Rotator(rotation));
        }
    }

    pub(crate) fn actor_rotation(&mut self, actor: ObjectId, delta: f32) -> Result<(), Error> {
        let rate = self.store.rotator_of(actor, Prop::RotationRate);
        if rate == [0; 3].into() || self.store.flag_of(actor, Prop::bInterpolating) {
            return Ok(());
        }
        let to_desired = self.store.flag_of(actor, Prop::bRotateToDesired);
        let fixed = self.store.flag_of(actor, Prop::bFixedRotationDir);
        let desired = self.store.rotator_of(actor, Prop::DesiredRotation);
        let current = self.store.rotator_of(actor, Prop::Rotation);
        if (!to_desired && !fixed) || (to_desired && current == desired) {
            return Ok(());
        }
        let turn = rate.map(|r| (r as f32 * delta) as i32);
        let mut rotation = current;
        for axis in [1, 0, 2] {
            if turn[axis] != 0 && (!to_desired || desired[axis] != rotation[axis]) {
                rotation[axis] = fixed_turn(rotation[axis], desired[axis], turn[axis], if to_desired { Toward::Desired } else { Toward::Spin }, fixed);
            }
        }
        if rotation != current {
            self.store.set(actor, Prop::Rotation, Value::Rotator(rotation));
            self.volume_moved(actor);
        }
        if to_desired && rotation == desired {
            self.event(actor, "EndedRotation", Vec::new())?;
        }
        Ok(())
    }
}
