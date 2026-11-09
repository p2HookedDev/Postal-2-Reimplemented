use p2_math::Vector;
use crate::{Error, ObjectId, LoaderRef, Prop, Value, Vm};

mod encroach;
mod level;
mod lifecycle;
mod tick;
mod touch;

fn add_time(profile: &mut std::collections::HashMap<String, std::time::Duration>, name: &str, time: std::time::Duration) {
    match profile.get_mut(name) {
        Some(total) => *total += time,
        None => {
            profile.insert(name.to_owned(), time);
        }
    }
}

const SPAWN_EVENTS: [&str; 3] = ["Spawned", "PreBeginPlay", "BeginPlay"];
const BEGUN_EVENTS: [&str; 3] = ["PostBeginPlay", "SetInitialState", "PostNetBeginPlay"];
const MIN_TICK: f32 = 0.0005;
const MAX_TICK: f32 = 0.4;

/// Live non-static actors in creation order: what a level tick and the collision hash rebuild walk.
/// `bStatic` is `const` in script, so a new actor is sorted once, on the first walk after it was made.
#[derive(Debug, Default)]
pub(crate) struct DynamicActors {
    ids: Vec<ObjectId>,
    unsorted: Vec<ObjectId>,
    /// All [`Vm::destroy`]'s walk needs besides `ids`. `None` until the first destroy and after a level or a save is
    /// loaded. `Owner` and `Touching` are `const` in script (unverified), so only natives write them.
    linked_statics: Option<std::collections::BTreeSet<ObjectId>>,
}

impl DynamicActors {
    pub(crate) fn added(&mut self, actor: ObjectId) {
        self.unsorted.push(actor);
    }

    pub(crate) fn removed(&mut self, actor: ObjectId) {
        if let Ok(at) = self.ids.binary_search(&actor) {
            self.ids.remove(at);
        }
        self.unsorted.retain(|&a| a != actor);
        if let Some(list) = &mut self.linked_statics {
            list.remove(&actor);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.ids.clear();
        self.unsorted.clear();
        self.linked_statics = None;
    }

    pub(crate) fn forget_linked_statics(&mut self) {
        self.linked_statics = None;
    }

    fn walk(&mut self, is_static: impl Fn(ObjectId) -> bool) -> Vec<ObjectId> {
        self.sort_new(is_static);
        self.ids.clone()
    }

    fn sort_new(&mut self, is_static: impl Fn(ObjectId) -> bool) {
        for actor in self.unsorted.drain(..) {
            if !is_static(actor)
                && let Err(at) = self.ids.binary_search(&actor)
            {
                self.ids.insert(at, actor);
            }
        }
    }

    fn next_after(&self, after: Option<ObjectId>) -> Option<ObjectId> {
        let at = after.map_or(0, |after| self.ids.partition_point(|&id| id <= after));
        let linked = self.linked_statics.as_ref().and_then(|list| next_level_actor(list, after));
        self.ids.get(at).copied().into_iter().chain(linked).min()
    }
}

impl<L: LoaderRef> Vm<L> {
    pub fn mark_rendered(&mut self, actor: ObjectId) -> Result<(), Error> {
        let Some(level) = self.level else { return Ok(()) };
        let time = self.store.get(level, Prop::TimeSeconds).unwrap_or(Value::Float(0.0));
        self.store.set(actor, Prop::LastRenderTime, time);
        Ok(())
    }

    /// `axes` in world directions. No touch events: bone attachments move only when their base is drawn,
    /// so scripts read where they were last drawn (a `Head`'s `Location` for head shots).
    pub fn place_attachment(&mut self, actor: ObjectId, location: Vector, axes: [Vector; 3]) {
        if self.store.is_deleted(actor) || self.store.object_of(actor, Prop::Base).is_none() || !self.on_bone(actor) {
            return;
        }
        self.store.set(actor, Prop::Location, Value::Vector(location));
        self.store.set(actor, Prop::Rotation, Value::Rotator(crate::natives::orthogonal_rotation(axes[0], axes[1], axes[2])));
        self.volume_moved(actor);
        self.relocate(actor);
    }
}

/// Both strictly less, compared squared, height first, as retail.
pub(crate) fn cylinders_meet(here: Vector, there: Vector, radii: f32, heights: f32) -> bool {
    let (dx, dy, dz) = (here[0] - there[0], here[1] - there[1], here[2] - there[2]);
    dz * dz < heights * heights && dy * dy + dx * dx < radii * radii
}

#[expect(clippy::struct_excessive_bools, reason = "the actor flags retail's IsOverlapping reads, one field each")]
#[derive(Clone, Copy, Default)]
struct OverlapSide {
    brush: bool,
    encroacher: bool,
    only_affect_pawns: bool,
    pawn: bool,
    blocks_extent: bool,
}

/// Retail's overlap rejects, before its shape test. Retail tests Owner one way only, and reads
/// `bBlockNonZeroExtentTraces` here, not `bCollideActors`.
fn overlap_allowed(a: OverlapSide, b: OverlapSide, related: bool) -> bool {
    !(a.brush && b.brush)
        && !related
        && !(a.encroacher && b.encroacher)
        && (!a.only_affect_pawns || b.pawn)
        && (!b.only_affect_pawns || a.pawn)
        && a.blocks_extent
        && b.blocks_extent
}

fn encroachment_tested([collide_actors, block_actors, block_players, encroacher]: [bool; 4]) -> bool {
    collide_actors || block_actors || block_players || encroacher
}

fn next_level_actor(actors: &std::collections::BTreeSet<ObjectId>, after: Option<ObjectId>) -> Option<ObjectId> {
    match after {
        Some(after) => actors.range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded)).next().copied(),
        None => actors.first().copied(),
    }
}

fn same_members(before: impl Iterator<Item = ObjectId> + Clone, now: &[ObjectId]) -> bool {
    before.clone().all(|old| now.contains(&old)) && now.iter().all(|&new| before.clone().any(|old| old == new))
}

/// What a far move tests, given `bCollideWorld`, `bCollideWhenPlacing` and `bSkipEncroachmentCheck`
/// (the first two names unverified).
fn far_move_tests(encroachment: Encroachment, [collide_world, collide_when_placing, skip_encroachment]: [bool; 3]) -> (bool, bool) {
    let checked = encroachment == Encroachment::Checked;
    (checked && (collide_world || collide_when_placing), checked && !skip_encroachment)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encroachment {
    Checked,
    Ignored,
}

#[cfg(test)]
mod tests {
    use super::{
        BEGUN_EVENTS, DynamicActors, OverlapSide, SPAWN_EVENTS, cylinders_meet, encroachment_tested,
        next_level_actor, overlap_allowed, same_members,
    };
    use crate::ObjectId;

    #[test]
    fn tick_probe_bit_is_the_tick_name_s() {
        assert_eq!(crate::states::probe_bit("Tick"), Some(crate::enums::EProbe::Tick as u32));
    }

    #[test]
    fn destroy_level_walk() {
        let mut actors: std::collections::BTreeSet<ObjectId> = [1, 3, 5].map(ObjectId).into();
        assert_eq!(next_level_actor(&actors, None), Some(ObjectId(1)));
        actors.insert(ObjectId(9));
        actors.remove(&ObjectId(3));
        assert_eq!(next_level_actor(&actors, Some(ObjectId(1))), Some(ObjectId(5)));
        assert_eq!(next_level_actor(&actors, Some(ObjectId(5))), Some(ObjectId(9)));
        assert_eq!(next_level_actor(&actors, Some(ObjectId(9))), None);
        assert_eq!(next_level_actor(&std::collections::BTreeSet::new(), None), None);
    }

    #[test]
    fn far_move_test_gate() {
        assert_eq!(super::far_move_tests(super::Encroachment::Ignored, [true, true, false]), (false, false));
        assert_eq!(super::far_move_tests(super::Encroachment::Checked, [false, false, false]), (false, true));
        assert_eq!(super::far_move_tests(super::Encroachment::Checked, [true, false, false]), (true, true));
        assert_eq!(super::far_move_tests(super::Encroachment::Checked, [false, true, true]), (true, false));
    }

    #[test]
    fn encroachment_test_gate() {
        assert!(!encroachment_tested([false, false, false, false]));
        assert!(encroachment_tested([true, false, false, false]));
        assert!(encroachment_tested([false, true, false, false]));
        assert!(encroachment_tested([false, false, true, false]));
        assert!(encroachment_tested([false, false, false, true]));
    }

    #[test]
    fn spawn_event_order() {
        assert_eq!(SPAWN_EVENTS, ["Spawned", "PreBeginPlay", "BeginPlay"]);
        assert_eq!(BEGUN_EVENTS, ["PostBeginPlay", "SetInitialState", "PostNetBeginPlay"]);
    }

    #[test]
    fn overlap_rejects() {
        let plain = OverlapSide { blocks_extent: true, ..OverlapSide::default() };
        let allowed = overlap_allowed;
        assert!(allowed(plain, plain, false));
        assert!(!allowed(plain, plain, true));
        assert!(!allowed(plain, plain, true));
        assert!(!allowed(plain, plain, true));
        let brush = OverlapSide { brush: true, ..plain };
        assert!(!allowed(brush, brush, false));
        assert!(allowed(brush, plain, false));
        let encroacher = OverlapSide { encroacher: true, ..plain };
        assert!(!allowed(encroacher, encroacher, false));
        assert!(allowed(encroacher, plain, false));
        let pawns_only = OverlapSide { only_affect_pawns: true, ..plain };
        let pawn = OverlapSide { pawn: true, ..plain };
        assert!(!allowed(pawns_only, plain, false));
        assert!(!allowed(plain, pawns_only, false));
        assert!(allowed(pawns_only, pawn, false));
        assert!(allowed(pawn, pawns_only, false));
        let no_extent = OverlapSide::default();
        assert!(!allowed(no_extent, plain, false));
        assert!(!allowed(plain, no_extent, false));
    }

    #[test]
    fn cylinders_meet_strictly_inside() {
        let at = |x: f32, y: f32, z: f32| p2_math::Vector::from([x, y, z]);
        assert!(cylinders_meet(at(0.0, 0.0, 0.0), at(3.0, 4.0, 0.0), 5.5, 1.0));
        assert!(!cylinders_meet(at(0.0, 0.0, 0.0), at(3.0, 4.0, 0.0), 5.0, 1.0));
        assert!(cylinders_meet(at(0.0, 0.0, 10.0), at(0.0, 0.0, 0.5), 1.0, 10.0));
        assert!(!cylinders_meet(at(0.0, 0.0, 0.0), at(0.0, 0.0, -10.0), 1.0, 10.0));
    }

    #[test]
    fn same_members_ignores_order() {
        let ids = |list: &[u32]| list.iter().map(|&i| ObjectId(i)).collect::<Vec<_>>();
        let (a, b) = (ids(&[1, 2, 3]), ids(&[3, 1, 2]));
        assert!(same_members(a.iter().copied(), &b));
        assert!(same_members(std::iter::empty(), &[]));
        assert!(!same_members(a.iter().copied(), &ids(&[1, 2])));
        assert!(!same_members(ids(&[1, 2]).into_iter(), &a));
        assert!(!same_members(std::iter::empty(), &ids(&[4])));
    }

    #[test]
    fn dynamic_actors_keep_creation_order() {
        let mut list = DynamicActors::default();
        let is_static = |actor: ObjectId| actor == ObjectId(3);
        for id in [1, 2, 3, 4] {
            list.added(ObjectId(id));
        }
        assert_eq!(list.walk(is_static), [ObjectId(1), ObjectId(2), ObjectId(4)]);
        list.removed(ObjectId(2));
        list.added(ObjectId(5));
        list.added(ObjectId(6));
        list.removed(ObjectId(6));
        assert_eq!(list.walk(is_static), [ObjectId(1), ObjectId(4), ObjectId(5)]);
        list.added(ObjectId(5));
        assert_eq!(list.walk(is_static), [ObjectId(1), ObjectId(4), ObjectId(5)]);
        list.added(ObjectId(7));
        list.clear();
        assert!(list.walk(is_static).is_empty());
    }

    #[test]
    fn the_destroy_walk_merges_linked_statics_in_level_order() {
        let mut list = DynamicActors::default();
        for id in [1, 3, 4, 7] {
            list.added(ObjectId(id));
        }
        list.sort_new(|actor| actor == ObjectId(3) || actor == ObjectId(4));
        list.linked_statics = Some([ObjectId(3), ObjectId(9)].into_iter().collect());
        let mut walked = Vec::new();
        let mut at = None;
        while let Some(actor) = list.next_after(at) {
            walked.push(actor);
            at = Some(actor);
            if actor == ObjectId(3) {
                list.removed(ObjectId(7));
                list.added(ObjectId(8));
                list.sort_new(|_| false);
            }
        }
        assert_eq!(walked, [ObjectId(1), ObjectId(3), ObjectId(8), ObjectId(9)]);
    }
}
