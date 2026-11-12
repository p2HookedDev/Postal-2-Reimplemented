//! Not modelled (unverified): `find_water_line` beyond water volumes and level geometry,
//! and buoyancy depth: a pawn in water counts as fully in it.

use super::{Bases, MoveHit, gravity_direction, horizontal_size, turned_velocity, two_wall_adjust};
use p2_math::Vector;
use crate::enums::EPhysics;
use crate::{Error, LoaderRef, ObjectId, Prop, Value, Vm};

/// `FluidFriction` of a pawn without a volume (unverified).
const DEFAULT_FLUID_FRICTION: f32 = 0.3;
const AI_ZONE_SPEED_SQUARED: f32 = 90_000.0;
/// A hit normal with |Z| below this may be stepped up.
const STEP_WALL_Z: f32 = 0.2;
const BUOYANT_RISE: f32 = 100.0;
const BOB_SPEED: f32 = 160.0;
const WATER_LINE_NUDGE: f32 = 0.1;
const SINK_BOB_SPEED: f32 = -160.0;
const MIN_SWIM_TIME: f32 = 0.01;

fn entry_velocity(average: Vector, old_velocity: Vector, terminal: f32) -> Vector {
    let velocity = (average * 2.0) - old_velocity;
    if velocity.dot(velocity) > terminal * terminal { velocity.safe_normal() * terminal } else { velocity }
}

fn sink_bob(mut velocity: Vector) -> Vector {
    if velocity[2] > SINK_BOB_SPEED && velocity[2] < 0.0 {
        velocity[2] = -80.0 - 0.7 * horizontal_size(velocity);
    }
    velocity
}

fn water_line_point(in_water: Vector, out_of_water: Vector, time: f32, own_volume: bool) -> Vector {
    let along = in_water - out_of_water;
    let hit = out_of_water + (along * time);
    let nudge = along.safe_normal() * WATER_LINE_NUDGE;
    if own_volume { hit + nudge } else { hit - nudge }
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn phys_flying(&mut self, pawn: ObjectId, delta: f32) -> Result<(), Error> {
        let accel_dir = self.store.vector(pawn, Prop::Acceleration).safe_normal();
        let friction = 0.5 * self.water(pawn).1;
        self.calc_fluid_velocity(pawn, accel_dir, delta, self.store.float_of(pawn, Prop::AirSpeed), friction, false);
        let mut old_location = self.store.vector(pawn, Prop::Location);
        self.store.set(pawn, Prop::bJustTeleported, Value::Bool(false));
        let zone = self.zone_push(pawn, 1.0);
        let adjusted = (self.store.vector(pawn, Prop::Velocity) + zone) * delta;
        let hit = self.move_actor(pawn, adjusted, Bases::Block)?;
        if self.store.is_deleted(pawn) {
            return Ok(());
        }
        if hit.time < 1.0 {
            self.blocked_move(pawn, adjusted, hit, &mut old_location, |vm, step| Ok((1.0, vm.move_actor(pawn, step, Bases::Block)?)))?;
        } else {
            self.set_vector(pawn, Prop::Floor, [0.0, 0.0, 1.0].into());
        }
        if !self.store.flag_of(pawn, Prop::bJustTeleported) && !self.store.flag_of(pawn, Prop::bNoVelocityUpdate) {
            let moved = self.store.vector(pawn, Prop::Location) - old_location;
            self.set_vector(pawn, Prop::Velocity, moved * (1.0 / delta));
        }
        self.store.set(pawn, Prop::bNoVelocityUpdate, Value::Bool(false));
        Ok(())
    }

    pub(crate) fn phys_swimming(&mut self, pawn: ObjectId, delta: f32, iterations: u32) -> Result<(), Error> {
        let (_, fluid_friction) = self.water(pawn);
        let buoyancy = self.store.float_of(pawn, Prop::Buoyancy);
        let mut velocity = self.store.vector(pawn, Prop::Velocity);
        if velocity[2] > BUOYANT_RISE && buoyancy != 0.0 {
            velocity[2] = velocity[2] * self.volume_buoyancy(pawn).0 / buoyancy;
            self.set_vector(pawn, Prop::Velocity, velocity);
        }
        let mut old_location = self.store.vector(pawn, Prop::Location);
        self.store.set(pawn, Prop::bJustTeleported, Value::Bool(false));
        let accel_dir = self.store.vector(pawn, Prop::Acceleration).safe_normal();
        self.calc_fluid_velocity(pawn, accel_dir, delta, self.store.float_of(pawn, Prop::WaterSpeed), 0.5 * fluid_friction, true);
        let mut vel_z = self.store.vector(pawn, Prop::Velocity)[2];
        let zone = self.zone_push(pawn, 25.0 * delta);
        let adjusted = (self.store.vector(pawn, Prop::Velocity) + zone) * delta;
        let (fraction, hit) = self.swim(pawn, adjusted)?;
        if self.store.is_deleted(pawn) {
            return Ok(());
        }
        let mut remaining = delta * fraction;
        if hit.time < 1.0 {
            remaining *= self.blocked_move(pawn, adjusted, hit, &mut old_location, |vm, step| vm.swim(pawn, step))?;
        } else {
            self.set_vector(pawn, Prop::Floor, [0.0, 0.0, 1.0].into());
        }
        if !self.store.flag_of(pawn, Prop::bJustTeleported) && remaining < delta {
            let water_jump = !self.water(pawn).0;
            if water_jump {
                vel_z = self.store.vector(pawn, Prop::Velocity)[2];
            }
            if !self.store.flag_of(pawn, Prop::bNoVelocityUpdate) {
                let moved = self.store.vector(pawn, Prop::Location) - old_location;
                self.set_vector(pawn, Prop::Velocity, moved * (1.0 / (delta - remaining)));
            }
            self.store.set(pawn, Prop::bNoVelocityUpdate, Value::Bool(false));
            if water_jump {
                let mut velocity = self.store.vector(pawn, Prop::Velocity);
                velocity[2] = vel_z;
                self.set_vector(pawn, Prop::Velocity, velocity);
            }
        }
        if !self.water(pawn).0 {
            if self.store.physics_of(pawn) == Some(EPhysics::Swimming) {
                self.set_physics(pawn, EPhysics::Falling, None)?;
            }
            let mut velocity = self.store.vector(pawn, Prop::Velocity);
            if velocity[2] > 0.0 && velocity[2] < BOB_SPEED {
                velocity[2] = 40.0 + 0.4 * horizontal_size(velocity);
                self.set_vector(pawn, Prop::Velocity, velocity);
            }
        }
        if self.store.physics_of(pawn) != Some(EPhysics::Swimming) {
            return self.start_new_physics(pawn, remaining, iterations + 1);
        }
        Ok(())
    }

    pub(crate) fn start_swimming(&mut self, pawn: ObjectId, old_location: Vector, old_velocity: Vector, tick: f32, remaining: f32, iterations: u32) -> Result<(), Error> {
        if !self.store.flag_of(pawn, Prop::bJustTeleported) {
            let average = if tick > 0.0 { (self.store.vector(pawn, Prop::Location) - old_location) * (1.0 / tick) } else { self.store.vector(pawn, Prop::Velocity) };
            let terminal = self.volume(pawn).terminal_velocity;
            self.set_vector(pawn, Prop::Velocity, entry_velocity(average, old_velocity, terminal));
        }
        let location = self.store.vector(pawn, Prop::Location);
        let own = self.store.object_of(pawn, Prop::PhysicsVolume);
        let line = self.find_water_line(own, location, old_location);
        let mut time_left = remaining;
        if line != location {
            time_left += (line - location).size() * tick / (location - old_location).size();
            self.move_actor(pawn, line - location, Bases::Block)?;
            if self.store.is_deleted(pawn) {
                return Ok(());
            }
        }
        let velocity = sink_bob(self.store.vector(pawn, Prop::Velocity));
        self.set_vector(pawn, Prop::Velocity, velocity);
        if time_left > MIN_SWIM_TIME {
            self.phys_swimming(pawn, time_left, iterations)?;
        }
        Ok(())
    }

    /// `own` is the volume the mover counts as its own `PhysicsVolume` at `out_of_water`.
    pub(crate) fn find_water_line(&mut self, own: Option<ObjectId>, in_water: Vector, out_of_water: Vector) -> Vector {
        let Some((volume, time)) = self.first_water_volume(out_of_water, in_water) else { return out_of_water };
        if self.trace_world(out_of_water, in_water, Vector::ZERO).is_some_and(|hit| hit.time < time) {
            return out_of_water;
        }
        water_line_point(in_water, out_of_water, time, own == Some(volume))
    }

    /// `advance` makes each further move (`MoveActor` flying, `Swim` swimming) and returns its time fraction; the result is
    /// the product of those fractions × (1 − their hit times) (swimming's time left, the retail order of evaluation).
    fn blocked_move(
        &mut self,
        pawn: ObjectId,
        adjusted: Vector,
        hit: MoveHit,
        old_location: &mut Vector,
        mut advance: impl FnMut(&mut Self, Vector) -> Result<(f32, MoveHit), Error>,
    ) -> Result<f32, Error> {
        self.set_vector(pawn, Prop::Floor, hit.normal);
        let grav_dir = gravity_direction(self.gravity_of(pawn));
        let desired_dir = adjusted.safe_normal();
        let up_down = grav_dir.dot(self.store.vector(pawn, Prop::Velocity).safe_normal());
        if hit.normal[2].abs() < STEP_WALL_Z && up_down < 0.5 && up_down > -0.2 {
            let step_z = self.store.vector(pawn, Prop::Location)[2];
            self.step_up(pawn, grav_dir, desired_dir, adjusted * (1.0 - hit.time), hit, 0)?;
            old_location[2] = self.store.vector(pawn, Prop::Location)[2] + (old_location[2] - step_z);
            return Ok(1.0);
        }
        self.process_hit_wall(pawn, hit.normal, hit.actor)?;
        let old_normal = hit.normal;
        let mut slide = (adjusted - (hit.normal * adjusted.dot(hit.normal))) * (1.0 - hit.time);
        let mut left = 1.0;
        if slide.dot(adjusted) >= 0.0 {
            let (fraction, again) = advance(self, slide)?;
            left *= fraction * (1.0 - again.time);
            if again.time < 1.0 {
                self.process_hit_wall(pawn, again.normal, again.actor)?;
                slide = two_wall_adjust(desired_dir, slide, again.normal, old_normal, again.time);
                let (fraction, last) = advance(self, slide)?;
                left *= fraction * (1.0 - last.time);
            }
        }
        Ok(left)
    }

    fn swim(&mut self, pawn: ObjectId, step: Vector) -> Result<(f32, MoveHit), Error> {
        let start = self.store.vector(pawn, Prop::Location);
        let hit = self.move_actor(pawn, step, Bases::Block)?;
        if self.store.is_deleted(pawn) || self.water(pawn).0 {
            return Ok((0.0, hit));
        }
        let location = self.store.vector(pawn, Prop::Location);
        let own = self.store.object_of(pawn, Prop::PhysicsVolume);
        let line = self.find_water_line(own, start, location);
        if line == location {
            return Ok((0.0, hit));
        }
        let back = line - location;
        let fraction = if (location - start).dot(back) > 0.0 { 0.0 } else { back.size() / step.size() };
        Ok((fraction, self.move_actor(pawn, back, Bases::Block)?))
    }

    fn water(&self, pawn: ObjectId) -> (bool, f32) {
        match self.store.object_of(pawn, Prop::PhysicsVolume).filter(|&v| self.store.object(v).class.is_some()) {
            Some(v) => (self.store.flag_of(v, Prop::bWaterVolume), self.store.float_of(v, Prop::FluidFriction)),
            None => (false, DEFAULT_FLUID_FRICTION),
        }
    }

    pub(super) fn zone_push(&self, pawn: ObjectId, factor: f32) -> Vector {
        let zone = self.volume(pawn).zone_velocity;
        if self.is_human_controlled(pawn) || zone.dot(zone) > AI_ZONE_SPEED_SQUARED { zone * factor } else { Vector::ZERO }
    }

    fn calc_fluid_velocity(&mut self, pawn: ObjectId, accel_dir: Vector, delta: f32, max_speed: f32, friction: f32, buoyant: bool) {
        let (limit, cap) = self.velocity_caps(pawn, max_speed);
        let mut velocity = self.store.vector(pawn, Prop::Velocity);
        let mut acceleration = self.store.vector(pawn, Prop::Acceleration);
        if acceleration.dot(acceleration) > limit * limit {
            acceleration = accel_dir * limit;
        }
        velocity = turned_velocity(velocity, accel_dir, delta, friction);
        velocity = (velocity * (1.0 - friction * delta)) + (acceleration * delta);
        if buoyant {
            let lift = 1.0 - self.store.float_of(pawn, Prop::Buoyancy) / self.store.float_of(pawn, Prop::Mass);
            if lift.is_finite() {
                velocity += self.gravity_of(pawn) * (delta * lift);
            }
        }
        if velocity.dot(velocity) > cap * cap {
            velocity = velocity.safe_normal() * cap;
        }
        self.set_vector(pawn, Prop::Velocity, velocity);
        self.set_vector(pawn, Prop::Acceleration, acceleration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vector, b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-3)
    }

    #[test]
    fn a_pawn_enters_water_with_twice_its_average_velocity_less_the_old_one() {
        assert!(close(entry_velocity([0.0, 0.0, -100.0].into(), [0.0, 0.0, -50.0].into(), 1000.0), [0.0, 0.0, -150.0]));
    }

    #[test]
    fn the_entry_velocity_is_capped_at_terminal_velocity() {
        assert!(close(entry_velocity([0.0, 0.0, -1000.0].into(), Vector::ZERO, 500.0), [0.0, 0.0, -500.0]));
    }

    #[test]
    fn only_a_slow_sink_bobs() {
        assert!(close(sink_bob([30.0, 40.0, -100.0].into()), [30.0, 40.0, -115.0]));
        assert!(close(sink_bob([30.0, 40.0, -200.0].into()), [30.0, 40.0, -200.0]));
        assert!(close(sink_bob([30.0, 40.0, 10.0].into()), [30.0, 40.0, 10.0]));
    }

    #[test]
    fn the_water_line_lies_inside_the_pawns_own_volume_and_outside_any_other() {
        let (in_water, out_of_water) = (Vector::ZERO, Vector::new(0.0, 0.0, 100.0));
        assert!(close(water_line_point(in_water, out_of_water, 0.5, true), [0.0, 0.0, 49.9]));
        assert!(close(water_line_point(in_water, out_of_water, 0.5, false), [0.0, 0.0, 50.1]));
    }
}
