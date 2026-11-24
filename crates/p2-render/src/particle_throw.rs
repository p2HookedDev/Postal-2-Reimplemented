//! Modern graphics: what a newborn sprite of the scripts' chunks, shards and sparks throws. A chunk throws droplets
//! for the GPU pool; a shard, a spark and a piece of skull or brain throw rigid pieces instead. A blood emitter's first
//! burst also throws a mist puff, fast streaks and a few heavy drops that fly as liquid.

use bevy::asset::Handle;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::Resource;
use bevy::image::Image;
use bevy::math::{Vec3, Vec4};
use p2_bevy_assets::to_bevy;
use p2_math::Vector;
use p2_particles::{DirectionUsage, DrawStyle, EmitterProperties, SpriteParticle};
use p2_vm::FluidType;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::debris::{PieceBirth, PieceKind};
use crate::gpu_particles::{Droplet, MIST, STREAK, TEXTURE_SLOTS, UNPUSHED, pool_size};
use crate::liquids::LiquidSpray;
use crate::particle_frame::DrawnParticles;

/// Past this many emitters the ones whose actor is gone are dropped.
const SOURCES_KEPT: usize = 4096;
/// A droplet lives its kind's `seconds` times between `LIFE_LEAST` and `LIFE_LEAST + LIFE_SPREAD`.
const LIFE_LEAST: f32 = 0.7;
const LIFE_SPREAD: f32 = 0.6;
/// A droplet's gravity, in Unreal units a second squared. How a chunk lands matches `RESTITUTION` and `KEEP` in
/// `gpu_particles.wesl`.
const DROPLET_GRAVITY: f32 = 800.0;

/// Unreal units in a metre (unverified).
const METRE: f32 = 52.5;
/// Newborn sprites in a blood burst that make it as big a hit as the spray gets.
const BIG_HIT: f32 = 15.0;
/// A burst whose sprites' mean velocity is less than this share of their mean speed has no way: it is thrown all round.
const RADIAL: f32 = 0.35;
/// Ranges below go from a small hit to a big one, or from least to most.
const MISTS: [f32; 2] = [1.0, 2.0];
const RADIAL_MISTS: [f32; 2] = [2.0, 4.0];
const MIST_SECONDS: [f32; 2] = [0.24, 0.3];
/// A puff's half size at birth; the shader grows it fourfold.
const MIST_HALF: f32 = 0.05 * METRE;
const MIST_DRIFT: f32 = 0.5 * METRE;
const STREAKS: [f32; 2] = [8.0, 40.0];
const RADIAL_STREAKS: [f32; 2] = [60.0, 120.0];
/// Half the cone's angle in degrees.
const CONE: [f32; 2] = [15.0, 10.0];
const SMALL_HIT_SPEED: [f32; 2] = [2.0 * METRE, 5.0 * METRE];
const BIG_HIT_SPEED: [f32; 2] = [4.0 * METRE, 9.0 * METRE];
const STREAK_SECONDS: [f32; 2] = [0.4, 0.8];
const STREAK_HALF: [f32; 2] = [0.25, 0.5];
const HEAVY: [f32; 2] = [1.0, 4.0];
const RADIAL_HEAVY: [f32; 2] = [5.0, 12.0];
/// A heavy drop's speed against a streak's.
const HEAVY_SPEED: [f32; 2] = [0.4, 0.7];
const HEAVY_RADIUS: [f32; 2] = [0.6, 1.2];
/// Thin blood's linear colour, before the sprite's tint and light.
const BLOOD: Vec3 = Vec3::new(0.35, 0.02, 0.015);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Throws {
    /// A piece of blood, dirt, concrete, bone or kevlar: a spinning cell of its texture in the GPU pool.
    Droplets,
    Pieces(PieceKind),
}

const PIECE_WORDS: [(&str, Throws); 6] = [
    ("glassshards", Throws::Pieces(PieceKind::Flat)),
    ("plywoodparts", Throws::Pieces(PieceKind::Flat)),
    ("skullchunks", Throws::Pieces(PieceKind::Lump)),
    ("brainchunks", Throws::Pieces(PieceKind::Lump)),
    ("chunk", Throws::Droplets),
    ("concrete", Throws::Droplets),
];
const SPARK_WORD: &str = "softwhitedot";
const FUSE_WORD: &str = "sparkler";
const SPARK: Throws = Throws::Pieces(PieceKind::Spark);

/// Told by the texture's name and how the scripts draw it: their breakage is glass shards and plywood parts, their
/// chunk textures hold `chunk` or `concrete`, both blended by their alpha; their sparks are the soft white dot drawn
/// along its motion (the same dot facing the eye is a glow or a trail) and a fuse's sparkler. A piece drawn flat on a
/// surface already lies where it fell and throws nothing.
fn detail_of(texture: &str, props: &EmitterProperties) -> Option<Throws> {
    let texture = texture.to_ascii_lowercase();
    if texture.contains(FUSE_WORD) {
        return Some(SPARK);
    }
    if texture.contains(SPARK_WORD) {
        return (props.use_direction_as != DirectionUsage::None).then_some(SPARK);
    }
    let lies = matches!(props.use_direction_as, DirectionUsage::Normal | DirectionUsage::UpAndNormal | DirectionUsage::RightAndNormal);
    let blended = matches!(props.draw_style, DrawStyle::AlphaBlend | DrawStyle::AlphaModulate);
    if !blended || lies {
        return None;
    }
    PIECE_WORDS.iter().find(|(word, _)| texture.contains(word)).map(|&(_, throws)| throws)
}

#[derive(Clone, Copy)]
struct Throw {
    per_level: f32,
    /// Unreal units a second, when the sprite's own motion is not known.
    speed: f32,
    seconds: f32,
    /// The share of the sprite's half size a piece takes, least and most.
    size: [f32; 2],
    /// The largest half size, in Unreal units.
    most: f32,
    /// The share of the sprite's speed a piece takes, least and most.
    fling: [f32; 2],
}

const CHUNK: Throw = Throw { per_level: 0.6, speed: 160.0, seconds: 8.0, size: [0.2, 0.45], most: 3.0, fling: [0.5, 1.2] };

/// The longest a droplet in the pool can live: only chunks reach it.
pub(crate) const LONGEST_DROPLET_LIFE: f32 = CHUNK.seconds * (LIFE_LEAST + LIFE_SPREAD);

fn throw_of(throws: Throws) -> Throw {
    match throws {
        Throws::Droplets => CHUNK,
        Throws::Pieces(PieceKind::Flat) => Throw { per_level: 0.8, speed: 200.0, seconds: 30.0, size: [0.5, 1.0], most: 6.0, fling: [0.5, 1.2] },
        Throws::Pieces(PieceKind::Spark) => Throw { per_level: 0.8, speed: 300.0, seconds: 1.0, size: [0.4, 0.7], most: 3.0, fling: [0.5, 1.2] },
        Throws::Pieces(PieceKind::Lump | PieceKind::Chunk) => Throw { per_level: 0.5, seconds: 15.0, size: [0.4, 0.8], most: 7.0, fling: [0.8, 1.3], ..CHUNK },
    }
}

fn droplets_per_sprite(level: u8, throws: Throws) -> u32 {
    (f32::from(level) * throw_of(throws).per_level).ceil() as u32
}

#[derive(Clone, Copy)]
struct Source {
    throws: Throws,
    slot: u32,
    blood: bool,
}

#[derive(Resource, Default)]
pub(crate) struct Spawner {
    map: u32,
    sources: FxHashMap<(Entity, u32), Source>,
    /// The highest spawn serial seen of each emitter: a sprite past it is newborn.
    seen: FxHashMap<(Entity, u32), u32>,
    /// The textures the droplets are drawn with, by slot; an emitter whose texture finds no slot throws nothing.
    pub(crate) textures: Vec<Handle<Image>>,
    /// Heavy drops thrown since the caller last took them.
    pub(crate) sprays: Vec<LiquidSpray>,
    seed: u32,
}

pub(crate) fn next_unit(seed: &mut u32) -> f32 {
    let mut x = (*seed).max(1);
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *seed = x;
    (x >> 8) as f32 / 16_777_216.0
}

/// Where a piece thrown from `sprite` starts, in Bevy axes before the world's origin, its velocity and its half size.
/// `step` is the game's seconds this frame: the sprite's first move over it is its speed.
fn thrown(sprite: &SpriteParticle, throw: &Throw, step: f32, seed: &mut u32) -> (Vec3, Vec3, f32) {
    let here = Vec3::from(to_bevy(sprite.location));
    let moved = here - Vec3::from(to_bevy(sprite.previous));
    let jitter = Vec3::new(next_unit(seed), next_unit(seed), next_unit(seed)) * 2.0 - Vec3::ONE;
    let own = if step > 1e-4 { moved / step } else { Vec3::ZERO };
    let own = if own.length() > 20.0 { own.clamp_length_max(900.0) } else { (Vec3::Y + jitter).normalize_or(Vec3::Y) * throw.speed };
    let velocity = own * (throw.fling[0] + (throw.fling[1] - throw.fling[0]) * next_unit(seed)) + jitter * 0.35 * own.length();
    let at = here + jitter * (sprite.size[0] * 0.3).min(4.0);
    let half = (sprite.size[0] * (throw.size[0] + (throw.size[1] - throw.size[0]) * next_unit(seed))).clamp(0.3, throw.most);
    (at, velocity, half)
}

fn sprite_rgb(sprite: &SpriteParticle) -> Vec3 {
    let [r, g, b, _] = sprite.color.map(|c| f32::from(c) / 255.0);
    Vec3::new(r, g, b)
}

/// `light` is in display light, as the sprite's own.
fn droplet(sprite: &SpriteParticle, slot: u32, origin: Vec3, step: f32, light: Vec3, seed: &mut u32) -> Droplet {
    let (at, velocity, half) = thrown(sprite, &CHUNK, step, seed);
    let seconds = CHUNK.seconds * (LIFE_LEAST + LIFE_SPREAD * next_unit(seed));
    let turn = next_unit(seed) * std::f32::consts::TAU;
    Droplet {
        place: (at + origin).extend(0.0),
        velocity: velocity.extend(seconds),
        colour: (sprite_rgb(sprite) * light).powf(2.2).extend(half),
        rect: Vec4::from_array(sprite.uv),
        shape: Vec4::new(slot as f32, 0.0, turn, DROPLET_GRAVITY),
        rest: Vec4::ZERO,
    }
}

fn between([low, high]: [f32; 2], t: f32) -> f32 {
    low + (high - low) * t
}

#[derive(Clone, Copy, Debug)]
struct Burst {
    axis: Vec3,
    radial: bool,
    /// 0 for a small hit to 1 for a big one.
    big: f32,
}

fn burst_of(sprites: &[&SpriteParticle], step: f32) -> Burst {
    let (mut sum, mut speed) = (Vec3::ZERO, 0.0);
    for sprite in sprites {
        let moved = Vec3::from(to_bevy(sprite.location)) - Vec3::from(to_bevy(sprite.previous));
        let own = if step > 1e-4 { moved / step } else { Vec3::ZERO };
        (sum, speed) = (sum + own, speed + own.length());
    }
    let count = sprites.len() as f32;
    let big = ((count - 1.0) / BIG_HIT).clamp(0.0, 1.0);
    if speed < 20.0 * count {
        return Burst { axis: Vec3::Y, radial: false, big };
    }
    Burst { axis: sum.normalize_or(Vec3::Y), radial: sprites.len() >= 4 && sum.length() < RADIAL * speed, big }
}

/// A way within `degrees` of `axis`, evenly over the cone's cap; 180 is any way.
fn in_cone(axis: Vec3, degrees: f32, seed: &mut u32) -> Vec3 {
    let cos = 1.0 - next_unit(seed) * (1.0 - degrees.to_radians().cos());
    let sin = (1.0 - cos * cos).max(0.0).sqrt();
    let turn = next_unit(seed) * std::f32::consts::TAU;
    let (a, b) = axis.any_orthonormal_pair();
    axis * cos + (a * turn.cos() + b * turn.sin()) * sin
}

/// Where a blood burst is in the world's coordinates and its colour in linear light.
struct Hit {
    at: Vec3,
    colour: Vec3,
    burst: Burst,
    source: u64,
}

fn spray(hit: &Hit, level: u8, seed: &mut u32, droplets: &mut Vec<Droplet>, heavy: &mut Vec<LiquidSpray>) {
    let Hit { at, colour, burst, source } = *hit;
    let big = burst.big;
    let (cone, mists, streaks, heavies) = if burst.radial { (180.0, RADIAL_MISTS, RADIAL_STREAKS, RADIAL_HEAVY) } else { (between(CONE, big), MISTS, STREAKS, HEAVY) };
    let speed = [between([SMALL_HIT_SPEED[0], BIG_HIT_SPEED[0]], big), between([SMALL_HIT_SPEED[1], BIG_HIT_SPEED[1]], big)];
    let (drift, puff) = if burst.radial { (Vec3::ZERO, 2.0) } else { (burst.axis * MIST_DRIFT, 1.0 + big) };
    for _ in 0..between(mists, big).round() as u32 {
        let off = Vec3::new(next_unit(seed), next_unit(seed), next_unit(seed)) * 2.0 - Vec3::ONE;
        droplets.push(Droplet {
            place: (at + off * MIST_HALF).extend(0.0),
            velocity: drift.extend(between(MIST_SECONDS, next_unit(seed))),
            colour: colour.extend(MIST_HALF * puff),
            rect: Vec4::ZERO,
            shape: Vec4::new(0.0, MIST, next_unit(seed) * std::f32::consts::TAU, 0.0),
            rest: Vec4::new(0.0, 0.0, 0.0, UNPUSHED),
        });
    }
    let streaks = (between(streaks, big) * f32::from(level) / 5.0).round().max(1.0) as u32;
    for _ in 0..streaks {
        let velocity = in_cone(burst.axis, cone, seed) * between(speed, next_unit(seed));
        droplets.push(Droplet {
            place: at.extend(0.0),
            velocity: velocity.extend(between(STREAK_SECONDS, next_unit(seed))),
            colour: colour.extend(between(STREAK_HALF, next_unit(seed))),
            rect: Vec4::ZERO,
            shape: Vec4::new(0.0, STREAK, 0.0, DROPLET_GRAVITY),
            rest: Vec4::new(0.0, 0.0, 0.0, UNPUSHED),
        });
    }
    for _ in 0..between(heavies, big).round() as u32 {
        let velocity = in_cone(burst.axis, cone, seed) * between(speed, next_unit(seed)) * between(HEAVY_SPEED, next_unit(seed));
        heavy.push(LiquidSpray { at, velocity, radius: between(HEAVY_RADIUS, next_unit(seed)), kind: FluidType::Blood, source });
    }
}

impl Spawner {
    fn slot_of(&mut self, image: &Handle<Image>) -> Option<u32> {
        let slot = self.textures.iter().position(|held| held.id() == image.id()).or_else(|| {
            (self.textures.len() < TEXTURE_SLOTS).then(|| {
                self.textures.push(image.clone());
                self.textures.len() - 1
            })
        })?;
        Some(slot as u32)
    }

    pub(crate) fn forget(&mut self) {
        self.sources.clear();
        self.seen.clear();
        self.textures.clear();
    }

    /// Before the drawing takes the frame's newborn emitters.
    pub(crate) fn note(&mut self, drawn: &DrawnParticles) {
        if self.map != drawn.level {
            self.map = drawn.level;
            self.forget();
        }
        for look in &drawn.born {
            if let Some(throws) = detail_of(&look.name, &look.props)
                && let Some(slot) = self.slot_of(&look.image)
            {
                let blood = throws == Throws::Droplets && look.name.to_ascii_lowercase().contains("blood");
                self.sources.insert((look.parent, look.order), Source { throws, slot, blood });
            }
        }
    }

    /// `light_at` gives a chunk's or shard's light where its sprite is, in display light; a spark makes its own. A
    /// sprite that throws pieces puts them in `births`; a blood burst's heavy drops go to `sprays`.
    pub(crate) fn throw(
        &mut self,
        drawn: &DrawnParticles,
        level: u8,
        origin: Option<Vec3>,
        step: f32,
        light_at: impl Fn(Entity, Vector) -> Vec3,
        births: &mut Vec<PieceBirth>,
    ) -> Vec<Droplet> {
        let cap = pool_size(level) as usize;
        let mut newborn = Vec::new();
        for slot in &drawn.actors {
            let eligible = slot.drawn && slot.overlay.is_none() && slot.puddle_seed.is_none();
            for (order, emitter) in slot.emitters.iter().enumerate() {
                let key = (slot.parent, order as u32);
                let Some(&source) = self.sources.get(&key) else { continue };
                let before = self.seen.get(&key).copied().unwrap_or(0);
                let Some(newest) = emitter.sprites.iter().map(|sprite| sprite.serial).max().filter(|&newest| newest > before) else { continue };
                self.seen.insert(key, newest);
                let Some(origin) = origin.filter(|_| eligible) else { continue };
                let count = droplets_per_sprite(level, source.throws);
                if source.blood && before == 0 {
                    let fresh: Vec<&SpriteParticle> = emitter.sprites.iter().filter(|sprite| sprite.serial > before).collect();
                    if let Some(first) = fresh.first() {
                        let centre = fresh.iter().map(|sprite| Vec3::from(to_bevy(sprite.location))).sum::<Vec3>() / fresh.len() as f32;
                        let light = light_at(slot.parent, first.location);
                        let colour = BLOOD * (sprite_rgb(first) * light).powf(2.2);
                        let hit = Hit { at: centre + origin, colour, burst: burst_of(&fresh, step), source: slot.parent.to_bits() };
                        let mut made = Vec::new();
                        spray(&hit, level, &mut self.seed, &mut made, &mut self.sprays);
                        let room = cap - newborn.len().min(cap);
                        newborn.extend(made.into_iter().take(room));
                    }
                }
                for sprite in emitter.sprites.iter().filter(|sprite| sprite.serial > before) {
                    match source.throws {
                        Throws::Pieces(kind) => {
                            let Some(image) = self.textures.get(source.slot as usize).filter(|_| count > 0) else { continue };
                            let throw = throw_of(source.throws);
                            let light = if kind == PieceKind::Spark { Vec3::ONE } else { light_at(slot.parent, sprite.location) };
                            let tint = sprite_rgb(sprite) * light;
                            for _ in 0..count {
                                let (at, velocity, half) = thrown(sprite, &throw, step, &mut self.seed);
                                births.push(PieceBirth {
                                    kind,
                                    place: Vector::from(to_bevy(at)),
                                    velocity: Vector::from(to_bevy(velocity)),
                                    half,
                                    uv: sprite.uv,
                                    image: image.clone(),
                                    tint,
                                    parent: slot.parent,
                                    seconds: throw.seconds,
                                });
                            }
                        }
                        Throws::Droplets => {
                            let room = count.min((cap - newborn.len().min(cap)) as u32);
                            let light = if room > 0 { light_at(slot.parent, sprite.location) } else { Vec3::ONE };
                            for _ in 0..room {
                                let mut made = droplet(sprite, source.slot, origin, step, light, &mut self.seed);
                                if source.blood {
                                    made.rest.w = UNPUSHED;
                                }
                                newborn.push(made);
                            }
                        }
                    }
                }
            }
        }
        if self.sources.len() > SOURCES_KEPT {
            let live: FxHashSet<Entity> = drawn.actors.iter().map(|slot| slot.parent).collect();
            self.sources.retain(|(parent, _), _| live.contains(parent));
            self.seen.retain(|(parent, _), _| live.contains(parent));
        }
        newborn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::particle_frame::{DrawnActor, DrawnEmitter, EmitterLook};

    const SHARD: Throws = Throws::Pieces(PieceKind::Flat);
    const HEAD: Throws = Throws::Pieces(PieceKind::Lump);

    fn drawn_as(draw_style: DrawStyle, use_direction_as: DirectionUsage) -> EmitterProperties {
        EmitterProperties { draw_style, use_direction_as, ..EmitterProperties::default() }
    }

    #[test]
    fn chunks_and_sparks_are_told_by_the_scripts_textures() {
        let blended = drawn_as(DrawStyle::AlphaBlend, DirectionUsage::None);
        for chunk in ["nathans.Skins.bloodchunks1", "nathans.Skins.darkchunks", "nathans.Skins.concrete1"] {
            assert_eq!(detail_of(chunk, &blended), Some(Throws::Droplets), "{chunk}");
        }
        for head in ["nathans.Skins.skullchunks", "nathans.Skins.brainchunks"] {
            assert_eq!(detail_of(head, &blended), Some(HEAD), "{head}");
        }
        for shard in ["nathans.Skins.glassshards", "AW_Textures.plywoodparts_AW", "nathans.Skins.plywoodparts"] {
            assert_eq!(detail_of(shard, &blended), Some(SHARD), "{shard}");
        }
        assert_eq!(detail_of("nathans.Skins.glassshards", &drawn_as(DrawStyle::AlphaModulate, DirectionUsage::None)), Some(SHARD));
        assert_eq!(detail_of("AW_Textures.plywoodparts_AW", &drawn_as(DrawStyle::AlphaBlend, DirectionUsage::Right)), Some(SHARD));
        assert_eq!(detail_of("nathans.Skins.bloodchunks1", &drawn_as(DrawStyle::Translucent, DirectionUsage::None)), None);
        assert_eq!(detail_of("nathans.Skins.concrete1", &drawn_as(DrawStyle::AlphaModulate, DirectionUsage::None)), Some(Throws::Droplets));
        assert_eq!(detail_of("nathans.Skins.kevlar_chunks", &blended), Some(Throws::Droplets));
        assert_eq!(detail_of("nathans.Skins.glassshards", &drawn_as(DrawStyle::AlphaBlend, DirectionUsage::Normal)), None, "a shard lying where it fell");
        assert_eq!(detail_of("xPatchTex.FX.fusesparkler", &drawn_as(DrawStyle::Brighten, DirectionUsage::None)), Some(SPARK));
        assert_eq!(detail_of("nathans.Skins.bloodimpacts", &blended), None);
        assert_eq!(detail_of("nathans.Skins.smoke5", &blended), None);
        let dot = "nathans.Skins.softwhitedot";
        assert_eq!(detail_of(dot, &drawn_as(DrawStyle::Translucent, DirectionUsage::Up)), Some(SPARK));
        assert_eq!(detail_of(dot, &drawn_as(DrawStyle::Translucent, DirectionUsage::None)), None);
    }

    #[test]
    fn droplets_grow_with_the_level_and_stop_at_zero() {
        for throws in [Throws::Droplets, SHARD, SPARK] {
            assert_eq!(droplets_per_sprite(0, throws), 0);
            assert!(droplets_per_sprite(1, throws) >= 1);
            assert!(droplets_per_sprite(10, throws) > droplets_per_sprite(5, throws));
        }
        assert!(droplets_per_sprite(10, SHARD) > droplets_per_sprite(10, Throws::Droplets));
    }

    #[test]
    fn a_ninth_texture_finds_no_slot() {
        let mut spawner = Spawner::default();
        let mut images = bevy::asset::Assets::<Image>::default();
        let held: Vec<Handle<Image>> = (0..9).map(|_| images.add(Image::default())).collect();
        let slots: Vec<Option<u32>> = held.iter().map(|image| spawner.slot_of(image)).collect();
        assert_eq!(slots[..8], [0, 1, 2, 3, 4, 5, 6, 7].map(Some));
        assert_eq!(slots[8], None);
        assert_eq!(spawner.slot_of(&held[3]), Some(3));
    }

    fn rising_sprite() -> SpriteParticle {
        let nowhere = Vector::new(0.0, 0.0, 0.0);
        SpriteParticle {
            location: Vector::new(0.0, 0.0, 4.0),
            previous: nowhere,
            size: [5.0, 5.0],
            up: nowhere,
            right: nowhere,
            color: [255; 4],
            uv: [0.0, 0.25, 1.0, 0.5],
            blend: None,
            serial: 1,
        }
    }

    #[test]
    fn a_shard_is_larger_and_longer_lived_than_a_chunk() {
        let sprite = rising_sprite();
        let half = |throws| thrown(&sprite, &throw_of(throws), 0.02, &mut 7).2;
        let (chunk, shard) = (half(Throws::Droplets), half(SHARD));
        assert!(shard > chunk, "half size {shard} against {chunk}");
        assert!(shard <= throw_of(SHARD).most);
        let chunk_life = droplet(&sprite, 0, Vec3::ZERO, 0.02, Vec3::ONE, &mut 7).velocity.w;
        assert!(throw_of(SHARD).seconds > 20.0 && throw_of(SHARD).seconds > chunk_life, "lifetime against {chunk_life}");
    }

    #[test]
    fn a_droplet_keeps_its_sprites_motion_and_texture_cell() {
        let made = droplet(&rising_sprite(), 2, Vec3::ZERO, 0.02, Vec3::new(1.0, 0.5, 0.0), &mut 7);
        assert!(made.velocity.y > 0.0, "a sprite moving up throws upward: {made:?}");
        assert_eq!(made.rect, Vec4::new(0.0, 0.25, 1.0, 0.5));
        assert_eq!(made.shape.x, 2.0);
        assert!((0.3..=3.0).contains(&made.colour.w));
        assert_eq!(made.shape.w, DROPLET_GRAVITY);
        assert_eq!(made.colour.x, 1.0);
        assert!((made.colour.y - 0.5f32.powf(2.2)).abs() < 1e-6, "half the display light is less than half the linear light");
        assert_eq!(made.colour.z, 0.0, "a room with no blue light leaves none in the chunk");
        assert_eq!(made.rest, Vec4::ZERO, "a droplet is born in the air");
    }

    #[test]
    fn no_droplet_outlives_the_longest_life() {
        let mut seed = 3;
        for _ in 0..1000 {
            let made = droplet(&rising_sprite(), 0, Vec3::ZERO, 0.02, Vec3::ONE, &mut seed);
            assert!(made.velocity.w < LONGEST_DROPLET_LIFE, "lifetime {}", made.velocity.w);
        }
    }

    #[test]
    fn a_shard_throws_pieces_and_a_chunk_droplets() {
        let mut spawner = Spawner::default();
        let mut images = bevy::asset::Assets::<Image>::default();
        spawner.textures.push(images.add(Image::default()));
        let parent = Entity::PLACEHOLDER;
        spawner.sources.insert((parent, 0), Source { throws: SHARD, slot: 0, blood: false });
        spawner.sources.insert((parent, 1), Source { throws: Throws::Droplets, slot: 0, blood: false });
        let emitter = || DrawnEmitter { sprites: vec![rising_sprite()], ..DrawnEmitter::default() };
        let actor = DrawnActor { parent, drawn: true, emitters: vec![emitter(), emitter()], ..DrawnActor::default() };
        let drawn = DrawnParticles { actors: vec![actor], ..DrawnParticles::default() };
        let mut births = Vec::new();
        let droplets = spawner.throw(&drawn, 5, Some(Vec3::ZERO), 0.02, |_, _| Vec3::ONE, &mut births);
        assert_eq!(droplets.len() as u32, droplets_per_sprite(5, Throws::Droplets), "only the chunk throws droplets");
        assert_eq!(births.len() as u32, droplets_per_sprite(5, SHARD));
        assert!(births.iter().all(|birth| birth.half > 0.0 && birth.uv == [0.0, 0.25, 1.0, 0.5] && birth.velocity.z > 0.0), "a rising sprite throws upward in Unreal axes");
        assert!(births.iter().all(|birth| birth.kind == PieceKind::Flat && birth.seconds == throw_of(SHARD).seconds));
    }

    #[test]
    fn sparks_throw_pieces_and_no_droplets() {
        let mut spawner = Spawner::default();
        let mut images = bevy::asset::Assets::<Image>::default();
        spawner.textures.push(images.add(Image::default()));
        spawner.sources.insert((Entity::PLACEHOLDER, 0), Source { throws: SPARK, slot: 0, blood: false });
        let emitter = DrawnEmitter { sprites: vec![rising_sprite()], ..DrawnEmitter::default() };
        let actor = DrawnActor { parent: Entity::PLACEHOLDER, drawn: true, emitters: vec![emitter], ..DrawnActor::default() };
        let drawn = DrawnParticles { actors: vec![actor], ..DrawnParticles::default() };
        let mut births = Vec::new();
        let droplets = spawner.throw(&drawn, 5, Some(Vec3::ZERO), 0.02, |_, _| Vec3::ONE, &mut births);
        assert!(droplets.is_empty());
        assert_eq!(births.len() as u32, droplets_per_sprite(5, SPARK));
        assert!(births.iter().all(|birth| birth.kind == PieceKind::Spark && birth.seconds == throw_of(SPARK).seconds));
    }

    #[test]
    fn a_heads_skull_and_brain_throw_lumps_and_its_blood_droplets() {
        let mut world = bevy::ecs::world::World::new();
        let head = world.spawn_empty().id();
        let mut images = bevy::asset::Assets::<Image>::default();
        let image = images.add(Image::default());
        let look = |order, name: &str| EmitterLook {
            parent: head,
            order,
            image: image.clone(),
            props: drawn_as(DrawStyle::AlphaBlend, DirectionUsage::None),
            name: name.to_owned(),
            fogless: false,
        };
        let born = vec![look(0, "nathans.Skins.skullchunks"), look(1, "nathans.Skins.brainchunks"), look(2, "nathans.Skins.bloodchunks1")];
        let mut spawner = Spawner::default();
        spawner.note(&DrawnParticles { born, ..DrawnParticles::default() });
        let emitter = || DrawnEmitter { sprites: vec![rising_sprite()], ..DrawnEmitter::default() };
        let actor = DrawnActor { parent: head, drawn: true, emitters: vec![emitter(), emitter(), emitter()], ..DrawnActor::default() };
        let drawn = DrawnParticles { actors: vec![actor], ..DrawnParticles::default() };
        let mut births = Vec::new();
        let droplets = spawner.throw(&drawn, 5, Some(Vec3::ZERO), 0.02, |_, _| Vec3::ONE, &mut births);
        let chunks = droplets.iter().filter(|droplet| droplet.shape.y == 0.0).count() as u32;
        assert_eq!(chunks, droplets_per_sprite(5, Throws::Droplets), "the blood throws droplets, its spray besides");
        assert_eq!(births.len() as u32, 2 * droplets_per_sprite(5, HEAD));
        assert!(births.iter().all(|birth| birth.kind == PieceKind::Lump && birth.seconds == throw_of(HEAD).seconds));
    }

    #[test]
    fn a_blood_bursts_first_frame_sprays_mist_streaks_and_heavy_drops_once() {
        let mut spawner = Spawner::default();
        let mut images = bevy::asset::Assets::<Image>::default();
        spawner.textures.push(images.add(Image::default()));
        spawner.sources.insert((Entity::PLACEHOLDER, 0), Source { throws: Throws::Droplets, slot: 0, blood: true });
        let drawn_with = |serial| {
            let emitter = DrawnEmitter { sprites: vec![SpriteParticle { serial, ..rising_sprite() }], ..DrawnEmitter::default() };
            DrawnParticles { actors: vec![DrawnActor { parent: Entity::PLACEHOLDER, drawn: true, emitters: vec![emitter], ..DrawnActor::default() }], ..DrawnParticles::default() }
        };
        let droplets = spawner.throw(&drawn_with(1), 5, Some(Vec3::ZERO), 0.02, |_, _| Vec3::ONE, &mut Vec::new());
        let kinds = |kind: f32| droplets.iter().filter(|droplet| droplet.shape.y == kind).count();
        assert_eq!((kinds(MIST), kinds(STREAK) as f32), (1, STREAKS[0]));
        assert!(droplets.iter().filter(|droplet| droplet.shape.y == STREAK).all(|streak| streak.velocity.y > 0.0), "a rising burst sprays upward");
        assert!(!spawner.sprays.is_empty() && spawner.sprays.iter().all(|drop| drop.kind == FluidType::Blood && drop.velocity.y > 0.0));
        let taken = std::mem::take(&mut spawner.sprays).len();
        let later = spawner.throw(&drawn_with(2), 5, Some(Vec3::ZERO), 0.02, |_, _| Vec3::ONE, &mut Vec::new());
        assert!(taken > 0 && later.iter().all(|droplet| droplet.shape.y == 0.0) && spawner.sprays.is_empty(), "only the first burst sprays");
    }

    #[test]
    fn a_burst_with_no_shared_way_is_radial() {
        let way = |x: f32, y: f32| SpriteParticle { location: Vector::new(x, y, 0.0), previous: Vector::ZERO, ..rising_sprite() };
        let spread = [way(4.0, 0.0), way(-4.0, 0.0), way(0.0, 4.0), way(0.0, -4.0)];
        assert!(burst_of(&spread.iter().collect::<Vec<_>>(), 0.02).radial);
        let rising = [rising_sprite(), rising_sprite(), rising_sprite(), rising_sprite()];
        let burst = burst_of(&rising.iter().collect::<Vec<_>>(), 0.02);
        assert!(!burst.radial && burst.axis.y > 0.99);
        let mut seed = 9;
        assert!((0..100).all(|_| in_cone(Vec3::Y, 15.0, &mut seed).y >= 15f32.to_radians().cos() - 1e-4));
    }
}
