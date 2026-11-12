use p2_math::Vector;
use crate::enums::EPhysics;
use crate::interp::int;
use crate::{Error, LoaderRef, ObjectId, Prop, Value, Vm, Kind};
use super::{
    AIR_BURST_SPEED, Bases, MAX_ITERATIONS, MIN_AIR_CONTROL, MIN_FLOOR_Z, MIN_LANDING_SECONDS, MIN_LANDING_TIME,
    MoveHit, VolumeProps, horizontal_size, substep, two_wall_adjust,
};

/// The velocity at the middle of a fall step: `lift` (buoyancy over mass) takes its share off the acceleration, gravity
/// included, and `fluid_friction` slows what the actor already had.
pub(crate) fn fall_velocity(old: Vector, acceleration: Vector, tick: f32, lift: f32, fluid_friction: f32) -> Vector {
    (acceleration * ((1.0 - lift) * 0.5 * tick)) + (old * (1.0 - fluid_friction * tick))
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn begin_fall_step(&mut self, actor: ObjectId) -> (Vector, Vector) {
        let old_location = self.store.vector(actor, Prop::Location);
        self.store.set(actor, Prop::bJustTeleported, Value::Bool(false));
        (old_location, self.store.vector(actor, Prop::Velocity))
    }

    pub(crate) fn fall_step_move(&mut self, actor: ObjectId, velocity: Vector, adjusted: Vector) -> Result<Option<MoveHit>, Error> {
        self.set_vector(actor, Prop::Velocity, velocity);
        let hit = self.move_actor(actor, adjusted, Bases::Block)?;
        Ok((!self.store.is_deleted(actor)).then_some(hit))
    }

    pub(super) fn phys_falling(&mut self, pawn: ObjectId, delta: f32, mut iterations: u32) -> Result<(), Error> {
        let mut real_acceleration = self.store.vector(pawn, Prop::Acceleration);
        let accel_rate = self.store.float_of(pawn, Prop::AccelRate);
        let (mut acceleration, bound_speed) = self.air_acceleration(pawn, real_acceleration, accel_rate, delta);
        let constant = self.store.vector(pawn, Prop::ConstantAcceleration);
        let ai = self.store.object_of(pawn, Prop::Controller).is_some_and(|c| self.store.is_a(c, Kind::AIController));
        let mass = self.store.float_of(pawn, Prop::Mass);
        let mut remaining = delta;
        while remaining > 0.0 && iterations < MAX_ITERATIONS {
            iterations += 1;
            let tick = substep(remaining);
            remaining -= tick;
            let (old_location, mut old_velocity) = self.begin_fall_step(pawn);
            let VolumeProps { gravity, zone_velocity, .. } = self.volume(pawn);
            let total = (acceleration + gravity) + constant;
            let (buoyancy, fluid_friction) = self.volume_buoyancy(pawn);
            let mut velocity = fall_velocity(old_velocity, total, tick, if buoyancy == 0.0 { 0.0 } else { buoyancy / mass }, fluid_friction);
            if bound_speed != 0.0 && horizontal_size(velocity) > bound_speed {
                let flat = Vector::new(velocity[0], velocity[1], 0.0).safe_normal() * bound_speed;
                velocity = [flat[0], flat[1], velocity[2]].into();
            }
            let adjusted = (velocity + zone_velocity) * tick;
            let Some(hit) = self.fall_step_move(pawn, velocity, adjusted)? else { return Ok(()) };
            if self.store.physics_of(pawn) == Some(EPhysics::Swimming) {
                return self.start_swimming(pawn, old_location, old_velocity, tick, (1.0 - hit.time) * tick + remaining, iterations);
            }
            if hit.time < 1.0 {
                if hit.normal[2] >= MIN_FLOOR_Z {
                    let left = (1.0 - hit.time) * tick + remaining;
                    if !self.store.flag_of(pawn, Prop::bJustTeleported) && hit.time > MIN_LANDING_TIME && hit.time * tick > MIN_LANDING_SECONDS {
                        let moved = self.store.vector(pawn, Prop::Location) - old_location;
                        self.set_vector(pawn, Prop::Velocity, moved * (1.0 / (hit.time * tick)));
                    }
                    return self.process_landed(pawn, hit.normal, hit.actor, left, iterations);
                }
                if real_acceleration == Vector::ZERO && ai {
                    acceleration = Vector::new(velocity[0], velocity[1], 0.0).safe_normal() * accel_rate;
                    real_acceleration = acceleration;
                    self.set_vector(pawn, Prop::Acceleration, acceleration);
                } else {
                    self.process_hit_wall(pawn, hit.normal, hit.actor)?;
                }
                let mut slide = (adjusted - (hit.normal * adjusted.dot(hit.normal))) * (1.0 - hit.time);
                if slide.dot(adjusted) >= 0.0 {
                    let again = self.move_actor(pawn, slide, Bases::Block)?;
                    if again.time < 1.0 {
                        if again.normal[2] >= MIN_FLOOR_Z {
                            return self.process_landed(pawn, again.normal, again.actor, 0.0, iterations);
                        }
                        self.process_hit_wall(pawn, again.normal, again.actor)?;
                        slide = two_wall_adjust(adjusted.safe_normal(), slide, again.normal, hit.normal, again.time);
                        let ditch = hit.normal[2] > 0.0 && again.normal[2] > 0.0 && slide[2] == 0.0 && again.normal.dot(hit.normal) < 0.0;
                        let last = self.move_actor(pawn, slide, Bases::Block)?;
                        if ditch || last.normal[2] >= MIN_FLOOR_Z {
                            return self.process_landed(pawn, last.normal, last.actor, 0.0, iterations);
                        }
                    }
                }
                let moved = (self.store.vector(pawn, Prop::Location) - old_location) * (1.0 / tick);
                old_velocity = [moved[0], moved[1], old_velocity[2]].into();
            }
            if !self.store.flag_of(pawn, Prop::bJustTeleported) {
                let VolumeProps { terminal_velocity: terminal, zone_velocity, .. } = self.volume(pawn);
                let moved = ((self.store.vector(pawn, Prop::Location) - old_location) * (1.0 / tick)) - zone_velocity;
                let mut velocity = moved;
                if moved[2] < old_velocity[2] || old_velocity[2] >= 0.0 {
                    velocity = (moved * 2.0) - old_velocity;
                }
                if velocity.dot(velocity) > terminal * terminal {
                    velocity = velocity.safe_normal() * terminal;
                }
                self.set_vector(pawn, Prop::Velocity, velocity);
            }
        }
        self.set_vector(pawn, Prop::Acceleration, real_acceleration);
        Ok(())
    }

    fn air_acceleration(&mut self, pawn: ObjectId, mut acceleration: Vector, accel_rate: f32, delta: f32) -> (Vector, f32) {
        acceleration[2] = 0.0;
        let mut air_control = self.store.float_of(pawn, Prop::AirControl);
        if air_control > MIN_AIR_CONTROL {
            let (radius, height) = self.store.collision_size(pawn);
            let mut test = ((acceleration.safe_normal() * (air_control * accel_rate)) + self.store.vector(pawn, Prop::Velocity)) * delta;
            test[2] = 0.0;
            let at = self.store.vector(pawn, Prop::Location);
            if self.trace_world(at, at + test, [radius, radius, height].into()).is_some() {
                air_control = 0.0;
            }
        }
        let speed = horizontal_size(self.store.vector(pawn, Prop::Velocity));
        let mut max_accel = accel_rate * air_control;
        let mut bound_speed = 0.0;
        if speed < AIR_BURST_SPEED && air_control > 0.0 {
            max_accel += (AIR_BURST_SPEED - speed) / delta;
        } else if speed >= self.store.float_of(pawn, Prop::GroundSpeed) {
            if air_control <= MIN_AIR_CONTROL {
                max_accel = 1.0;
            } else {
                bound_speed = speed;
            }
        }
        if acceleration.dot(acceleration) > max_accel * max_accel {
            acceleration = acceleration.safe_normal() * max_accel;
        }
        self.set_vector(pawn, Prop::Acceleration, acceleration);
        (acceleration, bound_speed)
    }

    fn process_landed(&mut self, pawn: ObjectId, hit_normal: Vector, floor: Option<ObjectId>, left: f32, iterations: u32) -> Result<(), Error> {
        self.set_vector(pawn, Prop::Floor, hit_normal);
        if self.store.physics_of(pawn) == Some(EPhysics::Falling) {
            let alive = self.store.get(pawn, Prop::Health).as_ref().map_or(1, int) > 0;
            self.set_physics(pawn, if alive { EPhysics::Walking } else { EPhysics::None }, Some((floor, hit_normal)))?;
        }
        if self.store.physics_of(pawn) == Some(EPhysics::Walking) {
            let direction = self.store.vector(pawn, Prop::Acceleration).safe_normal();
            self.set_vector(pawn, Prop::Acceleration, direction);
        }
        let args = [Value::Vector(hit_normal)];
        let handled = match self.store.object_of(pawn, Prop::Controller) {
            Some(controller) => self.event_bool(controller, crate::states::Event::NotifyLanded, args.clone())?,
            None => false,
        };
        if !handled {
            self.event(pawn, crate::states::Event::Landed, args)?;
        }
        self.start_new_physics(pawn, left, iterations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pawn_inside_a_trigger_volume_falls_as_fast_as_outside() {
        let (old, tick) = (Vector::new(100.0, 0.0, 550.0), 0.05);
        let outside = fall_velocity(old, Vector::new(0.0, 0.0, -1500.0), tick, 0.0, 0.0);
        assert_eq!(outside, Vector::new(100.0, 0.0, 550.0 - 1500.0 * 0.025));
        let inside = fall_velocity(old, Vector::new(0.0, 0.0, -3000.0), tick, 99.0 * 0.5 / 100.0, 0.3 * 0.5);
        assert!((inside[2] - (550.0 * 0.9925 - 3000.0 * 0.505 * 0.025)).abs() < 1e-3 && (inside[0] - 99.25).abs() < 1e-3, "{inside:?}");
    }
}
