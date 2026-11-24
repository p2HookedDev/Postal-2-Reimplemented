mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::App;
use bevy::asset::{Assets, Handle};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Camera, Camera3d, RenderTarget};
use bevy::color::{Color, LinearRgba};
use bevy::ecs::observer::On;
use bevy::image::Image;
use bevy::light::{AmbientLight, CascadeShadowConfigBuilder, DirectionalLight};
use bevy::math::{Quat, Vec3};
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use p2_assets::Model;
use p2_collision::{BspHulls, MeshSet};
use p2_engine::display::{MODERN_GRAPHICS, Settings};
use p2_engine::mesh_view::{CASTER_LAYER, LevelAlbedos, LevelCollision, OVERLAY_LAYERS, SUN_VIEW_LAYER, ShownScene, StaticCollision};
use p2_engine::render::actor_light::{ActorLightPlugin, ActorLitMaterial, HardwareLighting, LightEnvironment, SkinnedLighting, view_model_lighting};
use p2_engine::render::lamps::LampsPlugin;
use p2_engine::render::map_lights::MapLights;
use p2_engine::render::mesh_light::{DynamicLit, MeshLightPlugin, MeshLitMaterial};
use p2_math::{Rotator, Vector};
use p2_render_cpu::{LE_SUNLIGHT, MeshLight};
use rustc_hash::FxHashMap;

const SIZE: u32 = 32;
const CLEAR: u8 = 43;
const ZONE: f32 = 0.05;

fn light_at_the_eye() -> (SkinnedLighting, Arc<LightEnvironment>, StaticCollision) {
    let sun = MeshLight { effect: LE_SUNLIGHT, rotation: Rotator { pitch: -16384, yaw: 0, roll: 0 }, brightness: 40.0, ..MeshLight::default() };
    let lamp = MeshLight { location: Vector::new(0.0, 0.0, 10.0), radius: 0.0, brightness: 60.0, ..MeshLight::default() };
    let environment = Arc::new(LightEnvironment::new(FxHashMap::from_iter([(0, sun), (1, lamp)]), vec![[ZONE; 3]], 1.0));
    let level = StaticCollision {
        model: Model::default(),
        hulls: BspHulls::default(),
        terrains: Vec::new(),
        meshes: Vec::new(),
        mesh_set: MeshSet::default(),
        lamp_casters: Vec::new(),
        lamp_masked: Vec::new(),
        albedos: LevelAlbedos::default(),
    };
    (view_model_lighting(&environment, &MapLights::new(&environment), &level, Vec3::ZERO, Vector::ZERO), environment, level)
}

fn target(app: &mut App, frames: &Arc<Mutex<Vec<Vec<u8>>>>) -> Handle<Image> {
    let world = app.world_mut();
    let mut image = Image::new_target_texture(SIZE, SIZE, TextureFormat::Rgba8UnormSrgb, None);
    image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let image = world.resource_mut::<Assets<Image>>().add(image);
    let store = frames.clone();
    world.spawn(Readback::texture(image.clone())).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").push(event.data.clone());
    });
    image
}

fn centre(frames: &Arc<Mutex<Vec<Vec<u8>>>>) -> Option<u8> {
    frames.lock().expect("lock").last().map(|last| last[((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize])
}

fn world_and_overlay(sun_blocked: bool) -> (u8, u8) {
    world_and_weapon(sun_blocked, 1)
}

fn world_and_weapon(sun_blocked: bool, overlay_field: u32) -> (u8, u8) {
    let (mut lighting, environment, level) = light_at_the_eye();
    assert_eq!(lighting.count, 2, "the sun and the lamp were not picked");
    let sun = lighting.lights[0];
    if sun_blocked {
        lighting.lights[0].color.w = 0.0;
    }
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((MeshLightPlugin, ActorLightPlugin, LampsPlugin));
    app.insert_resource(Settings::read(|query| if query == MODERN_GRAPHICS { "True".to_owned() } else { String::new() }));
    let mut shown = ShownScene::default();
    shown.lighting = Some(environment);
    app.insert_resource(shown);
    app.insert_resource(LevelCollision(Some(Arc::new(level))));
    app.finish();
    app.cleanup();
    let (world_frames, overlay_frames) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
    let world_image = target(&mut app, &world_frames);
    let overlay_image = target(&mut app, &overlay_frames);
    let world = app.world_mut();
    let eye = Transform::from_xyz(0.0, 20.0, 0.0).looking_at(Vec3::ZERO, Vec3::Z);
    let facing_up = Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2));
    let camera = (Camera3d::default(), Camera::default(), Tonemapping::None, DebandDither::Disabled, Msaa::Off, eye);
    let ambient = AmbientLight { color: Color::LinearRgba(LinearRgba::new(ZONE, ZONE, ZONE, 0.0)), brightness: 1.0, affects_lightmapped_meshes: false };
    world.spawn((camera.clone(), RenderTarget::Image(world_image.into()), RenderLayers::from_layers(&[0, SUN_VIEW_LAYER]), ambient));
    let overlay = RenderLayers::layer(OVERLAY_LAYERS[0]);
    world.spawn((camera, RenderTarget::Image(overlay_image.into()), overlay.clone()));

    let quad = world.resource_mut::<Assets<Mesh>>().add(common::quad(30.0, 0.0, [1.0; 4]));
    let mut extension = DynamicLit::default();
    extension.layers.detail.z = 7.0;
    let ground = world.resource_mut::<Assets<MeshLitMaterial>>().add(MeshLitMaterial { base: StandardMaterial::default(), extension });
    world.spawn((Mesh3d(quad.clone()), MeshMaterial3d(ground), facing_up));
    let mut extension = HardwareLighting { lighting, overlay: overlay_field, ..HardwareLighting::default() };
    extension.layers.detail.z = 7.0;
    let actor = world.resource_mut::<Assets<ActorLitMaterial>>().add(ActorLitMaterial { base: StandardMaterial::default(), extension });
    world.spawn((Mesh3d(quad), MeshMaterial3d(actor), facing_up, overlay));

    let sun_light = DirectionalLight {
        color: Color::LinearRgba(LinearRgba::rgb(sun.color.x, sun.color.y, sun.color.z)),
        illuminance: 1.0,
        shadow_maps_enabled: true,
        affects_lightmapped_mesh_diffuse: false,
        shadow_depth_bias: 0.02,
        ..DirectionalLight::default()
    };
    let cascades = CascadeShadowConfigBuilder { num_cascades: 2, minimum_distance: 0.1, maximum_distance: 60.0, first_cascade_far_bound: 30.0, overlap_proportion: 0.1 };
    let along = Transform::default().looking_to(sun.direction, Vec3::Z);
    world.spawn((sun_light, cascades.build(), along, RenderLayers::from_layers(&[SUN_VIEW_LAYER, CASTER_LAYER])));
    if sun_blocked {
        let roof = world.resource_mut::<Assets<Mesh>>().add(common::quad(4.0, 0.0, [1.0; 4]));
        let plain = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial::default());
        world.spawn((Mesh3d(roof), MeshMaterial3d(plain), facing_up.with_translation(Vec3::new(0.0, 5.0, 0.0)), RenderLayers::layer(CASTER_LAYER)));
    }

    let start = Instant::now();
    let mut settled = None;
    for frame in 0.. {
        app.update();
        let shown = [centre(&world_frames), centre(&overlay_frames)];
        assert!(start.elapsed() < Duration::from_secs(60), "the quads never drew: {shown:?}, compiling {}", common::compiling(&app));
        if settled.is_none() && shown.iter().all(|seen| seen.is_some_and(|red| ![0, CLEAR].contains(&red))) && !common::compiling(&app) {
            settled = Some(frame);
        }
        if settled.is_some_and(|settled| frame > settled + 30) {
            break;
        }
    }
    let readings = (centre(&world_frames).expect("no world frame"), centre(&overlay_frames).expect("no overlay frame"));
    eprintln!("sun blocked {sun_blocked}: world {}, overlay {}", readings.0, readings.1);
    readings
}

#[test]
#[ignore = "needs a GPU"]
fn the_weapon_takes_the_light_a_world_surface_takes_in_the_open() {
    let (world, overlay) = world_and_overlay(false);
    assert!(world.abs_diff(overlay) <= 12, "world {world}, overlay {overlay}");
}

#[test]
#[ignore = "needs a GPU"]
fn the_weapon_loses_the_sun_where_the_world_does() {
    let (open, _) = world_and_overlay(false);
    let (world, overlay) = world_and_overlay(true);
    assert!(world + 20 < open, "the sun was not blocked in the world: {world}, open {open}");
    assert!(world.abs_diff(overlay) <= 12, "world {world}, overlay {overlay}");
}

#[test]
#[ignore = "needs a GPU"]
fn a_weapon_whose_material_misses_the_overlay_mark_takes_no_shade() {
    let (_, open) = world_and_weapon(false, 0);
    let (world, under_roof) = world_and_weapon(true, 0);
    assert!(open.abs_diff(under_roof) <= 3, "an unmarked weapon took the roof's shade: {open} in the open, {under_roof} under it");
    assert!(world.abs_diff(under_roof) > 12, "an unmarked weapon is lit as the world is: world {world}, weapon {under_roof}");
}
