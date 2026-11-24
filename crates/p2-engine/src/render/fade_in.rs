use std::ops::Range;

use bevy::camera::visibility::VisibilityRange;
use bevy::core_pipeline::prepass::{DepthPrepass, NormalPrepass};
use bevy::prelude::*;

use crate::components::ActorModel;
use crate::display::{MODERN_FADE_IN, Settings};
use crate::play::Play;

/// Level 10 fades over the last quarter of the cull distance.
const SHARE_PER_LEVEL: f32 = 0.025;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct FadeInSystems;

pub struct FadeInPlugin;

impl Plugin for FadeInPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(Update, FadeInSystems.after(crate::mesh_view::SceneViewSystems::Zones).after(crate::render::modern::ModernGraphicsSystems::Apply))
            .add_systems(Update, (fade_by_cull_distance, normals_for_the_dither).chain().in_set(FadeInSystems));
    }
}

#[derive(Component)]
struct Faded;

/// The band ends at the cull distance, so nothing is drawn that retail would not draw. A distance of 0 culls nothing.
fn fade_band(cull_distance: f32, level: u8) -> Option<Range<f32>> {
    let share = f32::from(level.min(10)) * SHARE_PER_LEVEL;
    (cull_distance > 0.0 && level > 0).then_some(cull_distance * (1.0 - share)..cull_distance)
}

fn fade_by_cull_distance(
    mut commands: Commands,
    settings: Res<Settings>,
    play: Option<Res<Play>>,
    models: Query<(Entity, &ActorModel)>,
    children: Query<&Children>,
    draws: Query<Option<&VisibilityRange>, With<Mesh3d>>,
    faded: Query<Entity, With<Faded>>,
) {
    let level = settings.modern_level(MODERN_FADE_IN);
    let world = play.as_deref().and_then(Play::world);
    if level == 0 || world.is_none() {
        for entity in &faded {
            commands.entity(entity).remove::<(VisibilityRange, Faded)>();
        }
        return;
    }
    for (root, model) in &models {
        let cull_distance = world.and_then(|world| world.actor(model.0)).map_or(0.0, |frame| frame.cull_distance);
        let wanted = fade_band(cull_distance, level).map(|end_margin| VisibilityRange { start_margin: 0.0..0.0, end_margin, use_aabb: false });
        for draw in children.iter_descendants(root) {
            let Ok(now) = draws.get(draw) else { continue };
            match &wanted {
                Some(range) if now != Some(range) => {
                    commands.entity(draw).insert((range.clone(), Faded));
                }
                None if faded.contains(draw) => {
                    commands.entity(draw).remove::<(VisibilityRange, Faded)>();
                }
                _ => {}
            }
        }
    }
}

/// A depth-only prepass runs no fragment for an opaque mesh and would keep the depth of the pixels the main pass
/// dithers away; with the normal prepass, Bevy's prepass fragment dithers them the same.
fn normals_for_the_dither(mut commands: Commands, faded: Query<(), With<Faded>>, cameras: Query<Entity, (With<Camera3d>, With<DepthPrepass>, Without<NormalPrepass>)>) {
    if faded.is_empty() {
        return;
    }
    for camera in &cameras {
        commands.entity(camera).insert(NormalPrepass);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_band_ends_at_the_cull_distance() {
        assert_eq!(fade_band(1000.0, 10), Some(750.0..1000.0));
        let band = fade_band(1000.0, 4).unwrap();
        assert!((band.start - 900.0).abs() < 0.01 && band.end == 1000.0);
    }

    #[test]
    fn no_band_without_a_distance_or_a_level() {
        assert_eq!(fade_band(0.0, 5), None);
        assert_eq!(fade_band(1000.0, 0), None);
    }

    #[test]
    fn nothing_is_drawn_from_the_cull_distance_on() {
        let range = VisibilityRange { start_margin: 0.0..0.0, end_margin: fade_band(1000.0, 5).unwrap(), use_aabb: false };
        assert!(range.is_culled(1000.0) && !range.is_culled(999.0) && !range.is_abrupt());
    }
}
