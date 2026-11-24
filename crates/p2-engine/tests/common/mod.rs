#![allow(dead_code, reason = "each test binary uses a part of this module")]

use std::path::PathBuf;
use std::time::Duration;

use bevy::app::App;
use bevy::asset::{AssetApp, AssetPlugin};
use bevy::audio::AudioSource;
use bevy::camera::visibility::Visibility;
use bevy::image::Image;
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::StandardMaterial;
use bevy::time::TimeUpdateStrategy;
use bevy::MinimalPlugins;
use p2_engine::animation::AnimationPlugin;
use p2_render::P2RenderPlugins;
use p2_engine::play::PlayPlugin;

#[path = "../../../p2-render/tests/common/mod.rs"]
mod renderer;
#[allow(unused_imports, reason = "each test binary uses a part of this module")]
pub use renderer::{compiling, headless_renderer, quad};

pub fn game_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("P2_GAME_DIR").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("P2_GAME_DIR unset, skipping");
    }
    dir
}

pub fn played_app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::transform::TransformPlugin, P2RenderPlugins, PlayPlugin, AnimationPlugin, p2_engine::FrameOrderPlugin, p2_engine::map::MapPresentPlugin, p2_render::wounds::WoundsPlugin))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .init_asset::<SkinnedMeshInverseBindposes>()
        .init_asset::<AudioSource>()
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(1.0 / 60.0)))
        .register_required_components::<Mesh3d, Visibility>();
    app
}

