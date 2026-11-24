//! Modern lamps: a pixel is lit by the lamps listed for its cell of the map ([`cells`]), read from a book of words the
//! shaders take as a texture. A lamp's shadow of the static world comes from one atlas drawn once a map ([`atlas`]); the
//! strongest lamps near the eye get a sharp tile as well, redrawn as the eye moves, and the strongest point lamps are
//! Bevy lights too, for a cube map of the movers.

pub(crate) mod atlas;
mod cells;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use bevy::app::{App, Plugin, PostUpdate, Update};
use bevy::asset::{Asset, AssetEvent, AssetEventSystems, Assets, Handle, RenderAssetUsages};
use bevy::camera::visibility::{InheritedVisibility, RenderLayers, Visibility};
use bevy::color::Color;
use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::message::{MessageCursor, Messages};
use bevy::ecs::prelude::{Commands, Component, Entity, Local, Query, Res, ResMut, Resource, With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::{PointLight, PointLightShadowMap};
use bevy::math::Vec3;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, StandardMaterial};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::renderer::RenderDevice;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::shader::Shader;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use bevy::time::Time;
use bevy::transform::components::Transform;
use p2_assets::Model;
use p2_render_cpu::{LE_SPOTLIGHT, LE_STATIC_SPOT, LE_SUNLIGHT, MeshLight};

use crate::components::SceneView;
use crate::display::{MODERN_LAMP_SHADOWS, Settings};
use crate::level_collision::{LevelCollision, StaticCollision};
use crate::mesh_view::{SeenWithin, ZoneCulled};
use crate::render::actor_light::{HardwareLighting, LightEnvironment};
use crate::render::blended::{OpaqueDecals, TerrainLayer};
use crate::render::bsp_light::{BevyLight, gpu_light, place};
use crate::render::lightmapped::BakedLight;
use crate::render::mesh_light::DynamicLit;
use crate::render::modern::{MOVER_LAYER, RESTING_PROP_LAYER, SUN_VIEW_LAYER, linear_light};
use crate::render::shaders::add_shaders;
use crate::render::terrain_material::TerrainLayers;
use crate::shown_scene::ShownScene;
use atlas::{ATLAS_SHADER, ATLAS_UNITS, LAMP_MARK, LampBake, SHARP_TILES, UNMAPPED, draw_lamp_atlas, lamp_bake, new_atlas, world_masked, world_triangles};
use cells::{BOOK_WIDTH, CellLamp, RECORD_TEXELS, book_texels, lamp_cells};

/// Where `modern.wesl`'s fade ends. A far lamp's pool covers few pixels but costs every cluster and list on its way.
const LAMP_SEEN_RADII: f32 = 30.0;
/// Without an atlas a lamp lights through walls and a cell lists every lamp in reach, so the cells are larger.
const SHADOWED_CELL: f32 = 128.0;
const UNSHADOWED_CELL: f32 = 512.0;
/// Left to the game while the lists are made: the slowest machine it must run on has four threads.
const THREADS_KEPT: usize = 2;

/// A lamp's texels in the book, as `modern.wesl` takes them apart: its place and radius, the radius negative for a spot;
/// its colour in linear light and its atlas tile; for a spot its direction's x and y, the cone's scale with the sign of
/// the direction's z, and the cone's offset; last its sharp tile, which the atlas pass writes.
fn lamp_record(light: &MeshLight, level_brightness: f32, origin: Vec3, tile: u32) -> [[f32; 4]; RECORD_TEXELS] {
    let at = Vec3::from(p2_bevy_assets::to_bevy(light.location)) + origin;
    let [r, g, b] = linear_light(light.color(level_brightness));
    let look = [r, g, b, (tile & (LAMP_MARK - 1)) as f32];
    if !matches!(light.effect, LE_STATIC_SPOT | LE_SPOTLIGHT) {
        return [[at.x, at.y, at.z, light.world_radius()], look, [0.0; 4], [0.0; 4]];
    }
    let along = Vec3::from(p2_bevy_assets::to_bevy(light.rotation.vector())).normalize_or_zero();
    let scale = 1.0 / (1.0 - light.cone_sine()).max(1e-4);
    [[at.x, at.y, at.z, -light.world_radius()], look, [along.x, along.y, scale.copysign(along.z), -light.cone_sine() * scale], [0.0; 4]]
}

const MOVER_PICK_FRAMES: u32 = 10;
const MOVER_KEEP_FACTOR: f32 = 1.5;
const MOVER_SHADOW_NEAR: f32 = 4.0;

/// The largest texture the GPU takes, once the renderer is known.
static MOST_ATLAS_SIDE: AtomicU32 = AtomicU32::new(u32::MAX);

/// An atlas side of 0 makes no atlas: lamps light without a shadow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LampQuality {
    atlas_side: u32,
    mover_cubes: usize,
    cube_size: usize,
}

fn lamp_quality(level: u8) -> LampQuality {
    let (atlas_side, mover_cubes, cube_size) = match level {
        0 => (0, 0, 512),
        1 | 2 => (4096, 0, 512),
        3 => (8192, 2, 1024),
        _ => (16384, 4, 1024),
    };
    LampQuality { atlas_side: atlas_side.min(MOST_ATLAS_SIDE.load(Ordering::Relaxed)), mover_cubes, cube_size }
}

fn is_lamp(light: &MeshLight) -> bool {
    !light.dynamic && !light.special_lit && light.effect != LE_SUNLIGHT && light.color(1.0).iter().sum::<f32>() > 0.0
}

pub(crate) fn map_lamps(environment: &LightEnvironment) -> Vec<MeshLight> {
    let mut lamps: Vec<MeshLight> = environment.lights.values().filter(|light| is_lamp(light)).copied().collect();
    lamps.sort_by(|a, b| <[f32; 3]>::from(a.location).partial_cmp(&<[f32; 3]>::from(b.location)).unwrap_or(std::cmp::Ordering::Equal));
    lamps
}

#[derive(Component)]
pub struct Lamp;

/// A lamp's number in the book, the sum of its colour in linear light, and its radius.
#[derive(Component)]
struct Placed {
    number: u32,
    strength: f32,
    range: f32,
}

/// A lamp that lights all round, which a cube map of the movers can serve.
#[derive(Component)]
struct PointLamp;

/// One of a few Bevy lights that stay for the whole map and move from lamp to lamp, each a cube map of the movers.
/// Their number never changes: Bevy makes its cube map texture anew, and draws every face again, when it does.
#[derive(Component, Default)]
struct MoverLight {
    lamp: Option<u32>,
    /// How far the cube map's shadows have faded in, 0 to 1.
    fade: f32,
    leaving: bool,
}

/// No lamp's number: what an idle mover light is marked with.
const NO_LAMP: u32 = LAMP_MARK - 1;
const MOVER_FADE_SECONDS: f32 = 0.3;

fn mover_light() -> impl bevy::ecs::bundle::Bundle {
    let light = PointLight {
        color: Color::BLACK,
        range: 1.0,
        radius: 0.0,
        shadow_maps_enabled: true,
        contact_shadows_enabled: false,
        affects_lightmapped_mesh_diffuse: false,
        shadow_depth_bias: (LAMP_MARK + NO_LAMP) as f32,
        shadow_map_near_z: MOVER_SHADOW_NEAR,
        ..PointLight::default()
    };
    (Lamp, MoverLight::default(), light, Transform::from_translation(Vec3::splat(-1.0e6)), RenderLayers::from_layers(&[SUN_VIEW_LAYER, MOVER_LAYER, RESTING_PROP_LAYER]))
}

#[derive(Resource, Default)]
struct LampBook {
    image: Option<Handle<Image>>,
    lamps: usize,
}

#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
struct LampAtlas {
    image: Option<Handle<Image>>,
    bake: Option<Arc<LampBake>>,
    side: u32,
    book: Option<Handle<Image>>,
    /// The lamp each sharp tile is wanted for.
    sharp: Vec<Option<u32>>,
}

struct LampJobResult {
    /// Each lamp with the zones it could light (none known for a lamp inside solid).
    lamps: Vec<(MeshLight, Vec<u8>)>,
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

fn lamp_job(environment: &LightEnvironment, collision: &StaticCollision, origin: Vec3, side: u32) -> LampJobResult {
    let lamps = map_lamps(environment);
    let spheres: Vec<_> = lamps.iter().map(|light| ((Vec3::from(p2_bevy_assets::to_bevy(light.location)) + origin).to_array(), light.world_radius())).collect();
    let bake = (side > 0).then(|| lamp_bake(&world_triangles(collision, origin), &world_masked(collision, origin), &spheres));
    let threads = std::thread::available_parallelism().map_or(1, |threads| threads.get().saturating_sub(THREADS_KEPT).max(1));
    let cells = {
        let drawn: Vec<_> = bake.as_ref().map_or_else(|| vec![None; lamps.len()], |bake| bake.drawn(side / ATLAS_UNITS).collect());
        let cell_lamps: Vec<_> = spheres
            .iter()
            .zip(drawn)
            .map(|(&(position, radius), drawn)| CellLamp { position, radius, face: drawn.map(|(face, _)| face), casters: drawn.map_or(&[], |(_, casters)| casters) })
            .collect();
        lamp_cells(bake.as_ref().map_or(&[], |bake| &bake.vertices), &cell_lamps, if side > 0 { SHADOWED_CELL } else { UNSHADOWED_CELL }, threads)
    };
    let records: Vec<_> = lamps.iter().enumerate().map(|(index, light)| lamp_record(light, environment.level_brightness, origin, bake.as_ref().map_or(UNMAPPED, |bake| bake.tile_bits(index)))).collect();
    let lit = cells.starts.windows(2).filter(|pair| pair[1] > pair[0]).count().max(1);
    let listed = format!(
        "cells of {} units, {:.1} lamps a lit cell, {} of the {} lamp and cell pairs in reach kept",
        cells.cell,
        cells.lamps.len() as f32 / lit as f32,
        cells.lamps.len(),
        cells.in_reach
    );
    let book = book_texels(&cells, &records);
    LampJobResult { lamps: lamps.into_iter().map(|light| (light, p2_collision::light_zones(&collision.model, light.location, light.world_radius()))).collect(), bake, book, listed }
}

/// Float texels in a slot Bevy binds as filterable: every desktop GPU filters them, and Bevy asks for all a GPU offers.
fn book_image(texels: &[[f32; 4]]) -> Image {
    let rows = texels.len().div_ceil(BOOK_WIDTH).max(1);
    let mut bytes: Vec<u8> = texels.iter().flatten().flat_map(|value| value.to_le_bytes()).collect();
    bytes.resize(rows * BOOK_WIDTH * 16, 0);
    Image::new(Extent3d { width: BOOK_WIDTH as u32, height: rows as u32, depth_or_array_layers: 1 }, TextureDimension::D2, bytes, TextureFormat::Rgba32Float, RenderAssetUsages::RENDER_WORLD)
}

/// The modern switch, the lamp shadow level and the indirect light level in force, for the loading thread.
static LOAD_LEVELS: AtomicU32 = AtomicU32::new(0);
const LOAD_MODERN: u32 = 1 << 16;

pub(crate) fn set_load_light_levels(settings: &Settings) {
    let levels = u32::from(settings.modern_level(MODERN_LAMP_SHADOWS)) | u32::from(settings.modern_level(crate::display::MODERN_INDIRECT)) << 8;
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
    pub(crate) probes: crate::render::probes::ProbePrebake,
}

pub(crate) fn bake_for_load(environment: &LightEnvironment, collision: &StaticCollision, origin: Vec3) -> Option<Arc<LightBakes>> {
    let levels = LOAD_LEVELS.load(Ordering::Relaxed);
    if levels & LOAD_MODERN == 0 {
        return None;
    }
    let side = lamp_quality((levels & 255) as u8).atlas_side;
    Some(Arc::new(LightBakes {
        lamps: Prebaked::new(side, lamp_job(environment, collision, origin, side)),
        probes: crate::render::probes::prebake(environment, collision, origin, (levels >> 8 & 255) as u8),
    }))
}

/// A lamp that could light no zone the eye can see costs nothing: zone culling hides it, as it hides meshes. A lamp
/// inside solid names no zone and takes every zone its box touches.
fn lamp_zone(light: &MeshLight, model: &Model, lit_zones: &[u8]) -> ZoneCulled {
    let (center, radius) = (<[f32; 3]>::from(light.location), light.world_radius());
    let (min, max) = (center.map(|axis| axis - radius), center.map(|axis| axis + radius));
    let zones = if lit_zones.is_empty() { p2_collision::box_zones(model, min.into(), max.into()) } else { lit_zones.to_vec() };
    ZoneCulled { zones, min, max, only_from: None }
}

fn spawn_lamp(commands: &mut Commands, index: usize, light: &MeshLight, environment: &LightEnvironment, origin: Vec3, zone: ZoneCulled) {
    let Some(gpu) = gpu_light(light, environment.level_brightness) else { return };
    let at = place(light, &gpu, origin);
    let [r, g, b] = linear_light(light.color(environment.level_brightness));
    let placed = Placed { number: index as u32, strength: r + g + b, range: light.world_radius() };
    let seen = (Lamp, placed, at, Visibility::default(), RenderLayers::from_layers(&[SUN_VIEW_LAYER, MOVER_LAYER, RESTING_PROP_LAYER]), zone, SeenWithin(light.world_radius() * LAMP_SEEN_RADII));
    match gpu {
        BevyLight::Point(_) => commands.spawn((seen, PointLamp)),
        BevyLight::Spot(_) => commands.spawn(seen),
    };
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
    cube_map: Option<ResMut<PointLightShadowMap>>,
) {
    let modern = settings.modern_graphics();
    let quality = lamp_quality(settings.modern_level(MODERN_LAMP_SHADOWS));
    if let Some(mut cube_map) = cube_map {
        let size = if modern { quality.cube_size } else { PointLightShadowMap::default().size };
        if cube_map.size != size {
            cube_map.size = size;
        }
    }
    let environment = shown.as_deref().and_then(|shown| shown.lighting.clone()).filter(|_| modern);
    let collision = level.as_ref().and_then(|level| level.0.clone());
    let key = environment.as_ref().zip(collision.as_ref()).map(|(environment, collision)| (Arc::as_ptr(environment).addr(), Arc::as_ptr(collision).addr(), quality.atlas_side));
    if job.key != key {
        job.key = key;
        job.task = None;
        job.ready = None;
        atlas.bake = None;
        atlas.book = None;
        atlas.sharp = vec![None; SHARP_TILES];
        book.image = None;
        let side = key.map_or(0, |(_, _, side)| side);
        if atlas.side != side {
            atlas.side = side;
            atlas.image = (side > 0).then(|| new_atlas(&mut images, side));
        }
        for entity in &lamps {
            commands.entity(entity).despawn();
        }
        if let (Some(environment), Some(collision), Some(shown)) = (environment.clone(), collision, shown.as_deref()) {
            let origin = shown.origin;
            job.ready = shown.light_bakes.as_ref().and_then(|bakes| bakes.lamps.take(&side));
            if job.ready.is_none() {
                job.task = Some(AsyncComputeTaskPool::get().spawn(async move { lamp_job(&environment, &collision, origin, side) }));
            }
        }
    }
    if job.ready.is_none() && !job.task.as_ref().is_some_and(Task::is_finished) {
        return;
    }
    let (Some(environment), Some(shown), Some(model)) = (environment, shown.as_deref(), level.as_ref().and_then(|level| level.model())) else { return };
    let Some(LampJobResult { lamps: map, bake, book: texels, listed }) = job.ready.take().or_else(|| job.task.take().map(block_on)) else { return };
    for (index, (light, zones)) in map.iter().enumerate() {
        spawn_lamp(&mut commands, index, light, &environment, shown.origin, lamp_zone(light, model, zones));
    }
    for _ in 0..quality.mover_cubes {
        commands.spawn(mover_light());
    }
    if !map.is_empty() {
        let zones: usize = map.iter().map(|(_, zones)| zones.len()).sum();
        let unknown = map.iter().filter(|(_, zones)| zones.is_empty()).count();
        bevy::log::info!("modern lamps: {}, {:.1} zones a lamp on average, {unknown} inside solid with no zone known; {listed}", map.len(), zones as f32 / map.len() as f32);
    }
    book.image = Some(images.add(book_image(&texels)));
    atlas.book.clone_from(&book.image);
    book.lamps = map.len();
    atlas.bake = bake.map(Arc::new);
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
}

impl MaterialSlot {
    fn of(self, material: &StandardMaterial) -> &Option<Handle<Image>> {
        match self {
            Self::Occlusion => &material.occlusion_texture,
            Self::MetallicRoughness => &material.metallic_roughness_texture,
            Self::Anisotropy => &material.anisotropy_texture,
        }
    }

    fn of_mut(self, material: &mut StandardMaterial) -> &mut Option<Handle<Image>> {
        match self {
            Self::Occlusion => &mut material.occlusion_texture,
            Self::MetallicRoughness => &mut material.metallic_roughness_texture,
            Self::Anisotropy => &mut material.anisotropy_texture,
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
) {
    let Some(mut materials) = materials else { return };
    if !atlas.is_changed() && materials.len() == *known && !material_added(events.as_deref(), &mut cursor) {
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

/// In the lamp's ranges from the eye: a lamp's pool, and a figure's shadow in it, is seen from well outside its range.
const MOVER_PICK_RANGES: f32 = 3.0;

fn mover_score(strength: f32, range: f32, distance: f32) -> f32 {
    let x = (distance / (range * MOVER_PICK_RANGES)).min(1.0);
    strength * (1.0 - x * x * (3.0 - 2.0 * x))
}

/// The lamps that matter most at the eye take the sharp tiles; one that holds a tile keeps it against a slightly
/// stronger newcomer and while its zone is out of sight, and keeps its place among the tiles so it is not redrawn. The
/// first tiles are the large ones, so they go to the strongest newcomers.
fn pick_sharp_tiles(mut frames: Local<u32>, mut atlas: ResMut<LampAtlas>, cameras: Query<&Transform, With<SceneView>>, lamps: Query<(&Transform, &Placed, &InheritedVisibility), With<Lamp>>) {
    *frames += 1;
    if *frames < MOVER_PICK_FRAMES || atlas.bake.is_none() {
        return;
    }
    *frames = 0;
    let Some(eye) = cameras.iter().next().map(|camera| camera.translation) else { return };
    let mut scored: Vec<(u32, f32)> = lamps
        .iter()
        .filter(|(_, placed, shown)| shown.get() || atlas.sharp.contains(&Some(placed.number)))
        .map(|(transform, placed, _)| {
            let score = mover_score(placed.strength, placed.range, transform.translation.distance(eye));
            (placed.number, if atlas.sharp.contains(&Some(placed.number)) { score * MOVER_KEEP_FACTOR } else { score })
        })
        .filter(|&(_, score)| score > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(SHARP_TILES);
    let mut sharp: Vec<Option<u32>> = atlas.sharp.iter().map(|held| held.filter(|lamp| scored.iter().any(|(picked, _)| picked == lamp))).collect();
    for (lamp, _) in scored {
        if !sharp.contains(&Some(lamp))
            && let Some(free) = sharp.iter_mut().find(|held| held.is_none())
        {
            *free = Some(lamp);
        }
    }
    if atlas.sharp != sharp {
        atlas.sharp = sharp;
    }
}

/// The strongest point lamps at the eye hold the mover lights. A light leaves a lamp by fading its shadows out, then
/// takes the next lamp that waits and fades in, so no shadow of a person appears or goes at once. Spot lamps keep no
/// mover map: Bevy sizes the sun's shadow texture by the spot maps in view too.
fn pick_mover_cubes(
    time: Res<Time>,
    mut frames: Local<u32>,
    cameras: Query<&Transform, With<SceneView>>,
    lamps: Query<(&Transform, &Placed, &InheritedVisibility), (With<PointLamp>, Without<MoverLight>)>,
    mut lights: Query<(&mut MoverLight, &mut PointLight, &mut Transform), Without<SceneView>>,
) {
    let step = time.delta_secs() / MOVER_FADE_SECONDS;
    for (mut mover, mut light, _) in &mut lights {
        let fade = if mover.leaving || mover.lamp.is_none() { (mover.fade - step).max(0.0) } else { (mover.fade + step).min(1.0) };
        if fade != mover.fade {
            mover.fade = fade;
            light.radius = fade;
        }
        if mover.leaving && fade == 0.0 {
            mover.lamp = None;
            mover.leaving = false;
            light.shadow_depth_bias = (LAMP_MARK + NO_LAMP) as f32;
        }
    }
    *frames += 1;
    if *frames < MOVER_PICK_FRAMES {
        return;
    }
    *frames = 0;
    let Some(eye) = cameras.iter().next().map(|camera| camera.translation) else { return };
    let held: Vec<u32> = lights.iter().filter_map(|(mover, ..)| mover.lamp).collect();
    let mut scored: Vec<(u32, f32, Vec3, f32)> = lamps
        .iter()
        .filter(|(_, placed, shown)| shown.get() || held.contains(&placed.number))
        .map(|(transform, placed, _)| {
            let score = mover_score(placed.strength, placed.range, transform.translation.distance(eye));
            (placed.number, if held.contains(&placed.number) { score * MOVER_KEEP_FACTOR } else { score }, transform.translation, placed.range)
        })
        .filter(|&(_, score, ..)| score > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(lights.iter().count());
    for (mut mover, ..) in &mut lights {
        if let Some(lamp) = mover.lamp {
            let wanted = scored.iter().any(|&(number, ..)| number == lamp);
            if mover.leaving == wanted {
                mover.leaving = !wanted;
            }
        }
    }
    let mut waiting = scored.iter().filter(|(number, ..)| !held.contains(number));
    for (mut mover, mut light, mut transform) in &mut lights {
        if mover.lamp.is_some() {
            continue;
        }
        let Some(&(number, _, at, range)) = waiting.next() else { break };
        mover.lamp = Some(number);
        mover.fade = 0.0;
        light.radius = 0.0;
        light.range = range;
        // The shaders find a cube map's lamp by its number above the mark.
        light.shadow_depth_bias = (LAMP_MARK + number) as f32;
        transform.translation = at;
    }
}

pub struct LampsPlugin;

impl Plugin for LampsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LampAtlas>();
        app.init_resource::<LampBook>();
        app.add_systems(Update, (follow_lamps, pick_mover_cubes, pick_sharp_tiles).chain());
        follow_atlas::<LampAtlas>(app);
        follow_atlas::<LampBook>(app);
        app.add_plugins(ExtractResourcePlugin::<LampAtlas>::default());
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [(ATLAS_SHADER, Shader::from_wesl(include_str!("lamp_atlas.wesl"), "p2_engine/lamp_atlas.wesl"))]);
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.add_systems(Render, draw_lamp_atlas.in_set(RenderSystems::PrepareResources));
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

    #[test]
    fn the_lamp_level_sets_atlas_and_mover_cubes() {
        let table: Vec<(u32, usize, usize)> = (0..=4).map(lamp_quality).map(|q| (q.atlas_side, q.mover_cubes, q.cube_size)).collect();
        assert_eq!(table, [(0, 0, 512), (4096, 0, 512), (4096, 0, 512), (8192, 2, 1024), (16384, 4, 1024)]);
        assert_eq!(lamp_quality(9), lamp_quality(4));
    }

    #[test]
    fn a_lamps_record_holds_its_place_radius_colour_and_tile() {
        let light = MeshLight { location: p2_math::Vector::new(100.0, 200.0, 300.0), radius: 64.0, ..MeshLight::default() };
        let tile = 5 | 9 << 8 | 2 << 16;
        let record = lamp_record(&light, 1.0, Vec3::new(1.0, 2.0, 3.0), LAMP_MARK | tile);
        let at = Vec3::from(p2_bevy_assets::to_bevy(light.location)) + Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(record[0], [at.x, at.y, at.z, light.world_radius()]);
        let [r, g, b] = linear_light(light.color(1.0));
        assert_eq!(record[1], [r, g, b, tile as f32]);
        assert_eq!(record[1][3] as u32, tile);
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
        assert_eq!((image.width() as usize, image.height()), (BOOK_WIDTH, 1));
        assert_eq!(image.data.as_ref().map(|data| (data.len(), data[..8].to_vec())), Some((BOOK_WIDTH * 16, [1.0f32.to_le_bytes(), 2.0f32.to_le_bytes()].concat())));
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
