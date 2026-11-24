use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::Assets;
use bevy::ecs::prelude::{Res, ResMut};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;

use p2_host::display::{MODERN_WOUNDS, Settings};
use crate::play::Play;
use p2_render::wounds::{WoundSplats, WoundsSystems, present_wounds};

use super::MapSystems;
use super::scene::plain_projector_blend;
use p2_render::level::projector_image;

pub(super) struct DecalAgingPlugin;

impl Plugin for DecalAgingPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(PostUpdate, WoundsSystems.after(super::spawned::present_projections)).add_systems(
            PostUpdate,
            load_wound_splats.after(super::spawned::present_projections).before(present_wounds).in_set(MapSystems::Present),
        );
    }
}

/// The decals the game's wall blood splat picks from. Its floor drips and its fluids' sprites are other pictures and
/// are left out: a wound is a splash, not a drop.
const WOUND_SPLATS: [&str; 5] = [
    "nathans.Skins.bloodsplatblend1",
    "nathans.Skins.bloodsplatblend2",
    "nathans.Skins.bloodsplatblend3",
    "nathans.Skins.bloodsplatblend5",
    "nathans.Skins.bloodsplatblend6",
];

/// Read from the install on the first hit on a person, so a wound needs no splat on a wall first.
fn load_wound_splats(
    settings: Option<Res<Settings>>,
    play: Res<Play>,
    mut session: ResMut<super::Session>,
    mut projectors: ResMut<super::Projectors>,
    mut images: ResMut<Assets<Image>>,
    mut splats: ResMut<WoundSplats>,
) {
    if !splats.0.is_empty() || settings.is_none_or(|settings| settings.modern_level(MODERN_WOUNDS) == 0) || play.world().is_none_or(|world| world.person_hits().is_empty()) {
        return;
    }
    let session = &mut *session;
    for name in WOUND_SPLATS {
        let found = match projectors.decal_images.get(name) {
            Some(found) => found.clone(),
            None => {
                let found = super::open_game(&mut session.library, session.map_system.as_deref()).and_then(|game| projector_image(game, |image| images.add(image), name));
                projectors.decal_images.insert(name.into(), found.clone());
                found
            }
        };
        if let Some((image, _, final_blend)) = found {
            splats.0.push((image, plain_projector_blend(final_blend)));
        }
    }
}
