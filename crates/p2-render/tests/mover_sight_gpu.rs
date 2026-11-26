mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::App;
use bevy::asset::Assets;
use bevy::camera::visibility::{NoCpuCulling, ViewVisibility};
use bevy::camera::{Camera, Camera3d, RenderTarget};
use bevy::color::Color;
use bevy::ecs::entity::Entity;
use bevy::ecs::observer::On;
use bevy::image::Image;
use bevy::math::Quat;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use p2_math::Vector;
use p2_render::world::components::FollowActor;
use p2_render::world::view::{MoverSightPlugin, MoversSeen};

const SIZE: u32 = 64;

fn pixels(frame: &Mutex<Vec<u8>>, wanted: fn([u8; 4]) -> bool) -> usize {
    frame.lock().expect("lock").as_chunks::<4>().0.iter().filter(|&&pixel| wanted(pixel)).count()
}

fn is_red([r, g, ..]: [u8; 4]) -> bool {
    u16::from(r) > u16::from(g) + 60
}

fn is_green([r, g, ..]: [u8; 4]) -> bool {
    u16::from(g) > u16::from(r) + 60
}

fn mover(app: &mut App, z: f32, color: Color) -> Entity {
    let world = app.world_mut();
    let mesh = world.resource_mut::<Assets<Mesh>>().add(common::quad(5.0, z, [1.0; 4]));
    let material = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial { base_color: color, unlit: true, double_sided: true, cull_mode: None, ..Default::default() });
    let follow = FollowActor { actor: p2_vm::ObjectId::from_index(1), scale: Vector::new(1.0, 1.0, 1.0), pre_pivot: Vector::ZERO, skeletal: None };
    world.spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::default(), follow)).id()
}

fn run_until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while !done(app) {
        app.update();
        assert!(start.elapsed() < Duration::from_secs(60), "{what}: compiling {}", common::compiling(app));
    }
}

#[test]
#[ignore = "needs a GPU"]
fn a_mover_is_drawn_and_listed_only_while_the_camera_holds_it() {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins(MoverSightPlugin);
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let mut target = Image::new_target_texture(SIZE, SIZE, TextureFormat::Rgba8UnormSrgb, None);
    target.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = world.resource_mut::<Assets<Image>>().add(target);
    let frame = Arc::new(Mutex::new(Vec::new()));
    let store = frame.clone();
    world.spawn(Readback::texture(target.clone())).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").clone_from(&event.data);
    });
    let eye = world
        .spawn((Camera3d::default(), Camera::default(), RenderTarget::Image(target.into()), Tonemapping::None, DebandDither::Disabled, Msaa::Off, Transform::default()))
        .id();
    let ahead = mover(&mut app, -20.0, Color::srgb(1.0, 0.0, 0.0));
    let behind = mover(&mut app, 20.0, Color::srgb(0.0, 1.0, 0.0));
    let tagged = |app: &App, entity| app.world().get::<NoCpuCulling>(entity).is_some();
    let shown = |app: &App, entity| app.world().get::<ViewVisibility>(entity).is_some_and(|shown| shown.get());
    let listed = |app: &App| app.world().resource::<MoversSeen>().drawn.clone();

    let seen = frame.clone();
    run_until(&mut app, "the mover ahead was never drawn", move |_| pixels(&seen, is_red) > 0);
    assert!(tagged(&app, ahead) && tagged(&app, behind), "the movers were drawn by Bevy's own culling, not by ours");
    assert_eq!(listed(&app), vec![ahead]);
    assert!(shown(&app, ahead) && !shown(&app, behind));
    assert_eq!(pixels(&frame, is_green), 0, "the mover behind the eye is drawn");

    app.world_mut().entity_mut(eye).insert(Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)));
    let seen = frame;
    run_until(&mut app, "the mover behind was never drawn after the eye turned", move |_| pixels(&seen, is_green) > 0 && pixels(&seen, is_red) == 0);
    assert_eq!(listed(&app), vec![behind]);
    assert!(shown(&app, behind) && !shown(&app, ahead));
}
