//! The world's liquids on the main world: the scripts' puddles and trail splats hold the surface they lie on wet
//! and landing drops add to it; the layer steps on the level's clock.

use std::f32::consts::PI;

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::Assets;
use bevy::ecs::message::MessageReader;
use bevy::ecs::prelude::{Commands, DetectChanges, Res, ResMut};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::render::RenderApp;
use bevy::render::extract_resource::ExtractResourcePlugin;
use p2_bevy_assets::to_bevy;
use p2_host::display::{MODERN_LIQUIDS, NORMAL_FLUID_DETAIL, Settings};
use p2_math::Vector;
use p2_particles::SpriteParticle;
use p2_vm::FluidHit;

use super::LiquidLanding;
use super::layer::{FILM_LIFE, Feed, Feeding, LayerFrame, Level, LiquidLayer, STEP, TEXEL, capacity, film_of, fluid_life};
use crate::frame_facts::FrameFacts;
use crate::level_collision::LevelCollision;
use crate::particle_draw::draw_particles;
use crate::particle_frame::{DrawnParticles, Liquid, LiquidRole};
use crate::screen_pass::modern_level;

const MOST_STEPS: u32 = 4;
/// The share of a puddle or splat sprite's half side its picture reaches at the most (unverified).
const SPRITE_FILL: f32 = 0.85;
const MOST_FEED_RADIUS: f32 = 280.0;
const DROP_TEXELS: f32 = 1.5;
/// Where a stream lands it piles a little liquid each second over this many texels. Small against drying, so it
/// hardly grows the scripts' splats.
const IMPACT_VOLUME: f32 = 30.0;
const IMPACT_TEXELS: f32 = 2.0;

pub struct LiquidWorldPlugin;

impl Plugin for LiquidWorldPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LiquidLayer>().init_resource::<LayerFrame>().add_systems(PostUpdate, step_liquids.after(draw_particles));
        if app.get_sub_app(RenderApp).is_some() {
            app.add_plugins(ExtractResourcePlugin::<LayerFrame>::default());
        }
    }
}

fn face_of(normal: Vector) -> Option<Vector> {
    (normal.z <= 0.5).then_some(normal)
}

/// The surfaces under the scripts' puddles and trail splats, held at the film while their sprites show and as
/// their fade thins it: the scripts keep their size, place and life. A splat lies on the face the scripts fitted
/// it to.
fn deposits(drawn: &DrawnParticles, feeds: &mut Vec<Feeding>) {
    for actor in drawn.actors.iter().filter(|actor| actor.drawn && actor.overlay.is_none()) {
        let Some(Liquid { kind, role: role @ (LiquidRole::Puddle | LiquidRole::Trail | LiquidRole::Flow), burning, .. }) = actor.liquid else { continue };
        let splats = actor
            .emitters
            .iter()
            .filter_map(|emitter| Some((emitter.shape.facing.filter(|facing| *facing != Vec3::ZERO)?, &emitter.sprites)))
            .flat_map(|(facing, sprites)| sprites.iter().map(move |sprite| (sprite, Vector::from(facing.to_array()))))
            .filter(|(sprite, normal)| sprite.color[3] > 0 && sprite.right.cross(sprite.up).dot(*normal).abs() >= 0.5 * sprite.right.size() * sprite.up.size());
        let fed = |(sprite, normal): (&SpriteParticle, Vector)| Feeding {
            at: sprite.location,
            radius: (sprite.right.size().max(sprite.up.size()) * SPRITE_FILL).min(MOST_FEED_RADIUS),
            kind,
            feed: Feed::Hold(film_of(kind) * f32::from(sprite.color[3]) / 255.0),
            burning,
            face: face_of(normal),
        };
        match role {
            LiquidRole::Trail => feeds.extend(splats.map(fed)),
            // A flow trail is the scripts' slide down a wall under a still aim: only where it reaches a floor is drawn.
            LiquidRole::Flow => feeds.extend(splats.map(fed).filter(|feeding| feeding.face.is_none())),
            _ => {
                // A puddle's sprites breathe in turn over one spot: the widest of them is the puddle, as strong as the
                // most opaque.
                let floor = splats.filter(|(_, normal)| face_of(*normal).is_none());
                let Some(widest) = floor.clone().map(fed).max_by(|a, b| a.radius.total_cmp(&b.radius)) else { continue };
                let shown = floor.map(|(sprite, _)| f32::from(sprite.color[3]) / 255.0).fold(0.0, f32::max);
                feeds.push(Feeding { feed: Feed::Hold(film_of(kind) * shown), ..widest });
            }
        }
    }
}

fn impact(hit: &FluidHit) -> Feeding {
    Feeding { at: hit.at, radius: IMPACT_TEXELS * TEXEL, kind: hit.kind, feed: Feed::Add(IMPACT_VOLUME * hit.seconds), burning: false, face: face_of(hit.normal) }
}

fn landing_radius(landing: &LiquidLanding) -> f32 {
    (landing.volume / (PI * film_of(landing.kind))).sqrt().max(DROP_TEXELS * TEXEL)
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
    mut images: ResMut<Assets<Image>>,
    mut commands: Commands,
) {
    let level = modern_level(settings.as_deref(), MODERN_LIQUIDS);
    let new_level = collision.as_ref().is_some_and(DetectChanges::is_changed);
    let wanted = if level == 0 || !facts.in_play { 0 } else { capacity(level) };
    if new_level || wanted != frame.capacity {
        layer.reset(&mut commands, wanted);
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
    feeds.extend(facts.fluid_hits.iter().map(impact));
    for landing in landings.read() {
        let at = Vector::from(to_bevy(landing.at - facts.origin));
        let normal = Vector::from(to_bevy(landing.normal));
        feeds.push(Feeding { at, radius: landing_radius(landing), kind: landing.kind, feed: Feed::Add(landing.volume), burning: false, face: face_of(normal) });
    }
    let steps = layer.steps(facts.step(), STEP, MOST_STEPS);
    frame.film_life = fluid_life(settings.as_deref().map_or(NORMAL_FLUID_DETAIL, Settings::fluid_detail), FILM_LIFE);
    let level = Level { collision: collision.as_deref().and_then(|collision| collision.0.as_ref()), origin: facts.origin, now: facts.level_time };
    layer.frame(&feeds, level, steps, &mut images, &mut commands, &mut frame);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::particle_frame::{DrawnActor, DrawnEmitter, EmitterShape};
    use p2_vm::{FluidType, ObjectId};

    #[test]
    fn a_stream_landing_piles_liquid_on_a_floor_a_wall_and_a_ceiling() {
        let hit = |normal: Vector| FluidHit { feeder: ObjectId::from_index(1), kind: FluidType::Urine, at: Vector::new(1.0, 2.0, 3.0), normal, seconds: 0.5, quantity: 1.0 };
        let floor = impact(&hit(Vector::new(0.0, 0.0, 1.0)));
        assert_eq!((floor.at, floor.feed, floor.face), (Vector::new(1.0, 2.0, 3.0), Feed::Add(IMPACT_VOLUME * 0.5), None));
        let wall = Vector::new(1.0, 0.0, 0.0);
        assert_eq!(impact(&hit(wall)).face, Some(wall));
        let ceiling = Vector::new(0.0, 0.0, -1.0);
        assert_eq!(impact(&hit(ceiling)).face, Some(ceiling));
    }

    #[test]
    fn a_landing_reaches_the_film_at_its_middle() {
        let landing = |volume: f32| LiquidLanding { at: Vec3::ZERO, normal: Vec3::Y, kind: FluidType::Urine, volume, velocity: Vec3::ZERO };
        assert_eq!(landing_radius(&landing(0.01)), DROP_TEXELS * TEXEL);
        let radius = landing_radius(&landing(400.0));
        assert!((400.0 / (PI * radius * radius) - film_of(FluidType::Urine)).abs() < 1e-4);
    }

    #[test]
    fn splats_feed_the_face_they_lie_on_and_a_flow_trail_only_floors() {
        let sprite = |x: f32, [right, up]: [Vector; 2]| SpriteParticle {
            location: Vector::new(x, 0.0, 0.0),
            previous: Vector::new(x, 0.0, 0.0),
            size: [10.0; 2],
            up,
            right,
            color: [255; 4],
            uv: [0.0, 0.0, 1.0, 1.0],
            blend: None,
            serial: 0,
        };
        let (flat, on_wall, upright) = (
            [Vector::new(10.0, 0.0, 0.0), Vector::new(0.0, 10.0, 0.0)],
            [Vector::new(0.0, 10.0, 0.0), Vector::new(0.0, 0.0, 10.0)],
            [Vector::new(10.0, 0.0, 0.0), Vector::new(0.0, 0.0, 10.0)],
        );
        let emitter = |facing: Option<Vec3>, x: f32, axes: [Vector; 2]| DrawnEmitter { sprites: vec![sprite(x, axes)], shape: EmitterShape { facing, ..EmitterShape::default() }, ..DrawnEmitter::default() };
        let actor = |role: LiquidRole| DrawnActor {
            drawn: true,
            liquid: Some(Liquid { kind: FluidType::Urine, role, burning: false, pour: None }),
            emitters: vec![
                emitter(Some(Vec3::Z), 1.0, flat),
                emitter(Some(Vec3::NEG_X), 2.0, on_wall),
                emitter(Some(Vec3::NEG_Z), 3.0, flat),
                emitter(None, 4.0, flat),
                emitter(Some(Vec3::NEG_Z), 5.0, upright),
            ],
            ..DrawnActor::default()
        };
        let fed = |role: LiquidRole| {
            let mut feeds = Vec::new();
            deposits(&DrawnParticles { actors: vec![actor(role)], ..DrawnParticles::default() }, &mut feeds);
            feeds.iter().map(|feeding| (feeding.at.x, feeding.face)).collect::<Vec<_>>()
        };
        assert_eq!(fed(LiquidRole::Trail), [(1.0, None), (2.0, Some(Vector::new(-1.0, 0.0, 0.0))), (3.0, Some(Vector::new(0.0, 0.0, -1.0)))]);
        assert_eq!(fed(LiquidRole::Flow), [(1.0, None)]);
        assert_eq!(fed(LiquidRole::Puddle), [(1.0, None)]);
        assert!(fed(LiquidRole::Stream).is_empty());
    }
}
