//! Modern graphics: photo mode's engine side. The script asks for a picture with the console word `shot`; it is saved
//! as a PNG in `Screenshots` beside the settings file. Its depth of field is Bevy's, on the scene camera.

use bevy::app::{App, Plugin, Update};
use bevy::ecs::entity::Entity;
use bevy::ecs::query::With;
use bevy::ecs::system::{Commands, Query, Res, ResMut};
use bevy::post_process::dof::{DepthOfField, DepthOfFieldMode};
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use p2_vm::{LoaderRef, ObjectId, Prop, Vm, ini_truthy};

use crate::components::SceneView;
use crate::display::{MODERN_PHOTO_ACTIVE, MODERN_PHOTO_BLUR, MODERN_PHOTO_FOCUS, MODERN_PHOTO_HIDE_PLAYER, Settings};
use crate::play::Play;

/// In Unreal units, at focus level 0; each four levels double it.
const NEAREST_FOCUS: f32 = 32.0;
/// Bevy's lens works in metres; the world is in Unreal units.
const UNITS_PER_METER: f32 = 50.0;
/// The f-number at blur level 1; each level divides it.
const F_STOPS_AT_ONE: f32 = 22.0;

pub struct PhotoPlugin;

impl Plugin for PhotoPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (take_screenshot, follow_photo_lens));
    }
}

pub(crate) fn photo_active<L: LoaderRef>(vm: &mut Vm<L>) -> bool {
    vm.get_command(MODERN_PHOTO_ACTIVE).is_ok_and(|value| ini_truthy(&value))
}

/// The key is saved with the settings, so a crash while photo mode was open would leave it on.
pub(crate) fn clear_photo_active<L: LoaderRef>(vm: &mut Vm<L>) {
    if photo_active(vm) {
        vm.set_command(&format!("{MODERN_PHOTO_ACTIVE} 0"));
    }
}

/// The player's pawn while photo mode is open with its hide row set.
pub(crate) fn photo_hidden_pawn<L: LoaderRef>(vm: &mut Vm<L>) -> Option<ObjectId> {
    if !photo_active(vm) || !vm.get_command(MODERN_PHOTO_HIDE_PLAYER).is_ok_and(|value| ini_truthy(&value)) {
        return None;
    }
    vm.store.object_of(vm.player()?, Prop::Pawn)
}

pub(crate) fn focus_distance(level: u8) -> f32 {
    NEAREST_FOCUS * (f32::from(level) / 4.0).exp2()
}

pub(crate) fn photo_lens(settings: &Settings) -> Option<DepthOfField> {
    let focus = settings.modern_level(MODERN_PHOTO_FOCUS);
    if focus == 0 || !settings.photo_active() {
        return None;
    }
    let blur = settings.modern_level(MODERN_PHOTO_BLUR);
    (blur > 0).then(|| {
        let default = DepthOfField::default();
        DepthOfField {
            mode: DepthOfFieldMode::Gaussian,
            focal_distance: focus_distance(focus),
            aperture_f_stops: F_STOPS_AT_ONE / f32::from(blur),
            sensor_height: default.sensor_height * UNITS_PER_METER,
            ..default
        }
    })
}

pub(crate) fn follow_photo_lens(mut commands: Commands, settings: Option<Res<Settings>>, cameras: Query<(Entity, Option<&DepthOfField>), With<SceneView>>) {
    let wanted = settings.as_deref().and_then(photo_lens);
    for (entity, shown) in &cameras {
        match (shown, wanted) {
            (None, None) => {}
            (Some(shown), Some(wanted)) if shown.aperture_f_stops == wanted.aperture_f_stops && shown.focal_distance == wanted.focal_distance => {}
            (_, Some(wanted)) => {
                commands.entity(entity).insert(wanted);
            }
            (Some(_), None) => {
                commands.entity(entity).remove::<DepthOfField>();
            }
        }
    }
}

fn take_screenshot(mut commands: Commands, play: Option<ResMut<Play>>) {
    if !play.and_then(|play| play.into_inner().world_mut()).is_some_and(|world| world.take_screenshot()) {
        return;
    }
    let Some(folder) = crate::game_dir::config_folder().map(|folder| folder.join("Screenshots")) else { return };
    if let Err(error) = std::fs::create_dir_all(&folder) {
        bevy::log::warn!("screenshot folder {}: {error}", folder.display());
        return;
    }
    let name = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S_%3f.png").to_string();
    commands.spawn(Screenshot::primary_window()).observe(save_to_disk(folder.join(name)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::MODERN_GRAPHICS;

    #[test]
    fn focus_doubles_every_four_levels() {
        assert!((focus_distance(0) - NEAREST_FOCUS).abs() < 1e-3);
        assert!((focus_distance(4) - 64.0).abs() < 1e-3);
        assert!((focus_distance(36) - 16384.0).abs() < 0.1);
    }

    #[test]
    fn the_photo_lens_needs_photo_mode_a_focus_and_a_blur() {
        let settings = |active: &str, focus: &str, blur: &str| {
            Settings::read(|query| match query {
                MODERN_GRAPHICS => "True".to_owned(),
                MODERN_PHOTO_ACTIVE => active.to_owned(),
                MODERN_PHOTO_FOCUS => focus.to_owned(),
                MODERN_PHOTO_BLUR => blur.to_owned(),
                _ => String::new(),
            })
        };
        assert!(photo_lens(&settings("0", "8", "5")).is_none());
        assert!(photo_lens(&settings("1", "0", "5")).is_none());
        assert!(photo_lens(&settings("1", "8", "0")).is_none());
        let (soft, strong) = (photo_lens(&settings("1", "8", "2")).expect("open"), photo_lens(&settings("1", "8", "10")).expect("open"));
        assert!((soft.focal_distance - focus_distance(8)).abs() < 1e-3);
        assert!(strong.aperture_f_stops < soft.aperture_f_stops);
    }
}
