//! Detail faces: a lamp cube face whose plain tile would show its texels gets its own square in a band a quarter
//! of the atlas side under the lamps' tiles, sized so a texel is no larger on screen than the shadow's soft edge.
//! Squares are powers of two, split from and merged back into the largest; an unwanted face keeps its square until
//! another needs it.

use bevy::math::Vec3;
use rustc_hash::FxHashMap;

use super::atlas::FACE_AXES;

/// Texels. A square's corner is stored in these, 8 bits an axis. `lamp_shadow.wesl` holds the same number.
pub(super) const DETAIL_UNIT: u32 = 64;
const LEVELS: usize = 5;
const ROOTS_ACROSS: u32 = 16;
const ROOTS_DOWN: u32 = 4;

/// Half the shadow's soft edge in world units (`SOFT_EDGE_UNITS` in `sun_shadow.wesl`): texels finer than this
/// show nothing more.
pub(super) const FINEST_TEXEL: f32 = 0.75;

/// A cube face reaches 45 degrees from its axis at the middle of a side and this far at a corner.
const FACE_REACH: f32 = 0.955_316_6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FaceSlot {
    pub(super) lamp: u32,
    pub(super) face: u8,
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) level: u8,
}

impl FaceSlot {
    pub(super) fn side(self) -> u32 {
        DETAIL_UNIT << self.level
    }

    pub(super) fn packed(self) -> f32 {
        (1 + u32::from(self.x) + (u32::from(self.y) << 8) + (u32::from(self.level) << 16)) as f32
    }
}

struct Region {
    free: [Vec<(u16, u16)>; LEVELS],
    top: usize,
}

fn root_units(atlas_side: u32) -> u32 {
    (atlas_side / DETAIL_UNIT / ROOTS_ACROSS).max(1)
}

/// The atlas height that holds `tile_texels` rows of lamp tiles and the detail band under them. The band starts on
/// a whole root square, since squares split and merge on corners aligned to their size.
pub(super) fn atlas_height(atlas_side: u32, tile_texels: u32) -> u32 {
    let root = root_units(atlas_side) * DETAIL_UNIT;
    tile_texels.next_multiple_of(root) + ROOTS_DOWN * root
}

impl Region {
    fn new([atlas_side, atlas_height]: [u32; 2]) -> Self {
        let root = root_units(atlas_side);
        let top = (root.trailing_zeros() as usize).min(LEVELS - 1);
        let mut free: [Vec<(u16, u16)>; LEVELS] = Default::default();
        let first_row = atlas_height / DETAIL_UNIT - ROOTS_DOWN * root;
        free[top] = (0..ROOTS_DOWN).flat_map(|y| (0..ROOTS_ACROSS).map(move |x| ((x * root) as u16, (first_row + y * root) as u16))).collect();
        Self { free, top }
    }

    fn take(&mut self, level: usize) -> Option<(u16, u16)> {
        if level > self.top {
            return None;
        }
        if let Some(corner) = self.free[level].pop() {
            return Some(corner);
        }
        let (x, y) = self.take(level + 1)?;
        let side = 1 << level;
        self.free[level].extend([(x + side, y), (x, y + side), (x + side, y + side)]);
        Some((x, y))
    }

    fn give(&mut self, (x, y): (u16, u16), level: usize) {
        let side = 1u16 << level;
        let parent = (x & !(2 * side - 1), y & !(2 * side - 1));
        let quarters = [parent, (parent.0 + side, parent.1), (parent.0, parent.1 + side), (parent.0 + side, parent.1 + side)];
        let others_free = level < self.top && quarters.iter().filter(|&&corner| corner != (x, y)).all(|corner| self.free[level].contains(corner));
        if others_free {
            self.free[level].retain(|corner| !quarters.contains(corner));
            self.give(parent, level + 1);
        } else {
            self.free[level].push((x, y));
        }
    }
}

pub(super) struct Eye {
    pub(super) at: Vec3,
    pub(super) texel_spread: f32,
}

pub(super) struct FaceLamp {
    pub(super) number: u32,
    pub(super) at: Vec3,
    pub(super) spot: Option<(Vec3, f32)>,
    pub(super) reach: [f32; 6],
    pub(super) plain_face: u32,
    pub(super) ahead: Option<Vec3>,
}

impl FaceLamp {
    fn least_level(&self) -> usize {
        (self.plain_face / DETAIL_UNIT).max(1).ilog2() as usize + 1
    }
}

/// The level a lamp face wants and how far off it is, or nothing when its plain tile is fine enough. Bounds err
/// towards the larger square. Distance counts the lamp's as well as the face's: indoors the eye stands inside
/// the reach of a hundred lamps, and the near ones light what it sees close up.
fn wanted(eye: &Eye, lamp: &FaceLamp, face: usize, top: usize) -> Option<(usize, f32)> {
    let axis = Vec3::from(FACE_AXES[face][2]);
    if let Some((along, cone)) = lamp.spot
        && along.dot(axis).clamp(-1.0, 1.0).acos() - FACE_REACH > cone.clamp(-1.0, 1.0).acos()
    {
        return None;
    }
    let reach = lamp.reach[face];
    let to_eye = eye.at - lamp.at;
    let distance = to_eye.length();
    let off_face = if distance < 1.0 { 0.0 } else { ((to_eye.dot(axis) / distance).clamp(-1.0, 1.0).acos() - FACE_REACH).max(0.0) };
    let from_face = if off_face >= std::f32::consts::FRAC_PI_2 { distance } else { distance * off_face.sin() };
    let nearest = from_face.max(distance - reach);
    let seen_from = nearest.max(FINEST_TEXEL / eye.texel_spread);
    let side = 2.0 * reach.min(distance + seen_from) / (eye.texel_spread * seen_from);
    let level = (side / DETAIL_UNIT as f32).log2().round();
    if level < lamp.least_level() as f32 {
        return None;
    }
    Some(((level as usize).min(top), distance + nearest))
}

#[derive(Clone, Copy)]
struct Want {
    lamp: u32,
    face: u8,
    level: usize,
    least: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Sight {
    Ahead,
    Seen,
}

struct Held {
    slot: FaceSlot,
    wanted_at: u32,
    level: usize,
    least: usize,
    sight: Sight,
}

const AHEAD_A_PICK: usize = 3;

/// How many levels under the largest a face may ask for by its rank among wanted faces, nearest first. Each
/// level's faces fill a quarter of the squares, so a crowded view grows coarser with distance.
fn levels_down(place: usize) -> usize {
    match place {
        0..16 => 0,
        16..80 => 1,
        80..336 => 2,
        _ => 3,
    }
}

pub(super) struct FaceBook {
    region: Region,
    held: Vec<Held>,
    places: FxHashMap<(u32, u8), usize>,
    picks: u32,
    ahead: Vec<(Want, f32)>,
}

impl FaceBook {
    pub(super) fn new(atlas_size: [u32; 2]) -> Self {
        Self { region: Region::new(atlas_size), held: Vec::new(), places: FxHashMap::default(), picks: 0, ahead: Vec::new() }
    }

    pub(super) fn slots(&self) -> Vec<FaceSlot> {
        self.held.iter().map(|held| held.slot).collect()
    }

    pub(super) fn take_square(&mut self, level: usize) -> Option<(u16, u16)> {
        self.take(level, Sight::Seen, &mut std::collections::VecDeque::new())
    }

    pub(super) fn give_square(&mut self, corner: (u16, u16), level: usize) {
        self.region.give(corner, level);
    }

    pub(super) fn top_level(&self) -> usize {
        self.region.top
    }

    fn held_mut(&mut self, lamp: u32, face: u8) -> Option<&mut Held> {
        let &at = self.places.get(&(lamp, face))?;
        self.held.get_mut(at)
    }

    fn hold(&mut self, held: Held) {
        self.places.insert((held.slot.lamp, held.slot.face), self.held.len());
        self.held.push(held);
    }

    fn let_go(&mut self, at: usize) -> Held {
        let gone = self.held.swap_remove(at);
        self.places.remove(&(gone.slot.lamp, gone.slot.face));
        if let Some(moved) = self.held.get(at) {
            self.places.insert((moved.slot.lamp, moved.slot.face), at);
        }
        gone
    }

    /// A square for a face. With none free it comes from faces held only for lying ahead, then the longest unwanted,
    /// then, for a face in sight, from a face holding more than it asks now, which goes back to `waiting`.
    /// A face ahead never takes a square wanted at this pick.
    fn take(&mut self, level: usize, by: Sight, waiting: &mut std::collections::VecDeque<Want>) -> Option<(u16, u16)> {
        loop {
            if let Some(corner) = self.region.take(level) {
                return Some(corner);
            }
            let unwanted = self.held.iter().enumerate().filter(|(_, held)| held.wanted_at < self.picks || (by == Sight::Seen && held.sight == Sight::Ahead)).min_by_key(|(_, held)| (held.sight, held.wanted_at));
            let spare = || self.held.iter().enumerate().filter(|(_, held)| usize::from(held.slot.level) > held.level).max_by_key(|(_, held)| usize::from(held.slot.level) - held.level);
            let (at, asks_again) = match (unwanted, by) {
                (Some((at, _)), _) => (at, None),
                (None, Sight::Seen) => spare().map(|(at, held)| (at, Some(Want { lamp: held.slot.lamp, face: held.slot.face, level: held.level, least: held.least })))?,
                (None, Sight::Ahead) => return None,
            };
            let gone = self.let_go(at).slot;
            self.region.give((gone.x, gone.y), usize::from(gone.level));
            waiting.extend(asks_again);
        }
    }

    pub(super) fn pick(&mut self, eye: &Eye, lamps: impl Iterator<Item = FaceLamp>, budget: u32) {
        self.picks += 1;
        let top = self.region.top;
        let mut ahead = std::mem::take(&mut self.ahead);
        ahead.clear();
        let mut wants: Vec<(Want, f32)> = Vec::new();
        for lamp in lamps {
            let least = lamp.least_level();
            let from = Eye { at: lamp.ahead.unwrap_or(eye.at), ..*eye };
            let into = if lamp.ahead.is_some() { &mut ahead } else { &mut wants };
            into.extend((0u8..6).filter_map(|face| wanted(&from, &lamp, usize::from(face), top).map(|(level, nearest)| (Want { lamp: lamp.number, face, level, least }, nearest))));
        }
        wants.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut waiting = std::collections::VecDeque::new();
        let picks = self.picks;
        for (place, &(want, _)) in wants.iter().enumerate() {
            let level = want.level.min(top.saturating_sub(levels_down(place)));
            if level < want.least {
                continue;
            }
            match self.held_mut(want.lamp, want.face) {
                Some(held) => {
                    (held.wanted_at, held.level, held.sight) = (picks, level, Sight::Seen);
                    if usize::from(held.slot.level) < level {
                        waiting.push_back(Want { level, least: usize::from(held.slot.level) + 1, ..want });
                    }
                }
                None => waiting.push_back(Want { level, ..want }),
            }
        }
        let mut spent = 0;
        while let Some(want) = waiting.pop_front() {
            let side = DETAIL_UNIT << want.level;
            if spent + side * side > budget {
                break;
            }
            let Some((level, (x, y))) = (want.least..=want.level).rev().find_map(|level| Some((level, self.take(level, Sight::Seen, &mut waiting)?))) else { continue };
            let side = DETAIL_UNIT << level;
            spent += side * side;
            let slot = FaceSlot { lamp: want.lamp, face: want.face, x, y, level: level as u8 };
            match self.held_mut(want.lamp, want.face) {
                Some(held) => {
                    let smaller = std::mem::replace(&mut held.slot, slot);
                    self.region.give((smaller.x, smaller.y), usize::from(smaller.level));
                }
                None => self.hold(Held { slot, wanted_at: picks, level: want.level, least: want.least, sight: Sight::Seen }),
            }
        }
        if waiting.is_empty() {
            ahead.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (place, (want, _)) in ahead.iter_mut().enumerate() {
                want.level = want.level.min(top.saturating_sub(levels_down(wants.len() + place)));
                if want.level >= want.least
                    && let Some(held) = self.held_mut(want.lamp, want.face).filter(|held| held.wanted_at < picks)
                {
                    (held.wanted_at, held.level, held.sight) = (picks, want.level, Sight::Ahead);
                }
            }
            let mut given = 0;
            for &(want, _) in &ahead {
                if given == AHEAD_A_PICK {
                    break;
                }
                if want.level < want.least || self.held_mut(want.lamp, want.face).is_some() {
                    continue;
                }
                let side = DETAIL_UNIT << want.level;
                if spent + side * side > budget {
                    break;
                }
                let Some((level, (x, y))) = (want.least..=want.level).rev().find_map(|level| Some((level, self.take(level, Sight::Ahead, &mut waiting)?))) else { continue };
                let side = DETAIL_UNIT << level;
                spent += side * side;
                given += 1;
                let slot = FaceSlot { lamp: want.lamp, face: want.face, x, y, level: level as u8 };
                self.hold(Held { slot, wanted_at: picks, level: want.level, least: want.least, sight: Sight::Ahead });
            }
        }
        self.ahead = ahead;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::shader_const;

    const SIDE: u32 = 8192;

    #[test]
    fn the_shader_reads_a_detail_corner_in_the_same_unit() {
        assert_eq!(shader_const(include_str!("../lamp_shadow.wesl"), "DETAIL_UNIT"), f64::from(DETAIL_UNIT));
    }

    fn lamp(number: u32, at: Vec3) -> FaceLamp {
        FaceLamp { number, at, spot: None, reach: [1600.0; 6], plain_face: 64, ahead: None }
    }

    fn eye(at: Vec3) -> Eye {
        Eye { at, texel_spread: 2.0 / 1440.0 }
    }

    fn overlap(a: FaceSlot, b: FaceSlot) -> bool {
        let span = |slot: FaceSlot| (u32::from(slot.x), u32::from(slot.y), u32::from(slot.x) + (1 << slot.level), u32::from(slot.y) + (1 << slot.level));
        let ((ax, ay, ax2, ay2), (bx, by, bx2, by2)) = (span(a), span(b));
        ax < bx2 && bx < ax2 && ay < by2 && by < ay2
    }

    #[test]
    fn squares_split_from_the_largest_and_merge_back_into_it() {
        let mut region = Region::new([SIDE; 2]);
        assert_eq!((region.top, region.free[3].len()), (3, 64));
        let taken: Vec<_> = (0..4).filter_map(|_| region.take(2)).collect();
        assert_eq!((taken.len(), region.free[3].len(), region.free[2].len()), (4, 63, 0));
        for corner in taken {
            region.give(corner, 2);
        }
        assert_eq!((region.free[3].len(), region.free[2].len()), (64, 0));
        assert!(region.take(4).is_none());
    }

    #[test]
    fn squares_lie_in_the_atlas_last_quarter() {
        let mut region = Region::new([SIDE; 2]);
        while let Some((x, y)) = region.take(0) {
            assert!(u32::from(x) < SIDE / DETAIL_UNIT && (SIDE / DETAIL_UNIT * 3 / 4..SIDE / DETAIL_UNIT).contains(&u32::from(y)));
        }
    }

    #[test]
    fn a_short_atlas_keeps_the_squares_under_its_tiles() {
        let height = atlas_height(SIDE, 1000);
        assert_eq!(height, 1024 + SIDE / 4);
        assert_eq!(atlas_height(SIDE, SIDE * 3 / 4), SIDE);
        let mut region = Region::new([SIDE, height]);
        while let Some((x, y)) = region.take(0) {
            assert!(u32::from(x) < SIDE / DETAIL_UNIT && (1024 / DETAIL_UNIT..height / DETAIL_UNIT).contains(&u32::from(y)));
        }
    }

    #[test]
    fn a_face_that_lights_only_what_is_close_to_the_lamp_gets_no_square() {
        let under_a_ceiling = FaceLamp { reach: [1600.0, 1600.0, 16.0, 1600.0, 1600.0, 1600.0], ..lamp(7, Vec3::ZERO) };
        let mut book = FaceBook::new([SIDE; 2]);
        book.pick(&eye(Vec3::new(300.0, -200.0, 0.0)), [under_a_ceiling].into_iter(), u32::MAX);
        let faces: Vec<u8> = book.slots().iter().map(|slot| slot.face).collect();
        assert!(faces.contains(&3) && !faces.contains(&2), "{faces:?}");
    }

    #[test]
    fn a_lamp_whose_plain_tile_is_fine_enough_gets_no_square() {
        let mut book = FaceBook::new([SIDE; 2]);
        book.pick(&eye(Vec3::ZERO), [FaceLamp { plain_face: 512, ..lamp(0, Vec3::new(200.0, 0.0, 0.0)) }].into_iter(), u32::MAX);
        assert!(book.slots().is_empty());
    }

    #[test]
    fn a_near_lamp_gets_finer_faces_than_a_far_one() {
        let mut book = FaceBook::new([SIDE; 2]);
        book.pick(&eye(Vec3::ZERO), [lamp(0, Vec3::new(200.0, 0.0, 0.0)), lamp(1, Vec3::new(20000.0, 0.0, 0.0))].into_iter(), u32::MAX);
        let finest = |lamp: u32| book.slots().iter().filter(|slot| slot.lamp == lamp).map(|slot| slot.level).max();
        assert_eq!(finest(0), Some(3));
        assert!(finest(1) < finest(0), "{:?}", finest(1));
    }

    #[test]
    fn a_spot_wants_no_face_its_cone_misses() {
        let down = FaceLamp { spot: Some((Vec3::NEG_Y, 0.9)), ..lamp(0, Vec3::ZERO) };
        let mut book = FaceBook::new([SIDE; 2]);
        book.pick(&eye(Vec3::new(0.0, -100.0, 0.0)), [down].into_iter(), u32::MAX);
        assert_eq!(book.slots().iter().map(|slot| slot.face).collect::<Vec<_>>(), [3]);
    }

    #[test]
    fn no_two_faces_share_texels_and_a_pick_spends_no_more_than_its_budget() {
        let mut book = FaceBook::new([SIDE; 2]);
        let lamps = || (0..40).map(|number| lamp(number, Vec3::new(number as f32 * 150.0, 0.0, 0.0)));
        book.pick(&eye(Vec3::ZERO), lamps(), 1 << 20);
        let first: u32 = book.slots().iter().map(|slot| slot.side() * slot.side()).sum();
        assert!(first > 0 && first <= 1 << 20, "{first}");
        for _ in 0..40 {
            book.pick(&eye(Vec3::ZERO), lamps(), u32::MAX);
        }
        let slots = book.slots();
        for (index, &a) in slots.iter().enumerate() {
            assert!(slots[index + 1..].iter().all(|&b| !overlap(a, b) && (a.lamp, a.face) != (b.lamp, b.face)), "{a:?}");
        }
    }

    #[test]
    fn a_face_out_of_use_keeps_its_square_until_another_needs_it() {
        let mut book = FaceBook::new([SIDE; 2]);
        book.pick(&eye(Vec3::ZERO), [lamp(0, Vec3::new(200.0, 0.0, 0.0))].into_iter(), u32::MAX);
        let before = book.slots();
        book.pick(&eye(Vec3::ZERO), std::iter::empty(), u32::MAX);
        assert_eq!(book.slots(), before);
        for number in 1..=12 {
            book.pick(&eye(Vec3::ZERO), [lamp(number, Vec3::new(0.0, 0.0, 200.0))].into_iter(), u32::MAX);
        }
        assert!(book.slots().iter().all(|slot| slot.lamp != 0));
        assert_eq!(book.slots().len(), 64);
    }

    #[test]
    fn a_crowded_view_gives_the_nearest_faces_the_largest_squares_and_the_rest_smaller_ones() {
        let mut book = FaceBook::new([SIDE; 2]);
        let crowd = || (0..100).map(|number| lamp(number, Vec3::new(100.0 + number as f32 * 10.0, 0.0, 0.0)));
        for _ in 0..4 {
            book.pick(&eye(Vec3::ZERO), crowd(), u32::MAX);
        }
        let count = |level: u8| book.slots().iter().filter(|slot| slot.level == level).count();
        assert_eq!((count(3), count(2), count(1)), (16, 64, 256));
        assert!(book.slots().iter().filter(|slot| slot.level == 3).all(|slot| slot.lamp < 16));
    }

    #[test]
    fn a_face_that_fell_behind_hands_its_large_square_to_a_nearer_one() {
        let mut book = FaceBook::new([SIDE; 2]);
        let crowd = || (0..100).map(|number| lamp(number, Vec3::new(100.0 + number as f32 * 10.0, 0.0, 0.0)));
        for _ in 0..4 {
            book.pick(&eye(Vec3::ZERO), crowd(), u32::MAX);
        }
        for at in [1200.0, 600.0] {
            for _ in 0..4 {
                book.pick(&eye(Vec3::new(at, 0.0, 0.0)), crowd(), u32::MAX);
            }
        }
        let slots = book.slots();
        let largest: Vec<u32> = slots.iter().filter(|slot| slot.level == 3).map(|slot| slot.lamp).collect();
        assert!(largest.iter().filter(|lamp| (45..=55).contains(*lamp)).count() >= 16, "{largest:?}");
        for (index, &a) in slots.iter().enumerate() {
            assert!(slots[index + 1..].iter().all(|&b| !overlap(a, b)), "{a:?}");
        }
    }

    #[test]
    fn a_seen_face_takes_a_square_from_a_face_ahead_before_one_longer_unwanted() {
        let mut book = FaceBook::new([SIDE; 2]);
        book.picks = 2;
        let mut number = 100;
        while let Some((x, y)) = book.region.take(3) {
            let (sight, wanted_at) = if number % 2 == 0 { (Sight::Seen, 1) } else { (Sight::Ahead, 2) };
            book.hold(Held { slot: FaceSlot { lamp: number, face: 0, x, y, level: 3 }, wanted_at, level: 3, least: 1, sight });
            number += 1;
        }
        book.pick(&eye(Vec3::ZERO), [lamp(0, Vec3::new(200.0, 0.0, 0.0))].into_iter(), u32::MAX);
        let slots = book.slots();
        assert!(slots.iter().any(|slot| slot.lamp == 0));
        assert_eq!(slots.iter().filter(|slot| slot.lamp >= 100 && slot.lamp % 2 == 0).count(), 32);
    }

    #[test]
    fn the_places_of_held_faces_agree_with_the_list_after_gains_and_losses() {
        let agrees = |book: &FaceBook| {
            book.places.len() == book.held.len() && book.held.iter().enumerate().all(|(at, held)| book.places.get(&(held.slot.lamp, held.slot.face)) == Some(&at))
        };
        let mut book = FaceBook::new([SIDE; 2]);
        book.pick(&eye(Vec3::ZERO), [lamp(0, Vec3::new(200.0, 0.0, 0.0))].into_iter(), u32::MAX);
        assert!(agrees(&book) && !book.held.is_empty());
        for number in 1..=12 {
            book.pick(&eye(Vec3::ZERO), [lamp(number, Vec3::new(0.0, 0.0, 200.0))].into_iter(), u32::MAX);
            assert!(agrees(&book), "{number}");
        }
        let gone = book.let_go(0).slot;
        assert!(agrees(&book) && book.held_mut(gone.lamp, gone.face).is_none());
    }

    #[test]
    fn a_packed_square_is_exact_as_a_float_and_never_0() {
        let slot = FaceSlot { lamp: 0, face: 0, x: 255, y: 255, level: 4 };
        assert_eq!(slot.packed() as u32, 1 + 255 + (255 << 8) + (4 << 16));
        assert_eq!(FaceSlot { x: 0, y: 0, level: 0, ..slot }.packed(), 1.0);
    }
}
