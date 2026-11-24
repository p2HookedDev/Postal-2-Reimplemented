//! Modern graphics: screen-space bounce light. Between the opaque and the transparent passes a half-size pass gathers
//! the lit colour of nearby surfaces that face each pixel, and a full-size pass brightens the frame by a blur of it
//! that keeps to surfaces of like depth and facing. Within one frame only: nothing is carried over from the last.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Handle, uuid_handle};
use bevy::camera::Camera;
use bevy::color::LinearRgba;
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::prepass::ViewPrepassTextures;
use bevy::core_pipeline::schedule::Core3d;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Query, Res, ResMut, Resource, With};
use bevy::ecs::query::QueryItem;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::lifetimeless::Read;
use bevy::math::UVec2;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, CachedRenderPipelineId, ColorTargetState,
    ColorWrites, Extent3d, FragmentState, Operations, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, ShaderStages,
    SpecializedRenderPipeline, SpecializedRenderPipelines, TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::sync_component::SyncComponent;
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::{ExtractedView, Msaa, ViewTarget, ViewUniform, ViewUniformOffset, ViewUniforms};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;

use crate::components::SceneView;
use p2_host::display::{MODERN_BOUNCE_LIGHT, Settings};
use crate::MainPassExtraSystems;
use crate::shaders::add_shaders;

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
        add_shaders(app, [(BOUNCE_SHADER, Shader::from_wesl(include_str!("bounce.wesl"), "p2_render/bounce.wesl"))]);
        app.add_plugins(ExtractComponentPlugin::<BounceLight>::default()).add_systems(PostUpdate, follow_bounce_settings);
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
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct BounceLight(f32);

fn strength(level: u8) -> Option<f32> {
    (level > 0).then(|| f32::from(level) * STRENGTH_PER_LEVEL)
}

pub(crate) fn half_size(full: UVec2) -> UVec2 {
    (full + UVec2::ONE) / 2
}

fn follow_bounce_settings(mut commands: Commands, settings: Option<Res<Settings>>, views: Query<(Entity, &Camera, &Msaa, Option<&BounceLight>), With<SceneView>>) {
    let wanted = strength(settings.as_ref().map_or(0, |settings| settings.modern_level(MODERN_BOUNCE_LIGHT)));
    for (entity, camera, msaa, shown) in &views {
        // The passes bind the prepass textures as single-sampled.
        match wanted.filter(|_| camera.is_active && matches!(msaa, Msaa::Off)) {
            Some(scale) if shown != Some(&BounceLight(scale)) => {
                commands.entity(entity).insert(BounceLight(scale));
            }
            None if shown.is_some() => {
                commands.entity(entity).remove::<BounceLight>();
            }
            _ => {}
        }
    }
}

impl SyncComponent<RenderApp> for BounceLight {
    type Target = (Self, BounceView);
}

impl ExtractComponent<RenderApp> for BounceLight {
    type QueryData = Read<Self>;
    type QueryFilter = ();
    type Out = Self;

    fn extract_component(light: QueryItem<'_, '_, Self::QueryData>) -> Option<Self::Out> {
        Some(*light)
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

/// The texture cache hands out a new texture when the view's size changes.
fn prepare_bounce_views(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<BouncePipeline>>,
    pipeline: Res<BouncePipeline>,
    mut texture_cache: ResMut<TextureCache>,
    render_device: Res<RenderDevice>,
    views: Query<(Entity, &ExtractedView, &ExtractedCamera), With<BounceLight>>,
) {
    for (entity, view, camera) in &views {
        let Some(size) = camera.physical_target_size.filter(|size| size.min_element() > 0).map(half_size) else {
            continue;
        };
        let gathered = texture_cache.get(
            &render_device,
            TextureDescriptor {
                label: Some("bounce light"),
                size: Extent3d { width: size.x, height: size.y, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: GATHERED_FORMAT,
                usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        );
        let gather = pipelines.specialize(&pipeline_cache, &pipeline, None);
        let composite = pipelines.specialize(&pipeline_cache, &pipeline, Some(view.target_format));
        commands.entity(entity).insert(BounceView { gather, composite, gathered });
    }
}

fn bounce(
    view: ViewQuery<(&ViewTarget, &ViewPrepassTextures, &ViewUniformOffset, &BounceLight, &BounceView)>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<BouncePipeline>,
    view_uniforms: Res<ViewUniforms>,
    mut ctx: RenderContext,
) {
    let (target, prepass, offset, light, prepared) = view.into_inner();
    let (Some(gather), Some(composite), Some(depth), Some(normals), Some(uniform)) = (
        pipeline_cache.get_render_pipeline(prepared.gather),
        pipeline_cache.get_render_pipeline(prepared.composite),
        prepass.depth_only_view(),
        prepass.normal_view(),
        view_uniforms.uniforms.binding(),
    ) else {
        return;
    };
    let layout = pipeline_cache.get_bind_group_layout(&pipeline.layout);
    let device = ctx.render_device();
    let from_scene = device.create_bind_group("bounce light gather", &layout, &BindGroupEntries::sequential((target.main_texture_view(), depth, normals, uniform.clone())));
    let from_gathered = device.create_bind_group("bounce light", &layout, &BindGroupEntries::sequential((&prepared.gathered.default_view, depth, normals, uniform)));
    {
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("bounce light gather"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &prepared.gathered.default_view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations::default(),
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_render_pipeline(gather);
        pass.set_bind_group(0, &from_scene, &[offset.offset]);
        pass.set_blend_constant(LinearRgba::rgb(light.0, light.0, light.0));
        pass.draw(0..3, 0..1);
    }
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("bounce light"),
        color_attachments: &[Some(target.get_color_attachment())],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(composite);
    pass.set_bind_group(0, &from_gathered, &[offset.offset]);
    pass.draw(0..3, 0..1);
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

    #[test]
    fn half_size_covers_odd_sides() {
        assert_eq!(half_size(UVec2::new(1921, 1080)), UVec2::new(961, 540));
        assert_eq!(half_size(UVec2::ONE), UVec2::ONE);
    }
}
