//! What a hit leaves as liquid beyond its GPU spray: heavy drops that fly with the other drops and pour nothing where
//! they land; the scripts' splats mark the ground.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::ecs::message::MessageReader;
use bevy::ecs::prelude::{Res, ResMut};
use bevy::ecs::schedule::IntoScheduleConfigs;
use p2_host::display::{MODERN_LIQUIDS, Settings};

use super::LiquidSpray;
use super::air_draw::AirLiquids;
use super::drops::{Feed, drop_cap};
use super::world::step_liquids;
use crate::frame_facts::FrameFacts;
use crate::screen_pass::modern_level;

pub struct SprayPlugin;

impl Plugin for SprayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, spray_liquids.before(step_liquids));
    }
}

fn spray_liquids(settings: Option<Res<Settings>>, facts: Res<FrameFacts>, mut sprays: MessageReader<LiquidSpray>, air: Option<ResMut<AirLiquids>>) {
    let level = modern_level(settings.as_deref(), MODERN_LIQUIDS);
    if level == 0 || !facts.in_play {
        sprays.clear();
        return;
    }
    let Some(mut air) = air else { return };
    let cap = drop_cap(level);
    for spray in sprays.read() {
        air.light_drops_of(spray.source);
        let feed = Feed { source: spray.source, kind: spray.kind, from: spray.at, velocity: [spray.velocity; 2], radius: [spray.radius; 2], pours: false };
        air.drops.throw_from(&feed, 1.0, 1, 0.0, cap);
    }
}
