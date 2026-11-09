//! The 35 / 37-unit lengths of the ladder step down, floor check and spider drop are (unverified).
//! `SpiderstepUp` recursion stops after 4 steps; a still pawn's foot line sees only level geometry.

use p2_math::Vector;
use crate::enums::EPhysics;
use crate::physics::{Bases, MAX_ITERATIONS, MAX_STEP_HEIGHT, MAX_SUBSTEP, MoveHit, STEP_AGAIN_SQUARED, Sweep, SweepAgainst, gravity_direction, nearly_zero, two_wall_adjust};
use crate::{Error, LoaderRef, ObjectId, Prop, Value, Vm};

const SPIDER_FLOOR_CHECK: f32 = MAX_STEP_HEIGHT + 2.0;
const SPIDER_FOOT_CHECK: f32 = 20.0;
const SPIDER_MIN_FLOOR_DIST: f32 = 4.1;
const SPIDER_MAX_FLOOR_DIST: f32 = 4.6;
const SPIDER_LOST_FLOOR: f32 = 8.0;
const SPIDER_WALL_DOT: f32 = 0.1;
const MIN_FALL_TIME: f32 = 0.005;
const FLOOR_DIRECTIONS: [Vector; 6] =
    [Vector::new(0.0, 0.0, 1.0), Vector::new(0.0, 1.0, 0.0), Vector::new(0.0, -1.0, 0.0), Vector::new(1.0, 0.0, 0.0), Vector::new(-1.0, 0.0, 0.0), Vector::new(0.0, 0.0, -1.0)];

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn phys_ladder(&mut self, pawn: ObjectId, delta: f32, iterations: u32) -> Result<(), Error> {
        let iterations = iterations + 1;
        self.set_vector(pawn, Prop::Velocity, Vector::ZERO);
        let acceleration = self.store.vector(pawn, Prop::Acceleration);
        if let Some(ladder) = self.store.object_of(pawn, Prop::OnLadder)
            && self.store.object_of(pawn, Prop::Controller).is_some()
            && acceleration != Vector::ZERO
            && self.climb_ladder(pawn, ladder, acceleration, delta, iterations)?
        {
            return Ok(());
        }
        if self.store.object_of(pawn, Prop::Controller).is_none() && !self.store.is_deleted(pawn) {
            self.set_physics(pawn, EPhysics::Falling, None)?;
        }
        Ok(())
    }

    fn climb_ladder(&mut self, pawn: ObjectId, ladder: ObjectId, acceleration: Vector, delta: f32, iterations: u32) -> Result<bool, Error> {
        let look = self.store.vector(ladder, Prop::LookDir);
        let climb = self.store.vector(ladder, Prop::ClimbDir);
        let physical = !self.store.flag_of(ladder, Prop::bNoPhysicalLadder);
        let climb_up = acceleration.dot(climb + look) > 0.0;
        let ground_speed = self.store.float_of(pawn, Prop::GroundSpeed);
        let mut remaining = delta;
        if physical && climb_up {
            self.set_vector(pawn, Prop::Velocity, look * ground_speed);
            let hit = self.move_actor(pawn, look * (remaining * ground_speed), Bases::Block)?;
            remaining *= 1.0 - hit.time;
            if self.store.is_deleted(pawn) {
                return Ok(true);
            }
            if self.store.object_of(pawn, Prop::OnLadder).is_none() {
                self.off_ladder(pawn)?;
                self.start_new_physics(pawn, remaining, iterations)?;
                return Ok(true);
            }
            if remaining == 0.0 {
                return Ok(true);
            }
        }
        let mut velocity = climb * self.store.float_of(pawn, Prop::LadderSpeed);
        if !climb_up {
            velocity *= -1.0;
        }
        self.set_vector(pawn, Prop::Velocity, velocity);
        let move_dir = velocity * remaining;
        let hit = self.move_actor(pawn, move_dir, Bases::Block)?;
        remaining *= 1.0 - hit.time;
        if self.store.is_deleted(pawn) {
            return Ok(true);
        }
        if self.store.object_of(pawn, Prop::OnLadder).is_none() {
            if move_dir.dot(self.gravity_of(pawn)) > 0.0 {
                self.set_physics(pawn, EPhysics::Falling, None)?;
                return Ok(true);
            }
            let (radius, height) = (self.store.float_of(pawn, Prop::CollisionRadius), self.store.float_of(pawn, Prop::CollisionHeight));
            let out = move_dir.safe_normal() * (1.1 * height);
            self.move_actor(pawn, out, Bases::Block)?;
            self.move_actor(pawn, look * (0.5 * radius), Bases::Block)?;
            self.move_actor(pawn, (out + move_dir) * (-1.0), Bases::Block)?;
            self.move_actor(pawn, look * (3.0 - 0.5 * radius), Bases::Block)?;
            if self.store.is_deleted(pawn) {
                return Ok(true);
            }
            self.set_vector(pawn, Prop::Velocity, Vector::ZERO);
            self.off_ladder(pawn)?;
            self.start_new_physics(pawn, remaining, iterations)?;
            return Ok(true);
        }
        let world = hit.actor.is_some_and(|a| Some(a) == self.level || self.store.flag_of(a, Prop::bWorldGeometry));
        if hit.time < 1.0 && world {
            let old_location = self.store.vector(pawn, Prop::Location);
            let mut step = look * (ground_speed * remaining);
            if !climb_up {
                step *= -1.0;
            }
            let ground = self.move_actor(pawn, step, Bases::Block)?;
            if ground.time < 1.0 {
                self.step_up(pawn, gravity_direction(self.gravity_of(pawn)), step.safe_normal(), step, ground, 0)?;
                if self.store.object_of(pawn, Prop::OnLadder).is_some() && self.store.physics_of(pawn) != Some(EPhysics::Ladder) {
                    self.set_physics(pawn, EPhysics::Ladder, None)?;
                }
            }
            let moved = self.store.vector(pawn, Prop::Location) - old_location;
            self.set_vector(pawn, Prop::Velocity, moved * (1.0 / remaining));
        } else if physical && !climb_up {
            let down = self.move_actor(pawn, climb * (-MAX_STEP_HEIGHT), Bases::Block)?;
            let distance = down.time * MAX_STEP_HEIGHT;
            if down.time == 1.0 {
                self.move_actor(pawn, look * (delta * ground_speed), Bases::Block)?;
            }
            self.move_actor(pawn, climb * distance, Bases::Block)?;
            if !self.store.is_deleted(pawn) && self.store.object_of(pawn, Prop::OnLadder).is_none() {
                self.off_ladder(pawn)?;
            }
        }
        Ok(false)
    }

    fn off_ladder(&mut self, pawn: ObjectId) -> Result<(), Error> {
        let water = self.store.object_of(pawn, Prop::PhysicsVolume).is_some_and(|v| self.store.flag_of(v, Prop::bWaterVolume));
        self.set_physics(pawn, if water { EPhysics::Swimming } else { EPhysics::Walking }, None)
    }

    pub(crate) fn phys_spider(&mut self, pawn: ObjectId, delta: f32, mut iterations: u32) -> Result<(), Error> {
        if self.store.object_of(pawn, Prop::Controller).is_none() {
            return Ok(());
        }
        if nearly_zero(self.store.vector(pawn, Prop::Floor)) && !self.find_new_floor(pawn, delta, iterations)? {
            return Ok(());
        }
        let friction = self.volume(pawn).ground_friction;
        let mut velocity = self.store.vector(pawn, Prop::Velocity);
        let mut acceleration = self.store.vector(pawn, Prop::Acceleration);
        if acceleration == Vector::ZERO {
            let old = velocity;
            velocity = velocity - (velocity * (2.0 * delta * friction));
            if old.dot(velocity) < 0.0 {
                velocity = acceleration;
            }
        } else {
            let accel_dir = acceleration.safe_normal();
            let speed = velocity.size();
            let accel_rate = self.store.float_of(pawn, Prop::AccelRate);
            if acceleration.dot(acceleration) > accel_rate * accel_rate {
                acceleration = accel_dir * accel_rate;
                self.set_vector(pawn, Prop::Acceleration, acceleration);
            }
            velocity = velocity - ((velocity - (accel_dir * speed)) * (delta * friction));
        }
        velocity += acceleration * delta;
        let floor = self.store.vector(pawn, Prop::Floor);
        velocity = velocity - (floor * floor.dot(velocity));
        let max_speed = self.store.float_of(pawn, Prop::GroundSpeed) * self.store.float_of(pawn, Prop::DesiredSpeed);
        if velocity.dot(velocity) > max_speed * max_speed {
            velocity = velocity.safe_normal() * max_speed;
        }
        self.set_vector(pawn, Prop::Velocity, velocity);
        iterations += 1;
        let desired_move = velocity;
        let old_location = self.store.vector(pawn, Prop::Location);
        self.store.set(pawn, Prop::bJustTeleported, Value::Bool(false));
        let (radius, height) = (self.store.float_of(pawn, Prop::CollisionRadius), self.store.float_of(pawn, Prop::CollisionHeight));
        let max_step_squared = (radius * radius).min(MAX_STEP_HEIGHT * MAX_STEP_HEIGHT);
        let mut remaining = delta;
        while remaining > 0.0 && iterations < MAX_ITERATIONS {
            iterations += 1;
            let whole = remaining <= MAX_SUBSTEP
                || (!self.is_human_controlled(pawn) && desired_move.dot(desired_move) * remaining * remaining <= max_step_squared);
            let tick = if whole { remaining } else { MAX_SUBSTEP.min(remaining * 0.5) };
            remaining -= tick;
            let step = desired_move * tick;
            let drop = if nearly_zero(step) {
                remaining = 0.0;
                let (floor, location) = (self.store.vector(pawn, Prop::Floor), self.store.vector(pawn, Prop::Location));
                let foot = location - (floor * height);
                let hit = self.trace_world(foot, foot - (floor * SPIDER_FOOT_CHECK), Vector::ZERO);
                let (distance, actor) = hit.map_or((SPIDER_FOOT_CHECK, None), |h| (h.time * SPIDER_FOOT_CHECK, self.level));
                self.store.object_of(pawn, Prop::Base) != actor || !(SPIDER_MIN_FLOOR_DIST..=SPIDER_MAX_FLOOR_DIST).contains(&distance)
            } else {
                let step_start = self.store.vector(pawn, Prop::Location);
                let hit = self.move_actor(pawn, step, Bases::Block)?;
                if hit.time < 1.0 {
                    self.spider_step_up(pawn, step.safe_normal(), step * (1.0 - hit.time), hit, 0)?;
                }
                if self.store.is_deleted(pawn) {
                    return Ok(());
                }
                if self.store.physics_of(pawn) == Some(EPhysics::Swimming) {
                    return self.start_swimming(pawn, step_start, self.store.vector(pawn, Prop::Velocity), tick, remaining, iterations);
                }
                true
            };
            if !drop {
                continue;
            }
            let (floor, location) = (self.store.vector(pawn, Prop::Floor), self.store.vector(pawn, Prop::Location));
            let below = self.sweep(pawn, Sweep { start: location, delta: floor * (-SPIDER_FLOOR_CHECK), radius, height }, SweepAgainst::WORLD_AND_ACTORS, None);
            if below.time < 1.0 {
                self.set_vector(pawn, Prop::Floor, below.normal);
                let landed = self.move_actor(pawn, below.normal * (-SPIDER_FLOOR_CHECK), Bases::Block)?;
                if landed.actor != self.store.object_of(pawn, Prop::Base) {
                    self.set_base(pawn, landed.actor, landed.normal)?;
                }
            } else {
                self.move_actor(pawn, floor * (-SPIDER_LOST_FLOOR), Bases::Block)?;
                if !self.find_new_floor(pawn, delta, iterations)? {
                    return Ok(());
                }
            }
            if self.store.is_deleted(pawn) {
                return Ok(());
            }
        }
        if !self.store.flag_of(pawn, Prop::bJustTeleported) {
            let moved = self.store.vector(pawn, Prop::Location) - old_location;
            self.set_vector(pawn, Prop::Velocity, moved * (1.0 / delta));
        }
        Ok(())
    }

    /// Both retail callers pass the frame's time as the time left, so the velocity rebuild is left out.
    fn find_new_floor(&mut self, pawn: ObjectId, delta: f32, iterations: u32) -> Result<bool, Error> {
        let location = self.store.vector(pawn, Prop::Location);
        let extent = [self.store.float_of(pawn, Prop::CollisionRadius), self.store.float_of(pawn, Prop::CollisionRadius), self.store.float_of(pawn, Prop::CollisionHeight)];
        for direction in FLOOR_DIRECTIONS {
            if let Some(hit) = self.trace_world(location, location - (direction * MAX_STEP_HEIGHT), extent.into()) {
                let level = self.level;
                self.set_base(pawn, level, hit.normal)?;
                return Ok(true);
            }
        }
        self.event(pawn, "Falling", Vec::new())?;
        if self.store.is_deleted(pawn) {
            return Ok(false);
        }
        if self.store.physics_of(pawn) == Some(EPhysics::Spider) {
            self.set_physics(pawn, EPhysics::Falling, None)?;
        }
        if self.store.physics_of(pawn) == Some(EPhysics::Falling) && delta > MIN_FALL_TIME {
            self.phys_falling(pawn, delta, iterations)?;
        }
        Ok(false)
    }

    fn spider_step_up(&mut self, pawn: ObjectId, desired_dir: Vector, delta: Vector, mut hit: MoveHit, depth: u32) -> Result<(), Error> {
        let mut floor = self.store.vector(pawn, Prop::Floor);
        if floor.dot(hit.normal) < SPIDER_WALL_DOT {
            self.move_actor(pawn, floor * MAX_STEP_HEIGHT, Bases::Block)?;
            hit = self.move_actor(pawn, delta, Bases::Block)?;
        } else {
            floor = hit.normal;
            self.set_vector(pawn, Prop::Floor, floor);
            hit = self.move_actor(pawn, delta + [0.0, 0.0, delta.size() * hit.normal[2]].into(), Bases::Block)?;
        }
        if hit.time < 1.0 {
            if floor.dot(hit.normal) < SPIDER_WALL_DOT && hit.time * delta.dot(delta) > STEP_AGAIN_SQUARED && depth < 4 {
                let landed = self.move_actor(pawn, floor * (-MAX_STEP_HEIGHT), Bases::Block)?;
                return self.spider_step_up(pawn, desired_dir, delta * (1.0 - landed.time), landed, depth + 1);
            }
            let old_floor = floor;
            floor = hit.normal;
            self.set_vector(pawn, Prop::Floor, floor);
            let wall = Vector::new(hit.normal[0], hit.normal[1], 0.0).safe_normal();
            let cross_y = floor.cross(old_floor).safe_normal();
            let x = cross_y.cross(old_floor).safe_normal().dot(delta);
            let (y, z) = (cross_y.dot(delta), old_floor.dot(delta));
            let along = ((cross_y.cross(floor) * x) + (cross_y * y)) + (floor * z);
            if along.dot(delta) >= 0.0 {
                let again = self.move_actor(pawn, along, Bases::Block)?;
                if again.time < 1.0 {
                    self.process_hit_wall(pawn, again.normal, again.actor)?;
                    if self.store.physics_of(pawn) == Some(EPhysics::Falling) {
                        return Ok(());
                    }
                    let adjusted = two_wall_adjust(desired_dir, along, again.normal, wall, again.time);
                    self.move_actor(pawn, adjusted, Bases::Block)?;
                }
            }
        }
        self.move_actor(pawn, floor * (-MAX_STEP_HEIGHT), Bases::Block)?;
        Ok(())
    }
}
