//! Liquid on the floors as a height field on the GPU, in world tiles of 128 x 128 texels of 2 Unreal units. The main
//! world decides which tiles live and in which atlas slot, traces each tile's floor once on a worker, and hands the
//! render world (`layer_gpu.rs`) the frame's sources and steps. Unreal units and axes, heights in Bevy's.

use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

use bevy::asset::{AssetId, Assets, Handle, RenderAssetUsages};
use bevy::ecs::prelude::{Commands, Entity, Resource};
use bevy::image::Image;
use bevy::light::ClusteredDecal;
use bevy::math::{Quat, Vec2, Vec3};
use bevy::render::RenderApp;
use bevy::render::extract_resource::ExtractResource;
use bevy::render::render_resource::{Extent3d, ShaderType, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::storage::ShaderBuffer;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use bevy::transform::components::Transform;
use p2_math::Vector;
use p2_vm::FluidType;
use rustc_hash::FxHashMap;

use crate::level_collision::StaticCollision;

pub const SIDE: u32 = 128;
pub const TEXEL: f32 = 2.0;
pub const TILE: f32 = SIDE as f32 * TEXEL;
pub const TEXELS: usize = (SIDE * SIDE) as usize;
pub const APRON: u32 = 2;
pub const SLOT_SIDE: u32 = SIDE + 2 * APRON;
pub const SLOTS_SIDE: u32 = 8;
pub const MOST_SLOTS: u32 = SLOTS_SIDE * SLOTS_SIDE;
/// The depth a wet floor keeps: only liquid above it flows. A liquid on a floor stands about this deep.
pub const FILM: f32 = 0.3;
/// Puke is a paste: it stands this deep before it flows.
pub const PASTE_FILM: f32 = 0.8;
/// Seconds of one step of the liquid, on the level's clock.
pub const STEP: f32 = 1.0 / 60.0;
/// A floor height at or past this is a wall, or not traced yet.
pub const WALL: f32 = 1e30;
/// A feed joins the tile at its column whose floor there lies within this of its height; a tile joins the one beside
/// it whose first liquid lies within the larger reach. Floors farther apart keep to tiles of their own.
const FEED_REACH: f32 = 24.0;
const BESIDE_REACH: f32 = 128.0;
/// Tiles one column holds, one a floor.
const MOST_LAYERS: i32 = 4;
/// `LEDGE`, `SPILL_CELL` and `SPILL_SCALE` in `liquid_layer.wesl`: a lip's drop, the side of a cell spills are counted
/// in, and the count's unit.
pub const LEDGE: f32 = 6.0;
pub const SPILL_CELL: u32 = 8;
const SPILL_CELLS: usize = ((SIDE / SPILL_CELL) * (SIDE / SPILL_CELL)) as usize;
const SPILL_SCALE: f32 = 1000.0;
/// The ways the spill bits name, as `WAYS` in `liquid_layer.wesl`.
const WAYS: [[i32; 2]; 4] = [[-1, 0], [1, 0], [0, -1], [0, 1]];
/// A tile's floor rays start this far over its first liquid and reach this far down: a kerb is a floor, a deep drop
/// a wall.
const STEP_UP: f32 = 24.0;
const REACH: f32 = 128.0;
/// Unreal units around a source whose tiles are made too, so its liquid has room to run.
const ROOM: f32 = 48.0;
/// Seconds a tile nothing feeds is kept: its film has dried by then.
const IDLE: f32 = 12.0;
/// A projector decal whose frame buffer blending is 7, which no game projector uses: the world's surfaces read it as
/// liquid. Its slot rides in bits 8 to 15.
const LIQUID_DECAL_TAG: u32 = 0x8000_0000 | 0x10_0000 | 0xf_0000 | 7;
/// How far the decal's box reaches above and below the tile's floors.
const BOX_MARGIN: f32 = 12.0;

pub trait Floor {
    /// The height of the floor straight under `from` within `reach`; `None` where there is none or `from` is inside.
    fn floor_below(&self, from: Vector, reach: f32) -> Option<f32>;
}

impl Floor for StaticCollision {
    fn floor_below(&self, from: Vector, reach: f32) -> Option<f32> {
        let hit = self.first_hit(from, from - Vector::new(0.0, 0.0, reach))?;
        (hit.time > 0.0 && hit.normal.z > 0.5).then_some(hit.location.z)
    }
}

/// `Slot` in `liquid_layer.wesl`.
#[derive(ShaderType, Clone, Copy, Debug, PartialEq)]
pub struct LayerSlot {
    pub around: [i32; 9],
    pub first_source: u32,
    pub sources: u32,
    pub atlas_x: u32,
    pub atlas_y: u32,
}

impl Default for LayerSlot {
    fn default() -> Self {
        Self { around: [-1; 9], first_source: 0, sources: 0, atlas_x: 0, atlas_y: 0 }
    }
}

/// `Source` in `liquid_layer.wesl`.
#[derive(ShaderType, Clone, Copy, Debug, Default, PartialEq)]
pub struct LayerSource {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub amount: f32,
    pub kind: f32,
    pub adds: u32,
    pub pad: Vec2,
}

/// What a feed does to the floor under it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Feed {
    /// Keeps the floor wet to this depth, as a script's puddle or trail does while it lives.
    Hold(f32),
    /// Adds this volume, as a landing drop does.
    Add(f32),
}

/// Liquid fed onto the floor this frame: a disc at `at` of `radius` Unreal units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Feeding {
    pub at: Vector,
    pub radius: f32,
    pub kind: FluidType,
    pub feed: Feed,
}

/// The frame's work for the render world. `floors` and `cleared` are handed over once.
#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub struct LayerFrame {
    /// Slots the buffers hold.
    pub capacity: u32,
    /// Changes with the level: the render world starts afresh.
    pub level: u32,
    pub steps: u32,
    pub slots: Vec<LayerSlot>,
    pub active: Vec<u32>,
    pub sources: Vec<LayerSource>,
    pub cleared: Vec<u32>,
    pub floors: Vec<(u32, Arc<Vec<f32>>)>,
    pub atlas: Option<AssetId<Image>>,
    pub spills: Option<AssetId<ShaderBuffer>>,
}

/// A tile: its column of the world's grid, and which of the floors in that column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub x: i32,
    pub y: i32,
    pub layer: i32,
}

impl TileKey {
    fn corner(self) -> Vector {
        Vector::new(self.x as f32 * TILE, self.y as f32 * TILE, 0.0)
    }
}

/// The grid column holding `at`.
fn column(at: Vector) -> [i32; 2] {
    [(at.x / TILE).floor() as i32, (at.y / TILE).floor() as i32]
}

struct Tile {
    slot: u32,
    /// The kind it was last fed.
    kind: FluidType,
    first_z: f32,
    last_fed: f32,
    tracing: Option<Task<Vec<f32>>>,
    /// The traced floor, Bevy heights, row by row.
    floors: Option<Arc<Vec<f32>>>,
    /// The lowest and highest traced floor, Bevy heights.
    span: Option<[f32; 2]>,
    decal: Option<Entity>,
}

/// Every tile's floor heights in Bevy's (Unreal z plus `lift`), row by row; `WALL` where liquid cannot stand.
pub fn trace_floor(floor: &impl Floor, key: TileKey, first_z: f32, lift: f32) -> Vec<f32> {
    let corner = key.corner();
    (0..SIDE)
        .flat_map(|j| (0..SIDE).map(move |i| (i, j)))
        .map(|(i, j)| {
            let from = Vector::new(corner.x + (i as f32 + 0.5) * TEXEL, corner.y + (j as f32 + 0.5) * TEXEL, first_z + STEP_UP);
            floor.floor_below(from, REACH).map_or(WALL, |z| z + lift)
        })
        .collect()
}

fn floor_span(floors: &[f32]) -> Option<[f32; 2]> {
    floors.iter().filter(|&&z| z < WALL).fold(None, |span, &z| {
        let [lo, hi] = span.unwrap_or([z, z]);
        Some([lo.min(z), hi.max(z)])
    })
}

/// What a new tile's floor is traced against, the world's origin in Bevy and the level's clock.
#[derive(Clone, Copy)]
pub struct Level<'a> {
    pub collision: Option<&'a Arc<StaticCollision>>,
    pub origin: Vec3,
    pub now: f32,
}

#[derive(Resource, Default)]
pub struct LiquidLayer {
    tiles: FxHashMap<TileKey, Tile>,
    free: Vec<u32>,
    capacity: u32,
    level: u32,
    atlas: Option<Handle<Image>>,
    spills: Option<Handle<ShaderBuffer>>,
    spare: f32,
}

/// Liquid spilling over a lip: the lip's place, the way it spills, the kind and the volume counted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lip {
    pub at: Vector,
    pub way: Vector,
    pub kind: FluidType,
    pub volume: f32,
}

/// The depth a kind stands at before it flows.
pub fn film_of(kind: FluidType) -> f32 {
    if matches!(kind, FluidType::Puke | FluidType::BloodyPuke) { PASTE_FILM } else { FILM }
}

/// The slots the buffers hold at a level of the Liquids slider.
pub fn capacity(level: u8) -> u32 {
    (4 + 6 * u32::from(level)).min(MOST_SLOTS)
}

pub fn new_liquid_atlas(images: &mut Assets<Image>) -> Handle<Image> {
    let side = SLOTS_SIDE * SLOT_SIDE;
    let mut image = Image::new_uninit(Extent3d { width: side, height: side, depth_or_array_layers: 1 }, TextureDimension::D2, TextureFormat::Rgba16Float, RenderAssetUsages::RENDER_WORLD);
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC;
    images.add(image)
}

fn decal_transform(key: TileKey, [lo, hi]: [f32; 2], origin: Vec3) -> Transform {
    let corner = key.corner();
    Transform::from_translation(Vec3::new(corner.x + TILE * 0.5 + origin.x, (lo + hi) * 0.5, corner.y + TILE * 0.5 + origin.z))
        .with_rotation(Quat::from_rotation_x(-FRAC_PI_2))
        .with_scale(Vec3::new(TILE, TILE, hi - lo + 2.0 * BOX_MARGIN))
}

/// The grid columns a disc of `radius` around `at` touches.
fn touched(at: Vector, radius: f32) -> impl Iterator<Item = [i32; 2]> {
    let ([x0, y0], [x1, y1]) = (column(at - Vector::new(radius, radius, 0.0)), column(at + Vector::new(radius, radius, 0.0)));
    (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| [x, y]))
}

/// The traced floor's Unreal height under `at` in the tile at `key`, if traced there and not a wall.
fn floor_under(key: TileKey, floors: &[f32], at: Vector, lift: f32) -> Option<f32> {
    let local = (at - key.corner()) * (1.0 / TEXEL);
    let (i, j) = (local.x.floor(), local.y.floor());
    let inside = (0.0..SIDE as f32).contains(&i) && (0.0..SIDE as f32).contains(&j);
    let z = *floors.get((j as usize) * SIDE as usize + i as usize).filter(|_| inside)?;
    (z < WALL).then_some(z - lift)
}

impl LiquidLayer {
    /// The tile at `column` whose floor lies nearest `z` within `reach`: under `at` where it is traced, else at its first
    /// liquid's height.
    fn layer_near(&self, [x, y]: [i32; 2], at: Vector, lift: f32, reach: f32) -> Option<TileKey> {
        (0..MOST_LAYERS)
            .map(|layer| TileKey { x, y, layer })
            .filter_map(|key| {
                let tile = self.tiles.get(&key)?;
                let floor = tile.floors.as_deref().and_then(|floors| floor_under(key, floors, at, lift)).unwrap_or(tile.first_z);
                let off = (floor - at.z).abs();
                (off < reach).then_some((key, off))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(key, _)| key)
    }

    /// Drops every tile; the render world starts afresh at `capacity` slots.
    pub fn reset(&mut self, commands: &mut Commands, capacity: u32) {
        for tile in self.tiles.drain().map(|(_, tile)| tile) {
            if let Some(decal) = tile.decal {
                commands.entity(decal).despawn();
            }
        }
        self.free = (0..capacity).rev().collect();
        (self.capacity, self.level, self.spare) = (capacity, self.level.wrapping_add(1), 0.0);
    }

    /// A tile at `column` for liquid at `at`, made with its floor traced from there if no tile holds that floor and there
    /// is room; a tile idle longest gives way when the slots run out.
    fn tile(&mut self, [x, y]: [i32; 2], at: Vector, level: Level, cleared: &mut Vec<u32>, commands: &mut Commands) {
        if self.layer_near([x, y], at, level.origin.y, FEED_REACH).is_some() {
            return;
        }
        let Some(key) = (0..MOST_LAYERS).map(|layer| TileKey { x, y, layer }).find(|key| !self.tiles.contains_key(key)) else { return };
        let first_z = at.z;
        if self.free.is_empty() {
            let idlest = self.tiles.iter().filter(|(_, tile)| level.now - tile.last_fed > 1.0).min_by(|a, b| a.1.last_fed.total_cmp(&b.1.last_fed)).map(|(&key, _)| key);
            let Some(idlest) = idlest else { return };
            self.drop_tile(idlest, commands);
        }
        let Some(slot) = self.free.pop() else { return };
        let lift = level.origin.y;
        let tracing = level.collision.cloned().map(|collision| AsyncComputeTaskPool::get().spawn(async move { trace_floor(&*collision, key, first_z, lift) }));
        self.tiles.insert(key, Tile { slot, kind: FluidType::Urine, first_z, last_fed: level.now, tracing, floors: None, span: None, decal: None });
        cleared.push(slot);
    }

    fn drop_tile(&mut self, key: TileKey, commands: &mut Commands) {
        if let Some(tile) = self.tiles.remove(&key) {
            if let Some(decal) = tile.decal {
                commands.entity(decal).despawn();
            }
            self.free.push(tile.slot);
        }
    }

    /// One frame: tiles for the feeds, their sources, finished floor traces, idle tiles dropped, `steps` steps.
    pub fn frame(&mut self, feeds: &[Feeding], level: Level, steps: u32, images: &mut Assets<Image>, commands: &mut Commands, frame: &mut LayerFrame) {
        let (now, lift) = (level.now, level.origin.y);
        let atlas = self.atlas.get_or_insert_with(|| new_liquid_atlas(images)).clone();
        frame.floors.clear();
        frame.cleared.clear();
        let mut by_slot: Vec<Vec<LayerSource>> = vec![Vec::new(); self.capacity as usize];
        for feeding in feeds {
            for column in touched(feeding.at, feeding.radius + ROOM) {
                self.tile(column, feeding.at, level, &mut frame.cleared, commands);
            }
            for column in touched(feeding.at, feeding.radius) {
                let Some(key) = self.layer_near(column, feeding.at, lift, FEED_REACH) else { continue };
                let Some(tile) = self.tiles.get_mut(&key) else { continue };
                (tile.last_fed, tile.kind) = (now, feeding.kind);
                let local = (feeding.at - key.corner()) * (1.0 / TEXEL);
                let (amount, adds) = match feeding.feed {
                    Feed::Hold(depth) => (depth, 0),
                    Feed::Add(volume) => (volume, 1),
                };
                by_slot[tile.slot as usize].push(LayerSource { x: local.x, y: local.y, radius: feeding.radius / TEXEL, amount, kind: f32::from(feeding.kind as u8), adds, pad: Vec2::ZERO });
            }
        }
        let idle: Vec<TileKey> = self.tiles.iter().filter(|(_, tile)| now - tile.last_fed > IDLE).map(|(&key, _)| key).collect();
        for key in idle {
            self.drop_tile(key, commands);
        }
        for (&key, tile) in &mut self.tiles {
            if tile.tracing.as_ref().is_some_and(Task::is_finished)
                && let Some(floors) = tile.tracing.take().map(block_on)
            {
                tile.span = floor_span(&floors);
                let floors = Arc::new(floors);
                tile.floors = Some(floors.clone());
                frame.floors.push((tile.slot, floors));
            }
            let span = tile.span.unwrap_or([tile.first_z + lift - STEP_UP, tile.first_z + lift + STEP_UP]);
            let transform = decal_transform(key, span, level.origin);
            match tile.decal {
                Some(decal) => {
                    commands.entity(decal).insert(transform);
                }
                None => {
                    let decal = ClusteredDecal { base_color_texture: Some(atlas.clone()), tag: LIQUID_DECAL_TAG | tile.slot << 8, ..Default::default() };
                    tile.decal = Some(commands.spawn((decal, transform)).id());
                }
            }
        }
        frame.slots = vec![LayerSlot::default(); self.capacity as usize];
        frame.active.clear();
        frame.sources.clear();
        for (&key, tile) in &self.tiles {
            let around = std::array::from_fn(|k| {
                let (dx, dy) = (k as i32 % 3 - 1, k as i32 / 3 - 1);
                if (dx, dy) == (0, 0) {
                    return tile.slot as i32;
                }
                let middle = key.corner() + Vector::new((dx as f32 + 0.5) * TILE, (dy as f32 + 0.5) * TILE, tile.first_z);
                let beside = self.layer_near([key.x + dx, key.y + dy], middle, lift, BESIDE_REACH);
                beside.and_then(|other| self.tiles.get(&other)).map_or(-1, |other| other.slot as i32)
            });
            let sources = std::mem::take(&mut by_slot[tile.slot as usize]);
            let (atlas_x, atlas_y) = (tile.slot % SLOTS_SIDE * SLOT_SIDE, tile.slot / SLOTS_SIDE * SLOT_SIDE);
            frame.slots[tile.slot as usize] = LayerSlot { around, first_source: frame.sources.len() as u32, sources: sources.len() as u32, atlas_x, atlas_y };
            frame.sources.extend(sources);
            frame.active.push(tile.slot);
        }
        frame.active.sort_unstable();
        (frame.capacity, frame.level, frame.steps, frame.atlas) = (self.capacity, self.level, steps, Some(atlas.id()));
    }

    /// The spill counter, made once for the most slots; true when it was made now.
    pub fn spill_buffer(&mut self, buffers: &mut Assets<ShaderBuffer>, frame: &mut LayerFrame) -> Option<Handle<ShaderBuffer>> {
        let made = self.spills.is_none();
        let spills = self.spills.get_or_insert_with(|| buffers.add(ShaderBuffer::with_size(u64::from(MOST_SLOTS) * SPILL_CELLS as u64 * 8, RenderAssetUsages::RENDER_WORLD))).clone();
        frame.spills = Some(spills.id());
        made.then_some(spills)
    }

    /// Where the counted spills went over lips: for each cell that spilt, the lip in it the traced floor shows, per way.
    pub fn lips(&self, counts: &[u8], lift: f32) -> Vec<Lip> {
        let word = |index: usize| counts.get(index * 4..index * 4 + 4).map_or(0, |bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
        let mut lips = Vec::new();
        for (&key, tile) in &self.tiles {
            let Some(floors) = tile.floors.as_deref() else { continue };
            for cell in 0..SPILL_CELLS {
                let counted = (tile.slot as usize * SPILL_CELLS + cell) * 2;
                let (volume, ways) = (word(counted), word(counted + 1));
                if volume == 0 || ways == 0 {
                    continue;
                }
                let per_side = (SIDE / SPILL_CELL) as usize;
                let (cx, cy) = ((cell % per_side) as i32 * SPILL_CELL as i32, (cell / per_side) as i32 * SPILL_CELL as i32);
                let share = volume as f32 / SPILL_SCALE / ways.count_ones() as f32;
                for [dx, dy] in WAYS.into_iter().enumerate().filter(|&(k, _)| ways & (1 << k) != 0).map(|(_, way)| way) {
                    let at = |x: i32, y: i32| floors.get((y * SIDE as i32 + x) as usize).copied().filter(|_| (0..SIDE as i32).contains(&x) && (0..SIDE as i32).contains(&y));
                    let lip = (cy..cy + SPILL_CELL as i32)
                        .flat_map(|y| (cx..cx + SPILL_CELL as i32).map(move |x| (x, y)))
                        .find(|&(x, y)| matches!((at(x, y), at(x + dx, y + dy)), (Some(high), Some(low)) if high < WALL && low < high - LEDGE));
                    let Some((x, y)) = lip else { continue };
                    let way = Vector::new(dx as f32, dy as f32, 0.0);
                    let middle = key.corner() + Vector::new((x as f32 + 0.5) * TEXEL, (y as f32 + 0.5) * TEXEL, floors[(y * SIDE as i32 + x) as usize] - lift);
                    lips.push(Lip { at: middle + way * (TEXEL * 0.5), way, kind: tile.kind, volume: share });
                }
            }
        }
        lips
    }

    /// Whole steps of `step` seconds owed after `seconds` more of the level's clock, at most `most`.
    pub fn steps(&mut self, seconds: f32, step: f32, most: u32) -> u32 {
        self.spare = (self.spare + seconds).min(step * most as f32);
        let steps = (self.spare / step).floor();
        self.spare -= steps * step;
        steps as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::shader_const;

    struct Flat(f32);

    impl Floor for Flat {
        fn floor_below(&self, from: Vector, _reach: f32) -> Option<f32> {
            (from.x < 100.0).then_some(self.0)
        }
    }

    #[test]
    fn the_shaders_know_the_layout() {
        let layer = include_str!("../liquid_layer.wesl");
        let projector = include_str!("../projector.wesl");
        assert_eq!(shader_const(layer, "SIDE"), f64::from(SIDE));
        assert_eq!(shader_const(layer, "APRON"), f64::from(APRON));
        assert_eq!(shader_const(layer, "TEXEL_UU"), f64::from(TEXEL));
        assert_eq!(shader_const(projector, "POOL_SIDE"), f64::from(SIDE));
        assert_eq!(shader_const(projector, "POOL_APRON"), f64::from(APRON));
        assert_eq!(shader_const(projector, "POOL_SLOT_SIDE"), f64::from(SLOT_SIDE));
        assert_eq!(shader_const(projector, "POOL_SLOTS_SIDE"), f64::from(SLOTS_SIDE));
        assert_eq!(shader_const(projector, "POOL_TEXEL_UU"), f64::from(TEXEL));
        assert_eq!(shader_const(projector, "POOL_FILM") as f32, FILM);
        assert_eq!(shader_const(projector, "POOL_PASTE_FILM") as f32, PASTE_FILM);
        assert_eq!(shader_const(layer, "PASTE_FILM") as f32, PASTE_FILM);
        assert_eq!((film_of(FluidType::Puke), film_of(FluidType::Gas)), (PASTE_FILM, FILM));
    }

    #[test]
    fn a_floor_is_traced_in_bevy_heights_with_walls_where_none_is() {
        let floors = trace_floor(&Flat(-20.0), TileKey { x: 0, y: 0, layer: 0 }, -20.0, 5.0);
        assert_eq!(floors.len(), TEXELS);
        assert_eq!((floors[0], floors[49]), (-15.0, -15.0));
        assert_eq!(floors[50], WALL, "texel 50's middle lies at x 101");
        assert_eq!(floor_span(&floors), Some([-15.0, -15.0]));
        let key = TileKey { x: 0, y: 0, layer: 0 };
        assert_eq!(floor_under(key, &floors, Vector::new(3.0, 3.0, 0.0), 5.0), Some(-20.0));
        assert_eq!(floor_under(key, &floors, Vector::new(150.0, 3.0, 0.0), 5.0), None, "a wall");
        assert_eq!(floor_under(key, &floors, Vector::new(-3.0, 3.0, 0.0), 5.0), None, "another tile");
    }

    #[test]
    fn a_disc_touches_the_tiles_it_overlaps() {
        assert_eq!(touched(Vector::new(10.0, 10.0, 0.0), 5.0).count(), 1);
        let columns: Vec<[i32; 2]> = touched(Vector::new(2.0, 250.0, 0.0), 20.0).collect();
        assert_eq!(columns.len(), 4);
        assert!(columns.contains(&[-1, 1]));
    }

    #[test]
    fn a_counted_spill_finds_its_lip_on_the_traced_floor() {
        let mut layer = LiquidLayer::default();
        let key = TileKey { x: 0, y: 0, layer: 0 };
        let floors: Vec<f32> = (0..TEXELS).map(|i| if i % SIDE as usize >= 20 { -30.0 } else { 0.0 }).collect();
        layer.tiles.insert(key, Tile { slot: 2, kind: FluidType::Gas, first_z: 0.0, last_fed: 0.0, tracing: None, floors: Some(Arc::new(floors)), span: None, decal: None });
        let mut counts = vec![0_u8; MOST_SLOTS as usize * SPILL_CELLS * 8];
        let cell = 2 * SPILL_CELLS + 2;
        counts[cell * 8..cell * 8 + 4].copy_from_slice(&500_u32.to_le_bytes());
        counts[cell * 8 + 4..cell * 8 + 8].copy_from_slice(&2_u32.to_le_bytes());
        let lips = layer.lips(&counts, 0.0);
        assert_eq!(lips.len(), 1);
        assert_eq!((lips[0].at.x, lips[0].at.z, lips[0].way, lips[0].kind), (40.0, 0.0, Vector::new(1.0, 0.0, 0.0), FluidType::Gas));
        assert!((lips[0].volume - 0.5).abs() < 1e-6);
        let shader = include_str!("../liquid_layer.wesl");
        assert_eq!((shader_const(shader, "LEDGE") as f32, shader_const(shader, "SPILL_CELL") as u32), (LEDGE, SPILL_CELL));
        assert_eq!((shader_const(shader, "SPILL_SCALE") as f32, shader_const(shader, "SPILL_CELLS") as usize), (SPILL_SCALE, SPILL_CELLS));
    }

    #[test]
    fn steps_follow_the_clock_and_stop_at_the_most() {
        let mut layer = LiquidLayer::default();
        assert_eq!(layer.steps(0.01, 0.02, 4), 0);
        assert_eq!(layer.steps(0.015, 0.02, 4), 1);
        assert_eq!(layer.steps(1.0, 0.02, 4), 4);
        assert_eq!(layer.steps(0.0, 0.02, 4), 0, "a long frame's steps are not owed later");
        assert_eq!((capacity(0), capacity(6), capacity(10)), (4, 40, 64));
    }
}
