//! Liquid on the level's surfaces as a height field on the GPU, in tiles of 128 x 128 texels of 2 Unreal units: floor
//! tiles on the world's grid, plane tiles on the planes of walls, slopes and ceilings. The main world decides which
//! tiles live and in which atlas slot, traces each tile's surface once on a worker, and hands the render world
//! (`layer_gpu.rs`) the frame's sources and steps. Unreal units and axes, heights in Bevy's.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::sync::Arc;

use bevy::asset::{AssetId, Assets, Handle, RenderAssetUsages};
use bevy::ecs::prelude::{Commands, Entity, Res, ResMut, Resource};
use bevy::image::Image;
use bevy::light::ClusteredDecal;
use bevy::math::{Mat3, Quat, Vec3};
use bevy::render::RenderApp;
use bevy::render::extract_resource::ExtractResource;
use bevy::render::render_resource::{Extent3d, ShaderType, TextureDimension, TextureFormat, TextureUsages};
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use bevy::transform::components::Transform;
use p2_host::display::NORMAL_FLUID_DETAIL;
use p2_math::Vector;
use p2_vm::FluidType;
use rustc_hash::FxHashMap;

use super::{LiquidTextures, is_puke, puke_decal};
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
/// A tile's floor rays start this far over its first liquid and reach this far down: a kerb is a floor, a deep drop
/// a wall.
const STEP_UP: f32 = 24.0;
const REACH: f32 = 128.0;
/// Unreal units around a source whose tiles are made too, so its liquid has room to run.
const ROOM: f32 = 48.0;
/// `SHAPE_REACH` in `liquid_layer.wesl`: a held source's blot lies within this many radii of its middle.
const SHAPE_REACH: f32 = 1.45;
/// Seconds a film the scripts stop holding takes to dry at the game's normal fluid detail.
pub const FILM_LIFE: f32 = 4.0;
/// A tile nothing feeds is kept this many film lives: its film has dried by then.
const IDLE_LIVES: f32 = 3.0;
const MOST_FLUID_DETAIL: i32 = 11;
const NEAR_FOREVER: f32 = 3600.0;
/// `SCORCH_LIFE` in `liquid_layer.wesl`: a tile fire touched is kept this long, so its scorch fades out.
pub const SCORCH_LIFE: f32 = 40.0;
/// `ADDS` and `BURNING` in `liquid_layer.wesl`, the bits of `LayerSource::adds`.
pub const ADDS: u32 = 1;
pub const BURNING: u32 = 2;
/// A projector decal whose frame buffer blending is 7, which no game projector uses: the world's surfaces read it as
/// liquid. Its slot rides in bits 8 to 15.
const LIQUID_DECAL_TAG: u32 = 0x8000_0000 | 0x10_0000 | 0xf_0000 | 7;
/// How far the decal's box reaches above and below the tile's floors.
const BOX_MARGIN: f32 = 12.0;
/// A plane tile's normal snaps to `YAWS` steps of the horizontal angle and to steps of `PITCH_STEP` up or down from
/// level, from `CEILING` (straight down) to `TOP_PITCH`; its offset along the normal snaps to `PLANE_SNAP`.
const YAWS: i32 = 64;
const CEILING: i32 = -9;
const TOP_PITCH: i32 = 3;
const PITCH_STEP: f32 = TAU / 36.0;
const PLANE_SNAP: f32 = 2.0;
/// `POOL_WALL_REACH_UU` in `projector.wesl`: how far off a plane tile's plane its face may stand. A face a little off
/// the plane's snapped direction leans away from the plane across the tile.
pub const WALL_FIT: f32 = 6.0;
/// `POOL_FACE_MATCH` in `projector.wesl`: the least cosine between a face's normal and its plane tile's.
pub const FACE_MATCH: f32 = 0.9;
/// `POOL_WALL_TILE` in `projector.wesl`: the tag bit of a plane tile's decal.
pub const WALL_TILE_TAG: u32 = 0x80_0000;
/// A feed off the floor joins a tile whose plane lies within this of it.
const PLANE_JOIN: f32 = 4.0;

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

/// The level's surfaces as a plane tile meets them: the first hit between two points, its place and normal.
pub trait Surfaces {
    fn hit(&self, from: Vector, to: Vector) -> Option<(Vector, Vector)>;
}

impl Surfaces for StaticCollision {
    fn hit(&self, from: Vector, to: Vector) -> Option<(Vector, Vector)> {
        self.first_hit(from, to).filter(|hit| hit.time > 0.0).map(|hit| (hit.location, hit.normal))
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
    /// `ADDS` when it adds volume, `BURNING` when the scripts' fire burns on it.
    pub adds: u32,
    /// Picks a held source's blot; from its place in the world, so every tile it spans draws the same one.
    pub seed: u32,
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
    /// The scripts' fire burns on it: the fuel under it catches.
    pub burning: bool,
    /// The face's normal when it lands off the floor, on a wall, slope or ceiling: it feeds that plane's tiles.
    pub face: Option<Vector>,
}

impl Feeding {
    /// How far its liquid reaches from `at`: a held blot's lobes stand past its radius.
    fn reach(&self) -> f32 {
        match self.feed {
            Feed::Hold(_) => self.radius * SHAPE_REACH,
            Feed::Add(_) => self.radius,
        }
    }
}

/// The frame's work for the render world. `floors` and `cleared` are handed over once.
#[derive(Resource, Clone, ExtractResource)]
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
    /// Seconds a film the scripts stop holding takes to dry, after the game's fluid detail.
    pub film_life: f32,
}

impl Default for LayerFrame {
    fn default() -> Self {
        Self { capacity: 0, level: 0, steps: 0, slots: Vec::new(), active: Vec::new(), sources: Vec::new(), cleared: Vec::new(), floors: Vec::new(), atlas: None, film_life: FILM_LIFE }
    }
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

/// A tile on a plane: its normal in `YAWS` steps of the horizontal angle and `PITCH_STEP` steps up or down, its offset
/// along that normal in `PLANE_SNAP` steps, and a cell of the plane's grid, u level along the plane and v across it
/// (up on a wall).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlaneKey {
    pub yaw: i32,
    pub pitch: i32,
    pub offset: i32,
    pub u: i32,
    pub v: i32,
}

impl PlaneKey {
    /// The tile holding `at` on the plane through it facing `normal`.
    fn at(at: Vector, normal: Vector) -> Self {
        let yaw = (normal.y.atan2(normal.x) / (TAU / YAWS as f32)).round() as i32;
        let pitch = (normal.z.clamp(-1.0, 1.0).asin() / PITCH_STEP).round() as i32;
        Self::on(yaw, pitch, at)
    }

    /// The tile holding `at` on the plane of direction `yaw` and `pitch` through it. A ceiling has one direction.
    fn on(yaw: i32, pitch: i32, at: Vector) -> Self {
        let pitch = pitch.clamp(CEILING, TOP_PITCH);
        let yaw = if pitch == CEILING { 0 } else { yaw.rem_euclid(YAWS) };
        let key = Self { yaw, pitch, offset: 0, u: 0, v: 0 };
        let [u, v] = column(key.flat(at));
        Self { offset: (at.dot(key.normal()) / PLANE_SNAP).round() as i32, u, v, ..key }
    }

    fn normal(self) -> Vector {
        let (yaw, pitch) = (self.yaw as f32 * TAU / YAWS as f32, self.pitch as f32 * PITCH_STEP);
        Vector::new(pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin())
    }

    fn along(self) -> Vector {
        let yaw = self.yaw as f32 * TAU / YAWS as f32;
        Vector::new(-yaw.sin(), yaw.cos(), 0.0)
    }

    fn across(self) -> Vector {
        self.normal().cross(self.along())
    }

    fn plane(self) -> f32 {
        self.offset as f32 * PLANE_SNAP
    }

    /// `at` in the plane's own axes, as x and y.
    fn flat(self, at: Vector) -> Vector {
        Vector::new(at.dot(self.along()), at.dot(self.across()), 0.0)
    }

    fn corner(self) -> Vector {
        self.normal() * self.plane() + self.along() * (self.u as f32 * TILE) + self.across() * (self.v as f32 * TILE)
    }
}

/// A plane tile's offset in Bevy's (along its normal, plus `shift`) for each texel whose face matches its plane, row by
/// row; `WALL` where none does. Every texel of the face takes the plane's one offset, so liquid on it finds no slope.
pub fn trace_plane(surfaces: &impl Surfaces, key: PlaneKey, shift: f32) -> Vec<f32> {
    let (normal, along, across, corner) = (key.normal(), key.along(), key.across(), key.corner());
    (0..SIDE)
        .flat_map(|j| (0..SIDE).map(move |i| (i, j)))
        .map(|(i, j)| {
            let on = corner + along * ((i as f32 + 0.5) * TEXEL) + across * ((j as f32 + 0.5) * TEXEL);
            let face = surfaces.hit(on + normal * WALL_FIT, on - normal * WALL_FIT);
            face.filter(|(_, face)| face.dot(normal) > FACE_MATCH).map_or(WALL, |_| key.plane() + shift)
        })
        .collect()
}

struct Tile {
    slot: u32,
    /// The kind it was last fed.
    kind: FluidType,
    first_z: f32,
    last_fed: f32,
    /// When a burning feed last touched it.
    burnt: f32,
    tracing: Option<Task<Vec<f32>>>,
    /// The traced floor, Bevy heights, row by row.
    floors: Option<Arc<Vec<f32>>>,
    /// The lowest and highest traced floor, Bevy heights.
    span: Option<[f32; 2]>,
    decal: Option<Entity>,
    placed: Transform,
    /// Its decal carries the puke splat's decal as a child.
    puke: bool,
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

/// How long a fluid of `seconds` at the normal detail lives at the game's fluid `detail`, as the game scales its trails:
/// shorter below normal, a whole multiple above, near forever at the top.
pub fn fluid_life(detail: i32, seconds: f32) -> f32 {
    match detail.clamp(1, MOST_FLUID_DETAIL) {
        NORMAL_FLUID_DETAIL => seconds,
        MOST_FLUID_DETAIL => NEAR_FOREVER,
        short if short < NORMAL_FLUID_DETAIL => (NORMAL_FLUID_DETAIL + short) as f32 / 10.0 * seconds,
        long => (long - NORMAL_FLUID_DETAIL + 1) as f32 * seconds,
    }
}

#[derive(Resource, Default)]
pub struct LiquidLayer {
    tiles: FxHashMap<TileKey, Tile>,
    planes: FxHashMap<PlaneKey, Tile>,
    free: Vec<u32>,
    capacity: u32,
    level: u32,
    atlas: Option<Handle<Image>>,
    spare: f32,
    no_room: bool,
}

/// A tile puke has reached takes the puke splat's decal, which the floors colour its puke from.
pub fn mark_puke_tiles(mut layer: ResMut<LiquidLayer>, textures: Res<LiquidTextures>, mut commands: Commands) {
    let Some(splat) = &textures.puke_splat else { return };
    let LiquidLayer { tiles, planes, .. } = &mut *layer;
    for tile in tiles.values_mut().chain(planes.values_mut()) {
        if let Some(decal) = tile.decal
            && !tile.puke
            && is_puke(tile.kind)
        {
            tile.puke = true;
            commands.spawn(puke_decal(splat, decal));
        }
    }
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

/// Projects along the plane's normal, u along the decal's x and v against its y, as a floor tile's decal lies.
fn plane_decal_transform(key: PlaneKey, origin: Vec3) -> Transform {
    let bevy = |v: Vector| Vec3::new(v.x, v.z, v.y);
    let middle = key.corner() + (key.along() + key.across()) * (TILE * 0.5);
    Transform::from_translation(bevy(middle) + origin)
        .with_rotation(Quat::from_mat3(&Mat3::from_cols(bevy(key.along()), -bevy(key.across()), bevy(key.normal()))))
        .with_scale(Vec3::new(TILE, TILE, 2.0 * (WALL_FIT + TEXEL)))
}

fn layer_source(feeding: &Feeding, local: Vector) -> LayerSource {
    let (amount, adds) = match feeding.feed {
        Feed::Hold(depth) => (depth, 0),
        Feed::Add(volume) => (volume, ADDS),
    };
    let adds = if feeding.burning { adds | BURNING } else { adds };
    let [x, y, z] = [feeding.at.x, feeding.at.y, feeding.at.z].map(|v| v.round() as i32);
    let seed = (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663) ^ z.wrapping_mul(83_492_791)) as u32;
    LayerSource { x: local.x, y: local.y, radius: feeding.radius / TEXEL, amount, kind: f32::from(feeding.kind as u8), adds, seed }
}

/// Takes a tile's finished trace, and hands it to the render world.
fn take_trace(tile: &mut Tile, frame: &mut LayerFrame) {
    if tile.tracing.as_ref().is_some_and(Task::is_finished)
        && let Some(floors) = tile.tracing.take().map(block_on)
    {
        tile.span = floor_span(&floors);
        let floors = Arc::new(floors);
        tile.floors = Some(floors.clone());
        frame.floors.push((tile.slot, floors));
    }
}

/// The tile fed longest ago, if more than a second ago, and when.
fn idlest<K: Copy>(tiles: &FxHashMap<K, Tile>, now: f32) -> Option<(K, f32)> {
    tiles.iter().filter(|(_, tile)| now - tile.last_fed > 1.0).min_by(|a, b| a.1.last_fed.total_cmp(&b.1.last_fed)).map(|(&key, tile)| (key, tile.last_fed))
}

fn add_slot(frame: &mut LayerFrame, tile: &Tile, around: [i32; 9], sources: Vec<LayerSource>) {
    let (atlas_x, atlas_y) = (tile.slot % SLOTS_SIDE * SLOT_SIDE, tile.slot / SLOTS_SIDE * SLOT_SIDE);
    frame.slots[tile.slot as usize] = LayerSlot { around, first_source: frame.sources.len() as u32, sources: sources.len() as u32, atlas_x, atlas_y };
    frame.sources.extend(sources);
    frame.active.push(tile.slot);
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
        for tile in self.tiles.drain().map(|(_, tile)| tile).chain(self.planes.drain().map(|(_, tile)| tile)) {
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
        let Some(slot) = self.free_slot(level.now, commands) else { return };
        let lift = level.origin.y;
        let tracing = level.collision.cloned().map(|collision| AsyncComputeTaskPool::get().spawn(async move { trace_floor(&*collision, key, first_z, lift) }));
        self.tiles.insert(key, Tile { slot, kind: FluidType::Urine, first_z, last_fed: level.now, burnt: f32::NEG_INFINITY, tracing, floors: None, span: None, decal: None, placed: Transform::IDENTITY, puke: false });
        cleared.push(slot);
    }

    /// A free slot; the tile idle longest, floor or plane, gives way when the slots run out.
    fn free_slot(&mut self, now: f32, commands: &mut Commands) -> Option<u32> {
        // Feeds only make tiles fresher, so once none can give way none can until the frame ends.
        if self.free.is_empty() && !self.no_room {
            match (idlest(&self.tiles, now), idlest(&self.planes, now)) {
                (Some((key, fed)), plane) if plane.is_none_or(|(_, plane_fed)| fed <= plane_fed) => self.drop_tile(key, commands),
                (_, Some((key, _))) => self.drop_plane(key, commands),
                _ => self.no_room = true,
            }
        }
        self.free.pop()
    }

    fn release(&mut self, tile: Tile, commands: &mut Commands) {
        if let Some(decal) = tile.decal {
            commands.entity(decal).despawn();
        }
        self.free.push(tile.slot);
    }

    fn drop_tile(&mut self, key: TileKey, commands: &mut Commands) {
        if let Some(tile) = self.tiles.remove(&key) {
            self.release(tile, commands);
        }
    }

    fn drop_plane(&mut self, key: PlaneKey, commands: &mut Commands) {
        if let Some(tile) = self.planes.remove(&key) {
            self.release(tile, commands);
        }
    }

    /// The plane of a tile already near `at` on a face of `normal`, else the plane through `at`, so hits on one face
    /// keep to one plane though their snapped offsets or directions differ by a step.
    fn plane_near(&self, at: Vector, normal: Vector) -> PlaneKey {
        let snapped = PlaneKey::at(at, normal);
        (-1..=1)
            .flat_map(|turn| (-1..=1).flat_map(move |tilt| (-2..=2).map(move |step| (turn, tilt, step))))
            .filter_map(|(turn, tilt, step)| {
                let near = PlaneKey::on(snapped.yaw + turn, snapped.pitch + tilt, at);
                let key = PlaneKey { offset: near.offset + step, ..near };
                let off = (at.dot(key.normal()) - key.plane()).abs();
                (off <= PLANE_JOIN && self.planes.contains_key(&key)).then_some((key, off))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map_or(snapped, |(key, _)| key)
    }

    /// A plane tile at `key`, made with its face traced if there is none and there is room.
    fn plane_tile(&mut self, key: PlaneKey, level: Level, cleared: &mut Vec<u32>, commands: &mut Commands) {
        if self.planes.contains_key(&key) {
            return;
        }
        let Some(slot) = self.free_slot(level.now, commands) else { return };
        let shift = key.normal().dot(Vector::new(level.origin.x, level.origin.z, level.origin.y));
        let tracing = level.collision.cloned().map(|collision| AsyncComputeTaskPool::get().spawn(async move { trace_plane(&*collision, key, shift) }));
        self.planes.insert(key, Tile { slot, kind: FluidType::Urine, first_z: key.plane(), last_fed: level.now, burnt: f32::NEG_INFINITY, tracing, floors: None, span: None, decal: None, placed: Transform::IDENTITY, puke: false });
        cleared.push(slot);
    }

    /// A feed off the floor: tiles of the plane it lands on around it, and its source in those under it.
    fn feed_plane(&mut self, feeding: &Feeding, level: Level, by_slot: &mut [Vec<LayerSource>], cleared: &mut Vec<u32>, commands: &mut Commands) {
        let Some(normal) = feeding.face else { return };
        let plane = self.plane_near(feeding.at, normal);
        let flat = plane.flat(feeding.at);
        for [u, v] in touched(flat, feeding.reach() + ROOM) {
            self.plane_tile(PlaneKey { u, v, ..plane }, level, cleared, commands);
        }
        for [u, v] in touched(flat, feeding.reach()) {
            let key = PlaneKey { u, v, ..plane };
            let Some(tile) = self.planes.get_mut(&key) else { continue };
            (tile.last_fed, tile.kind) = (level.now, feeding.kind);
            let local = (flat - Vector::new(u as f32 * TILE, v as f32 * TILE, 0.0)) * (1.0 / TEXEL);
            by_slot[tile.slot as usize].push(layer_source(feeding, local));
        }
    }

    /// One frame: tiles for the feeds, their sources, finished floor traces, idle tiles dropped, `steps` steps.
    pub fn frame(&mut self, feeds: &[Feeding], level: Level, steps: u32, images: &mut Assets<Image>, commands: &mut Commands, frame: &mut LayerFrame) {
        let (now, lift) = (level.now, level.origin.y);
        let atlas = self.atlas.get_or_insert_with(|| new_liquid_atlas(images)).clone();
        frame.floors.clear();
        frame.cleared.clear();
        self.no_room = false;
        let mut by_slot: Vec<Vec<LayerSource>> = vec![Vec::new(); self.capacity as usize];
        for feeding in feeds {
            if feeding.face.is_some() {
                self.feed_plane(feeding, level, &mut by_slot, &mut frame.cleared, commands);
                continue;
            }
            for column in touched(feeding.at, feeding.reach() + ROOM) {
                self.tile(column, feeding.at, level, &mut frame.cleared, commands);
                if feeding.burning
                    && let Some(tile) = self.layer_near(column, feeding.at, lift, BESIDE_REACH).and_then(|key| self.tiles.get_mut(&key))
                {
                    tile.burnt = now;
                }
            }
            for column in touched(feeding.at, feeding.reach()) {
                let Some(key) = self.layer_near(column, feeding.at, lift, FEED_REACH) else { continue };
                let Some(tile) = self.tiles.get_mut(&key) else { continue };
                (tile.last_fed, tile.kind) = (now, feeding.kind);
                let local = (feeding.at - key.corner()) * (1.0 / TEXEL);
                by_slot[tile.slot as usize].push(layer_source(feeding, local));
            }
        }
        let kept = frame.film_life * IDLE_LIVES;
        let idle: Vec<TileKey> = self.tiles.iter().filter(|(_, tile)| now - tile.last_fed > kept && now - tile.burnt > SCORCH_LIFE).map(|(&key, _)| key).collect();
        for key in idle {
            self.drop_tile(key, commands);
        }
        let idle: Vec<PlaneKey> = self.planes.iter().filter(|(_, tile)| now - tile.last_fed > kept && now - tile.burnt > SCORCH_LIFE).map(|(&key, _)| key).collect();
        for key in idle {
            self.drop_plane(key, commands);
        }
        for (&key, tile) in &mut self.planes {
            take_trace(tile, frame);
            if tile.decal.is_none() {
                let decal = ClusteredDecal { metallic_roughness_texture: Some(atlas.clone()), tag: LIQUID_DECAL_TAG | WALL_TILE_TAG | tile.slot << 8, ..Default::default() };
                tile.decal = Some(commands.spawn((decal, plane_decal_transform(key, level.origin))).id());
            }
        }
        for (&key, tile) in &mut self.tiles {
            take_trace(tile, frame);
            let span = tile.span.unwrap_or([tile.first_z + lift - STEP_UP, tile.first_z + lift + STEP_UP]);
            let transform = decal_transform(key, span, level.origin);
            // An unchanged transform reinserted would mark the decal moved every frame.
            match tile.decal {
                Some(_) if tile.placed == transform => {}
                Some(decal) => {
                    commands.entity(decal).insert(transform);
                }
                None => {
                    let decal = ClusteredDecal { metallic_roughness_texture: Some(atlas.clone()), tag: LIQUID_DECAL_TAG | tile.slot << 8, ..Default::default() };
                    tile.decal = Some(commands.spawn((decal, transform)).id());
                }
            }
            tile.placed = transform;
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
            add_slot(frame, tile, around, std::mem::take(&mut by_slot[tile.slot as usize]));
        }
        for (&key, tile) in &self.planes {
            let around = std::array::from_fn(|k| {
                let beside = PlaneKey { u: key.u + k as i32 % 3 - 1, v: key.v + k as i32 / 3 - 1, ..key };
                self.planes.get(&beside).map_or(-1, |other| other.slot as i32)
            });
            add_slot(frame, tile, around, std::mem::take(&mut by_slot[tile.slot as usize]));
        }
        frame.active.sort_unstable();
        (frame.capacity, frame.level, frame.steps, frame.atlas) = (self.capacity, self.level, steps, Some(atlas.id()));
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
    use bevy::ecs::world::{CommandQueue, World};

    struct Flat(f32);

    impl Floor for Flat {
        fn floor_below(&self, from: Vector, _reach: f32) -> Option<f32> {
            (from.x < 100.0).then_some(self.0)
        }
    }

    #[test]
    fn fluid_life_follows_the_game_detail() {
        assert_eq!([1, 4, 5, 6, 10, 11].map(|detail| fluid_life(detail, 30.0)), [18.0, 27.0, 30.0, 60.0, 180.0, 3600.0]);
        assert_eq!([-3, 0, 12, 99].map(|detail| fluid_life(detail, 30.0)), [18.0, 18.0, 3600.0, 3600.0]);
        assert_eq!(fluid_life(NORMAL_FLUID_DETAIL, FILM_LIFE), 4.0);
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
        assert_eq!(shader_const(layer, "SHAPE_REACH") as f32, SHAPE_REACH);
        assert!(f64::from(SHAPE_REACH) >= 1.0 + shader_const(layer, "SHAPE_LOBE") + shader_const(layer, "SHAPE_BUMP"), "the farthest lobe lies within the reach");
    }

    #[test]
    fn a_held_source_draws_one_blot_in_every_tile_it_spans() {
        let puddle = Feeding { at: Vector::new(250.4, -30.2, 12.0), radius: 20.0, kind: FluidType::Blood, feed: Feed::Hold(FILM), burning: false, face: None };
        let (here, beside) = (layer_source(&puddle, Vector::new(125.2, 49.0, 0.0)), layer_source(&puddle, Vector::new(-2.8, 49.0, 0.0)));
        assert_eq!(here.seed, beside.seed);
        let other = layer_source(&Feeding { at: Vector::new(260.0, -30.0, 12.0), ..puddle }, Vector::ZERO);
        assert_ne!(here.seed, other.seed);
        assert_eq!((puddle.reach(), Feeding { feed: Feed::Add(5.0), ..puddle }.reach()), (20.0 * SHAPE_REACH, 20.0));
    }

    #[test]
    fn the_shaders_agree_on_the_fire() {
        let layer = include_str!("../liquid_layer.wesl");
        let projector = include_str!("../projector.wesl");
        assert_eq!(shader_const(layer, "SCORCH_LIFE") as f32, SCORCH_LIFE);
        assert_eq!((shader_const(layer, "ADDS") as u32, shader_const(layer, "BURNING") as u32), (ADDS, BURNING));
        assert_eq!(shader_const(layer, "FIRE_BASE"), shader_const(projector, "POOL_FIRE_BASE"));
        assert_eq!(shader_const(layer, "LEVELS"), shader_const(projector, "POOL_FIRE_LEVELS"));
        let kinds = [("GAS", FluidType::Gas), ("URINE", FluidType::Urine), ("BLOODY_URINE", FluidType::BloodyUrine), ("NAPALM", FluidType::Napalm)];
        for (name, kind) in kinds {
            assert_eq!(shader_const(layer, name), f64::from(kind as u8), "{name}");
        }
        let (kinds, levels, base) = (shader_const(layer, "KINDS"), shader_const(layer, "LEVELS"), shader_const(layer, "FIRE_BASE"));
        assert!(kinds > f64::from(FluidType::Napalm as u8) && levels < 32.0, "kind and scorch fit their places");
        assert!(base + levels * 33.0 < 2048.0, "a half float holds every fire code exactly");
        assert!((levels * 33.0 + 1.0) * kinds <= 16384.0, "the blood share keeps its precision above the fire");
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

    struct Face(Vector);

    impl Surfaces for Face {
        fn hit(&self, from: Vector, to: Vector) -> Option<(Vector, Vector)> {
            (from.x < 100.0 && from.y < 50.0 && to.y >= 50.0).then(|| (from + (to - from) * ((50.0 - from.y) / (to.y - from.y)), self.0))
        }
    }

    /// A flat face through the plane of `normal` at `offset`, met from its front.
    struct Slab(Vector, f32);

    impl Surfaces for Slab {
        fn hit(&self, from: Vector, to: Vector) -> Option<(Vector, Vector)> {
            let (a, b) = (from.dot(self.0) - self.1, to.dot(self.0) - self.1);
            (a > 0.0 && b <= 0.0).then(|| (from + (to - from) * (a / (a - b)), self.0))
        }
    }

    #[test]
    fn plane_keys_snap_walls_ceilings_and_overhangs() {
        let wall = PlaneKey::at(Vector::new(10.0, 50.6, 100.0), Vector::new(0.0, -1.0, 0.0));
        assert_eq!(wall, PlaneKey { yaw: 48, pitch: 0, offset: -25, u: 0, v: 0 });
        assert!((wall.across() - Vector::new(0.0, 0.0, 1.0)).size() < 1e-5, "v runs up a wall");
        let ceiling = PlaneKey::at(Vector::new(10.0, 20.0, 300.0), Vector::new(0.3, 0.0, -0.999));
        assert_eq!(ceiling, PlaneKey { yaw: 0, pitch: -9, offset: -150, u: 0, v: 0 });
        let tilt = 40_f32.to_radians();
        let overhang = PlaneKey::at(Vector::new(10.0, 20.0, 30.0), Vector::new(tilt.cos(), 0.0, -tilt.sin()));
        assert_eq!((overhang.yaw, overhang.pitch), (0, -4));
        assert!((overhang.normal() - Vector::new(tilt.cos(), 0.0, -tilt.sin())).size() < 1e-5);
    }

    #[test]
    fn a_plane_decal_projects_along_the_normal_from_its_low_corner() {
        let wall = PlaneKey::at(Vector::new(10.0, 50.6, 100.0), Vector::new(0.0, -1.0, 0.0));
        let corner = wall.corner();
        assert!(corner.x.abs() < 1e-3 && (corner.y - 50.0).abs() < 1e-3 && corner.z.abs() < 1e-3);
        let transform = plane_decal_transform(wall, Vec3::new(1.0, 2.0, 3.0));
        assert!(transform.rotation.mul_vec3(Vec3::Z).abs_diff_eq(Vec3::NEG_Z, 1e-5));
        assert!(transform.transform_point(Vec3::new(-0.5, 0.5, 0.0)).abs_diff_eq(Vec3::new(1.0, 2.0, 53.0), 1e-3), "texel 0 at the low corner, as on a floor");
        let ceiling = PlaneKey::at(Vector::new(10.0, 20.0, 300.0), Vector::new(0.0, 0.0, -1.0));
        let transform = plane_decal_transform(ceiling, Vec3::ZERO);
        assert!(transform.rotation.mul_vec3(Vec3::Z).abs_diff_eq(Vec3::NEG_Y, 1e-5), "a ceiling's decal projects up into it");
        assert!(transform.transform_point(Vec3::new(-0.5, 0.5, 0.0)).abs_diff_eq(Vec3::new(0.0, 300.0, 0.0), 1e-3));
        let projector = include_str!("../projector.wesl");
        assert_eq!(shader_const(projector, "POOL_WALL_REACH_UU") as f32, WALL_FIT);
        assert_eq!(WALL_FIT, 6.0);
        assert_eq!(shader_const(projector, "POOL_FACE_MATCH") as f32, FACE_MATCH);
        assert_eq!(shader_const(projector, "POOL_WALL_TILE") as u32, WALL_TILE_TAG);
        assert_eq!(WALL_TILE_TAG & (LIQUID_DECAL_TAG | 0xff00 | shader_const(projector, "PUKE_SPLAT") as u32), 0);
    }

    #[test]
    fn a_wall_is_traced_on_its_face_and_nothing_else() {
        let key = PlaneKey::at(Vector::new(10.0, 50.0, 100.0), Vector::new(0.0, -1.0, 0.0));
        let faces = trace_plane(&Face(Vector::new(0.0, -1.0, 0.0)), key, 3.0);
        assert_eq!(faces.len(), TEXELS);
        assert_eq!((faces[0], faces[49], faces[SIDE as usize * 7 + 3]), (-47.0, -47.0, -47.0), "one offset over the face");
        assert_eq!(faces[50], WALL, "texel 50's middle lies at x 101");
        assert!(trace_plane(&Face(Vector::new(0.0, 0.0, 1.0)), key, 3.0).iter().all(|&offset| offset >= WALL), "a floor is no wall's face");
    }

    #[test]
    fn a_ceiling_and_an_overhang_are_traced_where_they_were_hit() {
        let at = Vector::new(10.0, 20.0, 30.0);
        let overhang = Vector::new(1.0, 0.0, -1.0) * std::f32::consts::FRAC_1_SQRT_2;
        for normal in [Vector::new(0.0, 0.0, -1.0), overhang] {
            let key = PlaneKey::at(at, normal);
            let faces = trace_plane(&Slab(normal, at.dot(normal)), key, 0.0);
            let local = (key.flat(at) - Vector::new(key.u as f32 * TILE, key.v as f32 * TILE, 0.0)) * (1.0 / TEXEL);
            let texel = local.y.floor() as usize * SIDE as usize + local.x.floor() as usize;
            assert_eq!(faces[texel], key.plane(), "the face under the hit at {normal:?}");
            assert!(trace_plane(&Slab(-normal, -at.dot(normal)), key, 0.0).iter().all(|&offset| offset >= WALL), "a face turned away is not this plane's");
        }
    }

    #[test]
    fn a_floor_feed_makes_floor_tiles_and_others_plane_tiles() {
        let world = World::new();
        let mut queue = CommandQueue::default();
        let mut commands = Commands::new(&mut queue, &world);
        let mut images = Assets::<Image>::default();
        let (mut layer, mut frame) = (LiquidLayer::default(), LayerFrame::default());
        layer.reset(&mut commands, 16);
        let level = Level { collision: None, origin: Vec3::ZERO, now: 0.0 };
        let drop = Feeding { at: Vector::new(10.0, 10.0, 0.0), radius: 3.0, kind: FluidType::Blood, feed: Feed::Add(5.0), burning: false, face: None };
        layer.frame(&[drop], level, 0, &mut images, &mut commands, &mut frame);
        assert!(layer.tiles.contains_key(&TileKey { x: 0, y: 0, layer: 0 }) && layer.planes.is_empty());
        let floors = layer.tiles.len();
        let splat = Feeding { at: Vector::new(10.0, 50.6, 100.0), face: Some(Vector::new(0.0, -1.0, 0.0)), ..drop };
        layer.frame(&[splat], level, 0, &mut images, &mut commands, &mut frame);
        let key = PlaneKey { yaw: 48, pitch: 0, offset: -25, u: 0, v: 0 };
        assert_eq!((layer.tiles.len(), layer.planes.contains_key(&key)), (floors, true));
        assert_eq!(frame.slots[layer.planes[&key].slot as usize].sources, 1);
        let planes = layer.planes.len();
        layer.frame(&[Feeding { at: Vector::new(10.0, 51.2, 100.0), ..splat }], level, 0, &mut images, &mut commands, &mut frame);
        assert_eq!(layer.planes.len(), planes, "a hit a step off the plane joins its tiles");
        let ceiling = Feeding { at: Vector::new(10.0, 20.0, 300.0), face: Some(Vector::new(0.0, 0.0, -1.0)), ..drop };
        layer.frame(&[ceiling], level, 0, &mut images, &mut commands, &mut frame);
        assert!(layer.planes.contains_key(&PlaneKey { yaw: 0, pitch: -9, offset: -150, u: 0, v: 0 }));
        assert_eq!(layer.tiles.len(), floors, "a ceiling is no floor");
    }

    #[test]
    fn a_full_layer_gives_a_slot_again_once_a_tile_idles() {
        let world = World::new();
        let mut queue = CommandQueue::default();
        let mut commands = Commands::new(&mut queue, &world);
        let mut images = Assets::<Image>::default();
        let (mut layer, mut frame) = (LiquidLayer::default(), LayerFrame::default());
        layer.reset(&mut commands, 1);
        let at = |now: f32| Level { collision: None, origin: Vec3::ZERO, now };
        let near = Feeding { at: Vector::new(128.0, 128.0, 0.0), radius: 3.0, kind: FluidType::Blood, feed: Feed::Add(5.0), burning: false, face: None };
        let far = Feeding { at: Vector::new(128.0 + 10.0 * TILE, 128.0, 0.0), ..near };
        let (near_key, far_key) = (TileKey { x: 0, y: 0, layer: 0 }, TileKey { x: 10, y: 0, layer: 0 });
        layer.frame(&[near], at(0.0), 0, &mut images, &mut commands, &mut frame);
        layer.frame(&[far, near, far], at(0.5), 0, &mut images, &mut commands, &mut frame);
        assert!(layer.no_room && layer.tiles.contains_key(&near_key) && !layer.tiles.contains_key(&far_key));
        layer.frame(&[far], at(2.0), 0, &mut images, &mut commands, &mut frame);
        assert!(!layer.no_room && layer.tiles.contains_key(&far_key) && !layer.tiles.contains_key(&near_key));
    }

    #[test]
    fn a_disc_touches_the_tiles_it_overlaps() {
        assert_eq!(touched(Vector::new(10.0, 10.0, 0.0), 5.0).count(), 1);
        let columns: Vec<[i32; 2]> = touched(Vector::new(2.0, 250.0, 0.0), 20.0).collect();
        assert_eq!(columns.len(), 4);
        assert!(columns.contains(&[-1, 1]));
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
