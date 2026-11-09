use std::sync::Arc;

use p2_assets::Model;

use p2_math::{Rotator, Vector, unrotate_by};
use crate::collision::Placement;
use crate::{Error, Kind, LoaderRef, ObjectId, Prop, Value, Vm};

const DEFAULT_VOLUME_PRIORITY: i32 = -1_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZoneEvents {
    Sent,
    Silent,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OldZone {
    Region,
    Level,
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn default_physics_volume(&mut self) -> Result<Option<ObjectId>, Error> {
        let Some(level) = self.level else { return Ok(None) };
        if let Some(volume) = self.store.object_of(level, Prop::PhysicsVolume) {
            return Ok(Some(volume));
        }
        let Ok(class) = self.find_class("Engine", "DefaultPhysicsVolume") else { return Ok(None) };
        let Some(volume) = self.spawn_actor(class, None, None, [None, None])? else { return Ok(None) };
        self.store.set(volume, Prop::Priority, Value::Int(DEFAULT_VOLUME_PRIORITY));
        self.store.set(level, Prop::PhysicsVolume, Value::Object(Some(volume)));
        Ok(Some(volume))
    }

    /// With `use_touch` retail looks only at touched volumes; the VM takes the colliding volumes.
    /// Flags and `Priority` are read live: a script may change them between two calls of a frame.
    pub(crate) fn physics_volume_at(&mut self, point: Vector, use_touch: bool) -> Result<Option<ObjectId>, Error> {
        let Some(default) = self.default_physics_volume()? else { return Ok(None) };
        self.refresh_volume_table();
        let default = (default, self.priority(default));
        let candidate = |vm: &Self, v: ObjectId| (!vm.store.is_deleted(v) && (!use_touch || vm.store.flag_of(v, Prop::bCollideActors))).then(|| vm.priority(v));
        let table = std::mem::take(&mut self.world.primitives.volume_table);
        let mut held = 0;
        let in_box = table
            .iter()
            .filter(|entry| {
                let holds = box_holds(entry.bounds, point);
                held += usize::from(holds);
                holds
            })
            .map(|entry| entry.volume);
        let best = highest_priority(self, default, in_box, candidate, |vm, v| vm.encompasses(v, point));
        self.count_n(crate::profiling::Work::VolumeBoxReject, table.len() - held);
        self.world.primitives.volume_table = table;
        Ok(Some(best))
    }

    fn refresh_volume_table(&mut self) {
        let frame = self.world.frame;
        if self.world.primitives.volume_table_frame == Some(frame) {
            return;
        }
        let volumes = match &self.world.primitives.physics_volumes {
            Some(list) => Arc::clone(list),
            None => {
                let mut list: Vec<ObjectId> = self.actors().filter(|&a| self.store.is_a(a, Kind::PhysicsVolume)).collect();
                list.reverse();
                let list: Arc<[ObjectId]> = list.into();
                self.world.primitives.physics_volumes = Some(Arc::clone(&list));
                list
            }
        };
        let mut table = std::mem::take(&mut self.world.primitives.volume_table);
        table.clear();
        for &volume in volumes.iter() {
            if !self.store.is_deleted(volume)
                && self.check_volume_place(volume, frame)
                && let Some(place) = self.world.primitives.volumes.get(&volume)
            {
                table.push(VolumeBox { bounds: place.world_bounds, volume });
            }
        }
        self.world.primitives.volume_table = table;
        self.world.primitives.volume_table_frame = Some(frame);
    }

    fn priority(&self, volume: ObjectId) -> i32 {
        match self.store.get(volume, Prop::Priority) {
            Some(Value::Int(priority)) => priority,
            _ => 0,
        }
    }

    pub fn encompasses(&mut self, volume: ObjectId, point: Vector) -> bool {
        let frame = self.world.frame;
        let test = |place: &VolumePlace| box_holds(place.world_bounds, point).then(|| place.encompasses(point));
        let mut outcome = self.world.primitives.volumes.get(&volume).filter(|place| place.checked == Some(frame)).map(test);
        if outcome.is_none() {
            if !self.check_volume_place(volume, frame) {
                return false;
            }
            outcome = self.world.primitives.volumes.get(&volume).map(test);
        }
        match outcome.flatten() {
            Some(inside) => {
                self.count(crate::profiling::Work::VolumeTest);
                inside
            }
            None => {
                self.count(crate::profiling::Work::VolumeBoxReject);
                false
            }
        }
    }

    fn check_volume_place(&mut self, volume: ObjectId, frame: u64) -> bool {
        let Some(brush) = self.store.object_of(volume, Prop::Brush) else { return false };
        let placement = self.placement(volume);
        if let Some(place) = self.world.primitives.volumes.get_mut(&volume)
            && place.brush == brush
            && place.placement == placement
        {
            place.checked = Some(frame);
            return true;
        }
        let Some(model) = self.brush_model(brush) else { return false };
        self.world.primitives.volumes.insert(volume, VolumePlace::new(brush, placement, model, frame));
        true
    }

    /// `actor` may have moved, turned or been rescaled: a physics volume's next test checks its place again and the next
    /// [`Vm::physics_volume_at`] takes its box anew.
    pub(crate) fn volume_moved(&mut self, actor: ObjectId) {
        if let Some(place) = self.world.primitives.volumes.get_mut(&actor) {
            place.checked = None;
            self.world.primitives.volume_table_frame = None;
        }
    }

    pub(crate) fn first_water_volume(&mut self, start: Vector, end: Vector) -> Option<(ObjectId, f32)> {
        self.refresh_volume_table();
        let mut first: Option<(ObjectId, f32)> = None;
        for entry in &self.world.primitives.volume_table {
            let volume = entry.volume;
            if self.store.is_deleted(volume) || !self.store.flag_of(volume, Prop::bWaterVolume) {
                continue;
            }
            let Some(time) = self.world.primitives.volumes.get(&volume).and_then(|place| place.line_time(start, end)) else { continue };
            if first.is_none_or(|(_, best)| time < best) {
                first = Some((volume, time));
            }
        }
        first
    }

    pub(crate) fn region_of(&self, actor: ObjectId) -> Option<(Option<ObjectId>, i32, u8)> {
        match self.members::<3>(actor, Prop::Region)? {
            [Value::Object(zone), Value::Int(leaf), Value::Byte(number)] => Some((zone, leaf, number)),
            _ => None,
        }
    }

    pub(crate) fn set_zone(&mut self, actor: ObjectId, events: ZoneEvents, zone_from: OldZone) -> Result<(), Error> {
        if self.store.is_deleted(actor) || !self.store.is_a(actor, Kind::Actor) || self.store.is_a(actor, Kind::LevelInfo) {
            return Ok(());
        }
        if self.store.is_a(actor, Kind::PhysicsVolume) {
            self.volume_moved(actor);
            self.store.set(actor, Prop::PhysicsVolume, Value::Object(Some(actor)));
            return Ok(());
        }
        let use_touch = self.store.flag_of(actor, Prop::bCollideActors) && events == ZoneEvents::Sent && zone_from == OldZone::Region;
        let location = self.store.vector(actor, Prop::Location);
        let (zone, leaf, number) = self.region_at(location);
        let region = self.region_of(actor);
        let old_zone = if zone_from == OldZone::Level { self.level() } else { region.and_then(|(zone, _, _)| zone) };
        let zone_changed = zone != old_zone && events == ZoneEvents::Sent;
        if zone_changed {
            if let Some(old) = old_zone {
                self.event(old, "ActorLeaving", [Value::Object(Some(actor))])?;
            }
            self.event(actor, "ZoneChange", [Value::Object(zone)])?;
        }
        if zone_changed || region != Some((zone, leaf, number)) {
            self.store.set(actor, Prop::Region, Value::Struct(vec![Value::Object(zone), Value::Int(leaf), Value::Byte(number)]));
        }
        if zone_changed && let Some(zone) = zone {
            self.event(zone, "ActorEntered", [Value::Object(Some(actor))])?;
        }
        // The volume part reads `Location` again: a warp zone's `ActorEntered` may have moved the actor.
        let location = self.store.vector(actor, Prop::Location);
        let new = self.physics_volume_at(location, use_touch)?;
        let old = self.store.object_of(actor, Prop::PhysicsVolume);
        let object = |o: Option<ObjectId>| Value::Object(o);
        if !self.store.is_a(actor, Kind::Pawn) {
            if events == ZoneEvents::Silent {
                self.store.set(actor, Prop::PhysicsVolume, object(new));
            } else if new != old {
                if let Some(old) = old {
                    self.event(old, "ActorLeavingVolume", [object(Some(actor))])?;
                    self.event(actor, "PhysicsVolumeChange", [object(new)])?;
                }
                self.store.set(actor, Prop::PhysicsVolume, object(new));
                if let Some(new) = new {
                    self.event(new, "ActorEnteredVolume", [object(Some(actor))])?;
                }
            }
            return Ok(());
        }
        let eyes = [location[0], location[1], location[2] + self.store.float_of(actor, Prop::BaseEyeHeight)];
        let head = self.physics_volume_at(eyes.into(), use_touch)?;
        if new != old {
            if events == ZoneEvents::Sent {
                if let Some(old) = old {
                    self.event(old, "PawnLeavingVolume", [object(Some(actor))])?;
                    self.event(actor, "PhysicsVolumeChange", [object(new)])?;
                }
                if let Some(controller) = self.store.object_of(actor, Prop::Controller) {
                    self.event(controller, "NotifyPhysicsVolumeChange", [object(new)])?;
                }
            }
            self.store.set(actor, Prop::PhysicsVolume, object(new));
            if events == ZoneEvents::Sent && let Some(new) = new {
                self.event(new, "PawnEnteredVolume", [object(Some(actor))])?;
            }
        }
        if head != self.store.object_of(actor, Prop::HeadVolume) {
            if events == ZoneEvents::Sent {
                let handled = match self.store.object_of(actor, Prop::Controller) {
                    Some(controller) => self.event_bool(controller, "NotifyHeadVolumeChange", [object(head)])?,
                    None => false,
                };
                if !handled {
                    self.event(actor, "HeadVolumeChange", [object(head)])?;
                }
            }
            self.store.set(actor, Prop::HeadVolume, object(head));
        }
        Ok(())
    }
}

/// As retail: strictly higher priority, `inside` tested only then. `priority` `None`: not a candidate.
fn highest_priority<C, T: Copy>(
    context: &mut C,
    default: (T, i32),
    candidates: impl IntoIterator<Item = T>,
    priority: impl Fn(&C, T) -> Option<i32>,
    mut inside: impl FnMut(&mut C, T) -> bool,
) -> T {
    let mut best = default;
    for candidate in candidates {
        let Some(weight) = priority(context, candidate) else { continue };
        if weight > best.1 && inside(context, candidate) {
            best = (candidate, weight);
        }
    }
    best.0
}

pub(crate) struct VolumeBox {
    bounds: Option<(Vector, Vector)>,
    volume: ObjectId,
}

pub(crate) struct VolumePlace {
    brush: ObjectId,
    placement: Placement,
    checked: Option<u64>,
    axes: [Vector; 3],
    model: Arc<Model>,
    bounds: Option<(Vector, Vector)>,
    world_bounds: Option<(Vector, Vector)>,
}

impl VolumePlace {
    fn new(brush: ObjectId, placement: Placement, model: Arc<Model>, frame: u64) -> Self {
        let bounds = (!model.nodes.is_empty() && !model.points.is_empty()).then(|| {
            let (min, max) = model.points.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(min, max), p| ([0, 1, 2].map(|i| min[i].min(p[i])), [0, 1, 2].map(|i| max[i].max(p[i]))));
            (Vector::from(min), Vector::from(max))
        });
        let axes = placement.rotation.axes();
        let world_bounds = bounds.map(|(min, max)| world_box((min.map(|m| m - 1.0), max.map(|m| m + 1.0)), placement, &axes));
        Self { brush, placement, checked: Some(frame), axes, model, bounds, world_bounds }
    }

    /// `point` is world and already inside `world_bounds`; 1 unit around the bounds still goes to the BSP (points on a plane
    /// count as inside).
    fn encompasses(&self, point: Vector) -> bool {
        let Placement { location, scale, pre_pivot, .. } = self.placement;
        let Some(local) = p2_collision::world_to_local(point, location, &self.axes, scale, pre_pivot) else { return false };
        if let Some((min, max)) = self.bounds
            && (0..3).any(|i| local[i] < min[i] - 1.0 || local[i] > max[i] + 1.0)
        {
            return false;
        }
        p2_collision::point_solid(&self.model, local)
    }

    fn line_time(&self, start: Vector, end: Vector) -> Option<f32> {
        let Placement { location, scale, pre_pivot, .. } = self.placement;
        let start = p2_collision::world_to_local(start, location, &self.axes, scale, pre_pivot)?;
        let end = p2_collision::world_to_local(end, location, &self.axes, scale, pre_pivot)?;
        p2_collision::line_check(&self.model, start, end).map(|hit| hit.time)
    }
}

/// Written so that a NaN coordinate is not rejected here, as it is not by the local bounds.
fn box_holds(bounds: Option<(Vector, Vector)>, point: Vector) -> bool {
    bounds.is_none_or(|(min, max)| !(0..3).any(|i| point[i] < min[i] || point[i] > max[i]))
}

/// World = location + Σₖ axes[k] · scale[k] · (local[k] − pre-pivot[k]), the inverse of
/// [`p2_collision::world_to_local`]; plus 1 unit and 1e-5 of the coordinates' size for rounding, so that a point outside
/// it is also outside `local` as `world_to_local` takes it there.
fn world_box(local: (Vector, Vector), placement: Placement, axes: &[Vector; 3]) -> (Vector, Vector) {
    let Placement { location, scale, pre_pivot, .. } = placement;
    let (min, max) = local;
    let centre = [0, 1, 2].map(|k| ((min[k] + max[k]) * 0.5 - pre_pivot[k]) * scale[k]);
    let half = [0, 1, 2].map(|k| ((max[k] - min[k]) * 0.5 * scale[k]).abs());
    let centre = [0, 1, 2].map(|c| location[c] + (0..3).map(|k| centre[k] * axes[k][c]).sum::<f32>());
    let half = [0, 1, 2].map(|c| (0..3).map(|k| half[k] * axes[k][c].abs()).sum::<f32>());
    let size = (0..3).map(|c| location[c].abs().max(centre[c].abs()) + half[c]).fold(0.0, f32::max);
    let margin = 1.0 + 1e-5 * size;
    ([0, 1, 2].map(|c| centre[c] - half[c] - margin).into(), [0, 1, 2].map(|c| centre[c] + half[c] + margin).into())
}

pub(crate) fn coords_of(value: Option<&Value>) -> [Vector; 4] {
    match value {
        Some(Value::Struct(members)) => [0, 1, 2, 3].map(|i| crate::natives::vec3(members.get(i))),
        _ => [Vector::new(0.0, 0.0, 0.0); 4],
    }
}

fn transposed([origin, x, y, z]: [Vector; 4]) -> [Vector; 4] {
    let column = |k: usize| Vector::new(x[k], y[k], z[k]);
    [-unrotate_by(&[x, y, z], origin), column(0), column(1), column(2)]
}

pub(crate) fn carry(coords: [Vector; 4], location: Vector, velocity: Vector, rotation: Rotator) -> (Vector, Vector, Rotator) {
    let [origin, axes @ ..] = coords;
    let [x, y, z] = rotation.axes().map(|axis| unrotate_by(&axes, axis));
    (unrotate_by(&axes, location - origin), unrotate_by(&axes, velocity), crate::natives::orthogonal_rotation(x, y, z))
}

pub(crate) fn warp(coords: [Vector; 4], location: Vector, velocity: Vector, rotation: Rotator) -> (Vector, Vector, Rotator) {
    carry(transposed(coords), location, velocity, rotation)
}

pub fn warp_view(this_side: [Vector; 4], other_side: [Vector; 4], location: Vector, rotation: Rotator) -> (Vector, Rotator) {
    let (location, _, rotation) = carry(this_side, location, Vector::ZERO, rotation);
    let (location, _, rotation) = warp(other_side, location, Vector::ZERO, rotation);
    (location, rotation)
}

/// The plane that cuts a far view (from `eye`, carried by [`warp_view`]) at the other side's portal: what lies between
/// `eye` and its portal must not show. Normal and distance: a point is kept where `normal.dot(point) >= distance`.
pub fn warp_clip_plane(other_side: [Vector; 4], eye: Vector) -> (Vector, f32) {
    let [origin, _, _, z] = other_side;
    let normal = if (eye - origin).dot(z) > 0.0 { -z } else { z };
    (normal, normal.dot(origin))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(origin: [f32; 3], x: [f32; 3], y: [f32; 3], z: [f32; 3]) -> [Vector; 4] {
        [origin.into(), x.into(), y.into(), z.into()]
    }

    fn close(a: Vector, b: [f32; 3]) -> bool {
        [0, 1, 2].iter().all(|&k| (a[k] - b[k]).abs() < 1e-3)
    }

    fn pick(default: (&'static str, i32), candidates: &[(&'static str, i32)], mut inside: impl FnMut(&'static str) -> bool) -> &'static str {
        highest_priority(&mut (), (default, default.1), candidates.iter().copied(), |_, c| Some(c.1), |_, c| inside(c.0)).0
    }

    #[test]
    fn a_view_through_a_portal_to_itself_does_not_move() {
        let side = frame([100.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        let (location, rotation) = warp_view(side, side, [90.0, 5.0, 7.0].into(), Rotator::default());
        assert!(close(location, [90.0, 5.0, 7.0]), "{location:?}");
        assert!(close(rotation.axes()[0], [1.0, 0.0, 0.0]));
    }

    #[test]
    fn a_view_is_moved_and_turned_by_the_difference_of_the_portals() {
        let here = frame([100.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        let there = frame([500.0, 0.0, 0.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        let (location, rotation) = warp_view(here, there, [90.0, 0.0, 0.0].into(), Rotator::default());
        assert!(close(location, [500.0, -10.0, 0.0]), "{location:?}");
        assert!(close(rotation.axes()[0], [0.0, 1.0, 0.0]));
    }

    #[test]
    fn the_clip_plane_keeps_what_lies_beyond_the_portal_from_the_eye() {
        let side = frame([500.0, 0.0, 20.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        for (eye, beyond) in [([500.0, 0.0, 70.0_f32], [500.0, 0.0, 10.0_f32]), ([500.0, 0.0, -30.0], [500.0, 0.0, 30.0])] {
            let (normal, distance) = warp_clip_plane(side, eye.into());
            assert!(normal.dot(eye.into()) < distance);
            assert!(normal.dot(beyond.into()) > distance);
        }
    }

    #[test]
    fn first_of_equal_priorities_wins_and_lower_ones_are_not_tested() {
        let mut tested = Vec::new();
        let best = pick(("default", -1_000_000), &[("low", -2_000_000), ("a", 5), ("b", 5), ("c", 3)], |v| {
            tested.push(v);
            true
        });
        assert_eq!(best, "a");
        assert_eq!(tested, ["a"]);
    }

    #[test]
    fn a_volume_that_does_not_encompass_is_skipped() {
        let best = pick(("none", 0), &[("high", 10), ("low", 5)], |v| v == "low");
        assert_eq!(best, "low");
        assert_eq!(pick(("none", 0), &[("high", 10)], |_| false), "none");
    }

    #[test]
    fn a_candidate_without_a_priority_is_neither_weighed_nor_tested() {
        let mut tested = Vec::new();
        let priority = |_: &(), v: i32| (v != 1).then_some(v * 10);
        let best = highest_priority(&mut (), (0, 0), [1, 2], priority, |_, v| {
            tested.push(v);
            true
        });
        assert_eq!(best, 2);
        assert_eq!(tested, [2]);
    }

    #[test]
    fn the_box_rejects_only_points_outside_it() {
        let bounds = Some((Vector::from([-10.0, 0.0, 5.0]), Vector::from([10.0, 20.0, 6.0])));
        for inside in [[0.0, 10.0, 5.5], [-10.0, 0.0, 5.0], [10.0, 20.0, 6.0]] {
            assert!(box_holds(bounds, inside.into()), "{inside:?}");
        }
        for outside in [[-10.01, 10.0, 5.5], [0.0, 20.01, 5.5], [0.0, 10.0, 4.99], [0.0, 10.0, 6.01]] {
            assert!(!box_holds(bounds, outside.into()), "{outside:?}");
        }
        assert!(box_holds(bounds, [f32::NAN, 10.0, 5.5].into()));
        assert!(box_holds(None, [1e9, -1e9, 0.0].into()));
    }

    #[test]
    fn the_world_box_holds_the_placed_local_box_and_nothing_outside_it_is_inside() {
        let placement = Placement { location: [1000.0, -2000.0, 300.0].into(), rotation: [3000, 12000, -7000].into(), scale: [2.0, -0.5, 3.0].into(), pre_pivot: [10.0, -4.0, 2.0].into() };
        let Placement { location, scale, pre_pivot, .. } = placement;
        let axes = placement.rotation.axes();
        let (min, max) = ([-64.0, -32.0, -128.0], [64.0, 96.0, 16.0]);
        let (low, high) = world_box((min.into(), max.into()), placement, &axes);
        let steps = [0.0, 0.25, 0.5, 0.75, 1.0];
        for tx in steps {
            for ty in steps {
                for tz in steps {
                    let t = [tx, ty, tz];
                    let local = [0, 1, 2].map(|k| min[k] + (max[k] - min[k]) * t[k]);
                    let s = [0, 1, 2].map(|k| (local[k] - pre_pivot[k]) * scale[k]);
                    let world = [0, 1, 2].map(|c| location[c] + s[0] * axes[0][c] + s[1] * axes[1][c] + s[2] * axes[2][c]);
                    assert!((0..3).all(|c| world[c] >= low[c] && world[c] <= high[c]), "{local:?} -> {world:?} not in {low:?}..{high:?}");
                }
            }
        }
        let middle = [0, 1, 2].map(|c| (low[c] + high[c]) * 0.5);
        for c in 0..3 {
            for beyond in [low[c] - 0.01, high[c] + 0.01] {
                let mut point = middle;
                point[c] = beyond;
                let local = p2_collision::world_to_local(point.into(), location, &axes.map(Vector::from), scale, pre_pivot).unwrap_or([f32::NAN; 3].into());
                assert!((0..3).any(|k| local[k] < min[k] || local[k] > max[k]), "{point:?} -> {local:?} in the local box");
            }
        }
    }

    fn assert_near((location, velocity, rotation): (Vector, Vector, Rotator), want: ([f32; 3], [f32; 3], [i32; 3])) {
        for (got, want) in [(location, want.0), (velocity, want.1)] {
            assert!((0..3).all(|i| (got[i] - want[i]).abs() < 1e-3), "{got:?} != {want:?}");
        }
        assert!((0..3).all(|i| matches!((rotation[i] - want.2[i]) & 0xffff, 0..=2 | 0xfffe..=0xffff)), "{rotation:?} != {:?}", want.2);
    }

    fn turned_frame() -> [Vector; 4] {
        [Vector::new(100.0, 0.0, 0.0), Vector::new(0.0, 1.0, 0.0), Vector::new(-1.0, 0.0, 0.0), Vector::new(0.0, 0.0, 1.0)]
    }

    #[test]
    fn unwarp_measures_location_velocity_and_rotation_in_the_portal_frame() {
        let carried = carry(turned_frame(), Vector::new(100.0, 50.0, 7.0), Vector::new(0.0, 10.0, -3.0), Rotator::new(0, 16384, 0));
        assert_near(carried, ([50.0, 0.0, 7.0], [10.0, 0.0, -3.0], [0, 0, 0]));
    }

    #[test]
    fn warp_places_portal_frame_values_in_the_world() {
        let frame = [Vector::new(0.0, 0.0, 500.0), Vector::new(1.0, 0.0, 0.0), Vector::new(0.0, 1.0, 0.0), Vector::new(0.0, 0.0, 1.0)];
        let carried = warp(frame, Vector::new(50.0, 0.0, 7.0), Vector::new(10.0, 0.0, -3.0), Rotator::new(0, 0, 0));
        assert_near(carried, ([50.0, 0.0, 507.0], [10.0, 0.0, -3.0], [0, 0, 0]));
        let carried = warp(turned_frame(), Vector::new(50.0, 0.0, 7.0), Vector::new(10.0, 0.0, -3.0), Rotator::new(0, 0, 0));
        assert_near(carried, ([100.0, 50.0, 7.0], [0.0, 10.0, -3.0], [0, 16384, 0]));
    }

    #[test]
    fn warp_undoes_unwarp_through_the_same_frame() {
        let (location, velocity, rotation) = (Vector::new(-30.0, 220.0, 64.0), Vector::new(5.0, -8.0, 2.0), Rotator::new(2000, -12000, 300));
        let (l, v, r) = carry(turned_frame(), location, velocity, rotation);
        assert_near(warp(turned_frame(), l, v, r), ([-30.0, 220.0, 64.0], [5.0, -8.0, 2.0], [2000, -12000, 300]));
    }

    #[test]
    fn coords_read_from_a_struct_and_default_to_zero() {
        let value = Value::Struct(turned_frame().into_iter().map(Value::Vector).collect());
        assert_eq!(coords_of(Some(&value)), turned_frame());
        assert_eq!(coords_of(None), [Vector::new(0.0, 0.0, 0.0); 4]);
    }
}
