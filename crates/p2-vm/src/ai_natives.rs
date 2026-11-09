use p2_math::{Rotator, Vector};
use crate::enums::EPhysics;
use crate::native_id::Native;
use crate::natives::vec3;
use crate::physics::{Bases, MoveHit};
use crate::senses::SightCheck;
use crate::{Error, Kind, LoaderRef, ObjectId, Prop, Value, Vm};

const VISIBILITY_SCALE: f32 = 1.0 / 128.0;
const WALL_STEP_FACTOR: f32 = 1.5;
const WALL_STEP_BASE: f32 = 16.0;
/// `moveSmooth` slides along a surface when its normal is at least this steep in Z, or the move is this vertical.
const SLIDE_NORMAL_Z: f32 = 0.2;
const SLIDE_UP: f32 = 0.5;
const SLIDE_DOWN: f32 = -0.2;
const AIM_YAW_DIVISOR: f32 = 15.0;
const AIM_PITCH_DIVISOR: f32 = 18.0;
const AIM_RENDERED_SECONDS: f32 = 0.1;

const MAX_NAVIGATION_POINTS: i32 = 100_000;

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn ai_native(&mut self, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let Some(this) = this else { return Ok(Some(Value::Bool(false))) };
        let object = |n: usize| crate::natives::object_arg(args, n);
        Ok(Some(match native {
            Native::CanSee => Value::Bool(match object(0) {
                Some(other) => self.see_pawn(this, other, SightCheck::Full),
                None => false,
            }),
            Native::PickWallAdjust => Value::Bool(self.pick_wall_adjust(this)),
            Native::FindPathTowardNearest => {
                let class = match object(0) {
                    Some(class) => self.class_of_object(class)?,
                    None => None,
                };
                Value::Object(self.find_path_toward_nearest(this, class)?)
            }
            Native::MoveSmooth => Value::Bool(self.move_smooth(this, vec3(args.first()))?),
            Native::GetAssistedAimTarget => Value::Object(self.assisted_aim_target(this, vec3(args.first()))),
            _ => return Err(Error::UnknownNative(format!("{native:?}"))),
        }))
    }

    pub(crate) fn see_pawn(&mut self, controller: ObjectId, other: ObjectId, check: SightCheck) -> bool {
        let Some(pawn) = self.store.object_of(controller, Prop::Pawn) else { return false };
        if self.store.object_of(controller, Prop::Enemy) == Some(other) {
            return self.line_of_sight_to(controller, other, SightCheck::Full);
        }
        let flag = self.store.flag_of(controller, Prop::bLOSflag);
        self.store.set(controller, Prop::bLOSflag, Value::Bool(!flag));
        let offset = self.store.vector(other, Prop::Location) - self.store.vector(pawn, Prop::Location);
        let distance_squared = offset.dot(offset);
        let reach = (self.store.byte_of(other, Prop::Visibility) as f32 * VISIBILITY_SCALE).min(1.0) * self.store.float_of(pawn, Prop::SightRadius);
        let reach_squared = reach * reach;
        if distance_squared > reach_squared {
            return false;
        }
        if check == SightCheck::Periodic && reach_squared * 0.25 < distance_squared && self.random() < 0.5 {
            return false;
        }
        let facing = offset.safe_normal().dot(self.store.rotator_of(controller, Prop::Rotation).vector());
        if facing < self.store.float_of(pawn, Prop::PeripheralVision) {
            return false;
        }
        if check == SightCheck::Periodic && self.random() * distance_squared > reach_squared * 0.01 {
            let flat = Vector::new(offset.x, offset.y, offset.z * 2.0);
            if flat.safe_normal().dot(self.store.rotator_of(controller, Prop::Rotation).vector()) < self.store.float_of(pawn, Prop::PeripheralVision) {
                return false;
            }
            let rise = offset[2].abs();
            if self.random() < rise * rise / distance_squared {
                return false;
            }
        }
        self.line_of_sight_to(controller, other, check)
    }

    /// Not modelled: retail skips the search under a level flag (unverified).
    fn assisted_aim_target(&mut self, weapon: ObjectId, start: Vector) -> Option<ObjectId> {
        let level = self.level?;
        let auto_aim = self.store.object_of(level, Prop::Game).map_or(1.0, |game| self.store.float_of(game, Prop::AutoAim));
        let assist = 1.0 - auto_aim;
        let (yaw_limit, pitch_limit) = (assist / AIM_YAW_DIVISOR, assist / AIM_PITCH_DIVISOR);
        let instigator = self.store.object_of(weapon, Prop::Instigator)?;
        let view = match self.store.object_of(instigator, Prop::Controller) {
            Some(controller) => self.store.rotator_of(controller, Prop::Rotation),
            None => self.store.rotator_of(instigator, Prop::Rotation),
        };
        let now = self.store.float_of(level, Prop::TimeSeconds);
        let pawns: Vec<ObjectId> = self.object_ids().filter(|&o| self.store.is_a(o, Kind::Pawn)).collect();
        pawns.into_iter().find(|&pawn| {
            if now - self.store.float_of(pawn, Prop::LastRenderTime) >= AIM_RENDERED_SECONDS || !matches!(self.store.get(pawn, Prop::Health), Some(Value::Int(health)) if health > 0) {
                return false;
            }
            let location = self.store.vector(pawn, Prop::Location);
            let to = (location - start).safe_normal();
            let target = to.rotation();
            let yaw_off = to.dot(Rotator::new(target.pitch, view.yaw, 0).vector()).clamp(-1.0, 1.0).acos();
            let pitch_off = to.dot(Rotator::new(view.pitch, target.yaw, 0).vector()).clamp(-1.0, 1.0).acos();
            yaw_off < self.store.float_of(pawn, Prop::CollisionRadius) * yaw_limit
                && pitch_off < self.store.float_of(pawn, Prop::CollisionHeight) * pitch_limit
                && self.trace_world(start, location, Vector::ZERO).is_none()
        })
    }

    /// A model for walking pawns. Not modelled: flying, swimming, jumping over a wall (unverified).
    fn pick_wall_adjust(&mut self, controller: ObjectId) -> bool {
        let Some(pawn) = self.store.object_of(controller, Prop::Pawn) else { return false };
        let physics = self.store.physics_of(pawn);
        if matches!(physics, Some(EPhysics::Falling | EPhysics::Swimming | EPhysics::Flying)) {
            return false;
        }
        let location = self.store.vector(pawn, Prop::Location);
        let (radius, height) = (self.store.float_of(pawn, Prop::CollisionRadius), self.store.float_of(pawn, Prop::CollisionHeight));
        let to = self.store.vector(controller, Prop::Destination) - location;
        let flat = Vector::new(to.x, to.y, 0.0);
        if to[2] < height && flat.dot(flat) - radius * radius < 0.0 {
            return false;
        }
        let distance = flat.size();
        if distance == 0.0 {
            return false;
        }
        let ahead = flat * (1.0 / distance);
        let left = Vector::new(ahead.y, -ahead.x, 0.0);
        let step = radius * WALL_STEP_FACTOR + WALL_STEP_BASE;
        let extent = Vector::new(radius, radius, height);
        for side in [left, (left * (-1.0))] {
            let aside = location + (side * step);
            let onward = aside + (ahead * step);
            if self.trace_world(location, aside, extent).is_none() && self.trace_world(aside, onward, extent).is_none() {
                self.set_vector(controller, Prop::AdjustLoc, aside);
                self.store.set(controller, Prop::bAdjusting, Value::Bool(true));
                return true;
            }
        }
        false
    }

    /// The last point of exactly `class` found becomes the `FindPath` goal.
    fn find_path_toward_nearest(&mut self, controller: ObjectId, class: Option<crate::ClassId>) -> Result<Option<ObjectId>, Error> {
        let (Some(class), Some(_)) = (class, self.store.object_of(controller, Prop::Pawn)) else {
            self.diagnostics.log.push(format!("Warning: No goal for FindPathTowardNearest by {}", self.store.object(controller).path));
            return Ok(None);
        };
        let mut goal = None;
        let mut next = self.level.and_then(|level| self.store.object_of(level, Prop::NavigationPointList));
        let mut seen = 0;
        while let Some(node) = next {
            if self.store.object(node).class == Some(class) {
                self.store.set(node, Prop::bTransientEndPoint, Value::Bool(true));
                goal = Some(node);
            }
            seen += 1;
            next = self.store.object_of(node, Prop::nextNavigationPoint).filter(|_| seen < MAX_NAVIGATION_POINTS);
        }
        let Some(goal) = goal else { return Ok(None) };
        self.find_path(controller, Some(goal), Vector::ZERO)
    }

    /// `two_wall_adjust` is approximated, wall hits are not modelled, and gravity is taken as -Z.
    /// Retail also clears an actor flag, not modelled (unverified). Returns whether the first move moved.
    pub(crate) fn move_smooth(&mut self, actor: ObjectId, delta: Vector) -> Result<bool, Error> {
        let step = |vm: &mut Self, by: Vector| -> Result<MoveHit, Error> { vm.move_actor(actor, by, Bases::Block) };
        let hit = step(self, delta)?;
        if hit.time < 1.0 {
            let up = -delta.safe_normal()[2];
            if hit.normal[2].abs() >= SLIDE_NORMAL_Z || up >= SLIDE_UP || up <= SLIDE_DOWN {
                let slide = (delta - (hit.normal * delta.dot(hit.normal))) * (1.0 - hit.time);
                if delta.dot(slide) >= 0.0 {
                    let second = step(self, slide)?;
                    if second.time < 1.0 {
                        let crease = hit.normal.cross(second.normal).safe_normal();
                        let rest = slide * (1.0 - second.time);
                        step(self, crease * rest.dot(crease))?;
                    }
                }
            }
        }
        Ok(hit.time > 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotator_vector_axes() {
        let forward = Rotator::ZERO.vector();
        assert!((forward.x - 1.0).abs() < 1e-5);
        let left = Rotator::new(0, 16384, 0).vector();
        assert!((left.y - 1.0).abs() < 1e-5);
        let up = Rotator::new(16384, 0, 0).vector();
        assert!((up.z - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cross_of_axes() {
        assert_eq!(Vector::new(1.0, 0.0, 0.0).cross(Vector::new(0.0, 1.0, 0.0)), Vector::new(0.0, 0.0, 1.0));
    }
}
