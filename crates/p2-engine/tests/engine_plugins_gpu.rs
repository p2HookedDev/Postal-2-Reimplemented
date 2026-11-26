mod common;

use bevy::app::{App, PluginGroup, PluginGroupBuilder};
use bevy::asset::Assets;
use bevy::camera::{Camera, Camera3d, RenderTarget};
use bevy::image::Image;
use bevy::prelude::Transform;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use bevy_egui::EguiPlugin;
use p2_engine::P2EnginePlugins;
use p2_host::settings::DisplayPlugin;
use p2_render::post::gamma::GammaPlugin;
use p2_render::modern::ModernGraphicsPlugin;

fn run_frames(engine: PluginGroupBuilder) {
    p2_engine::play::init_script_stacks();
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((EguiPlugin::default(), engine));
    app.finish();
    app.cleanup();
    app.update();
    let world = app.world_mut();
    let target = world.resource_mut::<Assets<Image>>().add(Image::new_target_texture(64, 64, TextureFormat::Rgba8UnormSrgb, None));
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::default(),
    ));
    for _ in 0..5 {
        app.update();
    }
}

#[test]
#[ignore = "needs a GPU"]
fn the_games_plugin_set_builds_and_runs_frames() {
    run_frames(P2EnginePlugins.build());
}

#[test]
#[ignore = "needs a GPU"]
fn the_viewers_plugin_set_builds_and_runs_frames() {
    run_frames(P2EnginePlugins.build().disable::<DisplayPlugin>().disable::<GammaPlugin>().disable::<ModernGraphicsPlugin>());
}
