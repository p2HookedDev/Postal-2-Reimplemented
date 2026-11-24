//! Modern graphics: depth of field and motion blur while a Matinee shot drives the camera.

use bevy::app::{App, Plugin, Update};
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Local, Query, Res};
use bevy::math::{Quat, Vec3};
use bevy::post_process::dof::{DepthOfField, DepthOfFieldMode};
use bevy::post_process::motion_blur::MotionBlur;

use crate::components::SceneView;
use crate::display::{MODERN_CUTSCENE_DOF, MODERN_CUTSCENE_MOTION_BLUR, Settings};
use crate::play::{Play, PlayWorld};
use crate::render::temporal_aa::is_cut;

/// Bevy's lens works in metres; the world is in Unreal units.
const UNITS_PER_METER: f32 = 50.0;
/// In Unreal units: about where a talking character stands in the game's shots (unverified).
const FOCUS: f32 = 300.0;
/// The f-number at level 1; each level divides it.
const F_STOPS_AT_ONE: f32 = 22.0;
/// Seven taps a pixel.
const BLUR_SAMPLES: u32 = 3;

pub struct CutsceneLensPlugin;

impl Plugin for CutsceneLensPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, follow_cutscene);
    }
}

pub(crate) fn lens(level: u8) -> Option<DepthOfField> {
    (level > 0).then(|| {
        let default = DepthOfField::default();
        DepthOfField {
            mode: DepthOfFieldMode::Gaussian,
            focal_distance: FOCUS,
            aperture_f_stops: F_STOPS_AT_ONE / f32::from(level),
            sensor_height: default.sensor_height * UNITS_PER_METER,
            ..default
        }
    })
}

/// Level 10 opens the shutter for the whole frame; never past it, so nothing streaks further than it moved.
pub(crate) fn shutter(level: u8) -> Option<MotionBlur> {
    (level > 0).then(|| MotionBlur { shutter_angle: f32::from(level) / 10.0, samples: BLUR_SAMPLES })
}

/// On the frame of a cut the motion vectors span the jump, so that frame is not blurred.
pub(crate) fn follow_cutscene(
    mut commands: Commands,
    settings: Option<Res<Settings>>,
    play: Option<Res<Play>>,
    cameras: Query<(Entity, &SceneView, Option<&DepthOfField>, Option<&MotionBlur>)>,
    mut last: Local<Option<(Vec3, Quat)>>,
) {
    let cutscene = play.as_ref().and_then(|play| play.world()).is_some_and(PlayWorld::cutscene);
    let level = |name: &str| settings.as_ref().filter(|_| cutscene).map_or(0, |settings| settings.modern_level(name));
    let wanted = lens(level(MODERN_CUTSCENE_DOF));
    let blur = shutter(level(MODERN_CUTSCENE_MOTION_BLUR));
    for (entity, view, shown, blurred) in &cameras {
        let now = (view.position, view.rotation);
        let cut = (*last).is_some_and(|before| is_cut(before, now));
        *last = Some(now);
        let blur = blur.clone().map(|blur| if cut { MotionBlur { samples: 0, ..blur } } else { blur });
        match (blurred, blur) {
            (None, None) => {}
            (Some(shown), Some(blur)) if shown.shutter_angle == blur.shutter_angle && shown.samples == blur.samples => {}
            (_, Some(blur)) => {
                commands.entity(entity).insert(blur);
            }
            (Some(_), None) => {
                commands.entity(entity).remove::<MotionBlur>();
            }
        }
        match (shown, wanted) {
            (None, None) => {}
            (Some(shown), Some(wanted)) if shown.aperture_f_stops == wanted.aperture_f_stops => {}
            (_, Some(wanted)) => {
                commands.entity(entity).insert(wanted);
            }
            (Some(_), None) => {
                commands.entity(entity).remove::<DepthOfField>();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lens_opens_with_the_level() {
        assert!(lens(0).is_none());
        let (soft, strong) = (lens(1).expect("level 1"), lens(10).expect("level 10"));
        assert!(strong.aperture_f_stops < soft.aperture_f_stops);
        assert_eq!(soft.focal_distance, FOCUS);
        assert!(shutter(0).is_none());
        let (short, open) = (shutter(1).expect("level 1"), shutter(10).expect("level 10"));
        assert!(short.shutter_angle < open.shutter_angle);
        assert_eq!(open.shutter_angle, 1.0);
        assert_eq!(open.samples, BLUR_SAMPLES);
    }
}
