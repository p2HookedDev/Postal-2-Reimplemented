mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::App;
use bevy::asset::{Assets, Handle};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Camera, Camera3d, ClearColorConfig, Hdr, RenderTarget};
use bevy::color::{Color, LinearRgba};
use bevy::ecs::entity::Entity;
use bevy::ecs::observer::On;
use bevy::image::Image;
use bevy::light::DirectionalLight;
use bevy::math::Vec3;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use bevy::window::{PrimaryWindow, Window, WindowResolution};
use p2_host::display::{GAMMA, MODERN_GRAPHICS, Settings};
use p2_render::view::{OVERLAY_LAYERS, SceneView, SunShadow};
use p2_render::gamma::GammaPlugin;

const SIZE: u32 = 64;

fn image_of(app: &App, camera: Entity) -> Option<Handle<Image>> {
    match app.world().get::<RenderTarget>(camera) {
        Some(RenderTarget::Image(image)) => Some(image.handle.clone()),
        _ => None,
    }
}

fn unlit(app: &mut App, value: f32, at: Vec3, half: f32, layers: RenderLayers) {
    let world = app.world_mut();
    let mesh = world.resource_mut::<Assets<Mesh>>().add(common::quad(half, 0.0, [1.0; 4]));
    let material = StandardMaterial { base_color: Color::LinearRgba(LinearRgba::rgb(value, value, value)), unlit: true, ..StandardMaterial::default() };
    let material = world.resource_mut::<Assets<StandardMaterial>>().add(material);
    world.spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::from_translation(at), layers));
}

struct Shown {
    bright: u8,
    dim: u8,
    canvas: u8,
    gap: u8,
    beside_sun: u8,
    view_draws_into_the_frame: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Sun {
    None,
    Ahead,
}

fn shown(modern: &'static str, bloom_level: u8) -> Shown {
    shown_after(modern, bloom_level, None, Sun::None)
}

fn settings(modern: &'static str) -> Settings {
    Settings::read(|query| match query {
        MODERN_GRAPHICS => modern.to_owned(),
        GAMMA => "1.05".to_owned(),
        _ => String::new(),
    })
}

fn shown_after(modern: &'static str, bloom_level: u8, modern_on_at: Option<u32>, sun: Sun) -> Shown {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((GammaPlugin, p2_render::modern_bloom::ModernBloomPlugin));
    app.insert_resource(settings(modern));
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    world.spawn((Window { resolution: WindowResolution::new(SIZE, SIZE), ..Window::default() }, PrimaryWindow));
    let place = Transform::from_xyz(0.0, 0.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y);
    let mut view = world.spawn((
        Camera3d::default(),
        Camera { order: -1, clear_color: if sun == Sun::Ahead { ClearColorConfig::Custom(Color::LinearRgba(LinearRgba::new(1.0, 1.0, 1.0, 0.0))) } else { ClearColorConfig::Default }, ..Camera::default() },
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        place,
        SceneView::default(),
    ));
    if modern == "True" {
        view.insert(Hdr);
    }
    if bloom_level > 0 {
        view.insert(p2_render::modern::bloom(bloom_level));
    }
    let view = view.id();
    if sun == Sun::Ahead {
        world.spawn((SunShadow, DirectionalLight { shadow_maps_enabled: false, ..DirectionalLight::default() }, Transform::IDENTITY.looking_to(Vec3::Z, Vec3::Y)));
    }
    let overlay = RenderLayers::layer(OVERLAY_LAYERS[0]);
    let mut canvas = world.spawn((
        Camera3d::default(),
        Camera { order: 1, clear_color: ClearColorConfig::None, ..Camera::default() },
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        place,
        overlay.clone(),
    ));
    if modern == "True" {
        canvas.insert(Hdr);
    }
    let canvas = canvas.id();
    unlit(&mut app, 4.0, Vec3::new(-4.0, 0.0, 0.0), 3.0, RenderLayers::default());
    unlit(&mut app, 0.25, Vec3::new(4.0, 0.0, 0.0), 3.0, RenderLayers::default());
    unlit(&mut app, 1.0, Vec3::new(0.0, 6.0, 0.0), 1.5, overlay);

    let frames = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let start = Instant::now();
    let mut reading = false;
    let mut settled = None;
    for frame in 0.. {
        assert!(start.elapsed() < Duration::from_secs(120), "the frame never drew");
        app.update();
        if modern_on_at == Some(frame) {
            app.insert_resource(settings("True"));
            app.world_mut().entity_mut(view).insert(Hdr);
            app.world_mut().entity_mut(canvas).insert(Hdr);
        }
        if !reading
            && let Some(target) = image_of(&app, canvas)
            && app.world().resource::<Assets<Image>>().get(&target).is_some_and(|image| image.width() == SIZE)
        {
            let store = frames.clone();
            app.world_mut().spawn(Readback::texture(target)).observe(move |event: On<ReadbackComplete>| {
                store.lock().expect("lock").push(event.data.clone());
            });
            reading = true;
        }
        let drawn = frames.lock().expect("lock").last().is_some_and(|last| last[((SIZE / 2 * SIZE + 16) * 4) as usize] != 0);
        if settled.is_none() && drawn && !common::compiling(&app) && modern_on_at.is_none_or(|at| frame > at + 5) {
            settled = Some(frame);
        }
        if settled.is_some_and(|settled| frame > settled + 20) {
            break;
        }
    }
    let frames = frames.lock().expect("lock");
    let last = frames.last().expect("no frame read back");
    let red = |x: u32, y: u32| last[((y * SIZE + x) * 4) as usize];
    Shown { bright: red(16, SIZE / 2), dim: red(48, SIZE / 2), canvas: red(SIZE / 2, 9), gap: red(SIZE / 2, SIZE / 2), beside_sun: red(40, SIZE / 2), view_draws_into_the_frame: image_of(&app, view) == image_of(&app, canvas) }
}

fn srgb(linear: f32) -> u8 {
    let encoded = if linear <= 0.003_130_8 { linear * 12.92 } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 };
    (encoded * 255.0).round() as u8
}

#[test]
#[ignore = "needs a GPU"]
fn the_tone_curve_rolls_off_bright_light_and_leaves_the_rest() {
    let Shown { bright, dim, canvas, view_draws_into_the_frame, .. } = shown("True", 0);
    assert!(view_draws_into_the_frame, "the view draws into another image with the modern graphics on");
    assert!((200..255).contains(&bright), "bright light not rolled off below white: {bright}");
    assert!(dim.abs_diff(srgb(0.25)) <= 3, "the curve changed a low value: {dim}, want {}", srgb(0.25));
    assert_eq!(canvas, 255, "the curve touched what is drawn after it");
}

#[test]
#[ignore = "needs a GPU"]
fn retail_frame_has_no_tone_pass() {
    let Shown { bright, dim, canvas, view_draws_into_the_frame, .. } = shown("False", 0);
    assert!(view_draws_into_the_frame, "the view draws into another image with the modern graphics off");
    assert_eq!(bright, 255, "bright light not clipped as retail");
    assert!(dim.abs_diff(srgb(0.25)) <= 3, "a low value changed: {dim}, want {}", srgb(0.25));
    assert_eq!(canvas, 255);
}

#[test]
#[ignore = "needs a GPU"]
fn bloom_spreads_bright_light_past_its_edge() {
    let (with, without) = (shown("True", 5).gap, shown("True", 0).gap);
    assert!(with > without + 3, "no glow beside the bright quad: {with} with bloom, {without} without");
    let strong = shown("True", 10).gap;
    assert!(strong > with, "the bloom setting does not strengthen the glow: {strong} at 10, {with} at 5");
}

#[test]
#[ignore = "needs a GPU"]
fn the_world_still_shows_when_modern_turns_on_after_the_first_frames() {
    let Shown { bright, dim, .. } = shown_after("False", 0, Some(40), Sun::None);
    assert!((200..255).contains(&bright), "the bright quad after the switch: {bright}");
    assert!(dim.abs_diff(srgb(0.25)) <= 3, "the dim quad after the switch: {dim}, want {}", srgb(0.25));
}

#[test]
#[ignore = "needs a GPU"]
fn sun_rays_carry_bright_sky_away_from_the_sun() {
    let (with, without) = (shown_after("True", 0, None, Sun::Ahead), shown("True", 0));
    assert!(with.beside_sun > without.beside_sun + 10, "no ray from the open sky over the quad beside the sun: {} with the sun, {} without", with.beside_sun, without.beside_sun);
}
