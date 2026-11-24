//! Modern graphics: screen-space reflections. After the main passes one full-screen pass marches the depth prepass
//! along each reflecting pixel's mirrored ray and adds what it meets to the frame. A pixel reflects when its normal
//! prepass alpha names a reflecting class, which our lit materials' prepass writes. A ray that leaves the screen or
//! meets nothing shows nothing: no stand-in light takes its place. Level 1 marches half as far as level 2. A puddle
//! is a sprite, which writes no prepass: the pass is told where the nearest puddles lie and mirrors the floor there.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Handle, uuid_handle};
use bevy::camera::Camera;
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::prepass::{NORMAL_PREPASS_FORMAT, ViewPrepassTextures};
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Query, Res, ResMut, Resource, With};
use bevy::ecs::query::QueryItem;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::lifetimeless::Read;
use bevy::math::{Mat4, Vec4};
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::{
    AddressMode, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, CachedRenderPipelineId, ColorTargetState, ColorWrites, FilterMode,
    FragmentState, Operations, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType,
    SamplerDescriptor, ShaderStages, ShaderType, SpecializedRenderPipeline, SpecializedRenderPipelines, TextureFormat, TextureSampleType, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::sync_component::SyncComponent;
use bevy::render::view::{ExtractedView, Msaa, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use bevy::transform::TransformSystems;
use bevy::transform::components::GlobalTransform;

use crate::components::SceneView;
use p2_host::display::{MODERN_REFLECTION_STRENGTH, MODERN_REFLECTIONS, Settings};
use crate::particle_frame::DrawnParticles;
use crate::screen_pass::{PassUniformOffset, PassUniforms, modern_level, prepare_pass_uniforms};
use crate::shaders::{add_libraries, add_shaders};
use crate::view::ShownScene;

const REFLECTIONS_SHADER: Handle<Shader> = uuid_handle!("b3e1c7d4-2a95-4f6e-8c13-7d0f5a92e4b1");
const CLASS_PREPASS_SHADER: Handle<Shader> = uuid_handle!("e2a4f9c1-7b36-4d58-9f0e-3c81b6d27a45");
const HALF_SIZE_LEVEL: u8 = 2;
const FULL_SIZE_LEVEL: u8 = 3;
const FULL_STRENGTH: f32 = 10.0;
const FULL_SIZE_STEPS: f32 = 48.0;
/// Matches the array in `reflections.wesl`.
const MAX_PUDDLES: usize = 16;

pub struct ReflectionsPlugin;

impl Plugin for ReflectionsPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_libraries(app);
        add_shaders(app, [(REFLECTIONS_SHADER, Shader::from_wesl(include_str!("reflections.wesl"), "p2_render/reflections.wesl"))]);
        add_shaders(app, [(CLASS_PREPASS_SHADER, Shader::from_wesl(include_str!("reflections_prepass.wesl"), "p2_render/reflections_prepass.wesl"))]);
        app.add_plugins(ExtractComponentPlugin::<Reflections>::default())
            .add_systems(PostUpdate, follow_reflection_settings.after(TransformSystems::Propagate));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_gpu_resource::<PassUniforms<Reflections>>()
            .init_gpu_resource::<SpecializedRenderPipelines<ReflectionsPipeline>>()
            .add_systems(RenderStartup, init_reflections_pipeline)
            .add_systems(Render, prepare_reflections_pipelines.in_set(RenderSystems::Prepare))
            .add_systems(Render, prepare_pass_uniforms::<Reflections>.in_set(RenderSystems::PrepareResources))
            // Before the temporal resolve, which steadies what was drawn through the frame's jitter.
            .add_systems(Core3d, reflect.in_set(Core3dSystems::EarlyPostProcess).before(bevy::anti_alias::taa::temporal_anti_alias));
    }
}

/// Where a material's bind group holds its `ShaderLayers`.
#[derive(Debug, Clone, Copy)]
pub enum LayersHeld {
    Alone,
    InActorShading,
}

/// A prepass pipeline that writes normals gets the shader that also writes the material's class in their alpha. Its
/// normals and depth are Bevy's own, so ambient occlusion and contact shadows read the same.
pub fn write_class_in_prepass(descriptor: &mut RenderPipelineDescriptor, held: LayersHeld) {
    let Some(fragment) = descriptor.fragment.as_mut() else {
        return;
    };
    if !matches!(fragment.targets.first(), Some(Some(normals)) if normals.format == NORMAL_PREPASS_FORMAT) {
        return;
    }
    fragment.shader = CLASS_PREPASS_SHADER;
    if matches!(held, LayersHeld::InActorShading) {
        fragment.shader_defs.push("ACTOR_SHADING".into());
    }
}

/// On a scene camera while screen-space reflections are on. As `reflections.wesl` reads it.
#[derive(Component, ShaderType, Debug, Clone, Copy, PartialEq)]
pub struct Reflections {
    clip_from_view: Mat4,
    view_from_clip: Mat4,
    view_from_world: Mat4,
    look: Vec4,
    /// In view space: the centre and the radius of each puddle; `puddle_count.x` of them are set.
    puddles: [Vec4; MAX_PUDDLES],
    puddle_count: Vec4,
}

/// x the strength, 1 at the top level (a surface returns no more than reaches it), y the march's stride in pixels (2 at
/// half size), z its steps, w the materials level. `None` while off. A ray that meets nothing on screen shows nothing.
pub(crate) fn look(level: u8, strength: u8, materials: u8) -> Option<Vec4> {
    (level > 0 && strength > 0).then(|| {
        let stride = if level >= FULL_SIZE_LEVEL { 1.0 } else { 2.0 };
        let steps = FULL_SIZE_STEPS / stride / if level >= HALF_SIZE_LEVEL { 1.0 } else { 2.0 };
        Vec4::new((f32::from(strength) / FULL_STRENGTH).min(1.0), stride, steps, f32::from(materials))
    })
}

fn follow_reflection_settings(
    mut commands: Commands,
    settings: Option<Res<Settings>>,
    drawn: Option<Res<DrawnParticles>>,
    scene: Option<Res<ShownScene>>,
    views: Query<(Entity, &Camera, &GlobalTransform, &Msaa, Option<&Reflections>), With<SceneView>>,
) {
    let wanted = look(
        modern_level(settings.as_deref(), MODERN_REFLECTIONS),
        modern_level(settings.as_deref(), MODERN_REFLECTION_STRENGTH),
        settings.as_deref().map_or(0, Settings::material_level),
    );
    for (entity, camera, place, msaa, shown) in &views {
        // The pass binds the prepass textures as single-sampled.
        let Some(look) = wanted.filter(|_| camera.is_active && matches!(msaa, Msaa::Off)) else {
            if shown.is_some() {
                commands.entity(entity).remove::<Reflections>();
            }
            continue;
        };
        let (clip_from_view, view_from_world) = (camera.clip_from_view(), place.to_matrix().inverse());
        let discs = (drawn.as_deref().zip(scene.as_deref())).map_or_else(Vec::new, |(drawn, scene)| drawn.puddle_discs(scene.origin, place.translation(), MAX_PUDDLES));
        let mut puddles = [Vec4::ZERO; MAX_PUDDLES];
        for (puddle, disc) in puddles.iter_mut().zip(&discs) {
            *puddle = view_from_world.transform_point3(disc.truncate()).extend(disc.w);
        }
        let puddle_count = Vec4::new(discs.len() as f32, 0.0, 0.0, 0.0);
        let reflections = Reflections { clip_from_view, view_from_clip: clip_from_view.inverse(), view_from_world, look, puddles, puddle_count };
        if shown != Some(&reflections) {
            commands.entity(entity).insert(reflections);
        }
    }
}

impl SyncComponent<RenderApp> for Reflections {
    type Target = (Self, ReflectionsPipelineId, PassUniformOffset<Self>);
}

impl ExtractComponent<RenderApp> for Reflections {
    type QueryData = Read<Self>;
    type QueryFilter = ();
    type Out = Self;

    fn extract_component(reflections: QueryItem<'_, '_, Self::QueryData>) -> Option<Self::Out> {
        Some(*reflections)
    }
}

#[derive(Resource)]
pub struct ReflectionsPipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    fullscreen: FullscreenShader,
}

#[derive(Component)]
pub struct ReflectionsPipelineId(CachedRenderPipelineId);

fn init_reflections_pipeline(mut commands: Commands, render_device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>) {
    let layout = BindGroupLayoutDescriptor::new(
        "reflections",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                binding_types::texture_2d(TextureSampleType::Float { filterable: true }),
                binding_types::sampler(SamplerBindingType::Filtering),
                binding_types::texture_2d(TextureSampleType::Depth),
                binding_types::texture_2d(TextureSampleType::Float { filterable: false }),
                binding_types::uniform_buffer::<Reflections>(true),
            ),
        ),
    );
    let sampler = render_device.create_sampler(&SamplerDescriptor {
        label: "reflections".into(),
        address_mode_u: AddressMode::ClampToEdge,
        address_mode_v: AddressMode::ClampToEdge,
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        ..Default::default()
    });
    commands.insert_resource(ReflectionsPipeline { layout, sampler, fullscreen: fullscreen.clone() });
}

impl SpecializedRenderPipeline for ReflectionsPipeline {
    type Key = TextureFormat;

    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("reflections".into()),
            layout: vec![self.layout.clone()],
            vertex: self.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: REFLECTIONS_SHADER,
                targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })],
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

fn prepare_reflections_pipelines(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<ReflectionsPipeline>>,
    pipeline: Res<ReflectionsPipeline>,
    views: Query<(Entity, &ExtractedView), With<Reflections>>,
) {
    for (entity, view) in &views {
        let id = pipelines.specialize(&pipeline_cache, &pipeline, view.target_format);
        commands.entity(entity).insert(ReflectionsPipelineId(id));
    }
}

fn reflect(
    view: ViewQuery<(&ViewTarget, &ViewPrepassTextures, &ReflectionsPipelineId, &PassUniformOffset<Reflections>)>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<ReflectionsPipeline>,
    buffer: Res<PassUniforms<Reflections>>,
    mut ctx: RenderContext,
) {
    let (target, prepass, id, offset) = view.into_inner();
    let (Some(render_pipeline), Some(depth), Some(normals), Some(uniform)) =
        (pipeline_cache.get_render_pipeline(id.0), prepass.depth_only_view(), prepass.normal_view(), buffer.0.binding())
    else {
        return;
    };
    let post = target.post_process_write();
    let bind_group = ctx.render_device().create_bind_group(
        "reflections",
        &pipeline_cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::sequential((post.source, &pipeline.sampler, depth, normals, uniform)),
    );
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("reflections"),
        color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(render_pipeline);
    pass.set_bind_group(0, &bind_group, &[offset.0]);
    pass.draw(0..3, 0..1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_pick_the_march() {
        assert_eq!(look(0, 5, 5), None);
        assert_eq!(look(2, 0, 5), None);
        let (short, half, full) = (look(1, 5, 5).expect("a short march"), look(2, 5, 5).expect("half size"), look(3, 10, 0).expect("full size"));
        assert_eq!((half.x, half.y, half.w), (0.5, 2.0, 5.0));
        assert_eq!((full.x, full.y, full.w), (1.0, 1.0, 0.0));
        assert!(0.0 < short.z && short.z < half.z && half.z < full.z);
    }

    #[test]
    fn only_a_prepass_writing_normals_takes_the_class_shader() {
        let with_target = |format| RenderPipelineDescriptor {
            fragment: Some(FragmentState { targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })], ..Default::default() }),
            ..Default::default()
        };
        let mut prepass = with_target(NORMAL_PREPASS_FORMAT);
        write_class_in_prepass(&mut prepass, LayersHeld::InActorShading);
        let fragment = prepass.fragment.expect("a fragment stage");
        assert_eq!(fragment.shader, CLASS_PREPASS_SHADER);
        assert_eq!(fragment.shader_defs.len(), 1);
        let mut main = with_target(TextureFormat::Rgba16Float);
        write_class_in_prepass(&mut main, LayersHeld::Alone);
        assert_ne!(main.fragment.expect("a fragment stage").shader, CLASS_PREPASS_SHADER);
        let mut depth_only = RenderPipelineDescriptor::default();
        write_class_in_prepass(&mut depth_only, LayersHeld::Alone);
        assert!(depth_only.fragment.is_none());
    }
}
