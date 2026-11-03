use p2_math::Vector;
use crate::enums::EPhysics;
use crate::native_id::Native;
use crate::interp::float;
use crate::natives::{bool_arg, object_arg, optional_float, vec3};
use crate::{Error, LoaderRef, ObjectId, Value, Vm, Prop, Kind};

const DEFAULT_TARGET_RADIUS: f32 = 50.0;
const PAWN_TARGET_MOVE_TIMER: f32 = 1.2;
const LANDING_WAIT_SECONDS: f32 = 4.0;

/// Save games store a move as its place in this list, so the order is part of the save format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum LatentMove {
    MoveTo,
    MoveToward,
    FinishRotation,
    WaitForLanding,
    MoveToWithRadius,
    MoveTowardWithRadius,
    WaitToSeeEnemy,
}

impl LatentMove {
    fn number(self) -> i32 {
        match self {
            Self::MoveTo => 501,
            Self::MoveToward => 503,
            Self::FinishRotation => 509,
            Self::WaitForLanding => 528,
            Self::WaitToSeeEnemy => 511,
            Self::MoveToWithRadius => 1276,
            Self::MoveTowardWithRadius => 1278,
        }
    }
}

impl<L: LoaderRef> Vm<L> {
    #[expect(clippy::too_many_lines, reason = "one dispatch over the movement natives; each arm is short")]
    pub(crate) fn move_native(&mut self, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let Some(this) = this else { return Ok(None) };
        if native == Native::ReachedDestination {
            let goal = object_arg(args, 0);
            let reached = goal.is_some_and(|goal| {
                let direction = self.vector(goal, Prop::Location) - self.vector(this, Prop::Location);
                self.reached_destination(this, direction, Some(goal))
            });
            return Ok(Some(Value::Bool(reached)));
        }
        if native == Native::InLatentExecution {
            let number = args.first().map_or(0, crate::interp::int);
            let current = self.script.states.objects.get(&this).and_then(|s| s.latent_move);
            return Ok(Some(Value::Bool(current.is_some_and(|m| m.number() == number))));
        }
        let pawn = self.object_of(this, Prop::Pawn);
        match native {
            Native::MoveTo | Native::MoveToWithRadius => {
                let Some(pawn) = pawn else { return Ok(None) };
                let radius_version = native == Native::MoveToWithRadius;
                let destination = vec3(args.first());
                let focus = object_arg(args, 1);
                let (radius, speed, walk) = match radius_version {
                    true => (optional_float(args, 2, DEFAULT_TARGET_RADIUS), optional_float(args, 3, 1.0), bool_arg(args, 4)),
                    false => (0.0, optional_float(args, 2, 1.0), bool_arg(args, 3)),
                };
                self.set_walking(pawn, walk)?;
                self.set(this, Prop::MoveTarget, Value::Object(None));
                self.set(pawn, Prop::bReducedSpeed, Value::Bool(false));
                self.set(pawn, Prop::DesiredSpeed, Value::Float(self.desired_speed(pawn, speed)));
                self.set(pawn, Prop::DestinationOffset, Value::Float(0.0));
                self.set(pawn, Prop::NextPathRadius, Value::Float(0.0));
                self.set(this, Prop::Focus, Value::Object(focus));
                if radius_version {
                    let timer = self.float_of(pawn, Prop::PathCheckTimePoint);
                    self.set(this, Prop::MoveTimer, Value::Float(timer));
                    self.set(this, Prop::TargetRadius, Value::Float(radius));
                } else {
                    let distance = (destination - self.vector(pawn, Prop::Location)).size();
                    self.set_move_timer(pawn, this, distance);
                }
                self.set_vector(this, Prop::Destination, destination);
                if focus.is_none() {
                    self.set_vector(this, Prop::FocalPoint, destination);
                }
                self.start_move(this, pawn, destination, false);
                if radius_version {
                    self.move_toward_with_radius(pawn, destination, None, radius)?;
                    self.latent_wait(LatentMove::MoveToWithRadius);
                } else {
                    self.move_toward(pawn, destination, None)?;
                    self.latent_wait(LatentMove::MoveTo);
                }
            }
            Native::MoveToward | Native::MoveTowardWithRadius => {
                let (Some(target), Some(pawn)) = (object_arg(args, 0), pawn) else { return Ok(None) };
                let radius_version = native == Native::MoveTowardWithRadius;
                let focus = object_arg(args, 1);
                let first = usize::from(radius_version);
                let radius = if radius_version { optional_float(args, 2, 50.0) } else { 0.0 };
                let speed = optional_float(args, 2 + first, 1.0);
                let offset = optional_float(args, 3 + first, 0.0);
                let strafing = bool_arg(args, 4 + first);
                self.set_walking(pawn, bool_arg(args, 5 + first))?;
                self.set(pawn, Prop::bReducedSpeed, Value::Bool(false));
                self.set(pawn, Prop::DesiredSpeed, Value::Float(self.desired_speed(pawn, speed)));
                self.set(this, Prop::MoveTarget, Value::Object(Some(target)));
                self.set(this, Prop::Focus, Value::Object(focus));
                let destination = self.vector(target, Prop::Location);
                if radius_version {
                    let timer = self.float_of(pawn, Prop::PathCheckTimeActor);
                    self.set(this, Prop::MoveTimer, Value::Float(timer));
                    self.set(this, Prop::TargetRadius, Value::Float(radius));
                } else if self.is_a(target, Kind::Pawn) {
                    self.set(this, Prop::MoveTimer, Value::Float(PAWN_TARGET_MOVE_TIMER));
                } else {
                    let distance = (destination - self.vector(pawn, Prop::Location)).size();
                    self.set_move_timer(pawn, this, distance);
                }
                self.set_vector(this, Prop::Destination, destination);
                self.start_move(this, pawn, destination, strafing);
                let mut destination_offset = offset;
                if self.is_a(target, Kind::NavigationPoint) {
                    if self.flag_of(target, Prop::bSpecialMove) {
                        self.event(target, "SuggestMovePreparation", [Value::Object(Some(pawn))])?;
                    }
                    let straight = self.flag_of(target, Prop::bNeverUseStrafing) || self.flag_of(target, Prop::bForceNoStrafing);
                    if !radius_version && offset == 0.0 && !straight && self.object_of(this, Prop::RouteGoal) != Some(target) {
                        let room = (self.float_of(pawn, Prop::NextPathRadius) - self.float_of(pawn, Prop::CollisionRadius)).max(0.0);
                        destination_offset = (self.random() * 0.3 + 0.7) * room;
                    }
                }
                self.set(pawn, Prop::DestinationOffset, Value::Float(destination_offset));
                self.set(pawn, Prop::NextPathRadius, Value::Float(0.0));
                if !self.flag_of(this, Prop::bPreparingMove) {
                    if radius_version {
                        self.move_toward_with_radius(pawn, destination, Some(target), radius)?;
                    } else {
                        self.move_toward(pawn, destination, Some(target))?;
                    }
                }
                self.latent_wait(if radius_version { LatentMove::MoveTowardWithRadius } else { LatentMove::MoveToward });
            }
            Native::FinishRotation => self.latent_wait(LatentMove::FinishRotation),
            Native::EndClimbLadder => self.end_climb_ladder(this, pawn),
            Native::StopWaiting => self.stop_waiting(this),
            Native::FindStairRotation => {
                let pitch = self.find_stair_rotation(this, pawn, optional_float(args, 0, 0.0));
                return Ok(Some(Value::Int(pitch)));
            }
            Native::WaitToSeeEnemy => {
                if let (Some(_), Some(enemy)) = (pawn, self.object_of(this, Prop::Enemy)) {
                    self.set(this, Prop::Focus, Value::Object(Some(enemy)));
                    self.latent_wait(LatentMove::WaitToSeeEnemy);
                }
            }
            Native::EAdjustJump => {
                let Some(pawn) = pawn else { return Ok(Some(Value::Vector(Vector::ZERO))) };
                let to = self.vector(this, Prop::Destination) - self.vector(pawn, Prop::Location);
                let (jump_z, ground_speed) = (self.float_of(pawn, "JumpZ"), self.float_of(pawn, "GroundSpeed"));
                let gravity_z = self.gravity_of(pawn).z;
                let velocity = crate::movement_natives::suggest_jump_velocity(to, optional_float(args, 1, 0.0), optional_float(args, 0, 0.0), jump_z, ground_speed, gravity_z);
                return Ok(Some(Value::Vector(velocity)));
            }
            Native::WaitForLanding => {
                self.set(this, Prop::LatentFloat, Value::Float(LANDING_WAIT_SECONDS));
                if pawn.is_some_and(|p| self.physics_of(p) == Some(EPhysics::Falling)) {
                    self.latent_wait(LatentMove::WaitForLanding);
                }
            }
            _ => return Err(Error::UnknownNative(format!("{native:?}"))),
        }
        Ok(None)
    }

    fn latent_wait(&mut self, action: LatentMove) {
        self.script.latent.sleep = Some(0.0);
        self.script.latent.movement = Some(action);
    }

    fn start_move(&mut self, controller: ObjectId, pawn: ObjectId, destination: Vector, strafing: bool) {
        self.set(controller, Prop::bAdjusting, Value::Bool(false));
        self.set(controller, Prop::CurrentPath, Value::Object(None));
        self.set(controller, Prop::bAdvancedTactics, Value::Bool(strafing));
        self.set_vector(controller, Prop::AdjustLoc, destination);
        self.set(pawn, Prop::SerpentineTime, Value::Float(0.0));
        self.set(pawn, Prop::SerpentineDist, Value::Float(0.0));
    }

    fn set_walking(&mut self, pawn: ObjectId, walk: bool) -> Result<(), Error> {
        if walk != self.flag_of(pawn, Prop::bIsWalking) {
            self.event(pawn, "SetWalking", [Value::Bool(walk)])?;
        }
        Ok(())
    }

    fn desired_speed(&self, pawn: ObjectId, speed: f32) -> f32 {
        let max = self.float_of(pawn, Prop::MaxDesiredSpeed);
        if max < 0.0 { 0.0 } else { speed.min(max) }
    }

    fn set_move_timer(&mut self, pawn: ObjectId, controller: ObjectId, distance: f32) {
        let desired = self.float_of(pawn, Prop::DesiredSpeed);
        let timer = if desired == 0.0 {
            0.5
        } else {
            let walking = self.flag_of(pawn, Prop::bIsWalking) || self.flag_of(pawn, Prop::bIsCrouched);
            let factor = if walking { 2.0_f32.max(1.0 / self.float_of(pawn, Prop::WalkingPct)) } else { 2.0 };
            factor * distance / (self.max_speed(pawn) * desired) + 1.0
        };
        self.set(controller, Prop::MoveTimer, Value::Float(timer));
    }

    pub(crate) fn poll_latent_move(&mut self, controller: ObjectId, action: LatentMove, delta: f32) -> Result<bool, Error> {
        let pawn = self.object_of(controller, Prop::Pawn);
        let timed_out = self.float_of(controller, Prop::MoveTimer) < 0.0;
        match action {
            LatentMove::MoveTo | LatentMove::MoveToWithRadius => {
                let Some(pawn) = pawn.filter(|_| !timed_out) else { return Ok(true) };
                let radius = self.float_of(controller, Prop::TargetRadius);
                let step = |vm: &mut Self, to: Vector| match action {
                    LatentMove::MoveTo => vm.move_toward(pawn, to, None),
                    _ => vm.move_toward_with_radius(pawn, to, None, radius),
                };
                if self.poll_adjust(controller, step)? {
                    return Ok(false);
                }
                step(self, self.vector(controller, Prop::Destination))
            }
            LatentMove::MoveToward | LatentMove::MoveTowardWithRadius => {
                let target = self.object_of(controller, Prop::MoveTarget);
                let (Some(target), Some(pawn)) = (target, pawn) else { return Ok(true) };
                if timed_out {
                    return Ok(true);
                }
                if self.flag_of(controller, Prop::bPreparingMove) {
                    return Ok(false);
                }
                let radius = self.float_of(controller, Prop::TargetRadius);
                let step = |vm: &mut Self, to: Vector| match action {
                    LatentMove::MoveToward => vm.move_toward(pawn, to, Some(target)),
                    _ => vm.move_toward_with_radius(pawn, to, Some(target), radius),
                };
                if self.poll_adjust(controller, step)? {
                    return Ok(false);
                }
                let mut destination = self.vector(target, Prop::Location);
                if self.physics_of(pawn) == Some(EPhysics::Flying) && self.is_a(target, Kind::Pawn) {
                    destination[2] += 0.7 * self.float_of(target, Prop::CollisionHeight);
                }
                self.set_vector(controller, Prop::Destination, destination);
                let done = step(self, destination)?;
                if let Some(target) = self.object_of(controller, Prop::MoveTarget) {
                    let location = self.vector(target, Prop::Location);
                    self.set_vector(controller, Prop::Destination, location);
                }
                Ok(done)
            }
            LatentMove::FinishRotation => {
                let Some(pawn) = pawn else { return Ok(true) };
                Ok(self.turned_to_desired(pawn))
            }
            LatentMove::WaitToSeeEnemy => {
                let (Some(pawn), Some(_)) = (pawn, self.object_of(controller, Prop::Enemy)) else { return Ok(true) };
                let now = self.level.map_or(0.0, |level| self.float_of(level, Prop::TimeSeconds));
                if now - self.float_of(controller, Prop::LastSeenTime) > 0.1 {
                    return Ok(false);
                }
                Ok(self.turned_to_desired(pawn))
            }
            LatentMove::WaitForLanding => {
                if pawn.is_some_and(|p| self.physics_of(p) != Some(EPhysics::Falling)) {
                    return Ok(true);
                }
                let left = self.float_of(controller, Prop::LatentFloat) - delta;
                self.set(controller, Prop::LatentFloat, Value::Float(left));
                if left < 0.0 {
                    self.event(controller, "LongFall", Vec::new())?;
                }
                Ok(false)
            }
        }
    }

    /// Only for walking pawns. Returns true once the destination is reached.
    pub(crate) fn move_toward(&mut self, pawn: ObjectId, destination: Vector, goal: Option<ObjectId>) -> Result<bool, Error> {
        self.move_toward_with(pawn, destination, goal, None)
    }

    /// Unlike `move_toward`, accelerates straight at `AccelRate` with no blending.
    pub(crate) fn move_toward_with_radius(&mut self, pawn: ObjectId, destination: Vector, goal: Option<ObjectId>, radius: f32) -> Result<bool, Error> {
        self.move_toward_with(pawn, destination, goal, Some(radius))
    }

    fn move_toward_with(&mut self, pawn: ObjectId, destination: Vector, goal: Option<ObjectId>, radius: Option<f32>) -> Result<bool, Error> {
        let Some(controller) = self.object_of(pawn, Prop::Controller) else { return Ok(false) };
        let goal = if self.flag_of(controller, Prop::bAdjusting) { None } else { goal };
        let location = self.vector(pawn, Prop::Location);
        let physics = self.physics_of(pawn);
        let mut direction = destination - location;
        match physics {
            Some(EPhysics::Walking) => direction[2] = 0.0,
            Some(EPhysics::Falling) => {
                let flat = Vector::new(direction[0], direction[1], 0.0).safe_normal();
                let accel = flat * self.float_of(pawn, Prop::AccelRate);
                self.set_vector(pawn, Prop::Acceleration, accel);
                return Ok(false);
            }
            _ => {}
        }
        let distance = direction.size();
        let fly = !self.flag_of(pawn, Prop::bCanStrafe) && matches!(physics, Some(EPhysics::Flying | EPhysics::Swimming));
        let to_goal = destination - location;
        let reached = match radius {
            Some(radius) => self.reached_destination_with_radius(pawn, to_goal, goal, radius),
            None => self.reached_destination(pawn, to_goal, goal),
        };
        if reached {
            if radius.is_some() && !fly {
                self.set_vector(pawn, Prop::Acceleration, Vector::ZERO);
            }
            if let Some(goal) = goal.filter(|&g| self.is_a(g, Kind::NavigationPoint)) {
                self.set(pawn, Prop::Anchor, Value::Object(Some(goal)));
            }
            self.event(controller, "HitPathGoal", [Value::Object(goal), Value::Vector(destination)])?;
            return Ok(true);
        }
        let heading = if fly {
            self.rotator_of(pawn, Prop::Rotation).vector()
        } else if distance > 0.0 {
            direction * (1.0 / distance)
        } else {
            direction
        };
        let accel_rate = self.float_of(pawn, Prop::AccelRate);
        let mut acceleration = self.vector(pawn, Prop::Acceleration);
        acceleration = if radius.is_some() || (acceleration[0] == 0.0 && acceleration[1] == 0.0) {
            heading * accel_rate
        } else {
            (acceleration * 0.8) + (heading * (accel_rate * 0.2))
        };
        let move_target = self.object_of(controller, Prop::MoveTarget);
        if !self.flag_of(controller, Prop::bAdjusting)
            && let Some(target) = move_target.filter(|&t| self.is_a(t, Kind::Pawn))
        {
            self.set_vector(pawn, Prop::Acceleration, acceleration);
            let reach = self.float_of(target, Prop::CollisionRadius) + self.float_of(pawn, Prop::CollisionRadius) + 0.8 * self.float_of(pawn, Prop::MeleeRange);
            return Ok(distance < reach);
        }
        let velocity = self.vector(pawn, Prop::Velocity);
        let speed = velocity.size();
        if !fly && speed > 100.0 {
            let velocity_dir = velocity * (1.0 / speed);
            let correction = (1.0 - velocity_dir.dot(heading)) * 0.2 * speed;
            acceleration -= (velocity_dir - heading) * correction;
        }
        self.set_vector(pawn, Prop::Acceleration, acceleration);
        if distance >= self.float_of(pawn, Prop::AvgPhysicsTime) * 1.4 * speed {
            return Ok(false);
        }
        let mut desired = self.float_of(pawn, Prop::DesiredSpeed);
        if !self.flag_of(pawn, Prop::bReducedSpeed) {
            desired *= 0.51;
            self.set(pawn, Prop::bReducedSpeed, Value::Bool(true));
        }
        if speed > 0.0 {
            desired = desired.min(200.0 / speed);
        }
        self.set(pawn, Prop::DesiredSpeed, Value::Float(desired));
        Ok(fly)
    }

    /// Cases between the two height thresholds (retail's slope test) count as not reached (unverified).
    pub(crate) fn reached_destination(&self, pawn: ObjectId, direction: Vector, goal: Option<ObjectId>) -> bool {
        let height = self.float_of(pawn, Prop::CollisionHeight);
        let radius = self.float_of(pawn, Prop::CollisionRadius);
        let default_height = self.object(pawn).class.and_then(|c| self.default_value(c, Prop::CollisionHeight)).as_ref().map_or(height, float);
        let (mut up, mut down, mut reach) = (2.0 * default_height - height, height, radius);
        if let Some(goal) = goal {
            let goal_height = self.float_of(goal, Prop::CollisionHeight);
            let goal_radius = self.float_of(goal, Prop::CollisionRadius);
            if self.is_a(goal, Kind::NavigationPoint) {
                up = up.max(goal_height + 2.0 - height + crate::physics::MAX_STEP_HEIGHT);
                down = down.max(height + 2.0 + crate::physics::MAX_STEP_HEIGHT - goal_height);
                reach = goal_radius + radius;
            } else if self.is_a(goal, Kind::Pawn) {
                up = up.max(goal_height);
                down = down.max(goal_height);
                let melee = self.float_of(pawn, Prop::MeleeRange).min(radius * 1.5);
                reach = (radius + melee + goal_radius) * 1.5;
            } else {
                up += goal_height;
                down += goal_height;
                if self.flag_of(goal, Prop::bBlockActors) {
                    reach = goal_radius + radius;
                }
            }
            reach += self.float_of(pawn, Prop::DestinationOffset);
        }
        within(direction, reach, up, down)
    }

    fn poll_adjust(&mut self, controller: ObjectId, step: impl Fn(&mut Self, Vector) -> Result<bool, Error>) -> Result<bool, Error> {
        if !self.flag_of(controller, Prop::bAdjusting) {
            return Ok(false);
        }
        let target = self.vector(controller, Prop::AdjustLoc);
        let adjusting = !step(self, target)?;
        self.set(controller, Prop::bAdjusting, Value::Bool(adjusting));
        Ok(adjusting)
    }

    fn turned_to_desired(&self, pawn: ObjectId) -> bool {
        let yaw = (self.rotator_of(pawn, Prop::DesiredRotation).yaw - (self.rotator_of(pawn, Prop::Rotation).yaw & 0xffff)).abs();
        !(2000..=63535).contains(&yaw)
    }

    fn reached_destination_with_radius(&self, pawn: ObjectId, direction: Vector, goal: Option<ObjectId>, radius: f32) -> bool {
        let height = self.float_of(pawn, Prop::CollisionHeight);
        let (threshold, reach) = match goal {
            None => (height + radius, radius),
            Some(goal) => (height + self.float_of(goal, Prop::CollisionHeight), radius + self.float_of(pawn, Prop::DestinationOffset)),
        };
        within(direction, reach, threshold, threshold)
    }

    pub(crate) fn controller_tick(&mut self, controller: ObjectId, delta: f32) -> Result<(), Error> {
        self.controller_sight(controller, delta)?;
        if self.is_deleted(controller) {
            return Ok(());
        }
        let Some(pawn) = self.object_of(controller, Prop::Pawn).filter(|&p| !self.is_deleted(p)) else { return Ok(()) };
        if self.flag_of(pawn, Prop::bRotateToDesired) {
            let focus = self.object_of(controller, Prop::Focus);
            let focal_point = self.vector(controller, Prop::FocalPoint);
            self.rotate_toward(pawn, focus, focal_point);
        }
        let rotation = self.rotator_of(pawn, Prop::Rotation);
        self.set(controller, Prop::Rotation, Value::Rotator(rotation));
        Ok(())
    }

    fn rotate_toward(&mut self, pawn: ObjectId, focus: Option<ObjectId>, focal_point: Vector) {
        let physics = self.physics_of(pawn);
        if physics == Some(EPhysics::Spider) {
            return;
        }
        if !self.flag_of(pawn, Prop::bCanStrafe) && matches!(physics, Some(EPhysics::Flying | EPhysics::Swimming)) {
            let ahead = self.rotator_of(pawn, Prop::Rotation).vector() * self.float_of(pawn, Prop::AccelRate);
            self.set_vector(pawn, Prop::Acceleration, ahead);
        }
        let point = focus.map_or(focal_point, |f| self.vector(f, Prop::Location));
        let mut desired = (point - self.vector(pawn, Prop::Location)).rotation();
        desired.yaw &= 0xffff;
        let toward_pawn = self.object_of(pawn, Prop::Controller).and_then(|c| self.object_of(c, Prop::MoveTarget)).is_some_and(|t| self.is_a(t, Kind::Pawn));
        if physics == Some(EPhysics::Walking) && !toward_pawn {
            desired.pitch = 0;
        }
        self.set(pawn, Prop::DesiredRotation, Value::Rotator(desired));
    }
}

fn within(direction: Vector, reach: f32, up: f32, down: f32) -> bool {
    if direction[0] * direction[0] + direction[1] * direction[1] > reach * reach {
        return false;
    }
    let threshold = if direction[2] > 0.0 { up } else { down };
    direction[2].abs() <= threshold
}
