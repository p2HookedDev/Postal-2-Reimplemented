use std::ops::Range;

use bevy::camera::visibility::VisibilityRange;
use bevy::core_pipeline::prepass::{DepthPrepass, NormalPrepass};
use bevy::ecs::entity::EntityHashSet;
use bevy::prelude::*;

use crate::components::ActorModel;
use p2_host::display::{MODERN_FADE_IN, Settings};
use crate::frame_facts::FrameFacts;

/// Level 10 fades over the last quarter of the cull distance.
const SHARE_PER_LEVEL: f32 = 0.025;
/// A first-person eye stands about this far above its pawn at most. Farther (behind view, a cutscene camera) nothing
/// fades: the game's cut is measured from the pawn and Bevy's range from the eye, and the two would disagree.
const MAX_EYE_OFFSET: f32 = 96.0;
const EYE_OFFSET_STEP: f32 = 16.0;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct FadeInSystems;

pub struct FadeInPlugin;

impl Plugin for FadeInPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(Update, FadeInSystems.after(crate::view::SceneViewSystems::Zones).after(crate::modern::ModernGraphicsSystems::Apply))
            .add_systems(Update, (fade_by_cull_distance, normals_for_the_dither).chain().in_set(FadeInSystems));
    }
}

#[derive(Component)]
struct Faded;

/// The game hides a thing past its cull distance from the view target; Bevy's range is measured from the eye,
/// `eye_offset` from that target. The band ends that much past the cut, so only the game's cut hides anything and the
/// scripts see the same at every level. A distance of 0 or below gets no band.
fn fade_band(cull_distance: f32, level: u8, eye_offset: f32) -> Option<Range<f32>> {
    let share = f32::from(level.min(10)) * SHARE_PER_LEVEL;
    (cull_distance > 0.0 && level > 0).then(|| {
        let band = (cull_distance * share).max(4.0 * eye_offset);
        (cull_distance - band).max(0.0)..cull_distance + eye_offset
    })
}

fn stepped_eye_offset(offset: f32) -> Option<f32> {
    let stepped = (offset / EYE_OFFSET_STEP).ceil() * EYE_OFFSET_STEP;
    (stepped <= MAX_EYE_OFFSET).then_some(stepped)
}

/// The band last given to the draws under an `ActorModel` root.
#[derive(Component)]
struct FadeApplied(Option<Range<f32>>);

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn fade_by_cull_distance(
    mut commands: Commands,
    settings: Res<Settings>,
    facts: Res<FrameFacts>,
    models: Query<(Entity, &ActorModel, Option<&FadeApplied>)>,
    children: Query<&Children>,
    parents: Query<&ChildOf>,
    joined: Query<Entity, Or<(Changed<ChildOf>, Added<Mesh3d>)>>,
    draws: Query<Option<&VisibilityRange>, With<Mesh3d>>,
    faded: Query<Entity, With<Faded>>,
    applied: Query<Entity, With<FadeApplied>>,
) {
    let level = settings.modern_level(MODERN_FADE_IN);
    let eye_to_view_target = (facts.in_play && level > 0).then(|| facts.eye_to_view_target.unwrap_or(MAX_EYE_OFFSET));
    let Some(eye_offset) = eye_to_view_target.and_then(stepped_eye_offset) else {
        for entity in &faded {
            commands.entity(entity).remove::<(VisibilityRange, Faded)>();
        }
        for root in &applied {
            commands.entity(root).remove::<FadeApplied>();
        }
        return;
    };
    let joined: EntityHashSet = joined.iter().flat_map(|entity| parents.iter_ancestors(entity)).filter(|&root| models.contains(root)).collect();
    for (root, model, applied) in &models {
        let cull_distance = facts.cull_distances.get(&model.0).copied().unwrap_or(0.0);
        let band = fade_band(cull_distance, level, eye_offset);
        if applied.is_some_and(|applied| applied.0 == band) && !joined.contains(&root) {
            continue;
        }
        commands.entity(root).insert(FadeApplied(band.clone()));
        let wanted = band.map(|end_margin| VisibilityRange { start_margin: 0.0..0.0, end_margin, use_aabb: false });
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
    fn the_band_ends_the_eyes_offset_past_the_cut() {
        assert_eq!(fade_band(1000.0, 10, 0.0), Some(750.0..1000.0));
        assert_eq!(fade_band(1000.0, 10, 48.0), Some(750.0..1048.0));
        let band = fade_band(1000.0, 4, 0.0).unwrap();
        assert!((band.start - 900.0).abs() < 0.01 && band.end == 1000.0);
    }

    #[test]
    fn a_short_distance_keeps_a_band_wider_than_the_eyes_offset() {
        assert_eq!(fade_band(1000.0, 1, 48.0), Some(808.0..1048.0));
        assert_eq!(fade_band(100.0, 1, 48.0), Some(0.0..148.0));
    }

    #[test]
    fn no_band_without_a_distance_or_a_level() {
        assert_eq!(fade_band(0.0, 5, 48.0), None);
        assert_eq!(fade_band(-500.0, 5, 48.0), None);
        assert_eq!(fade_band(1000.0, 0, 48.0), None);
    }

    #[test]
    fn the_range_never_hides_what_the_game_still_shows() {
        let range = VisibilityRange { start_margin: 0.0..0.0, end_margin: fade_band(1000.0, 5, 48.0).unwrap(), use_aabb: false };
        assert!(!range.is_culled(1047.0) && range.is_culled(1048.0) && !range.is_abrupt());
    }

    #[test]
    fn a_far_eye_fades_nothing() {
        assert_eq!(stepped_eye_offset(40.0), Some(48.0));
        assert_eq!(stepped_eye_offset(96.0), Some(96.0));
        assert_eq!(stepped_eye_offset(97.0), None);
    }

    #[test]
    fn the_draws_are_walked_again_only_when_a_distance_or_a_draw_changes() {
        let actor = p2_vm::ObjectId::from_index(1);
        let mut facts = FrameFacts { in_play: true, eye_to_view_target: Some(0.0), ..Default::default() };
        facts.cull_distances.insert(actor, 1000.0);
        let mut world = World::new();
        world.insert_resource(facts);
        world.insert_resource(Settings::read(|query| match query {
            p2_host::display::MODERN_GRAPHICS => "True".to_owned(),
            MODERN_FADE_IN => "10".to_owned(),
            _ => String::new(),
        }));
        let root = world.spawn(ActorModel(actor)).id();
        let draw = world.spawn((Mesh3d::default(), ChildOf(root))).id();
        let fade = world.register_system(fade_by_cull_distance);
        let end = |world: &World, draw: Entity| world.get::<VisibilityRange>(draw).map(|range| range.end_margin.end);
        world.run_system(fade).unwrap();
        assert_eq!(end(&world, draw), Some(1000.0));
        let elsewhere = VisibilityRange { start_margin: 0.0..0.0, end_margin: 0.0..1.0, use_aabb: false };
        world.entity_mut(draw).insert(elsewhere.clone());
        world.run_system(fade).unwrap();
        assert_eq!(end(&world, draw), Some(1.0));
        let late = world.spawn((Mesh3d::default(), ChildOf(root))).id();
        world.run_system(fade).unwrap();
        assert_eq!((end(&world, draw), end(&world, late)), (Some(1000.0), Some(1000.0)));
        world.resource_mut::<FrameFacts>().cull_distances.insert(actor, 2000.0);
        world.entity_mut(draw).insert(elsewhere);
        world.run_system(fade).unwrap();
        assert_eq!((end(&world, draw), end(&world, late)), (Some(2000.0), Some(2000.0)));
    }
}
