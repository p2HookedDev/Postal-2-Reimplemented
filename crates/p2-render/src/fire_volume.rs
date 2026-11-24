//! Modern graphics: fire as a volume. Each flame emitter the scripts keep burning near the eye is drawn as flame
//! marched through rising noise in a box around its live sprites, and those sprites are hidden. Where a fire is,
//! and when it starts, spreads and dies, stays with the scripts' emitters; nothing here changes them.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Handle, uuid_handle};
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::prepass::ViewPrepassTextures;
use bevy::core_pipeline::schedule::Core3d;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Has, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::math::{Vec3, Vec4};
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, Buffer, BufferId,
    BufferInitDescriptor, BufferUsages, CachedRenderPipelineId, ColorTargetState, ColorWrites, FragmentState, PipelineCache, RenderPipelineDescriptor,
    ShaderStages, ShaderType, SpecializedRenderPipeline, SpecializedRenderPipelines, TextureFormat, TextureId, TextureSampleType, TextureViewId, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::{ExtractedView, ViewTarget, ViewUniform, ViewUniformOffset, ViewUniforms};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use bevy::time::Time;
use bevy::transform::components::GlobalTransform;
use rustc_hash::FxHashSet;

use crate::components::SceneView;
use p2_host::display::{MODERN_FIRE_VOLUMES, Settings};
use crate::MainPassExtraSystems;
use crate::frame_facts::FrameFacts;
use crate::particle_draw::ParticleDraws;
use crate::particle_frame::DrawnParticles;
use crate::screen_pass::{
    HalfThenFull, PassView, ScreenStep, ViewBindGroups, add_view_pass, follow_view_pass, half_texture, half_viewport, modern_level, shader_bytes,
    tag_pass_views,
};
use crate::shaders::{add_libraries, add_shaders};

const FIRE_SHADER: Handle<Shader> = uuid_handle!("b3e81c5a-6d27-4f90-8a1e-2c7d94f05b63");
const MARCHED_FORMAT: TextureFormat = TextureFormat::Rgba16Float;
/// Matches `MAX_VOLUMES` in `fire_volume.wesl`.
const MAX_VOLUMES: usize = 8;
/// In Unreal units (about 2 cm each): a fire farther from the eye stays sprites.
const REACH: f32 = 2048.0;

/// Adds the marched flame light, whose alpha is 0, to the frame and keeps the frame's alpha, the sun rays' mask.
const OVER: BlendState = BlendState {
    color: BlendComponent { src_factor: BlendFactor::One, dst_factor: BlendFactor::OneMinusSrcAlpha, operation: BlendOperation::Add },
    alpha: BlendComponent { src_factor: BlendFactor::Zero, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
};

pub struct FireVolumePlugin;

#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct FireVolumeSystems;

impl Plugin for FireVolumePlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_libraries(app);
        add_shaders(app, [(FIRE_SHADER, Shader::from_wesl(include_str!("fire_volume.wesl"), "p2_render/fire_volume.wesl"))]);
        add_view_pass::<FireVolumes>(app);
        app.init_resource::<VolumeFires>().add_systems(PostUpdate, follow_fires.after(tag_pass_views).in_set(FireVolumeSystems));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<FirePipeline>>()
            .add_systems(RenderStartup, init_fire_pipeline)
            .add_systems(Render, prepare_fire_views.in_set(RenderSystems::PrepareResources))
            .add_systems(Core3d, fire_volume.in_set(MainPassExtraSystems::FireVolume));
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
#[derive(Component, Debug, Clone, PartialEq, ExtractComponent)]
#[extract_app(RenderApp)]
#[extract_component_sync_target((Self, FireView))]
pub struct FireVolumes {
    quality: Quality,
    seconds: f32,
    volumes: Vec<FireBox>,
}

/// The emitters, by parent and order, drawn as volumes this frame: the particle drawing hides their sprites.
#[derive(Resource, Default)]
pub struct VolumeFires(pub(crate) FxHashSet<(Entity, u32)>);

/// Hides the sprites only of fires a view will draw. The fires hidden this frame are the ones picked last frame, so no
/// frame draws a fire as both.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn follow_fires(
    mut commands: Commands,
    settings: Option<Res<Settings>>,
    time: Res<Time>,
    facts: Res<FrameFacts>,
    draws: Option<Res<ParticleDraws>>,
    drawn: Option<Res<DrawnParticles>>,
    mut hidden: ResMut<VolumeFires>,
    views: Query<(Entity, &GlobalTransform, Has<PassView>, Option<&FireVolumes>), With<SceneView>>,
) {
    let wanted = quality(modern_level(settings.as_deref(), MODERN_FIRE_VOLUMES));
    let eye = views.iter().find(|&(_, _, drawn_in, _)| drawn_in).map(|(_, at, ..)| at.translation()).filter(|_| wanted.is_some());
    let origin = facts.in_play.then_some(facts.origin);
    let volumes = match (draws.as_deref(), drawn.as_deref(), eye, origin) {
        (Some(draws), Some(drawn), Some(eye), Some(origin)) => {
            let (volumes, next) = draws.fire_volumes(drawn, &hidden.0, origin, eye, MAX_VOLUMES, REACH);
            hidden.0 = next;
            volumes
        }
        _ => {
            hidden.0.clear();
            Vec::new()
        }
    };
    let seconds = time.elapsed_secs_wrapped();
    for (entity, _, drawn_in, shown) in &views {
        let fires = wanted.filter(|_| drawn_in).map(|quality| FireVolumes { quality, seconds, volumes: volumes.clone() });
        follow_view_pass(commands.entity(entity), shown, fires);
    }
}

/// `Volume` in `fire_volume.wesl`.
#[derive(ShaderType, Clone, Copy, Default)]
struct Volume {
    centre: Vec4,
    extent: Vec4,
    tint: Vec4,
}

/// `Fires` in `fire_volume.wesl`.
#[derive(ShaderType)]
struct Fires {
    volumes: [Volume; MAX_VOLUMES],
    count: u32,
    steps: u32,
    octaves: u32,
    seconds: f32,
}

fn fires_of(fires: &FireVolumes) -> Fires {
    let volume = |fire: &FireBox| Volume { centre: fire.centre.extend(0.0), extent: fire.half.extend(0.0), tint: fire.tint };
    Fires {
        volumes: std::array::from_fn(|at| fires.volumes.get(at).map(volume).unwrap_or_default()),
        count: u32::try_from(fires.volumes.len().min(MAX_VOLUMES)).unwrap_or(0),
        steps: fires.quality.steps,
        octaves: fires.quality.octaves,
        seconds: fires.seconds,
    }
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
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn prepare_fire_views(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<FirePipeline>>,
    pipeline: Res<FirePipeline>,
    mut texture_cache: ResMut<TextureCache>,
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
    views: Query<(Entity, &ExtractedView, &FireVolumes, Option<&FireView>)>,
) {
    for (entity, view, fires, prepared) in &views {
        // Asked for before the first fire is near: its sprites are hidden from the frame its volume is picked.
        let march = pipelines.specialize(&pipeline_cache, &pipeline, None);
        let composite = pipelines.specialize(&pipeline_cache, &pipeline, Some(view.target_format));
        if fires.volumes.is_empty() {
            continue;
        }
        let Some(size) = half_viewport(view) else {
            continue;
        };
        let marched = half_texture(&mut texture_cache, &render_device, "fire volume", MARCHED_FORMAT, size);
        let bytes = shader_bytes(&fires_of(fires));
        let fires = match prepared {
            Some(prepared) => {
                render_queue.write_buffer(&prepared.fires, 0, &bytes);
                prepared.fires.clone()
            }
            None => render_device.create_buffer_with_data(&BufferInitDescriptor {
                label: Some("fire volumes"),
                contents: &bytes,
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            }),
        };
        commands.entity(entity).insert(FireView { march, composite, marched, fires });
    }
}

fn fire_volume(
    view: ViewQuery<(&ViewTarget, &ExtractedCamera, &ViewPrepassTextures, &ViewUniformOffset, &FireVolumes, &FireView)>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<FirePipeline>,
    view_uniforms: Res<ViewUniforms>,
    mut scene_groups: Local<ViewBindGroups<(TextureViewId, TextureId, BufferId, BufferId)>>,
    mut marched_groups: Local<ViewBindGroups<(TextureViewId, TextureId, BufferId, BufferId)>>,
    mut ctx: RenderContext,
) {
    let entity = view.entity();
    let (target, camera, prepass, offset, fires, prepared) = view.into_inner();
    if fires.volumes.is_empty() {
        return;
    }
    let (Some(march), Some(composite), Some(depth), Some(depth_texture), Some(uniform), Some(uniforms)) = (
        pipeline_cache.get_render_pipeline(prepared.march),
        pipeline_cache.get_render_pipeline(prepared.composite),
        prepass.depth_only_view(),
        prepass.depth.as_ref().map(|depth| depth.texture.texture.id()),
        view_uniforms.uniforms.binding(),
        view_uniforms.uniforms.buffer().map(|buffer| buffer.id()),
    ) else {
        return;
    };
    let layout = pipeline_cache.get_bind_group_layout(&pipeline.layout);
    let device = ctx.render_device();
    let scene = target.main_texture_view();
    let from_scene = scene_groups.get_or_make(entity, (scene.id(), depth_texture, uniforms, prepared.fires.id()), || {
        device.create_bind_group("fire volume march", &layout, &BindGroupEntries::sequential((scene, depth, uniform.clone(), prepared.fires.as_entire_binding())))
    });
    let marched = &prepared.marched.default_view;
    let from_marched = marched_groups.get_or_make(entity, (marched.id(), depth_texture, uniforms, prepared.fires.id()), || {
        device.create_bind_group("fire volume", &layout, &BindGroupEntries::sequential((marched, depth, uniform, prepared.fires.as_entire_binding())))
    });
    HalfThenFull {
        span: "fire_volume",
        half: marched,
        march: ScreenStep { label: "fire volume march", pipeline: march, bind_group: from_scene },
        march_blend_constant: None,
        composite: ScreenStep { label: "fire volume", pipeline: composite, bind_group: from_marched },
        offset: offset.offset,
    }
    .draw(&mut ctx, target, camera);
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
    fn the_uniform_counts_only_the_volumes_it_holds() {
        Fires::assert_uniform_compat();
        let fire = FireBox { centre: Vec3::ONE, half: Vec3::splat(2.0), tint: Vec4::ONE };
        let fires = FireVolumes { quality: Quality { steps: 16, octaves: 3 }, seconds: 1.5, volumes: vec![fire; MAX_VOLUMES + 3] };
        assert_eq!(fires_of(&fires).count, MAX_VOLUMES as u32);
        assert_eq!(fires_of(&fires).volumes[MAX_VOLUMES - 1].extent, Vec4::new(2.0, 2.0, 2.0, 0.0));
        assert_eq!(fires_of(&FireVolumes { volumes: Vec::new(), ..fires }).count, 0);
    }
}
