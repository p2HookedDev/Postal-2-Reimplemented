use bevy::app::{App, Plugin};
use bevy::ecs::error::Result;
use bevy::ecs::prelude::{Commands, ResMut, SystemSet};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::log::warn;
use bevy::state::prelude::{NextState, in_state};
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};

use crate::ViewerState;
use crate::browser::Browser;
use p2_engine::game_dir_form::GameDirForm;
use p2_engine::game_dir::save_game_dir;
use p2_engine::map::{Hud, Particles, SpawnedActors};

pub struct GameDirPlugin;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum GameDirSystems {
    Ui,
}

impl Plugin for GameDirPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameDirForm>().add_systems(
            EguiPrimaryContextPass,
            form.in_set(GameDirSystems::Ui).run_if(in_state(ViewerState::Setup)),
        );
    }
}

fn form(
    mut contexts: EguiContexts,
    mut form: ResMut<GameDirForm>,
    mut commands: Commands,
    mut next: ResMut<NextState<ViewerState>>,
) -> Result {
    let mut open = false;
    egui::Window::new("Locate POSTAL 2")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(contexts.ctx_mut()?, |ui| {
            ui.label("Select the POSTAL 2 install folder (the one that contains System\\Core.u).");
            form.show(ui);
            open = ui.button("Open").clicked();
        });

    if open {
        form.check();
        let Some(dir) = form.info.as_ref().map(|info| info.dir.clone()) else { return Ok(()) };
        match Browser::new(dir.clone(), None, None) {
            Ok((browser, session)) => {
                if let Err(e) = save_game_dir(&dir) {
                    warn!("could not remember the game folder: {e}");
                }
                form.error = None;
                commands.insert_resource(browser);
                commands.insert_resource(session);
                commands.insert_resource(Particles::default());
                commands.insert_resource(SpawnedActors::default());
                commands.insert_resource(Hud::default());
                next.set(ViewerState::Browse);
            }
            Err(e) => form.error = Some(format!("{}: {e}", dir.display())),
        }
    }
    Ok(())
}
