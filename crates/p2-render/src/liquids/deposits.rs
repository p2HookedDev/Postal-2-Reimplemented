//! Puddles and trails as our own liquid: each sprite the scripts lay for one becomes a few thin blobs, which the
//! surface draws with the stream, flat on whatever the depth prepass shows under them.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::ecs::prelude::{Commands, DetectChanges, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::math::{Vec2, Vec3};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_resource::{BufferDescriptor, BufferUsages};
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::transform::components::GlobalTransform;
use p2_bevy_assets::to_bevy;
use p2_host::display::{MODERN_LIQUIDS, Settings};
use p2_particles::SpriteParticle;

use super::stream::{PARTICLE_BYTES, liquid_bits};
use super::{LIQUID_DEPOSIT, LIQUID_LANDED, LIQUID_PARTICLE_WORDS, LiquidDeposits};
use crate::frame_facts::FrameFacts;
use crate::gpu_particles::next_unit;
use crate::particle_draw::{ParticleDrawSystems, draw_particles};
use crate::particle_frame::{DrawnParticles, Liquid, LiquidRole};
use crate::screen_pass::{PassView, modern_level, tag_pass_views};

const MOST_BLOBS: usize = 8192;
/// Unreal units between blob centres across a sprite.
const CELL: f32 = 16.0;
const MOST_CELLS_ACROSS: u32 = 6;
/// How far a blob strays from its cell's middle, as a share of the cell.
const STRAY: f32 = 0.2;
/// A blob's radius against its cell's side: wide enough that neighbours still overlap after straying.
const SIZE: [f32; 2] = [0.72, 0.9];

pub struct LiquidDepositsPlugin;

impl Plugin for LiquidDepositsPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        app.add_plugins(ExtractResourcePlugin::<DepositBlobs>::default())
            .init_resource::<DepositBlobs>()
            .add_systems(PostUpdate, gather_deposits.after(draw_particles).after(tag_pass_views).in_set(ParticleDrawSystems));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.add_systems(Render, upload_deposits.in_set(RenderSystems::PrepareResources));
    }
}

/// The frame's blobs as `LiquidParticle`s, in Bevy axes from the world's origin; changed only when they differ.
#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub struct DepositBlobs {
    words: Vec<[u32; LIQUID_PARTICLE_WORDS]>,
}

/// One sprite of a puddle or trail: its middle and half axes in Bevy axes, the scripts' fade and its blobs' seed.
struct Spot {
    distance: f32,
    at: Vec3,
    right: Vec3,
    up: Vec3,
    fade: f32,
    bits: u32,
    seed: u32,
}

#[derive(Default)]
struct Gatherer {
    spots: Vec<Spot>,
    words: Vec<[u32; LIQUID_PARTICLE_WORDS]>,
}

fn spot(sprite: &SpriteParticle, origin: Vec3, eye: Vec3, bits: u32, seed: u32) -> Option<Spot> {
    let fade = f32::from(sprite.color[3]) / 255.0;
    let (right, up) = (Vec3::from(to_bevy(sprite.right)), Vec3::from(to_bevy(sprite.up)));
    if fade <= 0.0 || right.length_squared() <= 0.0 || up.length_squared() <= 0.0 {
        return None;
    }
    let at = Vec3::from(to_bevy(sprite.location)) + origin;
    let [x, y, z] = <[f32; 3]>::from(sprite.location).map(f32::to_bits);
    let seed = seed ^ x.wrapping_mul(0x9E37_79B1) ^ y.rotate_left(11).wrapping_mul(0x85EB_CA77) ^ z.rotate_left(22);
    Some(Spot { distance: eye.distance_squared(at), at, right, up, fade, bits, seed })
}

/// A jittered grid of blobs over the sprite's disc, each blob's radius shrunk by the fade.
fn blobs(spot: &Spot, words: &mut Vec<[u32; LIQUID_PARTICLE_WORDS]>) {
    let across = |axis: Vec3| ((axis.length() * 2.0 / CELL).ceil() as u32).clamp(1, MOST_CELLS_ACROSS);
    let cells = Vec2::new(across(spot.right) as f32, across(spot.up) as f32);
    let side = (spot.right.length() * 2.0 / cells.x).min(spot.up.length() * 2.0 / cells.y);
    let mut seed = spot.seed;
    for row in 0..cells.y as u32 {
        for column in 0..cells.x as u32 {
            let mut unit = || next_unit(&mut seed);
            let stray = (Vec2::new(unit(), unit()) * 2.0 - Vec2::ONE) * STRAY * 2.0 / cells;
            let size = SIZE[0] + (SIZE[1] - SIZE[0]) * unit();
            let middle = (Vec2::new(column as f32, row as f32) + 0.5) / cells * 2.0 - Vec2::ONE;
            if middle.length_squared() > 1.0 {
                continue;
            }
            let place = (middle + stray).clamp_length_max(1.0);
            let at = spot.at + spot.right * place.x + spot.up * place.y;
            let [x, y, z, radius, still] = [at.x, at.y, at.z, side * size * spot.fade, 0.0].map(f32::to_bits);
            words.push([x, y, z, radius, still, still, still, spot.bits]);
        }
    }
}

/// The blobs of every drawn puddle and trail, the sprites nearest `eye` first until `MOST_BLOBS`.
fn gather(drawn: &DrawnParticles, origin: Vec3, eye: Vec3, gatherer: &mut Gatherer) {
    let Gatherer { spots, words } = gatherer;
    spots.clear();
    words.clear();
    for actor in drawn.actors.iter().filter(|actor| actor.drawn && actor.seen && actor.overlay.is_none()) {
        let Some(Liquid { kind, role: role @ (LiquidRole::Puddle | LiquidRole::Trail), burning, .. }) = actor.liquid else { continue };
        let (bits, seed) = (liquid_bits(kind, burning) | LIQUID_LANDED | LIQUID_DEPOSIT, actor.puddle_seed.unwrap_or(0));
        let sprites = actor.emitters.iter().flat_map(|emitter| &emitter.sprites).filter_map(|sprite| spot(sprite, origin, eye, bits, seed));
        if role == LiquidRole::Trail {
            spots.extend(sprites);
            continue;
        }
        // A puddle's sprites lie on one place and only breathe in and out of view: one steady spot, the largest at the
        // strongest fade, or the puddle would pulse.
        let puddle = sprites.reduce(|a, b| {
            let fade = a.fade.max(b.fade);
            Spot { fade, ..if b.right.length_squared() > a.right.length_squared() { b } else { a } }
        });
        spots.extend(puddle);
    }
    spots.sort_unstable_by(|a, b| a.distance.total_cmp(&b.distance));
    for spot in spots.iter() {
        if words.len() >= MOST_BLOBS {
            break;
        }
        blobs(spot, words);
    }
    words.truncate(MOST_BLOBS);
}

fn gather_deposits(
    settings: Option<Res<Settings>>,
    facts: Res<FrameFacts>,
    drawn: Option<Res<DrawnParticles>>,
    views: Query<&GlobalTransform, With<PassView>>,
    mut gatherer: Local<Gatherer>,
    mut deposits: ResMut<DepositBlobs>,
) {
    let level = modern_level(settings.as_deref(), MODERN_LIQUIDS);
    let eye = views.iter().next().map(GlobalTransform::translation);
    match (drawn.as_deref(), eye.filter(|_| level > 0)) {
        (Some(drawn), Some(eye)) => gather(drawn, facts.origin, eye, &mut gatherer),
        _ => gatherer.words.clear(),
    }
    if deposits.words != gatherer.words {
        std::mem::swap(&mut deposits.words, &mut gatherer.words);
    }
}

fn upload_deposits(
    mut commands: Commands,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    blobs: Option<Res<DepositBlobs>>,
    uploaded: Option<Res<LiquidDeposits>>,
    mut bytes: Local<Vec<u8>>,
) {
    let Some(blobs) = blobs.filter(|blobs| blobs.is_changed()) else {
        return;
    };
    let count = u32::try_from(blobs.words.len()).unwrap_or(u32::MAX);
    if count == 0 {
        if uploaded.is_some() {
            commands.remove_resource::<LiquidDeposits>();
        }
        return;
    }
    let buffer = uploaded.map_or_else(
        || {
            device.create_buffer(&BufferDescriptor {
                label: Some("liquid deposits"),
                size: MOST_BLOBS as u64 * PARTICLE_BYTES,
                usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        },
        |uploaded| uploaded.buffer.clone(),
    );
    bytes.clear();
    bytes.extend(blobs.words.iter().flatten().flat_map(|word| word.to_le_bytes()));
    queue.write_buffer(&buffer, 0, &bytes);
    commands.insert_resource(LiquidDeposits { buffer, count });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::liquids::{LIQUID_BURNING, LIQUID_KIND_MASK};
    use crate::particle_frame::{DrawnActor, DrawnEmitter};
    use p2_math::Vector;
    use p2_vm::FluidType;

    fn sprite(at: [f32; 3], half: f32, alpha: u8) -> SpriteParticle {
        let location: Vector = at.into();
        let (up, right) = ([0.0, half, 0.0].into(), [half, 0.0, 0.0].into());
        SpriteParticle { location, previous: location, size: [half * 2.0; 2], up, right, color: [255, 255, 255, alpha], uv: [0.0; 4], blend: None, serial: 0 }
    }

    fn lying(role: LiquidRole, sprites: Vec<SpriteParticle>) -> DrawnActor {
        let liquid = Liquid { kind: FluidType::Blood, role, burning: true, pour: None };
        DrawnActor { drawn: true, seen: true, puddle_seed: Some(7), liquid: Some(liquid), emitters: vec![DrawnEmitter { sprites, ..Default::default() }], ..DrawnActor::default() }
    }

    fn gathered(actors: Vec<DrawnActor>) -> Vec<[u32; LIQUID_PARTICLE_WORDS]> {
        let mut gatherer = Gatherer::default();
        gather(&DrawnParticles { actors, ..DrawnParticles::default() }, Vec3::ZERO, Vec3::ZERO, &mut gatherer);
        gatherer.words
    }

    fn float(words: &[u32; LIQUID_PARTICLE_WORDS], word: usize) -> f32 {
        f32::from_bits(words[word])
    }

    #[test]
    fn a_sprite_lays_a_grid_of_lying_blobs_of_its_kind() {
        let words = gathered(vec![lying(LiquidRole::Puddle, vec![sprite([0.0; 3], 24.0, 255)])]);
        assert_eq!(words.len(), 9, "three cells across, every middle inside the disc");
        for blob in &words {
            assert_eq!(blob[7], FluidType::Blood as u32 & LIQUID_KIND_MASK | LIQUID_BURNING | LIQUID_LANDED | LIQUID_DEPOSIT);
            assert_eq!(blob[4..7], [0; 3], "a deposit lies still");
        }
        assert_eq!(gathered(vec![lying(LiquidRole::Trail, vec![sprite([0.0; 3], 48.0, 255)])]).len(), 32, "six across less the corners");
        assert_eq!(gathered(vec![lying(LiquidRole::Puddle, vec![sprite([0.0; 3], 4.0, 255)])]).len(), 1);
    }

    #[test]
    fn only_drawn_puddles_and_trails_lay_blobs() {
        let mut hidden = lying(LiquidRole::Puddle, vec![sprite([0.0; 3], 24.0, 255)]);
        hidden.drawn = false;
        let stream = lying(LiquidRole::Stream, vec![sprite([0.0; 3], 24.0, 255)]);
        let splash = lying(LiquidRole::Splash, vec![sprite([0.0; 3], 24.0, 255)]);
        assert!(gathered(vec![hidden, stream, splash]).is_empty());
    }

    #[test]
    fn the_fade_shrinks_the_blobs_and_a_faded_sprite_lays_none() {
        let full = gathered(vec![lying(LiquidRole::Puddle, vec![sprite([0.0; 3], 24.0, 255)])]);
        let half = gathered(vec![lying(LiquidRole::Puddle, vec![sprite([0.0; 3], 24.0, 51)])]);
        for (full, half) in full.iter().zip(&half) {
            assert!((float(half, 3) / float(full, 3) - 0.2).abs() < 1e-4);
            assert_eq!(full[..3], half[..3], "the fade keeps the blobs in place");
        }
        assert!(gathered(vec![lying(LiquidRole::Puddle, vec![sprite([0.0; 3], 24.0, 0)])]).is_empty());
    }

    #[test]
    fn blobs_stay_inside_their_sprite_and_overlap_into_one_film() {
        let half = 40.0;
        let sprites: Vec<_> = (0..20).map(|n| sprite([n as f32 * 300.0, 50.0, 10.0], half, 255)).collect();
        let words = gathered(vec![lying(LiquidRole::Puddle, sprites)]);
        assert!(!words.is_empty());
        for blob in &words {
            let at = Vec3::new(float(blob, 0), float(blob, 1), float(blob, 2));
            let middle = Vec3::new((at.x / 300.0).round() * 300.0, 10.0, 50.0);
            assert!(at.distance(middle) <= half + 1e-3, "{at} strays outside {middle}");
            assert!((at.y - 10.0).abs() < 1e-4, "Unreal up is Bevy y");
            assert!(float(blob, 3) >= SIZE[0] * half * 2.0 / 5.0 - 1e-3, "neighbours overlap after straying");
        }
        assert_eq!(words, gathered(vec![lying(LiquidRole::Puddle, (0..20).map(|n| sprite([n as f32 * 300.0, 50.0, 10.0], half, 255)).collect())]), "the same every frame");
        let mut reseeded = lying(LiquidRole::Puddle, vec![sprite([0.0, 50.0, 10.0], half, 255)]);
        reseeded.puddle_seed = Some(8);
        assert_ne!(gathered(vec![reseeded])[..], words[..21], "another puddle strays another way");
    }

    #[test]
    fn a_puddle_is_one_steady_spot_whatever_its_sprites_fade() {
        let breathing = |fades: [u8; 3]| gathered(vec![lying(LiquidRole::Puddle, fades.map(|fade| sprite([0.0; 3], 24.0, fade)).to_vec())]);
        assert_eq!(breathing([255, 40, 120]), breathing([30, 255, 90]));
        assert_eq!(breathing([255, 40, 120]), gathered(vec![lying(LiquidRole::Puddle, vec![sprite([0.0; 3], 24.0, 255)])]));
    }

    #[test]
    fn the_nearest_sprites_are_kept_up_to_the_cap() {
        let sprites: Vec<_> = (0..1200).rev().map(|n| sprite([n as f32 * 100.0, 0.0, 0.0], 24.0, 255)).collect();
        let words = gathered(vec![lying(LiquidRole::Trail, sprites)]);
        assert_eq!(words.len(), MOST_BLOBS);
        let farthest = words.iter().map(|blob| float(blob, 0)).fold(0.0, f32::max);
        assert!(farthest < (MOST_BLOBS / 9 + 1) as f32 * 100.0, "{farthest}");
    }
}
