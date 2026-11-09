//! Modern graphics option, off by default and followed at runtime. One realtime sun casts only dynamic
//! objects; shaders darken the baked sun only where the sun mask has it and such an object blocks it.

use bevy::anti_alias::contrast_adaptive_sharpening::ContrastAdaptiveSharpening;
use bevy::anti_alias::fxaa::Fxaa;
use bevy::app::{App, Plugin, Update};
use bevy::asset::{Asset, AssetEvent, Assets};
use bevy::camera::Camera3d;
use bevy::camera::visibility::RenderLayers;
use bevy::color::{Color, LinearRgba};
use bevy::core_pipeline::prepass::{DepthPrepass, NormalPrepass};
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut};
use bevy::ecs::prelude::{Added, Changed, Commands, Component, Entity, Has, Local, MessageReader, Or, Query, Res, ResMut, SystemSet, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::schedule::common_conditions::resource_changed;
use bevy::ecs::world::EntityWorldMut;
use bevy::light::{CascadeShadowConfig, CascadeShadowConfigBuilder, DirectionalLight};
use bevy::log::info;
use bevy::math::Vec3;
use bevy::material::AlphaMode;
use bevy::mesh::Mesh3d;
use bevy::pbr::{ContactShadows, ExtendedMaterial, MaterialExtension, MeshMaterial3d, ScreenSpaceAmbientOcclusion, StandardMaterial};
use bevy::post_process::bloom::{Bloom, BloomPrefilter};
use bevy::render::occlusion_culling::OcclusionCulling;
use bevy::camera::Hdr;
use bevy::transform::components::Transform;
use p2_math::Vector;
use p2_render_cpu::{HardwareLight, MeshLight};

use crate::render::actor_light::{ActorLitMaterial, HardwareLighting};
use crate::components::FollowBone;
use crate::render::blended::{OpaqueDecals, TerrainLayerMaterial};
use p2_render_cpu::LE_SUNLIGHT;
use crate::display::{MODERN_AO, MODERN_CONTACT_SHADOWS, MODERN_POST, MODERN_RELIEF, MODERN_SHADOWS, MODERN_SPECULAR, Settings};
use crate::render::lightmapped::{BakedLight, LightmappedMaterial};
use crate::render::mesh_light::{DynamicLit, MeshLitMaterial};
use crate::render::particle_material::ParticleMaterial;
use crate::components::{CanvasDrawn, FollowActor, OVERLAY_LAYERS};
use crate::shown_scene::ShownScene;
use crate::components::SceneView;
use crate::render::sun_mask::{SunMask, sun_mask};
use crate::render::terrain_material::TerrainMaterial;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum ModernGraphicsSystems {
    Apply,
}

pub struct ModernGraphicsPlugin;

impl Plugin for ModernGraphicsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SunMask>();
        app.add_systems(
            Update,
            (
                apply.run_if(resource_changed::<Settings>),
                follow_flag::<LightmappedMaterial>,
                follow_flag::<MeshLitMaterial>,
                follow_flag::<ActorLitMaterial>,
                follow_flag::<TerrainMaterial>,
                follow_flag::<TerrainLayerMaterial>,
                sun_mask,
                sun_shadows,
                sun_casters,
            )
                .in_set(ModernGraphicsSystems::Apply),
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
    fn flag(&self) -> u8;
    fn set_flag(&mut self, flag: u8);
}

macro_rules! layers_flag {
    ($material:ty, $kind:literal) => {
        impl ModernFlagged for $material {
            const KIND: &'static str = $kind;
            fn flag(&self) -> u8 {
                self.extension.layers.detail.z.round() as u8
            }
            fn set_flag(&mut self, flag: u8) {
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
    fn flag(&self) -> u8 {
        u8::try_from(self.extension.params.count.y).unwrap_or(u8::MAX)
    }
    fn set_flag(&mut self, flag: u8) {
        self.extension.params.count.y = u32::from(flag);
    }
}

/// Animated materials change every frame, which must not rescan the whole store.
fn follow_flag<M: ModernFlagged>(settings: Res<Settings>, mut events: MessageReader<AssetEvent<M>>, mut materials: ResMut<Assets<M>>) {
    let bits = modern_flag(&settings);
    let store = materials.as_ref();
    let stale: Vec<_> = if settings.is_changed() {
        store.iter().filter(|(_, material)| material.flag() != bits).map(|(id, _)| id).collect()
    } else {
        events
            .read()
            .filter_map(|event| match event {
                AssetEvent::Added { id } | AssetEvent::Modified { id } => Some(*id),
                _ => None,
            })
            .filter(|&id| store.get(id).is_some_and(|material| material.flag() != bits))
            .collect()
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

/// HDR goes on every 3D camera together: they draw into one shared texture, which only stays shared while they agree. The
/// post effects go on the view camera only, so each runs once per frame (the first-person weapon, drawn after, is left as
/// retail draws it). No tone curve either way: values past 1 only feed the bloom ([`bloom`]). Contact shadows bring the
/// depth prepass; off takes it away again unless the occlusion culling has its own. SSAO is off by default (its
/// blotches) and turns MSAA off while on (`follow_msaa`). Only what differs is touched.
fn apply(
    settings: Res<Settings>,
    mut commands: Commands,
    cameras: Query<
        (Entity, Has<SceneView>, Has<Hdr>, Has<ContactShadows>, Has<ScreenSpaceAmbientOcclusion>, Has<OcclusionCulling>, Option<&RenderLayers>),
        With<Camera3d>,
    >,
) {
    let post = settings.modern_effect(MODERN_POST);
    let contact = settings.modern_effect(MODERN_CONTACT_SHADOWS);
    for (camera, view, hdr, contact_now, ssao_now, culled, layers) in &cameras {
        let mut camera = commands.entity(camera);
        if hdr != post {
            if !post {
                camera.remove::<(Hdr, Bloom, Fxaa, ContrastAdaptiveSharpening)>();
            } else if view {
                let sharpening = ContrastAdaptiveSharpening { sharpening_strength: 0.45, ..ContrastAdaptiveSharpening::default() };
                camera.insert((Hdr, bloom(), Fxaa::default(), sharpening));
            } else {
                camera.insert(Hdr);
            }
        }
        if !view {
            continue;
        }
        if contact != contact_now {
            if contact {
                camera.insert(sun_contact_shadows());
            } else {
                camera.remove::<ContactShadows>();
            }
        }
        let ssao_wanted = settings.modern_effect(MODERN_AO);
        if ssao_wanted != ssao_now {
            if ssao_wanted {
                camera.insert(ssao());
            } else {
                camera.remove::<(ScreenSpaceAmbientOcclusion, NormalPrepass)>();
            }
        }
        if !contact && !ssao_wanted && !culled {
            camera.remove::<DepthPrepass>();
        }
        let current = layers.cloned().unwrap_or_default();
        let wanted = if settings.modern_effect(MODERN_SHADOWS) || contact { current.clone().with(SUN_VIEW_LAYER) } else { current.clone().without(SUN_VIEW_LAYER) };
        if wanted != current {
            camera.insert(wanted);
        }
    }
}

fn bloom() -> Bloom {
    Bloom { intensity: 0.15, prefilter: BloomPrefilter { threshold: 0.85, threshold_softness: 0.3 }, ..Bloom::OLD_SCHOOL }
}

/// In Unreal units (about 2 cm each): Bevy's defaults are in metres (0.73 and 0.25), so about 32 and 12 units here (tuned
/// by eye, unverified). Needs the depth and normal prepasses. Off by default (its blotches).
fn ssao() -> ScreenSpaceAmbientOcclusion {
    ScreenSpaceAmbientOcclusion { radius: 32.0, constant_object_thickness: 12.0, ..ScreenSpaceAmbientOcclusion::default() }
}

#[derive(Component)]
pub struct SunShadow;

/// The layer the view camera adds while the sun shadows are on, shared by the sun so the view sees it while the sun
/// keeps its own casters (Bevy: a light's casters are the meshes sharing a layer with it). No mesh is on it.
/// Layers 0 to 6 are the view, overlay, sky and gamma ones (`mesh_view.rs`, `gamma.rs`).
pub const SUN_VIEW_LAYER: usize = 7;

/// The layer [`sun_casters`] adds to every dynamic object's mesh in the world: the sun's only casters. No camera sees
/// it, and no static world mesh is ever on it, so the static world never casts (its shadows are baked).
pub const CASTER_LAYER: usize = 8;

pub(crate) fn brightest_sun<'a>(lights: impl IntoIterator<Item = &'a MeshLight>, level_brightness: f32) -> Option<(MeshLight, HardwareLight)> {
    let strength = |light: &HardwareLight| light.color.iter().sum::<f32>();
    lights
        .into_iter()
        .filter(|light| light.effect == LE_SUNLIGHT)
        .filter_map(|light| Some((*light, HardwareLight::new(light, level_brightness, Vector::default(), 0.0)?)))
        // Equal suns by location: the lights come in hash order, which differs between runs.
        .max_by(|(light_a, a), (light_b, b)| {
            strength(a)
                .total_cmp(&strength(b))
                .then_with(|| light_a.location.x.total_cmp(&light_b.location.x))
                .then_with(|| light_a.location.y.total_cmp(&light_b.location.y))
                .then_with(|| light_a.location.z.total_cmp(&light_b.location.z))
        })
}

/// The colour is what the baked light held of it, so the shaders take off exactly the baked sun. At illuminance 1 (Bevy
/// premultiplies the colour by it; on a surface Bevy's own PBR lights, 1 lux is about a thousandth: invisible). The depth
/// bias is in world units (Unreal units are about 2 cm, Bevy's default 0.02 is far too small; tuned by eye, unverified);
/// the normal bias a little over Bevy's 1.8, against acne on actors (unverified).
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

/// In Unreal units: rays of 48 units (Bevy's 0.3 m is about 15), depth texels 16 units thick (tuned by eye, unverified).
fn sun_contact_shadows() -> ContactShadows {
    ContactShadows { linear_steps: 16, thickness: 16.0, length: 48.0 }
}

fn sun_place(direction: Vec3) -> Transform {
    let up = if direction.normalize_or_zero().y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    Transform::IDENTITY.looking_to(direction, up)
}

/// A map without a sun still gets contact shadows: the key light's side stands for it, and its black light adds nothing.
const NO_SUN_DIRECTION: Vec3 = Vec3::new(-0.458088, -0.814379, -0.356291);

fn sun_cascades() -> CascadeShadowConfig {
    CascadeShadowConfigBuilder {
        num_cascades: WORLD_CASCADES,
        minimum_distance: 1.0,
        maximum_distance: 4000.0,
        first_cascade_far_bound: 600.0,
        overlap_proportion: 0.2,
    }
    .build()
}

const WORLD_CASCADES: usize = 4;

fn sun_shadows(
    settings: Res<Settings>,
    shown: Option<Res<ShownScene>>,
    mut commands: Commands,
    mut suns: Query<(Entity, &mut DirectionalLight, &mut Transform), With<SunShadow>>,
    mut logged: Local<Option<[f32; 3]>>,
) {
    let (shadows, contact) = (settings.modern_effect(MODERN_SHADOWS), settings.modern_effect(MODERN_CONTACT_SHADOWS));
    let environment = shown.as_deref().and_then(|shown| shown.lighting.as_deref()).filter(|_| shadows || contact);
    let Some(environment) = environment else {
        for (entity, ..) in &suns {
            commands.entity(entity).despawn();
        }
        *logged = None;
        return;
    };
    let sun = environment.sun.as_ref();
    let color = sun.map_or([0.0; 3], |(light, _)| light.color(environment.level_brightness));
    let place = sun_place(sun.map_or(NO_SUN_DIRECTION, |(_, hardware)| Vec3::from(p2_bevy_assets::to_bevy(hardware.direction))));
    let wanted = sun_light(color, shadows && sun.is_some(), contact);
    let mut found = false;
    for (entity, mut current, mut transform) in &mut suns {
        if std::mem::replace(&mut found, true) {
            commands.entity(entity).despawn();
            continue;
        }
        if current.color != wanted.color || current.contact_shadows_enabled != wanted.contact_shadows_enabled || current.shadow_maps_enabled != wanted.shadow_maps_enabled {
            *current = wanted;
        }
        transform.set_if_neq(place);
    }
    if !found {
        let layers = RenderLayers::from_layers(&[SUN_VIEW_LAYER, CASTER_LAYER]);
        commands.spawn((SunShadow, wanted, place, sun_cascades(), layers));
    }
    let Some((light, _)) = sun else { return };
    let key = <[f32; 3]>::from(light.location);
    if *logged != Some(key) {
        let suns = environment.lights.values().filter(|light| light.effect == LE_SUNLIGHT).count();
        let [r, g, b] = color;
        info!("modern sun shadows: {suns} sun lights in the map, the brightest at ({}, {}, {}) colour ({r:.2}, {g:.2}, {b:.2})", key[0], key[1], key[2]);
        *logged = Some(key);
    }
}

type Dynamic = Or<(With<FollowActor>, With<FollowBone>)>;

fn sun_casters(
    settings: Res<Settings>,
    mut on_before: Local<bool>,
    mut commands: Commands,
    all: Query<Entity, (With<Mesh3d>, Dynamic)>,
    changed: Query<
        Entity,
        (
            With<Mesh3d>,
            Dynamic,
            Or<(Added<Mesh3d>, Changed<RenderLayers>, Added<CanvasDrawn>, Changed<MeshMaterial3d<MeshLitMaterial>>, Changed<MeshMaterial3d<ActorLitMaterial>>, Changed<MeshMaterial3d<StandardMaterial>>)>,
        ),
    >,
) {
    let on = settings.modern_effect(MODERN_SHADOWS);
    let flipped = std::mem::replace(&mut *on_before, on) != on;
    if !on && !flipped {
        return;
    }
    let mut retag = |entity: Entity| {
        commands.entity(entity).queue_silenced(move |mut entity: EntityWorldMut| {
            let current = entity.get::<RenderLayers>().cloned().unwrap_or_default();
            let wanted = caster_layers(&current, on && !entity.contains::<CanvasDrawn>() && !is_see_through(&entity));
            if wanted != current {
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

/// Glass, fading and additive surfaces let the sun through; an opaque shadow from them cuts a hard shape out of a soft
/// baked sun patch. Masked ones keep casting, with their holes.
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

fn caster_layers(layers: &RenderLayers, casts: bool) -> RenderLayers {
    let world = layers.intersects(&RenderLayers::layer(0)) && !layers.intersects(&RenderLayers::from_layers(&OVERLAY_LAYERS));
    if casts && world { layers.clone().with(CASTER_LAYER) } else { layers.clone().without(CASTER_LAYER) }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::message::Messages;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::ecs::world::World;
    use bevy::pbr::StandardMaterial;

    use super::*;
    use crate::render::blended::TerrainLayer;
    use crate::display::MODERN_GRAPHICS;
    use crate::render::terrain_material::TerrainLayers;

    #[test]
    fn every_material_kind_follows_the_setting() {
        let mut world = World::new();
        let base = StandardMaterial::default;
        let mut lightmapped = Assets::<LightmappedMaterial>::default();
        let bsp = lightmapped.add(LightmappedMaterial { base: base(), extension: BakedLight::default() });
        let mut meshes = Assets::<MeshLitMaterial>::default();
        let mesh = meshes.add(MeshLitMaterial { base: base(), extension: DynamicLit::default() });
        let mut actors = Assets::<ActorLitMaterial>::default();
        let actor = actors.add(ActorLitMaterial { base: base(), extension: HardwareLighting::default() });
        let mut terrain = Assets::<TerrainMaterial>::default();
        let ground = terrain.add(TerrainMaterial { base: base(), extension: TerrainLayers::default() });
        let mut terrain_layers = Assets::<TerrainLayerMaterial>::default();
        let layer = terrain_layers.add(TerrainLayerMaterial { base: base(), extension: TerrainLayer::default() });
        world.insert_resource(lightmapped);
        world.insert_resource(meshes);
        world.insert_resource(actors);
        world.insert_resource(terrain);
        world.insert_resource(terrain_layers);
        world.init_resource::<Messages<AssetEvent<LightmappedMaterial>>>();
        world.init_resource::<Messages<AssetEvent<MeshLitMaterial>>>();
        world.init_resource::<Messages<AssetEvent<ActorLitMaterial>>>();
        world.init_resource::<Messages<AssetEvent<TerrainMaterial>>>();
        world.init_resource::<Messages<AssetEvent<TerrainLayerMaterial>>>();
        let mut flags = |on: bool| {
            let value = if on { "True" } else { "False" };
            world.insert_resource(Settings::read(|query| if query == MODERN_GRAPHICS { value.to_owned() } else { String::new() }));
            world.run_system_once(follow_flag::<LightmappedMaterial>).unwrap();
            world.run_system_once(follow_flag::<MeshLitMaterial>).unwrap();
            world.run_system_once(follow_flag::<ActorLitMaterial>).unwrap();
            world.run_system_once(follow_flag::<TerrainMaterial>).unwrap();
            world.run_system_once(follow_flag::<TerrainLayerMaterial>).unwrap();
            [
                world.resource::<Assets<LightmappedMaterial>>().get(&bsp).unwrap().extension.layers.detail.z > 0.5,
                world.resource::<Assets<MeshLitMaterial>>().get(&mesh).unwrap().extension.layers.detail.z > 0.5,
                world.resource::<Assets<ActorLitMaterial>>().get(&actor).unwrap().extension.layers.detail.z > 0.5,
                world.resource::<Assets<TerrainMaterial>>().get(&ground).unwrap().extension.params.count.y == 1,
                world.resource::<Assets<TerrainLayerMaterial>>().get(&layer).unwrap().extension.layers.detail.z > 0.5,
            ]
        };
        assert_eq!(flags(true), [true; 5]);
        assert_eq!(flags(false), [false; 5]);
        assert_eq!(flags(true), [true; 5]);
    }

    #[test]
    fn every_material_gets_the_flag_once() {
        let mut world = World::new();
        let mut materials = Assets::<LightmappedMaterial>::default();
        let ids: Vec<_> = (0..3).map(|_| materials.add(LightmappedMaterial { base: StandardMaterial::default(), extension: BakedLight::default() })).collect();
        world.insert_resource(materials);
        world.init_resource::<Messages<AssetEvent<LightmappedMaterial>>>();
        world.insert_resource(Settings::read(|query| if query == MODERN_GRAPHICS { "True".to_owned() } else { String::new() }));
        world.run_system_once(follow_flag::<LightmappedMaterial>).unwrap();
        let bits = modern_flag(world.resource::<Settings>());
        assert!(bits & MODERN_ON != 0);
        let materials = world.resource::<Assets<LightmappedMaterial>>();
        assert!(ids.iter().all(|id| materials.get(id).is_some_and(|material| material.flag() == bits)));
    }

    #[test]
    fn the_modern_flag_holds_each_effect_and_round_trips() {
        let settings = |master: &str, relief: &str, specular: &str| {
            Settings::read(|query| match query {
                MODERN_GRAPHICS => master.to_owned(),
                MODERN_RELIEF => relief.to_owned(),
                MODERN_SPECULAR => specular.to_owned(),
                _ => String::new(),
            })
        };
        assert_eq!(modern_flag(&settings("False", "False", "False")), 0);
        assert_eq!(modern_flag(&settings("True", "", "")), MODERN_ON);
        assert_eq!(modern_flag(&settings("True", "False", "True")), MODERN_ON | NO_RELIEF);
        assert_eq!(modern_flag(&settings("True", "True", "False")), MODERN_ON | NO_GLOSS);
        assert_eq!(modern_flag(&settings("True", "False", "False")), MODERN_ON | NO_RELIEF | NO_GLOSS);
        for bits in 0..8u8 {
            let flag = f32::from(bits);
            assert_eq!(u32::from(bits), (flag.max(0.0) + 0.5) as u32);
        }
    }

    fn light(effect: p2_render_cpu::LightEffect, brightness: f32, x: f32) -> MeshLight {
        MeshLight { light_type: p2_render_cpu::LightType(1), effect, brightness, location: Vector::from([x, 0.0, 0.0]), ..MeshLight::default() }
    }

    #[test]
    fn the_brightest_sun_is_picked_and_none_without_sun() {
        let lights = [light(p2_render_cpu::LightEffect(0), 255.0, 1.0), light(LE_SUNLIGHT, 64.0, 2.0), light(LE_SUNLIGHT, 200.0, 3.0), light(LE_SUNLIGHT, 100.0, 4.0)];
        let (picked, sun) = brightest_sun(&lights, 1.0).expect("a sun");
        assert_eq!(<[f32; 3]>::from(picked.location), [3.0, 0.0, 0.0]);
        assert_eq!(sun.kind, p2_render_cpu::HardwareKind::Directional);
        let baked = picked.color(1.0);
        assert!((sun.color[0] - 0.875 * baked[0]).abs() < 1e-6);
        assert!(brightest_sun(&lights[..1], 1.0).is_none());
        assert!(brightest_sun(&[] as &[MeshLight], 1.0).is_none());
    }

    #[test]
    fn casters_carry_the_caster_layer_only_in_the_world() {
        let world = RenderLayers::layer(0);
        let tagged = RenderLayers::from_layers(&[0, CASTER_LAYER]);
        assert!(caster_layers(&world, true) == tagged);
        assert!(caster_layers(&tagged, true) == tagged);
        assert!(caster_layers(&tagged, false) == world);
        let overlay = RenderLayers::layer(OVERLAY_LAYERS[0]);
        assert!(caster_layers(&overlay, true) == overlay);
        let sky = RenderLayers::layer(crate::components::SKY_LAYER);
        assert!(caster_layers(&sky, true) == sky);
        let view = RenderLayers::from_layers(&[0, SUN_VIEW_LAYER]);
        assert!(!view.intersects(&RenderLayers::layer(CASTER_LAYER)));
        assert!(!RenderLayers::from_layers(&OVERLAY_LAYERS).intersects(&RenderLayers::from_layers(&[SUN_VIEW_LAYER, CASTER_LAYER])));
    }

    #[test]
    fn see_through_surfaces_do_not_cast() {
        for mode in [AlphaMode::Blend, AlphaMode::Premultiplied, AlphaMode::Add, AlphaMode::Multiply, AlphaMode::Mask(2.0)] {
            assert!(see_through_mode(mode));
        }
        for mode in [AlphaMode::Opaque, AlphaMode::Mask(0.5)] {
            assert!(!see_through_mode(mode));
        }
    }

    #[test]
    fn only_dynamic_world_meshes_cast() {
        let settings = |on: &'static str| Settings::read(move |query| if query == MODERN_GRAPHICS { on.to_owned() } else { String::new() });
        let mut world = World::new();
        world.insert_resource(settings("True"));
        let follow = FollowActor { actor: p2_vm::ObjectId::from_index(1), scale: [1.0; 3].into(), pre_pivot: [0.0; 3].into(), skeletal: None };
        let floor = world.spawn(Mesh3d::default()).id();
        let prop = world.spawn((Mesh3d::default(), follow)).id();
        let weapon = world.spawn((Mesh3d::default(), follow, RenderLayers::layer(OVERLAY_LAYERS[0]))).id();
        let hud = world.spawn((Mesh3d::default(), follow, CanvasDrawn(0))).id();
        let system = world.register_system(sun_casters);
        world.run_system(system).unwrap();
        let casts = |world: &World, entity: Entity| world.get::<RenderLayers>(entity).is_some_and(|layers| layers.intersects(&RenderLayers::layer(CASTER_LAYER)));
        assert!(casts(&world, prop));
        assert!(!casts(&world, floor) && !casts(&world, weapon) && !casts(&world, hud));
        world.insert_resource(settings("False"));
        world.run_system(system).unwrap();
        assert!(!casts(&world, prop));
    }
}
