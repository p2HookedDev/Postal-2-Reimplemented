//! Screen-space reflections: a full-screen pass marches the depth prepass along each reflecting pixel's ray.
//! A ray that leaves the screen or hits nothing adds nothing. Puddles are sprites with no prepass, so the pass is
//! given their places; a lit pool stores its gloss in frame alpha above 1 (negated on blood), reset to 1 here.
//! See-through glass is not in the prepass: it marches in its own shader, reading a copy of the frame taken
//! before the translucents, and marks its pixels for this pass to skip.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Handle, uuid_handle};
use bevy::camera::{Camera, CameraUpdateSystems};
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::core_3d::{Transparent3d, main_opaque_pass_3d, main_transparent_pass_3d};
use bevy::core_pipeline::prepass::{NORMAL_PREPASS_FORMAT, ViewPrepassTextures};
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Has, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::ToExtents;
use bevy::math::{Mat4, URect, UVec2, Vec4};
use bevy::pbr::{ScreenSpaceTransmission, ScreenSpaceTransmissionQuality, ViewTransmissionTexture};
use bevy::render::camera::ExtractedCamera;
use bevy::render::diagnostic::RecordDiagnostics;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_phase::ViewSortedRenderPhases;
use bevy::render::render_resource::{
    AddressMode, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BufferId, CachedRenderPipelineId, ColorTargetState, ColorWrites,
    FilterMode, FragmentState, Operations, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType,
    SamplerDescriptor, ShaderStages, ShaderType, SpecializedRenderPipeline, SpecializedRenderPipelines, TextureDescriptor, TextureDimension, TextureFormat, TextureId,
    TextureSampleType, TextureUsages, TextureViewId, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::texture::TextureCache;
use bevy::render::view::{ExtractedView, Msaa, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use bevy::transform::TransformSystems;
use bevy::transform::components::GlobalTransform;

use crate::components::SceneView;
use p2_host::display::{MODERN_REFLECTION_STRENGTH, MODERN_REFLECTIONS, Settings};
use crate::particle_frame::DrawnParticles;
use crate::screen_pass::{PassUniformOffset, PassUniforms, PassView, ViewBindGroups, add_view_pass, follow_view_pass, modern_level, prepare_pass_uniforms, tag_pass_views};
use crate::shaders::{add_libraries, add_shaders, shader_file};
use crate::stage_encoder::{AfterOpaque, submit_stage};
use crate::view::ShownScene;

const REFLECTIONS_SHADER: Handle<Shader> = uuid_handle!("b3e1c7d4-2a95-4f6e-8c13-7d0f5a92e4b1");
pub(crate) const CLASS_PREPASS_SHADER: Handle<Shader> = uuid_handle!("e2a4f9c1-7b36-4d58-9f0e-3c81b6d27a45");
const WHOLE_MARCH_LEVEL: u8 = 2;
const EVERY_PIXEL_LEVEL: u8 = 3;
const FULL_STRENGTH: f32 = 10.0;
const STEPS_AT_ONE_PIXEL: f32 = 48.0;
/// Matches the array in `reflections.wesl`.
const MAX_PUDDLES: usize = 16;

pub struct ReflectionsPlugin;

impl Plugin for ReflectionsPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        crate::stage_encoder::build(app);
        add_libraries(app);
        add_shaders(app, [(REFLECTIONS_SHADER, shader_file("reflections.wesl"))]);
        add_view_pass::<Reflections>(app);
        app.add_systems(PostUpdate, follow_reflection_settings.after(TransformSystems::Propagate).after(CameraUpdateSystems).after(tag_pass_views));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_gpu_resource::<PassUniforms<Reflections>>()
            .init_gpu_resource::<SpecializedRenderPipelines<ReflectionsPipeline>>()
            .add_systems(RenderStartup, init_reflections_pipeline)
            .add_systems(Render, prepare_reflections_pipelines.in_set(RenderSystems::Prepare))
            .add_systems(Render, (prepare_pass_uniforms::<Reflections>, prepare_glass_copy).in_set(RenderSystems::PrepareResources))
            .add_systems(
                Core3d,
                copy_glass_frame.in_set(Core3dSystems::MainPass).after(main_opaque_pass_3d).before(submit_stage::<AfterOpaque>).before(main_transparent_pass_3d),
            )
            .add_systems(Core3d, reflect.in_set(Core3dSystems::EarlyPostProcess).before(bevy::anti_alias::taa::temporal_anti_alias));
    }
}

#[derive(Debug, Clone, Copy)]
pub enum LayersHeld {
    Alone,
    InActorShading,
}

/// Prepass pipelines that write normals also store the material's reflecting class in the normal's alpha.
/// Normals and depth stay Bevy's own, so ambient occlusion and contact shadows read the same.
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

#[derive(Component, ShaderType, Debug, Clone, Copy, PartialEq, ExtractComponent)]
#[extract_app(RenderApp)]
#[extract_component_sync_target((Self, ReflectionsPipelineId, PassUniformOffset<Self>))]
pub struct Reflections {
    clip_from_view: Mat4,
    view_from_clip: Mat4,
    view_from_world: Mat4,
    look: Vec4,
    puddles: [Vec4; MAX_PUDDLES],
    puddle_count: Vec4,
    in_frame: Vec4,
    from_frame: Vec4,
}

fn viewport_in_frame(viewport: URect, target: UVec2) -> (Vec4, Vec4) {
    let target = target.as_vec2();
    let (corner, size) = (viewport.min.as_vec2() / target, viewport.size().as_vec2() / target);
    (Vec4::new(corner.x, corner.y, size.x, size.y), Vec4::new(-corner.x / size.x, -corner.y / size.y, 1.0 / size.x, 1.0 / size.y))
}

/// x strength (1 at the top level), y march stride in pixels (2 below the top level), z step count,
/// w the materials level. `None` while off.
pub(crate) fn look(level: u8, strength: u8, materials: u8) -> Option<Vec4> {
    (level > 0 && strength > 0).then(|| {
        let stride = if level >= EVERY_PIXEL_LEVEL { 1.0 } else { 2.0 };
        let steps = STEPS_AT_ONE_PIXEL / stride / if level >= WHOLE_MARCH_LEVEL { 1.0 } else { 2.0 };
        Vec4::new((f32::from(strength) / FULL_STRENGTH).min(1.0), stride, steps, f32::from(materials))
    })
}

fn follow_reflection_settings(
    mut commands: Commands,
    settings: Option<Res<Settings>>,
    drawn: Option<Res<DrawnParticles>>,
    scene: Option<Res<ShownScene>>,
    views: Query<(Entity, &Camera, &GlobalTransform, Has<PassView>, Option<&Reflections>, Has<ScreenSpaceTransmission>), With<SceneView>>,
) {
    let wanted = look(
        modern_level(settings.as_deref(), MODERN_REFLECTIONS),
        modern_level(settings.as_deref(), MODERN_REFLECTION_STRENGTH),
        settings.as_deref().map_or(0, Settings::material_level),
    );
    // Adding or removing it respecializes every mesh pipeline, so it follows modern graphics, not the reflections.
    let bound = settings.as_deref().is_some_and(Settings::modern_graphics);
    for (entity, camera, place, drawn_in, shown, copied) in &views {
        if bound && drawn_in && !copied {
            commands.entity(entity).insert(GLASS_COPY);
        } else if !(bound && drawn_in) && copied {
            commands.entity(entity).remove::<ScreenSpaceTransmission>();
        }
        let viewport = camera.physical_viewport_rect().filter(|rect| rect.size().min_element() > 0);
        let target = camera.physical_target_size().filter(|size| size.min_element() > 0);
        let (Some(look), Some(viewport), Some(target)) = (wanted.filter(|_| drawn_in), viewport, target) else {
            follow_view_pass(commands.entity(entity), shown, None);
            continue;
        };
        let (clip_from_view, view_from_world) = (camera.clip_from_view(), place.to_matrix().inverse());
        let discs = (drawn.as_deref().zip(scene.as_deref())).map_or_else(Vec::new, |(drawn, scene)| drawn.puddle_discs(scene.origin, place.translation(), MAX_PUDDLES));
        let mut puddles = [Vec4::ZERO; MAX_PUDDLES];
        for (puddle, disc) in puddles.iter_mut().zip(&discs) {
            *puddle = view_from_world.transform_point3(disc.truncate()).extend(disc.w);
        }
        let puddle_count = Vec4::new(discs.len() as f32, 0.0, 0.0, 0.0);
        let (in_frame, from_frame) = viewport_in_frame(viewport, target);
        let reflections = Reflections { clip_from_view, view_from_clip: clip_from_view.inverse(), view_from_world, look, puddles, puddle_count, in_frame, from_frame };
        follow_view_pass(commands.entity(entity), shown, Some(reflections));
    }
}

/// Bevy binds the copy as the view's transmission texture to every mesh shader; with no steps it never copies or
/// prepares one itself, as none of our materials transmit.
const GLASS_COPY: ScreenSpaceTransmission = ScreenSpaceTransmission { steps: 0, quality: ScreenSpaceTransmissionQuality::Low };

/// Glass reads the copy only through a single-sampled view while the reflections run, and without any translucent
/// draw no glass is drawn.
fn prepare_glass_copy(
    mut commands: Commands,
    mut textures: ResMut<TextureCache>,
    render_device: Res<RenderDevice>,
    phases: Res<ViewSortedRenderPhases<Transparent3d>>,
    mut sampler: Local<Option<Sampler>>,
    views: Query<(Entity, &ExtractedCamera, &ExtractedView, &Msaa, Has<ScreenSpaceTransmission>, Has<Reflections>, Has<ViewTransmissionTexture>)>,
) {
    for (entity, camera, view, msaa, copied, mirrored, held) in &views {
        let drawn = phases.get(&view.retained_view_entity).is_some_and(|phase| !phase.items.is_empty());
        let Some(size) = camera.physical_target_size.filter(|_| copied && mirrored && drawn && msaa.samples() == 1) else {
            if held {
                commands.entity(entity).remove::<ViewTransmissionTexture>();
            }
            continue;
        };
        let copy = textures.get(
            &render_device,
            TextureDescriptor {
                label: Some("glass_mirror_copy"),
                size: size.to_extents(),
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: view.target_format,
                usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
                view_formats: &[],
            },
        );
        let sampler = sampler
            .get_or_insert_with(|| {
                render_device.create_sampler(&SamplerDescriptor { label: "glass_mirror_copy".into(), mag_filter: FilterMode::Linear, min_filter: FilterMode::Linear, ..Default::default() })
            })
            .clone();
        commands.entity(entity).insert(ViewTransmissionTexture { texture: copy.texture, view: copy.default_view, sampler });
    }
}

fn copy_glass_frame(view: ViewQuery<(&ExtractedCamera, &ViewTarget, &ViewTransmissionTexture)>, mut ctx: RenderContext) {
    let (camera, target, copy) = view.into_inner();
    let Some(size) = camera.physical_target_size else {
        return;
    };
    ctx.command_encoder().copy_texture_to_texture(target.main_texture().as_image_copy(), copy.texture.as_image_copy(), size.to_extents());
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
    mut groups: Local<ViewBindGroups<(TextureViewId, TextureId, TextureViewId, BufferId)>>,
    mut ctx: RenderContext,
) {
    let entity = view.entity();
    let (target, prepass, id, offset) = view.into_inner();
    let (Some(render_pipeline), Some(depth), Some(depth_texture), Some(normals), Some(uniform), Some(uniforms)) = (
        pipeline_cache.get_render_pipeline(id.0),
        prepass.depth_only_view(),
        prepass.depth.as_ref().map(|depth| depth.texture.texture.id()),
        prepass.normal_view(),
        buffer.0.binding(),
        buffer.0.buffer().map(|uniforms| uniforms.id()),
    ) else {
        return;
    };
    let post = target.post_process_write();
    let bind_group = groups.get_or_make(entity, (post.source.id(), depth_texture, normals.id(), uniforms), || {
        ctx.render_device().create_bind_group(
            "reflections",
            &pipeline_cache.get_bind_group_layout(&pipeline.layout),
            &BindGroupEntries::sequential((post.source, &pipeline.sampler, depth, normals, uniform)),
        )
    });
    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let span = diagnostics.time_span(ctx.command_encoder(), "reflections");
    {
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("reflections"),
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
    fn the_pass_reads_the_pool_records_and_the_default_surface_as_the_lit_shaders_write_them() {
        use crate::shaders::shader_const;
        let pass = include_str!("reflections.wesl");
        assert_eq!(shader_const(pass, "BLOOD_RECORD"), shader_const(include_str!("projector.wesl"), "BLOOD_RECORD"));
        assert_eq!(shader_const(pass, "DEFAULT_ROUGHNESS"), shader_const(include_str!("modern.wesl"), "DEFAULT_ROUGHNESS"));
    }

    #[test]
    fn levels_pick_the_march() {
        assert_eq!(look(0, 5, 5), None);
        assert_eq!(look(2, 0, 5), None);
        let (short, strided, full) = (look(1, 5, 5).expect("a short march"), look(2, 5, 5).expect("a stride of two"), look(3, 10, 0).expect("every pixel"));
        assert_eq!((strided.x, strided.y, strided.w), (0.5, 2.0, 5.0));
        assert_eq!((full.x, full.y, full.w), (1.0, 1.0, 0.0));
        assert!(0.0 < short.z && short.z < strided.z && strided.z < full.z);
    }

    #[test]
    fn a_whole_viewport_maps_exactly_and_a_part_maps_back() {
        let target = UVec2::new(1921, 1080);
        let (in_frame, from_frame) = viewport_in_frame(URect::from_corners(UVec2::ZERO, target), target);
        assert_eq!((in_frame, from_frame), (Vec4::new(0.0, 0.0, 1.0, 1.0), Vec4::new(0.0, 0.0, 1.0, 1.0)));
        let (in_frame, from_frame) = viewport_in_frame(URect::new(0, 90, 1920, 990), UVec2::new(1920, 1080));
        let frame_uv = 0.3 * in_frame.w + in_frame.y;
        assert!((frame_uv * from_frame.w + from_frame.y - 0.3).abs() < 1e-6);
        assert!((frame_uv * 1080.0 - (90.0 + 0.3 * 900.0)).abs() < 1e-3);
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

    #[test]
    fn the_pass_tints_blood_as_the_lit_shaders_do() {
        let sheen = |source: &'static str| source.lines().map(str::trim).find(|line| line.starts_with("const BLOOD_SHEEN:")).expect("a blood sheen");
        assert_eq!(sheen(include_str!("reflections.wesl")), sheen(include_str!("projector.wesl")));
    }
}
