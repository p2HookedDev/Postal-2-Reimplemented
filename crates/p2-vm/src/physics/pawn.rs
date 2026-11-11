use p2_math::Vector;
use crate::enums::EPhysics;
use crate::interp::float;
use crate::{Error, LoaderRef, ObjectId, Prop, Value, Vm, Kind};
use super::{
    BRAKE_STEP, DEFAULT_GRAVITY, DEFAULT_GROUND_FRICTION, DEFAULT_TERMINAL_VELOCITY, Floor, MAX_ITERATIONS,
    SOFT_KILL_Z_GRACE, Sweep, SweepAgainst, VolumeProps, turned_velocity,
};

impl<L: LoaderRef> Vm<L> {
    pub(super) fn is_human_controlled(&self, pawn: ObjectId) -> bool {
        self.store.object_of(pawn, Prop::Controller).is_some_and(|c| self.store.is_a(c, Kind::PlayerController))
    }

    pub(crate) fn find_floor(&mut self, pawn: ObjectId, start: Vector, radius: f32, height: f32, reach: f32) -> Option<Floor> {
        let bottom = start[2] - height;
        if let Some(hit) = self.trace_lines(start, [start[0], start[1], bottom].into())
            && hit.normal[2] > 0.0
        {
            return Some(Floor { distance: hit.location[2] - bottom, normal: hit.normal });
        }
        let hit = self.sweep(pawn, Sweep { start, delta: [0.0, 0.0, -reach].into(), radius, height }, SweepAgainst::WORLD_AND_ACTORS, None);
        (hit.time < 1.0).then_some(Floor { distance: hit.time * reach, normal: hit.normal })
    }

    pub(crate) fn volume(&self, pawn: ObjectId) -> VolumeProps {
        match self.store.object_of(pawn, Prop::PhysicsVolume).filter(|&v| self.store.object(v).class.is_some()) {
            Some(v) => VolumeProps {
                gravity: self.store.vector(v, Prop::Gravity),
                ground_friction: self.store.float_of(v, Prop::GroundFriction),
                terminal_velocity: self.store.float_of(v, Prop::TerminalVelocity),
                zone_velocity: self.store.vector(v, Prop::ZoneVelocity),
            },
            None => VolumeProps {
                gravity: DEFAULT_GRAVITY,
                ground_friction: DEFAULT_GROUND_FRICTION,
                terminal_velocity: DEFAULT_TERMINAL_VELOCITY,
                zone_velocity: Vector::ZERO,
            },
        }
    }

    pub(crate) fn gravity_of(&self, pawn: ObjectId) -> Vector {
        match self.store.object_of(pawn, Prop::PhysicsVolume).filter(|&v| self.store.object(v).class.is_some()) {
            Some(v) => self.store.vector(v, Prop::Gravity),
            None => DEFAULT_GRAVITY,
        }
    }

    pub(crate) fn kill_z(&self, actor: ObjectId, walking: bool) -> Option<f32> {
        let zone = self.region_of(actor).and_then(|(zone, _, _)| zone).or(self.level)?;
        let kill_z = self.store.float_of(zone, Prop::KillZ);
        Some(if self.store.flag_of(zone, Prop::bSoftKillZ) && !walking { kill_z - SOFT_KILL_Z_GRACE } else { kill_z })
    }

    /// Zone number 0 and no `bIgnoreOutOfWorld`; a level without zones has no outside here,
    /// while retail would count all of it as zone 0.
    pub(crate) fn out_of_world(&self, actor: ObjectId) -> bool {
        let zone_zero = self.region_of(actor).is_some_and(|(_, _, number)| number == 0);
        zone_zero && self.has_zones() && !self.store.flag_of(actor, Prop::bIgnoreOutOfWorld)
    }

    pub(crate) fn pawn_physics(&mut self, pawn: ObjectId, delta: f32) -> Result<(), Error> {
        let physics = self.store.physics_of(pawn);
        if matches!(physics, Some(EPhysics::Karma | EPhysics::KarmaRagDoll)) {
            return Ok(());
        }
        let controller = self.store.object_of(pawn, Prop::Controller);
        if controller.is_some() && controller == self.player() && !self.options.player_physics {
            return Ok(());
        }
        let start = self.store.vector(pawn, Prop::Location);
        if controller.is_some()
            && self.kill_z(pawn, physics == Some(EPhysics::Walking)).is_some_and(|kill_z| start[2] < kill_z)
        {
            self.event(pawn, crate::states::Event::FellOutOfWorld, Vec::new())?;
            if self.store.is_deleted(pawn) {
                return Ok(());
            }
        }
        if self.store.flag_of(pawn, Prop::bCollideWorld) && self.out_of_world(pawn) {
            self.event(pawn, crate::states::Event::FellOutOfWorld, Vec::new())?;
            return Ok(());
        }
        self.store.set(pawn, Prop::OldZ, Value::Float(start[2]));
        if self.store.flag_of(pawn, Prop::bWantsToCrouch) {
            if !self.store.flag_of(pawn, Prop::bIsCrouched) {
                self.crouch(pawn)?;
            } else if self.store.flag_of(pawn, Prop::bTryToUncrouch) {
                let left = self.store.float_of(pawn, Prop::UncrouchTime) - delta;
                self.store.set(pawn, Prop::UncrouchTime, Value::Float(left));
                if left <= 0.0 {
                    self.store.set(pawn, Prop::bWantsToCrouch, Value::Bool(false));
                    self.store.set(pawn, Prop::bTryToUncrouch, Value::Bool(false));
                }
            }
        }
        self.start_new_physics(pawn, delta, 0)?;
        if self.store.is_deleted(pawn) {
            return Ok(());
        }
        if self.store.flag_of(pawn, Prop::bIsCrouched) && !self.store.flag_of(pawn, Prop::bWantsToCrouch) {
            self.uncrouch(pawn)?;
        }
        if let Some(controller) = self.store.object_of(pawn, Prop::Controller) {
            let timer = self.store.float_of(controller, Prop::MoveTimer) - delta;
            self.store.set(controller, Prop::MoveTimer, Value::Float(timer));
            let physics = self.store.physics_of(pawn);
            let turning = self.store.flag_of(pawn, Prop::bCrawler)
                || self.store.rotator_of(pawn, Prop::Rotation) != self.store.rotator_of(pawn, Prop::DesiredRotation)
                || self.store.rotator_of(pawn, Prop::RotationRate).roll > 0
                || self.is_human_controlled(pawn);
            if !self.store.flag_of(pawn, Prop::bInterpolating) && !matches!(physics, Some(EPhysics::Karma | EPhysics::KarmaRagDoll)) && turning {
                self.pawn_rotation(pawn, delta);
            }
        }
        let average = self.store.float_of(pawn, Prop::AvgPhysicsTime) * 0.8 + delta * 0.2;
        self.store.set(pawn, Prop::AvgPhysicsTime, Value::Float(average));
        if self.store.vector(pawn, Prop::Location) != start {
            let phase = self.profile_start();
            self.update_touching(pawn)?;
            self.profile_phase("touching", phase);
        }
        Ok(())
    }

    fn crouch(&mut self, pawn: ObjectId) -> Result<(), Error> {
        let (radius, height) = self.store.collision_size(pawn);
        let (crouch_radius, crouch_height) = (self.store.float_of(pawn, Prop::CrouchRadius), self.store.float_of(pawn, Prop::CrouchHeight));
        if radius == crouch_radius && height == crouch_height {
            return Ok(());
        }
        self.store.set(pawn, Prop::CollisionRadius, Value::Float(crouch_radius));
        self.store.set(pawn, Prop::CollisionHeight, Value::Float(crouch_height));
        let adjust = height - crouch_height;
        if (crouch_radius > radius || crouch_height > height) && self.encroached(pawn).is_some() {
            self.store.set(pawn, Prop::CollisionRadius, Value::Float(radius));
            self.store.set(pawn, Prop::CollisionHeight, Value::Float(height));
            return Ok(());
        }
        let location = self.store.vector(pawn, Prop::Location);
        self.set_vector(pawn, Prop::Location, [location[0], location[1], location[2] - adjust].into());
        self.relocate(pawn);
        self.set_vector(pawn, Prop::PrePivot, [0.0, 0.0, adjust].into());
        self.store.set(pawn, Prop::bIsCrouched, Value::Bool(true));
        self.event(pawn, crate::states::Event::StartCrouch, [Value::Float(adjust)])
    }

    fn uncrouch(&mut self, pawn: ObjectId) -> Result<(), Error> {
        let Some(class) = self.store.object(pawn).class else { return Ok(()) };
        let default = |vm: &Self, name: Prop| vm.store.default_value(class, name).as_ref().map_or(0.0, float);
        let (radius, height) = (default(self, Prop::CollisionRadius), default(self, Prop::CollisionHeight));
        let (crouch_radius, crouch_height) = self.store.collision_size(pawn);
        let adjust = height - crouch_height;
        let location = self.store.vector(pawn, Prop::Location);
        let up = Vector::new(location.x, location.y, location.z + 2.0 * adjust);
        let blocked_above = self.trace_world(location, up, [crouch_radius, crouch_radius, crouch_height].into()).is_some();
        self.store.set(pawn, Prop::CollisionRadius, Value::Float(radius));
        self.store.set(pawn, Prop::CollisionHeight, Value::Float(height));
        self.set_vector(pawn, Prop::Location, [location[0], location[1], location[2] + adjust].into());
        if blocked_above || self.encroached(pawn).is_some() {
            self.set_vector(pawn, Prop::Location, location);
            self.store.set(pawn, Prop::CollisionRadius, Value::Float(crouch_radius));
            self.store.set(pawn, Prop::CollisionHeight, Value::Float(crouch_height));
            return Ok(());
        }
        // The size and place tried above are kept: the pawn's hash entry follows (the encroachment test skips the
        // pawn itself, so a tried and undone size needs none).
        self.relocate(pawn);
        self.set_vector(pawn, Prop::PrePivot, Vector::ZERO);
        self.store.set(pawn, Prop::bIsCrouched, Value::Bool(false));
        self.event(pawn, crate::states::Event::EndCrouch, [Value::Float(adjust)])
    }

    pub(super) fn start_new_physics(&mut self, pawn: ObjectId, delta: f32, iterations: u32) -> Result<(), Error> {
        if delta < 0.0003 || iterations >= MAX_ITERATIONS {
            return Ok(());
        }
        match self.store.physics_of(pawn) {
            Some(EPhysics::Walking) => self.phys_walking(pawn, delta, iterations),
            Some(EPhysics::Falling) => self.phys_falling(pawn, delta, iterations),
            Some(EPhysics::Swimming) => self.phys_swimming(pawn, delta, iterations),
            Some(EPhysics::Flying) => self.phys_flying(pawn, delta),
            Some(EPhysics::Spider) => self.phys_spider(pawn, delta, iterations),
            Some(EPhysics::Ladder) => self.phys_ladder(pawn, delta, iterations),
            Some(EPhysics::RootMotion) => {
                self.phys_root_motion(pawn);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub(super) fn velocity_caps(&self, pawn: ObjectId, max_speed: f32) -> (f32, f32) {
        let walking = self.store.flag_of(pawn, Prop::bIsWalking) || self.store.flag_of(pawn, Prop::bIsCrouched) || self.store.flag_of(pawn, Prop::bIsDeathCrawling);
        let walking_pct = self.store.float_of(pawn, Prop::WalkingPct);
        let accel_rate = self.store.float_of(pawn, Prop::AccelRate);
        let max_speed = if self.is_human_controlled(pawn) { max_speed } else { max_speed * self.store.float_of(pawn, Prop::DesiredSpeed) };
        if walking { (walking_pct * accel_rate, walking_pct * max_speed) } else { (accel_rate, max_speed) }
    }

    pub(super) fn calc_velocity(&mut self, pawn: ObjectId, accel_dir: Vector, delta: f32, max_speed: f32, friction: f32) {
        let (accel_limit, speed_limit) = self.velocity_caps(pawn, max_speed);
        let mut velocity = self.store.vector(pawn, Prop::Velocity);
        let mut acceleration = self.store.vector(pawn, Prop::Acceleration);
        if acceleration == Vector::ZERO {
            let old = velocity;
            let mut sum = Vector::ZERO;
            let mut remaining = delta;
            while remaining > BRAKE_STEP {
                velocity = velocity - (velocity * (2.0 * BRAKE_STEP * friction));
                if velocity.dot(old) > 0.0 {
                    sum += velocity * (BRAKE_STEP / delta);
                }
                remaining -= BRAKE_STEP;
            }
            velocity = velocity - (velocity * (2.0 * remaining * friction));
            if velocity.dot(old) > 0.0 {
                sum += velocity * (remaining / delta);
            }
            velocity = sum;
            if old.dot(velocity) < 0.0 || velocity.dot(velocity) < 100.0 {
                velocity = Vector::ZERO;
            }
        } else {
            if acceleration.dot(acceleration) > accel_limit * accel_limit {
                acceleration = accel_dir * accel_limit;
            }
            velocity = turned_velocity(velocity, accel_dir, delta, friction);
        }
        velocity += acceleration * delta;
        if velocity.dot(velocity) > speed_limit * speed_limit {
            velocity = velocity.safe_normal() * speed_limit;
        }
        self.set_vector(pawn, Prop::Velocity, velocity);
        self.set_vector(pawn, Prop::Acceleration, acceleration);
    }

    pub(crate) fn max_speed(&self, pawn: ObjectId) -> f32 {
        match self.store.physics_of(pawn) {
            Some(EPhysics::Flying) => self.store.float_of(pawn, Prop::AirSpeed),
            Some(EPhysics::Swimming) => self.store.float_of(pawn, Prop::WaterSpeed),
            _ => self.store.float_of(pawn, Prop::GroundSpeed),
        }
    }
}
