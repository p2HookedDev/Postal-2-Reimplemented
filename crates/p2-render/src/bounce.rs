//! Modern graphics: screen-space bounce light. Between the opaque and the transparent passes a half-size pass gathers
//! the lit colour of nearby surfaces that face each pixel, and a full-size pass brightens the frame by a blur of it
//! that keeps to surfaces of like depth and facing. Within one frame only: nothing is carried over from the last.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Handle, uuid_handle};
use bevy::color::LinearRgba;
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::prepass::ViewPrepassTextures;
use bevy::core_pipeline::schedule::Core3d;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Has, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, BufferId, CachedRenderPipelineId,
    ColorTargetState, ColorWrites, FragmentState, PipelineCache, RenderPipelineDescriptor, ShaderStages, SpecializedRenderPipeline, SpecializedRenderPipelines,
    TextureFormat, TextureId, TextureSampleType, TextureViewId, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::{ExtractedView, ViewTarget, ViewUniform, ViewUniformOffset, ViewUniforms};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;

use crate::components::SceneView;
use p2_host::display::{MODERN_BOUNCE_LIGHT, Settings};
use crate::MainPassExtraSystems;
use crate::screen_pass::{HalfThenFull, PassView, ScreenStep, ViewBindGroups, add_view_pass, follow_view_pass, half_texture, half_viewport, tag_pass_views};
use crate::shaders::{add_libraries, add_shaders};

const BOUNCE_SHADER: Handle<Shader> = uuid_handle!("4f0d2b7e-9c31-4a8e-b6d5-1e7a3c90f2d8");
const GATHERED_FORMAT: TextureFormat = TextureFormat::Rgba16Float;
/// Tuned by eye later (unverified).
const STRENGTH_PER_LEVEL: f32 = 0.05;

/// The gathered light times the blend constant, the strength.
const SCALED: BlendState = BlendState {
    color: BlendComponent { src_factor: BlendFactor::Constant, dst_factor: BlendFactor::Zero, operation: BlendOperation::Add },
    alpha: BlendComponent::REPLACE,
};

/// Adds the frame times the gathered light and keeps the frame's alpha, the sun rays' mask. No pass holds a surface's
/// own colour, so the frame's lit colour stands in for it: a dark or shadowed surface takes little and stays dark.
const ADD_BY_FRAME: BlendState = BlendState {
    color: BlendComponent { src_factor: BlendFactor::Dst, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
    alpha: BlendComponent { src_factor: BlendFactor::Zero, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
};

pub struct BounceLightPlugin;

impl Plugin for BounceLightPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_libraries(app);
        add_shaders(app, [(BOUNCE_SHADER,Shader::from_wesl(include_str!("bounce.wesl"), "p2_render/bounce.wesl"))]);
        add_view_pass::<BounceLight>(app);
        app.add_systems(PostUpdate, follow_bounce_settings.after(tag_pass_views));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<BouncePipeline>>()
            .add_systems(RenderStartup, init_bounce_pipeline)
            .add_systems(Render, prepare_bounce_views.in_set(RenderSystems::PrepareResources))
            .add_systems(Core3d, bounce.in_set(MainPassExtraSystems::Bounce));
    }
}

/// On a scene camera while bounce light is on: its strength.
#[derive(Component, Debug, Clone, Copy, PartialEq, ExtractComponent)]
#[extract_app(RenderApp)]
#[extract_component_sync_target((Self, BounceView))]
pub struct BounceLight(f32);

fn strength(level: u8) -> Option<f32> {
    (level > 0).then(|| f32::from(level) * STRENGTH_PER_LEVEL)
}

fn follow_bounce_settings(mut commands: Commands, settings: Option<Res<Settings>>, views: Query<(Entity, Has<PassView>, Option<&BounceLight>), With<SceneView>>) {
    let wanted = strength(settings.as_ref().map_or(0, |settings| settings.modern_level(MODERN_BOUNCE_LIGHT)));
    for (entity, drawn_in, shown) in &views {
        follow_view_pass(commands.entity(entity), shown, wanted.filter(|_| drawn_in).map(BounceLight));
    }
}

#[derive(Resource)]
pub struct BouncePipeline {
    layout: BindGroupLayoutDescriptor,
    fullscreen: FullscreenShader,
}

#[derive(Component)]
pub struct BounceView {
    gather: CachedRenderPipelineId,
    composite: CachedRenderPipelineId,
    gathered: CachedTexture,
}

fn init_bounce_pipeline(mut commands: Commands, fullscreen: Res<FullscreenShader>) {
    let layout = BindGroupLayoutDescriptor::new(
        "bounce light",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                binding_types::texture_2d(TextureSampleType::Float { filterable: false }),
                binding_types::texture_2d(TextureSampleType::Depth),
                binding_types::texture_2d(TextureSampleType::Float { filterable: false }),
                binding_types::uniform_buffer::<ViewUniform>(true),
            ),
        ),
    );
    commands.insert_resource(BouncePipeline { layout, fullscreen: fullscreen.clone() });
}

/// `None` is the half-size gather; a view's format is the full-size pass that adds the light to that view.
impl SpecializedRenderPipeline for BouncePipeline {
    type Key = Option<TextureFormat>;

    fn specialize(&self, view_format: Option<TextureFormat>) -> RenderPipelineDescriptor {
        let (entry, format, blend) = match view_format {
            None => ("gather", GATHERED_FORMAT, SCALED),
            Some(format) => ("composite", format, ADD_BY_FRAME),
        };
        RenderPipelineDescriptor {
            label: Some("bounce light".into()),
            layout: vec![self.layout.clone()],
            vertex: self.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: BOUNCE_SHADER,
                entry_point: Some(entry.into()),
                targets: vec![Some(ColorTargetState { format, blend: Some(blend), write_mask: ColorWrites::ALL })],
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

/// The half-size texture covers the viewport; the texture cache hands out a new one when the viewport's size changes.
fn prepare_bounce_views(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<BouncePipeline>>,
    pipeline: Res<BouncePipeline>,
    mut texture_cache: ResMut<TextureCache>,
    render_device: Res<RenderDevice>,
    views: Query<(Entity, &ExtractedView), With<BounceLight>>,
) {
    for (entity, view) in &views {
        let Some(size) = half_viewport(view) else {
            continue;
        };
        let gathered = half_texture(&mut texture_cache, &render_device, "bounce light", GATHERED_FORMAT, size);
        let gather = pipelines.specialize(&pipeline_cache, &pipeline, None);
        let composite = pipelines.specialize(&pipeline_cache, &pipeline, Some(view.target_format));
        commands.entity(entity).insert(BounceView { gather, composite, gathered });
    }
}

fn bounce(
    view: ViewQuery<(&ViewTarget, &ExtractedCamera, &ViewPrepassTextures, &ViewUniformOffset, &BounceLight, &BounceView)>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<BouncePipeline>,
    view_uniforms: Res<ViewUniforms>,
    mut scene_groups: Local<ViewBindGroups<(TextureViewId, TextureId, TextureViewId, BufferId)>>,
    mut gathered_groups: Local<ViewBindGroups<(TextureViewId, TextureId, TextureViewId, BufferId)>>,
    mut ctx: RenderContext,
) {
    let entity = view.entity();
    let (target, camera, prepass, offset, light, prepared) = view.into_inner();
    let (Some(gather), Some(composite), Some(depth), Some(depth_texture), Some(normals), Some(uniform), Some(uniforms)) = (
        pipeline_cache.get_render_pipeline(prepared.gather),
        pipeline_cache.get_render_pipeline(prepared.composite),
        prepass.depth_only_view(),
        prepass.depth.as_ref().map(|depth| depth.texture.texture.id()),
        prepass.normal_view(),
        view_uniforms.uniforms.binding(),
        view_uniforms.uniforms.buffer().map(|buffer| buffer.id()),
    ) else {
        return;
    };
    let layout = pipeline_cache.get_bind_group_layout(&pipeline.layout);
    let device = ctx.render_device();
    let scene = target.main_texture_view();
    let from_scene = scene_groups.get_or_make(entity, (scene.id(), depth_texture, normals.id(), uniforms), || {
        device.create_bind_group("bounce light gather", &layout, &BindGroupEntries::sequential((scene, depth, normals, uniform.clone())))
    });
    let gathered = &prepared.gathered.default_view;
    let from_gathered = gathered_groups.get_or_make(entity, (gathered.id(), depth_texture, normals.id(), uniforms), || {
        device.create_bind_group("bounce light", &layout, &BindGroupEntries::sequential((gathered, depth, normals, uniform)))
    });
    HalfThenFull {
        span: "bounce",
        half: gathered,
        march: ScreenStep { label: "bounce light gather", pipeline: gather, bind_group: from_scene },
        march_blend_constant: Some(LinearRgba::rgb(light.0, light.0, light.0)),
        composite: ScreenStep { label: "bounce light", pipeline: composite, bind_group: from_gathered },
        offset: offset.offset,
    }
    .draw(&mut ctx, target, camera);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_zero_is_off_and_higher_is_stronger() {
        assert_eq!(strength(0), None);
        let levels: Vec<f32> = (1..=10).filter_map(strength).collect();
        assert_eq!(levels.len(), 10);
        assert!(levels.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
