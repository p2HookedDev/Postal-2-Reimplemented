//! Which movers get a tile from which lamps this frame, and the squares of the atlas their tiles hold.

use std::ops::Range;

use bevy::asset::{AssetEvent, AssetId, Assets};
use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::{InheritedVisibility, RenderLayers, Visibility};
use bevy::camera::{Camera, Projection};
use bevy::ecs::prelude::{ChildOf, DetectChanges, DetectChangesMut, Entity, Local, MessageReader, Or, Query, Ref, Res, ResMut, With};
use bevy::math::{Mat4, Vec2, Vec3};
use bevy::mesh::{Mesh, Mesh3d};
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::time::Time;
use bevy::transform::components::{GlobalTransform, Transform};
use p2_math::Vector;
use rustc_hash::FxHashMap;

use super::faces::{DETAIL_UNIT, FINEST_TEXEL, FaceBook};
use super::grounding::{Capsule, GRID_COLUMNS, grounding_row};
use super::movers::{Drawn, MOST_MOVER_TILES, MoverCaster, MoverTile, MoverTiles, TILE_MARGIN, WIDEST_TILE};
use super::tile_grid::tile_grid_rows;
use super::{DetailSquares, Lamp, LampAtlas, MOVER_FADE_SECONDS, Placed, lamp_falloff, lamp_quality, scene_eye};
use crate::components::{ActorModel, BaseModel, FollowActor, FollowBone, MOVER_LAYER, RESTING_PROP_LAYER, SceneView};
use p2_host::display::{MODERN_LAMP_SHADOWS, Settings};
use crate::level_collision::{LevelCollision, StaticCollision};
use crate::modern::StillProp;
use crate::view::ShownScene;

const MOVER_REACH: f32 = 4096.0;
const MOST_MOVERS: usize = 256;
const WALL_CHECKED: usize = 3;
const WALL_SLOTS: usize = 4;
const RECHECK_UNITS: f32 = 16.0;
const RECHECK_SECONDS: f32 = 0.25;
/// A lamp's fitting often holds it inside its own collision, so the trace stops this short of it.
const LAMP_CLEARANCE: f32 = 16.0;
const SWITCH_FACTOR: f32 = 1.5;
const SWITCH_SECONDS: f32 = 0.3;
const TAKE_EXTRA: f32 = 0.25;
const KEEP_EXTRA: f32 = 0.15;
const MOST_EXTRAS: usize = 2;
const GROW_SECONDS: f32 = 0.25;
const SHRINK_SECONDS: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Lit {
    lamp: u32,
    at: Vec3,
    range: f32,
    strength: f32,
    spot: Option<(Vec3, f32)>,
    score: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Square {
    /// In [`DETAIL_UNIT`]s.
    corner: (u16, u16),
    level: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fading {
    In,
    Out,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct LevelHold {
    level: usize,
    growing: f32,
    shrinking: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Primary {
    lamp: Option<u32>,
    /// The lamp that outscores it enough, and for how long it has.
    rival: Option<(u32, f32)>,
}

#[derive(Clone, Copy)]
struct WallCheck {
    lamp: u32,
    from: Vec3,
    at: f32,
    clear: bool,
}

struct HeldTile {
    lamp: u32,
    lamp_at: Vec3,
    lamp_radius: f32,
    axis: Vec3,
    tan_half: f32,
    near: f32,
    hold: LevelHold,
    square: Option<Square>,
    fade: f32,
    fading: Fading,
    /// The square, axis and tangent it was drawn with last frame.
    last: Option<(Square, Vec3, f32)>,
}

#[derive(Default)]
struct MoverState {
    primary: Primary,
    extras: [Option<u32>; MOST_EXTRAS],
    walls: [Option<WallCheck>; WALL_SLOTS],
    tiles: Vec<HeldTile>,
    near: bool,
    resting: bool,
    eye_distance: f32,
}

struct Bounds {
    min: Vec3,
    max: Vec3,
    resting: bool,
}

impl Bounds {
    fn sphere(&self) -> (Vec3, f32) {
        ((self.min + self.max) * 0.5, (self.max - self.min).length() * 0.5)
    }
}

/// A column's side in world units, unless the movers spread wider than `LAMP_COLUMNS_MOST` of them.
const LAMP_COLUMN: f32 = 512.0;
const LAMP_COLUMNS_MOST: f32 = 64.0;

/// The lamps that may reach each column of a grid in x and z over the near movers' middles, each column's list in the
/// lamps' own order, so a mover scores them as it would score the whole list.
#[derive(Default)]
struct LampGrid {
    low: Vec2,
    side: f32,
    across: usize,
    down: usize,
    starts: Vec<u32>,
    listed: Vec<u32>,
    next_slot: Vec<u32>,
}

impl LampGrid {
    fn column(&self, at: f32, low: f32, count: usize) -> usize {
        ((at - low) / self.side).floor().clamp(0.0, count.saturating_sub(1) as f32) as usize
    }

    /// The columns a lamp may reach, with the same slack as `may_light`.
    fn reach(&self, lamp: &Lit) -> (Range<usize>, Range<usize>) {
        let reach = lamp.range.abs() * 1.001 + 1.0;
        let xs = self.column(lamp.at.x - reach, self.low.x, self.across)..self.column(lamp.at.x + reach, self.low.x, self.across) + 1;
        let zs = self.column(lamp.at.z - reach, self.low.y, self.down)..self.column(lamp.at.z + reach, self.low.y, self.down) + 1;
        (xs, zs)
    }

    fn build(&mut self, lamps: &[Lit], low: Vec3, high: Vec3) {
        self.low = Vec2::new(low.x, low.z);
        let size = Vec2::new(high.x - low.x, high.z - low.z).max(Vec2::ZERO);
        self.side = LAMP_COLUMN.max(size.max_element() / LAMP_COLUMNS_MOST);
        (self.across, self.down) = ((size.x / self.side) as usize + 1, (size.y / self.side) as usize + 1);
        self.starts.clear();
        self.starts.resize(self.across * self.down + 1, 0);
        for lamp in lamps {
            let (xs, zs) = self.reach(lamp);
            for z in zs {
                for x in xs.clone() {
                    self.starts[z * self.across + x + 1] += 1;
                }
            }
        }
        for column in 1..self.starts.len() {
            self.starts[column] += self.starts[column - 1];
        }
        self.listed.clear();
        self.listed.resize(self.starts.last().map_or(0, |&total| total as usize), 0);
        self.next_slot.clone_from(&self.starts);
        for (index, lamp) in lamps.iter().enumerate() {
            let (xs, zs) = self.reach(lamp);
            for z in zs {
                for x in xs.clone() {
                    let slot = &mut self.next_slot[z * self.across + x];
                    self.listed[*slot as usize] = index as u32;
                    *slot += 1;
                }
            }
        }
    }

    /// The lamps that may reach `middle`, by their place in the list the grid was built from.
    fn near(&self, middle: Vec3) -> &[u32] {
        let column = self.column(middle.z, self.low.y, self.down) * self.across + self.column(middle.x, self.low.x, self.across);
        match (self.starts.get(column), self.starts.get(column + 1)) {
            (Some(&start), Some(&end)) => &self.listed[start as usize..end as usize],
            _ => &[],
        }
    }
}

#[derive(Default)]
pub(super) struct PickState {
    made: u32,
    movers: FxHashMap<Entity, MoverState>,
    bounds: FxHashMap<Entity, Bounds>,
    near: Vec<(f32, Entity)>,
    /// This frame's casting meshes and their movers, in the meshes' order.
    casting: Vec<(Entity, Entity)>,
    lamps: Vec<Lit>,
    lamp_grid: LampGrid,
    scored: Vec<Lit>,
    order: Vec<(f32, Entity)>,
    numbers: FxHashMap<Entity, u32>,
    grid_made_from: Vec<u32>,
    skins: Skins,
}

#[derive(Default)]
struct BuiltSkin {
    joints: Vec<Entity>,
    inverse: AssetId<SkinnedMeshInverseBindposes>,
    matrices: Vec<Mat4>,
}

/// Each skinned caster's joint matrices as the last run and this one built them, by the caster.
#[derive(Default)]
struct Skins {
    last: FxHashMap<Entity, BuiltSkin>,
    now: FxHashMap<Entity, BuiltSkin>,
}

fn lamp_score(lamp: &Lit, middle: Vec3) -> f32 {
    lamp.strength * lamp_falloff(lamp.at, lamp.range, lamp.spot, middle)
}

/// Whether the lamp may score above 0 somewhere in the box; the slack covers rounding, so it never says no to such a lamp.
fn may_light(lamp: &Lit, low: Vec3, high: Vec3) -> bool {
    let gap = (low - lamp.at).max(lamp.at - high).max(Vec3::ZERO);
    gap.length_squared() <= (lamp.range.abs() * 1.001 + 1.0).powi(2)
}

fn bits<const N: usize>(floats: [f32; N]) -> [u32; N] {
    floats.map(f32::to_bits)
}

fn tile_key(tile: &MoverTile) -> (u32, u32, u32, u32, u32, Drawn, [u32; 10]) {
    let MoverTile { mover, lamp, lamp_at, lamp_radius, axis, tan_half, near, x, y, size, fade, drawn } = *tile;
    (mover, lamp, x, y, size, drawn, bits([lamp_at.x, lamp_at.y, lamp_at.z, lamp_radius, axis.x, axis.y, axis.z, tan_half, near, fade]))
}

fn caster_key(caster: &MoverCaster) -> (AssetId<Mesh>, [u32; 16], Option<Range<u32>>, u32) {
    let MoverCaster { mesh, world_from_mesh, joints, mover } = caster;
    (*mesh, bits(world_from_mesh.to_cols_array()), joints.clone(), *mover)
}

/// Moves `next` into `held` when they differ, and says whether they did.
fn take_if_new<T>(held: &mut Vec<T>, next: &mut Vec<T>, same: impl Fn(&T, &T) -> bool) -> bool {
    if held.len() == next.len() && held.iter().zip(next.iter()).all(|(a, b)| same(a, b)) {
        return false;
    }
    std::mem::swap(held, next);
    true
}

/// The tile's axis, the tangent of its half angle and the lamp's distance to the mover's middle.
fn tile_view(lamp_at: Vec3, middle: Vec3, radius: f32) -> (Vec3, f32, f32) {
    let to_mover = middle - lamp_at;
    let distance = to_mover.length();
    let tan_half = (TILE_MARGIN * radius / (distance * distance - radius * radius).max(1.0).sqrt()).min(WIDEST_TILE);
    (to_mover.normalize_or(Vec3::NEG_Y), tan_half, distance)
}

/// The level whose texel on the mover is no larger than `texel` world units.
fn tile_level(tan_half: f32, distance: f32, texel: f32, finest: usize) -> usize {
    let texels = 2.0 * tan_half * distance / texel;
    (texels / DETAIL_UNIT as f32).log2().round().clamp(0.0, finest as f32) as usize
}

fn hold_level(hold: LevelHold, wanted: usize, dt: f32) -> LevelHold {
    let (growing, shrinking) = match wanted.cmp(&hold.level) {
        std::cmp::Ordering::Greater => (hold.growing + dt, 0.0),
        std::cmp::Ordering::Less => (0.0, hold.shrinking + dt),
        std::cmp::Ordering::Equal => (0.0, 0.0),
    };
    if growing >= GROW_SECONDS || shrinking >= SHRINK_SECONDS {
        LevelHold { level: wanted, ..LevelHold::default() }
    } else {
        LevelHold { level: hold.level, growing, shrinking }
    }
}

/// `best` is the best scoring lamp and `held_score` what the held primary scores now.
fn hold_primary(held: Primary, best: Option<(u32, f32)>, held_score: f32, dt: f32) -> Primary {
    let Some((best, best_score)) = best.filter(|&(_, score)| score > 0.0) else { return Primary::default() };
    match held.lamp {
        Some(lamp) if lamp != best && best_score >= SWITCH_FACTOR * held_score => {
            let time = match held.rival {
                Some((rival, time)) if rival == best => time + dt,
                _ => dt,
            };
            if time >= SWITCH_SECONDS { Primary { lamp: Some(best), rival: None } } else { Primary { lamp: Some(lamp), rival: Some((best, time)) } }
        }
        Some(lamp) => Primary { lamp: Some(lamp), rival: None },
        None => Primary { lamp: Some(best), rival: None },
    }
}

/// `scores` best first; held extras are kept before new ones are taken.
fn pick_extras(held: [Option<u32>; MOST_EXTRAS], scores: impl Iterator<Item = (u32, f32)> + Clone, (primary, primary_score): (u32, f32), most: usize) -> [Option<u32>; MOST_EXTRAS] {
    let was_held = |lamp: u32| held.contains(&Some(lamp));
    let others = scores.filter(move |&(lamp, score)| lamp != primary && score > 0.0);
    let kept = others.clone().filter(|&(lamp, score)| was_held(lamp) && score >= KEEP_EXTRA * primary_score);
    let taken = others.filter(|&(lamp, score)| !was_held(lamp) && score >= TAKE_EXTRA * primary_score);
    let mut picked = [None; MOST_EXTRAS];
    for (slot, (lamp, _)) in picked.iter_mut().take(most).zip(kept.chain(taken)) {
        *slot = Some(lamp);
    }
    picked
}

fn faded(fade: f32, fading: Fading, dt: f32) -> f32 {
    let step = dt / MOVER_FADE_SECONDS;
    match fading {
        Fading::In => (fade + step).min(1.0),
        Fading::Out => (fade - step).max(0.0),
    }
}

fn fresh(check: &WallCheck, middle: Vec3, now: f32) -> bool {
    check.from.distance_squared(middle) < RECHECK_UNITS * RECHECK_UNITS && now - check.at < RECHECK_SECONDS
}

fn world_min_max(aabb: &Aabb, placed: &GlobalTransform) -> (Vec3, Vec3) {
    let affine = placed.affine();
    let middle = affine.transform_point3a(aabb.center);
    let axes = affine.matrix3;
    let reach = axes.x_axis.abs() * aabb.half_extents.x + axes.y_axis.abs() * aabb.half_extents.y + axes.z_axis.abs() * aabb.half_extents.z;
    (Vec3::from(middle - reach), Vec3::from(middle + reach))
}

/// A mover's middle inside solid traces as clear, as the actor lights do.
fn lamp_reaches(level: &StaticCollision, origin: Vec3, lamp_at: Vec3, middle: Vec3) -> bool {
    let model = &level.model;
    let unreal = |at: Vec3| Vector::from(p2_bevy_assets::to_bevy(at - origin));
    let from = unreal(middle);
    if p2_collision::point_leaf(model, from).is_none() {
        return true;
    }
    let to_lamp = unreal(lamp_at) - from;
    let distance = to_lamp.dot(to_lamp).sqrt();
    if distance <= LAMP_CLEARANCE {
        return true;
    }
    level.light_passes(from, from + to_lamp * (1.0 - LAMP_CLEARANCE / distance))
}

/// A square of `level`, else the largest smaller one free; a held square stays while no larger one is free.
fn resize(book: &mut FaceBook, held: Option<Square>, level: usize) -> Option<Square> {
    match held {
        Some(square) if square.level == level => Some(square),
        Some(square) if square.level < level => match book.take_square(level) {
            Some(corner) => {
                book.give_square(square.corner, square.level);
                Some(Square { corner, level })
            }
            None => Some(square),
        },
        held => {
            if let Some(square) = held {
                book.give_square(square.corner, square.level);
            }
            (0..=level).rev().find_map(|level| Some(Square { corner: book.take_square(level)?, level }))
        }
    }
}

/// Matrices built at the last run are kept only while no joint has changed since, which the joints' change ticks
/// can tell because that run is this system's last.
fn skin_joints(
    caster: Entity,
    skin: &SkinnedMesh,
    bindposes: &Assets<SkinnedMeshInverseBindposes>,
    joints: &Query<Ref<GlobalTransform>>,
    skins: &mut Skins,
    into: &mut Vec<Mat4>,
) -> Option<Range<u32>> {
    let last = skins.last.get_mut(&caster).map(std::mem::take);
    let still = last.as_ref().is_some_and(|built| {
        built.inverse == skin.inverse_bindposes.id()
            && built.joints == skin.joints
            && skin.joints.iter().all(|&joint| joints.get(joint).is_ok_and(|placed| !placed.is_changed()))
    });
    let mut built = last.unwrap_or_default();
    if !still {
        let inverse = bindposes.get(&skin.inverse_bindposes)?;
        built.matrices.clear();
        for (&joint, bind) in skin.joints.iter().zip(inverse.iter()) {
            built.matrices.push(joints.get(joint).ok()?.to_matrix() * *bind);
        }
        built.joints.clone_from(&skin.joints);
        built.inverse = skin.inverse_bindposes.id();
    }
    let start = into.len();
    into.extend_from_slice(&built.matrices);
    skins.now.insert(caster, built);
    Some(start as u32..into.len() as u32)
}

impl MoverState {
    fn lamp_clear(&mut self, lamp: &Lit, middle: Vec3, now: f32, level: Option<(&StaticCollision, Vec3)>) -> bool {
        let age = |check: &Option<WallCheck>| check.map_or(f32::MIN, |check| check.at);
        let slot = self.walls.iter().position(|check| check.is_some_and(|check| check.lamp == lamp.lamp))
            .unwrap_or_else(|| (0..WALL_SLOTS).min_by(|&a, &b| age(&self.walls[a]).total_cmp(&age(&self.walls[b]))).unwrap_or(0));
        if let Some(check) = self.walls[slot].filter(|check| check.lamp == lamp.lamp && fresh(check, middle, now)) {
            return check.clear;
        }
        let clear = level.is_none_or(|(level, origin)| lamp_reaches(level, origin, lamp.at, middle));
        self.walls[slot] = Some(WallCheck { lamp: lamp.lamp, from: middle, at: now, clear });
        clear
    }
}

impl PickState {
    /// Each near mover's primary lamp and extras, and a tile begun for each lamp newly wanted.
    fn choose_lamps(&mut self, eye: Vec3, now: f32, dt: f32, walls: Option<(&StaticCollision, Vec3)>, most_extras: usize) {
        self.near.clear();
        self.near.extend(self.bounds.iter().map(|(&root, bounds)| (bounds.sphere().0.distance(eye), root)).filter(|&(distance, _)| distance <= MOVER_REACH));
        self.near.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        self.near.truncate(MOST_MOVERS);
        let (low, high) = self.near.iter().filter_map(|(_, root)| self.bounds.get(root)).map(|bounds| bounds.sphere().0).fold((Vec3::MAX, Vec3::MIN), |(low, high), middle| (low.min(middle), high.max(middle)));
        self.lamps.retain(|lamp| may_light(lamp, low, high));
        self.lamp_grid.build(&self.lamps, low, high);

        for mover in self.movers.values_mut() {
            mover.near = false;
        }
        for &(_, root) in &self.near {
            let Some(bounds) = self.bounds.get(&root) else { continue };
            let middle = bounds.sphere().0;
            let mover = self.movers.entry(root).or_default();
            mover.near = true;
            self.scored.clear();
            self.scored.extend(self.lamp_grid.near(middle).iter().filter_map(|&index| {
                let lamp = self.lamps.get(index as usize)?;
                let score = lamp_score(lamp, middle);
                (score > 0.0).then_some(Lit { score, ..*lamp })
            }));
            self.scored.sort_unstable_by(|a, b| b.score.total_cmp(&a.score));
            let mut walled = false;
            for lamp in self.scored.iter_mut().take(WALL_CHECKED) {
                if !mover.lamp_clear(lamp, middle, now, walls) {
                    lamp.score = 0.0;
                    walled = true;
                }
            }
            if walled {
                self.scored.sort_unstable_by(|a, b| b.score.total_cmp(&a.score));
            }
            let score_of = |lamp: Option<u32>| lamp.and_then(|lamp| self.scored.iter().find(|lit| lit.lamp == lamp)).map_or(0.0, |lit| lit.score);
            mover.primary = hold_primary(mover.primary, self.scored.first().map(|lit| (lit.lamp, lit.score)), score_of(mover.primary.lamp), dt);
            mover.extras = match mover.primary.lamp {
                Some(primary) => pick_extras(mover.extras, self.scored.iter().map(|lit| (lit.lamp, lit.score)), (primary, score_of(Some(primary))), most_extras),
                None => [None; MOST_EXTRAS],
            };
            let wanted = [mover.primary.lamp, mover.extras[0], mover.extras[1]];
            for tile in &mut mover.tiles {
                tile.fading = if wanted.contains(&Some(tile.lamp)) { Fading::In } else { Fading::Out };
            }
            for lamp in wanted.into_iter().flatten() {
                if mover.tiles.iter().any(|tile| tile.lamp == lamp) {
                    continue;
                }
                let Some(lit) = self.scored.iter().find(|lit| lit.lamp == lamp) else { continue };
                mover.tiles.push(HeldTile { lamp, lamp_at: lit.at, lamp_radius: lit.range, axis: Vec3::NEG_Y, tan_half: 0.0, near: 0.0, hold: LevelHold::default(), square: None, fade: 0.0, fading: Fading::In, last: None });
            }
        }
    }

    /// Every tile's view, square and fade for this frame. Whether any square was taken or given back.
    fn place_tiles(&mut self, book: &mut FaceBook, eye: Vec3, spread: f32, finest: usize, dt: f32) -> bool {
        let mut squares_changed = false;
        for (root, mover) in &mut self.movers {
            let Some(bounds) = self.bounds.get(root) else {
                for square in mover.tiles.iter().filter_map(|tile| tile.square) {
                    book.give_square(square.corner, square.level);
                    squares_changed = true;
                }
                mover.tiles.clear();
                continue;
            };
            if !mover.near {
                (mover.primary, mover.extras) = (Primary::default(), [None; MOST_EXTRAS]);
                mover.tiles.iter_mut().for_each(|tile| tile.fading = Fading::Out);
            }
            let (middle, radius) = bounds.sphere();
            mover.resting = bounds.resting;
            mover.eye_distance = (middle.distance(eye) - radius).max(0.0);
            let texel = FINEST_TEXEL.max(spread * mover.eye_distance);
            for tile in &mut mover.tiles {
                let (axis, tan_half, distance) = tile_view(tile.lamp_at, middle, radius);
                (tile.axis, tile.tan_half, tile.near) = (axis, tan_half, (distance - radius).max(0.0));
                if tile.fading == Fading::In {
                    let wanted = tile_level(tile.tan_half, distance, texel, finest);
                    tile.hold = if tile.square.is_some() { hold_level(tile.hold, wanted, dt) } else { LevelHold { level: wanted, ..LevelHold::default() } };
                    let square = resize(book, tile.square, tile.hold.level);
                    squares_changed |= square != tile.square;
                    tile.square = square;
                }
                if tile.square.is_some() {
                    tile.fade = faded(tile.fade, tile.fading, dt);
                }
            }
            mover.tiles.retain(|tile| {
                let ended = tile.fading == Fading::Out && (tile.square.is_none() || tile.fade <= 0.0);
                if ended && let Some(square) = tile.square {
                    book.give_square(square.corner, square.level);
                    squares_changed = true;
                }
                !ended
            });
        }
        self.movers.retain(|_, mover| mover.near || !mover.tiles.is_empty());
        squares_changed
    }

    /// The tiles ordered by lamp, the nearest movers' first when there are too many, and each mover's number.
    fn fill_tiles(&mut self, out: &mut MoverTiles) {
        self.order.clear();
        self.order.extend(self.movers.iter().filter(|(_, mover)| !mover.tiles.is_empty()).map(|(&root, mover)| (mover.eye_distance, root)));
        self.order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        out.tiles.clear();
        out.casters.clear();
        out.joints.clear();
        self.numbers.clear();
        for &(_, root) in &self.order {
            let Some(mover) = self.movers.get_mut(&root) else { continue };
            let number = self.numbers.len() as u32;
            for tile in &mut mover.tiles {
                let Some(square) = tile.square.filter(|_| out.tiles.len() < MOST_MOVER_TILES) else {
                    tile.last = None;
                    continue;
                };
                let drawn_with = (square, tile.axis, tile.tan_half);
                let drawn = if mover.resting && tile.last == Some(drawn_with) { Drawn::Kept } else { Drawn::Again };
                tile.last = Some(drawn_with);
                let (x, y) = (u32::from(square.corner.0) * DETAIL_UNIT, u32::from(square.corner.1) * DETAIL_UNIT);
                let size = DETAIL_UNIT << square.level;
                out.tiles.push(MoverTile { mover: number, lamp: tile.lamp, lamp_at: tile.lamp_at, lamp_radius: tile.lamp_radius, axis: tile.axis, tan_half: tile.tan_half, near: tile.near, x, y, size, fade: tile.fade, drawn });
                self.numbers.entry(root).or_insert(number);
            }
        }
        out.tiles.sort_unstable_by_key(|tile| (tile.lamp, tile.mover));
    }
}

fn empty(out: &mut ResMut<MoverTiles>) {
    if !out.tiles.is_empty() || !out.casters.is_empty() || !out.joints.is_empty() {
        out.tiles.clear();
        out.casters.clear();
        out.joints.clear();
    }
}

/// Runs after `pick_faces`: a mover's tile may take a square from a face, which asks again at its next pick.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn pick_mover_tiles(
    time: Res<Time>,
    settings: Res<Settings>,
    mut state: Local<PickState>,
    mut squares: ResMut<DetailSquares>,
    mut atlas: ResMut<LampAtlas>,
    mut out: ResMut<MoverTiles>,
    mut next: Local<MoverTiles>,
    level: Option<Res<LevelCollision>>,
    shown: Option<Res<ShownScene>>,
    bindposes: Res<Assets<SkinnedMeshInverseBindposes>>,
    cameras: Query<(&Transform, &Projection, &Camera), With<SceneView>>,
    lamps: Query<(&Transform, &Placed, &InheritedVisibility), With<Lamp>>,
    meshes: Query<
        (Entity, Option<&ChildOf>, &Mesh3d, &Aabb, &GlobalTransform, &RenderLayers, Option<&Visibility>, Option<&StillProp>, Option<&SkinnedMesh>, Option<&BaseModel>),
        Or<(With<FollowActor>, With<FollowBone>)>,
    >,
    models: Query<(), With<ActorModel>>,
    joints: Query<Ref<GlobalTransform>>,
    mut bindpose_events: MessageReader<AssetEvent<SkinnedMeshInverseBindposes>>,
) {
    // A head or bolt-on casts with the body it is on. Every map attachment shares one parent, which is no mover.
    let mover_of = |entity: Entity, child: Option<&ChildOf>, base: Option<&BaseModel>| {
        let own = child.map(ChildOf::parent).filter(|&parent| models.contains(parent));
        base.and_then(|base| base.0).or(own).unwrap_or(entity)
    };
    let state = &mut *state;
    std::mem::swap(&mut state.skins.last, &mut state.skins.now);
    state.skins.now.clear();
    if bindpose_events.read().count() > 0 {
        state.skins.last.clear();
    }
    if state.made != squares.made || squares.book.is_none() {
        state.made = squares.made;
        state.movers.clear();
    }
    let Some(book) = squares.book.as_mut() else { return empty(&mut out) };
    let shadow_level = settings.modern_level(MODERN_LAMP_SHADOWS);
    let Some(eye) = scene_eye(&cameras, lamp_quality(shadow_level).texel_pixels) else { return empty(&mut out) };
    let (now, dt) = (time.elapsed_secs(), time.delta_secs());
    let most_extras = match shadow_level {
        0..=2 => 0,
        3 => 1,
        _ => MOST_EXTRAS,
    };
    let finest = usize::min(if shadow_level >= 4 { 3 } else { 2 }, book.top_level());
    let walls = level.as_deref().and_then(|level| level.0.as_deref()).zip(shown.as_deref().map(|shown| shown.origin));
    let casting = RenderLayers::from_layers(&[MOVER_LAYER, RESTING_PROP_LAYER]);
    let resting_layer = RenderLayers::layer(RESTING_PROP_LAYER);
    let casts = |layers: &RenderLayers, seen: Option<&Visibility>| layers.intersects(&casting) && seen != Some(&Visibility::Hidden);

    state.lamps.clear();
    state.lamps.extend(lamps.iter().filter(|(.., seen)| seen.get()).map(|(at, placed, _)| Lit {
        lamp: placed.number,
        at: at.translation,
        range: placed.range,
        strength: placed.strength,
        spot: placed.spot,
        score: 0.0,
    }));

    state.bounds.clear();
    state.casting.clear();
    for (entity, child, _, aabb, placed, layers, seen, still, _, base) in &meshes {
        if !casts(layers, seen) {
            continue;
        }
        let (min, max) = world_min_max(aabb, placed);
        let resting = still.is_some_and(|still| still.0) || layers.intersects(&resting_layer);
        let root = mover_of(entity, child, base);
        state.casting.push((entity, root));
        state.bounds.entry(root)
            .and_modify(|bounds| {
                bounds.min = bounds.min.min(min);
                bounds.max = bounds.max.max(max);
                bounds.resting &= resting;
            })
            .or_insert(Bounds { min, max, resting });
    }

    state.choose_lamps(eye.at, now, dt, walls, most_extras);
    if state.place_tiles(book, eye.at, eye.texel_spread, finest, dt) {
        let slots = book.slots();
        if atlas.faces != slots {
            atlas.faces = slots;
        }
    }
    let next = &mut *next;
    state.fill_tiles(next);
    let capsules = state.near.iter().filter_map(|(_, root)| state.bounds.get(root)).map(|bounds| Capsule::of_box(bounds.min, bounds.max));
    next.grounding.clear();
    for column_units in GRID_COLUMNS {
        grounding_row(eye.at, capsules.clone(), column_units, &mut next.grounding);
    }
    for &(entity, root) in &state.casting {
        let Some(&mover) = state.numbers.get(&root) else { continue };
        let Ok((_, _, mesh, _, placed, .., skinned, _)) = meshes.get(entity) else { continue };
        let posed = match skinned {
            Some(skin) => match skin_joints(entity, skin, &bindposes, &joints, &mut state.skins, &mut next.joints) {
                Some(range) => Some(range),
                None => continue,
            },
            None => None,
        };
        next.casters.push(MoverCaster { mesh: mesh.0.id(), world_from_mesh: placed.to_matrix(), joints: posed, mover });
    }
    // Only a frame whose bits differ marks the resource changed, since a changed resource is cloned whole to render.
    let held = out.bypass_change_detection();
    let changed = [
        tile_grid_rows(eye.at, &next.tiles, &mut held.tile_grid, &mut state.grid_made_from),
        take_if_new(&mut held.tiles, &mut next.tiles, |a, b| tile_key(a) == tile_key(b)),
        take_if_new(&mut held.casters, &mut next.casters, |a, b| caster_key(a) == caster_key(b)),
        take_if_new(&mut held.joints, &mut next.joints, |a, b| bits(a.to_cols_array()) == bits(b.to_cols_array())),
        take_if_new(&mut held.grounding, &mut next.grounding, |a, b| bits(*a) == bits(*b)),
    ];
    if changed.into_iter().any(|changed| changed) {
        out.set_changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::{Quat, Vec3A};

    fn lamp(at: Vec3, spot: Option<(Vec3, f32)>) -> Lit {
        Lit { lamp: 0, at, range: 1000.0, strength: 2.0, spot, score: 0.0 }
    }

    #[test]
    fn the_lamp_grid_hands_each_point_every_lamp_that_reaches_it_in_the_lamps_order() {
        let mut seed = 7_u32;
        let mut unit = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32
        };
        let lamps: Vec<Lit> = (0..200)
            .map(|number| Lit { lamp: number, at: Vec3::new(unit() * 9000.0 - 4500.0, unit() * 400.0, unit() * 9000.0 - 4500.0), range: 100.0 + unit() * 1500.0, strength: 1.0, spot: None, score: 0.0 })
            .collect();
        let points: Vec<Vec3> = (0..300).map(|_| Vec3::new(unit() * 8192.0 - 4096.0, unit() * 400.0, unit() * 8192.0 - 4096.0)).collect();
        let (low, high) = points.iter().fold((Vec3::MAX, Vec3::MIN), |(low, high), &point| (low.min(point), high.max(point)));
        let mut grid = LampGrid::default();
        grid.build(&lamps, low, high);
        for &point in &points {
            let near = grid.near(point);
            assert!(near.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(near.len() < lamps.len() / 2, "{} of {}", near.len(), lamps.len());
            for (index, _) in lamps.iter().enumerate().filter(|(_, lamp)| lamp_score(lamp, point) > 0.0) {
                assert!(near.contains(&(index as u32)), "lamp {index} reaches {point} but is not listed");
            }
        }
        let fewer = &lamps[..lamps.len() / 3];
        let (low, high) = (low * 0.5, high * 0.5);
        let mut new_grid = LampGrid::default();
        new_grid.build(fewer, low, high);
        grid.build(fewer, low, high);
        for &point in &points {
            assert_eq!(grid.near(point), new_grid.near(point), "{point}");
        }
    }

    #[test]
    fn a_lamp_scores_its_strength_at_its_middle_and_nothing_at_its_range() {
        let lit = lamp(Vec3::ZERO, None);
        assert!((lamp_score(&lit, Vec3::ZERO) - 2.0).abs() < 1e-5);
        assert!((lamp_score(&lit, Vec3::new(500.0, 0.0, 0.0)) - 1.0).abs() < 1e-5);
        assert_eq!(lamp_score(&lit, Vec3::new(1000.0, 0.0, 0.0)), 0.0);
    }

    #[test]
    fn a_spot_scores_nothing_outside_its_cone_and_most_along_its_axis() {
        let lit = lamp(Vec3::ZERO, Some((Vec3::NEG_Y, 0.8)));
        assert_eq!(lamp_score(&lit, Vec3::new(100.0, 0.0, 0.0)), 0.0);
        let along = lamp_score(&lit, Vec3::new(0.0, -100.0, 0.0));
        let aside = lamp_score(&lit, Vec3::new(30.0, -100.0, 0.0));
        assert!(along > aside && aside > 0.0, "{along} {aside}");
    }

    #[test]
    fn a_lamp_left_out_by_the_movers_box_scores_nothing_at_any_of_them() {
        let mut seed = 11u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / 16_777_216.0
        };
        let (mut left_out, mut kept) = (0, 0);
        for _ in 0..200 {
            let centre = Vec3::new(next() - 0.5, next() - 0.5, next() - 0.5) * 20000.0;
            let middles: Vec<Vec3> = (0..8).map(|_| centre + Vec3::new(next() - 0.5, next() - 0.5, next() - 0.5) * 3000.0).collect();
            let (low, high) = middles.iter().fold((Vec3::MAX, Vec3::MIN), |(low, high), &middle| (low.min(middle), high.max(middle)));
            for _ in 0..50 {
                let at = centre + Vec3::new(next() - 0.5, next() - 0.5, next() - 0.5) * 9000.0;
                let spot = (next() < 0.5).then(|| (Vec3::new(next() - 0.5, -1.0, next() - 0.5).normalize(), next()));
                let lit = Lit { range: 50.0 + next() * 3000.0, ..lamp(at, spot) };
                if may_light(&lit, low, high) {
                    kept += 1;
                } else {
                    left_out += 1;
                    assert!(middles.iter().all(|&middle| lamp_score(&lit, middle) <= 0.0), "{lit:?} {low} {high}");
                }
            }
        }
        assert!(left_out > 0 && kept > 0, "{left_out} {kept}");
    }

    #[test]
    fn a_lamp_just_out_of_reach_of_the_box_is_left_out_and_one_just_inside_is_kept() {
        let (low, high) = (Vec3::new(-100.0, 0.0, -100.0), Vec3::new(100.0, 50.0, 100.0));
        let inside = lamp(Vec3::new(1099.0, 20.0, 0.0), None);
        assert!(may_light(&inside, low, high) && lamp_score(&inside, Vec3::new(100.0, 20.0, 0.0)) > 0.0);
        assert!(!may_light(&lamp(Vec3::new(1200.0, 20.0, 0.0), None), low, high));
    }

    #[test]
    fn a_tile_looks_wider_than_its_mover_and_no_wider_than_the_widest() {
        let (axis, tan_half, distance) = tile_view(Vec3::ZERO, Vec3::new(0.0, 0.0, 500.0), 50.0);
        assert_eq!((axis, distance), (Vec3::Z, 500.0));
        assert!(tan_half > 50.0 / 500.0 && tan_half < 0.2, "{tan_half}");
        assert_eq!(tile_view(Vec3::ZERO, Vec3::new(0.0, 0.0, 10.0), 50.0).1, WIDEST_TILE);
    }

    #[test]
    fn a_near_tile_is_finer_than_a_far_one_and_no_finer_than_the_finest() {
        assert_eq!(tile_level(0.1, 500.0, 0.25, 2), 2);
        assert_eq!(tile_level(0.1, 500.0, 0.25, 3), 3);
        assert_eq!(tile_level(0.1, 500.0, 50.0, 3), 0);
        assert_eq!(tile_level(0.1, 500.0, 100.0 / 128.0, 3), 1);
        assert_eq!(tile_level(0.0, 0.0, 1.0, 3), 0);
    }

    #[test]
    fn a_tile_grows_after_a_quarter_second_and_shrinks_after_two() {
        let mut hold = LevelHold { level: 1, ..LevelHold::default() };
        hold = hold_level(hold, 2, 0.2);
        assert_eq!(hold.level, 1);
        hold = hold_level(hold, 2, 0.1);
        assert_eq!(hold.level, 2);
        for _ in 0..19 {
            hold = hold_level(hold, 0, 0.1);
        }
        assert_eq!(hold.level, 2);
        hold = hold_level(hold, 0, 0.2);
        assert_eq!(hold.level, 0);
    }

    #[test]
    fn a_rival_takes_the_primary_only_after_outscoring_it_for_long_enough_without_a_break() {
        let held = Primary { lamp: Some(1), rival: None };
        let once = hold_primary(held, Some((2, 1.6)), 1.0, 0.2);
        assert_eq!(once, Primary { lamp: Some(1), rival: Some((2, 0.2)) });
        assert_eq!(hold_primary(once, Some((2, 1.6)), 1.0, 0.2).lamp, Some(2));
        let broken = hold_primary(once, Some((2, 1.4)), 1.0, 0.2);
        assert_eq!(broken, held);
        assert_eq!(hold_primary(broken, Some((2, 1.6)), 1.0, 0.2).lamp, Some(1));
        assert_eq!(hold_primary(held, None, 0.0, 0.2), Primary::default());
        assert_eq!(hold_primary(Primary::default(), Some((3, 0.5)), 0.0, 0.0).lamp, Some(3));
    }

    #[test]
    fn an_extra_needs_a_quarter_of_the_primary_to_come_and_stays_until_under_fifteen_percent() {
        let scores = [(0, 10.0), (1, 2.0), (2, 1.8), (3, 1.0)];
        let iter = || scores.iter().copied();
        assert_eq!(pick_extras([None; 2], iter(), (0, 10.0), 2), [None; 2]);
        assert_eq!(pick_extras([Some(1), Some(2)], iter(), (0, 10.0), 2), [Some(1), Some(2)]);
        assert_eq!(pick_extras([Some(3), None], iter(), (0, 10.0), 2), [None; 2]);
        let higher = [(0, 10.0), (4, 3.0), (5, 2.6), (1, 2.0)];
        assert_eq!(pick_extras([Some(1), None], higher.iter().copied(), (0, 10.0), 2), [Some(1), Some(4)]);
        assert_eq!(pick_extras([None; 2], higher.iter().copied(), (0, 10.0), 1), [Some(4), None]);
    }

    #[test]
    fn a_tile_fades_in_and_out_over_its_fade_time() {
        let half = MOVER_FADE_SECONDS * 0.5;
        assert!((faded(0.0, Fading::In, half) - 0.5).abs() < 1e-5);
        assert_eq!(faded(0.9, Fading::In, half), 1.0);
        assert_eq!(faded(0.4, Fading::Out, half), 0.0);
    }

    #[test]
    fn a_wall_check_holds_until_the_mover_moves_or_time_passes() {
        let check = WallCheck { lamp: 0, from: Vec3::ZERO, at: 1.0, clear: true };
        assert!(fresh(&check, Vec3::new(10.0, 0.0, 0.0), 1.2));
        assert!(!fresh(&check, Vec3::new(20.0, 0.0, 0.0), 1.1));
        assert!(!fresh(&check, Vec3::ZERO, 1.3));
    }

    #[test]
    fn a_turned_and_scaled_box_is_held_by_its_world_box() {
        let aabb = Aabb { center: Vec3A::new(0.0, 1.0, 0.0), half_extents: Vec3A::new(1.0, 1.0, 2.0) };
        let placed = GlobalTransform::from(Transform::from_xyz(10.0, 0.0, 0.0).with_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)).with_scale(Vec3::splat(2.0)));
        let (min, max) = world_min_max(&aabb, &placed);
        assert!(min.abs_diff_eq(Vec3::new(6.0, 0.0, -2.0), 1e-4) && max.abs_diff_eq(Vec3::new(14.0, 4.0, 2.0), 1e-4), "{min} {max}");
    }

    #[test]
    fn a_tile_square_changes_size_and_gives_the_old_one_back() {
        let mut book = FaceBook::new(8192);
        let first = resize(&mut book, None, 2);
        assert_eq!(first.map(|square| square.level), Some(2));
        let smaller = resize(&mut book, first, 1);
        assert_eq!(smaller.map(|square| square.level), Some(1));
        let larger = resize(&mut book, smaller, 3);
        assert_eq!(larger.map(|square| square.level), Some(3));
        assert_eq!(resize(&mut book, larger, 3), larger);
        let mut taken = 0;
        while book.take_square(3).is_some() {
            taken += 1;
        }
        assert_eq!(taken, 63);
    }
}
