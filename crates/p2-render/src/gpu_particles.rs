//! Modern graphics: detail particles. The chunks, shards and sparks the scripts spawn throw a few more of the same
//! texture that the GPU moves, bounces and slides on the depth prepass; shards, sparks and pieces of skull and brain
//! are thrown as rigid pieces instead. They are seen only; nothing reads them back.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{AssetId, Handle, uuid_handle};
use bevy::camera::Camera;
use bevy::core_pipeline::prepass::{DepthPrepass, ViewPrepassTextures};
use bevy::core_pipeline::schedule::Core3d;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Has, Query, Res, ResMut, Resource, With};
use bevy::ecs::query::QueryItem;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::SystemParam;
use bevy::ecs::system::lifetimeless::Read;
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    AddressMode, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, Buffer,
    BufferDescriptor, BufferInitDescriptor, BufferUsages, CachedComputePipelineId, CachedRenderPipelineId, ColorTargetState, ColorWrites,
    ComputePassDescriptor, ComputePipelineDescriptor, FilterMode, FragmentState, PipelineCache, RenderPassDescriptor, RenderPipelineDescriptor, Sampler,
    SamplerBindingType, SamplerDescriptor, ShaderStages, SpecializedRenderPipeline, SpecializedRenderPipelines, TextureFormat, TextureSampleType,
    VertexState, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::sync_component::SyncComponent;
use bevy::render::texture::{FallbackImage, GpuImage};
use bevy::render::view::{ExtractedView, Msaa, ViewTarget, ViewUniform, ViewUniformOffset, ViewUniforms};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use bevy::time::Time;
use p2_bevy_assets::to_bevy;
use p2_host::display::{MODERN_GPU_PARTICLES, MODERN_PARTICLE_LIGHT, Settings};
use p2_math::Vector;
use p2_particles::{DirectionUsage, DrawStyle, EmitterProperties, SpriteParticle};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::MainPassExtraSystems;
use crate::components::SceneView;
use crate::debris::{PieceBirth, PieceBirths, PieceKind};
use crate::frame_facts::FrameFacts;
use crate::level_collision::LevelCollision;
use crate::map_lights::MapLights;
use crate::particle_draw::sprites::lamps_shown;
use crate::particle_draw::{ParticleDrawSystems, ParticleDraws, draw_particles};
use crate::particle_frame::{Blast, DrawnParticles};
use crate::screen_pass::modern_level;
use crate::shaders::add_shaders;

const DROPLET_SHADER: Handle<Shader> = uuid_handle!("7c4e2a91-5b38-4d6f-a0e7-3f9b1c82d465");
/// Matches `Droplet` in `gpu_particles.wesl`: place and age, velocity and lifetime, colour and half size, the
/// texture's rectangle, then texture slot, stretch, turn and gravity, then the surface it lies on, then how it lands
/// and slides.
const DROPLET_WORDS: usize = 28;
/// Matches the textures bound in `gpu_particles.wesl`.
const TEXTURE_SLOTS: usize = 8;
const DROPLET_BYTES: u64 = DROPLET_WORDS as u64 * 4;
const POOL_PER_LEVEL: u32 = 4096;
const POOL_CAP: u32 = 40960;
/// Matches `PUSHES` in `gpu_particles.wesl`.
const PUSHES: usize = 8;
/// Matches `@workgroup_size` in `gpu_particles.wesl`.
const WORKGROUP: u32 = 64;
/// The sun's share of a puff's light and the most light it takes, as the lit sprites' shader has them.
const PUFF_SUN: f32 = 0.5;
const LIGHT_LIMIT: f32 = 2.0;
/// How far a line to the sun is followed, in Unreal units.
const SUN_REACH: f32 = 30_000.0;
/// Past this many emitters the ones whose actor is gone are dropped.
const SOURCES_KEPT: usize = 4096;

/// Premultiplied colour over the frame; the frame's alpha, the sun rays' mask, is kept.
const OVER: BlendState = BlendState {
    color: BlendComponent { src_factor: BlendFactor::One, dst_factor: BlendFactor::OneMinusSrcAlpha, operation: BlendOperation::Add },
    alpha: BlendComponent { src_factor: BlendFactor::Zero, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
};

pub struct GpuParticlesPlugin;

#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct GpuParticleSystems;

impl Plugin for GpuParticlesPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [(DROPLET_SHADER, Shader::from_wesl(include_str!("gpu_particles.wesl"), "p2_render/gpu_particles.wesl"))]);
        app.add_plugins(ExtractComponentPlugin::<GpuDroplets>::default()).init_resource::<Spawner>().init_resource::<PieceBirths>().add_systems(
            PostUpdate,
            (note_sources.before(draw_particles), throw_droplets.after(draw_particles)).chain().in_set(GpuParticleSystems).in_set(ParticleDrawSystems),
        );
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<DropletPipeline>>()
            .add_systems(RenderStartup, init_droplet_pipeline)
            .add_systems(Render, prepare_pools.in_set(RenderSystems::PrepareResources))
            .add_systems(Core3d, gpu_particles.in_set(MainPassExtraSystems::GpuParticles));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Detail {
    /// A piece of blood, dirt, concrete, bone or kevlar: a spinning cell of its texture.
    Chunk,
    /// A piece of a broken window, bottle or wooden door: a larger cell that lands, slides along the surface and lies.
    Shard,
    /// A streak along its motion that burns from its colour to red.
    Spark,
    /// A piece of skull or brain: a larger lump that bounces, rolls and lies.
    Head,
}

const PIECE_WORDS: [(&str, Detail); 6] = [
    ("glassshards", Detail::Shard),
    ("plywoodparts", Detail::Shard),
    ("skullchunks", Detail::Head),
    ("brainchunks", Detail::Head),
    ("chunk", Detail::Chunk),
    ("concrete", Detail::Chunk),
];
const SPARK_WORD: &str = "softwhitedot";
const FUSE_WORD: &str = "sparkler";

/// Told by the texture's name and how the scripts draw it: their breakage is glass shards and plywood parts, their
/// chunk textures hold `chunk` or `concrete`, both blended by their alpha; their sparks are the soft white dot drawn
/// along its motion (the same dot facing the eye is a glow or a trail) and a fuse's sparkler. A piece drawn flat on a
/// surface already lies where it fell and throws nothing.
fn detail_of(texture: &str, props: &EmitterProperties) -> Option<Detail> {
    let texture = texture.to_ascii_lowercase();
    if texture.contains(FUSE_WORD) {
        return Some(Detail::Spark);
    }
    if texture.contains(SPARK_WORD) {
        return (props.use_direction_as != DirectionUsage::None).then_some(Detail::Spark);
    }
    let lies = matches!(props.use_direction_as, DirectionUsage::Normal | DirectionUsage::UpAndNormal | DirectionUsage::RightAndNormal);
    let blended = matches!(props.draw_style, DrawStyle::AlphaBlend | DrawStyle::AlphaModulate);
    if !blended || lies {
        return None;
    }
    PIECE_WORDS.iter().find(|(word, _)| texture.contains(word)).map(|&(_, detail)| detail)
}

struct Throw {
    per_level: f32,
    /// Unreal units a second, when the sprite's own motion is not known.
    speed: f32,
    seconds: f32,
    /// The share of the sprite's half size a droplet takes, least and most.
    size: [f32; 2],
    /// The largest half size, in Unreal units.
    most: f32,
    /// The share of the sprite's speed a piece takes, least and most.
    fling: [f32; 2],
    /// Length over width; 0 is a square that spins.
    stretch: f32,
    /// Unreal units a second squared.
    gravity: f32,
    /// On landing, the share of its speed into the surface it bounces back with and of its speed along it it keeps.
    restitution: f32,
    keep: f32,
    /// How fast it slows while sliding on a surface, in Unreal units a second squared; 0 never slides.
    skid: f32,
}

fn throw_of(detail: Detail) -> Throw {
    match detail {
        Detail::Chunk => Throw {
            per_level: 0.6,
            speed: 160.0,
            seconds: 8.0,
            size: [0.2, 0.45],
            most: 3.0,
            fling: [0.5, 1.2],
            stretch: 0.0,
            gravity: 800.0,
            restitution: 0.35,
            keep: 0.7,
            skid: 0.0,
        },
        Detail::Shard => Throw {
            per_level: 0.8,
            speed: 200.0,
            seconds: 30.0,
            size: [0.5, 1.0],
            most: 6.0,
            fling: [0.5, 1.2],
            stretch: 0.0,
            gravity: 1200.0,
            restitution: 0.2,
            keep: 0.85,
            skid: 400.0,
        },
        Detail::Spark => Throw {
            per_level: 0.8,
            speed: 300.0,
            seconds: 1.0,
            size: [0.4, 0.7],
            most: 3.0,
            fling: [0.5, 1.2],
            stretch: 12.0,
            gravity: 500.0,
            restitution: 0.35,
            keep: 0.7,
            skid: 0.0,
        },
        Detail::Head => Throw { per_level: 0.5, seconds: 15.0, size: [0.4, 0.8], most: 7.0, fling: [0.8, 1.3], ..throw_of(Detail::Chunk) },
    }
}

fn piece_kind(detail: Detail) -> Option<PieceKind> {
    match detail {
        Detail::Chunk => None,
        Detail::Shard => Some(PieceKind::Flat),
        Detail::Head => Some(PieceKind::Lump),
        Detail::Spark => Some(PieceKind::Spark),
    }
}

fn droplets_per_sprite(level: u8, detail: Detail) -> u32 {
    (f32::from(level) * throw_of(detail).per_level).ceil() as u32
}

fn pool_size(level: u8) -> u32 {
    (POOL_PER_LEVEL * u32::from(level)).min(POOL_CAP)
}

#[derive(Clone, Copy)]
struct Source {
    detail: Detail,
    slot: u32,
}

#[derive(Resource, Default)]
struct Spawner {
    map: u32,
    sources: FxHashMap<(Entity, u32), Source>,
    /// The highest spawn serial seen of each emitter: a sprite past it is newborn.
    seen: FxHashMap<(Entity, u32), u32>,
    /// The textures the droplets are drawn with, by slot; an emitter whose texture finds no slot throws nothing.
    textures: Vec<Handle<Image>>,
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
fn droplet(sprite: &SpriteParticle, source: Source, origin: Vec3, step: f32, light: Vec3, seed: &mut u32) -> [f32; DROPLET_WORDS] {
    let throw = throw_of(source.detail);
    let (at, velocity, half) = thrown(sprite, &throw, step, seed);
    let at = at + origin;
    let seconds = throw.seconds * (0.7 + 0.6 * next_unit(seed));
    let [r, g, b] = (sprite_rgb(sprite) * light).powf(2.2).to_array();
    let turn = next_unit(seed) * std::f32::consts::TAU;
    let [u0, v0, u1, v1] = sprite.uv;
    [
        at.x, at.y, at.z, 0.0, velocity.x, velocity.y, velocity.z, seconds, r, g, b, half, u0, v0, u1, v1, source.slot as f32, throw.stretch, turn, throw.gravity,
        0.0, 0.0, 0.0, 0.0, throw.restitution, throw.keep, throw.skid, 0.0,
    ]
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

    fn forget(&mut self) {
        self.sources.clear();
        self.seen.clear();
        self.textures.clear();
    }

    /// Before the drawing takes the frame's newborn emitters.
    fn note(&mut self, drawn: &DrawnParticles) {
        if self.map != drawn.level {
            self.map = drawn.level;
            self.forget();
        }
        for look in &drawn.born {
            if let Some(detail) = detail_of(&look.name, &look.props)
                && let Some(slot) = self.slot_of(&look.image)
            {
                self.sources.insert((look.parent, look.order), Source { detail, slot });
            }
        }
    }

    /// `light_at` gives a chunk's or shard's light where its sprite is, in display light; a spark makes its own. A
    /// shard's, a spark's and a skull or brain sprite throws rigid pieces into `births` instead of droplets.
    fn throw(
        &mut self,
        drawn: &DrawnParticles,
        level: u8,
        origin: Option<Vec3>,
        step: f32,
        light_at: impl Fn(Entity, Vector) -> Vec3,
        births: &mut Vec<PieceBirth>,
    ) -> Vec<[f32; DROPLET_WORDS]> {
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
                let count = droplets_per_sprite(level, source.detail);
                for sprite in emitter.sprites.iter().filter(|sprite| sprite.serial > before) {
                    if let Some(kind) = piece_kind(source.detail) {
                        let Some(image) = self.textures.get(source.slot as usize).filter(|_| count > 0) else { continue };
                        let throw = throw_of(source.detail);
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
                        continue;
                    }
                    let room = count.min((cap - newborn.len().min(cap)) as u32);
                    let light = if room > 0 { light_at(slot.parent, sprite.location) } else { Vec3::ONE };
                    for _ in 0..room {
                        newborn.push(droplet(sprite, source, origin, step, light, &mut self.seed));
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

/// On a scene camera while detail particles are on: the pool's size, the map it belongs to, the frame's seconds, the
/// droplets born this frame and the textures by slot.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct GpuDroplets {
    pool: u32,
    map: u32,
    seconds: f32,
    newborn: Vec<[f32; DROPLET_WORDS]>,
    /// This frame's pushes, `Push` in `gpu_particles.wesl`: the place as the droplets', the reach, then the speed.
    pushes: Vec<[f32; 8]>,
    textures: Vec<AssetId<Image>>,
}

/// What lights a chunk as its sprite is lit: the light at its emitter and the sun where a line to it is free.
#[derive(SystemParam)]
struct RoomLight<'w> {
    draws: Option<Res<'w, ParticleDraws>>,
    suns: Option<Res<'w, MapLights>>,
    level: Option<Res<'w, LevelCollision>>,
}

impl RoomLight<'_> {
    /// In display light; `None` for an emitter the drawing does not know.
    fn at(&self, parent: Entity, location: Vector) -> Option<Vec3> {
        let emitter = self.draws.as_deref()?.light_of(parent)?;
        let level = self.level.as_deref().and_then(|level| level.0.as_deref());
        let sun = self.suns.as_deref().and_then(|suns| Some((suns.sun.as_ref()?.1.direction, suns.sun_color)));
        let sunlit = sun.filter(|&(direction, _)| level.is_none_or(|level| level.light_passes(location, location - direction * SUN_REACH)));
        let sun = sunlit.map_or(Vec3::ZERO, |(_, color)| lamps_shown(Vec3::from(color)) * PUFF_SUN);
        Some((emitter + sun).min(Vec3::splat(LIGHT_LIMIT)))
    }
}

fn note_sources(settings: Option<Res<Settings>>, drawn: Option<Res<DrawnParticles>>, mut spawner: ResMut<Spawner>) {
    match drawn.as_deref().filter(|_| modern_level(settings.as_deref(), MODERN_GPU_PARTICLES) > 0) {
        Some(drawn) => spawner.note(drawn),
        None => spawner.forget(),
    }
}

/// Only a view that is active, single sampled and has a depth prepass draws droplets.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn throw_droplets(
    mut commands: Commands,
    settings: Option<Res<Settings>>,
    time: Res<Time>,
    facts: Res<FrameFacts>,
    drawn: Option<Res<DrawnParticles>>,
    views: Query<(Entity, &Camera, &Msaa, Has<DepthPrepass>, Has<GpuDroplets>), With<SceneView>>,
    mut spawner: ResMut<Spawner>,
    mut births: ResMut<PieceBirths>,
    room: RoomLight,
) {
    let level = modern_level(settings.as_deref(), MODERN_GPU_PARTICLES);
    let drawn_by = |camera: &Camera, msaa: &Msaa, depth: bool| level > 0 && camera.is_active && matches!(msaa, Msaa::Off) && depth;
    let any_view = views.iter().any(|(_, camera, msaa, depth, _)| drawn_by(camera, msaa, depth));
    // With the particles' light off a sprite is drawn as its texture is, and so is its chunk.
    let lit = facts.in_play && modern_level(settings.as_deref(), MODERN_PARTICLE_LIGHT) > 0;
    let light_at = |parent: Entity, location: Vector| room.at(parent, location).filter(|_| lit).unwrap_or(Vec3::ONE);
    let newborn = match drawn.as_deref() {
        Some(drawn) if any_view => spawner.throw(drawn, level, facts.in_play.then_some(facts.origin), facts.game_step, light_at, &mut births.0),
        _ => Vec::new(),
    };
    let seconds = if facts.paused { 0.0 } else { time.delta_secs().min(0.1) };
    let (pool, map) = (pool_size(level), drawn.as_deref().map_or(0, |drawn| drawn.level));
    let pushes: Vec<[f32; 8]> = drawn.as_deref().map_or(&[][..], |drawn| &drawn.blasts).iter().take(PUSHES).map(|blast| push_words(blast, facts.origin)).collect();
    let textures: Vec<AssetId<Image>> = spawner.textures.iter().map(Handle::id).collect();
    for (entity, camera, msaa, depth, shown) in &views {
        if drawn_by(camera, msaa, depth) {
            commands.entity(entity).insert(GpuDroplets { pool, map, seconds, newborn: newborn.clone(), pushes: pushes.clone(), textures: textures.clone() });
        } else if shown {
            commands.entity(entity).remove::<GpuDroplets>();
        }
    }
}

impl SyncComponent<RenderApp> for GpuDroplets {
    type Target = (Self, DropletPool);
}

impl ExtractComponent<RenderApp> for GpuDroplets {
    type QueryData = Read<Self>;
    type QueryFilter = ();
    type Out = Self;

    fn extract_component(droplets: QueryItem<'_, '_, Self::QueryData>) -> Option<Self::Out> {
        Some(droplets.clone())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RingWrite {
    slot: u32,
    from: usize,
    len: usize,
}

/// Where `count` newborn droplets go in a ring of `size` slots from `cursor`, and the next cursor; when more are born
/// than the ring holds, the newest are kept.
fn ring_writes(cursor: u32, size: u32, count: usize) -> (Vec<RingWrite>, u32) {
    let size = size as usize;
    if size == 0 {
        return (Vec::new(), 0);
    }
    let kept = count.min(size);
    let (from, cursor) = (count - kept, cursor as usize % size);
    let first = kept.min(size - cursor);
    let mut writes = Vec::new();
    if first > 0 {
        writes.push(RingWrite { slot: cursor as u32, from, len: first });
    }
    if kept > first {
        writes.push(RingWrite { slot: 0, from: from + first, len: kept - first });
    }
    (writes, ((cursor + kept) % size) as u32)
}

fn push_words(blast: &Blast, origin: Vec3) -> [f32; 8] {
    let [x, y, z] = (Vec3::from(to_bevy(blast.at)) + origin).to_array();
    [x, y, z, blast.radius, blast.push, 0.0, 0.0, 0.0]
}

/// `Frame` in `gpu_particles.wesl`.
fn frame_bytes(seconds: f32, size: u32, pushes: &[[f32; 8]]) -> Vec<u8> {
    let held = pushes.iter().chain(std::iter::repeat(&[0.0; 8])).take(PUSHES).flatten().map(|word| word.to_bits());
    [seconds.to_bits(), size, pushes.len().min(PUSHES) as u32, 0].into_iter().chain(held).flat_map(u32::to_le_bytes).collect()
}

#[derive(Resource)]
pub struct DropletPipeline {
    simulate_layout: BindGroupLayoutDescriptor,
    draw_layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    /// Queued when a view first draws droplets, so nothing is built while the feature is off.
    simulate: Option<CachedComputePipelineId>,
}

/// A view's ring of droplets; a new, zeroed one is made when the level or the map changes.
#[derive(Component)]
pub struct DropletPool {
    buffer: Buffer,
    frame: Buffer,
    draw: CachedRenderPipelineId,
    size: u32,
    map: u32,
    cursor: u32,
}

fn init_droplet_pipeline(mut commands: Commands, device: Res<RenderDevice>) {
    let simulate_layout = BindGroupLayoutDescriptor::new(
        "gpu particles step",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                binding_types::storage_buffer_sized(false, None),
                binding_types::texture_2d(TextureSampleType::Depth),
                binding_types::uniform_buffer::<ViewUniform>(true),
                binding_types::uniform_buffer_sized(false, None),
            ),
        ),
    );
    let draw_layout = BindGroupLayoutDescriptor::new(
        "gpu particles",
        &BindGroupLayoutEntries::with_indices(
            ShaderStages::VERTEX_FRAGMENT,
            (
                (1, binding_types::texture_2d(TextureSampleType::Depth)),
                (2, binding_types::uniform_buffer::<ViewUniform>(true)),
                (4, binding_types::storage_buffer_read_only_sized(false, None)),
                (5, binding_types::sampler(SamplerBindingType::Filtering)),
                (6, binding_types::texture_2d(TextureSampleType::Float { filterable: true })),
                (7, binding_types::texture_2d(TextureSampleType::Float { filterable: true })),
                (8, binding_types::texture_2d(TextureSampleType::Float { filterable: true })),
                (9, binding_types::texture_2d(TextureSampleType::Float { filterable: true })),
                (10, binding_types::texture_2d(TextureSampleType::Float { filterable: true })),
                (11, binding_types::texture_2d(TextureSampleType::Float { filterable: true })),
                (12, binding_types::texture_2d(TextureSampleType::Float { filterable: true })),
                (13, binding_types::texture_2d(TextureSampleType::Float { filterable: true })),
            ),
        ),
    );
    let sampler = device.create_sampler(&SamplerDescriptor {
        label: "gpu particles".into(),
        address_mode_u: AddressMode::ClampToEdge,
        address_mode_v: AddressMode::ClampToEdge,
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        ..Default::default()
    });
    commands.insert_resource(DropletPipeline { simulate_layout, draw_layout, sampler, simulate: None });
}

impl SpecializedRenderPipeline for DropletPipeline {
    type Key = TextureFormat;

    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("gpu particles".into()),
            layout: vec![self.draw_layout.clone()],
            vertex: VertexState { shader: DROPLET_SHADER, entry_point: Some("vertex".into()), ..Default::default() },
            fragment: Some(FragmentState {
                shader: DROPLET_SHADER,
                entry_point: Some("fragment".into()),
                targets: vec![Some(ColorTargetState { format, blend: Some(OVER), write_mask: ColorWrites::ALL })],
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

fn prepare_pools(
    mut commands: Commands,
    cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<DropletPipeline>>,
    mut pipeline: ResMut<DropletPipeline>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut views: Query<(Entity, &ExtractedView, &GpuDroplets, Option<&mut DropletPool>)>,
) {
    for (entity, view, droplets, pool) in &mut views {
        if pipeline.simulate.is_none() {
            pipeline.simulate = Some(cache.queue_compute_pipeline(ComputePipelineDescriptor {
                label: Some("gpu particles step".into()),
                layout: vec![pipeline.simulate_layout.clone()],
                shader: DROPLET_SHADER,
                entry_point: Some("simulate".into()),
                ..Default::default()
            }));
        }
        let draw = pipelines.specialize(&cache, &pipeline, view.target_format);
        let frame = device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("gpu particles frame"),
            contents: &frame_bytes(droplets.seconds, droplets.pool, &droplets.pushes),
            usage: BufferUsages::UNIFORM,
        });
        let Some(mut pool) = pool.filter(|pool| pool.size == droplets.pool && pool.map == droplets.map) else {
            let buffer = device.create_buffer(&BufferDescriptor {
                label: Some("gpu particles"),
                size: u64::from(droplets.pool) * DROPLET_BYTES,
                usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            commands.entity(entity).insert(DropletPool { buffer, frame, draw, size: droplets.pool, map: droplets.map, cursor: 0 });
            continue;
        };
        let bytes: Vec<u8> = droplets.newborn.iter().flatten().flat_map(|word| word.to_le_bytes()).collect();
        let (writes, cursor) = ring_writes(pool.cursor, pool.size, droplets.newborn.len());
        let stride = DROPLET_BYTES as usize;
        for RingWrite { slot, from, len } in writes {
            if let Some(range) = bytes.get(from * stride..(from + len) * stride) {
                queue.write_buffer(&pool.buffer, u64::from(slot) * DROPLET_BYTES, range);
            }
        }
        (pool.cursor, pool.frame, pool.draw) = (cursor, frame, draw);
    }
}

fn gpu_particles(
    view: ViewQuery<(&ViewTarget, &ViewPrepassTextures, &ViewUniformOffset, &DropletPool, &GpuDroplets)>,
    cache: Res<PipelineCache>,
    pipeline: Res<DropletPipeline>,
    view_uniforms: Res<ViewUniforms>,
    images: Res<RenderAssets<GpuImage>>,
    fallback: Res<FallbackImage>,
    mut ctx: RenderContext,
) {
    let (target, prepass, offset, pool, droplets) = view.into_inner();
    let (Some(simulate), Some(draw), Some(depth), Some(uniform)) = (
        pipeline.simulate.and_then(|id| cache.get_compute_pipeline(id)),
        cache.get_render_pipeline(pool.draw),
        prepass.depth_only_view(),
        view_uniforms.uniforms.binding(),
    ) else {
        return;
    };
    let texture = |slot: usize| &droplets.textures.get(slot).and_then(|&id| images.get(id)).unwrap_or(&fallback.d2).texture_view;
    let device = ctx.render_device();
    let stepped = device.create_bind_group(
        "gpu particles step",
        &cache.get_bind_group_layout(&pipeline.simulate_layout),
        &BindGroupEntries::sequential((pool.buffer.as_entire_binding(), depth, uniform.clone(), pool.frame.as_entire_binding())),
    );
    let shown = device.create_bind_group(
        "gpu particles",
        &cache.get_bind_group_layout(&pipeline.draw_layout),
        &BindGroupEntries::with_indices((
            (1, depth),
            (2, uniform),
            (4, pool.buffer.as_entire_binding()),
            (5, &pipeline.sampler),
            (6, texture(0)),
            (7, texture(1)),
            (8, texture(2)),
            (9, texture(3)),
            (10, texture(4)),
            (11, texture(5)),
            (12, texture(6)),
            (13, texture(7)),
        )),
    );
    {
        let mut pass = ctx.command_encoder().begin_compute_pass(&ComputePassDescriptor { label: Some("gpu particles step"), timestamp_writes: None });
        pass.set_pipeline(simulate);
        pass.set_bind_group(0, &*stepped, &[offset.offset]);
        pass.dispatch_workgroups(pool.size.div_ceil(WORKGROUP), 1, 1);
    }
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("gpu particles"),
        color_attachments: &[Some(target.get_color_attachment())],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(draw);
    pass.set_bind_group(0, &shown, &[offset.offset]);
    pass.draw(0..pool.size * 6, 0..1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::particle_frame::{DrawnActor, DrawnEmitter, EmitterLook};

    fn drawn_as(draw_style: DrawStyle, use_direction_as: DirectionUsage) -> EmitterProperties {
        EmitterProperties { draw_style, use_direction_as, ..EmitterProperties::default() }
    }

    #[test]
    fn chunks_and_sparks_are_told_by_the_scripts_textures() {
        let blended = drawn_as(DrawStyle::AlphaBlend, DirectionUsage::None);
        for chunk in ["nathans.Skins.bloodchunks1", "nathans.Skins.darkchunks", "nathans.Skins.concrete1"] {
            assert_eq!(detail_of(chunk, &blended), Some(Detail::Chunk), "{chunk}");
        }
        for head in ["nathans.Skins.skullchunks", "nathans.Skins.brainchunks"] {
            assert_eq!(detail_of(head, &blended), Some(Detail::Head), "{head}");
        }
        for shard in ["nathans.Skins.glassshards", "AW_Textures.plywoodparts_AW", "nathans.Skins.plywoodparts"] {
            assert_eq!(detail_of(shard, &blended), Some(Detail::Shard), "{shard}");
        }
        assert_eq!(detail_of("nathans.Skins.glassshards", &drawn_as(DrawStyle::AlphaModulate, DirectionUsage::None)), Some(Detail::Shard));
        assert_eq!(detail_of("AW_Textures.plywoodparts_AW", &drawn_as(DrawStyle::AlphaBlend, DirectionUsage::Right)), Some(Detail::Shard));
        assert_eq!(detail_of("nathans.Skins.bloodchunks1", &drawn_as(DrawStyle::Translucent, DirectionUsage::None)), None);
        assert_eq!(detail_of("nathans.Skins.concrete1", &drawn_as(DrawStyle::AlphaModulate, DirectionUsage::None)), Some(Detail::Chunk));
        assert_eq!(detail_of("nathans.Skins.kevlar_chunks", &blended), Some(Detail::Chunk));
        assert_eq!(detail_of("nathans.Skins.glassshards", &drawn_as(DrawStyle::AlphaBlend, DirectionUsage::Normal)), None, "a shard lying where it fell");
        assert_eq!(detail_of("xPatchTex.FX.fusesparkler", &drawn_as(DrawStyle::Brighten, DirectionUsage::None)), Some(Detail::Spark));
        assert_eq!(detail_of("nathans.Skins.bloodimpacts", &blended), None);
        assert_eq!(detail_of("nathans.Skins.smoke5", &blended), None);
        let dot = "nathans.Skins.softwhitedot";
        assert_eq!(detail_of(dot, &drawn_as(DrawStyle::Translucent, DirectionUsage::Up)), Some(Detail::Spark));
        assert_eq!(detail_of(dot, &drawn_as(DrawStyle::Translucent, DirectionUsage::None)), None);
    }

    #[test]
    fn droplets_grow_with_the_level_and_stop_at_zero() {
        for detail in [Detail::Chunk, Detail::Shard, Detail::Spark] {
            assert_eq!(droplets_per_sprite(0, detail), 0);
            assert!(droplets_per_sprite(1, detail) >= 1);
            assert!(droplets_per_sprite(10, detail) > droplets_per_sprite(5, detail));
        }
        assert!(droplets_per_sprite(10, Detail::Shard) > droplets_per_sprite(10, Detail::Chunk));
        assert_eq!(pool_size(0), 0);
        assert_eq!(pool_size(5), 5 * POOL_PER_LEVEL);
        assert_eq!(pool_size(10), POOL_CAP);
        assert_eq!(pool_size(20), POOL_CAP);
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
    fn a_shard_is_larger_and_longer_lived_than_a_chunk_and_slides() {
        let made = |detail| droplet(&rising_sprite(), Source { detail, slot: 0 }, Vec3::ZERO, 0.02, Vec3::ONE, &mut 7);
        let (chunk, shard) = (made(Detail::Chunk), made(Detail::Shard));
        assert!(shard[11] > chunk[11], "half size {} against {}", shard[11], chunk[11]);
        assert!(shard[11] <= throw_of(Detail::Shard).most);
        assert!(shard[7] > 20.0 && shard[7] > chunk[7], "lifetime {} against {}", shard[7], chunk[7]);
        assert_eq!(chunk[24..], [0.35, 0.7, 0.0, 0.0], "a chunk bounces and never slides");
        assert!(shard[24] < chunk[24] && shard[25] > chunk[25] && shard[26] > 0.0, "{:?}", &shard[24..]);
    }

    #[test]
    fn a_droplet_keeps_its_sprites_motion_and_texture_cell() {
        let sprite = rising_sprite();
        let mut seed = 7;
        let made = droplet(&sprite, Source { detail: Detail::Chunk, slot: 2 }, Vec3::ZERO, 0.02, Vec3::new(1.0, 0.5, 0.0), &mut seed);
        assert!(made[5] > 0.0, "a sprite moving up throws upward: {made:?}");
        assert_eq!(made[12..17], [0.0, 0.25, 1.0, 0.5, 2.0]);
        assert!((0.3..=3.0).contains(&made[11]));
        assert_eq!(made[19], 800.0);
        assert_eq!(made[8], 1.0);
        assert!((made[9] - 0.5f32.powf(2.2)).abs() < 1e-6, "half the display light is less than half the linear light");
        assert_eq!(made[10], 0.0, "a room with no blue light leaves none in the chunk");
        assert_eq!(made[20..24], [0.0; 4], "a droplet is born in the air");
    }

    #[test]
    fn a_shard_throws_pieces_and_a_chunk_droplets() {
        let mut spawner = Spawner::default();
        let mut images = bevy::asset::Assets::<Image>::default();
        spawner.textures.push(images.add(Image::default()));
        let parent = Entity::PLACEHOLDER;
        spawner.sources.insert((parent, 0), Source { detail: Detail::Shard, slot: 0 });
        spawner.sources.insert((parent, 1), Source { detail: Detail::Chunk, slot: 0 });
        let emitter = || DrawnEmitter { sprites: vec![rising_sprite()], ..DrawnEmitter::default() };
        let actor = DrawnActor { parent, drawn: true, emitters: vec![emitter(), emitter()], ..DrawnActor::default() };
        let drawn = DrawnParticles { actors: vec![actor], ..DrawnParticles::default() };
        let mut births = Vec::new();
        let droplets = spawner.throw(&drawn, 5, Some(Vec3::ZERO), 0.02, |_, _| Vec3::ONE, &mut births);
        assert_eq!(droplets.len() as u32, droplets_per_sprite(5, Detail::Chunk), "only the chunk throws droplets");
        assert_eq!(births.len() as u32, droplets_per_sprite(5, Detail::Shard));
        assert!(births.iter().all(|birth| birth.half > 0.0 && birth.uv == [0.0, 0.25, 1.0, 0.5] && birth.velocity.z > 0.0), "a rising sprite throws upward in Unreal axes");
        assert!(births.iter().all(|birth| birth.kind == PieceKind::Flat && birth.seconds == throw_of(Detail::Shard).seconds));
    }

    #[test]
    fn sparks_throw_pieces_and_no_droplets() {
        let mut spawner = Spawner::default();
        let mut images = bevy::asset::Assets::<Image>::default();
        spawner.textures.push(images.add(Image::default()));
        spawner.sources.insert((Entity::PLACEHOLDER, 0), Source { detail: Detail::Spark, slot: 0 });
        let emitter = DrawnEmitter { sprites: vec![rising_sprite()], ..DrawnEmitter::default() };
        let actor = DrawnActor { parent: Entity::PLACEHOLDER, drawn: true, emitters: vec![emitter], ..DrawnActor::default() };
        let drawn = DrawnParticles { actors: vec![actor], ..DrawnParticles::default() };
        let mut births = Vec::new();
        let droplets = spawner.throw(&drawn, 5, Some(Vec3::ZERO), 0.02, |_, _| Vec3::ONE, &mut births);
        assert!(droplets.is_empty());
        assert_eq!(births.len() as u32, droplets_per_sprite(5, Detail::Spark));
        assert!(births.iter().all(|birth| birth.kind == PieceKind::Spark && birth.seconds == throw_of(Detail::Spark).seconds));
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
        assert_eq!(droplets.len() as u32, droplets_per_sprite(5, Detail::Chunk), "the blood throws droplets");
        assert_eq!(births.len() as u32, 2 * droplets_per_sprite(5, Detail::Head));
        assert!(births.iter().all(|birth| birth.kind == PieceKind::Lump && birth.seconds == throw_of(Detail::Head).seconds));
    }

    #[test]
    fn ring_writes_wrap_and_keep_the_newest() {
        assert_eq!(ring_writes(2, 8, 3), (vec![RingWrite { slot: 2, from: 0, len: 3 }], 5));
        assert_eq!(ring_writes(6, 8, 4), (vec![RingWrite { slot: 6, from: 0, len: 2 }, RingWrite { slot: 0, from: 2, len: 2 }], 2));
        assert_eq!(ring_writes(3, 4, 10), (vec![RingWrite { slot: 3, from: 6, len: 1 }, RingWrite { slot: 0, from: 7, len: 3 }], 3));
        assert_eq!(ring_writes(5, 8, 0), (Vec::new(), 5));
        assert_eq!(ring_writes(5, 0, 3), (Vec::new(), 0));
    }

    #[test]
    fn the_frame_uniform_holds_its_head_and_eight_pushes() {
        let push = push_words(&Blast { at: Vector::new(1.0, 2.0, 3.0), radius: 48.0, push: 450.0 }, Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(push[3..5], [48.0, 450.0]);
        assert_eq!(Vec3::from_slice(&push[..3]), Vec3::from(to_bevy(Vector::new(1.0, 2.0, 3.0))) + Vec3::new(10.0, 0.0, 0.0));
        let bytes = frame_bytes(0.25, 4096, &[push; 9]);
        assert_eq!(bytes.len(), 16 + PUSHES * 32);
        assert_eq!(bytes[4..8], 4096u32.to_le_bytes());
        assert_eq!(bytes[8..12], 8u32.to_le_bytes());
        assert_eq!(frame_bytes(0.25, 4096, &[]).len(), bytes.len());
        assert_eq!(DROPLET_BYTES, 112);
    }
}
