mod common;

use std::sync::{Arc, Mutex};

use bevy::app::App;
use bevy::asset::{Assets, RenderAssetUsages};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Camera, Camera3d, RenderTarget};
use bevy::color::{Color, LinearRgba};
use bevy::ecs::observer::On;
use bevy::image::Image;
use bevy::light::{CascadeShadowConfigBuilder, DirectionalLight};
use bevy::math::Vec3;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use bevy::math::Vec4;
use p2_render::actor_light::{ActorLitMaterial, HardwareLightUniform, HardwareLighting, SkinnedLighting};
use p2_render::mesh_light::{DynamicLit, MeshLitMaterial};
use bevy::light::AmbientLight;
use p2_render::view::{CASTER_LAYER, MOVER_LAYER, STATIC_CASTER_LAYER, SUN_VIEW_LAYER, SceneView};
use p2_render::sun_cache::{CachedSunLevel, SunCachePlugin, SunCacheStale, level_config};

const SUN_LESS: f32 = 0.3;
const SUN_TERM: f32 = 0.2;

const SIZE: u32 = 64;

const CLEAR: u8 = 43;

fn quad(half: f32, z: f32, light: f32) -> Mesh {
    common::quad(half, z, [light, light, light, 1.0])
}

#[derive(Clone, Copy, PartialEq)]
enum Ground {
    StaticMesh,
    Indoor,
    Actor,
}

fn ground_under_and_beside(modern: bool, ground_kind: Ground) -> (u8, u8) {
    ground_with_flag(if modern { 1.0 } else { 0.0 }, ground_kind)
}

fn ground_with_flag(flag: f32, ground_kind: Ground) -> (u8, u8) {
    let actor = ground_kind == Ground::Actor;
    let seen = RenderLayers::from_layers(&[0, SUN_VIEW_LAYER]);
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((p2_render::mesh_light::MeshLightPlugin, p2_render::actor_light::ActorLightPlugin));
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let mut target = Image::new_target_texture(SIZE, SIZE, TextureFormat::Rgba8UnormSrgb, None);
    target.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = world.resource_mut::<Assets<Image>>().add(target);
    let frames = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let store = frames.clone();
    world.spawn(Readback::texture(target.clone())).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").push(event.data.clone());
    });
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::from_xyz(0.0, 0.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y),
        seen.clone(),
        AmbientLight { brightness: 0.0, ..AmbientLight::default() },
    ));
    let baked = if ground_kind == Ground::Indoor { SUN_LESS } else { SUN_LESS + SUN_TERM };
    let ground = world.resource_mut::<Assets<Mesh>>().add(quad(30.0, 0.0, (2.0 * baked).powf(2.2)));
    let mut extension = DynamicLit::default();
    extension.layers.detail.z = flag;
    let texels: Vec<u8> = (0..16).flat_map(|i| { let v = 160 + (i % 3) as u8 * 8; [v, v, v, 255] }).collect();
    let texture = world.resource_mut::<Assets<Image>>().add(Image::new(
        Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
        TextureDimension::D2,
        texels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    ));
    let base = StandardMaterial { base_color_texture: Some(texture), ..StandardMaterial::default() };
    if actor {
        let mut lighting = SkinnedLighting { ambient: Vec3::splat(0.1), count: 1, ..Default::default() };
        lighting.lights[0] = HardwareLightUniform {
            position: Vec4::new(0.0, 0.0, 0.0, 1e6),
            direction: Vec3::new(0.0, 0.0, -1.0),
            kind: 2,
            color: Vec4::new(0.5, 0.5, 0.5, 0.0),
            attenuation: Vec4::new(1.0, 0.0, 0.0, 0.0),
        };
        let mut extension = HardwareLighting { lighting, ..Default::default() };
        extension.layers.detail.z = flag;
        let material = world.resource_mut::<Assets<ActorLitMaterial>>().add(ActorLitMaterial { base, extension });
        world.spawn((Mesh3d(ground), MeshMaterial3d(material), Transform::default(), seen));
    } else {
        let material = world.resource_mut::<Assets<MeshLitMaterial>>().add(MeshLitMaterial { base, extension });
        world.spawn((Mesh3d(ground), MeshMaterial3d(material), Transform::default()));
    }
    let caster = world.resource_mut::<Assets<Mesh>>().add(quad(3.0, 5.0, 1.0));
    let plain = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial::default());
    let caster_layer = if ground_kind == Ground::Indoor { 3 } else { CASTER_LAYER };
    world.spawn((Mesh3d(caster), MeshMaterial3d(plain), Transform::default(), RenderLayers::layer(caster_layer)));
    let sun = DirectionalLight {
        color: Color::LinearRgba(LinearRgba::rgb(SUN_TERM, SUN_TERM, SUN_TERM)),
        illuminance: 1.0,
        shadow_maps_enabled: true,
        affects_lightmapped_mesh_diffuse: false,
        shadow_depth_bias: 0.02,
        ..DirectionalLight::default()
    };
    let cascades = |count: usize| CascadeShadowConfigBuilder {
        num_cascades: count,
        minimum_distance: 0.1,
        maximum_distance: 60.0,
        first_cascade_far_bound: 60.0 / count as f32,
        // The overlap the modern shaders know the sun by.
        overlap_proportion: 0.1,
    };
    let down = Transform::default().looking_to(Vec3::NEG_Z, Vec3::Y);
    world.spawn((sun, cascades(4).build(), down, RenderLayers::from_layers(&[SUN_VIEW_LAYER, CASTER_LAYER])));
    // Pipelines compile over the first frames (in the background, up to a minute): until the ground shows (not the clear colour,
    // Bevy's default grey: 43; nor the empty first read backs: 0), then a few more for the shadow map.
    let red = |frame: &[u8], x: u32, y: u32| frame[((y * SIZE + x) * 4) as usize];
    let mut shown = None;
    let start = std::time::Instant::now();
    for frame in 0.. {
        if start.elapsed() > std::time::Duration::from_secs(60) {
            break;
        }
        app.update();
        let frames = frames.lock().expect("lock");
        if shown.is_none() && frames.last().is_some_and(|last| ![0, CLEAR].contains(&red(last, 4, 4))) {
            shown = Some(frame);
        }
        if shown.is_some_and(|shown| frame > shown + 30) {
            break;
        }
    }
    let frames = frames.lock().expect("lock");
    let last = frames.last().expect("no frame read back");
    let pixel = |x: u32, y: u32| &last[((y * SIZE + x) * 4) as usize..((y * SIZE + x) * 4 + 4) as usize];
    assert!(shown.is_some(), "the ground never drew: pixel (4, 4) {:?}, centre {:?}", pixel(4, 4), pixel(SIZE / 2, SIZE / 2));
    (red(last, SIZE / 2, SIZE / 2), red(last, 4, 4))
}

#[test]
#[ignore = "needs a GPU"]
fn a_static_mesh_takes_the_suns_shadow_under_modern() {
    let (under, beside) = ground_with_flag(7.0, Ground::StaticMesh);
    assert!(beside > 0, "the ground did not draw");
    assert!(under + 10 < beside, "no shadow on a static mesh: under {under}, beside {beside}");
}

#[test]
#[ignore = "needs a GPU"]
fn retail_path_has_no_shadow() {
    let (under, beside) = ground_under_and_beside(false, Ground::StaticMesh);
    assert!(beside > 0, "the ground did not draw");
    assert!(under.abs_diff(beside) <= 2, "a shadow with the modern graphics off: under {under}, beside {beside}");
}

#[test]
#[ignore = "needs a GPU"]
fn an_actor_receives_a_dynamic_casters_shadow() {
    let (under, beside) = ground_under_and_beside(true, Ground::Actor);
    assert!(beside > 0, "the model did not draw");
    assert!(under + 10 < beside, "no shadow on the actor: under {under}, beside {beside}");
}

#[test]
#[ignore = "needs a GPU"]
fn modern_sunlit_ground_is_neither_black_nor_blown_out() {
    let (under, beside) = ground_with_flag(5.0, Ground::StaticMesh);
    assert!((20..250).contains(&beside), "sunlit ground out of range: {beside}");
    assert!(under < beside, "the shadow is not darker than the sunlit ground: under {under}, beside {beside}");
}

#[test]
#[ignore = "needs a GPU"]
fn a_mesh_off_the_caster_layer_casts_no_shadow() {
    let (under, beside) = ground_under_and_beside(true, Ground::Indoor);
    assert!(under.abs_diff(beside) <= 2, "a shadow from a mesh that does not cast: under {under}, beside {beside}");
}

#[test]
#[ignore = "needs a GPU"]
fn gloss_adds_light_and_never_takes_it() {
    let (_, glossy) = ground_with_flag(1.0, Ground::StaticMesh);
    let (_, matte) = ground_with_flag(5.0, Ground::StaticMesh);
    assert!(glossy >= matte, "gloss darkened the ground: gloss {glossy}, no gloss {matte}");
}

#[test]
#[ignore = "needs a GPU"]
fn a_cached_sun_level_keeps_its_shadow_until_it_redraws() {
    let seen = RenderLayers::from_layers(&[0, SUN_VIEW_LAYER]);
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((p2_render::mesh_light::MeshLightPlugin, p2_render::actor_light::ActorLightPlugin, SunCachePlugin));
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let mut target = Image::new_target_texture(SIZE, SIZE, TextureFormat::Rgba8UnormSrgb, None);
    target.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = world.resource_mut::<Assets<Image>>().add(target);
    let frames = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let store = frames.clone();
    world.spawn(Readback::texture(target.clone())).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").push(event.data.clone());
    });
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::from_xyz(0.0, 0.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y),
        seen,
        SceneView::default(),
        AmbientLight { brightness: 0.0, ..AmbientLight::default() },
    ));
    let ground = world.resource_mut::<Assets<Mesh>>().add(quad(30.0, 0.0, 1.0));
    let mut extension = DynamicLit::default();
    extension.layers.detail.z = 7.0;
    let material = world.resource_mut::<Assets<MeshLitMaterial>>().add(MeshLitMaterial { base: StandardMaterial::default(), extension });
    world.spawn((Mesh3d(ground), MeshMaterial3d(material), Transform::default()));
    let caster = world.resource_mut::<Assets<Mesh>>().add(quad(3.0, 5.0, 1.0));
    let plain = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial::default());
    let caster = world.spawn((Mesh3d(caster), MeshMaterial3d(plain), Transform::default(), RenderLayers::layer(STATIC_CASTER_LAYER))).id();
    let down = Transform::default().looking_to(Vec3::NEG_Z, Vec3::Y);
    let sun = DirectionalLight {
        color: Color::LinearRgba(LinearRgba::rgb(0.5, 0.5, 0.5)),
        illuminance: 1.0,
        shadow_maps_enabled: true,
        affects_lightmapped_mesh_diffuse: false,
        ..DirectionalLight::default()
    };
    let config = CascadeShadowConfigBuilder { num_cascades: 3, minimum_distance: 0.1, maximum_distance: 60.0, first_cascade_far_bound: 15.0, overlap_proportion: 0.1 };
    world.spawn((sun, config.build(), down, RenderLayers::from_layers(&[SUN_VIEW_LAYER, MOVER_LAYER])));
    let black = DirectionalLight { color: Color::BLACK, illuminance: 0.0, shadow_maps_enabled: true, affects_lightmapped_mesh_diffuse: false, ..DirectionalLight::default() };
    world.spawn((CachedSunLevel::new(512.0), black, level_config(512.0), down, RenderLayers::layer(SUN_VIEW_LAYER)));

    let red = |frame: &[u8], x: u32, y: u32| frame[((y * SIZE + x) * 4) as usize];
    let read = |frames: &Arc<Mutex<Vec<Vec<u8>>>>| frames.lock().expect("lock").last().map(|last| (red(last, SIZE / 2, SIZE / 2), red(last, 4, 4)));
    let start = std::time::Instant::now();
    let (mut under, mut beside) = (0, 0);
    while start.elapsed() < std::time::Duration::from_secs(60) {
        app.update();
        if let Some(seen) = read(&frames) {
            (under, beside) = seen;
            if beside != 0 && beside != CLEAR && under + 20 < beside {
                break;
            }
        }
    }
    assert!(under + 20 < beside, "the level never drew the caster: under {under}, beside {beside}");
    for _ in 0..20 {
        app.update();
    }
    let drawn = read(&frames).expect("no frame read back");
    assert!(drawn.0 + 20 < drawn.1, "the shadow did not stay: {drawn:?}");

    app.world_mut().despawn(caster);
    for _ in 0..20 {
        app.update();
    }
    let kept = read(&frames).expect("no frame read back");
    assert!(kept.0 + 20 < kept.1, "the cached layer was lost: {kept:?}");

    app.world().resource::<SunCacheStale>().mark();
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(5) {
        app.update();
        if read(&frames).is_some_and(|seen| seen.0 + 5 >= seen.1) {
            break;
        }
    }
    for _ in 0..20 {
        app.update();
    }
    let redrawn = read(&frames).expect("no frame read back");
    assert!(redrawn.0 + 5 >= redrawn.1, "the redraw kept a caster that is gone: {redrawn:?}");
}
