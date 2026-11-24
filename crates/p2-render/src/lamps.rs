//! Modern lamps: a pixel is lit by the lamps listed for its cell of the map ([`cells`]), read from a book the shaders
//! take as a texture. The static world's shadows come from one atlas drawn once a map ([`atlas`]), finer where a face
//! would show its texels ([`faces`]); what moves casts into tiles of its own ([`movers`]).

pub(crate) mod atlas;
mod cells;
mod faces;
mod grounding;
mod mover_pass;
mod mover_pick;
mod movers;
mod tile_grid;

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bevy::app::{App, Plugin, PostUpdate, Update};
use bevy::asset::{Asset, AssetEvent, AssetEventSystems, Assets, Handle, RenderAssetUsages};
use bevy::camera::visibility::{InheritedVisibility, RenderLayers, Visibility, VisibilitySystems};
use bevy::camera::{Camera, Projection};
use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::message::{MessageCursor, Messages};
use bevy::ecs::prelude::{Commands, Component, Entity, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, StandardMaterial};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::renderer::RenderDevice;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::shader::Shader;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use bevy::transform::TransformSystems;
use bevy::transform::components::Transform;
use p2_assets::Model;
use p2_render_cpu::{LE_SPOTLIGHT, LE_STATIC_SPOT, LE_SUNLIGHT, MeshLight};

use crate::components::{MOVER_LAYER, RESTING_PROP_LAYER, SUN_VIEW_LAYER, SceneView};
use p2_host::display::{MODERN_LAMP_SHADOWS, Settings};
use crate::level_collision::{LevelCollision, StaticCollision};
use crate::view::SeenWithin;
use crate::scene::ZoneCulled;
use crate::actor_light::{HardwareLighting, LightEnvironment};
use crate::blended::{OpaqueDecals, TerrainLayer};
use crate::bsp_light::{gpu_light, place};
use crate::lightmapped::BakedLight;
use crate::mesh_light::DynamicLit;
use crate::modern::linear_light;
use crate::shaders::add_shaders;
use crate::terrain_material::TerrainLayers;
use crate::view::ShownScene;
use atlas::{ATLAS_SHADER, ATLAS_UNITS, BookRecords, LampBake, PlainTilesDrawn, UNMAPPED, bytes_of, draw_lamp_atlas, lamp_bake, new_atlas, world_masked, world_triangles};
use cells::{BOOK_WIDTH, CellLamp, RECORD_TEXELS, TILE_GRID_FROM_END, book_texels, lamp_cells};
use faces::{Eye, FaceBook, FaceLamp, FaceSlot};
use mover_pass::{MOVER_CASTER_SHADER, draw_mover_tiles};
use mover_pick::pick_mover_tiles;
pub use movers::MoverTiles;

pub const LAMP_SHADOW_SPAN: &str = "lamp_shadows";

/// Where `lamp_shadow.wesl`'s fade ends. A far lamp's pool covers few pixels but costs every cluster and list on its way.
const LAMP_SEEN_RADII: f32 = 30.0;
/// Without an atlas a lamp lights through walls and a cell lists every lamp in reach, so the cells are larger.
const SHADOWED_CELL: f32 = 128.0;
const UNSHADOWED_CELL: f32 = 512.0;
/// Left to the game while the lists are made: the slowest machine it must run on has four threads.
const THREADS_KEPT: usize = 2;
const ITEMS_PER_BATCH: usize = 8;

/// `work` on each of `count` items, on `threads` threads, the results in the items' order.
fn on_threads<T: Send>(count: usize, threads: usize, work: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, T)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.max(1))
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let start = next.fetch_add(ITEMS_PER_BATCH, Ordering::Relaxed);
                        if start >= count {
                            return done;
                        }
                        done.extend((start..count.min(start + ITEMS_PER_BATCH)).map(|item| (item, work(item))));
                    }
                })
            })
            .collect();
        workers.into_iter().flat_map(|worker| worker.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))).collect()
    });
    done.sort_unstable_by_key(|&(item, _)| item);
    done.into_iter().map(|(_, result)| result).collect()
}

/// Set above the tile in a lamp's colour texel when the lamp has a size of its own. Under 2^24, so it stays exact as a
/// float. `lamp_shadow.wesl` holds the same number.
const BOOK_SIZED: u32 = 1 << 20;

/// A lamp's texels in the book, as `lamp_shadow.wesl` reads them: place and radius (negative for a spot); linear colour
/// and atlas tile; a spot's direction x and y, its cone's scale signed as the direction's z, and the cone's offset.
/// The next three are written by the passes: two of detail squares, one of where its movers' tiles lie. The last holds
/// the lamp's size, which the shader reads only when the tile carries `BOOK_SIZED`: the lamp has a size of its own.
fn lamp_record(light: &MeshLight, level_brightness: f32, origin: Vec3, tile: u32) -> [[f32; 4]; RECORD_TEXELS] {
    let (at, spot) = scene_place(light, origin);
    let [r, g, b] = linear_light(light.color(level_brightness));
    let sized = if light.corona > 0.0 { BOOK_SIZED } else { 0 };
    let look = [r, g, b, (tile | sized) as f32];
    let size = [crate::lamp_size::lamp_size(light.world_radius(), Some(light.corona), None), 0.0, 0.0, 0.0];
    let Some(along) = spot else {
        return [[at.x, at.y, at.z, light.world_radius()], look, [0.0; 4], [0.0; 4], [0.0; 4], [0.0; 4], size];
    };
    let scale = 1.0 / (1.0 - light.cone_sine()).max(1e-4);
    [[at.x, at.y, at.z, -light.world_radius()], look, [along.x, along.y, scale.copysign(along.z), -light.cone_sine() * scale], [0.0; 4], [0.0; 4], [0.0; 4], size]
}

/// A lamp's place in the scene and, for a spot, the way it shines.
fn scene_place(light: &MeshLight, origin: Vec3) -> (Vec3, Option<Vec3>) {
    let spot = matches!(light.effect, LE_STATIC_SPOT | LE_SPOTLIGHT).then(|| Vec3::from(p2_bevy_assets::to_bevy(light.rotation.vector())).normalize_or_zero());
    (Vec3::from(p2_bevy_assets::to_bevy(light.location)) + origin, spot)
}

/// The first scene camera's place, and how wide a texel may be a unit of distance away for it to cover `texel_pixels`.
fn scene_eye(cameras: &Query<(&Transform, &Projection, &Camera), With<SceneView>>, texel_pixels: f32) -> Option<Eye> {
    let Some((eye, Projection::Perspective(lens), camera)) = cameras.iter().next() else { return None };
    let height = camera.physical_viewport_size().map(|size| size.y as f32).filter(|&height| height > 0.0)?;
    Some(Eye { at: eye.translation, texel_spread: 2.0 * (0.5 * lens.fov).tan() / height * texel_pixels })
}

const FACE_PICK_FRAMES: u32 = 4;
/// A mover's tile fades in and out over this long.
const MOVER_FADE_SECONDS: f32 = 0.3;

/// The largest texture the GPU takes, once the renderer is known.
static MOST_ATLAS_SIDE: AtomicU32 = AtomicU32::new(u32::MAX);

/// An atlas side of 0 makes no atlas: lamps light without a shadow.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LampQuality {
    atlas_side: u32,
    /// How many pixels of the screen a texel of a detail face may cover.
    texel_pixels: f32,
    /// How many texels of new detail faces one pick may ask the atlas pass to draw.
    texels_a_pick: u32,
}

fn lamp_quality(level: u8) -> LampQuality {
    let (atlas_side, texel_pixels, texels_a_pick) = match level {
        0 => (0, 2.0, 0),
        1 | 2 => (4096, 2.0, 1 << 20),
        3 => (8192, 1.5, 1 << 21),
        _ => (16384, 1.0, 1 << 23),
    };
    LampQuality { atlas_side: atlas_side.min(MOST_ATLAS_SIDE.load(Ordering::Relaxed)), texel_pixels, texels_a_pick }
}

pub(crate) fn is_lamp(light: &MeshLight) -> bool {
    !light.dynamic && !light.special_lit && light.effect != LE_SUNLIGHT && light.color(1.0).iter().sum::<f32>() > 0.0
}

pub(crate) fn map_lamps(environment: &LightEnvironment) -> Vec<MeshLight> {
    let mut lamps: Vec<MeshLight> = environment.lights.values().filter(|light| is_lamp(light)).copied().collect();
    lamps.sort_by(|a, b| <[f32; 3]>::from(a.location).partial_cmp(&<[f32; 3]>::from(b.location)).unwrap_or(std::cmp::Ordering::Equal));
    lamps
}

#[derive(Component)]
pub struct Lamp;

#[derive(Component)]
pub(crate) struct Placed {
    number: u32,
    strength: f32,
    /// Linear light.
    pub(crate) color: Vec3,
    pub(crate) range: f32,
    /// A spot's direction and the cosine of its cone's half angle.
    pub(crate) spot: Option<(Vec3, f32)>,
    /// How far the static world it lights lies from it through each face of its tile.
    reach: [f32; 6],
}

/// As the shader lights a point: 0 outside the lamp's range or a spot's cone.
pub(crate) fn lamp_falloff(lamp_at: Vec3, range: f32, spot: Option<(Vec3, f32)>, point: Vec3) -> f32 {
    let to_point = point - lamp_at;
    let squared = to_point.length_squared();
    if squared >= range * range {
        return 0.0;
    }
    let x = squared.sqrt() / range;
    let cone = spot.map_or(1.0, |(along, cos)| {
        let share = ((along.dot(to_point.normalize_or_zero()) - cos) / (1.0 - cos).max(1e-4)).clamp(0.0, 1.0);
        share * share
    });
    (1.0 - x * x * (3.0 - 2.0 * x)) * cone
}

/// The linear light of the placed lamps at a point, of those in range that `reaches` says light it.
pub(crate) fn lamp_sum<'a>(lamps: impl IntoIterator<Item = (Vec3, &'a Placed)>, point: Vec3, reaches: impl Fn(Vec3) -> bool) -> Vec3 {
    let lit = lamps.into_iter().map(|(at, placed)| (at, placed.color * lamp_falloff(at, placed.range, placed.spot, point)));
    lit.filter(|&(at, light)| light != Vec3::ZERO && reaches(at)).map(|(_, light)| light).sum()
}

#[derive(Resource, Default)]
struct LampBook {
    image: Option<Handle<Image>>,
    lamps: usize,
}

#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub struct LampAtlas {
    image: Option<Handle<Image>>,
    bake: Option<Arc<LampBake>>,
    side: u32,
    book: Option<Handle<Image>>,
    faces: Vec<FaceSlot>,
}

impl LampAtlas {
    pub fn has_detail_faces(&self) -> bool {
        !self.faces.is_empty()
    }
}

/// The squares of the atlas's last quarter, which the lamps' detail faces and the movers' tiles share. A new atlas
/// starts them anew, under the next `made` number.
#[derive(Resource, Default)]
struct DetailSquares {
    bake: usize,
    made: u32,
    book: Option<FaceBook>,
}

struct LampJobResult {
    /// Each lamp with the zones it could light (none known for a lamp inside solid) and its reach through each face.
    lamps: Vec<(MeshLight, Vec<u8>, [f32; 6])>,
    bake: Option<LampBake>,
    book: Vec<[f32; 4]>,
    listed: String,
}

#[derive(Default)]
struct LampJob {
    key: Option<(usize, usize, u32)>,
    task: Option<Task<LampJobResult>>,
    ready: Option<LampJobResult>,
}

fn lamp_job(environment: &LightEnvironment, collision: &StaticCollision, triangles: &[[[f32; 3]; 3]], origin: Vec3, side: u32) -> LampJobResult {
    let lamps = map_lamps(environment);
    let spheres: Vec<_> = lamps.iter().map(|light| (scene_place(light, origin).0.to_array(), light.world_radius())).collect();
    let threads = std::thread::available_parallelism().map_or(1, |threads| threads.get().saturating_sub(THREADS_KEPT).max(1));
    let bake = (side > 0).then(|| lamp_bake(triangles, &world_masked(collision, origin), &spheres, threads));
    let records: Vec<_> = lamps.iter().enumerate().map(|(index, light)| lamp_record(light, environment.level_brightness, origin, bake.as_ref().map_or(UNMAPPED, |bake| bake.tile_bits(index)))).collect();
    let cells = {
        let drawn: Vec<_> = bake.as_ref().map_or_else(|| vec![None; lamps.len()], |bake| bake.drawn(side / ATLAS_UNITS).collect());
        let cell_lamps: Vec<_> = records.iter().zip(drawn).map(|(record, drawn)| CellLamp::of(record, drawn.map(|(face, _)| face), drawn.map_or(&[], |(_, casters)| casters))).collect();
        lamp_cells(bake.as_ref().map_or(&[], |bake| &bake.vertices), &cell_lamps, if side > 0 { SHADOWED_CELL } else { UNSHADOWED_CELL }, threads)
    };
    let lit = cells.starts.windows(2).filter(|pair| pair[1] > pair[0]).count().max(1);
    let listed = format!(
        "cells of {} units, {:.1} lamps a lit cell, {} of the {} lamp and cell pairs in reach kept",
        cells.cell,
        cells.lamps.len() as f32 / lit as f32,
        cells.lamps.len(),
        cells.in_reach
    );
    let book = book_texels(&cells, &records);
    let zones = on_threads(lamps.len(), threads, |lamp| p2_collision::light_zones(&collision.model, lamps[lamp].location, lamps[lamp].world_radius()));
    let lamps = lamps.into_iter().zip(zones).zip(cells.reach).map(|((light, zones), reach)| (light, zones, reach)).collect();
    LampJobResult { lamps, bake, book, listed }
}

/// Float texels in a slot Bevy binds as filterable: every desktop GPU filters them, and Bevy asks for all a GPU offers.
/// More rows after the texels hold the movers' tile grid, their tiles and their capsules in two grids.
fn book_image(texels: &[[f32; 4]]) -> Image {
    let rows = texels.len().div_ceil(BOOK_WIDTH).max(1) + TILE_GRID_FROM_END as usize;
    let mut bytes = bytes_of(texels);
    bytes.resize(rows * BOOK_WIDTH * 16, 0);
    Image::new(Extent3d { width: BOOK_WIDTH as u32, height: rows as u32, depth_or_array_layers: 1 }, TextureDimension::D2, bytes, TextureFormat::Rgba32Float, RenderAssetUsages::RENDER_WORLD)
}

/// The modern switch, the lamp shadow level and the indirect light level in force, for the loading thread.
static LOAD_LEVELS: AtomicU32 = AtomicU32::new(0);
const LOAD_MODERN: u32 = 1 << 16;

pub(crate) fn set_load_light_levels(settings: Res<Settings>) {
    let levels = u32::from(settings.modern_level(MODERN_LAMP_SHADOWS)) | u32::from(settings.modern_level(p2_host::display::MODERN_INDIRECT)) << 8;
    LOAD_LEVELS.store(if settings.modern_graphics() { levels | LOAD_MODERN } else { 0 }, Ordering::Relaxed);
}

/// A bake kept with the setting it was made for, taken once by the system that would otherwise bake it.
pub(crate) struct Prebaked<K, T>(Mutex<Option<(K, T)>>);

impl<K, T> Default for Prebaked<K, T> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl<K: PartialEq, T> Prebaked<K, T> {
    pub(crate) fn new(key: K, baked: T) -> Self {
        Self(Mutex::new(Some((key, baked))))
    }

    pub(crate) fn take(&self, key: &K) -> Option<T> {
        self.0.lock().ok()?.take_if(|(baked_for, _)| baked_for == key).map(|(_, baked)| baked)
    }
}

/// The lamps' shadow atlas and the probes, baked while the map loads so its light is there on the first frame.
#[derive(Default)]
pub struct LightBakes {
    lamps: Prebaked<u32, LampJobResult>,
    pub(crate) probes: crate::probes::ProbePrebake,
}

pub(crate) fn bake_for_load(environment: &LightEnvironment, collision: &StaticCollision, origin: Vec3) -> Option<Arc<LightBakes>> {
    let levels = LOAD_LEVELS.load(Ordering::Relaxed);
    if levels & LOAD_MODERN == 0 {
        return None;
    }
    let side = lamp_quality((levels & 255) as u8).atlas_side;
    let world = world_triangles(collision, origin);
    let (lamps, probes) = std::thread::scope(|scope| {
        let probes = scope.spawn(|| crate::probes::prebake(environment, collision, &world, origin, (levels >> 8 & 255) as u8));
        (lamp_job(environment, collision, &world.triangles, origin, side), probes.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
    });
    Some(Arc::new(LightBakes { lamps: Prebaked::new(side, lamps), probes }))
}

/// A lamp that could light no zone the eye can see costs nothing: zone culling hides it, as it hides meshes. A lamp
/// inside solid names no zone and takes every zone its box touches.
fn lamp_zone(light: &MeshLight, model: &Model, lit_zones: &[u8]) -> ZoneCulled {
    let (center, radius) = (<[f32; 3]>::from(light.location), light.world_radius());
    let (min, max) = (center.map(|axis| axis - radius), center.map(|axis| axis + radius));
    let zones = if lit_zones.is_empty() { p2_collision::box_zones(model, min.into(), max.into()) } else { lit_zones.to_vec() };
    ZoneCulled { zones, min, max, only_from: None }
}

fn spawn_lamp(commands: &mut Commands, index: usize, light: &MeshLight, reach: [f32; 6], environment: &LightEnvironment, origin: Vec3, zone: ZoneCulled) {
    let Some(gpu) = gpu_light(light, environment.level_brightness) else { return };
    let at = place(light, &gpu, origin);
    let [r, g, b] = linear_light(light.color(environment.level_brightness));
    let spot = scene_place(light, origin).1.map(|along| (along, light.cone_sine()));
    let placed = Placed { number: index as u32, strength: r + g + b, color: Vec3::new(r, g, b), range: light.world_radius(), spot, reach };
    let seen = (Lamp, placed, at, Visibility::default(), RenderLayers::from_layers(&[SUN_VIEW_LAYER, MOVER_LAYER, RESTING_PROP_LAYER]), zone, SeenWithin(light.world_radius() * LAMP_SEEN_RADII));
    commands.spawn(seen);
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn follow_lamps(
    settings: Res<Settings>,
    shown: Option<Res<ShownScene>>,
    level: Option<Res<LevelCollision>>,
    mut atlas: ResMut<LampAtlas>,
    mut book: ResMut<LampBook>,
    mut job: Local<LampJob>,
    mut images: ResMut<Assets<Image>>,
    mut commands: Commands,
    lamps: Query<Entity, With<Lamp>>,
) {
    let modern = settings.modern_graphics();
    let quality = lamp_quality(settings.modern_level(MODERN_LAMP_SHADOWS));
    let environment = shown.as_deref().and_then(|shown| shown.lighting.clone()).filter(|_| modern);
    let collision = level.as_ref().and_then(|level| level.0.clone());
    let key = environment.as_ref().zip(collision.as_ref()).map(|(environment, collision)| (Arc::as_ptr(environment).addr(), Arc::as_ptr(collision).addr(), quality.atlas_side));
    if job.key != key {
        job.key = key;
        job.task = None;
        job.ready = None;
        atlas.bake = None;
        atlas.book = None;
        atlas.faces.clear();
        book.image = None;
        let side = key.map_or(0, |(_, _, side)| side);
        if atlas.side != side {
            atlas.side = side;
            atlas.image = (side > 0).then(|| new_atlas(&mut images, [side, side], TextureFormat::R16Float));
        }
        for entity in &lamps {
            commands.entity(entity).despawn();
        }
        if let (Some(environment), Some(collision), Some(shown)) = (environment.clone(), collision, shown.as_deref()) {
            let origin = shown.origin;
            job.ready = shown.light_bakes.as_ref().and_then(|bakes| bakes.lamps.take(&side));
            if job.ready.is_none() {
                job.task = Some(AsyncComputeTaskPool::get().spawn(async move { lamp_job(&environment, &collision, &if side > 0 { world_triangles(&collision, origin).triangles } else { Vec::new() }, origin, side) }));
            }
        }
    }
    if job.ready.is_none() && !job.task.as_ref().is_some_and(Task::is_finished) {
        return;
    }
    let (Some(environment), Some(shown), Some(model)) = (environment, shown.as_deref(), level.as_ref().and_then(|level| level.model())) else { return };
    let Some(LampJobResult { lamps: map, bake, book: texels, listed }) = job.ready.take().or_else(|| job.task.take().map(block_on)) else { return };
    for (index, (light, zones, reach)) in map.iter().enumerate() {
        spawn_lamp(&mut commands, index, light, *reach, &environment, shown.origin, lamp_zone(light, model, zones));
    }
    if !map.is_empty() {
        let zones: usize = map.iter().map(|(_, zones, _)| zones.len()).sum();
        let unknown = map.iter().filter(|(_, zones, _)| zones.is_empty()).count();
        bevy::log::info!("modern lamps: {}, {:.1} zones a lamp on average, {unknown} inside solid with no zone known; {listed}", map.len(), zones as f32 / map.len() as f32);
    }
    book.image = Some(images.add(book_image(&texels)));
    atlas.book.clone_from(&book.image);
    book.lamps = map.len();
    atlas.bake = bake.map(|mut bake| {
        bake.keep_record_rows(&texels);
        Arc::new(bake)
    });
}

/// One spawned as another goes leaves the count the same, so a count alone misses it.
pub(crate) fn material_added<A: Asset>(events: Option<&Messages<AssetEvent<A>>>, cursor: &mut MessageCursor<AssetEvent<A>>) -> bool {
    events.is_some_and(|events| cursor.read(events).fold(false, |added, event| added | matches!(event, AssetEvent::Added { .. })))
}

/// Our shaders replace Bevy's lighting, the only reader of these slots, so the atlas needs no new binding.
#[derive(Clone, Copy)]
pub(crate) enum MaterialSlot {
    Occlusion,
    MetallicRoughness,
    Anisotropy,
    /// Bevy reads it only for parallax, compiled out without vertex tangents: a mesh that gains tangents would shift its
    /// UVs by this image as a height map.
    DepthMap,
}

impl MaterialSlot {
    fn of(self, material: &StandardMaterial) -> &Option<Handle<Image>> {
        match self {
            Self::Occlusion => &material.occlusion_texture,
            Self::MetallicRoughness => &material.metallic_roughness_texture,
            Self::Anisotropy => &material.anisotropy_texture,
            Self::DepthMap => &material.depth_map,
        }
    }

    fn of_mut(self, material: &mut StandardMaterial) -> &mut Option<Handle<Image>> {
        match self {
            Self::Occlusion => &mut material.occlusion_texture,
            Self::MetallicRoughness => &mut material.metallic_roughness_texture,
            Self::Anisotropy => &mut material.anisotropy_texture,
            Self::DepthMap => &mut material.depth_map,
        }
    }
}

pub(crate) trait MaterialAtlas: Resource {
    const SLOT: MaterialSlot;

    fn wanted(&self) -> Option<Handle<Image>>;
}

impl MaterialAtlas for LampAtlas {
    const SLOT: MaterialSlot = MaterialSlot::Occlusion;

    fn wanted(&self) -> Option<Handle<Image>> {
        self.bake.as_ref().and(self.image.clone())
    }
}

/// Bevy reads this slot only for a material with anisotropy, and none of ours has any.
impl MaterialAtlas for LampBook {
    const SLOT: MaterialSlot = MaterialSlot::Anisotropy;

    fn wanted(&self) -> Option<Handle<Image>> {
        self.image.clone().filter(|_| self.lamps > 0)
    }
}

fn follow_atlas_binding<A: MaterialAtlas, E: MaterialExtension>(
    atlas: Res<A>,
    materials: Option<ResMut<Assets<ExtendedMaterial<StandardMaterial, E>>>>,
    events: Option<Res<Messages<AssetEvent<ExtendedMaterial<StandardMaterial, E>>>>>,
    mut cursor: Local<MessageCursor<AssetEvent<ExtendedMaterial<StandardMaterial, E>>>>,
    mut known: Local<usize>,
    mut bound: Local<Option<Handle<Image>>>,
) {
    let moved = atlas.is_changed() && atlas.wanted() != *bound;
    let Some(mut materials) = materials else { return };
    if !moved && materials.len() == *known && !material_added(events.as_deref(), &mut cursor) {
        return;
    }
    *known = materials.len();
    let wanted = atlas.wanted();
    let stale: Vec<_> = materials.iter().filter(|(_, material)| *A::SLOT.of(&material.base) != wanted).map(|(id, _)| id).collect();
    for id in stale {
        if let Some(mut material) = materials.get_mut(id) {
            A::SLOT.of_mut(&mut material.base).clone_from(&wanted);
        }
    }
    *bound = wanted;
}

/// Before the frame's asset events go out, so a new material reaches the GPU once, already bound.
pub(crate) fn follow_atlas<A: MaterialAtlas>(app: &mut App) {
    app.add_systems(
        PostUpdate,
        (
            follow_atlas_binding::<A, BakedLight>,
            follow_atlas_binding::<A, DynamicLit>,
            follow_atlas_binding::<A, HardwareLighting>,
            follow_atlas_binding::<A, TerrainLayers>,
            follow_atlas_binding::<A, TerrainLayer>,
            follow_atlas_binding::<A, OpaqueDecals>,
        )
            .before(AssetEventSystems),
    );
}

/// The faces of the lamps in sight that would show their texels get a square each, those nearest the eye first. A
/// face keeps its square while its lamp's zone is out of sight, so walking back draws nothing again. Lamps out of sight
/// in a zone one portal from the eye's get squares too, sized as seen from that portal, so a room is drawn before it
/// is entered.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn pick_faces(
    mut frames: Local<u32>,
    mut squares: ResMut<DetailSquares>,
    settings: Res<Settings>,
    mut atlas: ResMut<LampAtlas>,
    scene: Option<Res<ShownScene>>,
    level: Option<Res<LevelCollision>>,
    mut portals: Local<(usize, Vec<([u8; 2], Vec3)>)>,
    mut doors: Local<Vec<(u8, Vec3)>>,
    cameras: Query<(&Transform, &Projection, &Camera), With<SceneView>>,
    lamps: Query<(&Transform, &Placed, &InheritedVisibility, &ZoneCulled), With<Lamp>>,
) {
    let Some(bake) = atlas.bake.clone() else {
        if squares.book.is_some() {
            squares.book = None;
        }
        return;
    };
    let key = Arc::as_ptr(&bake).addr();
    if squares.book.is_none() || squares.bake != key {
        *squares = DetailSquares { bake: key, made: squares.made + 1, book: Some(FaceBook::new(atlas.side)) };
    }
    *frames += 1;
    if *frames < FACE_PICK_FRAMES {
        return;
    }
    *frames = 0;
    let quality = lamp_quality(settings.modern_level(MODERN_LAMP_SHADOWS));
    let Some(eye) = scene_eye(&cameras, quality.texel_pixels) else { return };
    let Some(faces) = squares.book.as_mut() else { return };
    let unit = atlas.side / ATLAS_UNITS;
    doors.clear();
    if let (Some(scene), Some(collision)) = (scene.as_deref(), level.as_ref().and_then(|level| level.0.as_ref())) {
        let model = &collision.model;
        let key = Arc::as_ptr(collision).addr();
        if portals.0 != key {
            let middles = model
                .nodes
                .iter()
                .filter(|node| node.zones[0] != node.zones[1] && model.surfaces.get(node.surface).is_some_and(|surface| surface.poly_flags & p2_assets::poly_flags::PORTAL != 0))
                .filter_map(|node| {
                    let corners = model.verts.iter().skip(node.vert_pool).take(usize::from(node.num_vertices)).filter_map(|vert| model.points.get(vert.point));
                    let (sum, count) = corners.fold((Vec3::ZERO, 0.0), |(sum, count), &corner| (sum + Vec3::from(corner), count + 1.0));
                    (count > 0.0).then(|| (node.zones, sum / count))
                });
            *portals = (key, middles.collect());
        }
        let own = p2_collision::point_zone(model, p2_bevy_assets::to_bevy(eye.at - scene.origin).into());
        if own != 0 {
            doors.extend(portals.1.iter().filter_map(|&([a, b], middle)| {
                let other = if a == own { b } else if b == own { a } else { return None };
                Some((other, Vec3::from(p2_bevy_assets::to_bevy(middle)) + scene.origin))
            }));
        }
    }
    let in_sight = lamps.iter().filter(|(_, _, shown, _)| shown.get()).filter_map(|(at, placed, _, _)| {
        Some(FaceLamp { number: placed.number, at: at.translation, spot: placed.spot, reach: placed.reach, plain_face: bake.plain_face(placed.number as usize, unit)?, ahead: None })
    });
    let ahead = lamps.iter().filter(|(_, _, shown, _)| !shown.get()).filter_map(|(at, placed, _, zone)| {
        let &(_, portal) = doors.iter().filter(|(other, _)| zone.zones.contains(other)).min_by(|a, b| a.1.distance_squared(at.translation).total_cmp(&b.1.distance_squared(at.translation)))?;
        Some(FaceLamp { number: placed.number, at: at.translation, spot: placed.spot, reach: placed.reach, plain_face: bake.plain_face(placed.number as usize, unit)?, ahead: Some(portal) })
    });
    faces.pick(&eye, in_sight.chain(ahead), quality.texels_a_pick);
    let slots = faces.slots();
    if atlas.faces != slots {
        atlas.faces = slots;
    }
}

pub struct LampsPlugin;

impl Plugin for LampsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LampAtlas>();
        app.init_resource::<LampBook>();
        app.init_resource::<DetailSquares>();
        app.init_resource::<MoverTiles>();
        app.add_systems(Update, (follow_lamps, pick_faces).chain().in_set(super::RenderFollowSystems));
        // After this frame's places and bounds are known: a tile aimed by last frame's trails its mover.
        app.add_systems(PostUpdate, pick_mover_tiles.after(TransformSystems::Propagate).after(VisibilitySystems::CalculateBounds));
        follow_atlas::<LampAtlas>(app);
        follow_atlas::<LampBook>(app);
        app.add_plugins((ExtractResourcePlugin::<LampAtlas>::default(), ExtractResourcePlugin::<MoverTiles>::default()));
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [
            (ATLAS_SHADER, Shader::from_wesl(include_str!("lamp_atlas.wesl"), "p2_render/lamp_atlas.wesl")),
            (MOVER_CASTER_SHADER, Shader::from_wesl(include_str!("mover_caster.wesl"), "p2_render/mover_caster.wesl")),
        ]);
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.init_resource::<PlainTilesDrawn>();
            render.init_resource::<BookRecords>();
            render.add_systems(Render, (draw_lamp_atlas, draw_mover_tiles).chain().in_set(RenderSystems::PrepareResources));
        }
    }

    fn finish(&self, app: &mut App) {
        if let Some(device) = app.world().get_resource::<RenderDevice>() {
            MOST_ATLAS_SIDE.store(device.limits().max_texture_dimension_2d, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::shader_const;

    #[test]
    fn the_lamp_sum_adds_the_lamps_in_range_by_their_falloff() {
        let placed = |color: Vec3, spot| Placed { number: 0, strength: color.element_sum(), color, range: 100.0, spot, reach: [0.0; 6] };
        let (red, blue, down) = (placed(Vec3::X, None), placed(Vec3::Z, None), placed(Vec3::Y, Some((Vec3::NEG_Y, 0.5))));
        let lamps = [(Vec3::ZERO, &red), (Vec3::new(500.0, 0.0, 0.0), &blue), (Vec3::new(0.0, 100.0, 0.0), &down)];
        let open = |_: Vec3| true;
        assert_eq!(lamp_sum(lamps.iter().copied(), Vec3::ZERO, open), Vec3::X);
        let half = lamp_sum(lamps.iter().copied(), Vec3::new(50.0, 0.0, 0.0), open);
        assert!((half - Vec3::new(0.5, 0.0, 0.0)).length() < 1e-6, "{half}");
        let under = lamp_sum(lamps.iter().copied(), Vec3::new(0.0, 50.0, 0.0), open);
        assert!((under - Vec3::new(0.5, 0.5, 0.0)).length() < 1e-6, "{under}");
        assert_eq!(lamp_sum(lamps.iter().copied(), Vec3::new(0.0, 0.0, 300.0), open), Vec3::ZERO);
        let walled = lamp_sum(lamps.iter().copied(), Vec3::new(0.0, 50.0, 0.0), |at: Vec3| at != Vec3::ZERO);
        assert!((walled - Vec3::new(0.0, 0.5, 0.0)).length() < 1e-6, "{walled}");
    }

    #[test]
    fn the_lamp_level_sets_the_atlas_side() {
        let table: Vec<u32> = (0..=4).map(lamp_quality).map(|quality| quality.atlas_side).collect();
        assert_eq!(table, [0, 4096, 4096, 8192, 16384]);
        assert_eq!(lamp_quality(9), lamp_quality(4));
    }

    #[test]
    fn the_shader_reads_a_lamps_record_as_it_is_written() {
        let shader = include_str!("lamp_shadow.wesl");
        assert_eq!(shader_const(shader, "BOOK_SIZED"), f64::from(BOOK_SIZED));
        assert_eq!(shader_const(shader, "LAMP_SIZE_SHIFT"), f64::from(crate::material_class::LEVEL_SHIFT_LAMP_SIZE));
    }

    #[test]
    fn a_lamps_record_holds_its_place_radius_colour_and_tile() {
        let light = MeshLight { location: p2_math::Vector::new(100.0, 200.0, 300.0), radius: 64.0, ..MeshLight::default() };
        let tile = 5 | 9 << 8 | 2 << 16;
        let record = lamp_record(&light, 1.0, Vec3::new(1.0, 2.0, 3.0), tile);
        let at = Vec3::from(p2_bevy_assets::to_bevy(light.location)) + Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(record[0], [at.x, at.y, at.z, light.world_radius()]);
        let [r, g, b] = linear_light(light.color(1.0));
        assert_eq!(record[1], [r, g, b, tile as f32]);
        assert_eq!(record[1][3] as u32, tile);
        assert_eq!(record[6], [light.world_radius() * crate::lamp_size::LAMP_SOURCE_SHARE, 0.0, 0.0, 0.0]);
        let crowned = lamp_record(&MeshLight { corona: 160.0, ..light }, 1.0, Vec3::ZERO, tile);
        assert_eq!(crowned[6][0], crate::lamp_size::lamp_size(light.world_radius(), Some(160.0), None));
        assert_eq!(crowned[1][3] as u32, BOOK_SIZED | tile);
    }

    #[test]
    fn a_spots_record_has_a_negative_radius_and_holds_its_direction_and_cone() {
        let light = MeshLight { effect: LE_SPOTLIGHT, cone: 128, ..MeshLight::default() };
        let record = lamp_record(&light, 1.0, Vec3::ZERO, UNMAPPED);
        assert_eq!(record[0][3], -light.world_radius());
        let along = Vec3::from(p2_bevy_assets::to_bevy(light.rotation.vector())).normalize();
        let [x, y, scale, offset] = record[2];
        let z = (1.0 - x * x - y * y).max(0.0).sqrt().copysign(scale);
        assert!(Vec3::new(x, y, z).distance(along) < 1e-5);
        assert!((scale.abs() - 2.0).abs() < 1e-5 && (offset + 1.0).abs() < 1e-5, "{scale} {offset}");
    }

    #[test]
    fn a_book_is_whole_rows_of_float_texels() {
        let image = book_image(&[[1.0, 2.0, 3.0, 4.0]]);
        assert_eq!((image.width() as usize, image.height()), (BOOK_WIDTH, 9));
        assert_eq!(image.data.as_ref().map(|data| (data.len(), data[..8].to_vec())), Some((9 * BOOK_WIDTH * 16, [1.0f32.to_le_bytes(), 2.0f32.to_le_bytes()].concat())));
    }

    #[test]
    fn suns_dynamic_and_special_lights_are_not_lamps() {
        assert!(is_lamp(&MeshLight::default()));
        assert!(!is_lamp(&MeshLight { dynamic: true, ..MeshLight::default() }));
        assert!(!is_lamp(&MeshLight { special_lit: true, ..MeshLight::default() }));
        assert!(!is_lamp(&MeshLight { effect: LE_SUNLIGHT, ..MeshLight::default() }));
        assert!(!is_lamp(&MeshLight { brightness: 0.0, ..MeshLight::default() }));
    }
}
