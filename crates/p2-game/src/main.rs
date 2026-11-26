#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod input;
mod launcher;
mod window;

use bevy::DefaultPlugins;
use bevy::app::{App, PluginGroup};
use bevy::render::RenderPlugin;
use bevy::render::settings::{Backends, WgpuFeatures, WgpuSettings};
use bevy::window::WindowPlugin;
use bevy_egui::EguiPlugin;
use p2_engine::P2EnginePlugins;
use p2_engine::diagnostics::{P2DiagnosticsPlugins, log_plugin, start_diagnostics};
use p2_engine::map::MapLoadPlugin;
// Links our crates from their DLLs instead of into the exe.
use p2_dylib_engine as _;

fn main() {
    let build = if cfg!(debug_assertions) { "debug" } else { "release" };
    start_diagnostics(&format!("Postal2Reimplemented {}, commit {}, {build} build", env!("CARGO_PKG_VERSION"), env!("P2_COMMIT")));
    p2_engine::play::init_script_stacks();
    App::new()
        .add_plugins((
            DefaultPlugins
                // Leaving DX12 out of the backends turns off wgpu's per-draw indirect validation.
                .set(RenderPlugin {
                    render_creation: WgpuSettings { backends: Some(Backends::VULKAN), disabled_features: Some(WgpuFeatures::PIPELINE_STATISTICS_QUERY), ..Default::default() }.into(),
                    ..Default::default()
                })
                .set(WindowPlugin { primary_window: Some(window::primary_window()), ..Default::default() })
                .set(log_plugin()),
            EguiPlugin::default(),
            P2EnginePlugins,
            P2DiagnosticsPlugins,
            launcher::LauncherPlugin,
            MapLoadPlugin,
            window::GameWindowPlugin,
            input::PlayerInputPlugin,
        ))
        .run();
}
