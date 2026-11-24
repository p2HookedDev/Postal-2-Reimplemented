mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::App;
use bevy::asset::{Assets, RenderAssetUsages};
use bevy::camera::{Camera, Camera3d, RenderTarget};
use bevy::color::Color;
use bevy::core_pipeline::core_3d::{main_opaque_pass_3d, main_transparent_pass_3d};
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::ecs::observer::On;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::math::Quat;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::RenderApp;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use p2_host::display::{MODERN_GPU_PARTICLES, MODERN_GRAPHICS, Settings};
use p2_collision::{BspHulls, MeshCollision, MeshSet};
use p2_math::{Rotator, Vector};
use p2_particles::{DrawStyle, EmitterProperties, SpriteParticle};
use p2_render::MainPassExtraSystems;
use p2_render::frame_facts::FrameFacts;
use p2_render::debris::DebrisPlugin;
use p2_render::gpu_particles::GpuParticlesPlugin;
use p2_render::level_collision::{LevelAlbedos, LevelCollision, StaticCollision};
use p2_render::particle_frame::{Blast, DrawnActor, DrawnEmitter, DrawnParticles, EmitterLook};
use p2_render::view::SceneView;

const SIZE: u32 = 128;

fn is_red([r, g, ..]: [u8; 4]) -> bool {
    u16::from(r) > u16::from(g) + 30
}

fn red_pixels(frame: &Mutex<Vec<u8>>) -> usize {
    frame.lock().expect("lock").as_chunks::<4>().0.iter().filter(|&&pixel| is_red(pixel)).count()
}

/// The mean column of the droplets' pixels.
fn red_middle(frame: &Mutex<Vec<u8>>) -> Option<f32> {
    let frame = frame.lock().expect("lock");
    let columns: Vec<f32> = frame.as_chunks::<4>().0.iter().enumerate().filter(|&(_, &pixel)| is_red(pixel)).map(|(at, _)| (at as u32 % SIZE) as f32).collect();
    (!columns.is_empty()).then(|| columns.iter().sum::<f32>() / columns.len() as f32)
}

/// The emitter's texture and the speed of its sprites to the right, in Unreal units a second.
#[derive(Clone, Copy)]
struct Source {
    texture: &'static str,
    sideways: f32,
}

const BLOOD: Source = Source { texture: "nathans.Skins.bloodchunks1", sideways: 0.0 };
const GLASS: Source = Source { texture: "nathans.Skins.glassshards", sideways: 60.0 };
const GAME_STEP: f32 = 0.02;

fn run_for(app: &mut App, seconds: f32) {
    let from = Instant::now();
    while from.elapsed() < Duration::from_secs_f32(seconds) {
        app.update();
    }
}

/// The floor the scene draws, as the level's collision: Unreal's z is up, 6 units under the eye.
fn floor_under() -> StaticCollision {
    let corner = |x: f32, y: f32| [x, y, -6.0];
    let floor = [[corner(-200.0, -200.0), corner(200.0, -200.0), corner(200.0, 200.0)], [corner(-200.0, -200.0), corner(200.0, 200.0), corner(-200.0, 200.0)]];
    let meshes = vec![MeshCollision::placed(floor, Vector::ZERO, Rotator::ZERO, Vector::new(1.0, 1.0, 1.0), Vector::ZERO)];
    StaticCollision {
        model: p2_assets::Model::default(),
        hulls: BspHulls::default(),
        terrains: Vec::new(),
        mesh_set: MeshSet::new(&meshes),
        meshes,
        lamp_casters: Vec::new(),
        lamp_masked: Vec::new(),
        albedos: LevelAlbedos::default(),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Under {
    Nothing,
    AFloor,
}

/// Throws droplets in front of a wall until some are drawn, then stops; the app and the frame it last drew.
fn thrown(under: Under, source: Source) -> (App, Arc<Mutex<Vec<u8>>>) {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((GpuParticlesPlugin, DebrisPlugin));
    app.insert_resource(Settings::read(|query| match query {
        MODERN_GRAPHICS => "True".to_owned(),
        MODERN_GPU_PARTICLES => "10".to_owned(),
        _ => String::new(),
    }));
    app.insert_resource(FrameFacts { in_play: true, game_step: GAME_STEP, ..FrameFacts::default() });
    app.insert_resource(DrawnParticles::default());
    if under == Under::AFloor {
        app.insert_resource(LevelCollision(Some(Arc::new(floor_under()))));
    }
    app.sub_app_mut(RenderApp).configure_sets(
        Core3d,
        MainPassExtraSystems::GpuParticles.in_set(Core3dSystems::MainPass).after(main_opaque_pass_3d).before(main_transparent_pass_3d),
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
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        DepthPrepass,
        Transform::default(),
        SceneView::default(),
    ));
    let wall = world.resource_mut::<Assets<Mesh>>().add(common::quad(100.0, -60.0, [1.0; 4]));
    let grey = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial { base_color: Color::srgb(0.2, 0.2, 0.2), unlit: true, ..Default::default() });
    world.spawn((Mesh3d(wall), MeshMaterial3d(grey.clone()), Transform::default()));
    if under == Under::AFloor {
        let floor = world.resource_mut::<Assets<Mesh>>().add(common::quad(100.0, 0.0, [1.0; 4]));
        let lying = Transform::from_xyz(0.0, -6.0, 0.0).with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2));
        world.spawn((Mesh3d(floor), MeshMaterial3d(grey), lying));
    }
    let parent = world.spawn_empty().id();
    let red = Image::new_fill(Extent3d { width: 2, height: 2, depth_or_array_layers: 1 }, TextureDimension::D2, &[255, 0, 0, 255], TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default());
    let image = world.resource_mut::<Assets<Image>>().add(red);
    let props = EmitterProperties { draw_style: DrawStyle::AlphaBlend, ..EmitterProperties::default() };
    let look = EmitterLook { parent, order: 0, image, props, name: source.texture.to_owned(), fogless: false };
    let mut drawn = world.resource_mut::<DrawnParticles>();
    drawn.born.push(look);
    drawn.actors.push(DrawnActor { parent, drawn: true, emitters: vec![DrawnEmitter::default()], ..DrawnActor::default() });
    let place = Vector::new(0.0, -40.0, 0.0);
    let sprite = SpriteParticle {
        location: place,
        previous: Vector::new(-source.sideways * GAME_STEP, -40.0, 0.0),
        size: [6.0, 6.0],
        up: Vector::new(0.0, 0.0, 1.0),
        right: Vector::new(1.0, 0.0, 0.0),
        color: [255; 4],
        uv: [0.0, 0.0, 1.0, 1.0],
        blend: None,
        serial: 0,
    };

    let start = Instant::now();
    while red_pixels(&frames) < 3 {
        let sprites = &mut app.world_mut().resource_mut::<DrawnParticles>().actors[0].emitters[0].sprites;
        let serial = sprites.len() as u32 + 1;
        sprites.push(SpriteParticle { serial, ..sprite });
        app.update();
        assert!(start.elapsed() < Duration::from_secs(60), "no droplet was drawn: compiling {}", common::compiling(&app));
    }
    (app, frames)
}

#[test]
#[ignore = "needs a GPU"]
fn blood_chunks_throw_droplets_of_their_texture_that_fall_out_of_sight() {
    let (mut app, frame) = thrown(Under::Nothing, BLOOD);
    let stopped = Instant::now();
    while red_pixels(&frame) > 0 {
        app.update();
        assert!(stopped.elapsed() < Duration::from_secs(20), "{} pixels of droplets are still drawn", red_pixels(&frame));
    }
}

#[test]
#[ignore = "needs a GPU"]
fn a_chunk_lies_on_the_floor_it_lands_on_until_its_life_ends() {
    let (mut app, frame) = thrown(Under::AFloor, BLOOD);
    let stopped = Instant::now();
    run_for(&mut app, 4.0);
    assert!(red_pixels(&frame) > 0, "no droplet lies on the floor four seconds after the last was thrown");
    while red_pixels(&frame) > 0 {
        app.update();
        assert!(stopped.elapsed() < Duration::from_secs(30), "{} pixels of droplets are still drawn after their lifetime", red_pixels(&frame));
    }
}

#[test]
#[ignore = "needs a GPU"]
fn a_rigid_shard_thrown_sideways_lands_on_the_level_and_lies_still_to_that_side() {
    let (mut app, frame) = thrown(Under::AFloor, GLASS);
    run_for(&mut app, 3.0);
    let lying = red_middle(&frame).expect("no shard lies on the floor three seconds after the last was thrown");
    assert!(lying > SIZE as f32 / 2.0 + 4.0, "the shards lie at column {lying}, not to the side they were thrown to");
    run_for(&mut app, 1.0);
    let later = red_middle(&frame).expect("the shards are gone a second later");
    assert!((later - lying).abs() < 0.5, "the shards still move a second later: column {lying} to {later}");
}

#[test]
#[ignore = "needs a GPU"]
fn a_push_from_the_left_throws_lying_droplets_to_the_right() {
    let (mut app, frame) = thrown(Under::AFloor, BLOOD);
    run_for(&mut app, 3.0);
    let lying = red_middle(&frame).expect("no droplet lies on the floor three seconds after the last was thrown");
    let push = Blast { at: Vector::new(-150.0, -40.0, -6.0), radius: 400.0, push: 120.0, way: Vector::ZERO };
    app.world_mut().resource_mut::<DrawnParticles>().blasts.push(push);
    run_for(&mut app, 2.0);
    let later = red_middle(&frame).expect("the push threw the droplets out of sight");
    assert!(later > lying + 4.0, "the droplets lay at column {lying} and lie at {later} after a push from their left");
}
