use p2_collision::{Primitive, mesh_box_check, pull_back_units, segment_cylinder};

use p2_math::{Rotator, Vector};
use crate::enums::EPhysics;
use crate::volumes::{OldZone, ZoneEvents};
use crate::interp::{float, int};
use crate::mover::turned;
use crate::{Error, LoaderRef, ObjectId, Prop, PropertyName, Value, Vm, Kind};

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

    pub(crate) fn event_bool(&mut self, object: ObjectId, name: &str, args: impl IntoIterator<Item = Value>) -> Result<bool, Error> {
        if let Some(bit) = crate::states::probe_bit(name)
            && !self.is_probing(object, bit)?
        {
            return Ok(false);
        }
        let Some(function) = self.find_object_function(object, name)? else { return Ok(false) };
        let Some(class) = self.store.object(object).class else { return Ok(false) };
        Ok(self.call(&function, Some(object), class, args)?.as_ref().is_some_and(crate::interp::truthy))
    }

    /// Each primitive pulls its hit back as retail: BSP 0.1 of the sweep, 0.1 to 4 units; static meshes
    /// 0.1 to 1 unit; terrain 0.5 units; cylinders 0.001 of the sweep.
    pub(crate) fn sweep(&mut self, mover: ObjectId, sweep: Sweep, against: SweepAgainst, mut touches: Option<&mut Vec<MoveHit>>) -> MoveHit {
        let Sweep { start, delta, radius, height } = sweep;
        let SweepAgainst { world, actors, ignore_bases } = against;
        let length = delta.size();
        let mut best = MoveHit::NONE;
        if length < 1e-4 {
            return best;
        }
        let end = start + delta;
        let extent = Vector::new(radius, radius, height);
        let level = self.level;
        let phase = self.profile_start();
        let bsp = if world { self.trace_bsp(start, end, extent) } else { None };
        self.profile_phase("sweep: bsp", phase);
        let phase = self.profile_start();
        let boxes = if world { self.trace_boxes(start, end, extent) } else { None };
        self.profile_phase("sweep: meshes and terrain", phase);
        let phase = self.profile_start();
        let mut consider = |time: f32, normal: Vector, actor: Option<ObjectId>| {
            if time < best.time {
                best = MoveHit { time, normal, actor };
            }
        };
        if let Some(hit) = bsp {
            consider(hit.time, hit.normal, level);
        }
        if let Some(hit) = boxes {
            consider(hit.time, hit.normal, level);
        }
        if actors && self.store.flag_of(mover, Prop::bCollideActors) {
            let (low, high) = ([0, 1, 2].map(|i| start[i].min(end[i])), [0, 1, 2].map(|i| start[i].max(end[i])));
            let nearby = self.colliders_near([low[0], low[1]], [high[0], high[1]], radius);
            self.count(crate::profiling::Work::Sweep);
            self.count_n(crate::profiling::Work::SweepCandidate, nearby.len());
            let blocking = self.blocking(mover);
            for other in nearby.iter().filter(|e| e.blocks).map(|e| e.actor) {
                self.count(crate::profiling::Work::SweepBlockingCandidate);
                if other == mover {
                    continue;
                }
                let shape = self.actor_shape(other);
                let (min, max) = if let Some(shape) = &shape {
                    let (min, max) = shape.boxes.bounds();
                    let grow = Vector::new(radius + 1.0, radius + 1.0, height + 1.0);
                    (min - grow, max + grow)
                } else {
                    let centre = self.store.vector(other, Prop::Location);
                    let r = radius + self.store.float_of(other, Prop::CollisionRadius) + 1.0;
                    let h = height + self.store.float_of(other, Prop::CollisionHeight) + 1.0;
                    let grow = Vector::new(r, r, h);
                    (centre - grow, centre + grow)
                };
                if (0..3).any(|i| max[i] < low[i] || min[i] > high[i]) {
                    self.count(crate::profiling::Work::SweepBoundsReject);
                    continue;
                }
                if self.in_owner_chain(mover, other)
                    || self.store.is_deleted(other)
                    || !self.store.flag_of(other, Prop::bCollideActors)
                    || !self.store.flag_of(other, Prop::bBlockNonZeroExtentTraces)
                    || self.store.object_of(other, Prop::Base) == Some(mover)
                    || self.store.object_of(other, Prop::Owner) == Some(mover)
                    || (ignore_bases && self.is_based_on(mover, other))
                {
                    continue;
                }
                let blocks = blocking.blocked_by(self.blocking(other)) && self.class_blocks(other, mover);
                if !blocks && touches.is_none() {
                    continue;
                }
                self.count(crate::profiling::Work::SweepTested);
                let hit = if let Some(shape) = shape {
                    let primitive = if shape.model { Primitive::Brush } else { Primitive::StaticMesh };
                    let back = pull_back_units(primitive, length) / length;
                    mesh_box_check([&*shape.boxes], start, end, extent).map(|hit| ((hit.time - back).clamp(0.0, 1.0), hit.normal))
                } else {
                    let centre = self.store.vector(other, Prop::Location);
                    let (r, h) = (radius + self.store.float_of(other, Prop::CollisionRadius), height + self.store.float_of(other, Prop::CollisionHeight));
                    let inside = (start[0] - centre[0]).hypot(start[1] - centre[1]) < r && (start[2] - centre[2]).abs() < h;
                    if inside { None } else { segment_cylinder(start, end, centre, r, h).map(|hit| ((hit.time - CYLINDER_PULL_BACK).max(0.0), hit.normal)) }
                };
                let Some((time, normal)) = hit else { continue };
                if blocks {
                    consider(time, normal, Some(other));
                } else if let Some(touches) = touches.as_deref_mut() {
                    touches.push(MoveHit { time, normal, actor: Some(other) });
                }
            }
            self.give_entries(nearby);
        }
        self.profile_phase("sweep: actors", phase);
        best
    }

    pub fn move_check(&mut self, actor: ObjectId, start: Vector, delta: Vector) -> Option<(f32, Vector, Option<ObjectId>)> {
        let (radius, height) = (self.store.float_of(actor, Prop::CollisionRadius), self.store.float_of(actor, Prop::CollisionHeight));
        let hit = self.sweep(actor, Sweep { start, delta, radius, height }, SweepAgainst::WORLD_AND_ACTORS, None);
        (hit.time < 1.0).then_some((hit.time, hit.normal, hit.actor))
    }

    /// A blocked move stops `MOVE_TEST_ADJUST` short of the hit, so a moved actor keeps a gap.
    /// Of a bump, the hit actor is notified first (unverified).
    pub(crate) fn move_actor(&mut self, actor: ObjectId, delta: Vector, bases: Bases) -> Result<MoveHit, Error> {
        let start = self.store.vector(actor, Prop::Location);
        if nearly_zero(delta) {
            self.set_vector(actor, Prop::Location, start + delta);
            self.relocate(actor);
            self.volume_moved(actor);
            return Ok(MoveHit::NONE);
        }
        let (radius, height) = (self.store.float_of(actor, Prop::CollisionRadius), self.store.float_of(actor, Prop::CollisionHeight));
        let length = delta.size();
        let direction = delta * (1.0 / length);
        let test = delta + (direction * MOVE_TEST_ADJUST);
        let collides = self.store.flag_of(actor, Prop::bCollideActors);
        let world = self.store.flag_of(actor, Prop::bCollideWorld);
        let mut touches = Vec::new();
        let block = self.sweep(
            actor,
            Sweep { start, delta: test, radius, height },
            SweepAgainst { world, actors: true, ignore_bases: bases == Bases::Ignore },
            collides.then_some(&mut touches),
        );
        let mut hit = block;
        let mut moved = delta;
        if hit.time < 1.0 {
            let reach = (length + MOVE_TEST_ADJUST) * hit.time;
            if reach <= MOVE_TEST_ADJUST {
                moved = Vector::ZERO;
                hit.time = 0.0;
            } else {
                moved = (test * hit.time) - (direction * MOVE_TEST_ADJUST);
                hit.time = (reach - MOVE_TEST_ADJUST) / length;
            }
        }
        self.set_vector(actor, Prop::Location, start + moved);
        self.relocate(actor);
        let rotation = self.store.rotator_of(actor, Prop::Rotation);
        if !self.carry_attached(actor, start, moved, rotation, rotation)? {
            return Ok(MoveHit { time: 0.0, ..hit });
        }
        self.update_relative_location(actor);
        if let Some(other) = hit.actor
            && Some(other) != self.level
            && !self.store.flag_of(other, Prop::bWorldGeometry)
            && !self.is_based_on(actor, other)
        {
            self.notify_bump(actor, other)?;
            self.notify_bump(other, actor)?;
        }
        touches.sort_by(|a, b| a.time.total_cmp(&b.time));
        for touch in touches.into_iter().filter(|t| t.time <= block.time) {
            if self.store.is_deleted(actor) {
                break;
            }
            if let Some(other) = touch.actor {
                self.begin_touch(actor, other)?;
            }
        }
        if collides && !self.store.is_deleted(actor) {
            self.end_touches(actor)?;
        }
        self.set_zone(actor, ZoneEvents::Sent, OldZone::Region)?;
        Ok(hit)
    }

    /// Retail reads neither `AttachmentBone` nor `bHardAttach` here, nor the base's `bCollideActors`.
    /// A refusal moves actors carried before it back by `delta`, not turned back; it stays where it went.
    pub(crate) fn carry_attached(&mut self, base: ObjectId, old_location: Vector, delta: Vector, old_rotation: Rotator, rotation: Rotator) -> Result<bool, Error> {
        let attached = self.store.object_list(base, Prop::Attached);
        let turn = attached_turn(old_rotation, rotation);
        for (i, &other) in attached.iter().enumerate() {
            if self.store.is_deleted(other) {
                continue;
            }
            if rotation != old_rotation
                && let Some(controller) = self.store.object_of(other, Prop::Controller).filter(|_| self.store.is_a(other, Kind::Pawn))
            {
                let view = self.store.rotator_of(controller, Prop::Rotation);
                let mut swung = [0, 1, 2].map(|c| view[c].wrapping_add(turn[c]));
                if !self.store.flag_of(other, Prop::bCrawler) {
                    swung[2] = view.roll;
                }
                self.store.set(controller, Prop::Rotation, Value::Rotator(swung.into()));
            }
            let motion = carried_motion(delta, old_rotation, rotation, self.store.vector(other, Prop::RelativeLocation));
            let own = self.store.rotator_of(other, Prop::Rotation);
            self.store.set(other, Prop::Rotation, Value::Rotator([0, 1, 2].map(|c| own[c].wrapping_add(turn[c])).into()));
            self.move_actor(other, motion, Bases::Ignore)?;
            if self.is_blocked_by(other, base)
                && self.primitive_overlaps(base, other)
                && self.event_bool(base, "EncroachingOn", [Value::Object(Some(other))])?
            {
                self.set_vector(base, Prop::Location, old_location);
                self.store.set(base, Prop::Rotation, Value::Rotator(old_rotation));
                self.relocate(base);
                for &earlier in &attached[..i] {
                    self.move_actor(earlier, delta * (-1.0), Bases::Ignore)?;
                }
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn update_relative_location(&mut self, actor: ObjectId) {
        if let Some(base) = self.store.object_of(actor, Prop::Base)
            && !self.on_bone(actor)
        {
            let offset = self.store.vector(actor, Prop::Location) - self.store.vector(base, Prop::Location);
            self.set_vector(actor, Prop::RelativeLocation, offset);
        }
    }

    /// Retail's `SetRotation` is a zero-delta move; with nothing attached it only sets the rotation.
    /// An encroacher's encroachment check on this path is not modelled.
    pub(crate) fn turn_attached(&mut self, actor: ObjectId, old_rotation: Rotator) -> Result<(), Error> {
        if self.store.object_list(actor, Prop::Attached).is_empty() {
            return Ok(());
        }
        let (location, rotation) = (self.store.vector(actor, Prop::Location), self.store.rotator_of(actor, Prop::Rotation));
        if !self.carry_attached(actor, location, Vector::ZERO, old_rotation, rotation)? {
            return Ok(());
        }
        if self.store.flag_of(actor, Prop::bCollideActors) && !self.store.is_deleted(actor) {
            self.end_touches(actor)?;
        }
        self.set_zone(actor, ZoneEvents::Sent, OldZone::Region)?;
        Ok(())
    }

    pub(crate) fn is_based_on(&self, actor: ObjectId, other: ObjectId) -> bool {
        std::iter::successors(self.store.object_of(actor, Prop::Base), |&b| self.store.object_of(b, Prop::Base)).take(MAX_BASE_CHAIN).any(|b| b == other)
    }

    fn in_owner_chain(&self, actor: ObjectId, other: ObjectId) -> bool {
        std::iter::successors(self.store.object_of(actor, Prop::Owner), |&o| self.store.object_of(o, Prop::Owner)).take(MAX_OWNER_CHAIN).any(|o| o == other)
    }

    fn is_human_controlled(&self, pawn: ObjectId) -> bool {
        self.store.object_of(pawn, Prop::Controller).is_some_and(|c| self.store.is_a(c, Kind::PlayerController))
    }

    /// Relative rotation is not updated. Without a base, or on world geometry, the actor leaves any bone.
    pub(crate) fn set_base(&mut self, actor: ObjectId, base: Option<ObjectId>, floor: Vector) -> Result<(), Error> {
        if self.store.is_a(actor, Kind::Pawn) {
            self.set_vector(actor, Prop::Floor, floor);
        }
        if base == Some(actor) || base.is_some_and(|b| self.is_based_on(b, actor)) {
            return Ok(());
        }
        let old = self.store.object_of(actor, Prop::Base);
        let world = |vm: &Self, b: ObjectId| Some(b) == vm.level || vm.store.flag_of(b, Prop::bWorldGeometry);
        if old != base {
            if let Some(old) = old.filter(|&o| !world(self, o)) {
                if let Some(Value::Array(attached)) = self.store.get(old, Prop::Attached) {
                    let kept = attached.into_iter().filter(|a| *a != Value::Object(Some(actor))).collect();
                    self.store.set(old, Prop::Attached, Value::Array(kept));
                }
                self.event(old, "Detach", [Value::Object(Some(actor))])?;
            }
            self.store.set(actor, Prop::Base, Value::Object(base));
            if let Some(new) = base
                && !self.on_bone(actor)
            {
                let offset = self.store.vector(actor, Prop::Location) - self.store.vector(new, Prop::Location);
                self.set_vector(actor, Prop::RelativeLocation, offset);
            }
            if let Some(new) = base.filter(|&b| !world(self, b)) {
                if let Some(Value::Array(mut attached)) = self.store.get(new, Prop::Attached) {
                    attached.push(Value::Object(Some(actor)));
                    self.store.set(new, Prop::Attached, Value::Array(attached));
                }
                self.event(new, "Attach", [Value::Object(Some(actor))])?;
            }
            self.event(actor, "BaseChange", Vec::new())?;
        }
        let base = self.store.object_of(actor, Prop::Base);
        if base.is_none_or(|b| world(self, b)) && self.on_bone(actor) {
            self.store.set(actor, Prop::AttachmentBone, Value::Name(crate::NameId::NONE));
        }
        Ok(())
    }

    pub(crate) fn on_bone(&self, actor: ObjectId) -> bool {
        matches!(self.store.get(actor, Prop::AttachmentBone), Some(Value::Name(bone)) if bone != crate::NameId::NONE)
    }

    /// Also for actors on a bone or `bHardAttach`: one put on a bone and then set to a base-keeping mode
    /// stays on only if the base is right below it.
    pub(crate) fn set_physics(&mut self, actor: ObjectId, physics: EPhysics, floor: Option<(Option<ObjectId>, Vector)>) -> Result<(), Error> {
        self.set_physics_byte(actor, physics as u8, floor)
    }

    pub(crate) fn set_physics_byte(&mut self, actor: ObjectId, physics: u8, floor: Option<(Option<ObjectId>, Vector)>) -> Result<(), Error> {
        let old = self.store.byte_of(actor, Prop::Physics);
        if old == physics {
            return Ok(());
        }
        self.store.set(actor, Prop::Physics, Value::Byte(physics));
        self.karma_physics_changed(actor, EPhysics::from_byte(old));
        let mode = EPhysics::from_byte(physics);
        if mode.is_some_and(keeps_base) {
            match floor {
                Some((Some(base), normal)) => {
                    if self.store.object_of(actor, Prop::Base) != Some(base) {
                        self.set_base(actor, Some(base), normal)?;
                    }
                }
                _ => self.find_base(actor)?,
            }
        } else if self.store.object_of(actor, Prop::Base).is_some() {
            self.set_base(actor, None, [0.0, 0.0, 1.0].into())?;
        }
        if mode.is_some_and(stops) {
            self.set_vector(actor, Prop::Velocity, Vector::ZERO);
            self.set_vector(actor, Prop::Acceleration, Vector::ZERO);
        }
        if let Some(volume) = self.store.object_of(actor, Prop::PhysicsVolume).filter(|&v| self.store.object(v).class.is_some()) {
            self.event(volume, "PhysicsChangedFor", [Value::Object(Some(actor))])?;
        }
        Ok(())
    }

    fn find_base(&mut self, actor: ObjectId) -> Result<(), Error> {
        let at = self.store.vector(actor, Prop::Location);
        let (radius, height) = (self.store.float_of(actor, Prop::CollisionRadius), self.store.float_of(actor, Prop::CollisionHeight));
        let hit = self.sweep(actor, Sweep { start: at, delta: [0.0, 0.0, -FIND_BASE_DEPTH].into(), radius, height }, SweepAgainst::WORLD_AND_ACTORS, None);
        let base = hit.actor;
        if self.store.object_of(actor, Prop::Base) != base {
            self.set_base(actor, base, hit.normal)?;
        }
        Ok(())
    }

    fn notify_bump(&mut self, actor: ObjectId, other: ObjectId) -> Result<(), Error> {
        if self.store.is_a(actor, Kind::Pawn)
            && let Some(controller) = self.store.object_of(actor, Prop::Controller)
            && self.event_bool(controller, "NotifyBump", [Value::Object(Some(other))])?
        {
            return Ok(());
        }
        self.event(actor, "Bump", [Value::Object(Some(other))])
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
            self.event(pawn, "FellOutOfWorld", Vec::new())?;
            if self.store.is_deleted(pawn) {
                return Ok(());
            }
        }
        if self.store.flag_of(pawn, Prop::bCollideWorld) && self.out_of_world(pawn) {
            self.event(pawn, "FellOutOfWorld", Vec::new())?;
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
        let (radius, height) = (self.store.float_of(pawn, Prop::CollisionRadius), self.store.float_of(pawn, Prop::CollisionHeight));
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
        self.event(pawn, "StartCrouch", [Value::Float(adjust)])
    }

    fn uncrouch(&mut self, pawn: ObjectId) -> Result<(), Error> {
        let Some(class) = self.store.object(pawn).class else { return Ok(()) };
        let default = |vm: &Self, name: Prop| vm.store.default_value(class, name).as_ref().map_or(0.0, float);
        let (radius, height) = (default(self, Prop::CollisionRadius), default(self, Prop::CollisionHeight));
        let (crouch_radius, crouch_height) = (self.store.float_of(pawn, Prop::CollisionRadius), self.store.float_of(pawn, Prop::CollisionHeight));
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
        self.event(pawn, "EndCrouch", [Value::Float(adjust)])
    }

    fn start_new_physics(&mut self, pawn: ObjectId, delta: f32, iterations: u32) -> Result<(), Error> {
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

    fn velocity_caps(&self, pawn: ObjectId, max_speed: f32) -> (f32, f32) {
        let walking = self.store.flag_of(pawn, Prop::bIsWalking) || self.store.flag_of(pawn, Prop::bIsCrouched) || self.store.flag_of(pawn, Prop::bIsDeathCrawling);
        let walking_pct = self.store.float_of(pawn, Prop::WalkingPct);
        let accel_rate = self.store.float_of(pawn, Prop::AccelRate);
        let max_speed = if self.is_human_controlled(pawn) { max_speed } else { max_speed * self.store.float_of(pawn, Prop::DesiredSpeed) };
        if walking { (walking_pct * accel_rate, walking_pct * max_speed) } else { (accel_rate, max_speed) }
    }

    fn calc_velocity(&mut self, pawn: ObjectId, accel_dir: Vector, delta: f32, max_speed: f32, friction: f32) {
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

    /// Walking AI does not step off ledges: the move is dropped (unverified).
    /// A move that leaves the pawn swimming ends in `start_swimming`, with the frame's start and velocity.
    fn phys_walking(&mut self, pawn: ObjectId, delta: f32, mut iterations: u32) -> Result<(), Error> {
        let Some(controller) = self.store.object_of(pawn, Prop::Controller) else { return Ok(()) };
        let VolumeProps { gravity, ground_friction, .. } = self.volume(pawn);
        let friction = if ground_friction == DEFAULT_GROUND_FRICTION { self.store.float_of(pawn, Prop::WalkFriction) } else { ground_friction };
        let desired_move = self.walking_velocity(pawn, delta, friction);
        let grav_dir = gravity_direction(gravity);
        let down = grav_dir * FLOOR_CHECK;
        let (radius, height) = (self.store.float_of(pawn, Prop::CollisionRadius), self.store.float_of(pawn, Prop::CollisionHeight));
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
                        self.event(pawn, "Falling", Vec::new())?;
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
            self.event(controller, "MayFall", Vec::new())?;
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
        self.event(pawn, "Falling", Vec::new())?;
        if self.store.physics_of(pawn) == Some(EPhysics::Walking) {
            self.set_physics(pawn, EPhysics::Falling, None)?;
        }
        self.start_new_physics(pawn, left, iterations)
    }

    fn step_up(&mut self, pawn: ObjectId, grav_dir: Vector, desired_dir: Vector, step: Vector, mut hit: MoveHit, depth: u32) -> Result<(), Error> {
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

    fn process_hit_wall(&mut self, pawn: ObjectId, hit_normal: Vector, wall: Option<ObjectId>) -> Result<(), Error> {
        let Some(wall) = wall.filter(|&w| !self.store.is_a(w, Kind::Pawn)) else { return Ok(()) };
        let args = [Value::Vector(hit_normal), Value::Object(Some(wall))];
        let Some(controller) = self.store.object_of(pawn, Prop::Controller) else { return self.event(pawn, "HitWall", args) };
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
            && !self.event_bool(controller, "NotifyHitWall", args.clone())?
        {
            self.event(pawn, "HitWall", args)?;
        }
        Ok(())
    }

    fn phys_falling(&mut self, pawn: ObjectId, delta: f32, mut iterations: u32) -> Result<(), Error> {
        let VolumeProps { gravity, terminal_velocity: terminal, zone_velocity, .. } = self.volume(pawn);
        let mut real_acceleration = self.store.vector(pawn, Prop::Acceleration);
        let accel_rate = self.store.float_of(pawn, Prop::AccelRate);
        let (mut acceleration, bound_speed) = self.air_acceleration(pawn, real_acceleration, accel_rate, delta);
        let constant = self.store.vector(pawn, Prop::ConstantAcceleration);
        let ai = self.store.object_of(pawn, Prop::Controller).is_some_and(|c| self.store.is_a(c, Kind::AIController));
        let mut remaining = delta;
        while remaining > 0.0 && iterations < MAX_ITERATIONS {
            iterations += 1;
            let tick = substep(remaining);
            remaining -= tick;
            let old_location = self.store.vector(pawn, Prop::Location);
            self.store.set(pawn, Prop::bJustTeleported, Value::Bool(false));
            let mut old_velocity = self.store.vector(pawn, Prop::Velocity);
            let total = (acceleration + gravity) + constant;
            let mut velocity = old_velocity + (total * (0.5 * tick));
            if bound_speed != 0.0 && horizontal_size(velocity) > bound_speed {
                let flat = Vector::new(velocity[0], velocity[1], 0.0).safe_normal() * bound_speed;
                velocity = [flat[0], flat[1], velocity[2]].into();
            }
            self.set_vector(pawn, Prop::Velocity, velocity);
            let adjusted = (velocity + zone_velocity) * tick;
            let hit = self.move_actor(pawn, adjusted, Bases::Block)?;
            if self.store.is_deleted(pawn) {
                return Ok(());
            }
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
            let (radius, height) = (self.store.float_of(pawn, Prop::CollisionRadius), self.store.float_of(pawn, Prop::CollisionHeight));
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
            Some(controller) => self.event_bool(controller, "NotifyLanded", args.clone())?,
            None => false,
        };
        if !handled {
            self.event(pawn, "Landed", args)?;
        }
        self.start_new_physics(pawn, left, iterations)
    }

    /// Banking from velocity changes is not modelled. Human pawns use the pawn's rate, not retail's.
    fn pawn_rotation(&mut self, pawn: ObjectId, delta: f32) {
        let controller = self.store.object_of(pawn, Prop::Controller);
        let human = controller.is_some_and(|c| self.store.is_a(c, Kind::PlayerController));
        let rotate = match controller {
            Some(c) if human => self.store.flag_of(c, Prop::bRotateToDesired),
            other => other.is_some(),
        };
        self.store.set(pawn, Prop::bRotateToDesired, Value::Bool(rotate));
        if let Some(c) = controller.filter(|_| human && rotate) {
            let own = self.store.rotator_of(c, Prop::DesiredRotation);
            let mut desired = self.store.rotator_of(pawn, Prop::DesiredRotation);
            desired.pitch = own.pitch;
            desired.yaw = own.yaw;
            self.store.set(pawn, Prop::DesiredRotation, Value::Rotator(desired));
        }
        let physics = self.store.physics_of(pawn);
        let fixed = self.store.flag_of(pawn, Prop::bFixedRotationDir);
        let rate = self.store.rotator_of(pawn, Prop::RotationRate);
        let turn = (rate.yaw as f32 * delta).round() as i32;
        let mut rotation = self.store.rotator_of(pawn, Prop::Rotation);
        let mut desired = self.store.rotator_of(pawn, Prop::DesiredRotation);
        if let Some(controller) = controller.filter(|_| rotate) {
            if desired.yaw != rotation.yaw {
                rotation.yaw = fixed_turn(rotation.yaw, desired.yaw, turn, Toward::Desired, fixed);
            }
            if matches!(physics, Some(EPhysics::Walking | EPhysics::Falling | EPhysics::Ladder)) && desired.pitch != 0 {
                desired.pitch = 0;
                self.store.set(pawn, Prop::DesiredRotation, Value::Rotator(desired));
            }
            if !(self.store.flag_of(pawn, Prop::bCrawler) && physics == Some(EPhysics::Walking)) && desired.pitch != rotation.pitch {
                rotation.pitch = fixed_turn(rotation.pitch, desired.pitch, turn, Toward::Desired, fixed);
            }
            if self.store.flag_of(controller, Prop::bRotateToDesired) {
                let mut own = self.store.rotator_of(controller, Prop::Rotation);
                own.yaw = rotation.yaw;
                let own_desired = self.store.rotator_of(controller, Prop::DesiredRotation);
                own.pitch = fixed_turn(own.pitch, own_desired.pitch, turn, Toward::Desired, self.store.flag_of(controller, Prop::bFixedRotationDir));
                self.store.set(controller, Prop::Rotation, Value::Rotator(own));
            }
        }
        if rate.roll < 1 {
            rotation.roll = 0;
        } else {
            let ease = (delta * 8.0).min(1.0);
            let roll = rotation.roll & 0xffff;
            rotation.roll = if roll < 0x8000 { ((1.0 - ease) * roll as f32) as i32 } else { (roll as f32 + (0x10000 - roll) as f32 * ease) as i32 };
        }
        if rotation != self.store.rotator_of(pawn, Prop::Rotation) {
            self.store.set(pawn, Prop::Rotation, Value::Rotator(rotation));
        }
    }

    pub(crate) fn actor_rotation(&mut self, actor: ObjectId, delta: f32) -> Result<(), Error> {
        let rate = self.store.rotator_of(actor, Prop::RotationRate);
        if rate == [0; 3].into() || self.store.flag_of(actor, Prop::bInterpolating) {
            return Ok(());
        }
        let to_desired = self.store.flag_of(actor, Prop::bRotateToDesired);
        let fixed = self.store.flag_of(actor, Prop::bFixedRotationDir);
        let desired = self.store.rotator_of(actor, Prop::DesiredRotation);
        let current = self.store.rotator_of(actor, Prop::Rotation);
        if (!to_desired && !fixed) || (to_desired && current == desired) {
            return Ok(());
        }
        let turn = rate.map(|r| (r as f32 * delta) as i32);
        let mut rotation = current;
        for axis in [1, 0, 2] {
            if turn[axis] != 0 && (!to_desired || desired[axis] != rotation[axis]) {
                rotation[axis] = fixed_turn(rotation[axis], desired[axis], turn[axis], if to_desired { Toward::Desired } else { Toward::Spin }, fixed);
            }
        }
        if rotation != current {
            self.store.set(actor, Prop::Rotation, Value::Rotator(rotation));
            self.volume_moved(actor);
        }
        if to_desired && rotation == desired {
            self.event(actor, "EndedRotation", Vec::new())?;
        }
        Ok(())
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
