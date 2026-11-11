use std::path::PathBuf;

use bevy::app::{App, AppExit, Plugin};
use bevy::ecs::error::Result;
use bevy::ecs::prelude::{Commands, MessageWriter, ResMut, Resource, Single};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::log::warn;
use bevy::state::prelude::{AppExtStates, NextState, OnEnter, States, in_state};
use bevy::window::{CursorGrabMode, CursorOptions};
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};
use p2_engine::game_dir::{known_game_dir, save_game_dir};
use p2_engine::game_dir_form::GameDirForm;
use p2_engine::install;
use p2_engine::map::{Hud, LoadMap, Particles, SpawnedActors};

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
            .add_systems(OnEnter(GameState::Launcher), release_cursor)
            .add_systems(EguiPrimaryContextPass, launcher.run_if(in_state(GameState::Launcher)));
    }
}

fn default_install(saved: Option<PathBuf>, found: &[PathBuf]) -> Option<PathBuf> {
    saved.or_else(|| found.first().cloned())
}

fn release_cursor(mut cursor: Single<&mut CursorOptions>) {
    cursor.visible = true;
    cursor.grab_mode = CursorGrabMode::None;
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
        match crate::new_game(&info.dir) {
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
