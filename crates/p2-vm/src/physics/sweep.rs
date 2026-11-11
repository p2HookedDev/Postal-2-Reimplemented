use p2_collision::{Primitive, mesh_box_check, pull_back_units, segment_cylinder};
use p2_math::Vector;
use crate::volumes::{OldZone, ZoneEvents};
use crate::{Error, LoaderRef, ObjectId, Prop, Vm};
use super::{Bases, CYLINDER_PULL_BACK, MOVE_TEST_ADJUST, MoveHit, Sweep, SweepAgainst, nearly_zero};

impl<L: LoaderRef> Vm<L> {
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
                let bounds = self.placed_bounds(other);
                let (min, max) = if let Some((min, max)) = bounds {
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
                let shape = bounds.and_then(|_| self.fill_shape(other)).and_then(|placement| self.placed_shape(other, &placement));
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
        let (radius, height) = self.store.collision_size(actor);
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
        let (radius, height) = self.store.collision_size(actor);
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
}
