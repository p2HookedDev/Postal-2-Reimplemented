//! Modern lamps: every placed lamp is a clustered real-time light. Its shadow of the static world comes from one atlas
//! drawn once a map ([`atlas`]); the strongest point lamps near the eye add a Bevy cube map for movers.

pub(crate) mod atlas;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use bevy::app::{App, Plugin, PostUpdate, Update};
use bevy::asset::{Asset, AssetEvent, AssetEventSystems, Assets, Handle};
use bevy::camera::visibility::{InheritedVisibility, RenderLayers};
use bevy::color::{Color, LinearRgba};
use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::message::{MessageCursor, Messages};
use bevy::ecs::prelude::{Commands, Component, Entity, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::{PointLight, PointLightShadowMap, SpotLight};
use bevy::math::Vec3;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, StandardMaterial};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::shader::Shader;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use bevy::transform::components::Transform;
use p2_assets::Model;
use p2_render_cpu::{LE_SUNLIGHT, MeshLight};

use crate::components::SceneView;
use crate::display::{MODERN_LAMP_SHADOWS, Settings};
use crate::level_collision::{LevelCollision, StaticCollision};
use crate::mesh_view::{SeenWithin, ZoneCulled};
use crate::render::actor_light::{HardwareLighting, LightEnvironment};
use crate::render::blended::{OpaqueDecals, TerrainLayer};
use crate::render::bsp_light::{BevyLight, gpu_light, place};
use crate::render::lightmapped::BakedLight;
use crate::render::mesh_light::DynamicLit;
use crate::render::modern::{MOVER_LAYER, SUN_VIEW_LAYER, linear_light};
use crate::render::shaders::add_shaders;
use crate::render::terrain_material::TerrainLayers;
use crate::shown_scene::ShownScene;
use atlas::{ATLAS_SHADER, LampBake, UNMAPPED, draw_lamp_atlas, lamp_bake, new_atlas, world_masked, world_triangles};

/// Where `modern.wesl`'s fade ends. A far lamp's pool covers few pixels but costs every cluster and list on its way.
const LAMP_SEEN_RADII: f32 = 30.0;
/// The share of a lamp's radius taken as the size of its source, which widens its highlight.
const SOURCE_SHARE: f32 = 0.08;

const MOVER_PICK_FRAMES: u32 = 10;
const MOVER_KEEP_FACTOR: f32 = 1.5;
const MOVER_SHADOW_NEAR: f32 = 4.0;

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
        3 => (8192, 1, 1024),
        _ => (8192, 2, 1024),
    };
    LampQuality { atlas_side, mover_cubes, cube_size }
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

#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
struct LampAtlas {
    image: Option<Handle<Image>>,
    bake: Option<Arc<LampBake>>,
    side: u32,
}

type LampJobResult = (Vec<MeshLight>, Option<LampBake>);

#[derive(Default)]
struct LampJob {
    key: Option<(usize, usize, u32)>,
    task: Option<Task<LampJobResult>>,
    ready: Option<LampJobResult>,
}

fn lamp_job(environment: &LightEnvironment, collision: &StaticCollision, origin: Vec3, side: u32) -> LampJobResult {
    let lamps = map_lamps(environment);
    let bake = (side > 0).then(|| {
        let spheres: Vec<_> = lamps.iter().map(|light| ((Vec3::from(p2_bevy_assets::to_bevy(light.location)) + origin).to_array(), light.world_radius())).collect();
        lamp_bake(&world_triangles(collision, origin), &world_masked(collision, origin), &spheres)
    });
    (lamps, bake)
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

/// A lamp whose reach touches no zone the eye can see costs nothing: zone culling hides it, as it hides meshes.
fn lamp_zone(light: &MeshLight, model: &Model) -> ZoneCulled {
    let (center, radius) = (<[f32; 3]>::from(light.location), light.world_radius());
    let (min, max) = (center.map(|axis| axis - radius), center.map(|axis| axis + radius));
    ZoneCulled { zones: p2_collision::box_zones(model, min.into(), max.into()), min, max, only_from: None }
}

fn spawn_lamp(commands: &mut Commands, light: &MeshLight, packed: u32, environment: &LightEnvironment, origin: Vec3, zone: ZoneCulled) {
    let Some(gpu) = gpu_light(light, environment.level_brightness) else { return };
    let at = place(light, &gpu, origin);
    let [r, g, b] = linear_light(light.color(environment.level_brightness));
    let color = Color::LinearRgba(LinearRgba::rgb(r, g, b));
    let radius = (light.world_radius() * SOURCE_SHARE).max(1.0);
    let shadow_depth_bias = packed as f32;
    let seen = (Lamp, at, RenderLayers::from_layers(&[SUN_VIEW_LAYER, MOVER_LAYER]), zone, SeenWithin(light.world_radius() * LAMP_SEEN_RADII));
    match gpu {
        BevyLight::Point(point) => commands.spawn((seen, PointLight { color, radius, shadow_depth_bias, shadow_map_near_z: MOVER_SHADOW_NEAR, ..point })),
        BevyLight::Spot(spot) => commands.spawn((seen, SpotLight { color, radius, shadow_depth_bias, ..spot })),
    };
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn follow_lamps(
    settings: Res<Settings>,
    shown: Option<Res<ShownScene>>,
    level: Option<Res<LevelCollision>>,
    mut atlas: ResMut<LampAtlas>,
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
    let Some((map, bake)) = job.ready.take().or_else(|| job.task.take().map(block_on)) else { return };
    for (index, light) in map.iter().enumerate() {
        let packed = bake.as_ref().map_or(UNMAPPED, |bake| bake.tile_bits(index));
        spawn_lamp(&mut commands, light, packed, &environment, shown.origin, lamp_zone(light, model));
    }
    atlas.bake = bake.map(Arc::new);
}

/// One spawned as another goes leaves the count the same, so a count alone misses it.
fn material_added<A: Asset>(events: Option<&Messages<AssetEvent<A>>>, cursor: &mut MessageCursor<AssetEvent<A>>) -> bool {
    events.is_some_and(|events| cursor.read(events).fold(false, |added, event| added | matches!(event, AssetEvent::Added { .. })))
}

/// Our shaders replace Bevy's lighting, the only reader of these slots, so the atlas needs no new binding.
#[derive(Clone, Copy)]
pub(crate) enum MaterialSlot {
    Occlusion,
    MetallicRoughness,
}

impl MaterialSlot {
    fn of(self, material: &StandardMaterial) -> &Option<Handle<Image>> {
        match self {
            Self::Occlusion => &material.occlusion_texture,
            Self::MetallicRoughness => &material.metallic_roughness_texture,
        }
    }

    fn of_mut(self, material: &mut StandardMaterial) -> &mut Option<Handle<Image>> {
        match self {
            Self::Occlusion => &mut material.occlusion_texture,
            Self::MetallicRoughness => &mut material.metallic_roughness_texture,
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

fn mover_score(color: &Color, range: f32, distance: f32) -> f32 {
    let linear = color.to_linear();
    let x = (distance / range).min(1.0);
    (linear.red + linear.green + linear.blue) * (1.0 - x * x * (3.0 - 2.0 * x))
}

/// Spot lamps keep no mover map: Bevy sizes the sun's shadow texture by the spot maps in view too.
fn pick_mover_cubes(
    settings: Res<Settings>,
    mut frames: Local<u32>,
    cameras: Query<&Transform, With<SceneView>>,
    mut lamps: Query<(&Transform, &mut PointLight, &InheritedVisibility), With<Lamp>>,
) {
    *frames += 1;
    if *frames < MOVER_PICK_FRAMES {
        return;
    }
    *frames = 0;
    let Some(eye) = cameras.iter().next().map(|camera| camera.translation) else { return };
    let mut scored: Vec<(usize, f32)> = lamps
        .iter()
        .enumerate()
        .filter(|(_, (_, _, shown))| shown.get())
        .map(|(index, (transform, light, _))| {
            let score = mover_score(&light.color, light.range, transform.translation.distance(eye));
            (index, if light.shadow_maps_enabled { score * MOVER_KEEP_FACTOR } else { score })
        })
        .filter(|&(_, score)| score > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(lamp_quality(settings.modern_level(MODERN_LAMP_SHADOWS)).mover_cubes);
    for (index, (_, mut light, _)) in lamps.iter_mut().enumerate() {
        let wanted = scored.iter().any(|&(picked, _)| picked == index);
        if light.shadow_maps_enabled != wanted {
            light.shadow_maps_enabled = wanted;
        }
    }
}

pub struct LampsPlugin;

impl Plugin for LampsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LampAtlas>();
        app.add_systems(Update, (follow_lamps, pick_mover_cubes).chain());
        follow_atlas::<LampAtlas>(app);
        app.add_plugins(ExtractResourcePlugin::<LampAtlas>::default());
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [(ATLAS_SHADER, Shader::from_wesl(include_str!("lamp_atlas.wesl"), "p2_engine/lamp_atlas.wesl"))]);
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.add_systems(Render, draw_lamp_atlas.in_set(RenderSystems::PrepareResources));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lamp_level_sets_atlas_and_mover_cubes() {
        let table: Vec<(u32, usize, usize)> = (0..=4).map(lamp_quality).map(|q| (q.atlas_side, q.mover_cubes, q.cube_size)).collect();
        assert_eq!(table, [(0, 0, 512), (4096, 0, 512), (4096, 0, 512), (8192, 1, 1024), (8192, 2, 1024)]);
        assert_eq!(lamp_quality(9), lamp_quality(4));
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
