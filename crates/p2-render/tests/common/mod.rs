#![allow(dead_code, reason = "each test binary uses a part of this module")]

use bevy::DefaultPlugins;
use bevy::app::{App, PluginGroup};
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, Mesh, PrimitiveTopology};
use bevy::render::pipelined_rendering::PipelinedRenderingPlugin;
use bevy::window::{ExitCondition, WindowPlugin};
use bevy::winit::WinitPlugin;

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

pub fn compiling(app: &App) -> bool {
    app.get_sub_app(bevy::render::RenderApp).is_some_and(|render| render.world().resource::<bevy::render::render_resource::PipelineCache>().waiting_pipelines().next().is_some())
}

#[allow(dead_code, reason = "not every test file names its stuck pipelines")]
pub fn stuck_pipelines(app: &App) -> String {
    use bevy::render::render_resource::{CachedPipelineState, PipelineCache};
    let Some(render) = app.get_sub_app(bevy::render::RenderApp) else { return String::new() };
    let cache = render.world().resource::<PipelineCache>();
    cache.pipelines().filter(|pipeline| !matches!(pipeline.state, CachedPipelineState::Ok(_))).map(|pipeline| format!("{:?}", pipeline.state)).collect::<Vec<_>>().join(" | ")
}

pub fn headless_renderer(app: &mut App) {
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin { primary_window: None, exit_condition: ExitCondition::DontExit, ..Default::default() })
            .disable::<WinitPlugin>()
            .disable::<PipelinedRenderingPlugin>(),
    );
}
