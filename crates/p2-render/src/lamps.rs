//! Modern lamps: a pixel is lit by the lamps listed for its map cell ([`cells`]), read from a book texture.
//! Static shadows come from an atlas drawn once a map ([`atlas`]), finer where a face would show its texels
//! ([`faces`]); what moves casts into tiles of its own ([`movers`]).

pub(crate) mod atlas;
mod bake_cache;
mod cells;
mod decal_bins;
mod faces;
mod grounding;
pub mod grid;
mod mover_pass;
mod mover_pick;
mod movers;
mod shadow_mask;
mod sky;
mod tile_grid;
pub mod visibility;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use bevy::app::{App, Plugin, PostUpdate, Update};
use bevy::asset::{Asset, AssetEvent, AssetEventSystems, Assets, Handle, RenderAssetUsages};
use bevy::camera::visibility::{InheritedVisibility, RenderLayers, Visibility, VisibilitySystems};
use bevy::camera::{Camera, Projection};
use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::message::{MessageCursor, Messages};
use bevy::ecs::prelude::{Commands, Component, Entity, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::{IntoScheduleConfigs, SystemSet};
use bevy::image::{Image, ImageSampler};
use bevy::math::Vec3;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, StandardMaterial};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::renderer::RenderDevice;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use bevy::transform::TransformSystems;
use bevy::transform::components::Transform;
use p2_assets::Model;
use p2_render_cpu::{LightEffect, MeshLight};

use crate::components::{MOVER_LAYER, RESTING_PROP_LAYER, SUN_VIEW_LAYER, SceneView};
use p2_host::display::{MODERN_LAMP_SHADOWS, Settings};
use crate::level_collision::{LevelCollision, StaticCollision};
use crate::view::SeenWithin;
use crate::scene::ZoneCulled;
use crate::actor_light::{HardwareLighting, LightEnvironment, reaches_sky};
use crate::blended::{OpaqueDecals, TerrainLayer};
use crate::bsp_light::{gpu_light, place};
use crate::lightmapped::BakedLight;
use crate::mesh_light::DynamicLit;
use crate::modern::linear_light;
use crate::shaders::{add_shaders, shader_file};
use crate::terrain_material::TerrainLayers;
use crate::view::ShownScene;
use atlas::{ATLAS_SHADER, ATLAS_UNITS, BookRecords, LampBake, PlainTilesDrawn, UNMAPPED, bytes_of, draw_lamp_atlas, lamp_bake, new_atlas, world_masked, world_triangles};
use cells::{BOOK_WIDTH, CellLamp, DECAL_BINS_FROM_END, PatchBox, RECORD_TEXELS, book_texels, lamp_cells};
use faces::{Eye, FaceBook, FaceLamp, FaceSlot};
use mover_pass::{MOVER_CASTER_SHADER, draw_mover_tiles};
use mover_pick::pick_mover_tiles;
pub use movers::MoverTiles;
use grid::ShadowSurface;
use sky::{SkyBake, bake_sky};
use visibility::{PatchJob, VISIBILITY_SHADER, VisibilityPlan, cache_lamp_visibility, cut, pack_blocks, patch_box, patch_jobs};

pub const LAMP_SHADOW_SPAN: &str = "lamp_shadows";

/// Where `lamp_shadow.wesl`'s fade ends. A far lamp's pool covers few pixels but costs every cluster and list on
/// its way.
const LAMP_SEEN_RADII: f32 = 30.0;
/// Without an atlas a lamp lights through walls and a cell lists every lamp in reach, so the cells are larger.
const SHADOWED_CELL: f32 = 128.0;
const UNSHADOWED_CELL: f32 = 512.0;
const ITEMS_PER_BATCH: usize = 8;

/// Runs `work` in the light-bake pool of all threads but two, which are left to the game.
/// Bakes running at once share the pool, each thread taking whichever bake's work is left.
pub(crate) fn in_bake_pool<R: Send>(work: impl FnOnce() -> R + Send) -> R {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism().map_or(1, |threads| threads.get().saturating_sub(2).max(1));
        rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|index| format!("light bake {index}")).build().ok()
    });
    match pool {
        Some(pool) => pool.install(work),
        None => work(),
    }
}

fn on_threads<T: Send>(count: usize, work: impl Fn(usize) -> T + Sync + Send) -> Vec<T> {
    use rayon::prelude::*;
    in_bake_pool(|| (0..count).into_par_iter().with_min_len(ITEMS_PER_BATCH).map(work).collect())
}

/// Set above the tile in a lamp's colour texel when the lamp has a size of its own. Under 2^24, so it stays exact
/// as a float. `lamp_shadow.wesl` holds the same number.
const BOOK_SIZED: u32 = 1 << 20;

/// A lamp's book texels, as `lamp_shadow.wesl` reads them: place and radius (negative for a spot), linear colour
/// and atlas tile, spot direction and cone; then three the passes write (detail squares, mover tiles);
/// the last holds the lamp's size, read only when the tile carries `BOOK_SIZED`.
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

fn scene_place(light: &MeshLight, origin: Vec3) -> (Vec3, Option<Vec3>) {
    let spot = matches!(light.effect, LightEffect::StaticSpot | LightEffect::Spotlight).then(|| Vec3::from(p2_bevy_assets::to_bevy(light.rotation.vector())).normalize_or_zero());
    (Vec3::from(p2_bevy_assets::to_bevy(light.location)) + origin, spot)
}

fn scene_eye(cameras: &Query<(&Transform, &Projection, &Camera), With<SceneView>>, texel_pixels: f32) -> Option<Eye> {
    let Some((eye, Projection::Perspective(lens), camera)) = cameras.iter().next() else { return None };
    let height = camera.physical_viewport_size().map(|size| size.y as f32).filter(|&height| height > 0.0)?;
    Some(Eye { at: eye.translation, texel_spread: 2.0 * (0.5 * lens.fov).tan() / height * texel_pixels })
}

const FACE_PICK_FRAMES: u32 = 4;
const MOVER_FADE_SECONDS: f32 = 0.3;

static MOST_ATLAS_SIDE: AtomicU32 = AtomicU32::new(u32::MAX);

#[derive(Clone, Copy, Debug, PartialEq)]
struct LampQuality {
    atlas_side: u32,
    texel_pixels: f32,
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
    !light.dynamic && !light.special_lit && light.effect != LightEffect::Sunlight && light.color(1.0).iter().sum::<f32>() > 0.0
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
    pub(crate) color: Vec3,
    pub(crate) range: f32,
    pub(crate) spot: Option<(Vec3, f32)>,
    reach: [f32; 6],
}

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
    height: u32,
    book: Option<Handle<Image>>,
    faces: Vec<FaceSlot>,
}

impl LampAtlas {
    pub fn has_detail_faces(&self) -> bool {
        !self.faces.is_empty()
    }
}

#[derive(Resource, Default)]
struct DetailSquares {
    bake: usize,
    made: u32,
    book: Option<FaceBook>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct LampJobResult {
    /// Not cached: [`map_lamps`] gives them again.
    #[serde(skip)]
    lights: Vec<MeshLight>,
    zones_reach: Vec<(Vec<u8>, [f32; 6])>,
    bake: Option<LampBake>,
    book: Vec<[f32; 4]>,
    listed: String,
    visibility: Option<VisibilityPlan>,
    patches: Vec<(u32, [u32; 2])>,
}

/// The surface patches' cached lamp visibility; no image when the map has none to cache or it would pass the cap.
#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub struct LampVisibility {
    pub image: Option<Handle<Image>>,
    pub jobs: Arc<[PatchJob]>,
}

/// How much sky each texel of the patches' blocks sees; no image until its trace is done.
#[derive(Resource, Default)]
pub struct LampSky {
    pub image: Option<Handle<Image>>,
}

#[derive(Default)]
struct LampJob {
    key: Option<(usize, usize, u32)>,
    task: Option<Task<LampJobResult>>,
    ready: Option<LampJobResult>,
    /// On its own thread, not the async pool: it runs during play and holds its thread for the whole trace.
    sky: Option<(std::thread::JoinHandle<SkyBake>, Arc<AtomicBool>)>,
}

/// `surfaces` are in world space; the job works in the scene's, `origin` away. Without shadow tiles there is
/// nothing to cache, so no surface is cut.
fn lamp_job(environment: &LightEnvironment, collision: &StaticCollision, triangles: &[[[f32; 3]; 3]], surfaces: &[ShadowSurface], origin: Vec3, side: u32) -> LampJobResult {
    let lamps = map_lamps(environment);
    let spheres: Vec<_> = lamps.iter().map(|light| (scene_place(light, origin).0.to_array(), light.world_radius())).collect();
    let bake = (side > 0).then(|| lamp_bake(triangles, &world_masked(collision, origin), &spheres));
    let records: Vec<_> = lamps.iter().enumerate().map(|(index, light)| lamp_record(light, environment.level_brightness, origin, bake.as_ref().map_or(UNMAPPED, |bake| bake.tile_bits(index)))).collect();
    let surfaces: Vec<_> = surfaces.iter().filter(|_| bake.is_some()).map(|surface| ShadowSurface { origin: (Vec3::from(surface.origin) + origin).to_array(), ..surface.clone() }).collect();
    let (firsts, patches) = cut(&surfaces);
    let cells = {
        let drawn: Vec<_> = bake.as_ref().map_or_else(|| vec![None; lamps.len()], |bake| bake.drawn(side / ATLAS_UNITS).collect());
        let cell_lamps: Vec<_> = records.iter().zip(drawn).map(|(record, drawn)| CellLamp::of(record, drawn.map(|(face, _)| face), drawn.map_or(&[], |(_, casters)| casters))).collect();
        let boxes: Vec<PatchBox> = patches.iter().map(|&(surface, corner)| patch_box(&surfaces[surface as usize], corner)).collect();
        lamp_cells(bake.as_ref().map_or(&[], |bake| &bake.vertices), &cell_lamps, &boxes, if side > 0 { SHADOWED_CELL } else { UNSHADOWED_CELL })
    };
    let groups: Vec<u32> = (0..patches.len()).map(|patch| cells.patch_of(patch).len().div_ceil(4) as u32).collect();
    let packed = pack_blocks(&groups);
    let lit = cells.starts.windows(2).filter(|pair| pair[1] > pair[0]).count().max(1);
    let atlas_height = bake.as_ref().map_or(0, |bake| bake.atlas_height(side));
    let listed = format!(
        "cells of {} units, {:.1} lamps a lit cell, {} of the {} lamp and cell pairs in reach kept; {} patches, {} visibility blocks, {}; shadow atlas {:.1} MB",
        cells.cell,
        cells.lamps.len() as f32 / lit as f32,
        cells.lamps.len(),
        cells.in_reach,
        patches.len(),
        groups.iter().sum::<u32>(),
        packed.as_ref().map_or_else(|| "no visibility cache".to_owned(), |([width, height], _)| format!("{:.1} MB", f64::from(width * height * 4) / f64::from(1 << 20))),
        f64::from(side * atlas_height) * 2.0 / f64::from(1 << 20)
    );
    let book = book_texels(&cells, &records, &firsts, packed.as_ref().map_or(&[][..], |(_, places)| places.as_slice()));
    let visibility = packed.map(|(size, _)| {
        let patches_start = book[2][1] as usize;
        VisibilityPlan { size, jobs: patch_jobs(&surfaces, &patches, &book[patches_start..patches_start + patches.len()]) }
    });
    let zones = on_threads(lamps.len(), |lamp| p2_collision::light_zones(&collision.model, lamps[lamp].location, lamps[lamp].world_radius()));
    let zones_reach = zones.into_iter().zip(cells.reach).collect();
    LampJobResult { lights: lamps, zones_reach, bake, book, listed, visibility, patches }
}

/// Float texels in a slot Bevy binds as filterable: every desktop GPU filters them. Rows after the texels hold
/// the decals' screen tile lists (written by a compute pass), the movers' tile grid, their tiles and capsules.
fn book_image(texels: &[[f32; 4]]) -> Image {
    let rows = texels.len().div_ceil(BOOK_WIDTH).max(1) + DECAL_BINS_FROM_END as usize;
    let mut bytes = bytes_of(texels);
    bytes.resize(rows * BOOK_WIDTH * 16, 0);
    let mut image = Image::new(Extent3d { width: BOOK_WIDTH as u32, height: rows as u32, depth_or_array_layers: 1 }, TextureDimension::D2, bytes, TextureFormat::Rgba32Float, RenderAssetUsages::RENDER_WORLD);
    image.texture_descriptor.usage |= TextureUsages::STORAGE_BINDING;
    image
}

static LOAD_LEVELS: AtomicU32 = AtomicU32::new(0);
const LOAD_MODERN: u32 = 1 << 16;

pub(crate) fn set_load_light_levels(settings: Res<Settings>) {
    let levels = u32::from(settings.modern_level(MODERN_LAMP_SHADOWS)) | u32::from(settings.modern_level(p2_host::display::MODERN_INDIRECT)) << 8;
    LOAD_LEVELS.store(if settings.modern_graphics() { levels | LOAD_MODERN } else { 0 }, Ordering::Relaxed);
}

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

#[derive(Default)]
pub struct LightBakes {
    lamps: Prebaked<u32, LampJobResult>,
    pub(crate) probes: crate::probes::ProbePrebake,
}

pub fn bakes_at_load() -> bool {
    LOAD_LEVELS.load(Ordering::Relaxed) & LOAD_MODERN != 0
}

pub enum LoadBake {
    Cached,
    Baked { lamps: Duration, probes: Duration },
}

/// A file in the bake cache made by this build for the same inputs stands in for the bake; a fresh bake is saved
/// on a thread of its own.
pub(crate) fn bake_for_load(environment: &LightEnvironment, collision: &StaticCollision, surfaces: &[ShadowSurface], origin: Vec3) -> Option<(Arc<LightBakes>, LoadBake)> {
    if !bakes_at_load() {
        return None;
    }
    let levels = LOAD_LEVELS.load(Ordering::Relaxed);
    let (side, probe_level) = (lamp_quality((levels & 255) as u8).atlas_side, (levels >> 8 & 255) as u8);
    let world = world_triangles(collision, origin);
    let inputs = bake_cache::BakeInputs { environment, collision, world: &world, surfaces, origin, side, probe_level };
    let place = bake_cache::folder().zip(bake_cache::exe_stamp().map(|stamp| bake_cache::key(&inputs, stamp)));
    let (baked, how) = match place.as_ref().and_then(|(folder, key)| bake_cache::load(folder, *key)) {
        Some(mut baked) => {
            baked.lamps.lights = map_lamps(environment);
            if let Some(bake) = &mut baked.lamps.bake {
                bake.restore(&world.triangles, &collision.lamp_masked);
            }
            (baked, LoadBake::Cached)
        }
        None => {
            let ((lamps, lamp_time), (probes, probe_time)) = std::thread::scope(|scope| {
                let probes = scope.spawn(|| {
                    let start = Instant::now();
                    (crate::probes::prebake(environment, collision, &world, origin, probe_level), start.elapsed())
                });
                let start = Instant::now();
                let lamps = lamp_job(environment, collision, &world.triangles, surfaces, origin, side);
                ((lamps, start.elapsed()), probes.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
            });
            let baked = bake_cache::Baked { lamps, probes };
            if let Some((folder, key)) = place {
                let copy = baked.clone();
                let _ = std::thread::Builder::new().name("bake cache".into()).spawn(move || bake_cache::save(&folder, key, &copy));
            }
            (baked, LoadBake::Baked { lamps: lamp_time, probes: probe_time })
        }
    };
    let probes = baked.probes.map_or_else(crate::probes::ProbePrebake::default, |(columns, bake)| Prebaked::new(columns, bake));
    Some((Arc::new(LightBakes { lamps: Prebaked::new(side, baked.lamps), probes }), how))
}

/// A lamp that could light no zone the eye can see costs nothing: zone culling hides it, as it hides meshes. A
/// lamp inside solid names no zone and takes every zone its box touches.
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
    mut visibility: ResMut<LampVisibility>,
    mut sky: ResMut<LampSky>,
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
        if let Some((_, cancel)) = job.sky.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        *sky = LampSky::default();
        atlas.bake = None;
        atlas.book = None;
        atlas.faces.clear();
        book.image = None;
        *visibility = LampVisibility::default();
        let side = key.map_or(0, |(_, _, side)| side);
        atlas.side = side;
        atlas.height = 0;
        atlas.image = None;
        for entity in &lamps {
            commands.entity(entity).despawn();
        }
        if let (Some(environment), Some(collision), Some(shown)) = (environment.clone(), collision, shown.as_deref()) {
            let (origin, surfaces) = (shown.origin, shown.shadow_surfaces.clone());
            job.ready = shown.light_bakes.as_ref().and_then(|bakes| bakes.lamps.take(&side));
            if job.ready.is_none() {
                job.task = Some(AsyncComputeTaskPool::get().spawn(async move { lamp_job(&environment, &collision, &if side > 0 { world_triangles(&collision, origin).triangles } else { Vec::new() }, &surfaces, origin, side) }));
            }
        }
    }
    if let Some(SkyBake { size: [width, height], texels }) = job.sky.take_if(|(thread, _)| thread.is_finished()).and_then(|(thread, _)| thread.join().ok()) {
        let mut image = Image::new(Extent3d { width, height, depth_or_array_layers: 1 }, TextureDimension::D2, texels, TextureFormat::R8Unorm, RenderAssetUsages::RENDER_WORLD);
        image.sampler = ImageSampler::linear();
        sky.image = Some(images.add(image));
    }
    if job.ready.is_none() && !job.task.as_ref().is_some_and(Task::is_finished) {
        return;
    }
    let (Some(environment), Some(shown), Some(model)) = (environment, shown.as_deref(), level.as_ref().and_then(|level| level.model())) else { return };
    let Some(LampJobResult { lights, zones_reach: map, bake, book: texels, listed, visibility: plan, patches }) = job.ready.take().or_else(|| job.task.take().map(block_on)) else { return };
    if let Some(collision) = level.as_ref().and_then(|level| level.0.clone()).filter(|_| !patches.is_empty()) {
        // The shown charts sit in world space on Bevy's axes; swapping the axes back gives the collision's frame.
        let surfaces = shown.shadow_surfaces.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        // Once cancelled, every point left reads as solid, so the rest of the trace is skipped.
        let solid = {
            let (collision, cancel) = (collision.clone(), cancel.clone());
            move |at: Vec3| cancel.load(Ordering::Relaxed) || p2_collision::point_solid(&collision.model, p2_bevy_assets::to_bevy(at).into())
        };
        let open = move |from: Vec3, way: Vec3| reaches_sky(&collision, p2_bevy_assets::to_bevy(from).into(), p2_bevy_assets::to_bevy(way).into());
        job.sky = std::thread::Builder::new().name("sky bake".into()).spawn(move || bake_sky(&surfaces, &patches, open, solid)).ok().map(|thread| (thread, cancel));
    }
    for (index, (light, (zones, reach))) in lights.iter().zip(&map).enumerate() {
        spawn_lamp(&mut commands, index, light, *reach, &environment, shown.origin, lamp_zone(light, model, zones));
    }
    if !map.is_empty() {
        let zones: usize = map.iter().map(|(zones, _)| zones.len()).sum();
        let unknown = map.iter().filter(|(zones, _)| zones.is_empty()).count();
        bevy::log::info!("modern lamps: {}, {:.1} zones a lamp on average, {unknown} inside solid with no zone known; {listed}", map.len(), zones as f32 / map.len() as f32);
    }
    book.image = Some(images.add(book_image(&texels)));
    atlas.book.clone_from(&book.image);
    book.lamps = map.len();
    if let Some(VisibilityPlan { size: [width, height], jobs }) = plan {
        let mut image = Image::new_uninit(Extent3d { width, height, depth_or_array_layers: 1 }, TextureDimension::D2, TextureFormat::Rgba8Unorm, RenderAssetUsages::RENDER_WORLD);
        image.texture_descriptor.usage = TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING;
        image.sampler = ImageSampler::linear();
        *visibility = LampVisibility { image: Some(images.add(image)), jobs: jobs.into() };
    }
    // Made with its bake, so the materials bind both in one frame and the height is known.
    let (side, height) = (atlas.side, bake.as_ref().map_or(0, |bake| bake.atlas_height(atlas.side)));
    atlas.height = height;
    atlas.image = (height > 0).then(|| new_atlas(&mut images, [side, height], TextureFormat::R16Float));
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
    /// Bevy reads it only for parallax, compiled out without vertex tangents: a mesh that gains tangents would shift
    /// its UVs by this image as a height map.
    DepthMap,
    /// Bevy reads it only for normal mapping, compiled out without vertex tangents: a mesh that gains tangents would
    /// bend its normals by this image.
    NormalMap,
    /// Bevy reads it only to scale `material.reflectance` in its PBR input, which our lit shaders never read.
    Specular,
    /// Bevy reads it only to tint specular in its PBR input, which our lit shaders never read.
    SpecularTint,
}

impl MaterialSlot {
    fn of(self, material: &StandardMaterial) -> &Option<Handle<Image>> {
        match self {
            Self::Occlusion => &material.occlusion_texture,
            Self::MetallicRoughness => &material.metallic_roughness_texture,
            Self::Anisotropy => &material.anisotropy_texture,
            Self::DepthMap => &material.depth_map,
            Self::NormalMap => &material.normal_map_texture,
            Self::Specular => &material.specular_texture,
            Self::SpecularTint => &material.specular_tint_texture,
        }
    }

    fn of_mut(self, material: &mut StandardMaterial) -> &mut Option<Handle<Image>> {
        match self {
            Self::Occlusion => &mut material.occlusion_texture,
            Self::MetallicRoughness => &mut material.metallic_roughness_texture,
            Self::Anisotropy => &mut material.anisotropy_texture,
            Self::DepthMap => &mut material.depth_map,
            Self::NormalMap => &mut material.normal_map_texture,
            Self::Specular => &mut material.specular_texture,
            Self::SpecularTint => &mut material.specular_tint_texture,
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

impl MaterialAtlas for LampVisibility {
    const SLOT: MaterialSlot = MaterialSlot::Specular;

    fn wanted(&self) -> Option<Handle<Image>> {
        self.image.clone()
    }
}

impl MaterialAtlas for LampSky {
    const SLOT: MaterialSlot = MaterialSlot::SpecularTint;

    fn wanted(&self) -> Option<Handle<Image>> {
        self.image.clone()
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

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AtlasSizeSystems;

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
            .after(AtlasSizeSystems)
            .before(AssetEventSystems),
    );
}

/// Faces of lamps in sight that would show their texels get a square each, nearest the eye first. A face keeps
/// its square while its lamp's zone is out of sight, so walking back draws nothing again. Lamps one portal away
/// get squares too, sized as seen from that portal, so a room is drawn before it is entered.
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
        *squares = DetailSquares { bake: key, made: squares.made + 1, book: Some(FaceBook::new([atlas.side, atlas.height])) };
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
        app.init_resource::<LampVisibility>();
        app.init_resource::<LampSky>();
        app.init_resource::<DetailSquares>();
        app.init_resource::<MoverTiles>();
        app.add_systems(Update, (follow_lamps, pick_faces).chain().in_set(super::RenderFollowSystems));
        app.add_systems(PostUpdate, pick_mover_tiles.after(TransformSystems::Propagate).after(VisibilitySystems::CalculateBounds));
        follow_atlas::<LampAtlas>(app);
        follow_atlas::<LampBook>(app);
        // Only the BSP reads the cached visibility and sky; blended surfaces light through Bevy, which reads the specular slots.
        app.add_systems(PostUpdate, (follow_atlas_binding::<LampVisibility, BakedLight>, follow_atlas_binding::<LampSky, BakedLight>).after(AtlasSizeSystems).before(AssetEventSystems));
        app.add_plugins((ExtractResourcePlugin::<LampAtlas>::default(), ExtractResourcePlugin::<MoverTiles>::default(), ExtractResourcePlugin::<LampVisibility>::default()));
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [
            (ATLAS_SHADER, shader_file("lamp_atlas.wesl")),
            (MOVER_CASTER_SHADER, shader_file("mover_caster.wesl")),
            (VISIBILITY_SHADER, shader_file("lamp_visibility.wesl")),
        ]);
        decal_bins::build(app);
        shadow_mask::build(app);
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.init_resource::<PlainTilesDrawn>();
            render.init_resource::<BookRecords>();
            render.add_systems(Render, (draw_lamp_atlas, cache_lamp_visibility, draw_mover_tiles).chain().in_set(RenderSystems::PrepareResources));
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
        let light = MeshLight { effect: LightEffect::Spotlight, cone: 128, ..MeshLight::default() };
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
        let rows = 1 + DECAL_BINS_FROM_END as usize;
        assert_eq!((image.width() as usize, image.height() as usize), (BOOK_WIDTH, rows));
        assert_eq!(image.data.as_ref().map(|data| (data.len(), data[..8].to_vec())), Some((rows * BOOK_WIDTH * 16, [1.0f32.to_le_bytes(), 2.0f32.to_le_bytes()].concat())));
    }

    #[test]
    fn suns_dynamic_and_special_lights_are_not_lamps() {
        assert!(is_lamp(&MeshLight::default()));
        assert!(!is_lamp(&MeshLight { dynamic: true, ..MeshLight::default() }));
        assert!(!is_lamp(&MeshLight { special_lit: true, ..MeshLight::default() }));
        assert!(!is_lamp(&MeshLight { effect: LightEffect::Sunlight, ..MeshLight::default() }));
        assert!(!is_lamp(&MeshLight { brightness: 0.0, ..MeshLight::default() }));
    }
}
