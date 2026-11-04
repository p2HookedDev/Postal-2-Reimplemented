mod common;

use bevy::app::App;
use bevy::asset::{Assets, RenderAssetUsages};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Camera, Camera3d, ClearColorConfig, RenderTarget};
use bevy::color::Color;
use bevy::image::Image;
use bevy::light::cluster::{ClusterConfig, GlobalClusterSettings};
use bevy::light::{ClusteredDecal, PointLight};
use bevy::math::Vec3;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::view::Msaa;

fn pixel_image() -> Image {
    Image::new(Extent3d { width: 1, height: 1, depth_or_array_layers: 1 }, TextureDimension::D2, vec![255; 4], TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default())
}

#[test]
#[ignore = "needs a GPU"]
fn the_games_cameras_render_with_many_lamps_and_decals() {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    world.resource_mut::<GlobalClusterSettings>().gpu_clustering = None;
    let target = world.resource_mut::<Assets<Image>>().add(Image::new_target_texture(256, 256, TextureFormat::Rgba8UnormSrgb, None));
    let at_target = || RenderTarget::Image(target.clone().into());
    let eye = Transform::from_xyz(0.0, 0.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y);
    world.spawn((Camera3d::default(), Camera { order: -1, ..Default::default() }, at_target(), Msaa::Off, eye, RenderLayers::layer(2), ClusterConfig::None));
    world.spawn((Camera3d::default(), Camera::default(), at_target(), Msaa::Off, eye));
    world.spawn((Camera3d::default(), Camera { order: 1, clear_color: ClearColorConfig::None, ..Default::default() }, at_target(), Msaa::Off, eye, RenderLayers::layer(3), ClusterConfig::None));
    world.spawn((Camera3d::default(), Camera { order: 2, clear_color: ClearColorConfig::None, ..Default::default() }, at_target(), Msaa::Off, eye, RenderLayers::layer(1)));
    let quad = world.resource_mut::<Assets<Mesh>>().add(common::quad(30.0, 0.0, [1.0; 4]));
    let material = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial::default());
    for layer in 0..4 {
        world.spawn((Mesh3d(quad.clone()), MeshMaterial3d(material.clone()), Transform::default(), RenderLayers::layer(layer)));
    }
    for index in 0..358 {
        let place = Vec3::new((index % 20) as f32 * 3.0 - 30.0, (index / 20) as f32 * 3.0 - 30.0, 2.0);
        world.spawn((PointLight { color: Color::WHITE, range: 8.0, ..Default::default() }, Transform::from_translation(place)));
    }
    let decal_image = world.resource_mut::<Assets<Image>>().add(pixel_image());
    for index in 0..60 {
        let place = Vec3::new((index % 10) as f32 * 5.0 - 25.0, (index / 10) as f32 * 5.0 - 15.0, 0.0);
        world.spawn((ClusteredDecal { base_color_texture: Some(decal_image.clone()), tag: index, ..Default::default() }, Transform::from_translation(place).with_scale(Vec3::splat(8.0))));
    }
    let start = std::time::Instant::now();
    let mut frames = 0;
    while start.elapsed() < std::time::Duration::from_secs(25) {
        app.update();
        frames += 1;
        assert!(app.should_exit().is_none(), "the renderer asked to quit after {frames} frames; the error is in the log above");
    }
    assert!(frames > 20, "only {frames} frames");
}
