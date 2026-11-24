//! The world's liquids on the main world: where the scripts' streams land and where drops land is poured into the
//! pools, which step at a fixed rate on the level's clock.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::ecs::message::MessageReader;
use bevy::ecs::prelude::{DetectChanges, Res, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use p2_bevy_assets::to_bevy;
use p2_host::display::{MODERN_LIQUIDS, Settings};
use p2_math::Vector;
use p2_vm::FluidType;

use super::LiquidLanding;
use super::pools::{CELL, PoolStore, Spill};
use crate::frame_facts::FrameFacts;
use crate::level_collision::LevelCollision;
use crate::particle_draw::draw_particles;
use crate::particle_frame::{DrawnParticles, Liquid, LiquidRole};
use crate::screen_pass::modern_level;

const FLOW_STEP: f32 = 1.0 / 30.0;
const MOST_STEPS_PER_FRAME: f32 = 4.0;
/// Depth in Unreal units a pool loses a second.
const DRYING: f32 = 1.0 / 90.0;
/// As much blood as 256 cells one unit deep.
const BLOOD_BUDGET: f32 = 256.0 * CELL * CELL;
const FEWEST_CHUNKS: usize = 16;
const CHUNKS_PER_LEVEL: usize = 24;
const MOST_LEVEL: usize = 10;
/// The depth the scripts' puddles and trails keep the floor under their sprites wet to.
const PUDDLE_FILM: f32 = 0.6;
const TRAIL_FILM: f32 = 0.15;
const MOST_FEED_RADIUS: f32 = 128.0;

/// The pools' volume for one unit of a feeder's `QuantityPerHit` over a second (unverified first guess).
fn pour_per_quantity(kind: FluidType) -> f32 {
    match kind {
        FluidType::Blood | FluidType::BloodyPuke | FluidType::BloodyUrine => 2000.0,
        FluidType::Gas | FluidType::Napalm | FluidType::Urine | FluidType::Puke | FluidType::Gonorrhea => 4000.0,
    }
}

#[derive(Resource)]
pub struct LiquidWorld {
    pub pools: PoolStore,
    spare: f32,
    spills: Vec<Spill>,
}

impl Default for LiquidWorld {
    fn default() -> Self {
        Self { pools: PoolStore::new(FEWEST_CHUNKS + MOST_LEVEL * CHUNKS_PER_LEVEL, BLOOD_BUDGET), spare: 0.0, spills: Vec::new() }
    }
}

pub struct LiquidWorldPlugin;

impl Plugin for LiquidWorldPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LiquidWorld>().add_systems(PostUpdate, step_liquids.after(draw_particles));
    }
}

/// Wets the floor under the flat sprites of the scripts' puddles and trails, topping it up to a film and no more.
fn feed_deposits(drawn: &DrawnParticles, pools: &mut PoolStore, now: f32) {
    for actor in drawn.actors.iter().filter(|actor| actor.drawn && actor.overlay.is_none()) {
        let Some(Liquid { kind, role: role @ (LiquidRole::Puddle | LiquidRole::Trail), .. }) = actor.liquid else { continue };
        let depth = if role == LiquidRole::Puddle { PUDDLE_FILM } else { TRAIL_FILM };
        for sprite in actor.emitters.iter().flat_map(|emitter| &emitter.sprites).filter(|sprite| sprite.color[3] > 0) {
            let (right, up) = (sprite.right, sprite.up);
            if (right.x * up.y - right.y * up.x).abs() < 0.5 * right.size() * up.size() {
                continue;
            }
            let radius = right.size().max(up.size()).min(MOST_FEED_RADIUS);
            let steps = (radius / CELL).ceil() as i32;
            for (i, j) in (-steps..=steps).flat_map(|i| (-steps..=steps).map(move |j| (i, j))) {
                let (dx, dy) = (i as f32 * CELL, j as f32 * CELL);
                if dx * dx + dy * dy <= radius * radius {
                    pools.film(sprite.location + Vector::new(dx, dy, 0.0), kind, depth, now);
                }
            }
        }
    }
}

pub(crate) fn step_liquids(
    settings: Option<Res<Settings>>,
    facts: Res<FrameFacts>,
    drawn: Option<Res<DrawnParticles>>,
    collision: Option<Res<LevelCollision>>,
    mut landings: MessageReader<LiquidLanding>,
    mut liquids: ResMut<LiquidWorld>,
) {
    let level = modern_level(settings.as_deref(), MODERN_LIQUIDS);
    let new_level = collision.as_ref().is_some_and(DetectChanges::is_changed);
    if (level == 0 || !facts.in_play || new_level) && !liquids.pools.chunks.is_empty() {
        liquids.pools.chunks.clear();
    }
    if level == 0 {
        landings.clear();
        return;
    }
    let liquids = &mut *liquids;
    liquids.pools.most_chunks = FEWEST_CHUNKS + usize::from(level) * CHUNKS_PER_LEVEL;
    let now = facts.level_time;
    // A blood feeder's spurts and drips carry its volume as drops; its hits would pour it twice.
    for hit in facts.fluid_hits.iter().filter(|hit| hit.normal.z > 0.5 && hit.kind != FluidType::Blood) {
        liquids.pools.pour(hit.at, hit.kind, hit.quantity * pour_per_quantity(hit.kind) * hit.seconds, now);
    }
    for landing in landings.read().filter(|landing| landing.normal.y > 0.5) {
        let [x, y, z] = to_bevy(landing.at - facts.origin);
        liquids.pools.pour(Vector::new(x, y, z), landing.kind, landing.volume, now);
    }
    if let Some(drawn) = drawn.as_deref() {
        feed_deposits(drawn, &mut liquids.pools, now);
    }
    let none = LevelCollision::default();
    let floor = collision.as_deref().unwrap_or(&none);
    liquids.spare = (liquids.spare + facts.step()).min(FLOW_STEP * MOST_STEPS_PER_FRAME);
    while liquids.spare >= FLOW_STEP {
        liquids.spare -= FLOW_STEP;
        liquids.pools.step(FLOW_STEP, floor, DRYING, now, &mut liquids.spills);
        for spill in liquids.spills.drain(..) {
            liquids.pools.pour(spill.at, spill.kind, spill.volume, now);
        }
    }
}
