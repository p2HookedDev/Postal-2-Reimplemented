mod common;

use std::f32::consts::FRAC_PI_2;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::App;
use bevy::asset::Assets;
use bevy::camera::{Camera, Camera3d, RenderTarget};
use bevy::color::Color;
use bevy::core_pipeline::core_3d::{main_opaque_pass_3d, main_transparent_pass_3d};
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::ecs::entity::Entity;
use bevy::ecs::observer::On;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::{AmbientLight, DirectionalLight};
use bevy::math::{Quat, Vec3};
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::RenderApp;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use p2_host::display::{MODERN_GRAPHICS, MODERN_LIQUIDS, Settings};
use p2_math::Vector;
use p2_render::MainPassExtraSystems;
use p2_render::frame_facts::FrameFacts;
use p2_render::liquids::LiquidsPlugin;
use p2_render::liquids::world::LiquidWorld;
use p2_render::particle_frame::{DrawnActor, DrawnEmitter, DrawnParticles, Liquid, LiquidRole, Pour};
use p2_particles::StripPoint;
use p2_render::view::SceneView;
use p2_vm::FluidType;

const SIZE: u32 = 128;

fn is_red([r, g, ..]: [u8; 4]) -> bool {
    u16::from(r) > u16::from(g) + 30
}

fn red_pixels(frame: &Mutex<Vec<u8>>) -> usize {
    frame.lock().expect("lock").as_chunks::<4>().0.iter().filter(|&&pixel| is_red(pixel)).count()
}

fn red_middle_x(frame: &Mutex<Vec<u8>>) -> f32 {
    let frame = frame.lock().expect("lock");
    let xs: Vec<f32> = frame.as_chunks::<4>().0.iter().enumerate().filter(|&(_, &pixel)| is_red(pixel)).map(|(i, _)| (i as u32 % SIZE) as f32).collect();
    xs.iter().sum::<f32>() / xs.len().max(1) as f32
}

/// A camera at `eye` drawing to an image with liquids at their top level; the app, the frame it last drew and the camera.
fn rendering(eye: Transform, game_step: f32) -> (App, Arc<Mutex<Vec<u8>>>, Entity) {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins(LiquidsPlugin);
    app.insert_resource(Settings::read(|query| match query {
        MODERN_GRAPHICS => "True".to_owned(),
        MODERN_LIQUIDS => "10".to_owned(),
        _ => String::new(),
    }));
    app.insert_resource(FrameFacts { in_play: true, game_step, ..FrameFacts::default() });
    app.insert_resource(DrawnParticles::default());
    app.sub_app_mut(RenderApp).configure_sets(
        Core3d,
        MainPassExtraSystems::Liquids.in_set(Core3dSystems::MainPass).after(main_opaque_pass_3d).before(main_transparent_pass_3d),
    );
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let mut target = Image::new_target_texture(SIZE, SIZE, TextureFormat::Rgba8UnormSrgb, None);
    target.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = world.resource_mut::<Assets<Image>>().add(target);
    let frames = Arc::new(Mutex::new(Vec::new()));
    let store = frames.clone();
    world.spawn(Readback::texture(target.clone())).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").clone_from(&event.data);
    });
    let camera = world
        .spawn((
            Camera3d::default(),
            Camera::default(),
            RenderTarget::Image(target.into()),
            Tonemapping::None,
            DebandDither::Disabled,
            Msaa::Off,
            DepthPrepass,
            AmbientLight::default(),
            eye,
            SceneView::default(),
        ))
        .id();
    (app, frames, camera)
}

fn until_red(app: &mut App, frames: &Mutex<Vec<u8>>) {
    let start = Instant::now();
    while red_pixels(frames) < 20 {
        app.update();
        assert!(start.elapsed() < Duration::from_secs(60), "no liquid was drawn: compiling {}", common::compiling(app));
    }
}

/// `actor` in front of a grey wall under white ambient light, until it draws red; the app and the frame it last drew.
fn showing(actor: impl FnOnce(Entity) -> DrawnActor) -> (App, Arc<Mutex<Vec<u8>>>) {
    let (mut app, frames, _) = rendering(Transform::default(), 0.02);
    let world = app.world_mut();
    let wall = world.resource_mut::<Assets<Mesh>>().add(common::quad(100.0, -60.0, [1.0; 4]));
    let grey = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial { base_color: Color::srgb(0.2, 0.2, 0.2), unlit: true, ..Default::default() });
    world.spawn((Mesh3d(wall), MeshMaterial3d(grey), Transform::default()));
    let parent = world.spawn_empty().id();
    world.resource_mut::<DrawnParticles>().actors.push(actor(parent));
    until_red(&mut app, &frames);
    (app, frames)
}

/// A puke pour falling straight down 40 units ahead of the eye, its strip from the nozzle to where it lands; puke, as
/// a blood stream breaks into drops before it lands.
fn pouring(parent: Entity) -> DrawnActor {
    let pour = Pour {
        from: Vector::new(0.0, -40.0, NOZZLE),
        velocity: [Vector::new(0.0, 0.0, -40.0), Vector::new(0.0, 0.0, -40.0)],
        acceleration: Vector::new(0.0, 0.0, -950.0),
        rate: 120.0,
        flowing: true,
    };
    let strip = (0..=8).map(|step| StripPoint { location: Vector::new(0.0, -40.0, NOZZLE - step as f32 * 3.0 * NOZZLE / 12.0), size: 4.0, color: [255; 4] });
    let emitter = DrawnEmitter { strip: strip.collect(), ..DrawnEmitter::default() };
    let liquid = Liquid { kind: FluidType::Puke, role: LiquidRole::Stream, burning: false, pour: Some(pour) };
    DrawnActor { parent, drawn: true, seen: true, place: Some(pour.from), liquid: Some(liquid), emitters: vec![emitter], ..DrawnActor::default() }
}

const NOZZLE: f32 = 12.0;

/// The rows of the middle column that draw liquid, from the top.
fn red_rows(frame: &Mutex<Vec<u8>>) -> Vec<usize> {
    let frame = frame.lock().expect("lock");
    let pixels = frame.as_chunks::<4>().0;
    (0..SIZE as usize).filter(|row| pixels.get(row * SIZE as usize + SIZE as usize / 2).is_some_and(|&pixel| is_red(pixel))).collect()
}

#[test]
#[ignore = "needs a GPU"]
fn a_pour_draws_one_unbroken_ribbon_from_its_nozzle_to_its_hit_and_nothing_once_it_stops() {
    let (mut app, frame) = showing(pouring);
    let rows = red_rows(&frame);
    let (first, last) = (rows.first().copied().unwrap_or_default(), rows.last().copied().unwrap_or_default());
    assert_eq!(rows.len(), last - first + 1, "the ribbon has gaps: {rows:?}");
    // Nozzle and hit are 12 units over and under the eye 40 units away; at a 45 degree view, 0.36 of the frame each way.
    let span = (SIZE as f32 * 0.36) as usize;
    assert!(first <= SIZE as usize / 2 - span + 3 && last + 3 >= SIZE as usize / 2 + span, "the ribbon runs over rows {first} to {last}");
    let liquid = app.world_mut().resource_mut::<DrawnParticles>().actors[0].liquid.as_mut().and_then(|liquid| liquid.pour.as_mut()).map(|pour| pour.flowing = false);
    assert!(liquid.is_some());
    let stopped = Instant::now();
    while red_pixels(&frame) > 0 {
        app.update();
        assert!(stopped.elapsed() < Duration::from_secs(30), "{} pixels of liquid are still drawn", red_pixels(&frame));
    }
}

#[test]
#[ignore = "needs a GPU"]
fn a_blood_pool_is_dark_red_under_white_light_stays_put_as_the_camera_turns_and_dries_away() {
    let eye = Transform::default().looking_at(Vec3::new(0.0, -20.0, -80.0), Vec3::Y);
    let (mut app, frames, camera) = rendering(eye, 0.1);
    let world = app.world_mut();
    let floor = world.resource_mut::<Assets<Mesh>>().add(common::quad(200.0, 0.0, [1.0; 4]));
    let grey = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial { base_color: Color::srgb(0.5, 0.5, 0.5), perceptual_roughness: 1.0, ..Default::default() });
    world.spawn((Mesh3d(floor), MeshMaterial3d(grey), Transform::from_xyz(0.0, -20.0, 0.0).with_rotation(Quat::from_rotation_x(-FRAC_PI_2))));
    world.spawn((DirectionalLight { color: Color::WHITE, illuminance: 3000.0, ..Default::default() }, Transform::default().looking_to(-Vec3::Y, Vec3::Z)));
    let pools = &mut world.resource_mut::<LiquidWorld>().into_inner().pools;
    for i in -1..=1 {
        for j in -1..=1 {
            pools.pour(Vector::new(i as f32 * 8.0, -80.0 + j as f32 * 8.0, -20.0), FluidType::Blood, 0.4 * 64.0, 0.0);
        }
    }
    until_red(&mut app, &frames);
    let before = red_middle_x(&frames);
    assert!(red_pixels(&frames) < (SIZE * SIZE / 4) as usize, "the pool covers {} pixels: more than its patch", red_pixels(&frames));
    app.world_mut().get_mut::<Transform>(camera).expect("camera").rotate_y(5f32.to_radians());
    for _ in 0..20 {
        app.update();
    }
    let moved = red_middle_x(&frames) - before;
    assert!((6.0..20.0).contains(&moved), "the pool moved {moved} pixels on screen as the camera turned 5 degrees");
    let start = Instant::now();
    while red_pixels(&frames) > 0 {
        app.update();
        assert!(start.elapsed() < Duration::from_secs(120), "{} pixels of the pool stay after its drying time", red_pixels(&frames));
    }
}
