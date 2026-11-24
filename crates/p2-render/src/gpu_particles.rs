//! Modern graphics: detail particles. The droplets the scripts' chunks throw live in a ring on the GPU, which moves
//! them, bounces them on the depth prepass and draws them. They are seen only; nothing reads them back.

use std::collections::VecDeque;

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{AssetId, Handle, uuid_handle};
use bevy::core_pipeline::prepass::ViewPrepassTextures;
use bevy::core_pipeline::schedule::Core3d;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Has, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::SystemParam;
use bevy::image::Image;
use bevy::math::{Vec3, Vec4};
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    AddressMode, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, Buffer,
    BufferDescriptor, BufferId, BufferInitDescriptor, BufferUsages, CachedComputePipelineId, CachedRenderPipelineId, ColorTargetState, ColorWrites,
    ComputePassDescriptor, ComputePipelineDescriptor, FilterMode, FragmentState, PipelineCache, RenderPassDescriptor, RenderPipelineDescriptor, Sampler,
    SamplerBindingType, SamplerDescriptor, ShaderStages, SpecializedRenderPipeline, SpecializedRenderPipelines, TextureFormat, TextureId, TextureSampleType,
    TextureViewId, VertexState, binding_types,
};
use bevy::render::render_resource::ShaderType;
use bevy::render::render_resource::encase::ShaderSize;
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::{FallbackImage, GpuImage};
use bevy::render::view::{ExtractedView, ViewTarget, ViewUniform, ViewUniformOffset, ViewUniforms};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use bevy::time::Time;
use p2_bevy_assets::to_bevy;
use p2_host::display::{MODERN_GPU_PARTICLES, MODERN_PARTICLE_LIGHT, Settings, modern_on, modern_turned_off};
use p2_math::Vector;

use crate::MainPassExtraSystems;
use crate::components::SceneView;
use crate::debris::PieceBirths;
use crate::frame_facts::FrameFacts;
use crate::level_collision::LevelCollision;
use crate::map_lights::MapLights;
use crate::particle_draw::sprites::lamps_shown;
use crate::particle_draw::{ParticleDrawSystems, ParticleDraws, draw_particles};
use crate::particle_frame::{Blast, DrawnParticles, push_from_person_hits};
use crate::particle_throw::{LONGEST_DROPLET_LIFE, Spawner};
use crate::screen_pass::{PassView, ViewBindGroups, add_view_pass, follow_view_pass, modern_level, shader_bytes, tag_pass_views};
use crate::shaders::{add_libraries, add_shaders, shader_file};

const DROPLET_SHADER: Handle<Shader> = uuid_handle!("7c4e2a91-5b38-4d6f-a0e7-3f9b1c82d465");
/// Matches the textures bound in `gpu_particles.wesl`.
pub(crate) const TEXTURE_SLOTS: usize = 8;
const DROPLET_BYTES: u64 = Droplet::SHADER_SIZE.get();
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
/// Seconds the pool is still stepped and drawn after its last droplet should have died.
const LIFE_MARGIN: f64 = 1.0;

/// Premultiplied colour over the frame; the frame's alpha, the sun rays' mask, is kept.
pub(crate) const OVER: BlendState = BlendState {
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
        add_libraries(app);
        add_shaders(app, [(DROPLET_SHADER, shader_file("gpu_particles.wesl"))]);
        add_view_pass::<GpuDroplets>(app);
        app.init_resource::<Spawner>().init_resource::<PieceBirths>().add_systems(
            PostUpdate,
            (
                (note_sources.run_if(modern_on(MODERN_GPU_PARTICLES)), (|mut spawner: ResMut<Spawner>| spawner.forget()).run_if(modern_turned_off(MODERN_GPU_PARTICLES)))
                    .before(draw_particles),
                push_from_person_hits.after(draw_particles),
                throw_droplets.after(tag_pass_views),
            )
                .chain()
                .in_set(GpuParticleSystems)
                .in_set(ParticleDrawSystems),
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

/// `Droplet` in `gpu_particles.wesl`.
#[derive(ShaderType, Clone, Copy, Debug, PartialEq)]
pub(crate) struct Droplet {
    /// Age in seconds in `w`.
    pub(crate) place: Vec4,
    /// Lifetime in seconds in `w`.
    pub(crate) velocity: Vec4,
    /// Half size in `w`.
    pub(crate) colour: Vec4,
    pub(crate) rect: Vec4,
    /// Texture slot, an unused word, turn in radians, gravity.
    pub(crate) shape: Vec4,
    /// The normal of the surface it lies on; zero in the air.
    pub(crate) rest: Vec4,
}

/// `Push` in `gpu_particles.wesl`: the place as the droplets' and the reach in `w`, then the speed and the hit's way.
#[derive(ShaderType, Clone, Copy, Debug, Default, PartialEq)]
struct Push {
    place: Vec4,
    speed: Vec4,
}

/// `Frame` in `gpu_particles.wesl`: `count` slots from `first` on, around the ring, may hold a droplet that lives.
#[derive(ShaderType)]
struct Frame {
    seconds: f32,
    count: u32,
    pushes: u32,
    first: u32,
    push: [Push; PUSHES],
}

pub(crate) fn pool_size(level: u8) -> u32 {
    (POOL_PER_LEVEL * u32::from(level)).min(POOL_CAP)
}

/// On a scene camera while detail particles are on: the pool's size, the map it belongs to, the frame's seconds, the
/// droplets born this frame and the textures by slot.
#[derive(Component, Debug, Clone, PartialEq, ExtractComponent)]
#[extract_app(RenderApp)]
#[extract_component_sync_target((Self, DropletPool))]
pub struct GpuDroplets {
    pool: u32,
    map: u32,
    seconds: f32,
    newborn: Vec<Droplet>,
    pushes: Vec<Push>,
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

fn note_sources(drawn: Option<Res<DrawnParticles>>, mut spawner: ResMut<Spawner>) {
    match drawn.as_deref() {
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
    views: Query<(Entity, Has<PassView>, Option<&GpuDroplets>), With<SceneView>>,
    mut spawner: ResMut<Spawner>,
    mut births: ResMut<PieceBirths>,
    room: RoomLight,
) {
    let level = modern_level(settings.as_deref(), MODERN_GPU_PARTICLES);
    let any_view = level > 0 && views.iter().any(|(_, drawn_in, _)| drawn_in);
    // With the particles' light off a sprite is drawn as its texture is, and so is its chunk.
    let lit = facts.in_play && modern_level(settings.as_deref(), MODERN_PARTICLE_LIGHT) > 0;
    let light_at = |parent: Entity, location: Vector| room.at(parent, location).filter(|_| lit).unwrap_or(Vec3::ONE);
    let newborn = match drawn.as_deref() {
        Some(drawn) if any_view => spawner.throw(drawn, level, facts.in_play.then_some(facts.origin), facts.game_step, light_at, &mut births.0),
        _ => Vec::new(),
    };
    let seconds = facts.capped_step(time.delta_secs(), 0.1);
    let (pool, map) = (pool_size(level), drawn.as_deref().map_or(0, |drawn| drawn.level));
    let pushes: Vec<Push> = drawn.as_deref().map_or(&[][..], |drawn| &drawn.blasts).iter().take(PUSHES).map(|blast| push_of(blast, facts.origin)).collect();
    let textures: Vec<AssetId<Image>> = spawner.textures.iter().map(Handle::id).collect();
    for (entity, drawn_in, shown) in &views {
        let droplets = (level > 0 && drawn_in).then(|| GpuDroplets { pool, map, seconds, newborn: newborn.clone(), pushes: pushes.clone(), textures: textures.clone() });
        follow_view_pass(commands.entity(entity), shown, droplets);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RingWrite {
    pub(crate) slot: u32,
    pub(crate) from: usize,
    pub(crate) len: usize,
}

/// Where `count` newborn droplets go in a ring of `size` slots from `cursor`, and the next cursor; when more are born
/// than the ring holds, the newest are kept.
pub(crate) fn ring_writes(cursor: u32, size: u32, count: usize) -> (Vec<RingWrite>, u32) {
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

fn push_of(blast: &Blast, origin: Vec3) -> Push {
    let [wx, wy, wz] = to_bevy(blast.way);
    Push { place: (Vec3::from(to_bevy(blast.at)) + origin).extend(blast.radius), speed: Vec4::new(blast.push, wx, wy, wz) }
}

fn frame_bytes(seconds: f32, (first, count): (u32, u32), pushes: &[Push]) -> Vec<u8> {
    let push = std::array::from_fn(|at| pushes.get(at).copied().unwrap_or_default());
    shader_bytes(&Frame { seconds, count, pushes: pushes.len().min(PUSHES) as u32, first, push })
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
    lives: Lives,
    /// The first slot and how many after it, around the ring, may hold a droplet that lives.
    span: (u32, u32),
}

/// When the pool's droplets die, on a clock of the seconds the droplets were stepped with, so a pause stops it: each
/// frame's births, oldest first, with the clock past which all of them are dead.
#[derive(Clone, Debug, Default, PartialEq)]
struct Lives {
    clock: f64,
    births: VecDeque<(f64, u32)>,
}

impl Lives {
    fn step(&mut self, seconds: f32, born: usize) {
        self.clock += f64::from(seconds);
        if born > 0 {
            self.births.push_back((self.clock + f64::from(LONGEST_DROPLET_LIFE) + LIFE_MARGIN, u32::try_from(born).unwrap_or(u32::MAX)));
        }
        while self.births.front().is_some_and(|&(dead, _)| self.clock > dead) {
            self.births.pop_front();
        }
    }

    /// How many slots just before the ring's cursor may still hold a droplet that lives.
    fn live(&self, size: u32) -> u32 {
        self.births.iter().fold(0_u32, |sum, &(_, born)| sum.saturating_add(born)).min(size)
    }
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
        let Some(mut pool) = pool.filter(|pool| pool.size == droplets.pool && pool.map == droplets.map) else {
            let buffer = device.create_buffer(&BufferDescriptor {
                label: Some("gpu particles"),
                size: u64::from(droplets.pool) * DROPLET_BYTES,
                usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let frame = device.create_buffer_with_data(&BufferInitDescriptor {
                label: Some("gpu particles frame"),
                contents: &frame_bytes(droplets.seconds, (0, 0), &droplets.pushes),
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            });
            commands.entity(entity).insert(DropletPool { buffer, frame, draw, size: droplets.pool, map: droplets.map, cursor: 0, lives: Lives::default(), span: (0, 0) });
            continue;
        };
        let bytes = shader_bytes(droplets.newborn.as_slice());
        let (writes, cursor) = ring_writes(pool.cursor, pool.size, droplets.newborn.len());
        let stride = DROPLET_BYTES as usize;
        for RingWrite { slot, from, len } in writes {
            if let Some(range) = bytes.get(from * stride..(from + len) * stride) {
                queue.write_buffer(&pool.buffer, u64::from(slot) * DROPLET_BYTES, range);
            }
        }
        pool.lives.step(droplets.seconds, droplets.newborn.len());
        let live = pool.lives.live(pool.size);
        (pool.cursor, pool.draw, pool.span) = (cursor, draw, ((cursor + pool.size - live) % pool.size.max(1), live));
        queue.write_buffer(&pool.frame, 0, &frame_bytes(droplets.seconds, pool.span, &droplets.pushes));
    }
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn gpu_particles(
    view: ViewQuery<(&ViewTarget, &ViewPrepassTextures, &ViewUniformOffset, &DropletPool, &GpuDroplets)>,
    cache: Res<PipelineCache>,
    pipeline: Res<DropletPipeline>,
    view_uniforms: Res<ViewUniforms>,
    images: Res<RenderAssets<GpuImage>>,
    fallback: Res<FallbackImage>,
    mut stepped_groups: Local<ViewBindGroups<(BufferId, TextureId, BufferId, BufferId)>>,
    mut shown_groups: Local<ViewBindGroups<(TextureId, BufferId, BufferId, [TextureViewId; TEXTURE_SLOTS])>>,
    mut ctx: RenderContext,
) {
    let entity = view.entity();
    let (target, prepass, offset, pool, droplets) = view.into_inner();
    let (first, live) = pool.span;
    if live == 0 {
        return;
    }
    let (Some(simulate), Some(draw), Some(depth), Some(depth_texture), Some(uniform), Some(uniforms)) = (
        pipeline.simulate.and_then(|id| cache.get_compute_pipeline(id)),
        cache.get_render_pipeline(pool.draw),
        prepass.depth_only_view(),
        prepass.depth.as_ref().map(|depth| depth.texture.texture.id()),
        view_uniforms.uniforms.binding(),
        view_uniforms.uniforms.buffer().map(|uniforms| uniforms.id()),
    ) else {
        return;
    };
    let texture = |slot: usize| &droplets.textures.get(slot).and_then(|&id| images.get(id)).unwrap_or(&fallback.d2).texture_view;
    let device = ctx.render_device();
    let stepped = stepped_groups.get_or_make(entity, (pool.buffer.id(), depth_texture, uniforms, pool.frame.id()), || {
        device.create_bind_group(
            "gpu particles step",
            &cache.get_bind_group_layout(&pipeline.simulate_layout),
            &BindGroupEntries::sequential((pool.buffer.as_entire_binding(), depth, uniform.clone(), pool.frame.as_entire_binding())),
        )
    });
    let shown = shown_groups.get_or_make(entity, (depth_texture, uniforms, pool.buffer.id(), std::array::from_fn(|slot| texture(slot).id())), || {
        device.create_bind_group(
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
        )
    });
    {
        let mut pass = ctx.command_encoder().begin_compute_pass(&ComputePassDescriptor { label: Some("gpu particles step"), timestamp_writes: None });
        pass.set_pipeline(simulate);
        pass.set_bind_group(0, stepped, &[offset.offset]);
        pass.dispatch_workgroups(live.div_ceil(WORKGROUP), 1, 1);
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
    pass.set_bind_group(0, shown, &[offset.offset]);
    // In the slots' order, as the whole pool was drawn, so overlapping droplets blend as before.
    let end = first + live;
    if end > pool.size {
        pass.draw(0..(end - pool.size) * 6, 0..1);
        pass.draw(first * 6..pool.size * 6, 0..1);
    } else {
        pass.draw(first * 6..end * 6, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::shader_const;

    #[test]
    fn the_shaders_hold_the_same_light_and_push_numbers() {
        let sprites = include_str!("particle.wesl");
        assert_eq!(shader_const(sprites, "PUFF_SUN") as f32, PUFF_SUN);
        assert_eq!(shader_const(sprites, "LIGHT_LIMIT") as f32, LIGHT_LIMIT);
        assert_eq!(shader_const(include_str!("gpu_particles.wesl"), "PUSHES"), PUSHES as f64);
    }

    #[test]
    fn the_pool_grows_with_the_level_up_to_its_cap() {
        assert_eq!(pool_size(0), 0);
        assert_eq!(pool_size(5), 5 * POOL_PER_LEVEL);
        assert_eq!(pool_size(10), POOL_CAP);
        assert_eq!(pool_size(20), POOL_CAP);
    }

    #[test]
    fn the_pool_is_quiet_only_once_its_longest_droplet_is_over() {
        let longest = LONGEST_DROPLET_LIFE;
        let mut lives = Lives::default();
        lives.step(0.1, 0);
        assert_eq!(lives.live(64), 0, "an empty pool is quiet");
        lives.step(0.1, 3);
        assert_eq!(lives.live(64), 3);
        for _ in 0..100 {
            lives.step(0.0, 0);
        }
        assert_eq!(lives.live(64), 3, "a pause does not run the clock out");
        lives.step(longest * 0.5, 2);
        lives.step(longest * 0.5, 0);
        assert_eq!(lives.live(64), 5, "a droplet is never cut short");
        assert_eq!(lives.live(4), 4, "never more than the pool");
        lives.step(1.5, 0);
        assert_eq!(lives.live(64), 2, "the first births are over, the later ones still live");
        lives.step(longest, 0);
        assert_eq!(lives.live(64), 0);
        lives.step(0.1, 1);
        assert_eq!(lives.live(64), 1, "a new droplet wakes the pool");
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
    fn a_push_holds_its_place_reach_and_speed() {
        Frame::assert_uniform_compat();
        let push = push_of(&Blast { at: Vector::new(1.0, 2.0, 3.0), radius: 48.0, push: 450.0, way: Vector::ZERO }, Vec3::new(10.0, 0.0, 0.0));
        assert_eq!((push.place.w, push.speed.x), (48.0, 450.0));
        assert_eq!(push.place.truncate(), Vec3::from(to_bevy(Vector::new(1.0, 2.0, 3.0))) + Vec3::new(10.0, 0.0, 0.0));
    }
}
