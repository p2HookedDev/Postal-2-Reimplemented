use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Assets, Handle};
use bevy::ecs::prelude::{DetectChangesMut, Res, ResMut};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::math::Vec2;

use p2_assets::FinalBlend;
use p2_host::display::{MODERN_LIQUIDS, MODERN_WOUNDS, Settings};
use crate::play::Play;
use p2_render::liquids::LiquidTextures;
use p2_render::wounds::{WoundSplats, WoundsSystems, present_wounds};

use super::MapSystems;
use super::scene::plain_projector_blend;
use p2_render::level::projector_image;

pub(super) struct DecalAgingPlugin;

impl Plugin for DecalAgingPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(PostUpdate, WoundsSystems.after(super::spawned::present_projections)).add_systems(
            PostUpdate,
            (
                load_wound_splats.after(super::spawned::present_projections).before(present_wounds).in_set(MapSystems::Present),
                load_liquid_textures.in_set(MapSystems::Present),
            ),
        );
    }
}

const WOUND_SPLATS: [&str; 5] = [
    "nathans.Skins.bloodsplatblend1",
    "nathans.Skins.bloodsplatblend2",
    "nathans.Skins.bloodsplatblend3",
    "nathans.Skins.bloodsplatblend5",
    "nathans.Skins.bloodsplatblend6",
];

const PUKE_SPLAT: &str = "nathans.Skins.pukesplat";
const PUKE_POUR: &str = "nathans.Skins.pukepour";
const PUKE_SPLASH: &str = "nathans.Skins.pukesplash";

fn decal_image(session: &mut super::Session, projectors: &mut super::Projectors, images: &mut Assets<Image>, name: &str) -> Option<(Handle<Image>, Vec2, Option<FinalBlend>)> {
    if let Some(found) = projectors.decal_images.get(name) {
        return found.clone();
    }
    let found = super::open_game(&mut session.library, session.map_system.as_deref()).and_then(|game| projector_image(game, |image| images.add(image), name));
    projectors.decal_images.insert(name.into(), found.clone());
    found
}

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
    for name in WOUND_SPLATS {
        if let Some((image, _, final_blend)) = decal_image(&mut session, &mut projectors, &mut images, name) {
            splats.0.push((image, plain_projector_blend(final_blend)));
        }
    }
}

fn load_liquid_textures(
    settings: Option<Res<Settings>>,
    play: Res<Play>,
    mut session: ResMut<super::Session>,
    mut projectors: ResMut<super::Projectors>,
    mut images: ResMut<Assets<Image>>,
    mut textures: ResMut<LiquidTextures>,
) {
    if textures.puke_splat.is_some() || settings.is_none_or(|settings| settings.modern_level(MODERN_LIQUIDS) == 0) || play.world().is_none() {
        return;
    }
    let mut image = |name: &str| decal_image(&mut session, &mut projectors, &mut images, name).map(|(image, ..)| image);
    let loaded = LiquidTextures { puke_splat: image(PUKE_SPLAT), puke_pour: image(PUKE_POUR), puke_splash: image(PUKE_SPLASH) };
    textures.set_if_neq(loaded);
}
