//! A mover with a static mesh pushes through obstacles; `EncroachingOn` decides what stops it.

use p2_collision::mesh_box_overlaps;

use p2_math::{Rotator, Vector};
use crate::enums::EPhysics;
use crate::volumes::{OldZone, ZoneEvents};
use crate::interp::{float, int};
use crate::physics::Bases;
use crate::{Encroachment, Error, Kind, LoaderRef, ObjectId, Value, Vm, Prop};

const MV_GLIDE_BY_TIME: u8 = 1;
const KEY_COUNT: usize = 8;
const ROTATION_PUSH: f32 = 1.5;

impl<L: LoaderRef> Vm<L> {
    /// `KeyNum` is read once per frame. A refused move ([`Vm::move_encroacher`]) leaves `PhysAlpha` alone.
    pub(crate) fn mover_physics(&mut self, mover: ObjectId, delta: f32) -> Result<(), Error> {
        if self.store.physics_of(mover) != Some(EPhysics::MovingBrush) {
            return Ok(());
        }
        let start = self.store.vec3_of(mover, Prop::Location);
        let key = self.store.get(mover, Prop::KeyNum).map_or(0, |k| int(&k).clamp(0, KEY_COUNT as i32) as usize);
        let mut left = delta;
        while left > 0.0 && self.store.flag_of(mover, Prop::bInterpolating) {
            let alpha = self.store.float_of(mover, Prop::PhysAlpha);
            let mut new_alpha = self.store.float_of(mover, Prop::PhysRate) * left + alpha;
            if new_alpha > 1.0 {
                left *= (new_alpha - 1.0) / (new_alpha - alpha);
                new_alpha = 1.0;
            } else {
                left = 0.0;
            }
            let eased = match self.store.get(mover, Prop::MoverGlideType) {
                Some(Value::Byte(MV_GLIDE_BY_TIME)) => 3.0 * new_alpha * new_alpha - 2.0 * new_alpha * new_alpha * new_alpha,
                _ => new_alpha,
            };
            let target: Vector = self.key_members(mover, Prop::KeyPos, key).map_or(Vector::ZERO, |members| members.map(|m| float(&m)).into());
            let (base, old) = (self.store.vec3_of(mover, Prop::BasePos), self.store.vec3_of(mover, Prop::OldPos));
            let location = [0, 1, 2].map(|i| old[i] + (base[i] + target[i] - old[i]) * eased);
            // Rotators: integer components, wrapping; the scaled difference truncates.
            let target: Rotator = self.key_members(mover, Prop::KeyRot, key).map_or(Rotator::new(0, 0, 0), |members| members.map(|m| int(&m)).into());
            let (base, old) = (self.store.rotator_of(mover, Prop::BaseRot), self.store.rotator_of(mover, Prop::OldRot));
            let rotation = [0, 1, 2].map(|i| {
                let difference = base[i].wrapping_add(target[i]).wrapping_sub(old[i]);
                old[i].wrapping_add((difference as f32 * eased) as i32)
            });
            let moved = if self.is_encroacher(mover) {
                self.move_encroacher(mover, location.into(), rotation.into())?
            } else {
                self.place_far_moved(mover, location.into(), Encroachment::Ignored)?;
                self.store.set(mover, Prop::Rotation, Value::Rotator(rotation.into()));
                // Retail's move touches; `place_far_moved` does not, so the touch update is here.
                self.update_touching(mover)?;
                true
            };
            if !moved {
                if new_alpha <= alpha {
                    break;
                }
                continue;
            }
            self.store.set(mover, Prop::PhysAlpha, Value::Float(new_alpha));
            if new_alpha == 1.0 {
                self.store.set(mover, Prop::bInterpolating, Value::Bool(false));
                self.event(mover, crate::states::Event::KeyFrameReached, Vec::new())?;
            }
        }
        if delta > 0.0 {
            let end = self.store.vec3_of(mover, Prop::Location);
            self.store.set(mover, Prop::Velocity, Value::Vector([0, 1, 2].map(|i| (end[i] - start[i]) / delta).into()));
        }
        Ok(())
    }

    fn key_members(&self, mover: ObjectId, property: Prop, key: usize) -> Option<[Value; 3]> {
        let field = self.store.field(mover, property)?;
        let crate::memory::Shape::Struct(id) = field.shape else { return None };
        if key >= field.dim as usize {
            return None;
        }
        let base = field.word + key as u32 * field.words;
        let words = &self.store.object(mover).mem().words;
        Some(self.store.memory.get(id).head.map(|(word, shape)| crate::memory::scalar(words.get((base + word) as usize).copied().unwrap_or(0), shape)))
    }

    fn move_encroacher(&mut self, mover: ObjectId, location: Vector, rotation: Rotator) -> Result<bool, Error> {
        if self.check_encroachment(mover, location, rotation)? {
            return Ok(false);
        }
        let (old_location, old_rotation) = (self.store.vector(mover, Prop::Location), self.store.rotator_of(mover, Prop::Rotation));
        self.place_mover(mover, location, rotation);
        if !self.carry_attached(mover, old_location, location - old_location, old_rotation, rotation)? {
            return Ok(false);
        }
        self.update_relative_location(mover);
        self.set_zone(mover, ZoneEvents::Sent, OldZone::Region)?;
        self.update_touching(mover)?;
        Ok(true)
    }

    /// Brushes and volumes are not tested. Retail touches the overlapped actors that do not block here;
    /// [`Vm::move_encroacher`] updates touching after the move instead.
    fn check_encroachment(&mut self, mover: ObjectId, location: Vector, rotation: Rotator) -> Result<bool, Error> {
        let (old_location, old_rotation) = (self.store.vector(mover, Prop::Location), self.store.rotator_of(mover, Prop::Rotation));
        let overlapped = self.posed(mover, location, rotation, |vm| vm.encroached_blockers(mover));
        for &other in &overlapped {
            if self.store.is_deleted(other) {
                continue;
            }
            if !self.is_encroacher(other) {
                let at = self.store.vector(other, Prop::Location);
                let mut push = location - old_location;
                if rotation != old_rotation {
                    let swung = location + turned(old_rotation, rotation, at - old_location);
                    push += (swung - at) * ROTATION_PUSH;
                }
                self.move_smooth(other, push)?;
                if !self.posed(mover, location, rotation, |vm| vm.primitive_overlaps(mover, other)) {
                    self.posed(mover, location, rotation, |vm| vm.move_actor(other, push * (-1.0), Bases::Block))?;
                    continue;
                }
            }
            if self.event_bool(mover, crate::states::Event::EncroachingOn, [Value::Object(Some(other))])? {
                return Ok(true);
            }
        }
        for other in overlapped {
            if !self.store.is_deleted(other) {
                self.event(other, crate::states::Event::EncroachedBy, [Value::Object(Some(mover))])?;
            }
        }
        Ok(false)
    }

    fn encroached_blockers(&mut self, mover: ObjectId) -> Vec<ObjectId> {
        let Some(shape) = self.fill_shape(mover).and_then(|placement| self.placed_shape(mover, &placement)) else { return Vec::new() };
        let (min, max) = shape.boxes.bounds();
        let mut found = Vec::new();
        let nearby = self.colliders_near([min[0], min[1]], [max[0], max[1]], 0.0);
        let blocking = self.blocking(mover);
        for other in nearby.iter().map(|e| e.actor) {
            if other != mover
                && Some(other) != self.level
                && !self.store.is_deleted(other)
                && self.store.is_a(other, Kind::Actor)
                && !self.store.is_a(other, Kind::Brush)
                && !self.is_based_on(other, mover)
                && blocking.blocked_by(self.blocking(other))
                && self.primitive_overlaps(mover, other)
            {
                found.push(other);
            }
        }
        self.give_entries(nearby);
        found
    }

    pub(crate) fn primitive_overlaps(&mut self, mover: ObjectId, other: ObjectId) -> bool {
        let Some(shape) = self.fill_shape(mover).and_then(|placement| self.placed_shape(mover, &placement)) else { return false };
        let (radius, height) = self.store.collision_size(other);
        mesh_box_overlaps([&*shape.boxes], self.store.vector(other, Prop::Location), [radius, radius, height].into())
    }

    fn posed<T>(&mut self, mover: ObjectId, location: Vector, rotation: Rotator, f: impl FnOnce(&mut Self) -> T) -> T {
        let (old_location, old_rotation) = (self.store.vector(mover, Prop::Location), self.store.rotator_of(mover, Prop::Rotation));
        self.set_vector(mover, Prop::Location, location);
        self.store.set(mover, Prop::Rotation, Value::Rotator(rotation));
        let result = f(self);
        self.set_vector(mover, Prop::Location, old_location);
        self.store.set(mover, Prop::Rotation, Value::Rotator(old_rotation));
        result
    }

    fn place_mover(&mut self, mover: ObjectId, location: Vector, rotation: Rotator) {
        self.set_vector(mover, Prop::Location, location);
        self.store.set(mover, Prop::Rotation, Value::Rotator(rotation));
        self.relocate(mover);
    }

    pub(crate) fn finish_interpolation(&mut self, actor: ObjectId) {
        self.script.latent.sleep = Some(0.0);
        self.script.latent.interpolation = Some(actor);
    }

    pub(crate) fn interpolation_finished(&self, actor: ObjectId) -> bool {
        !self.store.flag_of(actor, Prop::bInterpolating)
    }
}

pub(crate) fn turned(from: Rotator, to: Rotator, offset: Vector) -> Vector {
    to.rotate(from.unrotate(offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turned_offsets_follow_the_rotation() {
        let swung = turned([0, 0, 0].into(), [0, 16384, 0].into(), [100.0, 0.0, 0.0].into());
        assert!(swung[0].abs() < 1e-3 && (swung[1] - 100.0).abs() < 1e-3, "{swung:?}");
        let kept = turned([0, 8000, 0].into(), [0, 8000, 0].into(), [30.0, -40.0, 5.0].into());
        assert!((0..3).all(|i| (kept[i] - [30.0, -40.0, 5.0][i]).abs() < 1e-3), "{kept:?}");
    }
}
