use p2_math::{Rotator, Vector};
use crate::enums::EPhysics;
use crate::mover::turned;
use crate::{Error, LoaderRef, ObjectId, PropertyName, Value, Vm};

pub(crate) const MAX_STEP_HEIGHT: f32 = 35.0;
pub(crate) const MIN_FLOOR_Z: f32 = 0.7;
/// The floor distance (`physWalking`'s floor check, after its primitive's pull-back) a walking pawn keeps:
/// moved down when above the band, up to its middle (2.15) when below.
const MIN_FLOOR_DIST: f32 = 1.9;
const MAX_FLOOR_DIST: f32 = 2.4;
const FLOOR_CHECK: f32 = MAX_STEP_HEIGHT + 2.0;
/// A move sweeps this many units past its end and stops this much short of a hit.
pub(crate) const MOVE_TEST_ADJUST: f32 = 2.0;
const MAX_STEP_SIDE_Z: f32 = 0.08;
const MAX_SUBSTEP: f32 = 0.05;
pub(crate) const MAX_ITERATIONS: u32 = 8;
const MAX_BASE_CHAIN: usize = 64;
const MAX_OWNER_CHAIN: usize = 16;
const BRAKE_STEP: f32 = 0.03;
pub(crate) const STEP_AGAIN_SQUARED: f32 = 144.0;
const SLOPE_SLIDE_FRICTION: f32 = 3.3;
const MIN_SLOPE_FRICTION: f32 = 0.5;
const MIN_AIR_CONTROL: f32 = 0.05;
const AIR_BURST_SPEED: f32 = 10.0;
pub(crate) const MIN_LANDING_TIME: f32 = 0.1;
pub(crate) const MIN_LANDING_SECONDS: f32 = 0.003;
const FIND_BASE_DEPTH: f32 = 8.0;
/// A `PhysicsVolume.GroundFriction` of this value leaves the pawn's own `WalkFriction` in charge.
const DEFAULT_GROUND_FRICTION: f32 = 8.0;
pub(crate) const DEFAULT_GRAVITY: Vector = Vector::new(0.0, 0.0, -1500.0);
const DEFAULT_TERMINAL_VELOCITY: f32 = 2500.0;
/// How far below a `bSoftKillZ` zone's `KillZ` an actor that is not walking may fall.
/// The script comment's "2000" is not what this build uses.
const SOFT_KILL_Z_GRACE: f32 = 2500.0;

fn keeps_base(physics: EPhysics) -> bool {
    matches!(physics, EPhysics::None | EPhysics::Walking | EPhysics::Rotating | EPhysics::RootMotion | EPhysics::Spider)
}

fn stops(physics: EPhysics) -> bool {
    matches!(physics, EPhysics::None | EPhysics::Rotating)
}

fn horizontal_size(a: Vector) -> f32 {
    a[0].hypot(a[1])
}

pub(crate) fn nearly_zero(a: Vector) -> bool {
    <[f32; 3]>::from(a).iter().all(|c| c.abs() < 1e-4)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MoveHit {
    pub time: f32,
    pub normal: Vector,
    pub actor: Option<ObjectId>,
}

/// A floor [`Vm::find_floor`] found: the cylinder bottom's distance above it (negative when the bottom sank into
/// terrain or a mesh, which only line traces see) and its normal.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Floor {
    pub distance: f32,
    pub normal: Vector,
}

#[derive(Clone, Copy)]
pub(crate) struct VolumeProps {
    pub gravity: Vector,
    pub ground_friction: f32,
    pub terminal_velocity: f32,
    pub zone_velocity: Vector,
}

pub(crate) fn gravity_direction(gravity: Vector) -> Vector {
    if gravity[2] > 0.0 { Vector::new(0.0, 0.0, 1.0) } else { Vector::new(0.0, 0.0, -1.0) }
}

pub(crate) fn substep(remaining: f32) -> f32 {
    if remaining > MAX_SUBSTEP { MAX_SUBSTEP.min(remaining * 0.5) } else { remaining }
}

fn turned_velocity(velocity: Vector, accel_dir: Vector, delta: f32, friction: f32) -> Vector {
    velocity - ((velocity - (accel_dir * velocity.size())) * (delta * friction))
}

impl MoveHit {
    pub(crate) const NONE: Self = Self { time: 1.0, normal: Vector::ZERO, actor: None };
}

pub(crate) const CYLINDER_PULL_BACK: f32 = 0.001;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Toward {
    Desired,
    Spin,
}

fn fixed_turn(current: i32, desired: i32, rate: i32, toward: Toward, fixed_direction: bool) -> i32 {
    if rate == 0 {
        return current & 0xffff;
    }
    let (current, desired) = (current & 0xffff, desired & 0xffff);
    if fixed_direction {
        if toward == Toward::Spin {
            return (current + rate) & 0xffff;
        }
        let step = if rate > 0 {
            let target = if desired < current { desired + 0x10000 } else { desired };
            (target - current).min(rate)
        } else {
            let from = if current < desired { current + 0x10000 } else { current };
            (desired - from).max(rate)
        };
        return (current + step) & 0xffff;
    }
    if toward == Toward::Spin {
        return current;
    }
    let rate = rate.abs();
    let turn = if desired < current {
        let difference = current - desired;
        if difference < 0x8000 { -difference.min(rate) } else { (desired - current + 0x10000).min(rate) }
    } else {
        let difference = desired - current;
        if difference < 0x8000 { difference.min(rate) } else { -(current - desired + 0x10000).min(rate) }
    };
    (current + turn) & 0xffff
}

#[derive(Clone, Copy)]
struct WalkStart {
    location: Vector,
    floor: Vector,
    base: Option<ObjectId>,
}

#[derive(Clone, Copy)]
pub(crate) struct Sweep {
    pub(crate) start: Vector,
    pub(crate) delta: Vector,
    pub(crate) radius: f32,
    pub(crate) height: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct SweepAgainst {
    pub(crate) world: bool,
    pub(crate) actors: bool,
    pub(crate) ignore_bases: bool,
}

impl SweepAgainst {
    pub(crate) const WORLD: Self = Self { world: true, actors: false, ignore_bases: false };
    pub(crate) const WORLD_AND_ACTORS: Self = Self { world: true, actors: true, ignore_bases: false };
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bases {
    Block,
    Ignore,
}

impl<L: LoaderRef> Vm<L> {

    #[inline]
    pub(crate) fn set_vector(&mut self, object: ObjectId, property: impl PropertyName, value: Vector) {
        self.store.set_vector(object, property, value);
    }

    pub(crate) fn event_bool(&mut self, object: ObjectId, name: impl crate::states::EventKey, args: impl IntoIterator<Item = Value>) -> Result<bool, Error> {
        let (name, probe) = name.resolve(self);
        if let Some(bit) = probe
            && !self.is_probing(object, bit)?
        {
            return Ok(false);
        }
        let Some(function) = self.find_object_function_id(object, name)? else { return Ok(false) };
        let Some(class) = self.store.object(object).class else { return Ok(false) };
        Ok(self.call(&function, Some(object), class, args)?.as_ref().is_some_and(crate::interp::truthy))
    }
}

pub(crate) fn two_wall_adjust(desired_dir: Vector, delta: Vector, hit_normal: Vector, old_hit_normal: Vector, time: f32) -> Vector {
    if old_hit_normal.dot(hit_normal) <= 0.0 {
        let crease = hit_normal.cross(old_hit_normal).safe_normal();
        let along = crease * (delta.dot(crease) * (1.0 - time));
        if desired_dir.dot(along) < 0.0 { along * (-1.0) } else { along }
    } else {
        let along = (delta - (hit_normal * delta.dot(hit_normal))) * (1.0 - time);
        if along.dot(desired_dir) <= 0.0 { Vector::ZERO } else { along }
    }
}

fn attached_turn(from: Rotator, to: Rotator) -> [i32; 3] {
    [0, 1, 2].map(|i| (to[i] & 0xfffc).wrapping_sub(from[i] & 0xfffc))
}

fn carried_motion(delta: Vector, from: Rotator, to: Rotator, relative: Vector) -> Vector {
    if from == to { delta } else { delta + (turned(from, to, relative) - relative) }
}

mod fly_swim;
mod ladder_spider;
pub(crate) mod root_motion;
mod base;
mod falling;
mod pawn;
mod rotation;
mod sweep;
mod walking;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attached_actors_follow_the_base() {
        assert_eq!(attached_turn([0, 3, 0].into(), [0, 16387, 0].into()), [0, 16384, 0]);
        assert_eq!(attached_turn([0, 0, 0].into(), [0, -4, 0].into()), [0, 0xfffc, 0]);
        let kept = carried_motion([10.0, 0.0, 0.0].into(), [0, 500, 0].into(), [0, 500, 0].into(), [100.0, 0.0, 0.0].into());
        assert!(kept[0] == 10.0 && kept[1] == 0.0 && kept[2] == 0.0, "{kept:?}");
        let swung = carried_motion([10.0, 0.0, 0.0].into(), [0, 0, 0].into(), [0, 16384, 0].into(), [100.0, 0.0, 0.0].into());
        assert!((swung[0] + 90.0).abs() < 1e-3 && (swung[1] - 100.0).abs() < 1e-3 && swung[2].abs() < 1e-3, "{swung:?}");
    }

    #[test]
    fn set_physics_mode_table() {
        let based: Vec<EPhysics> = (0..=14).filter_map(EPhysics::from_byte).filter(|&p| keeps_base(p)).collect();
        assert_eq!(based, [EPhysics::None, EPhysics::Walking, EPhysics::Rotating, EPhysics::Spider, EPhysics::RootMotion]);
        let stopped: Vec<EPhysics> = (0..=14).filter_map(EPhysics::from_byte).filter(|&p| stops(p)).collect();
        assert_eq!(stopped, [EPhysics::None, EPhysics::Rotating]);
    }

    #[test]
    fn fixed_turn_takes_the_short_way() {
        assert_eq!(fixed_turn(0, 1000, 300, Toward::Desired, false), 300);
        assert_eq!(fixed_turn(0, 65000, 300, Toward::Desired, false), 65236);
        assert_eq!(fixed_turn(100, 120, 300, Toward::Desired, false), 120);
        assert_eq!(fixed_turn(65500, 100, 300, Toward::Desired, false), 100);
        assert_eq!(fixed_turn(65500, 1000, 300, Toward::Desired, false), 264);
        assert_eq!(fixed_turn(10, 5000, 300, Toward::Spin, false), 10);
        assert_eq!(fixed_turn(65400, 0, 300, Toward::Spin, true), 164);
    }

    #[test]
    fn corner_slides_along_the_crease() {
        let wall = two_wall_adjust([1.0, 0.0, 0.0].into(), [10.0, 10.0, 0.0].into(), [0.0, -1.0, 0.0].into(), [-0.7, -0.7, 0.0].into(), 0.5);
        assert!((wall[0] - 5.0).abs() < 1e-4 && wall[1].abs() < 1e-4);
    }
}
