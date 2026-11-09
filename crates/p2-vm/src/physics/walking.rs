use p2_math::Vector;
use crate::enums::EPhysics;
use crate::{Error, LoaderRef, ObjectId, Prop, Value, Vm, Kind};
use super::{
    Bases, DEFAULT_GROUND_FRICTION, FLOOR_CHECK, MAX_FLOOR_DIST, MAX_ITERATIONS, MAX_STEP_HEIGHT, MAX_STEP_SIDE_Z,
    MIN_FLOOR_DIST, MIN_FLOOR_Z, MIN_SLOPE_FRICTION, MoveHit, SLOPE_SLIDE_FRICTION, STEP_AGAIN_SQUARED, Sweep,
    SweepAgainst, VolumeProps, WalkStart, gravity_direction, horizontal_size, nearly_zero, substep, two_wall_adjust,
};

impl<L: LoaderRef> Vm<L> {
    /// Walking AI does not step off ledges: the move is dropped (unverified).
    /// A move that leaves the pawn swimming ends in `start_swimming`, with the frame's start and velocity.
    pub(super) fn phys_walking(&mut self, pawn: ObjectId, delta: f32, mut iterations: u32) -> Result<(), Error> {
        let Some(controller) = self.store.object_of(pawn, Prop::Controller) else { return Ok(()) };
        let VolumeProps { gravity, ground_friction, .. } = self.volume(pawn);
        let friction = if ground_friction == DEFAULT_GROUND_FRICTION { self.store.float_of(pawn, Prop::WalkFriction) } else { ground_friction };
        let desired_move = self.walking_velocity(pawn, delta, friction);
        let grav_dir = gravity_direction(gravity);
        let down = grav_dir * FLOOR_CHECK;
        let (radius, height) = self.store.collision_size(pawn);
        let start = WalkStart { location: self.store.vector(pawn, Prop::Location), floor: self.store.vector(pawn, Prop::Floor), base: self.store.object_of(pawn, Prop::Base) };
        self.store.set(pawn, Prop::bJustTeleported, Value::Bool(false));
        let avoids_ledges = (self.store.flag_of(pawn, Prop::bIsWalking) || self.store.flag_of(pawn, Prop::bIsCrouched))
            && !self.store.flag_of(pawn, Prop::bCanFly)
            && !self.store.flag_of(pawn, Prop::bCanWalkOffLedges);
        let mut checked_fall = false;
        let mut remaining = delta;
        while remaining > 0.0 && iterations < MAX_ITERATIONS && self.store.object_of(pawn, Prop::Controller).is_some() {
            iterations += 1;
            let tick = substep(remaining);
            remaining -= tick;
            let mut step = desired_move * tick;
            let sub_location = self.store.vector(pawn, Prop::Location);
            if nearly_zero(step) {
                remaining = 0.0;
            } else {
                if avoids_ledges {
                    let ahead = sub_location + step;
                    if self.find_floor(pawn, ahead, radius, height, FLOOR_CHECK).is_none_or(|f| f.normal[2] < MIN_FLOOR_Z) {
                        step = Vector::ZERO;
                        checked_fall = true;
                    }
                }
                let hit = self.move_actor(pawn, step, Bases::Block)?;
                if hit.time < 1.0 {
                    let length = step.size();
                    self.step_up(pawn, grav_dir, step * (1.0 / length), step * (1.0 - hit.time), hit, 0)?;
                    if self.store.physics_of(pawn) == Some(EPhysics::Falling) {
                        let done = horizontal_size(self.store.vector(pawn, Prop::Location) - sub_location);
                        let left = (1.0 - (done / length).min(1.0)) * tick + remaining;
                        self.event(pawn, crate::states::Event::Falling, Vec::new())?;
                        return self.start_new_physics(pawn, left, iterations);
                    }
                }
            }
            if self.store.is_deleted(pawn) {
                return Ok(());
            }
            if self.store.physics_of(pawn) == Some(EPhysics::Swimming) {
                return self.start_swimming(pawn, start.location, self.store.vector(pawn, Prop::Velocity), tick, remaining, iterations);
            }
            let floor = self.settle_on_floor(pawn, step, down, radius, height)?;
            if self.store.is_deleted(pawn) {
                return Ok(());
            }
            if self.store.physics_of(pawn) == Some(EPhysics::Swimming) {
                return self.start_swimming(pawn, start.location, self.store.vector(pawn, Prop::Velocity), tick, remaining, iterations);
            }
            if floor.time < 1.0 && floor.normal[2] >= MIN_FLOOR_Z {
                if floor.normal[2] < 1.0 && floor.normal[2] * friction < SLOPE_SLIDE_FRICTION {
                    let slide = gravity * (delta * delta / (2.0 * friction.max(MIN_SLOPE_FRICTION)));
                    let slip = slide - (floor.normal * slide.dot(floor.normal));
                    if slip.dot(slide) >= 0.0 {
                        self.move_actor(pawn, slip, Bases::Block)?;
                        if !self.store.is_deleted(pawn) && self.store.physics_of(pawn) == Some(EPhysics::Swimming) {
                            return self.start_swimming(pawn, start.location, self.store.vector(pawn, Prop::Velocity), tick, remaining, iterations);
                        }
                    }
                }
                continue;
            }
            if self.ledge_stops(pawn, controller, checked_fall, start)? {
                return Ok(());
            }
            let length = step.size();
            let left = if length == 0.0 {
                0.0
            } else {
                let done = horizontal_size(self.store.vector(pawn, Prop::Location) - sub_location);
                (1.0 - (done / length).min(1.0)) * tick + remaining
            };
            return self.walk_off_ledge(pawn, left, iterations);
        }
        let mut velocity = if !self.store.flag_of(pawn, Prop::bJustTeleported) && !self.store.flag_of(pawn, Prop::bNoVelocityUpdate) {
            (self.store.vector(pawn, Prop::Location) - start.location) * (1.0 / delta)
        } else {
            self.store.vector(pawn, Prop::Velocity)
        };
        self.store.set(pawn, Prop::bNoVelocityUpdate, Value::Bool(false));
        velocity[2] = 0.0;
        self.set_vector(pawn, Prop::Velocity, velocity);
        Ok(())
    }

    fn walking_velocity(&mut self, pawn: ObjectId, delta: f32, friction: f32) -> Vector {
        let mut velocity = self.store.vector(pawn, Prop::Velocity);
        velocity[2] = 0.0;
        self.set_vector(pawn, Prop::Velocity, velocity);
        let mut acceleration = self.store.vector(pawn, Prop::Acceleration);
        acceleration[2] = 0.0;
        self.set_vector(pawn, Prop::Acceleration, acceleration);
        let accel_dir = acceleration.safe_normal();
        self.calc_velocity(pawn, accel_dir, delta, self.max_speed(pawn), friction);
        let mut desired_move = self.store.vector(pawn, Prop::Velocity);
        desired_move += self.zone_push(pawn, 25.0 * delta);
        desired_move[2] = 0.0;
        desired_move
    }

    fn settle_on_floor(&mut self, pawn: ObjectId, step: Vector, down: Vector, radius: f32, height: f32) -> Result<MoveHit, Error> {
        let location = self.store.vector(pawn, Prop::Location);
        let mut floor = self.sweep(pawn, Sweep { start: location, delta: down, radius, height }, SweepAgainst::WORLD_AND_ACTORS, None);
        let floor_distance = floor.time * FLOOR_CHECK;
        self.set_vector(pawn, Prop::Floor, floor.normal);
        let walking = |vm: &Self| vm.store.physics_of(pawn) == Some(EPhysics::Walking);
        if floor.normal[2] < MIN_FLOOR_Z && !nearly_zero(step) && step.dot(floor.normal) < 0.0 {
            let up = Vector::new(0.0, 0.0, MAX_STEP_HEIGHT);
            let slide = up - (floor.normal * up.dot(floor.normal));
            floor = self.move_actor(pawn, slide * (-1.0), Bases::Block)?;
            if floor.actor != self.store.object_of(pawn, Prop::Base) && walking(self) {
                self.set_base(pawn, floor.actor, floor.normal)?;
            }
        } else if floor.time < 1.0 && (floor.actor != self.store.object_of(pawn, Prop::Base) || floor_distance > MAX_FLOOR_DIST) {
            floor = self.move_actor(pawn, down, Bases::Block)?;
            if floor.actor != self.store.object_of(pawn, Prop::Base) && walking(self) {
                self.set_base(pawn, floor.actor, floor.normal)?;
            }
        } else if floor_distance < MIN_FLOOR_DIST {
            self.move_actor(pawn, [0.0, 0.0, 0.5 * (MIN_FLOOR_DIST + MAX_FLOOR_DIST) - floor_distance].into(), Bases::Block)?;
        }
        Ok(floor)
    }

    fn ledge_stops(&mut self, pawn: ObjectId, controller: ObjectId, checked_fall: bool, start: WalkStart) -> Result<bool, Error> {
        if self.store.flag_of(pawn, Prop::bCanJump) && !checked_fall {
            self.event(controller, crate::states::Event::MayFall, Vec::new())?;
        }
        let stops = !self.store.flag_of(pawn, Prop::bCanJump)
            || (!self.store.flag_of(pawn, Prop::bCanWalkOffLedges) && (self.store.flag_of(pawn, Prop::bIsWalking) || self.store.flag_of(pawn, Prop::bIsCrouched)));
        if self.store.flag_of(pawn, Prop::bJustTeleported) || !stops {
            return Ok(false);
        }
        self.set_vector(pawn, Prop::Velocity, Vector::ZERO);
        self.set_vector(pawn, Prop::Acceleration, Vector::ZERO);
        self.set_vector(pawn, Prop::Location, start.location);
        self.relocate(pawn);
        if let Some(base) = start.base
            && (self.store.flag_of(base, Prop::bStatic) || self.store.flag_of(base, Prop::bWorldGeometry) || !self.store.flag_of(base, Prop::bMovable))
        {
            self.set_base(pawn, Some(base), start.floor)?;
        }
        self.store.set(controller, Prop::MoveTimer, Value::Float(-1.0));
        Ok(true)
    }

    fn walk_off_ledge(&mut self, pawn: ObjectId, left: f32, iterations: u32) -> Result<(), Error> {
        let mut velocity = self.store.vector(pawn, Prop::Velocity);
        velocity[2] = 0.0;
        self.set_vector(pawn, Prop::Velocity, velocity);
        self.event(pawn, crate::states::Event::Falling, Vec::new())?;
        if self.store.physics_of(pawn) == Some(EPhysics::Walking) {
            self.set_physics(pawn, EPhysics::Falling, None)?;
        }
        self.start_new_physics(pawn, left, iterations)
    }

    pub(super) fn step_up(&mut self, pawn: ObjectId, grav_dir: Vector, desired_dir: Vector, step: Vector, mut hit: MoveHit, depth: u32) -> Result<(), Error> {
        let down = grav_dir * MAX_STEP_HEIGHT;
        if hit.normal[2].abs() < MAX_STEP_SIDE_Z {
            self.move_actor(pawn, down * (-1.0), Bases::Block)?;
            hit = self.move_actor(pawn, step, Bases::Block)?;
        } else if hit.normal[2] >= MIN_FLOOR_Z || self.store.physics_of(pawn) != Some(EPhysics::Walking) {
            let climb = step + [0.0, 0.0, step.size() * hit.normal[2]].into();
            hit = self.move_actor(pawn, climb, Bases::Block)?;
        }
        if hit.time < 1.0 {
            let pushed = hit.actor.filter(|&d| {
                self.is_human_controlled(pawn) && self.store.is_a(d, Kind::Decoration) && self.store.flag_of(d, Prop::bPushable) && hit.normal.dot(desired_dir) < -0.9
            });
            if let Some(decoration) = pushed {
                self.store.set(pawn, Prop::bNoVelocityUpdate, Value::Bool(true));
                let mass = self.store.float_of(pawn, Prop::Mass);
                let share = mass / (mass + self.store.float_of(decoration, Prop::Mass));
                if share.is_finite() {
                    self.set_vector(pawn, Prop::Velocity, self.store.vector(pawn, Prop::Velocity) * share);
                }
                self.process_hit_wall(pawn, hit.normal, Some(decoration))?;
                if self.store.physics_of(pawn) == Some(EPhysics::Falling) {
                    return Ok(());
                }
            } else if hit.normal[2].abs() < MAX_STEP_SIDE_Z && step.dot(step) * hit.time > STEP_AGAIN_SQUARED && depth < 4 {
                let landed = self.move_actor(pawn, down, Bases::Block)?;
                return self.step_up(pawn, grav_dir, desired_dir, step * (1.0 - landed.time), landed, depth + 1);
            }
            self.process_hit_wall(pawn, hit.normal, hit.actor)?;
            if self.store.physics_of(pawn) == Some(EPhysics::Falling) {
                return Ok(());
            }
            let wall = Vector::new(hit.normal[0], hit.normal[1], 0.0).safe_normal();
            let mut slide = (step - (wall * step.dot(wall))) * (1.0 - hit.time);
            if slide.dot(step) >= 0.0 {
                let again = self.move_actor(pawn, slide, Bases::Block)?;
                if again.time < 1.0 {
                    self.process_hit_wall(pawn, again.normal, again.actor)?;
                    if self.store.physics_of(pawn) == Some(EPhysics::Falling) {
                        return Ok(());
                    }
                    slide = two_wall_adjust(desired_dir, slide, again.normal, wall, again.time);
                    self.move_actor(pawn, slide, Bases::Block)?;
                }
            }
        }
        self.move_actor(pawn, down, Bases::Block)?;
        Ok(())
    }

    pub(super) fn process_hit_wall(&mut self, pawn: ObjectId, hit_normal: Vector, wall: Option<ObjectId>) -> Result<(), Error> {
        let Some(wall) = wall.filter(|&w| !self.store.is_a(w, Kind::Pawn)) else { return Ok(()) };
        let args = [Value::Vector(hit_normal), Value::Object(Some(wall))];
        let Some(controller) = self.store.object_of(pawn, Prop::Controller) else { return self.event(pawn, crate::states::Event::HitWall, args) };
        if self.store.vector(pawn, Prop::Acceleration) == Vector::ZERO {
            return Ok(());
        }
        let mut direction = (self.store.vector(controller, Prop::Destination) - self.store.vector(pawn, Prop::Location)).safe_normal();
        let mut facing = hit_normal;
        if self.store.physics_of(pawn) == Some(EPhysics::Walking) {
            direction[2] = 0.0;
            facing[2] = 0.0;
        }
        if direction.dot(facing) <= self.store.float_of(controller, Prop::MinHitWall)
            && !self.event_bool(controller, crate::states::Event::NotifyHitWall, args.clone())?
        {
            self.event(pawn, crate::states::Event::HitWall, args)?;
        }
        Ok(())
    }
}
