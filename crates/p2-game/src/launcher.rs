use std::path::{Path, PathBuf};

use anyhow::Context;
use bevy::app::{App, AppExit, Plugin};
use bevy::ecs::error::Result;
use bevy::ecs::prelude::{Commands, MessageReader, MessageWriter, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::schedule::common_conditions::resource_exists;
use bevy::log::warn;
use bevy::math::UVec2;
use bevy::state::prelude::{AppExtStates, NextState, OnEnter, States, in_state};
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};
use p2_host::game_dir::{known_game_dir, save_game_dir};
use p2_host::game_dir_form::GameDirForm;
use p2_host::install;
use p2_engine::map::{CanvasArea, Hud, LoadMap, MapFailed, MapSystems, Particles, Session, SpawnedActors, map_package, start_map};
use p2_package::Ini;

#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GameState {
    #[default]
    Launcher,
    Playing,
}

#[derive(Resource, Default)]
pub struct Launcher {
    installs: Vec<PathBuf>,
    pub failure: Option<String>,
}

pub struct LauncherPlugin;

impl Plugin for LauncherPlugin {
    fn build(&self, app: &mut App) {
        let installs = install::detect();
        let form = default_install(known_game_dir(), &installs).map(|dir| GameDirForm::new(&dir)).unwrap_or_default();
        app.init_state::<GameState>()
            .insert_resource(form)
            .insert_resource(Launcher { installs, failure: None })
            .add_systems(OnEnter(GameState::Launcher), drop_session)
            .add_systems(EguiPrimaryContextPass, launcher.run_if(in_state(GameState::Launcher)))
            .add_systems(EguiPrimaryContextPass, place_map.after(MapSystems::Load).before(MapSystems::Paint).run_if(resource_exists::<Session>));
    }
}

pub(crate) fn system_ini(root: &Path) -> anyhow::Result<Ini> {
    let ini = root.join("System").join("Postal2.ini");
    Ini::read(&ini).with_context(|| ini.display().to_string())
}

fn new_game(root: &Path) -> anyhow::Result<(String, Session)> {
    let system = system_ini(root)?;
    let map = match start_map(std::env::args().skip(1), root) {
        Some(name) => name,
        None => system.get("URL", "LocalMap").context("no [URL] LocalMap in Postal2.ini")?.to_owned(),
    };
    Ok((map_package(&map), Session::new(root.to_path_buf())))
}

fn place_map(
    mut contexts: EguiContexts,
    mut view: ResMut<p2_render::world::view::MainView>,
    mut area: ResMut<CanvasArea>,
    mut launcher: ResMut<Launcher>,
    mut next: ResMut<NextState<GameState>>,
    mut failed: MessageReader<MapFailed>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    let rect = ctx.viewport_rect();
    let scale = ctx.pixels_per_point();
    let size = UVec2::new((rect.width() * scale) as u32, (rect.height() * scale) as u32);
    view.viewport = (size.x > 0 && size.y > 0).then_some((UVec2::ZERO, size));
    area.0 = Some(rect);
    if let Some(MapFailed(failure)) = failed.read().last() {
        launcher.failure = Some(failure.clone());
        view.viewport = None;
        area.0 = None;
        next.set(GameState::Launcher);
    }
    Ok(())
}

fn default_install(saved: Option<PathBuf>, found: &[PathBuf]) -> Option<PathBuf> {
    saved.or_else(|| found.first().cloned())
}

/// Removed at the state change: map systems later in the failing frame still read the session.
fn drop_session(mut commands: Commands) {
    commands.remove_resource::<Session>();
}

fn launcher(
    mut contexts: EguiContexts,
    mut state: ResMut<Launcher>,
    mut form: ResMut<GameDirForm>,
    mut commands: Commands,
    mut next: ResMut<NextState<GameState>>,
    mut exit: MessageWriter<AppExit>,
    mut loads: MessageWriter<LoadMap>,
) -> Result {
    let (mut play, mut quit) = (false, false);
    egui::Window::new("POSTAL 2 Reimplemented")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(contexts.ctx_mut()?, |ui| {
            if state.installs.is_empty() {
                ui.label("No POSTAL 2 install was found. Pick its folder with Browse.");
            } else {
                ui.label("POSTAL 2 installs found on this computer:");
                for dir in &state.installs {
                    let chosen = form.info.as_ref().is_some_and(|info| &info.dir == dir);
                    if ui.selectable_label(chosen, dir.display().to_string()).clicked() {
                        form.set(dir);
                    }
                }
            }
            ui.add_space(8.0);
            ui.label("Game folder:");
            form.show(ui);
            if let Some(info) = &form.info {
                ui.label(info.edition());
            }
            if let Some(failure) = &state.failure {
                ui.add_space(8.0);
                ui.colored_label(egui::Color32::LIGHT_RED, failure);
                if ui.button("Copy details").clicked() {
                    ui.ctx().copy_text(failure.clone());
                }
            }
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                let button = egui::Button::new(egui::RichText::new("Play").size(28.0)).min_size(egui::vec2(220.0, 52.0));
                play = ui.add_enabled(form.info.is_some(), button).clicked();
                quit = ui.button("Quit").clicked();
            });
        });

    if quit {
        exit.write(AppExit::Success);
    }
    if play && let Some(info) = form.info.clone() {
        match new_game(&info.dir) {
            Ok((package, session)) => {
                if let Err(e) = save_game_dir(&info.dir) {
                    warn!("could not remember the game folder: {e}");
                }
                state.failure = None;
                loads.write(LoadMap { package, url: None });
                commands.insert_resource(session);
                commands.insert_resource(Particles::default());
                commands.insert_resource(SpawnedActors::default());
                commands.insert_resource(Hud::default());
                next.set(GameState::Playing);
            }
            Err(e) => state.failure = Some(format!("{e:#}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_install_comes_first_then_the_first_found() {
        let found = [PathBuf::from("C:/A"), PathBuf::from("C:/B")];
        assert_eq!(default_install(Some(PathBuf::from("C:/Saved")), &found), Some(PathBuf::from("C:/Saved")));
        assert_eq!(default_install(None, &found), Some(PathBuf::from("C:/A")));
        assert_eq!(default_install(None, &[]), None);
    }
}
