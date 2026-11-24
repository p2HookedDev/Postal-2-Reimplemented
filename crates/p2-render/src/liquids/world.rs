//! The world's liquids on the main world: the scripts' puddles and trails hold the floor wet under them and landing
//! drops add to it; the layer steps on the level's clock.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::Assets;
use bevy::ecs::message::MessageReader;
use bevy::ecs::observer::On;
use bevy::ecs::prelude::{Commands, DetectChanges, Res, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::render::RenderApp;
use bevy::render::extract_resource::ExtractResourcePlugin;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::storage::ShaderBuffer;
use p2_bevy_assets::to_bevy;
use p2_host::display::{MODERN_LIQUIDS, Settings};
use p2_math::Vector;
use p2_particles::SpriteParticle;
use p2_vm::FluidHit;

use super::LiquidLanding;
use super::layer::{Feed, Feeding, LayerFrame, Level, LiquidLayer, STEP, TEXEL, capacity, film_of};
use super::run_draw::RUN_ROWS;
use super::runs::{Runs, STEEP, WallHit};
use crate::frame_facts::FrameFacts;
use crate::level_collision::LevelCollision;
use crate::particle_draw::draw_particles;
use crate::particle_frame::{DrawnParticles, Liquid, LiquidRole};
use crate::screen_pass::modern_level;

const MOST_STEPS: u32 = 4;
/// The share of a puddle or splat sprite's half side its picture covers (unverified).
const SPRITE_FILL: f32 = 0.7;
const MOST_FEED_RADIUS: f32 = 280.0;
/// A landing drop's volume spreads over at least this radius, in texels.
const DROP_TEXELS: f32 = 1.5;
/// Where a stream lands it piles a little liquid each second over this many texels, which runs off: the pool under a
/// stream moves. Small against drying, so it hardly grows the scripts' pool.
const IMPACT_VOLUME: f32 = 30.0;
const IMPACT_TEXELS: f32 = 2.0;
/// A drop that lands on a wall splats with this many times its volume: what it carries is only seen.
const SPLAT_VOLUME: f32 = 6.0;

/// A lip's run starts with this many times what the pools counted over it in a frame: its liquid is only seen, as
/// the pools carry the volume down.
const LIP_VOLUME: f32 = 10.0;

/// Liquid on the walls.
#[derive(Resource, Default)]
pub struct LiquidRuns(pub Runs);

/// The newest spill counts read back from the GPU; `fresh` until they are taken.
#[derive(Resource, Default)]
struct Spilled {
    counts: Vec<u8>,
    fresh: bool,
}

/// The runs drawn at a level of the Liquids slider.
fn run_cap(level: u8) -> usize {
    (8 + 5 * usize::from(level)).min(RUN_ROWS)
}

pub struct LiquidWorldPlugin;

impl Plugin for LiquidWorldPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LiquidLayer>()
            .init_resource::<LayerFrame>()
            .init_resource::<LiquidRuns>()
            .init_resource::<Spilled>()
            .add_systems(PostUpdate, step_liquids.after(draw_particles));
        // Spills are counted on the GPU and read back: only where something renders.
        if app.get_sub_app(RenderApp).is_some() {
            app.add_plugins(ExtractResourcePlugin::<LayerFrame>::default()).add_systems(PostUpdate, spill_runs.after(step_liquids));
        }
    }
}

/// The floor under the scripts' puddles and trails, held at the film while their sprites show and as their fade
/// thins it: the scripts keep their size and their life.
fn deposits(drawn: &DrawnParticles, feeds: &mut Vec<Feeding>) {
    for actor in drawn.actors.iter().filter(|actor| actor.drawn && actor.overlay.is_none()) {
        let Some(Liquid { kind, role: role @ (LiquidRole::Puddle | LiquidRole::Trail), .. }) = actor.liquid else { continue };
        let lying = actor.emitters.iter().flat_map(|emitter| &emitter.sprites).filter(|sprite| {
            let (right, up) = (sprite.right, sprite.up);
            sprite.color[3] > 0 && (right.x * up.y - right.y * up.x).abs() >= 0.5 * right.size() * up.size()
        });
        let fed = |sprite: &SpriteParticle| Feeding {
            at: sprite.location,
            radius: (sprite.right.size().max(sprite.up.size()) * SPRITE_FILL).min(MOST_FEED_RADIUS),
            kind,
            feed: Feed::Hold(film_of(kind) * f32::from(sprite.color[3]) / 255.0),
        };
        if role == LiquidRole::Trail {
            feeds.extend(lying.map(fed));
            continue;
        }
        // A puddle's sprites breathe in turn over one spot: the widest of them is the puddle, as strong as the most opaque.
        let Some(widest) = lying.clone().map(fed).max_by(|a, b| a.radius.total_cmp(&b.radius)) else { continue };
        let shown = lying.map(|sprite| f32::from(sprite.color[3]) / 255.0).fold(0.0, f32::max);
        feeds.push(Feeding { feed: Feed::Hold(film_of(kind) * shown), ..widest });
    }
}

/// What a stream's landing on a floor piles there.
fn impact(hit: &FluidHit) -> Option<Feeding> {
    (hit.normal.z > 0.5).then_some(Feeding { at: hit.at, radius: IMPACT_TEXELS * TEXEL, kind: hit.kind, feed: Feed::Add(IMPACT_VOLUME * hit.seconds) })
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(crate) fn step_liquids(
    settings: Option<Res<Settings>>,
    facts: Res<FrameFacts>,
    drawn: Option<Res<DrawnParticles>>,
    collision: Option<Res<LevelCollision>>,
    mut landings: MessageReader<LiquidLanding>,
    mut layer: ResMut<LiquidLayer>,
    mut frame: ResMut<LayerFrame>,
    mut runs: ResMut<LiquidRuns>,
    mut images: ResMut<Assets<Image>>,
    mut commands: Commands,
) {
    let level = modern_level(settings.as_deref(), MODERN_LIQUIDS);
    let new_level = collision.as_ref().is_some_and(DetectChanges::is_changed);
    let wanted = if level == 0 || !facts.in_play { 0 } else { capacity(level) };
    if new_level || wanted != frame.capacity {
        layer.reset(&mut commands, wanted);
        runs.0 = Runs::default();
    }
    if wanted == 0 {
        landings.clear();
        *frame = LayerFrame { level: frame.level, ..LayerFrame::default() };
        return;
    }
    let mut feeds = Vec::new();
    if let Some(drawn) = drawn.as_deref() {
        deposits(drawn, &mut feeds);
    }
    let (now, cap) = (facts.level_time, run_cap(level));
    feeds.extend(facts.fluid_hits.iter().filter_map(impact));
    for hit in facts.fluid_hits.iter().filter(|hit| hit.normal.z.abs() < STEEP) {
        runs.0.stream(hit.feeder, WallHit { at: hit.at, normal: hit.normal, kind: hit.kind }, hit.seconds, now, cap);
    }
    for landing in landings.read() {
        let at = Vector::from(to_bevy(landing.at - facts.origin));
        let normal = Vector::from(to_bevy(landing.normal));
        if normal.z > 0.5 {
            feeds.push(Feeding { at, radius: DROP_TEXELS * TEXEL, kind: landing.kind, feed: Feed::Add(landing.volume) });
        } else if normal.z.abs() < STEEP {
            runs.0.splat(WallHit { at, normal, kind: landing.kind }, landing.volume * SPLAT_VOLUME, now, cap);
        }
    }
    if let Some(surfaces) = collision.as_deref().and_then(|collision| collision.0.as_deref()) {
        let mut pours = Vec::new();
        runs.0.step(facts.step(), surfaces, now, &mut pours);
        feeds.extend(pours.into_iter().map(|pour| Feeding { at: pour.at, radius: DROP_TEXELS * TEXEL, kind: pour.kind, feed: Feed::Add(pour.volume) }));
    }
    let steps = layer.steps(facts.step(), STEP, MOST_STEPS);
    let level = Level { collision: collision.as_deref().and_then(|collision| collision.0.as_ref()), origin: facts.origin, now: facts.level_time };
    layer.frame(&feeds, level, steps, &mut images, &mut commands, &mut frame);
}

/// Runs down the faces under the lips the pools spilt over, read back from the GPU a few frames late.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn spill_runs(
    settings: Option<Res<Settings>>,
    facts: Res<FrameFacts>,
    mut layer: ResMut<LiquidLayer>,
    mut frame: ResMut<LayerFrame>,
    mut spilled: ResMut<Spilled>,
    mut runs: ResMut<LiquidRuns>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut commands: Commands,
) {
    if frame.capacity == 0 {
        return;
    }
    if let Some(made) = layer.spill_buffer(&mut buffers, &mut frame) {
        commands.spawn(Readback::buffer(made)).observe(|event: On<ReadbackComplete>, mut spilled: ResMut<Spilled>| {
            spilled.counts.clone_from(&event.data);
            spilled.fresh = true;
        });
    }
    if !std::mem::take(&mut spilled.fresh) {
        return;
    }
    let cap = run_cap(modern_level(settings.as_deref(), MODERN_LIQUIDS));
    for lip in layer.lips(&spilled.counts, facts.origin.y) {
        let hit = WallHit { at: lip.at + lip.way * 0.05 - Vector::new(0.0, 0.0, 0.3), normal: lip.way, kind: lip.kind };
        runs.0.spill(hit, lip.volume * LIP_VOLUME, facts.level_time, cap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2_vm::{FluidType, ObjectId};

    #[test]
    fn a_stream_landing_on_a_floor_piles_liquid_and_one_on_a_wall_none() {
        let hit = |normal: Vector| FluidHit { feeder: ObjectId::from_index(1), kind: FluidType::Urine, at: Vector::new(1.0, 2.0, 3.0), normal, seconds: 0.5, quantity: 1.0 };
        let floor = impact(&hit(Vector::new(0.0, 0.0, 1.0)));
        assert_eq!(floor.map(|feeding| (feeding.at, feeding.feed)), Some((Vector::new(1.0, 2.0, 3.0), Feed::Add(IMPACT_VOLUME * 0.5))));
        assert!(impact(&hit(Vector::new(1.0, 0.0, 0.0))).is_none());
    }
}
