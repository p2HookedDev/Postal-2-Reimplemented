use std::f32::consts::FRAC_PI_3;

use bevy::anti_alias::taa::TemporalAntiAliasing;
use bevy::app::{App, Plugin, Update};
use bevy::camera::{Camera, Camera3d};
use bevy::core_pipeline::prepass::{DepthPrepass, MotionVectorPrepass};
use bevy::ecs::prelude::{Commands, Entity, Has, Local, Query, Res, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::math::{Quat, Vec3};
use bevy::post_process::motion_blur::MotionBlur;
use bevy::render::camera::TemporalJitter;

use crate::world::components::SceneView;
use p2_host::settings::{MODERN_TEMPORAL_AA, Settings};
use crate::world::view::SceneViewSystems;
use crate::post::photo::follow_photo_lens;
use crate::modern::ModernGraphicsSystems;

pub struct TemporalAaPlugin;

impl Plugin for TemporalAaPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, follow_temporal_aa.after(ModernGraphicsSystems::Apply).after(SceneViewSystems::Place).after(follow_photo_lens));
    }
}

const CUT_DISTANCE: f32 = 256.0;
const CUT_TURN: f32 = FRAC_PI_3;

pub(crate) fn is_cut((was_at, was_facing): (Vec3, Quat), (at, facing): (Vec3, Quat)) -> bool {
    was_at.distance_squared(at) > CUT_DISTANCE * CUT_DISTANCE || was_facing.angle_between(facing) > CUT_TURN
}

fn follow_temporal_aa(
    mut commands: Commands,
    settings: Option<Res<Settings>>,
    mut cameras: Query<
        (Entity, &Camera, &SceneView, Option<&mut TemporalAntiAliasing>, Has<DepthPrepass>, Has<MotionVectorPrepass>, Has<MotionBlur>),
        With<Camera3d>,
    >,
    mut last: Local<Option<(Vec3, Quat)>>,
) {
    let wanted = settings.is_some_and(|settings| settings.modern_level(MODERN_TEMPORAL_AA) > 0);
    for (entity, camera, view, taa, depth, motion, blur) in &mut cameras {
        if motion != (wanted || blur) {
            if motion {
                commands.entity(entity).remove::<MotionVectorPrepass>();
            } else {
                commands.entity(entity).insert(MotionVectorPrepass);
            }
        }
        match taa {
            Some(mut taa) if wanted => {
                if !depth {
                    commands.entity(entity).insert(DepthPrepass);
                }
                let now = (view.position, view.rotation);
                if camera.is_active && (*last).is_none_or(|before| is_cut(before, now)) {
                    taa.reset = true;
                }
                *last = camera.is_active.then_some(now);
            }
            Some(_) => {
                commands.entity(entity).remove::<(TemporalAntiAliasing, TemporalJitter)>();
                *last = None;
            }
            None if wanted => {
                commands.entity(entity).insert(TemporalAntiAliasing::default());
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walking_and_turning_keep_the_history() {
        let before = (Vec3::ZERO, Quat::IDENTITY);
        assert!(!is_cut(before, (Vec3::new(20.0, 0.0, 0.0), Quat::from_rotation_y(0.2))));
    }

    #[test]
    fn a_jump_or_a_sharp_turn_is_a_cut() {
        let before = (Vec3::ZERO, Quat::IDENTITY);
        assert!(is_cut(before, (Vec3::new(0.0, 0.0, 400.0), Quat::IDENTITY)));
        assert!(is_cut(before, (Vec3::ZERO, Quat::from_rotation_y(1.5))));
    }
}
