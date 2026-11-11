#![allow(dead_code, reason = "each test binary uses a part of this module")]

use std::path::PathBuf;
use std::time::Duration;

use bevy::app::{App, PluginGroup};
use bevy::asset::{AssetApp, AssetPlugin, RenderAssetUsages};
use bevy::audio::AudioSource;
use bevy::camera::visibility::Visibility;
use bevy::image::Image;
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::mesh::{Indices, Mesh, Mesh3d, PrimitiveTopology};
use bevy::pbr::StandardMaterial;
use bevy::render::pipelined_rendering::PipelinedRenderingPlugin;
use bevy::time::TimeUpdateStrategy;
use bevy::window::{ExitCondition, WindowPlugin};
use bevy::winit::WinitPlugin;
use bevy::{DefaultPlugins, MinimalPlugins};
use p2_engine::animation::AnimationPlugin;
use p2_engine::map::{Fluids, Hud, MaterialSwitches, Particles, Projectors, SpawnedActors};
use p2_engine::mesh_view::MeshViewPlugin;
use p2_engine::play::PlayPlugin;

pub fn game_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("P2_GAME_DIR").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("P2_GAME_DIR unset, skipping");
    }
    dir
}

pub fn played_app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::transform::TransformPlugin, MeshViewPlugin, PlayPlugin, AnimationPlugin))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .init_asset::<SkinnedMeshInverseBindposes>()
        .init_asset::<AudioSource>()
        .init_resource::<Fluids>()
        .init_resource::<MaterialSwitches>()
        .init_resource::<Projectors>()
        .init_resource::<Particles>()
        .init_resource::<SpawnedActors>()
        .init_resource::<Hud>()
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(1.0 / 60.0)))
        .register_required_components::<Mesh3d, Visibility>();
    app
}

pub fn quad(half: f32, z: f32, color: [f32; 4]) -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    let corners = [[-half, -half, z], [half, -half, z], [half, half, z], [-half, half, z]];
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, corners.to_vec());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.5, 0.5]; 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, vec![[0.5, 0.5]; 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![color; 4]);
    mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
    mesh
}

/// Read backs during this are not results: a frame stays black or clear for as long as a shader takes to compile.
pub fn compiling(app: &App) -> bool {
    app.get_sub_app(bevy::render::RenderApp).is_some_and(|render| render.world().resource::<bevy::render::render_resource::PipelineCache>().waiting_pipelines().next().is_some())
}

pub fn headless_renderer(app: &mut App) {
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin { primary_window: None, exit_condition: ExitCondition::DontExit, ..Default::default() })
            .disable::<WinitPlugin>()
            .disable::<PipelinedRenderingPlugin>(),
    );
}
