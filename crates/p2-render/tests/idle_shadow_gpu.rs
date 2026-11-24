mod common;

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};

use bevy::app::App;
use bevy::asset::Assets;
use bevy::camera::{Camera3d, RenderTarget};
use bevy::core_pipeline::schedule::RootNonCameraView;
use bevy::ecs::entity::Entity;
use bevy::ecs::observer::On;
use bevy::ecs::prelude::{Query, Res, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::{NotShadowCaster, PointLight};
use bevy::math::Vec3;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::view::Tonemapping;
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::shape::Cuboid;
use p2_render::sun_cache::SunCachePlugin;

const FRAMES: usize = 40;

#[derive(Resource, Clone, Default)]
struct ShadowPasses(Arc<AtomicUsize>);

fn count_passes(passes: Res<ShadowPasses>, faces: Query<(), With<RootNonCameraView>>) {
    passes.0.store(faces.iter().count(), Ordering::Relaxed);
}

fn shadow_passes(app: &mut App) -> usize {
    app.sub_app(RenderApp).world().resource::<ShadowPasses>().0.load(Ordering::Relaxed)
}

fn settle(app: &mut App) -> usize {
    for _ in 0..FRAMES {
        app.update();
    }
    while common::compiling(app) {
        app.update();
    }
    for _ in 0..FRAMES {
        app.update();
    }
    shadow_passes(app)
}

fn room(app: &mut App, skip_idle: bool) {
    common::headless_renderer(app);
    if skip_idle {
        app.add_plugins(SunCachePlugin);
    }
    app.sub_app_mut(RenderApp).init_resource::<ShadowPasses>().add_systems(Render, count_passes.in_set(RenderSystems::PrepareBindGroups));
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let target = world.resource_mut::<Assets<Image>>().add(Image::new_target_texture(32, 32, TextureFormat::Rgba8UnormSrgb, None));
    world.spawn((Camera3d::default(), RenderTarget::Image(target.into()), Transform::from_xyz(0.0, 0.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y)));
    let floor = world.resource_mut::<Assets<Mesh>>().add(common::quad(30.0, 0.0, [1.0; 4]));
    let plain = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial::default());
    world.spawn((Mesh3d(floor), MeshMaterial3d(plain), NotShadowCaster));
    world.spawn((PointLight { shadow_maps_enabled: true, range: 40.0, ..PointLight::default() }, Transform::from_xyz(0.0, 0.0, 10.0)));
}

fn add_mover(app: &mut App) {
    let world = app.world_mut();
    let cube = world.resource_mut::<Assets<Mesh>>().add(Mesh::from(Cuboid::new(1.0, 1.0, 1.0)));
    let plain = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial::default());
    world.spawn((Mesh3d(cube), MeshMaterial3d(plain), Transform::from_xyz(0.0, 0.0, 4.0)));
}

fn surround_lamp(app: &mut App) {
    let world = app.world_mut();
    let cube = world.resource_mut::<Assets<Mesh>>().add(Mesh::from(Cuboid::new(1.0, 1.0, 1.0)));
    let plain = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial::default());
    for offset in [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y, Vec3::Z, Vec3::NEG_Z] {
        world.spawn((Mesh3d(cube.clone()), MeshMaterial3d(plain.clone()), Transform::from_translation(Vec3::new(0.0, 0.0, 10.0) + offset * 5.0)));
    }
}

#[test]
#[ignore = "needs a GPU"]
fn a_lamp_cube_with_nothing_to_cast_runs_no_shadow_schedule() {
    let mut plain = App::new();
    room(&mut plain, false);
    assert_eq!(settle(&mut plain), 6, "Bevy alone does not run the six faces of an empty cube map");

    let mut app = App::new();
    room(&mut app, true);
    assert_eq!(settle(&mut app), 0, "empty cube faces still run");
    add_mover(&mut app);
    let with_mover = settle(&mut app);
    assert!((1..6).contains(&with_mover), "with a mover under the lamp: {with_mover} of 6 faces run");
    surround_lamp(&mut app);
    assert_eq!(settle(&mut app), 4, "faces that all hold movers do not take turns");
}

/// A row of the read back image is padded to 256 bytes, which 64 pixels fill exactly.
const SIDE: u32 = 64;

/// The red of the floor left and right of the middle, where the mover's shadow falls from either of its two places.
fn floor_left_and_right(frames: &Arc<Mutex<Vec<Vec<u8>>>>) -> (u32, u32) {
    let frames = frames.lock().expect("lock");
    let last = frames.last().expect("no frame read back");
    let red = |x: u32| u32::from(last[((SIDE / 2 * SIDE + x) * 4) as usize]);
    (red(10), red(54))
}

#[test]
#[ignore = "needs a GPU"]
fn a_movers_lamp_shadow_follows_it_while_the_faces_take_turns() {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins(SunCachePlugin);
    app.sub_app_mut(RenderApp).init_resource::<ShadowPasses>().add_systems(Render, count_passes.in_set(RenderSystems::PrepareBindGroups));
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let mut image = Image::new_target_texture(SIDE, SIDE, TextureFormat::Rgba8UnormSrgb, None);
    image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = world.resource_mut::<Assets<Image>>().add(image);
    let frames = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let store = frames.clone();
    world.spawn(Readback::texture(target.clone())).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").push(event.data.clone());
    });
    world.spawn((Camera3d::default(), Tonemapping::None, RenderTarget::Image(target.into()), Transform::from_xyz(0.0, 0.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y)));
    let floor = world.resource_mut::<Assets<Mesh>>().add(common::quad(30.0, 0.0, [1.0; 4]));
    let plain = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial::default());
    world.spawn((Mesh3d(floor), MeshMaterial3d(plain.clone()), NotShadowCaster));
    world.spawn((PointLight { shadow_maps_enabled: true, range: 40.0, intensity: 2_000_000.0, ..PointLight::default() }, Transform::from_xyz(0.0, 0.0, 10.0)));
    let cube = world.resource_mut::<Assets<Mesh>>().add(Mesh::from(Cuboid::new(1.5, 1.5, 1.5)));
    let mover: Entity = world.spawn((Mesh3d(cube), MeshMaterial3d(plain), Transform::from_xyz(-3.0, 0.0, 4.0))).id();
    surround_lamp(&mut app);

    assert_eq!(settle(&mut app), 4, "the faces are not taking turns");
    let (left, right) = floor_left_and_right(&frames);
    assert!(left + 40 < right, "no shadow under the mover's first place: left {left}, right {right}");
    app.world_mut().entity_mut(mover).insert(Transform::from_xyz(3.0, 0.0, 4.0));
    for _ in 0..12 {
        app.update();
    }
    let (left, right) = floor_left_and_right(&frames);
    assert!(right + 40 < left, "the shadow did not follow the mover: left {left}, right {right}");
}
