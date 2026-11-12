use p2_math::Vector;
use crate::movement_natives::suggest_jump_velocity;
use crate::nav::{WALK_MIN_MOVE, WalkMove};
use crate::physics::{DEFAULT_GRAVITY, MoveHit, two_wall_adjust};
use crate::{LoaderRef, ObjectId, Prop, Vm};

const JUMP_UP_HEIGHT: f32 = 61.0;
const JUMP_UP_DROP: f32 = 96.0;
const JUMP_UP_MIN_MOVE_SQUARED: f32 = 16.81;
const LANDING_TICK: f32 = 0.1;
/// Retail runs a 36th tick too, but always puts the pawn back after it.
const LANDING_TICKS: usize = 35;
/// Compared in double precision, so a normal of exactly 0.7 in single precision is not a floor.
const LANDING_FLOOR_Z: f64 = 0.7;

struct Air {
    gravity: Vector,
    push: Vector,
    friction: f32,
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn find_jump_up(&mut self, pawn: ObjectId, from: Vector, step: Vector) -> (Vector, bool) {
        let mut at = from;
        self.test_move(pawn, &mut at, Vector::new(0.0, 0.0, JUMP_UP_HEIGHT));
        let walked;
        (at, walked) = self.walk_move(pawn, at, step, WALK_MIN_MOVE);
        if matches!(walked, WalkMove::Stopped) {
            return (from, false);
        }
        self.test_move(pawn, &mut at, Vector::new(0.0, 0.0, -JUMP_UP_DROP));
        let (dx, dy) = (at.x - from.x, at.y - from.y);
        (at, matches!(walked, WalkMove::Moved) && dx * dx + dy * dy >= JUMP_UP_MIN_MOVE_SQUARED)
    }

    pub(crate) fn find_best_jump(&mut self, pawn: ObjectId, from: Vector, target: Vector) -> (Vector, bool) {
        let ground_speed = self.store.float_of(pawn, "GroundSpeed");
        let gravity_z = self.air_at(from).gravity.z;
        let velocity = suggest_jump_velocity(target - from, ground_speed, 0.0, self.store.float_of(pawn, "JumpZ"), ground_speed, gravity_z);
        let at = self.jump_landing(pawn, from, velocity);
        if at == from || (!self.store.flag_of(pawn, Prop::bCanSwim) && self.water_at(at)) {
            return (at, false);
        }
        let flat = |v: Vector| Vector::new(v.x, v.y, 0.0).size();
        (at, flat(target - from) - flat(target - at) > 0.0)
    }

    fn jump_landing(&mut self, pawn: ObjectId, from: Vector, mut velocity: Vector) -> Vector {
        let max_fall = self.store.float_of(pawn, Prop::MaxFallSpeed);
        let mut at = from;
        for _ in 0..LANDING_TICKS {
            let air = self.air_at(at);
            velocity = landing_velocity(velocity, air.gravity, air.friction);
            let delta = (air.push + velocity) * LANDING_TICK;
            let hit = self.test_move(pawn, &mut at, delta);
            if self.has_zones() && self.zone_number(at) == 0 {
                return from;
            }
            if self.water_at(at) {
                return at;
            }
            if velocity.z < -max_fall {
                return from;
            }
            if hit.time < 1.0 && self.lands_after_hit(pawn, &mut at, delta, hit) {
                return at;
            }
        }
        from
    }

    fn lands_after_hit(&mut self, pawn: ObjectId, at: &mut Vector, delta: Vector, hit: MoveHit) -> bool {
        if on_floor(hit.normal) {
            return true;
        }
        let slide = (delta - hit.normal * hit.normal.dot(delta)) * (1.0 - hit.time);
        if slide.dot(delta) < 0.0 {
            return false;
        }
        let again = self.test_move(pawn, at, slide);
        if again.time >= 1.0 {
            return false;
        }
        let slide = two_wall_adjust(delta.safe_normal(), slide, again.normal, hit.normal, again.time);
        let last = self.test_move(pawn, at, slide);
        on_floor(again.normal) || on_floor(last.normal)
    }

    fn air_at(&mut self, point: Vector) -> Air {
        match self.physics_volume_at(point, None).ok().flatten() {
            Some(volume) => Air {
                gravity: self.store.vector(volume, Prop::Gravity),
                push: self.store.vector(volume, Prop::ZoneVelocity),
                friction: if self.store.flag_of(volume, Prop::bWaterVolume) { self.store.float_of(volume, Prop::FluidFriction) } else { 0.0 },
            },
            None => Air { gravity: DEFAULT_GRAVITY, push: Vector::ZERO, friction: 0.0 },
        }
    }
}

fn landing_velocity(velocity: Vector, gravity: Vector, friction: f32) -> Vector {
    gravity * LANDING_TICK + velocity * (1.0 - friction * LANDING_TICK)
}

fn on_floor(normal: Vector) -> bool {
    f64::from(normal.z) >= LANDING_FLOOR_Z
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landing_velocity_adds_a_tick_of_gravity_and_drags_in_water() {
        let v = landing_velocity(Vector::new(100.0, 0.0, 0.0), Vector::new(0.0, 0.0, -1500.0), 0.0);
        assert_eq!((v.x, v.y, v.z), (100.0, 0.0, -150.0));
        let v = landing_velocity(Vector::new(100.0, 0.0, 0.0), Vector::ZERO, 2.0);
        assert!((v.x - 80.0).abs() < 1e-4);
    }

    #[test]
    fn single_precision_point_seven_is_not_a_floor() {
        assert!(!on_floor(Vector::new(0.0, 0.0, 0.7)));
        assert!(on_floor(Vector::new(0.0, 0.0, 0.71)));
        assert!(on_floor(Vector::new(0.0, 0.0, 1.0)));
    }
}
