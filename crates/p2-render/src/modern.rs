//! Modern graphics option, off by default and followed at runtime. No baked light shows: a real-time sun with shadow
//! cascades, the map's other suns as fills and the eye's zone ambient light the world.

use bevy::anti_alias::contrast_adaptive_sharpening::ContrastAdaptiveSharpening;
use bevy::anti_alias::fxaa::Fxaa;
use bevy::anti_alias::smaa::{Smaa, SmaaPreset};
use bevy::app::{App, Plugin, PostUpdate, Update};
use bevy::asset::{Asset, AssetEvent, AssetEventSystems, AssetId, Assets};
use bevy::camera::{Camera, Camera3d, CameraOutputMode, ClearColorConfig};
use bevy::camera::Hdr;
use bevy::camera::visibility::RenderLayers;
use bevy::color::{Color, LinearRgba};
use bevy::core_pipeline::prepass::{DepthPrepass, NormalPrepass};
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut};
use bevy::ecs::entity::EntityHashSet;
use bevy::ecs::prelude::{Added, Changed, Commands, Component, Entity, Has, Local, Mut, Or, Query, Ref, Res, ResMut, SystemSet, With, Without};
use bevy::ecs::message::{MessageCursor, Messages};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::schedule::common_conditions::{resource_changed, resource_exists_and_changed};
use bevy::ecs::world::EntityWorldMut;
use bevy::light::{AmbientLight, CascadeShadowConfig, CascadeShadowConfigBuilder, Cascades, DirectionalLight, DirectionalLightShadowMap, ShadowFilteringMethod, SimulationLightSystems};
use bevy::log::info;
use bevy::material::AlphaMode;
use bevy::math::{Quat, Vec3};
use bevy::mesh::Mesh3d;
use bevy::pbr::{ContactShadows, ExtendedMaterial, MaterialExtension, MeshMaterial3d, ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel, StandardMaterial};
use bevy::render::occlusion_culling::OcclusionCulling;
use bevy::time::Time;
use bevy::transform::components::Transform;
use p2_math::Vector;
use p2_render_cpu::MeshLight;

use crate::components::{CASTER_LAYER, CanvasDrawn, FollowActor, FollowBone, MOVER_LAYER, OVERLAY_LAYERS, RESTING_PROP_LAYER, STATIC_CASTER_LAYER, SUN_VIEW_LAYER, SceneView, SunShadow, UNSEEN_LAYER};
use p2_host::display::{
    MODERN_ANTI_ALIASING, MODERN_AO, MODERN_BLOOM, MODERN_CONTACT_SHADOWS, MODERN_LAMP_SHADOWS, MODERN_LAMP_SIZE, MODERN_RELIEF, MODERN_SHADOW_DISTANCE, MODERN_SHADOWS,
    MODERN_SHARPEN, MODERN_SPECULAR, MODERN_TEMPORAL_AA, Settings,
};
use crate::material_class::level_bits;
use crate::level_collision::LevelCollision;
use crate::view::{NoSunCast, SKY_LAYER};
use crate::tone::ToneCamera;
use crate::actor_light::{ActorLitMaterial, HardwareLighting, SunSet, settle_step};
use crate::blended::{OpaqueDecals, TerrainLayerMaterial};
use crate::lightmapped::{BakedLight, LightmappedMaterial};
use crate::map_lights::{MapLights, follow_map_lights};
use crate::modern_bloom::ModernBloom;
use bevy::render::render_resource::BlendState;
use crate::mesh_light::{DynamicLit, MeshLitMaterial};
use crate::particle_material::ParticleMaterial;
use crate::sun_cache::{SunCachePlugin, SunCacheStale, follow_sun};
use crate::terrain_material::TerrainMaterial;
use crate::world_pass::WorldCasts;
use crate::view::ShownScene;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum ModernGraphicsSystems {
    Apply,
}

pub struct ModernGraphicsPlugin;

impl Plugin for ModernGraphicsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(crate::lamps::LampsPlugin);
        app.add_plugins(crate::probes::ProbesPlugin);
        app.add_plugins(SunCachePlugin);
        app.add_systems(Update, crate::lamps::set_load_light_levels.run_if(resource_exists_and_changed::<Settings>));
        app.add_systems(
            Update,
            (
                apply.run_if(resource_changed::<Settings>),
                (follow_map_lights, sun_shadows, follow_sun).chain(),
                view_ambient.after(crate::probes::follow_probes),
                (watch_props, sun_casters).chain(),
            )
                .in_set(ModernGraphicsSystems::Apply),
        );
        app.add_systems(
            PostUpdate,
            sun_view_cascades.after(SimulationLightSystems::UpdateDirectionalLightCascades).before(SimulationLightSystems::UpdateLightFrusta),
        );
        // Before the frame's asset events go out, so a new material reaches the GPU once, with its flag.
        app.add_systems(
            PostUpdate,
            (
                follow_flag::<LightmappedMaterial>,
                follow_flag::<MeshLitMaterial>,
                follow_flag::<ActorLitMaterial>,
                follow_flag::<TerrainMaterial>,
                follow_flag::<TerrainLayerMaterial>,
            )
                .before(AssetEventSystems),
        );
    }
}

pub const MODERN_ON: u8 = 1;
pub const NO_RELIEF: u8 = 2;
pub const NO_GLOSS: u8 = 4;

pub fn modern_flag(settings: &Settings) -> u8 {
    if !settings.modern_graphics() {
        return 0;
    }
    let off = |on: bool, bit: u8| if on { 0 } else { bit };
    MODERN_ON | off(settings.modern_effect(MODERN_RELIEF), NO_RELIEF) | off(settings.modern_effect(MODERN_SPECULAR), NO_GLOSS)
}

trait ModernFlagged: Asset {
    const KIND: &'static str;
    fn flag(&self) -> u16;
    fn set_flag(&mut self, flag: u16);
}

macro_rules! layers_flag {
    ($material:ty, $kind:literal) => {
        impl ModernFlagged for $material {
            const KIND: &'static str = $kind;
            fn flag(&self) -> u16 {
                self.extension.layers.detail.z.round() as u16
            }
            fn set_flag(&mut self, flag: u16) {
                self.extension.layers.detail.z = f32::from(flag);
            }
        }
    };
}
layers_flag!(LightmappedMaterial, "lightmapped");
layers_flag!(MeshLitMaterial, "static mesh");
layers_flag!(ActorLitMaterial, "actor");
layers_flag!(TerrainLayerMaterial, "terrain layer");

impl ModernFlagged for TerrainMaterial {
    const KIND: &'static str = "terrain";
    fn flag(&self) -> u16 {
        u16::try_from(self.extension.params.count.y).unwrap_or(u16::MAX)
    }
    fn set_flag(&mut self, flag: u16) {
        self.extension.params.count.y = u32::from(flag);
    }
}

/// Animated materials change every frame, which must not rescan the whole store: only a changed setting or a store
/// that grew this frame does, as its new ids are not known before the frame's asset events go out.
fn follow_flag<M: ModernFlagged>(
    settings: Res<Settings>,
    events: Option<Res<Messages<AssetEvent<M>>>>,
    mut cursor: Local<MessageCursor<AssetEvent<M>>>,
    mut known: Local<usize>,
    mut materials: ResMut<Assets<M>>,
) {
    let added: Vec<AssetId<M>> = events.as_deref().map_or_else(Vec::new, |events| {
        cursor.read(events).filter_map(|event| match *event { AssetEvent::Added { id } => Some(id), _ => None }).collect()
    });
    let bits = u16::from(modern_flag(&settings)) | level_bits(settings.material_level(), settings.modern_level(MODERN_LAMP_SIZE));
    let stale: Vec<_> = if settings.is_changed() || materials.len() != *known {
        *known = materials.len();
        materials.iter().filter(|(_, material)| material.flag() != bits).map(|(id, _)| id).collect()
    } else {
        added.into_iter().filter(|&id| materials.get(id).is_some_and(|material| material.flag() != bits)).collect()
    };
    for &id in &stale {
        if let Some(mut material) = materials.get_mut(id) {
            material.set_flag(bits);
        }
    }
    if !stale.is_empty() {
        info!("modern graphics flag {bits}: {} {} materials updated", stale.len(), M::KIND);
    }
}

/// The view and sky cameras share the world image only while their HDR agrees. Bloom runs before the tone curve,
/// anti-aliasing and sharpening after it; the depth prepass stays while the occlusion culling needs it.
pub fn apply(
    settings: Res<Settings>,
    mut commands: Commands,
    mut cameras: Query<(Entity, &mut Camera, Has<SceneView>, Has<Hdr>, Has<OcclusionCulling>, Option<&RenderLayers>), With<Camera3d>>,
    tone: Query<Entity, With<ToneCamera>>,
) {
    let bloom_level = settings.modern_level(MODERN_BLOOM);
    let sharpen = settings.modern_level(MODERN_SHARPEN);
    let contact = settings.modern_level(MODERN_CONTACT_SHADOWS);
    let ao = settings.modern_level(MODERN_AO);
    let sun_level = settings.modern_level(MODERN_SHADOWS);
    commands.insert_resource(DirectionalLightShadowMap { size: sun_quality(sun_level).0 });
    commands.insert_resource(WorldCasts(sun_level > 0));
    let sky = RenderLayers::layer(SKY_LAYER);
    for (entity, mut shown, view, hdr, culled, layers) in &mut cameras {
        let mut camera = commands.entity(entity);
        let hdr_wanted = settings.modern_graphics() && (view || layers.is_some_and(|layers| layers.intersects(&sky)));
        if hdr != hdr_wanted {
            if hdr_wanted {
                camera.insert(Hdr);
            } else {
                camera.remove::<(Hdr, ModernBloom)>();
            }
        }
        if !view {
            if layers.is_some_and(|layers| layers.intersects(&sky)) {
                camera.insert(if hdr_wanted { sky_view_mark() } else { AmbientLight { affects_lightmapped_meshes: false, ..AmbientLight::default() } });
            }
            continue;
        }
        // Bevy blends a later camera's frame over the target by its alpha. The view shares the sky's frame, which holds
        // the sky already, and the alpha there is the rays' mask, so under modern the view's frame replaces the target.
        let replaces = matches!(shown.output_mode, CameraOutputMode::Write { blend_state: Some(_), .. });
        if replaces != settings.modern_graphics() {
            shown.output_mode = if replaces {
                CameraOutputMode::default()
            } else {
                CameraOutputMode::Write { blend_state: Some(BlendState::REPLACE), clear_color: ClearColorConfig::None }
            };
        }
        if bloom_level > 0 {
            camera.insert(bloom(bloom_level));
        } else {
            camera.remove::<ModernBloom>();
        }
        let mut post = commands.entity(tone.single().unwrap_or(entity));
        if sharpen > 0 {
            post.insert(ContrastAdaptiveSharpening { sharpening_strength: f32::from(sharpen) / 10.0, ..ContrastAdaptiveSharpening::default() });
        } else {
            post.remove::<ContrastAdaptiveSharpening>();
        }
        let temporal = settings.modern_level(MODERN_TEMPORAL_AA) > 0;
        match if temporal { 0 } else { settings.modern_level(MODERN_ANTI_ALIASING) } {
            1 => post.remove::<Smaa>().insert(Fxaa::default()),
            2 => post.remove::<Fxaa>().insert(Smaa { preset: SmaaPreset::High }),
            _ => post.remove::<(Fxaa, Smaa)>(),
        };
        let mut camera = commands.entity(entity);
        if contact > 0 {
            camera.insert(sun_contact_shadows(contact));
        } else {
            camera.remove::<ContactShadows>();
        }
        let screen_reflections = settings.modern_level(p2_host::display::MODERN_REFLECTIONS) > 0 || settings.modern_level(p2_host::display::MODERN_BOUNCE_LIGHT) > 0;
        if screen_reflections {
            camera.insert((DepthPrepass, NormalPrepass));
        }
        if ao > 0 {
            camera.insert(ssao(ao));
        } else if screen_reflections {
            camera.remove::<ScreenSpaceAmbientOcclusion>();
        } else {
            camera.remove::<(ScreenSpaceAmbientOcclusion, NormalPrepass)>();
        }
        let fire_volumes = settings.modern_level(p2_host::display::MODERN_FIRE_VOLUMES) > 0;
        let lamp_fog = settings.modern_level(p2_host::display::MODERN_VOLUMETRIC_FOG) > 0;
        if fire_volumes || lamp_fog {
            camera.insert(DepthPrepass);
        }
        if contact == 0 && ao == 0 && !culled && !screen_reflections && !fire_volumes && !lamp_fog {
            camera.remove::<DepthPrepass>();
        }
        // Above level 1 the world shaders use their own filter; Gaussian is for the actors.
        camera.insert(if sun_level == 1 { ShadowFilteringMethod::Hardware2x2 } else { ShadowFilteringMethod::Gaussian });
        let current = layers.cloned().unwrap_or_default();
        let wanted = if settings.modern_graphics() { current.clone().with(SUN_VIEW_LAYER) } else { current.clone().without(SUN_VIEW_LAYER) };
        if wanted != current {
            camera.insert(wanted);
        }
    }
}

const BLOOM_PER_LEVEL: f32 = 0.02;

/// The sky camera's ambient light, which its shaders do not use: the negative alpha tells them to store the sky as
/// alpha 0, the mask the sun's rays read. Matches `modern.wesl`.
fn sky_view_mark() -> AmbientLight {
    AmbientLight { color: Color::LinearRgba(LinearRgba::new(0.0, 0.0, 0.0, -1.0)), brightness: 1.0, affects_lightmapped_meshes: false }
}

/// Added over the frame through a knee that starts at black: mid tones scatter a little, which is the haze, and bright
/// light scatters most.
pub fn bloom(level: u8) -> ModernBloom {
    ModernBloom {
        intensity: f32::from(level) * BLOOM_PER_LEVEL,
        low_frequency_boost: 0.7,
        low_frequency_boost_curvature: 0.95,
        high_pass_frequency: 1.0,
        threshold: 0.35,
        threshold_softness: 1.0,
    }
}

/// In Unreal units (about 2 cm each): Bevy's defaults are in metres (0.73 and 0.25), so about 32 and 12 units here (tuned
/// by eye, unverified). Needs the depth and normal prepasses.
fn ssao(level: u8) -> ScreenSpaceAmbientOcclusion {
    let quality_level = match level {
        1 => ScreenSpaceAmbientOcclusionQualityLevel::Low,
        2 => ScreenSpaceAmbientOcclusionQualityLevel::Medium,
        _ => ScreenSpaceAmbientOcclusionQualityLevel::High,
    };
    ScreenSpaceAmbientOcclusion { quality_level, radius: 32.0, constant_object_thickness: 12.0 }
}

#[derive(Component)]
struct FillSun;

/// Kept apart from [`PropMotion`] so its change detection fires only when the answer changes.
#[derive(Component, PartialEq)]
pub struct StillProp(pub bool);

/// A placed mesh casts with the static world, cached, once it has stood still this long, and with the movers while it moves.
const PROP_SETTLE_SECONDS: f32 = 2.0;
const MAP_LOAD_PROPS: usize = 50;

#[derive(Component)]
pub struct PropMotion {
    translation: Vec3,
    rotation: Quat,
    last_moved: f32,
    settled_before: bool,
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn watch_props(
    time: Res<Time>,
    settings: Res<Settings>,
    stale: Res<SunCacheStale>,
    mut commands: Commands,
    new: Query<(Entity, &FollowActor, &Transform), (Added<FollowActor>, Without<FollowBone>)>,
    placed: Query<(Entity, Ref<Transform>), With<PropMotion>>,
    mut props: Query<(&Transform, &mut PropMotion, &mut StillProp)>,
    mut settling: Local<EntityHashSet>,
) {
    if !casters_wanted(&settings) {
        return;
    }
    let now = time.elapsed_secs();
    // After a settings change every prop is checked again: one may have moved while nothing watched.
    let every = settings.is_changed();
    settling.extend(placed.iter().filter(|(_, transform)| every || transform.is_changed()).map(|(entity, _)| entity));
    settling.retain(|&entity| {
        let Ok((transform, mut motion, mut still)) = props.get_mut(entity) else { return false };
        if settle(transform, &mut motion, &mut still, now) {
            stale.mark();
        }
        !still.0
    });
    // A map's props arrive together and belong in every level. One spawned in play is too small for the coarse ones.
    let map_loading = new.iter().count() > MAP_LOAD_PROPS;
    for (entity, follow, transform) in &new {
        if follow.skeletal.is_none() {
            let motion = PropMotion { translation: transform.translation, rotation: transform.rotation, last_moved: now, settled_before: !map_loading };
            commands.entity(entity).try_insert((motion, StillProp(false)));
            settling.insert(entity);
        }
    }
}

/// Whether it has just settled for the first time: that draws it into every level, a door's later swings only the near ones.
fn settle(transform: &Transform, motion: &mut PropMotion, still: &mut Mut<StillProp>, now: f32) -> bool {
    if transform.translation != motion.translation || transform.rotation != motion.rotation {
        motion.translation = transform.translation;
        motion.rotation = transform.rotation;
        motion.last_moved = now;
        still.set_if_neq(StillProp(false));
    } else if now - motion.last_moved > PROP_SETTLE_SECONDS {
        still.set_if_neq(StillProp(true));
        return !std::mem::replace(&mut motion.settled_before, true);
    }
    false
}

/// Shaders read the colour as is, at illuminance 1; Bevy's own PBR lights take about a thousandth of it. The biases (world
/// units, tuned by eye, unverified) serve the actors' shadow lookup; the world's filter has its own.
fn sun_light(color: [f32; 3], shadows: bool, contact: bool) -> DirectionalLight {
    let [r, g, b] = color;
    DirectionalLight {
        color: Color::LinearRgba(LinearRgba::rgb(r, g, b)),
        illuminance: 1.0,
        shadow_maps_enabled: shadows,
        contact_shadows_enabled: contact,
        affects_lightmapped_mesh_diffuse: false,
        shadow_depth_bias: 2.0,
        shadow_normal_bias: 2.5,
    }
}

/// A light's colour is stored in display space and doubled when drawn, so its linear light is that doubled and squared.
pub(crate) fn linear_light(color: [f32; 3]) -> [f32; 3] {
    color.map(|channel| 4.0 * channel * channel)
}

/// In Unreal units: rays of 48 units (Bevy's 0.3 m is about 15), depth texels 16 units thick (tuned by eye, unverified).
fn sun_contact_shadows(level: u8) -> ContactShadows {
    ContactShadows { linear_steps: if level > 1 { 16 } else { 8 }, thickness: 16.0, length: 48.0 }
}

fn sun_place(direction: Vec3) -> Transform {
    let up = if direction.normalize_or_zero().y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    Transform::IDENTITY.looking_to(direction, up)
}

/// A map without a sun still gets contact shadows: the key light's side stands for it, and its black light adds nothing.
const NO_SUN_DIRECTION: Vec3 = Vec3::new(-0.458088, -0.814379, -0.356291);

/// By the sun's shadow level: the side of every directional shadow map and the static world's cached levels. Level 0
/// draws no shadow map.
pub(crate) fn sun_quality(level: u8) -> (usize, usize) {
    match level {
        0 | 1 => (1024, 3),
        2 => (2048, 3),
        3 => (2048, 4),
        _ => (4096, 4),
    }
}

/// Two, as a sun with one cascade reads as a cached level.
const MOVER_CASCADES: usize = 2;

/// In Unreal units. Every mover in a cascade is queued and drawn again each frame, and past this its shadow is a few
/// pixels wide.
const MOVER_SHADOW_REACH: f32 = 4096.0;

/// In Unreal units, by the shadow distance level 1 to 5: the movers' reach and the outermost cached level's half width.
const SHADOW_REACH: [f32; 5] = [2048.0, 4096.0, 8192.0, 16384.0, 32768.0];
const FIRST_CASCADE_FAR: f32 = 400.0;

/// Also what `sun_shadow.wesl` tells the sun from the fills by while it draws no shadow map.
const SUN_OVERLAP: f32 = 0.1;

pub(crate) fn shadow_reach(level: u8) -> f32 {
    SHADOW_REACH[usize::from(level.clamp(1, 5) - 1)]
}

fn sun_cascades(distance_level: u8) -> CascadeShadowConfig {
    CascadeShadowConfigBuilder {
        num_cascades: MOVER_CASCADES,
        minimum_distance: 1.0,
        maximum_distance: shadow_reach(distance_level).min(MOVER_SHADOW_REACH),
        first_cascade_far_bound: FIRST_CASCADE_FAR,
        overlap_proportion: SUN_OVERLAP,
    }
    .build()
}

/// With the sun and the later cached levels this stays inside Bevy's ten directional lights.
const MAX_FILL_SUNS: usize = 4;

/// Suns this close in direction light the same way: maps stack several on one direction, and their light adds up.
const SAME_SUN_COS: f32 = 0.99452;

/// The map's still suns grouped by direction, each group's colours summed as retail's baked light sums them. The
/// shadowed sun's group is first when there is one, then the strongest others.
fn sun_groups(set: SunSet) -> Vec<(Vector, [f32; 3])> {
    let strength = |light: &MeshLight| light.color(set.level_brightness).iter().sum::<f32>();
    let mut candidates: Vec<&MeshLight> = set.suns.iter().collect();
    candidates.sort_by(|a, b| {
        strength(b).total_cmp(&strength(a)).then_with(|| <[f32; 3]>::from(a.location).partial_cmp(&<[f32; 3]>::from(b.location)).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut groups: Vec<(Vector, [f32; 3])> = set.sun.map(|(light, _)| (light.rotation.vector(), [0.0; 3])).into_iter().collect();
    for light in candidates {
        let (along, color) = (light.rotation.vector(), light.color(set.level_brightness));
        match groups.iter_mut().find(|(other, _)| other.dot(along) > SAME_SUN_COS) {
            Some((_, sum)) => sum.iter_mut().zip(color).for_each(|(sum, channel)| *sum += channel),
            None => groups.push((along, color)),
        }
    }
    groups
}

/// The shadowed sun's linear colour: every sun on its direction together.
pub(crate) fn sun_color(set: SunSet) -> [f32; 3] {
    let Some(sun) = set.sun else { return [0.0; 3] };
    let own = sun.0.color(set.level_brightness);
    let summed = sun_groups(set).first().map_or(own, |&(_, sum)| sum);
    // A sun that is not in the list of lights still shines.
    linear_light(if summed.iter().sum::<f32>() > 0.0 { summed } else { own })
}

pub(crate) fn fill_suns(set: SunSet) -> Vec<(Vec3, [f32; 3])> {
    sun_groups(set)
        .into_iter()
        .skip(usize::from(set.sun.is_some()))
        .take(MAX_FILL_SUNS)
        .map(|(along, color)| (Vec3::from(p2_bevy_assets::to_bevy(along)), linear_light(color)))
        .collect()
}

fn sun_shadows(
    settings: Res<Settings>,
    map: Option<Res<MapLights>>,
    mut commands: Commands,
    mut suns: Query<(Entity, &mut DirectionalLight, &mut Transform, &mut CascadeShadowConfig), With<SunShadow>>,
    fills: Query<Entity, With<FillSun>>,
    mut last_fills: Local<Vec<(Vec3, [f32; 3])>>,
    time: Res<Time>,
) {
    let Some(map) = map else {
        for entity in suns.iter().map(|(entity, ..)| entity).chain(&fills) {
            commands.entity(entity).despawn();
        }
        last_fills.clear();
        return;
    };
    let sun = map.sun.as_ref();
    let sun_level = settings.modern_level(MODERN_SHADOWS);
    let place = sun_place(sun.map_or(NO_SUN_DIRECTION, |(_, hardware)| Vec3::from(p2_bevy_assets::to_bevy(hardware.direction))));
    let wanted = sun_light(map.sun_color, sun_level > 0 && sun.is_some(), settings.modern_level(MODERN_CONTACT_SHADOWS) > 0);
    let cascades = sun_cascades(settings.modern_level(MODERN_SHADOW_DISTANCE));
    let mut found = false;
    for (entity, mut current, mut transform, mut config) in &mut suns {
        if std::mem::replace(&mut found, true) {
            commands.entity(entity).despawn();
            continue;
        }
        // Another zone's suns come in as its ambient does.
        let color = Color::LinearRgba(blend_light(current.color.to_linear(), wanted.color.to_linear(), settle_step(time.delta_secs(), ZONE_AMBIENT_BLEND_SECONDS)));
        if current.color != color || current.contact_shadows_enabled != wanted.contact_shadows_enabled || current.shadow_maps_enabled != wanted.shadow_maps_enabled {
            *current = DirectionalLight { color, ..wanted };
        }
        if config.bounds != cascades.bounds {
            *config = cascades.clone();
        }
        transform.set_if_neq(place);
    }
    if !found {
        let layers = RenderLayers::from_layers(&[SUN_VIEW_LAYER, MOVER_LAYER]);
        commands.spawn((SunShadow, wanted, place, cascades, layers));
    }
    if *last_fills != map.fills {
        for entity in &fills {
            commands.entity(entity).despawn();
        }
        for &(along, [r, g, b]) in &map.fills {
            let config = CascadeShadowConfig { overlap_proportion: 0.0, ..CascadeShadowConfig::default() };
            let light = DirectionalLight { color: Color::LinearRgba(LinearRgba::rgb(r, g, b)), illuminance: 1.0, shadow_maps_enabled: false, affects_lightmapped_mesh_diffuse: false, ..DirectionalLight::default() };
            commands.spawn((FillSun, light, config, sun_place(along), RenderLayers::layer(SUN_VIEW_LAYER)));
        }
        last_fills.clone_from(&map.fills);
    }
}

/// Only the scene camera shares a layer with the sun, so the other cameras' cascades would be culled for nothing.
fn sun_view_cascades(mut suns: Query<&mut Cascades, With<SunShadow>>, views: Query<(), With<SceneView>>) {
    for mut cascades in &mut suns {
        if cascades.cascades.keys().any(|&camera| !views.contains(camera)) {
            cascades.cascades.retain(|&camera, _| views.contains(camera));
        }
    }
}

/// Retail snaps a zone's ambient at a zone crossing; ours fades, as the zone fog does.
const ZONE_AMBIENT_BLEND_SECONDS: f32 = 0.35;

/// The zone the eye is in lights every surface with its ambient, under a roof or not. In display light, as `modern.wesl`
/// adds it; the alpha carries the probe grid's spacing, 0 without one.
pub fn view_ambient(
    time: Res<Time>,
    settings: Res<Settings>,
    shown: Option<Res<ShownScene>>,
    level: Res<LevelCollision>,
    probes: Option<Res<crate::probes::ProbeAtlas>>,
    mut cameras: Query<(&SceneView, &mut AmbientLight)>,
) {
    let environment = shown.as_deref().and_then(|shown| Some((shown, shown.lighting.as_deref()?))).filter(|_| settings.modern_graphics());
    let step = settle_step(time.delta_secs(), ZONE_AMBIENT_BLEND_SECONDS);
    for (view, mut ambient) in &mut cameras {
        let wanted = match environment {
            Some((shown, environment)) => {
                let position = p2_bevy_assets::to_bevy(view.position - shown.origin);
                let zone = level.model().map_or(0, |model| p2_collision::point_zone(model, position.into()));
                let [r, g, b] = environment.zone_ambients.get(usize::from(zone)).copied().unwrap_or_default();
                let target = LinearRgba::new(r, g, b, probes.as_ref().map_or(0.0, |probes| probes.spacing));
                let color = if ambient.brightness == 1.0 { blend_light(ambient.color.to_linear(), target, step) } else { target };
                AmbientLight { color: Color::LinearRgba(color), brightness: 1.0, affects_lightmapped_meshes: false }
            }
            None => AmbientLight { affects_lightmapped_meshes: false, ..AmbientLight::default() },
        };
        if ambient.color != wanted.color || ambient.brightness != wanted.brightness {
            *ambient = wanted;
        }
    }
}

fn blend_light(shown: LinearRgba, target: LinearRgba, t: f32) -> LinearRgba {
    let mix = |a: f32, b: f32| a + (b - a) * t;
    let blended = LinearRgba::new(mix(shown.red, target.red), mix(shown.green, target.green), mix(shown.blue, target.blue), target.alpha);
    let near = [blended.red - target.red, blended.green - target.green, blended.blue - target.blue].iter().all(|d| d.abs() < 1.0 / 512.0);
    if near { target } else { blended }
}

/// The lamps' shadows of movers read the same layers and resting marks as the sun's, with the sun's shadows off too.
fn casters_wanted(settings: &Settings) -> bool {
    settings.modern_level(MODERN_SHADOWS) > 0 || settings.modern_level(MODERN_LAMP_SHADOWS) > 0
}

fn sun_casters(
    settings: Res<Settings>,
    stale: Res<SunCacheStale>,
    mut on_before: Local<bool>,
    mut commands: Commands,
    all: Query<Entity, With<Mesh3d>>,
    changed: Query<
        Entity,
        (
            With<Mesh3d>,
            Or<(
                Added<Mesh3d>,
                Changed<StillProp>,
                Changed<RenderLayers>,
                Added<CanvasDrawn>,
                Changed<MeshMaterial3d<LightmappedMaterial>>,
                Changed<MeshMaterial3d<MeshLitMaterial>>,
                Changed<MeshMaterial3d<ActorLitMaterial>>,
                Changed<MeshMaterial3d<StandardMaterial>>,
            )>,
        ),
    >,
) {
    let on = casters_wanted(&settings);
    let flipped = std::mem::replace(&mut *on_before, on) != on;
    if !on && !flipped {
        return;
    }
    let mut retag = |entity: Entity| {
        let stale = stale.clone();
        commands.entity(entity).queue_silenced(move |mut entity: EntityWorldMut| {
            let current = entity.get::<RenderLayers>().cloned().unwrap_or_default();
            let casts = on && !entity.contains::<CanvasDrawn>() && !entity.contains::<NoSunCast>() && !is_see_through(&entity);
            let still = entity.get::<StillProp>().is_some_and(|still| still.0);
            let mover = (entity.contains::<FollowActor>() || entity.contains::<FollowBone>()) && !still;
            let cast = caster_layers(&current, casts, mover);
            let resting = still && cast.intersects(&RenderLayers::layer(STATIC_CASTER_LAYER));
            let wanted = if resting { cast.with(RESTING_PROP_LAYER) } else { cast.without(RESTING_PROP_LAYER) };
            if wanted != current {
                let cached = RenderLayers::layer(STATIC_CASTER_LAYER);
                if wanted.intersects(&cached) != current.intersects(&cached) {
                    stale.mark_near();
                }
                entity.insert(wanted);
            }
        });
    };
    if flipped {
        all.iter().for_each(&mut retag);
    } else {
        changed.iter().for_each(&mut retag);
    }
}

/// Glass, fading and additive surfaces let the sun through; masked ones keep casting, with their holes.
fn see_through_mode(mode: AlphaMode) -> bool {
    match mode {
        AlphaMode::Opaque | AlphaMode::AlphaToCoverage => false,
        AlphaMode::Mask(cutoff) => cutoff > 1.0,
        _ => true,
    }
}

fn extended_alpha<E: MaterialExtension>(entity: &EntityWorldMut) -> Option<AlphaMode> {
    let handle = entity.get::<MeshMaterial3d<ExtendedMaterial<StandardMaterial, E>>>()?;
    Some(entity.world().get_resource::<Assets<ExtendedMaterial<StandardMaterial, E>>>()?.get(&handle.0)?.base.alpha_mode)
}

fn is_see_through(entity: &EntityWorldMut) -> bool {
    let standard = || {
        let handle = entity.get::<MeshMaterial3d<StandardMaterial>>()?;
        Some(entity.world().get_resource::<Assets<StandardMaterial>>()?.get(&handle.0)?.alpha_mode)
    };
    entity.contains::<MeshMaterial3d<ParticleMaterial>>()
        || extended_alpha::<DynamicLit>(entity)
            .or_else(|| extended_alpha::<HardwareLighting>(entity))
            .or_else(|| extended_alpha::<BakedLight>(entity))
            .or_else(|| extended_alpha::<OpaqueDecals>(entity))
            .or_else(standard)
            .is_some_and(see_through_mode)
}

fn caster_layers(layers: &RenderLayers, casts: bool, mover: bool) -> RenderLayers {
    let world = layers.intersects(&RenderLayers::from_layers(&[0, CASTER_LAYER, UNSEEN_LAYER])) && !layers.intersects(&RenderLayers::from_layers(&OVERLAY_LAYERS));
    let casts = casts && world;
    let on_layer = |layers: RenderLayers, layer: usize, wanted: bool| if wanted { layers.with(layer) } else { layers.without(layer) };
    let tagged = on_layer(on_layer(layers.clone(), CASTER_LAYER, casts), MOVER_LAYER, casts && mover);
    on_layer(tagged, STATIC_CASTER_LAYER, casts && !mover)
}

#[cfg(test)]
mod tests;
