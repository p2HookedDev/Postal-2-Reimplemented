//! The liquid stream's surface, drawn in screen space: the particles splat as sphere impostors into a liquid depth and
//! a thickness, a narrow-range filter smooths the depth, and one composite lights it from the sun and the view's
//! ambient and colours it by what each liquid absorbs. Its targets live only while liquid does, and a little after.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Handle, uuid_handle};
use bevy::camera::Camera;
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::prepass::{DepthPrepass, ViewPrepassTextures};
use bevy::core_pipeline::schedule::Core3d;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Has, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::query::QueryItem;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::lifetimeless::Read;
use bevy::light::{AmbientLight, DirectionalLight};
use bevy::math::{UVec2, Vec3};
use bevy::render::camera::ExtractedCamera;
use bevy::render::diagnostic::RecordDiagnostics;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, BufferId,
    CachedRenderPipelineId, ColorTargetState, ColorWrites, CompareFunction, DepthBiasState, DepthStencilState, FragmentState, Operations, PipelineCache,
    RenderPassColorAttachment, RenderPassDepthStencilAttachment, RenderPassDescriptor, RenderPipelineDescriptor, ShaderStages, ShaderType,
    SpecializedRenderPipeline, SpecializedRenderPipelines, StencilState, TextureFormat, TextureId, TextureSampleType, TextureView, TextureViewId, VertexState,
    binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::sync_component::SyncComponent;
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::{ExtractedView, Msaa, ViewTarget, ViewUniform, ViewUniformOffset, ViewUniforms};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use bevy::time::Time;
use bevy::transform::components::GlobalTransform;
use p2_host::display::{MODERN_LIQUIDS, Settings};

use super::{LiquidParticles, LiquidPassSystems};
use crate::components::{SceneView, SunShadow};
use crate::gpu_particles::OVER;
use crate::screen_pass::{PassUniformOffset, PassUniforms, ScreenStep, ViewBindGroups, half_texture, modern_level, prepare_pass_uniforms};
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
        app.add_plugins(ExtractComponentPlugin::<LiquidSurface>::default()).add_systems(PostUpdate, follow_views);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<LiquidSurfacePipeline>>()
            .init_gpu_resource::<PassUniforms<LiquidSurface>>()
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

/// On a scene camera while liquids are on; `Surface` in `liquid_surface.wesl`. The sun is in linear light, toward it;
/// the ambient in display light, as the zone's ambient is carried.
#[derive(Component, ShaderType, Debug, Clone, Copy, PartialEq)]
pub struct LiquidSurface {
    sun_direction: Vec3,
    seconds: f32,
    sun_color: Vec3,
    scale: u32,
    ambient: Vec3,
    delta: f32,
}

/// Only an active, single-sampled view with a depth prepass draws liquid.
fn follow_views(
    mut commands: Commands,
    settings: Option<Res<Settings>>,
    time: Res<Time>,
    views: Query<(Entity, &Camera, &Msaa, Option<&AmbientLight>, Has<DepthPrepass>, Has<LiquidSurface>), With<SceneView>>,
    suns: Query<(&DirectionalLight, &GlobalTransform), With<SunShadow>>,
) {
    let scale = scale_of(modern_level(settings.as_deref(), MODERN_LIQUIDS));
    let (sun_direction, sun_color) = suns.iter().next().map_or((Vec3::Y, Vec3::ZERO), |(light, place)| {
        let color = light.color.to_linear();
        (-place.forward().as_vec3(), Vec3::new(color.red, color.green, color.blue) * light.illuminance)
    });
    let (seconds, delta) = (time.elapsed_secs_wrapped(), time.delta_secs());
    for (entity, camera, msaa, ambient, depth, shown) in &views {
        match scale.filter(|_| camera.is_active && matches!(msaa, Msaa::Off) && depth) {
            Some(scale) => {
                let ambient = ambient.map_or(Vec3::ZERO, |ambient| {
                    let color = ambient.color.to_linear();
                    Vec3::new(color.red, color.green, color.blue)
                });
                commands.entity(entity).insert(LiquidSurface { sun_direction, seconds, sun_color, scale, ambient, delta });
            }
            None if shown => {
                commands.entity(entity).remove::<LiquidSurface>();
            }
            None => {}
        }
    }
}

impl SyncComponent<RenderApp> for LiquidSurface {
    type Target = (Self, PassUniformOffset<Self>, LiquidPipelineIds, LiquidTargets);
}

impl ExtractComponent<RenderApp> for LiquidSurface {
    type QueryData = Read<Self>;
    type QueryFilter = ();
    type Out = Self;

    fn extract_component(surface: QueryItem<'_, '_, Self::QueryData>) -> Option<Self::Out> {
        Some(*surface)
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
}

#[derive(Component)]
pub struct LiquidPipelineIds {
    splat: CachedRenderPipelineId,
    thickness: CachedRenderPipelineId,
    narrow_range: CachedRenderPipelineId,
    composite: CachedRenderPipelineId,
}

#[derive(Component)]
pub struct LiquidTargets {
    depth: CachedTexture,
    front: CachedTexture,
    smoothed: CachedTexture,
    optical: CachedTexture,
    idle: f32,
}

fn init_liquid_pipeline(mut commands: Commands, fullscreen: Res<FullscreenShader>) {
    let depth = || binding_types::texture_2d(TextureSampleType::Depth);
    let view = || binding_types::uniform_buffer::<ViewUniform>(true);
    let surface = || binding_types::uniform_buffer::<LiquidSurface>(true);
    let splat = BindGroupLayoutDescriptor::new(
        "liquid splat",
        &BindGroupLayoutEntries::with_indices(
            ShaderStages::VERTEX_FRAGMENT,
            ((0, binding_types::storage_buffer_read_only_sized(false, None)), (1, depth()), (2, view()), (3, surface())),
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
            ),
        ),
    );
    commands.insert_resource(LiquidSurfacePipeline { splat, narrow_range, composite, fullscreen: fullscreen.clone() });
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
            Stage::Composite(format) => (&self.composite, self.fullscreen.to_vertex_state(), "composite", format, Some(OVER), None),
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
    views: Query<(Entity, &ExtractedView, &LiquidSurface, Option<&LiquidTargets>)>,
) {
    let liquid = if particles.is_some_and(|particles| particles.live > 0) { Liquid::Live } else { Liquid::Gone };
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
            }
            _ => {}
        }
    }
}

/// A pass into one of the liquid's own targets, cleared first.
fn offscreen_pass(
    ctx: &mut RenderContext<'_, '_>,
    step: ScreenStep<'_>,
    color: &TextureView,
    depth: Option<&TextureView>,
    offsets: &[u32],
    vertices: u32,
) {
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some(step.label),
        color_attachments: &[Some(RenderPassColorAttachment { view: color, depth_slice: None, resolve_target: None, ops: Operations::default() })],
        depth_stencil_attachment: depth.map(|view| RenderPassDepthStencilAttachment { view, depth_ops: Some(Operations::default()), stencil_ops: None }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(step.pipeline);
    pass.set_bind_group(0, step.bind_group, offsets);
    pass.draw(0..vertices, 0..1);
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn liquid_surface(
    view: ViewQuery<(&ViewTarget, &ExtractedCamera, &ViewPrepassTextures, &ViewUniformOffset, &PassUniformOffset<LiquidSurface>, &LiquidPipelineIds, &LiquidTargets)>,
    particles: Option<Res<LiquidParticles>>,
    cache: Res<PipelineCache>,
    pipeline: Res<LiquidSurfacePipeline>,
    view_uniforms: Res<ViewUniforms>,
    surfaces: Res<PassUniforms<LiquidSurface>>,
    mut splat_groups: Local<ViewBindGroups<(BufferId, TextureId, BufferId, BufferId)>>,
    mut narrow_groups: Local<ViewBindGroups<(TextureViewId, BufferId, BufferId)>>,
    mut composite_groups: Local<ViewBindGroups<(TextureId, BufferId, BufferId, [TextureViewId; 3])>>,
    mut ctx: RenderContext,
) {
    let Some(particles) = particles.filter(|particles| particles.live > 0) else {
        return;
    };
    let entity = view.entity();
    let (target, camera, prepass, view_offset, surface_offset, ids, targets) = view.into_inner();
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
    let splat_group = splat_groups.get_or_make(entity, (particles.buffer.id(), depth_texture, view_buffer, surface_buffer), || {
        device.create_bind_group(
            "liquid splat",
            &cache.get_bind_group_layout(&pipeline.splat),
            &BindGroupEntries::with_indices(((0, particles.buffer.as_entire_binding()), (1, depth), (2, view_binding.clone()), (3, surface_binding.clone()))),
        )
    });
    let narrow_group = narrow_groups.get_or_make(entity, (liquid_depth.id(), view_buffer, surface_buffer), || {
        device.create_bind_group(
            "liquid narrow range",
            &cache.get_bind_group_layout(&pipeline.narrow_range),
            &BindGroupEntries::with_indices(((2, view_binding.clone()), (3, surface_binding.clone()), (4, liquid_depth))),
        )
    });
    let composite_group = composite_groups.get_or_make(entity, (depth_texture, view_buffer, surface_buffer, [smoothed.id(), optical.id(), front.id()]), || {
        device.create_bind_group(
            "liquid composite",
            &cache.get_bind_group_layout(&pipeline.composite),
            &BindGroupEntries::with_indices(((1, depth), (2, view_binding), (3, surface_binding), (5, smoothed), (6, optical), (7, front))),
        )
    });
    let offsets = [view_offset.offset, surface_offset.0];
    let vertices = particles.live.min(particles.capacity).saturating_mul(6);
    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let span = diagnostics.time_span(ctx.command_encoder(), "liquid_surface");
    offscreen_pass(&mut ctx, ScreenStep { label: "liquid splat", pipeline: splat, bind_group: splat_group }, front, Some(liquid_depth), &offsets, vertices);
    offscreen_pass(&mut ctx, ScreenStep { label: "liquid thickness", pipeline: thickness, bind_group: splat_group }, optical, None, &offsets, vertices);
    offscreen_pass(&mut ctx, ScreenStep { label: "liquid narrow range", pipeline: narrow_range, bind_group: narrow_group }, smoothed, None, &offsets, 3);
    {
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("liquid composite"),
            color_attachments: &[Some(target.get_color_attachment())],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(viewport) = camera.viewport.as_ref() {
            pass.set_camera_viewport(viewport);
        }
        pass.set_render_pipeline(composite);
        pass.set_bind_group(0, composite_group, &offsets);
        pass.draw(0..3, 0..1);
    }
    span.end(ctx.command_encoder());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::liquids::{LIQUID_BURNING, LIQUID_KIND_MASK, LIQUID_LANDED};
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
        assert_eq!(shader_const(shader, "KINDS"), 9.0, "a row for each FluidType byte and one for none");
        assert_eq!(LiquidSurface::min_size().get(), 48);
    }
}
