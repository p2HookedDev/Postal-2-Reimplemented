//! Asset viewer. Optional arguments `[package [asset]]` preselect a package (path suffix, e.g. `AW7Tex.utx`) and an
//! asset.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browser;
mod controls;
mod game_dir;
mod walk;

use bevy::DefaultPlugins;
use bevy::app::{App, PluginGroup};
use bevy::log::LogPlugin;
use bevy::state::prelude::{AppExtStates, States};
use bevy_egui::EguiPlugin;

use browser::{AssetBrowserPlugin, Browser};
use controls::ViewerControlsPlugin;
use game_dir::GameDirPlugin;
use p2_engine::P2EnginePlugins;
use p2_engine::diagnostics::{DiagnosticsPlugin, capture_layer, install_panic_hook};
use p2_engine::display::DisplayPlugin;
use p2_engine::game_dir_form::GameDirForm;
use p2_engine::render::gamma::GammaPlugin;
use p2_engine::render::modern::ModernGraphicsPlugin;

#[derive(States, Debug, Clone, PartialEq, Eq, Hash)]
enum ViewerState {
    Setup,
    Browse,
}

fn main() {
    install_panic_hook();
    let mut args = std::env::args().skip(1);
    let package = args.next();
    let asset = args.next();

    p2_engine::play::init_script_stacks();
    let mut app = App::new();
    let defaults = DefaultPlugins.set(LogPlugin { custom_layer: capture_layer, ..Default::default() });
    app.add_plugins((defaults, EguiPlugin::default(), GameDirPlugin, AssetBrowserPlugin, ViewerControlsPlugin))
        .add_plugins(P2EnginePlugins.build().disable::<DisplayPlugin>().disable::<GammaPlugin>().disable::<ModernGraphicsPlugin>())
        .add_plugins(DiagnosticsPlugin);

    let state = match p2_engine::game_dir::known_game_dir() {
        Some(dir) => match Browser::new(dir.clone(), package.as_deref(), asset) {
            Ok((browser, session)) => {
                app.insert_resource(browser);
                app.insert_resource(session);
                ViewerState::Browse
            }
            Err(e) => {
                let mut form = GameDirForm::new(&dir);
                form.error = Some(format!("{}: {e}", dir.display()));
                app.insert_resource(form);
                ViewerState::Setup
            }
        },
        None => ViewerState::Setup,
    };
    app.insert_state(state).run();
}
