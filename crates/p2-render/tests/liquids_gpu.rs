mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::App;
use bevy::asset::Assets;
use bevy::camera::{Camera, Camera3d, RenderTarget};
use bevy::color::Color;
use bevy::core_pipeline::core_3d::{main_opaque_pass_3d, main_transparent_pass_3d};
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::ecs::observer::On;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::AmbientLight;
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
use p2_render::particle_frame::{DrawnActor, DrawnParticles, Liquid, LiquidRole, Pour};
use p2_render::view::SceneView;
use p2_vm::FluidType;

const SIZE: u32 = 128;

fn is_red([r, g, ..]: [u8; 4]) -> bool {
    u16::from(r) > u16::from(g) + 30
}

fn red_pixels(frame: &Mutex<Vec<u8>>) -> usize {
    frame.lock().expect("lock").as_chunks::<4>().0.iter().filter(|&&pixel| is_red(pixel)).count()
}

/// A slow blood pour in front of a grey wall, 40 units ahead of the eye, under white ambient light; the app and the
/// frame it last drew.
fn pouring() -> (App, Arc<Mutex<Vec<u8>>>) {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins(LiquidsPlugin);
    app.insert_resource(Settings::read(|query| match query {
        MODERN_GRAPHICS => "True".to_owned(),
        MODERN_LIQUIDS => "10".to_owned(),
        _ => String::new(),
    }));
    app.insert_resource(FrameFacts { in_play: true, game_step: 0.02, ..FrameFacts::default() });
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
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        DepthPrepass,
        AmbientLight::default(),
        Transform::default(),
        SceneView::default(),
    ));
    let wall = world.resource_mut::<Assets<Mesh>>().add(common::quad(100.0, -60.0, [1.0; 4]));
    let grey = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial { base_color: Color::srgb(0.2, 0.2, 0.2), unlit: true, ..Default::default() });
    world.spawn((Mesh3d(wall), MeshMaterial3d(grey), Transform::default()));
    let parent = world.spawn_empty().id();
    let pour = Pour {
        from: Vector::new(0.0, -40.0, 8.0),
        velocity: [Vector::new(-4.0, 0.0, -4.0), Vector::new(4.0, 0.0, 4.0)],
        acceleration: Vector::new(0.0, 0.0, -15.0),
        rate: 120.0,
        flowing: true,
    };
    let liquid = Liquid { kind: FluidType::Blood, role: LiquidRole::Stream, burning: false, pour: Some(pour) };
    world.resource_mut::<DrawnParticles>().actors.push(DrawnActor { parent, drawn: true, liquid: Some(liquid), ..DrawnActor::default() });

    let start = Instant::now();
    while red_pixels(&frames) < 20 {
        app.update();
        assert!(start.elapsed() < Duration::from_secs(60), "no liquid was drawn: compiling {}", common::compiling(&app));
    }
    (app, frames)
}

#[test]
#[ignore = "needs a GPU"]
fn a_blood_pour_draws_a_red_surface_that_is_gone_once_the_pour_stops_and_its_drops_die() {
    let (mut app, frame) = pouring();
    let liquid = app.world_mut().resource_mut::<DrawnParticles>().actors[0].liquid.as_mut().and_then(|liquid| liquid.pour.as_mut()).map(|pour| pour.flowing = false);
    assert!(liquid.is_some());
    let stopped = Instant::now();
    while red_pixels(&frame) > 0 {
        app.update();
        assert!(stopped.elapsed() < Duration::from_secs(30), "{} pixels of liquid are still drawn", red_pixels(&frame));
    }
}

