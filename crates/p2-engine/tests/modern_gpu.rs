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
use p2_engine::render::actor_light::{ActorLitMaterial, HardwareLightUniform, HardwareLighting, SkinnedLighting};
use p2_engine::render::mesh_light::{DynamicLit, MeshLitMaterial};
use p2_engine::render::modern::{CASTER_LAYER, SUN_VIEW_LAYER};

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
    app.add_plugins((p2_engine::render::mesh_light::MeshLightPlugin, p2_engine::render::actor_light::ActorLightPlugin));
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
        overlap_proportion: 0.2,
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
fn a_static_vertex_lit_ground_keeps_its_bake_under_a_dynamic_caster() {
    let (under, beside) = ground_with_flag(7.0, Ground::StaticMesh);
    assert!(beside > 0, "the ground did not draw");
    assert!(under.abs_diff(beside) <= 2, "a realtime shadow on a static vertex-lit mesh: under {under}, beside {beside}");
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
fn modern_keeps_the_retail_brightness_outside_shadow() {
    let (_, modern) = ground_with_flag(5.0, Ground::StaticMesh);
    let (_, retail) = ground_under_and_beside(false, Ground::StaticMesh);
    assert!(modern.abs_diff(retail) <= 8, "modern shifts the lit colour: {modern} against retail {retail}");
}

#[test]
#[ignore = "needs a GPU"]
fn a_static_roof_does_not_darken_the_floor_under_it() {
    let (under, _) = ground_under_and_beside(true, Ground::Indoor);
    let (_, retail) = ground_under_and_beside(false, Ground::Indoor);
    assert!(under.abs_diff(retail) <= 2, "the structure's shadow changed baked light: under {under}, retail {retail}");
}

#[test]
#[ignore = "needs a GPU"]
fn the_sun_highlight_shows_on_glossy_ground() {
    let (_, glossy) = ground_with_flag(1.0, Ground::StaticMesh);
    let (_, matte) = ground_with_flag(5.0, Ground::StaticMesh);
    assert!(glossy >= matte + 8, "no visible highlight: gloss {glossy}, no gloss {matte}");
}
