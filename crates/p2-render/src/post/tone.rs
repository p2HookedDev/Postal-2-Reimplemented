use bevy::anti_alias::contrast_adaptive_sharpening::cas;
use bevy::anti_alias::fxaa::fxaa;
use bevy::anti_alias::smaa::smaa;
use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::Handle;
use bevy::camera::{Camera, CameraUpdateSystems, Projection};
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::core_pipeline::tonemapping::tonemapping;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::light::DirectionalLight;
use bevy::math::{Mat4, Vec2, Vec3, Vec4};
use bevy::render::diagnostic::RecordDiagnostics;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BufferId, CachedRenderPipelineId, ColorTargetState, ColorWrites, FilterMode, FragmentState,
    Operations, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerDescriptor, ShaderStages, ShaderType,
    SpecializedRenderPipeline, SpecializedRenderPipelines, TextureFormat, TextureViewId, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use bevy::transform::TransformSystems;
use bevy::transform::components::GlobalTransform;

use crate::world::components::{SceneView, SunShadow};
use crate::gpu::screen_pass::{PassUniformOffset, PassUniforms, ViewBindGroups, filtered_texture_and_sampler, follow_view_pass, prepare_pass_uniforms};
use p2_host::settings::{
    MODERN_CHROMATIC_ABERRATION, MODERN_CONTRAST, MODERN_EXPOSURE, MODERN_GOD_RAYS, MODERN_PHOTO_WARMTH, MODERN_SATURATION, MODERN_VIGNETTE, NEUTRAL_LEVEL, Settings,
};
use crate::gpu::shaders::{add_libraries, add_shaders};

const TONE_SHADER: Handle<Shader> = crate::gpu::shaders::shader_handle("tone.wesl");

pub struct TonePlugin;

impl Plugin for TonePlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_libraries(app);
        add_shaders(app, ["tone.wesl"]);
        app.add_plugins(ExtractComponentPlugin::<ToneEffects>::default())
            .add_systems(PostUpdate, follow_effects.after(TransformSystems::Propagate).after(CameraUpdateSystems));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_gpu_resource::<PassUniforms<ToneEffects>>()
            .init_gpu_resource::<SpecializedRenderPipelines<TonePipeline>>()
            .add_systems(RenderStartup, init_tone_pipeline)
            .add_systems(Render, prepare_tone_pipelines.in_set(RenderSystems::Prepare))
            .add_systems(Render, prepare_pass_uniforms::<ToneEffects>.in_set(RenderSystems::PrepareResources))
            .add_systems(Core3d, draw_tone.in_set(Core3dSystems::PostProcess).after(tonemapping).before(fxaa).before(smaa).before(cas));
    }
}

#[derive(Component, ShaderType, Debug, Clone, Copy, PartialEq, ExtractComponent)]
#[extract_app(RenderApp)]
#[extract_component_sync_target((Self, TonePipelineId, PassUniformOffset<Self>))]
pub struct ToneEffects {
    sun: Vec4,
    tint: Vec4,
    grade: Vec4,
    lens: Vec4,
    balance: Vec4,
}

const WARMTH_PER_LEVEL: Vec3 = Vec3::new(0.03, 0.0, -0.04);
const LUMINANCE: Vec3 = Vec3::new(0.2126, 0.7152, 0.0722);

pub(crate) fn white_balance(warmth: f32) -> Vec3 {
    let gains = Vec3::ONE + WARMTH_PER_LEVEL * warmth;
    gains / gains.dot(LUMINANCE)
}

const RAYS_PER_LEVEL: f32 = 0.08;
const VIGNETTE_PER_LEVEL: f32 = 0.05;
const FRINGE_PER_LEVEL: f32 = 0.0005;
const SATURATION_PER_LEVEL: f32 = 0.1;
const CONTRAST_PER_LEVEL: f32 = 0.05;
const EXPOSURE_STOPS_PER_LEVEL: f32 = 0.2;
const RAYS_FACING: (f32, f32) = (0.1, 0.45);

pub(crate) fn sun_on_screen(clip_from_world: Mat4, forward: Vec3, to_sun: Vec3) -> Option<(Vec2, f32)> {
    let clip = clip_from_world * to_sun.extend(0.0);
    if clip.w <= 0.0 {
        return None;
    }
    let facing = ((forward.dot(to_sun) - RAYS_FACING.0) / (RAYS_FACING.1 - RAYS_FACING.0)).clamp(0.0, 1.0);
    (facing > 0.0).then(|| (Vec2::new(0.5 + 0.5 * clip.x / clip.w, 0.5 - 0.5 * clip.y / clip.w), facing * facing * (3.0 - 2.0 * facing)))
}

fn follow_effects(
    mut commands: Commands,
    settings: Res<Settings>,
    views: Query<(Entity, &Camera, &GlobalTransform, &Projection, Option<&ToneEffects>), With<SceneView>>,
    suns: Query<(&DirectionalLight, &GlobalTransform), With<SunShadow>>,
) {
    let level = |query: &str| f32::from(settings.modern_level(query));
    let around_neutral = |query: &str| level(query) - f32::from(NEUTRAL_LEVEL);
    let rays = level(MODERN_GOD_RAYS) * RAYS_PER_LEVEL;
    let sun = suns.iter().map(|(light, place)| (light.color.to_linear(), place)).find(|(color, _)| color.red + color.green + color.blue > 0.0);
    for (entity, camera, eye, projection, shown) in &views {
        if !settings.modern_graphics() {
            follow_view_pass(commands.entity(entity), shown, None);
            continue;
        }
        let aspect = camera.physical_target_size().map_or(1.0, |size| size.x.max(1) as f32 / size.y.max(1) as f32);
        let mut wanted = ToneEffects {
            sun: Vec4::new(0.5, 0.5, 0.0, aspect),
            tint: Vec4::ONE,
            grade: Vec4::new(
                (around_neutral(MODERN_EXPOSURE) * EXPOSURE_STOPS_PER_LEVEL).exp2(),
                1.0 + around_neutral(MODERN_SATURATION) * SATURATION_PER_LEVEL,
                1.0 + around_neutral(MODERN_CONTRAST) * CONTRAST_PER_LEVEL,
                level(MODERN_VIGNETTE) * VIGNETTE_PER_LEVEL,
            ),
            lens: {
                let panini = match projection {
                    Projection::Perspective(perspective) => crate::world::modern_view::panini(settings.modern_level(p2_host::settings::MODERN_PANINI), perspective.fov, aspect),
                    _ => Vec3::ZERO,
                };
                Vec4::new(level(MODERN_CHROMATIC_ABERRATION) * FRINGE_PER_LEVEL, panini.x, panini.y, panini.z)
            },
            balance: white_balance(if settings.photo_active() { around_neutral(MODERN_PHOTO_WARMTH) } else { 0.0 }).extend(1.0),
        };
        if rays > 0.0
            && let Some((color, place)) = sun
            && let Some((at, facing)) = sun_on_screen(camera.clip_from_view() * eye.to_matrix().inverse(), eye.forward().as_vec3(), place.back().as_vec3())
        {
            let brightest = color.red.max(color.green).max(color.blue);
            wanted.sun = Vec4::new(at.x, at.y, rays * facing, aspect);
            wanted.tint = Vec4::new(color.red / brightest, color.green / brightest, color.blue / brightest, 1.0);
        }
        follow_view_pass(commands.entity(entity), shown, Some(wanted));
    }
}

#[derive(Resource)]
pub struct TonePipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    fullscreen: FullscreenShader,
}

#[derive(Component)]
pub struct TonePipelineId(CachedRenderPipelineId);

fn init_tone_pipeline(mut commands: Commands, render_device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>) {
    let (sampled, filtering) = filtered_texture_and_sampler();
    let layout = BindGroupLayoutDescriptor::new(
        "tone",
        &BindGroupLayoutEntries::sequential(ShaderStages::FRAGMENT, (sampled, filtering, binding_types::uniform_buffer::<ToneEffects>(true))),
    );
    let sampler = render_device.create_sampler(&SamplerDescriptor { label: "tone".into(), mag_filter: FilterMode::Linear, min_filter: FilterMode::Linear, ..Default::default() });
    commands.insert_resource(TonePipeline { layout, sampler, fullscreen: fullscreen.clone() });
}

impl SpecializedRenderPipeline for TonePipeline {
    type Key = TextureFormat;

    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("tone".into()),
            layout: vec![self.layout.clone()],
            vertex: self.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: TONE_SHADER,
                targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })],
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

fn prepare_tone_pipelines(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<TonePipeline>>,
    pipeline: Res<TonePipeline>,
    views: Query<(Entity, &ExtractedView), With<ToneEffects>>,
) {
    for (entity, view) in &views {
        let id = pipelines.specialize(&pipeline_cache, &pipeline, view.target_format);
        commands.entity(entity).insert(TonePipelineId(id));
    }
}

fn draw_tone(
    view: ViewQuery<(&ViewTarget, &TonePipelineId, &PassUniformOffset<ToneEffects>)>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<TonePipeline>,
    buffer: Res<PassUniforms<ToneEffects>>,
    mut groups: Local<ViewBindGroups<(TextureViewId, BufferId)>>,
    mut ctx: RenderContext,
) {
    let entity = view.entity();
    let (target, id, offset) = view.into_inner();
    let (Some(render_pipeline), Some(uniform), Some(uniforms)) =
        (pipeline_cache.get_render_pipeline(id.0), buffer.0.binding(), buffer.0.buffer().map(|uniforms| uniforms.id()))
    else {
        return;
    };
    let post = target.post_process_write();
    let bind_group = groups.get_or_make(entity, (post.source.id(), uniforms), || {
        ctx.render_device().create_bind_group(
            "tone",
            &pipeline_cache.get_bind_group_layout(&pipeline.layout),
            &BindGroupEntries::sequential((post.source, &pipeline.sampler, uniform)),
        )
    });
    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let span = diagnostics.time_span(ctx.command_encoder(), "tone");
    {
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("tone"),
            color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_render_pipeline(render_pipeline);
        pass.set_bind_group(0, bind_group, &[offset.0]);
        pass.draw(0..3, 0..1);
    }
    span.end(ctx.command_encoder());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sun_is_placed_on_the_screen_and_fades_as_the_view_turns_away() {
        let clip_from_world = Mat4::from_cols(Vec4::X, Vec4::Y, Vec4::new(0.0, 0.0, 0.0, -1.0), Vec4::new(0.0, 0.0, 0.1, 0.0));
        let (at, facing) = sun_on_screen(clip_from_world, Vec3::NEG_Z, Vec3::NEG_Z).expect("the sun ahead");
        assert_eq!((at, facing), (Vec2::splat(0.5), 1.0));
        let up_right = Vec3::new(0.3, 0.3, -1.0).normalize();
        let (at, _) = sun_on_screen(clip_from_world, Vec3::NEG_Z, up_right).expect("the sun up and to the right");
        assert!(at.x > 0.5 && at.y < 0.5, "{at}");
        assert_eq!(sun_on_screen(clip_from_world, Vec3::NEG_Z, Vec3::Z), None);
        assert_eq!(sun_on_screen(clip_from_world, Vec3::NEG_Z, Vec3::new(1.0, 0.0, -0.05).normalize()), None);
    }
}
