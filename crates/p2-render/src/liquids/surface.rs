//! The liquid stream's surface, drawn in screen space: the particles splat as sphere impostors into a liquid depth and
//! a thickness, a narrow-range filter smooths the depth, and one composite lights it by the frame behind it, with the
//! sun's glint and the frame's reflection, and colours it by what each liquid absorbs. A puddle's edge and ripples come
//! from a texture generated at startup. Its targets live only while liquid does, and a little after.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Handle, uuid_handle};
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::prepass::ViewPrepassTextures;
use bevy::core_pipeline::schedule::Core3d;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Has, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::light::DirectionalLight;
use bevy::math::{UVec2, Vec3};
use bevy::render::diagnostic::RecordDiagnostics;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::{
    AddressMode, BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, Buffer, BufferId,
    CachedRenderPipelineId, ColorTargetState, ColorWrites, CompareFunction, DepthBiasState, DepthStencilState, Extent3d, FilterMode, FragmentState, Operations, Origin3d, PipelineCache,
    RenderPassColorAttachment, RenderPassDepthStencilAttachment, RenderPassDescriptor, RenderPipeline, RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages, ShaderType,
    SpecializedRenderPipeline, SpecializedRenderPipelines, StencilState, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect, TextureDescriptor, TextureDimension,
    TextureFormat, TextureId, TextureSampleType, TextureUsages, TextureView, TextureViewDescriptor, TextureViewId, VertexState,
    binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::{ExtractedView, ViewTarget, ViewUniform, ViewUniformOffset, ViewUniforms};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use bevy::time::Time;
use bevy::transform::components::GlobalTransform;
use p2_host::display::{MODERN_LIQUIDS, Settings};

use super::{LiquidDeposits, LiquidParticles, LiquidPassSystems};
use crate::components::{SceneView, SunShadow};
use crate::screen_pass::{
    PassUniformOffset, PassUniforms, PassView, ViewBindGroups, add_view_pass, follow_view_pass, half_texture, modern_level, prepare_pass_uniforms, tag_pass_views,
};
use crate::shaders::add_shaders;

const LIQUID_SHADER: Handle<Shader> = uuid_handle!("4f1a9c2e-8b37-4d05-9e6a-2c71b5d8a3f0");
const DEPTH_FORMAT: TextureFormat = TextureFormat::Depth32Float;
/// The nearest particle's bits word, low byte.
const FRONT_FORMAT: TextureFormat = TextureFormat::R8Uint;
/// Eye depth after the narrow-range filter; 0 where no liquid is.
const SMOOTHED_FORMAT: TextureFormat = TextureFormat::R32Float;
/// Optical depth per channel, and the thickness in alpha.
const OPTICAL_FORMAT: TextureFormat = TextureFormat::Rgba16Float;
const FULL_SIZE_FROM_LEVEL: u8 = 8;
/// Every live texture costs each pass on the render thread, yet a stream that stops and starts should not make its
/// targets again.
const LINGER_SECONDS: f32 = 2.0;

const ADD: BlendState = BlendState {
    color: BlendComponent { src_factor: BlendFactor::One, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
    alpha: BlendComponent { src_factor: BlendFactor::One, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
};

pub struct LiquidSurfacePlugin;

impl Plugin for LiquidSurfacePlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [(LIQUID_SHADER, Shader::from_wesl(include_str!("../liquid_surface.wesl"), "p2_render/liquid_surface.wesl"))]);
        add_view_pass::<LiquidSurface>(app);
        app.add_systems(PostUpdate, follow_views.after(tag_pass_views));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<LiquidSurfacePipeline>>()
            .init_gpu_resource::<PassUniforms<LiquidSurface>>()
            .init_gpu_resource::<LiquidGroups>()
            .add_systems(RenderStartup, init_liquid_pipeline)
            .add_systems(Render, (prepare_pass_uniforms::<LiquidSurface>, prepare_liquid_views).in_set(RenderSystems::PrepareResources))
            .add_systems(Core3d, liquid_surface.in_set(LiquidPassSystems::Surface));
    }
}

/// Target texels per side of a full-size pixel's; `None` draws nothing.
fn scale_of(level: u8) -> Option<u32> {
    match level {
        0 => None,
        level if level < FULL_SIZE_FROM_LEVEL => Some(2),
        _ => Some(1),
    }
}

fn target_size(viewport: UVec2, scale: u32) -> Option<UVec2> {
    (viewport.min_element() > 0 && scale > 0).then(|| (viewport + UVec2::splat(scale - 1)) / scale)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Liquid {
    Live,
    Gone,
}

/// Seconds the targets have gone unused after this frame; `None` lets them go, or makes none while no liquid lives.
fn idle_after(idle: Option<f32>, liquid: Liquid, seconds: f32) -> Option<f32> {
    match liquid {
        Liquid::Live => Some(0.0),
        Liquid::Gone => idle.map(|idle| idle + seconds).filter(|&idle| idle < LINGER_SECONDS),
    }
}

/// On a scene camera while liquids are on; `Surface` in `liquid_surface.wesl`. The sun is in linear light, toward it.
#[derive(Component, ShaderType, Debug, Clone, Copy, PartialEq, ExtractComponent)]
#[extract_app(RenderApp)]
#[extract_component_sync_target((Self, PassUniformOffset<Self>, LiquidPipelineIds, LiquidTargets))]
pub struct LiquidSurface {
    sun_direction: Vec3,
    seconds: f32,
    sun_color: Vec3,
    scale: u32,
    delta: f32,
}

/// Only an active, single-sampled view with a depth prepass draws liquid.
fn follow_views(
    mut commands: Commands,
    settings: Option<Res<Settings>>,
    time: Res<Time>,
    views: Query<(Entity, Has<PassView>, Option<&LiquidSurface>), With<SceneView>>,
    suns: Query<(&DirectionalLight, &GlobalTransform), With<SunShadow>>,
) {
    let scale = scale_of(modern_level(settings.as_deref(), MODERN_LIQUIDS));
    let (sun_direction, sun_color) = suns.iter().next().map_or((Vec3::Y, Vec3::ZERO), |(light, place)| {
        let color = light.color.to_linear();
        (-place.forward().as_vec3(), Vec3::new(color.red, color.green, color.blue) * light.illuminance)
    });
    let (seconds, delta) = (time.elapsed_secs_wrapped(), time.delta_secs());
    for (entity, drawn_in, shown) in &views {
        let surface = scale.filter(|_| drawn_in).map(|scale| LiquidSurface { sun_direction, seconds, sun_color, scale, delta });
        follow_view_pass(commands.entity(entity), shown, surface);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Splat,
    Thickness,
    NarrowRange,
    Composite(TextureFormat),
}

#[derive(Resource)]
pub struct LiquidSurfacePipeline {
    splat: BindGroupLayoutDescriptor,
    narrow_range: BindGroupLayoutDescriptor,
    composite: BindGroupLayoutDescriptor,
    fullscreen: FullscreenShader,
    puddle: TextureView,
    puddle_sampler: Sampler,
}

#[derive(Component)]
pub struct LiquidPipelineIds {
    splat: CachedRenderPipelineId,
    thickness: CachedRenderPipelineId,
    narrow_range: CachedRenderPipelineId,
    composite: CachedRenderPipelineId,
}

/// Held apart from the systems so the targets' release can drop the groups that would keep them alive.
#[derive(Resource, Default)]
pub struct LiquidGroups {
    splat: ViewBindGroups<(BufferId, TextureId, BufferId, BufferId)>,
    deposits: ViewBindGroups<(BufferId, TextureId, BufferId, BufferId)>,
    narrow: ViewBindGroups<(TextureViewId, BufferId, BufferId)>,
    composite: ViewBindGroups<(TextureId, BufferId, BufferId, [TextureViewId; 4])>,
}

#[derive(Component)]
pub struct LiquidTargets {
    depth: CachedTexture,
    front: CachedTexture,
    smoothed: CachedTexture,
    optical: CachedTexture,
    idle: f32,
}

/// Texels a side of the generated puddle texture.
const PUDDLE_TEXELS: usize = 128;
const OCTAVES: u32 = 4;
/// A ripple's slope a unit of its height's change from texel to texel.
const RIPPLE_GAIN: f32 = 8.0;

/// A lattice value in 0..1 that repeats every `period` cells.
fn lattice(x: i32, y: i32, period: i32, seed: u32) -> f32 {
    let (x, y) = (x.rem_euclid(period) as u32, y.rem_euclid(period) as u32);
    let mut h = x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xFFFF) as f32 / 65535.0
}

/// Smooth value noise over the unit square, `period` cells a side, tiling.
fn tiled_noise(u: f32, v: f32, period: i32, seed: u32) -> f32 {
    let (x, y) = (u * period as f32, v * period as f32);
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let ease = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (ease(x - x.floor()), ease(y - y.floor()));
    let row = |j: i32| lattice(ix, iy + j, period, seed) * (1.0 - sx) + lattice(ix + 1, iy + j, period, seed) * sx;
    row(0) * (1.0 - sy) + row(1) * sy
}

/// Octaves from `period` up, each half as strong as the last; 0..1.
fn fbm(u: f32, v: f32, period: i32, seed: u32) -> f32 {
    let (sum, weight) = (0..OCTAVES).fold((0.0, 0.0), |(sum, weight), octave| {
        let strength = 0.5f32.powi(octave as i32);
        (sum + tiled_noise(u, v, period << octave, seed + octave) * strength, weight + strength)
    });
    sum / weight
}

/// The puddles' tiling texture, RGBA8: r an organic shape from domain-warped noise, stretched to 0..1; g and b a
/// ripple's slope across and along, 0.5 for flat; a the ripple's height.
fn puddle_texels() -> Vec<u8> {
    let n = PUDDLE_TEXELS;
    let at = |i: usize| (i as f32 + 0.5) / n as f32;
    let grid = |field: &dyn Fn(f32, f32) -> f32| -> Vec<f32> { (0..n * n).map(|i| field(at(i % n), at(i / n))).collect() };
    let shape = grid(&|u, v| fbm(u + 0.5 * fbm(u, v, 2, 1), v + 0.5 * fbm(u, v, 2, 2), 3, 3));
    let ripple = grid(&|u, v| fbm(u, v, 8, 7));
    let (low, high) = shape.iter().fold((f32::MAX, f32::MIN), |(low, high), &s| (low.min(s), high.max(s)));
    let byte = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    let height = |x: usize, y: usize| ripple[(y % n) * n + x % n];
    (0..n * n)
        .flat_map(|i| {
            let (x, y) = (i % n, i / n);
            let across = (height(x + 1, y) - height(x + n - 1, y)) * RIPPLE_GAIN;
            let along = (height(x, y + 1) - height(x, y + n - 1)) * RIPPLE_GAIN;
            [byte((shape[i] - low) / (high - low).max(1e-6)), byte(0.5 + 0.5 * across), byte(0.5 + 0.5 * along), byte(ripple[i])]
        })
        .collect()
}

fn init_liquid_pipeline(mut commands: Commands, fullscreen: Res<FullscreenShader>, device: Res<RenderDevice>, queue: Res<RenderQueue>) {
    let size = Extent3d { width: PUDDLE_TEXELS as u32, height: PUDDLE_TEXELS as u32, depth_or_array_layers: 1 };
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("liquid puddle"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
        &puddle_texels(),
        TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size.width * 4), rows_per_image: None },
        size,
    );
    let puddle = texture.create_view(&TextureViewDescriptor::default());
    let puddle_sampler = device.create_sampler(&SamplerDescriptor {
        label: "liquid puddle".into(),
        address_mode_u: AddressMode::Repeat,
        address_mode_v: AddressMode::Repeat,
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        ..Default::default()
    });
    let pool = || binding_types::texture_2d(TextureSampleType::Float { filterable: true });
    let pool_sampler = || binding_types::sampler(SamplerBindingType::Filtering);
    let depth = || binding_types::texture_2d(TextureSampleType::Depth);
    let view = || binding_types::uniform_buffer::<ViewUniform>(true);
    let surface = || binding_types::uniform_buffer::<LiquidSurface>(true);
    let splat = BindGroupLayoutDescriptor::new(
        "liquid splat",
        &BindGroupLayoutEntries::with_indices(
            ShaderStages::VERTEX_FRAGMENT,
            ((0, binding_types::storage_buffer_read_only_sized(false, None)), (1, depth()), (2, view()), (3, surface()), (9, pool()), (10, pool_sampler())),
        ),
    );
    let narrow_range = BindGroupLayoutDescriptor::new(
        "liquid narrow range",
        &BindGroupLayoutEntries::with_indices(ShaderStages::FRAGMENT, ((2, view()), (3, surface()), (4, depth()))),
    );
    let composite = BindGroupLayoutDescriptor::new(
        "liquid composite",
        &BindGroupLayoutEntries::with_indices(
            ShaderStages::FRAGMENT,
            (
                (1, depth()),
                (2, view()),
                (3, surface()),
                (5, binding_types::texture_2d(TextureSampleType::Float { filterable: false })),
                (6, binding_types::texture_2d(TextureSampleType::Float { filterable: false })),
                (7, binding_types::texture_2d(TextureSampleType::Uint)),
                (8, binding_types::texture_2d(TextureSampleType::Float { filterable: false })),
                (9, pool()),
                (10, pool_sampler()),
            ),
        ),
    );
    commands.insert_resource(LiquidSurfacePipeline { splat, narrow_range, composite, fullscreen: fullscreen.clone(), puddle, puddle_sampler });
}

impl SpecializedRenderPipeline for LiquidSurfacePipeline {
    type Key = Stage;

    fn specialize(&self, stage: Stage) -> RenderPipelineDescriptor {
        let impostors = || VertexState { shader: LIQUID_SHADER, entry_point: Some("splat_vertex".into()), ..Default::default() };
        let (layout, vertex, entry, format, blend, depth_stencil) = match stage {
            Stage::Splat => {
                let nearest = DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(CompareFunction::Greater),
                    stencil: StencilState::default(),
                    bias: DepthBiasState::default(),
                };
                (&self.splat, impostors(), "splat", FRONT_FORMAT, None, Some(nearest))
            }
            Stage::Thickness => (&self.splat, impostors(), "thickness", OPTICAL_FORMAT, Some(ADD), None),
            Stage::NarrowRange => (&self.narrow_range, self.fullscreen.to_vertex_state(), "narrow_range", SMOOTHED_FORMAT, None, None),
            Stage::Composite(format) => (&self.composite, self.fullscreen.to_vertex_state(), "composite", format, None, None),
        };
        RenderPipelineDescriptor {
            label: Some("liquid surface".into()),
            layout: vec![layout.clone()],
            vertex,
            fragment: Some(FragmentState {
                shader: LIQUID_SHADER,
                entry_point: Some(entry.into()),
                targets: vec![Some(ColorTargetState { format, blend, write_mask: ColorWrites::ALL })],
                ..Default::default()
            }),
            depth_stencil,
            ..Default::default()
        }
    }
}

/// The pipelines are asked for while liquids are on, so they are built before the first stream.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn prepare_liquid_views(
    mut commands: Commands,
    cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<LiquidSurfacePipeline>>,
    pipeline: Res<LiquidSurfacePipeline>,
    mut textures: ResMut<TextureCache>,
    device: Res<RenderDevice>,
    particles: Option<Res<LiquidParticles>>,
    deposits: Option<Res<LiquidDeposits>>,
    views: Query<(Entity, &ExtractedView, &LiquidSurface, Option<&LiquidTargets>)>,
    mut groups: ResMut<LiquidGroups>,
) {
    let liquid = if particles.is_some_and(|particles| particles.live > 0) || deposits.is_some() { Liquid::Live } else { Liquid::Gone };
    for (entity, view, surface, targets) in &views {
        let mut stage = |stage| pipelines.specialize(&cache, &pipeline, stage);
        let ids = LiquidPipelineIds {
            splat: stage(Stage::Splat),
            thickness: stage(Stage::Thickness),
            narrow_range: stage(Stage::NarrowRange),
            composite: stage(Stage::Composite(view.target_format)),
        };
        let mut view_entity = commands.entity(entity);
        view_entity.insert(ids);
        let size = target_size(UVec2::new(view.viewport.z, view.viewport.w), surface.scale);
        match (idle_after(targets.map(|targets| targets.idle), liquid, surface.delta), size) {
            (Some(idle), Some(size)) => {
                let mut target = |label: &'static str, format: TextureFormat| half_texture(&mut textures, &device, label, format, size);
                view_entity.insert(LiquidTargets {
                    depth: target("liquid depth", DEPTH_FORMAT),
                    front: target("liquid front", FRONT_FORMAT),
                    smoothed: target("liquid smoothed depth", SMOOTHED_FORMAT),
                    optical: target("liquid optical depth", OPTICAL_FORMAT),
                    idle,
                });
            }
            _ if targets.is_some() => {
                view_entity.remove::<LiquidTargets>();
                groups.splat.forget(entity);
                groups.deposits.forget(entity);
                groups.narrow.forget(entity);
                groups.composite.forget(entity);
            }
            _ => {}
        }
    }
}

/// A pass into one of the liquid's own targets, cleared first, drawing each bind group given with its vertex count.
fn offscreen_pass(
    ctx: &mut RenderContext<'_, '_>,
    label: &'static str,
    pipeline: &RenderPipeline,
    color: &TextureView,
    depth: Option<&TextureView>,
    offsets: &[u32],
    draws: &[Option<(&BindGroup, u32)>],
) {
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(RenderPassColorAttachment { view: color, depth_slice: None, resolve_target: None, ops: Operations::default() })],
        depth_stencil_attachment: depth.map(|view| RenderPassDepthStencilAttachment { view, depth_ops: Some(Operations::default()), stencil_ops: None }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(pipeline);
    for &(bind_group, vertices) in draws.iter().flatten() {
        pass.set_bind_group(0, bind_group, offsets);
        pass.draw(0..vertices, 0..1);
    }
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn liquid_surface(
    view: ViewQuery<(&ViewTarget, &ViewPrepassTextures, &ViewUniformOffset, &PassUniformOffset<LiquidSurface>, &LiquidPipelineIds, &LiquidTargets)>,
    particles: Option<Res<LiquidParticles>>,
    deposits: Option<Res<LiquidDeposits>>,
    cache: Res<PipelineCache>,
    pipeline: Res<LiquidSurfacePipeline>,
    view_uniforms: Res<ViewUniforms>,
    surfaces: Res<PassUniforms<LiquidSurface>>,
    mut groups: ResMut<LiquidGroups>,
    mut ctx: RenderContext,
) {
    let particles = particles.filter(|particles| particles.live > 0);
    if particles.is_none() && deposits.is_none() {
        return;
    }
    let entity = view.entity();
    let (target, prepass, view_offset, surface_offset, ids, targets) = view.into_inner();
    let (Some(splat), Some(thickness), Some(narrow_range), Some(composite)) = (
        cache.get_render_pipeline(ids.splat),
        cache.get_render_pipeline(ids.thickness),
        cache.get_render_pipeline(ids.narrow_range),
        cache.get_render_pipeline(ids.composite),
    ) else {
        return;
    };
    let (Some(depth), Some(depth_texture), Some(view_binding), Some(view_buffer), Some(surface_binding), Some(surface_buffer)) = (
        prepass.depth_only_view(),
        prepass.depth.as_ref().map(|depth| depth.texture.texture.id()),
        view_uniforms.uniforms.binding(),
        view_uniforms.uniforms.buffer().map(|buffer| buffer.id()),
        surfaces.0.binding(),
        surfaces.0.buffer().map(|buffer| buffer.id()),
    ) else {
        return;
    };
    let (liquid_depth, front, smoothed, optical) = (&targets.depth.default_view, &targets.front.default_view, &targets.smoothed.default_view, &targets.optical.default_view);
    let device = ctx.render_device();
    let LiquidGroups { splat: splat_groups, deposits: deposit_groups, narrow: narrow_groups, composite: composite_groups } = &mut *groups;
    let splat_group = |buffer: &Buffer| {
        device.create_bind_group(
            "liquid splat",
            &cache.get_bind_group_layout(&pipeline.splat),
            &BindGroupEntries::with_indices(((0, buffer.as_entire_binding()), (1, depth), (2, view_binding.clone()), (3, surface_binding.clone()), (9, &pipeline.puddle), (10, &pipeline.puddle_sampler))),
        )
    };
    let stream_draw = particles.as_ref().map(|particles| {
        let group = splat_groups.get_or_make(entity, (particles.buffer.id(), depth_texture, view_buffer, surface_buffer), || splat_group(&particles.buffer));
        (group, particles.live.min(particles.capacity).saturating_mul(6))
    });
    let deposit_draw = deposits.as_ref().map(|deposits| {
        let group = deposit_groups.get_or_make(entity, (deposits.buffer.id(), depth_texture, view_buffer, surface_buffer), || splat_group(&deposits.buffer));
        (group, deposits.count.saturating_mul(6))
    });
    let splat_draws = [stream_draw, deposit_draw];
    let narrow_group = narrow_groups.get_or_make(entity, (liquid_depth.id(), view_buffer, surface_buffer), || {
        device.create_bind_group(
            "liquid narrow range",
            &cache.get_bind_group_layout(&pipeline.narrow_range),
            &BindGroupEntries::with_indices(((2, view_binding.clone()), (3, surface_binding.clone()), (4, liquid_depth))),
        )
    });
    let post = target.post_process_write();
    let composite_group = composite_groups.get_or_make(entity, (depth_texture, view_buffer, surface_buffer, [smoothed.id(), optical.id(), front.id(), post.source.id()]), || {
        device.create_bind_group(
            "liquid composite",
            &cache.get_bind_group_layout(&pipeline.composite),
            &BindGroupEntries::with_indices(((1, depth), (2, view_binding), (3, surface_binding), (5, smoothed), (6, optical), (7, front), (8, post.source), (9, &pipeline.puddle), (10, &pipeline.puddle_sampler))),
        )
    });
    let offsets = [view_offset.offset, surface_offset.0];
    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let span = diagnostics.time_span(ctx.command_encoder(), "liquid_surface");
    offscreen_pass(&mut ctx, "liquid splat", splat, front, Some(liquid_depth), &offsets, &splat_draws);
    offscreen_pass(&mut ctx, "liquid thickness", thickness, optical, None, &offsets, &splat_draws);
    offscreen_pass(&mut ctx, "liquid narrow range", narrow_range, smoothed, None, &offsets, &[Some((narrow_group, 3))]);
    {
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("liquid composite"),
            color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_render_pipeline(composite);
        pass.set_bind_group(0, composite_group, &offsets);
        pass.draw(0..3, 0..1);
    }
    span.end(ctx.command_encoder());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::liquids::{LIQUID_BURNING, LIQUID_DEPOSIT, LIQUID_KIND_MASK, LIQUID_LANDED};
    use crate::shaders::shader_const;

    #[test]
    fn half_size_below_level_eight_full_from_it_none_at_zero() {
        assert_eq!(scale_of(0), None);
        assert_eq!(scale_of(1), Some(2));
        assert_eq!(scale_of(7), Some(2));
        assert_eq!(scale_of(8), Some(1));
        assert_eq!(scale_of(10), Some(1));
        assert_eq!(target_size(UVec2::new(1921, 1080), 2), Some(UVec2::new(961, 540)));
        assert_eq!(target_size(UVec2::new(1920, 1080), 1), Some(UVec2::new(1920, 1080)));
        assert_eq!(target_size(UVec2::new(0, 1080), 2), None);
    }

    #[test]
    fn targets_are_made_by_liquid_and_let_go_a_while_after_it() {
        assert_eq!(idle_after(None, Liquid::Gone, 0.016), None, "no liquid makes no targets");
        assert_eq!(idle_after(None, Liquid::Live, 0.016), Some(0.0));
        assert_eq!(idle_after(Some(1.0), Liquid::Live, 0.016), Some(0.0), "liquid again keeps them fresh");
        assert_eq!(idle_after(Some(0.0), Liquid::Gone, 0.5), Some(0.5));
        assert_eq!(idle_after(Some(LINGER_SECONDS - 0.1), Liquid::Gone, 0.2), None);
    }

    #[test]
    fn the_shader_reads_the_bits_and_the_uniform_as_written() {
        let shader = include_str!("../liquid_surface.wesl");
        assert_eq!(shader_const(shader, "KIND_MASK"), f64::from(LIQUID_KIND_MASK));
        assert_eq!(shader_const(shader, "LANDED"), f64::from(LIQUID_LANDED));
        assert_eq!(shader_const(shader, "BURNING"), f64::from(LIQUID_BURNING));
        assert_eq!(shader_const(shader, "DEPOSIT"), f64::from(LIQUID_DEPOSIT));
        assert_eq!(shader_const(shader, "KINDS"), 9.0, "a row for each FluidType byte and one for none");
        assert_eq!(shader_const(shader, "LANE"), f64::from(super::super::stream::LANE));
        let stream = include_str!("../liquid_stream.wesl");
        for name in ["BROKEN", "AGE_SHIFT", "AGE_UNITS", "LINK_GAP"] {
            assert_eq!(shader_const(shader, name), shader_const(stream, name), "{name}: the ribbon links as the stream does");
        }
    }

    #[test]
    fn the_puddle_texture_spans_its_shape_and_tiles() {
        let texels = puddle_texels();
        let n = PUDDLE_TEXELS;
        assert_eq!(texels.len(), n * n * 4);
        let shape = |x: usize, y: usize| i32::from(texels[(y * n + x) * 4]);
        assert_eq!(texels.chunks(4).map(|texel| texel[0]).min(), Some(0));
        assert_eq!(texels.chunks(4).map(|texel| texel[0]).max(), Some(255));
        let inside = (0..n).flat_map(|y| (1..n).map(move |x| (x, y))).map(|(x, y)| (shape(x, y) - shape(x - 1, y)).abs()).max().unwrap_or(0);
        for y in 0..n {
            assert!((shape(0, y) - shape(n - 1, y)).abs() <= 2 * inside, "row {y} has a seam where the texture wraps");
        }
        assert_eq!(LiquidSurface::min_size().get(), 48);
    }
}
