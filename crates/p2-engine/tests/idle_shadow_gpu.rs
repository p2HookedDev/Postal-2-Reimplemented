mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bevy::app::App;
use bevy::asset::Assets;
use bevy::camera::{Camera3d, RenderTarget};
use bevy::core_pipeline::schedule::RootNonCameraView;
use bevy::ecs::prelude::{Query, Res, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::{NotShadowCaster, PointLight};
use bevy::math::Vec3;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::render::render_resource::TextureFormat;
use bevy::shape::Cuboid;
use p2_engine::render::sun_cache::SunCachePlugin;

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
}
