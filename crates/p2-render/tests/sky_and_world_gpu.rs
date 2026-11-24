mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::{App, Update};
use bevy::asset::{Assets, Handle};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Camera, Camera3d, ClearColorConfig, RenderTarget};
use bevy::color::Color;
use bevy::ecs::entity::Entity;
use bevy::ecs::observer::On;
use bevy::image::Image;
use bevy::light::{CascadeShadowConfigBuilder, DirectionalLight};
use bevy::math::Vec3;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use bevy::window::{PrimaryWindow, Window, WindowResolution};
use p2_host::display::{GAMMA, MODERN_GRAPHICS, Settings};
use p2_render::view::{OVERLAY_LAYERS, SKY_LAYER, SUN_VIEW_LAYER, SceneView};
use p2_render::gamma::GammaPlugin;
use p2_render::mesh_light::{DynamicLit, MeshLightPlugin, MeshLitMaterial};

const SIZE: u32 = 64;

fn image_of(app: &App, camera: Entity) -> Option<Handle<Image>> {
    match app.world().get::<RenderTarget>(camera) {
        Some(RenderTarget::Image(image)) => Some(image.handle.clone()),
        _ => None,
    }
}

/// The red of the world's quad, the gap between the quads and the sky's quad, as the frame before the HUD shows them.
/// The scene camera draws the sky quad too, through a sky material.
fn world_gap_sky(sky_at: Vec3, sky_color: Color) -> [u8; 3] {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((GammaPlugin, MeshLightPlugin));
    app.add_systems(Update, p2_render::modern::apply);
    app.insert_resource(Settings::read(|query| match query {
        MODERN_GRAPHICS => "True".to_owned(),
        GAMMA => "1.05".to_owned(),
        _ => String::new(),
    }));
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    world.spawn((Window { resolution: WindowResolution::new(SIZE, SIZE), ..Window::default() }, PrimaryWindow));
    let place = Transform::from_xyz(0.0, 0.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y);
    let lens = (Camera3d::default(), Tonemapping::None, DebandDither::Disabled, Msaa::Off, place);
    world.spawn((lens.clone(), Camera { order: -1, ..Camera::default() }, SceneView::default(), RenderLayers::from_layers(&[0, SKY_LAYER])));
    let canvas = world.spawn((lens, Camera { order: 1, clear_color: ClearColorConfig::None, ..Camera::default() }, RenderLayers::layer(OVERLAY_LAYERS[0]))).id();
    let sun = DirectionalLight { shadow_maps_enabled: false, affects_lightmapped_mesh_diffuse: false, ..DirectionalLight::default() };
    let cascades = CascadeShadowConfigBuilder { overlap_proportion: 0.1, ..CascadeShadowConfigBuilder::default() }.build();
    world.spawn((sun, cascades, Transform::IDENTITY.looking_to(Vec3::NEG_Z, Vec3::Y), RenderLayers::layer(SUN_VIEW_LAYER)));
    let quad = world.resource_mut::<Assets<Mesh>>().add(common::quad(3.0, 0.0, [1.0; 4]));
    let mut extension = DynamicLit::default();
    extension.layers.detail.z = 7.0;
    let sky_look = MeshLitMaterial { base: StandardMaterial { base_color: sky_color, ..StandardMaterial::default() }, extension: DynamicLit { sky: true, ..extension.clone() } };
    let sky_lit = world.resource_mut::<Assets<MeshLitMaterial>>().add(sky_look);
    let lit = world.resource_mut::<Assets<MeshLitMaterial>>().add(MeshLitMaterial { base: StandardMaterial::default(), extension });
    world.spawn((Mesh3d(quad.clone()), MeshMaterial3d(lit), Transform::from_xyz(-4.0, 0.0, 0.0)));
    world.spawn((Mesh3d(quad), MeshMaterial3d(sky_lit), Transform::from_translation(sky_at), RenderLayers::layer(SKY_LAYER)));

    let frames = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let start = Instant::now();
    let mut reading = false;
    let mut settled = None;
    for frame in 0.. {
        app.update();
        assert!(start.elapsed() < Duration::from_secs(40), "the frame never drew: reading {reading}, frames {}, compiling {}, canvas target {:?}, waiting on {}", frames.lock().expect("lock").len(), common::compiling(&app), image_of(&app, canvas).is_some(), common::stuck_pipelines(&app));
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
        if settled.is_none() && frames.lock().expect("lock").len() > 5 && !common::compiling(&app) {
            settled = Some(frame);
        }
        if settled.is_some_and(|settled| frame > settled + 30) {
            break;
        }
    }
    let frames = frames.lock().expect("lock");
    let last = frames.last().expect("no frame read back");
    let red = |x: u32| last[((SIZE / 2 * SIZE + x) * 4) as usize];
    [red(16), red(SIZE / 2), red(48)]
}

#[test]
#[ignore = "needs a GPU"]
fn the_world_and_the_sky_both_show_through_the_tone_pass() {
    let [world, gap, sky] = world_gap_sky(Vec3::new(4.0, 0.0, 0.0), Color::WHITE);
    assert!(world > gap.saturating_add(50), "the world's quad is not in the frame: {world} against the empty {gap}");
    assert!(sky > gap.saturating_add(50), "the sky's quad is not in the frame: {sky} against the empty {gap}");
}

#[test]
#[ignore = "needs a GPU"]
fn a_sky_quad_nearer_than_the_world_stays_behind_it() {
    let [plain, ..] = world_gap_sky(Vec3::new(4.0, 0.0, 0.0), Color::WHITE);
    let [covered, ..] = world_gap_sky(Vec3::new(-4.0, 0.0, 10.0), Color::srgb(0.0, 0.0, 1.0));
    assert!(covered.abs_diff(plain) <= 2, "the nearer sky quad covered the world's: red {covered} against {plain}");
}
