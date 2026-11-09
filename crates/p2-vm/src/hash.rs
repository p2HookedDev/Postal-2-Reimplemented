//! Every write of a colliding actor's `Location` or collision must go through [`Vm::relocate`].

use p2_math::Vector;
use rustc_hash::FxHashMap;

use crate::profiling::Work;
use crate::{LoaderRef, ObjectId, Prop, Vm};

const CELL: f32 = 256.0;
/// Actors wider than a cell go into every cell their reach covers, up to this many cells (a reach of about 2000
/// units); wider ones are candidates of every query.
const MAX_SPREAD_CELLS: i64 = 256;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Entry {
    pub actor: ObjectId,
    /// May block box (extent) moves: `bBlockNonZeroExtentTraces`, and not part of the static world geometry (which
    /// `Vm::trace_boxes` checks). Whether it blocks a given mover is `Vm::is_blocked_by`.
    pub blocks: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Place {
    Cells([i32; 4]),
    Always,
}

#[derive(Debug, Default)]
pub(crate) struct CollisionHash {
    cells: FxHashMap<(i32, i32), Vec<ObjectId>>,
    always: Vec<ObjectId>,
    placed: FxHashMap<ObjectId, Place>,
    stamps: Vec<u32>,
    stamp: u32,
    spare: Vec<Vec<Entry>>,
    pub(crate) built: bool,
}

fn cell(v: f32) -> i32 {
    (v / CELL).floor() as i32
}

/// A long line's corridor instead of its bounding rectangle (a diagonal shot across a map covered hundreds of cells and
/// every actor in them). Square cells of `size` units within `pad` of `a`..`b`, column by column without repeats.
pub(crate) fn segment_cells(a: [f32; 2], b: [f32; 2], pad: f32, size: f32) -> impl Iterator<Item = (i32, i32)> {
    let index = move |v: f32| (v / size).floor() as i32;
    let (x0, x1) = (index(a[0].min(b[0]) - pad), index(a[0].max(b[0]) + pad));
    let dx = b[0] - a[0];
    (x0..=x1).flat_map(move |x| {
        let (low, high) = (x as f32 * size - pad, (x + 1) as f32 * size + pad);
        let (t0, t1) = if dx.abs() < 1e-6 {
            (0.0, 1.0)
        } else {
            let (u, v) = ((low - a[0]) / dx, (high - a[0]) / dx);
            (u.min(v).max(0.0), u.max(v).min(1.0))
        };
        let (ya, yb) = (a[1] + (b[1] - a[1]) * t0, a[1] + (b[1] - a[1]) * t1);
        let (y0, y1) = if t0 <= t1 { (index(ya.min(yb) - pad), index(ya.max(yb) + pad)) } else { (0, -1) };
        (y0..=y1).map(move |y| (x, y))
    })
}

fn place_of(location: Vector, radius: f32) -> Place {
    if radius <= CELL {
        let (x, y) = (cell(location[0]), cell(location[1]));
        return Place::Cells([x, x, y, y]);
    }
    let (x0, x1) = (cell(location[0] - radius), cell(location[0] + radius));
    let (y0, y1) = (cell(location[1] - radius), cell(location[1] + radius));
    if (i64::from(x1) - i64::from(x0) + 1) * (i64::from(y1) - i64::from(y0) + 1) > MAX_SPREAD_CELLS {
        return Place::Always;
    }
    Place::Cells([x0, x1, y0, y1])
}

fn place_cells([x0, x1, y0, y1]: [i32; 4]) -> impl Iterator<Item = (i32, i32)> {
    (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| (x, y)))
}

impl CollisionHash {
    pub(crate) fn insert(&mut self, actor: ObjectId, location: Vector, radius: f32) -> bool {
        let place = place_of(location, radius);
        if self.placed.get(&actor) == Some(&place) {
            return false;
        }
        self.remove(actor);
        match place {
            Place::Always => self.always.push(actor),
            Place::Cells(range) => {
                for c in place_cells(range) {
                    self.cells.entry(c).or_default().push(actor);
                }
            }
        }
        self.placed.insert(actor, place);
        true
    }

    pub(crate) fn remove(&mut self, actor: ObjectId) {
        let Some(place) = self.placed.remove(&actor) else { return };
        match place {
            Place::Always => self.always.retain(|&a| a != actor),
            Place::Cells(range) => {
                for c in place_cells(range) {
                    if let Some(list) = self.cells.get_mut(&c)
                        && let Some(i) = list.iter().position(|&a| a == actor)
                    {
                        list.swap_remove(i);
                    }
                }
            }
        }
    }

    fn gather(&mut self, keys: impl Iterator<Item = (i32, i32)>) -> Vec<Entry> {
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.stamps.fill(0);
            self.stamp = 1;
        }
        let mut found = self.spare.pop().unwrap_or_default();
        let (cells, stamps, stamp) = (&self.cells, &mut self.stamps, self.stamp);
        for &actor in keys.filter_map(|c| cells.get(&c)).flatten().chain(&self.always) {
            let i = actor.0 as usize;
            if stamps.len() <= i {
                stamps.resize(i + 1, 0);
            }
            if stamps[i] != stamp {
                stamps[i] = stamp;
                found.push(Entry { actor, blocks: false });
            }
        }
        found
    }

    /// The box grows by a cell, the largest radius of an actor kept by its location's cell.
    /// A box of more cells than the hash has actors walks the actors, so a huge radius stays cheap.
    pub(crate) fn near(&mut self, min: [f32; 2], max: [f32; 2]) -> Vec<Entry> {
        let (x0, x1) = (cell(min[0] - CELL), cell(max[0] + CELL));
        let (y0, y1) = (cell(min[1] - CELL), cell(max[1] + CELL));
        let cells = (i64::from(x1) - i64::from(x0) + 1).saturating_mul(i64::from(y1) - i64::from(y0) + 1);
        if cells > self.placed.len() as i64 {
            let overlaps = |place: &Place| match *place {
                Place::Always => true,
                Place::Cells([a0, a1, b0, b1]) => a0 <= x1 && x0 <= a1 && b0 <= y1 && y0 <= b1,
            };
            let mut found = self.spare.pop().unwrap_or_default();
            found.extend(self.placed.iter().filter(|(_, place)| overlaps(place)).map(|(&actor, _)| Entry { actor, blocks: false }));
            return found;
        }
        self.gather(place_cells([x0, x1, y0, y1]))
    }

    pub(crate) fn near_segment(&mut self, a: [f32; 2], b: [f32; 2], pad: f32) -> Vec<Entry> {
        self.gather(segment_cells(a, b, pad + CELL, CELL))
    }
}

impl<L: LoaderRef> Vm<L> {
    fn hash_read(&self, actor: ObjectId) -> Option<(ObjectId, Vector, Option<f32>)> {
        if self.store.is_deleted(actor) || !self.store.flag_of(actor, Prop::bCollideActors) {
            return None;
        }
        let world = self.world.static_actors.contains(&actor);
        // A live ragdoll goes in by the box around its Karma bodies, not its cylinder:
        // the box's centre, reaching its larger horizontal half size.
        if let Some((min, max)) = self.ragdoll_box(actor) {
            let half = (max.x - min.x).max(max.y - min.y) * 0.5;
            return Some((actor, (min + max) * 0.5, Some(half)));
        }
        let reach = if world { Some(self.store.float_of(actor, Prop::CollisionRadius)) } else { self.placed_reach(actor) };
        Some((actor, self.store.vector(actor, Prop::Location), reach))
    }

    pub(crate) fn collision_hash(&mut self) -> &mut CollisionHash {
        if !self.hash.built {
            let phase = self.profile_start();
            let mut hash = CollisionHash::default();
            let actors: Vec<ObjectId> = self.actors().collect();
            let read: Vec<_> = actors.iter().map(|&actor| self.hash_read(actor)).collect();
            self.count_n(Work::HashActor, actors.len());
            for (actor, location, reach) in read.into_iter().flatten() {
                self.count(Work::HashEntry);
                let radius = reach.unwrap_or_else(|| self.collision_reach(actor));
                hash.insert(actor, location, radius);
            }
            hash.built = true;
            self.hash = hash;
            self.profile_phase("collision hash", phase);
        }
        &mut self.hash
    }

    pub fn collision_hash_census(&self) -> (usize, usize, usize) {
        let hash = &self.hash;
        let stale = hash.placed.keys().filter(|&&actor| self.store.is_deleted(actor) || !self.store.flag_of(actor, Prop::bCollideActors)).count();
        (hash.placed.len(), hash.always.len(), stale)
    }

    /// Nothing before the first query, which reads every actor. A static actor has the whole hash read again at the next
    /// query.
    pub(crate) fn relocate(&mut self, actor: ObjectId) {
        if !self.hash.built {
            return;
        }
        if self.store.flag_of(actor, Prop::bStatic) {
            self.hash.built = false;
            return;
        }
        self.count(Work::HashActor);
        match self.hash_read(actor) {
            None => self.hash.remove(actor),
            Some((_, location, reach)) => {
                let radius = reach.unwrap_or_else(|| self.collision_reach(actor));
                if self.hash.insert(actor, location, radius) {
                    self.count(Work::HashEntry);
                }
            }
        }
    }

    pub(crate) fn colliders_along(&mut self, a: [f32; 2], b: [f32; 2], radius: f32) -> Vec<Entry> {
        let found = self.collision_hash().near_segment(a, b, radius);
        self.current_entries(found)
    }

    pub(crate) fn colliders_near(&mut self, min: [f32; 2], max: [f32; 2], radius: f32) -> Vec<Entry> {
        let (min, max) = ([min[0] - radius, min[1] - radius], [max[0] + radius, max[1] + radius]);
        let found = self.collision_hash().near(min, max);
        self.current_entries(found)
    }

    pub(crate) fn give_entries(&mut self, mut entries: Vec<Entry>) {
        entries.clear();
        self.hash.spare.push(entries);
    }

    /// In actor order: touch events and `encroached` see actors in it. `blocks` is read now: scripts may change
    /// `bBlockNonZeroExtentTraces` without moving the actor.
    fn current_entries(&self, mut found: Vec<Entry>) -> Vec<Entry> {
        for entry in &mut found {
            entry.blocks = !self.world.static_actors.contains(&entry.actor) && self.store.flag_of(entry.actor, Prop::bBlockNonZeroExtentTraces);
        }
        found.sort_unstable_by_key(|e| e.actor);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_cells_cover_the_corridor_once() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut random = move |low: f32, high: f32| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            low + (high - low) * ((state >> 40) as f32 / 16_777_216.0)
        };
        for _ in 0..300 {
            let (a, b) = ([random(-3000.0, 3000.0), random(-3000.0, 3000.0)], [random(-3000.0, 3000.0), random(-3000.0, 3000.0)]);
            let pad = random(0.0, 600.0);
            let cells: Vec<(i32, i32)> = segment_cells(a, b, pad, CELL).collect();
            let mut unique = cells.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), cells.len(), "repeats");
            for k in 0..=64 {
                let t = k as f32 / 64.0;
                let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                for q in [[p[0] - pad, p[1] - pad], [p[0] + pad, p[1] + pad], [p[0] - pad, p[1] + pad], [p[0] + pad, p[1] - pad], p] {
                    assert!(unique.binary_search(&(cell(q[0]), cell(q[1]))).is_ok(), "{a:?} {b:?} {pad}: {q:?} missed");
                }
            }
        }
        let diagonal: Vec<_> = segment_cells([0.0, 0.0], [10_000.0, 10_000.0], 0.0, CELL).collect();
        assert!(diagonal.len() < 200, "{}", diagonal.len());
    }

    fn found(hash: &mut CollisionHash, at: [f32; 2]) -> Vec<ObjectId> {
        hash.near(at, at).into_iter().map(|e| e.actor).collect()
    }

    fn entries(hash: &CollisionHash, actor: ObjectId) -> usize {
        hash.cells.values().flatten().chain(&hash.always).filter(|&&a| a == actor).count()
    }

    #[test]
    fn wide_actors_fill_the_cells_they_cover() {
        let mut hash = CollisionHash::default();
        let (wide, huge) = (ObjectId(1), ObjectId(2));
        hash.insert(wide, [0.0; 3].into(), 1000.0);
        hash.insert(huge, [0.0; 3].into(), 5000.0);
        assert!(found(&mut hash, [990.0, -990.0]).contains(&wide));
        assert!(!found(&mut hash, [3000.0, 0.0]).contains(&wide));
        assert!(found(&mut hash, [100_000.0, 0.0]).contains(&huge));
    }

    #[test]
    fn relocated_actors_leave_their_old_cells() {
        let mut hash = CollisionHash::default();
        for (actor, radius) in [(ObjectId(1), 50.0), (ObjectId(2), 1000.0)] {
            hash.insert(actor, [0.0; 3].into(), radius);
            assert!(hash.insert(actor, [10_000.0, 10_000.0, 0.0].into(), radius));
            assert!(found(&mut hash, [10_000.0, 10_000.0]).contains(&actor));
            assert!(!found(&mut hash, [0.0, 0.0]).contains(&actor));
        }
        let huge = ObjectId(3);
        hash.insert(huge, [0.0; 3].into(), 5000.0);
        hash.insert(huge, [0.0; 3].into(), 50.0);
        assert!(!found(&mut hash, [100_000.0, 0.0]).contains(&huge));
        assert_eq!(entries(&hash, huge), 1);
    }

    #[test]
    fn removed_actors_are_not_found() {
        let mut hash = CollisionHash::default();
        let (narrow, wide, huge) = (ObjectId(1), ObjectId(2), ObjectId(3));
        hash.insert(narrow, [0.0; 3].into(), 50.0);
        hash.insert(wide, [0.0; 3].into(), 1000.0);
        hash.insert(huge, [0.0; 3].into(), 5000.0);
        for actor in [narrow, wide, huge, ObjectId(4)] {
            hash.remove(actor);
        }
        assert!(found(&mut hash, [0.0, 0.0]).is_empty());
        assert_eq!(entries(&hash, wide), 0);
    }

    #[test]
    fn unchanged_relocate_keeps_one_entry() {
        let mut hash = CollisionHash::default();
        let actor = ObjectId(1);
        assert!(hash.insert(actor, [10.0, 10.0, 0.0].into(), 50.0));
        assert!(!hash.insert(actor, [20.0, 30.0, 5.0].into(), 50.0));
        assert_eq!(entries(&hash, actor), 1);
        assert_eq!(found(&mut hash, [0.0, 0.0]), [actor]);
    }

    #[test]
    fn a_box_of_many_cells_finds_what_its_cells_hold() {
        let (mut few, mut many) = (CollisionHash::default(), CollisionHash::default());
        for (actor, x, radius) in [(1, 0.0, 50.0), (2, 600.0, 50.0), (3, 5000.0, 50.0), (4, 900.0, 1000.0), (5, 0.0, 5000.0)] {
            few.insert(ObjectId(actor), [x, 0.0, 0.0].into(), radius);
            many.insert(ObjectId(actor), [x, 0.0, 0.0].into(), radius);
        }
        for n in 0..40 {
            many.insert(ObjectId(100 + n), [100_000.0 + n as f32 * 1000.0, 100_000.0, 0.0].into(), 50.0);
        }
        let sorted = |hash: &mut CollisionHash| {
            let mut actors: Vec<ObjectId> = hash.near([0.0, 0.0], [256.0, 256.0]).into_iter().map(|e| e.actor).collect();
            actors.sort_unstable();
            actors
        };
        assert_eq!(sorted(&mut few), sorted(&mut many));
    }
}
