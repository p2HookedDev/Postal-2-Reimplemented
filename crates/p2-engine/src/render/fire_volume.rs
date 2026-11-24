//! Modern graphics: fire as a volume. Each flame emitter the scripts keep burning near the eye is drawn as flame
//! marched through rising noise in a box around its live sprites, and those sprites are hidden. Where a fire is,
//! and when it starts, spreads and dies, stays with the scripts' emitters; nothing here changes them.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Handle, uuid_handle};
use bevy::camera::Camera;
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::core_3d::{main_opaque_pass_3d, main_transparent_pass_3d};
use bevy::core_pipeline::prepass::{DepthPrepass, ViewPrepassTextures};
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Has, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::query::QueryItem;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::lifetimeless::Read;
use bevy::math::{Vec3, Vec4};
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, Buffer, BufferInitDescriptor,
    BufferUsages, CachedRenderPipelineId, ColorTargetState, ColorWrites, Extent3d, FragmentState, Operations, PipelineCache, RenderPassColorAttachment,
    RenderPassDescriptor, RenderPipelineDescriptor, ShaderStages, SpecializedRenderPipeline, SpecializedRenderPipelines, TextureDescriptor, TextureDimension,
    TextureFormat, TextureSampleType, TextureUsages, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::sync_component::SyncComponent;
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::{ExtractedView, Msaa, ViewTarget, ViewUniform, ViewUniformOffset, ViewUniforms};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use bevy::time::Time;
use bevy::transform::components::GlobalTransform;

use crate::components::SceneView;
use crate::display::{MODERN_FIRE_VOLUMES, Settings};
use crate::map::{MapSystems, Particles};
use crate::play::Play;
use crate::render::bounce::half_size;
use crate::render::screen_pass::modern_level;
use crate::render::shaders::add_shaders;

const FIRE_SHADER: Handle<Shader> = uuid_handle!("b3e81c5a-6d27-4f90-8a1e-2c7d94f05b63");
const MARCHED_FORMAT: TextureFormat = TextureFormat::Rgba16Float;
/// Matches `MAX_VOLUMES` in `fire_volume.wesl`.
const MAX_VOLUMES: usize = 8;
/// In Unreal units (about 2 cm each): a fire farther from the eye stays sprites.
const REACH: f32 = 2048.0;
const VOLUME_WORDS: usize = 12;

/// Adds the marched flame light, whose alpha is 0, to the frame and keeps the frame's alpha, the sun rays' mask.
const OVER: BlendState = BlendState {
    color: BlendComponent { src_factor: BlendFactor::One, dst_factor: BlendFactor::OneMinusSrcAlpha, operation: BlendOperation::Add },
    alpha: BlendComponent { src_factor: BlendFactor::Zero, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
};

pub struct FireVolumePlugin;

impl Plugin for FireVolumePlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [(FIRE_SHADER, Shader::from_wesl(include_str!("fire_volume.wesl"), "p2_engine/fire_volume.wesl"))]);
        app.add_plugins(ExtractComponentPlugin::<FireVolumes>::default()).add_systems(PostUpdate, follow_fires.after(MapSystems::Present));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<FirePipeline>>()
            .add_systems(RenderStartup, init_fire_pipeline)
            .add_systems(Render, prepare_fire_views.in_set(RenderSystems::PrepareResources))
            .add_systems(Core3d, fire_volume.in_set(Core3dSystems::MainPass).after(main_opaque_pass_3d).before(main_transparent_pass_3d));
    }
}

/// One fire's box in world space; `tint` is its sprites' mean colour, and their mean alpha in `w`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FireBox {
    pub centre: Vec3,
    pub half: Vec3,
    pub tint: Vec4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Quality {
    steps: u32,
    octaves: u32,
}

fn quality(level: u8) -> Option<Quality> {
    match level {
        0 => None,
        1 => Some(Quality { steps: 16, octaves: 3 }),
        _ => Some(Quality { steps: 40, octaves: 5 }),
    }
}

/// On a scene camera while fire volumes are on: this frame's fires, nearest first.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct FireVolumes {
    quality: Quality,
    seconds: f32,
    volumes: Vec<FireBox>,
}

/// Hides the sprites only of fires a view will draw: one active, single-sampled, with a depth prepass.
fn follow_fires(
    mut commands: Commands,
    settings: Option<Res<Settings>>,
    time: Res<Time>,
    play: Option<Res<Play>>,
    particles: Option<ResMut<Particles>>,
    views: Query<(Entity, &Camera, &Msaa, &GlobalTransform, Has<DepthPrepass>, Has<FireVolumes>), With<SceneView>>,
    mut hiding: Local<bool>,
) {
    let wanted = quality(modern_level(settings.as_deref(), MODERN_FIRE_VOLUMES));
    let drawn_by = |camera: &Camera, msaa: &Msaa, depth: bool| wanted.is_some() && camera.is_active && matches!(msaa, Msaa::Off) && depth;
    let eye = views.iter().find(|(_, camera, msaa, _, depth, _)| drawn_by(camera, msaa, *depth)).map(|(_, _, _, at, ..)| at.translation());
    let origin = play.as_ref().and_then(|play| play.world()).map(|world| world.origin());
    let mut volumes = Vec::new();
    if let Some(mut particles) = particles {
        match (wanted, eye, origin) {
            (Some(_), Some(eye), Some(origin)) => {
                volumes = particles.fire_volumes(origin, eye, MAX_VOLUMES, REACH);
                *hiding = true;
            }
            _ if std::mem::take(&mut *hiding) => {
                particles.fire_volumes(Vec3::ZERO, Vec3::ZERO, 0, 0.0);
            }
            _ => {}
        }
    }
    let seconds = time.elapsed_secs_wrapped();
    for (entity, camera, msaa, _, depth, shown) in &views {
        match wanted.filter(|_| drawn_by(camera, msaa, depth)) {
            Some(quality) => {
                commands.entity(entity).insert(FireVolumes { quality, seconds, volumes: volumes.clone() });
            }
            None if shown => {
                commands.entity(entity).remove::<FireVolumes>();
            }
            None => {}
        }
    }
}

impl SyncComponent<RenderApp> for FireVolumes {
    type Target = (Self, FireView);
}

impl ExtractComponent<RenderApp> for FireVolumes {
    type QueryData = Read<Self>;
    type QueryFilter = ();
    type Out = Self;

    fn extract_component(fires: QueryItem<'_, '_, Self::QueryData>) -> Option<Self::Out> {
        Some(fires.clone())
    }
}

/// `Fires` in `fire_volume.wesl`: the volumes, then their count, the march's steps and noise octaves, and the seconds.
fn fires_bytes(fires: &FireVolumes) -> Vec<u8> {
    let mut words: Vec<u32> = Vec::with_capacity(MAX_VOLUMES * VOLUME_WORDS + 4);
    for fire in fires.volumes.iter().take(MAX_VOLUMES) {
        words.extend(fire.centre.extend(0.0).to_array().map(f32::to_bits));
        words.extend(fire.half.extend(0.0).to_array().map(f32::to_bits));
        words.extend(fire.tint.to_array().map(f32::to_bits));
    }
    words.resize(MAX_VOLUMES * VOLUME_WORDS, 0);
    let count = u32::try_from(fires.volumes.len().min(MAX_VOLUMES)).unwrap_or(0);
    words.extend([count, fires.quality.steps, fires.quality.octaves, fires.seconds.to_bits()]);
    words.into_iter().flat_map(u32::to_le_bytes).collect()
}

#[derive(Resource)]
pub struct FirePipeline {
    layout: BindGroupLayoutDescriptor,
    fullscreen: FullscreenShader,
}

#[derive(Component)]
pub struct FireView {
    march: CachedRenderPipelineId,
    composite: CachedRenderPipelineId,
    marched: CachedTexture,
    fires: Buffer,
}

fn init_fire_pipeline(mut commands: Commands, fullscreen: Res<FullscreenShader>) {
    let layout = BindGroupLayoutDescriptor::new(
        "fire volume",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                binding_types::texture_2d(TextureSampleType::Float { filterable: false }),
                binding_types::texture_2d(TextureSampleType::Depth),
                binding_types::uniform_buffer::<ViewUniform>(true),
                binding_types::uniform_buffer_sized(false, None),
            ),
        ),
    );
    commands.insert_resource(FirePipeline { layout, fullscreen: fullscreen.clone() });
}

/// `None` is the half-size march; a view's format is the full-size pass that lays the volumes over that view.
impl SpecializedRenderPipeline for FirePipeline {
    type Key = Option<TextureFormat>;

    fn specialize(&self, view_format: Option<TextureFormat>) -> RenderPipelineDescriptor {
        let (entry, format, blend) = match view_format {
            None => ("march", MARCHED_FORMAT, None),
            Some(format) => ("composite", format, Some(OVER)),
        };
        RenderPipelineDescriptor {
            label: Some("fire volume".into()),
            layout: vec![self.layout.clone()],
            vertex: self.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: FIRE_SHADER,
                entry_point: Some(entry.into()),
                targets: vec![Some(ColorTargetState { format, blend, write_mask: ColorWrites::ALL })],
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

/// The texture cache hands out a new texture when the view's size changes.
fn prepare_fire_views(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<FirePipeline>>,
    pipeline: Res<FirePipeline>,
    mut texture_cache: ResMut<TextureCache>,
    render_device: Res<RenderDevice>,
    views: Query<(Entity, &ExtractedView, &ExtractedCamera, &FireVolumes)>,
) {
    for (entity, view, camera, fires) in &views {
        // Asked for before the first fire is near: its sprites are hidden from the frame its volume is picked.
        let march = pipelines.specialize(&pipeline_cache, &pipeline, None);
        let composite = pipelines.specialize(&pipeline_cache, &pipeline, Some(view.target_format));
        if fires.volumes.is_empty() {
            continue;
        }
        let Some(size) = camera.physical_target_size.filter(|size| size.min_element() > 0).map(half_size) else {
            continue;
        };
        let marched = texture_cache.get(
            &render_device,
            TextureDescriptor {
                label: Some("fire volume"),
                size: Extent3d { width: size.x, height: size.y, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: MARCHED_FORMAT,
                usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        );
        let fires = render_device.create_buffer_with_data(&BufferInitDescriptor { label: Some("fire volumes"), contents: &fires_bytes(fires), usage: BufferUsages::UNIFORM });
        commands.entity(entity).insert(FireView { march, composite, marched, fires });
    }
}

fn fire_volume(
    view: ViewQuery<(&ViewTarget, &ViewPrepassTextures, &ViewUniformOffset, &FireVolumes, &FireView)>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<FirePipeline>,
    view_uniforms: Res<ViewUniforms>,
    mut ctx: RenderContext,
) {
    let (target, prepass, offset, fires, prepared) = view.into_inner();
    if fires.volumes.is_empty() {
        return;
    }
    let (Some(march), Some(composite), Some(depth), Some(uniform)) = (
        pipeline_cache.get_render_pipeline(prepared.march),
        pipeline_cache.get_render_pipeline(prepared.composite),
        prepass.depth_only_view(),
        view_uniforms.uniforms.binding(),
    ) else {
        return;
    };
    let layout = pipeline_cache.get_bind_group_layout(&pipeline.layout);
    let device = ctx.render_device();
    let from_scene = device.create_bind_group(
        "fire volume march",
        &layout,
        &BindGroupEntries::sequential((target.main_texture_view(), depth, uniform.clone(), prepared.fires.as_entire_binding())),
    );
    let from_marched = device.create_bind_group(
        "fire volume",
        &layout,
        &BindGroupEntries::sequential((&prepared.marched.default_view, depth, uniform, prepared.fires.as_entire_binding())),
    );
    {
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("fire volume march"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &prepared.marched.default_view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations::default(),
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_render_pipeline(march);
        pass.set_bind_group(0, &from_scene, &[offset.offset]);
        pass.draw(0..3, 0..1);
    }
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("fire volume"),
        color_attachments: &[Some(target.get_color_attachment())],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(composite);
    pass.set_bind_group(0, &from_marched, &[offset.offset]);
    pass.draw(0..3, 0..1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_zero_is_off_and_two_is_finer() {
        assert_eq!(quality(0), None);
        let (Some(coarse), Some(fine)) = (quality(1), quality(2)) else { panic!("levels 1 and 2 are on") };
        assert!(fine.steps > coarse.steps && fine.octaves > coarse.octaves);
    }

    #[test]
    fn the_uniform_has_room_for_every_volume_and_counts_only_those_sent() {
        let fire = FireBox { centre: Vec3::ONE, half: Vec3::splat(2.0), tint: Vec4::ONE };
        let fires = FireVolumes { quality: Quality { steps: 16, octaves: 3 }, seconds: 1.5, volumes: vec![fire; MAX_VOLUMES + 3] };
        let bytes = fires_bytes(&fires);
        assert_eq!(bytes.len(), (MAX_VOLUMES * VOLUME_WORDS + 4) * 4);
        let count_at = MAX_VOLUMES * VOLUME_WORDS * 4;
        assert_eq!(bytes[count_at..count_at + 4], (MAX_VOLUMES as u32).to_le_bytes());
        let empty = FireVolumes { volumes: Vec::new(), ..fires };
        assert_eq!(fires_bytes(&empty).len(), bytes.len());
    }
}
