//! Modern graphics: lamp-lit fog. Between the opaque and the see-through passes a half-size pass marches each view ray
//! to its depth through the nearest lamps' light, and a full-size pass blurs that where depths agree and adds it to the
//! frame. Lamps only, without shadows: the fog never darkens or greys, and the sun's rays stay the tone pass's.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Handle, uuid_handle};
use bevy::camera::{Camera, CameraUpdateSystems};
use bevy::camera::visibility::InheritedVisibility;
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::prepass::ViewPrepassTextures;
use bevy::core_pipeline::schedule::Core3d;
use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Has, Local, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::math::{Mat4, UVec2, UVec4, Vec3, Vec4};
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, BufferId, CachedRenderPipelineId,
    ColorTargetState, ColorWrites, FragmentState, PipelineCache, RenderPipelineDescriptor, ShaderStages, ShaderType, SpecializedRenderPipeline,
    SpecializedRenderPipelines, TextureFormat, TextureId, TextureSampleType, TextureViewId, binding_types,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use bevy::transform::TransformSystems;
use bevy::transform::components::{GlobalTransform, Transform};
use p2_assets::Model;

use crate::components::SceneView;
use p2_host::display::{MODERN_VOLUMETRIC_FOG, Settings};
use crate::level_collision::LevelCollision;
use crate::MainPassExtraSystems;
use crate::lamps::{Lamp, Placed, lamp_falloff};
use crate::modern::linear_light;
use crate::screen_pass::{
    HalfThenFull, PassUniformOffset, PassUniforms, PassView, ScreenStep, ViewBindGroups, add_view_pass, follow_view_pass, half_texture, prepare_pass_uniforms,
    tag_pass_views,
};
use crate::shaders::{add_libraries, add_shaders};
use crate::view::ShownScene;

const LAMP_FOG_SHADER: Handle<Shader> = uuid_handle!("5d8c2f71-0e4b-4a96-b3d7-91f6c4a2e83d");
const FOG_LAMPS: usize = 4;
const STEPS: f32 = 16.0;
/// How far along the view a lamp's fog is looked for, and how far a ray into the sky is marched.
const VIEW_REACH: f32 = 4096.0;
/// Without a shadow a lamp's fog would show through walls, so it reaches only this share of the lamp's range.
const FOG_REACH: f32 = 0.6;
/// At level 3 a ray through the middle of a lamp of range 750 gathers about a sixth of the lamp's colour.
const DENSITY_PER_LEVEL: f32 = 6e-5;
const MARCH_FORMAT: TextureFormat = TextureFormat::Rgba16Float;
const ADD: BlendState = BlendState {
    color: BlendComponent { src_factor: BlendFactor::One, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
    alpha: BlendComponent::REPLACE,
};

pub struct LampFogPlugin;

impl Plugin for LampFogPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_libraries(app);
        add_shaders(app, [(LAMP_FOG_SHADER, Shader::from_wesl(include_str!("lamp_fog.wesl"), "p2_render/lamp_fog.wesl"))]);
        add_view_pass::<LampFog>(app);
        app.add_systems(PostUpdate, follow_lamp_fog_settings.after(TransformSystems::Propagate).after(CameraUpdateSystems).after(tag_pass_views));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_gpu_resource::<PassUniforms<LampFog>>()
            .init_gpu_resource::<SpecializedRenderPipelines<LampFogPipeline>>()
            .add_systems(RenderStartup, init_lamp_fog_pipeline)
            .add_systems(Render, prepare_pipelines.in_set(RenderSystems::Prepare))
            .add_systems(Render, (prepare_pass_uniforms::<LampFog>, prepare_textures).in_set(RenderSystems::PrepareResources))
            .add_systems(Core3d, draw_fog.in_set(MainPassExtraSystems::LampFog));
    }
}

#[derive(ShaderType, Debug, Clone, Copy, PartialEq, Default)]
pub struct FogLamp {
    /// In view space; w the fog's reach.
    place: Vec4,
    /// Linear light; w a spot's cone cosine, below -1 for a point lamp.
    color: Vec4,
    along: Vec4,
}

/// On a scene camera while lamp fog shows. As `lamp_fog.wesl` reads it.
#[derive(Component, ShaderType, Debug, Clone, Copy, PartialEq, ExtractComponent)]
#[extract_app(RenderApp)]
#[extract_component_sync_target((Self, LampFogPipelineIds, PassUniformOffset<Self>, LampFogTexture))]
pub struct LampFog {
    view_from_clip: Mat4,
    lamps: [FogLamp; FOG_LAMPS],
    /// x the density a unit, y the lamps, z the steps, w the reach into the sky.
    look: Vec4,
    /// The view's part of the target in pixels: corner in xy, size in zw.
    viewport: Vec4,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct FogSource {
    at: Vec3,
    color: Vec3,
    range: f32,
    spot: Option<(Vec3, f32)>,
}

/// A lamp scored by its light at the eye and at the points of the view's line nearest to it and to its cone's middle.
fn view_score(source: &FogSource, eye: Vec3, forward: Vec3) -> f32 {
    let reach = source.range * FOG_REACH;
    let on_view = |point: Vec3| eye + forward * (point - eye).dot(forward).clamp(0.0, VIEW_REACH);
    let middle = source.spot.map_or(source.at, |(along, _)| source.at + along * (0.5 * reach));
    let light = [eye, on_view(source.at), on_view(middle)].into_iter().map(|point| lamp_falloff(source.at, reach, source.spot, point)).fold(0.0, f32::max);
    light * source.color.element_sum()
}

/// The lamps whose fog the view shows, strongest first.
fn pick_lamps(sources: &[FogSource], eye: Vec3, forward: Vec3) -> Vec<FogSource> {
    let mut scored: Vec<(f32, FogSource)> = sources.iter().map(|source| (view_score(source, eye, forward), *source)).filter(|(score, _)| *score > 0.0).collect();
    scored.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().take(FOG_LAMPS).map(|(_, source)| source).collect()
}

/// The fog's density a unit: none without a lamp in reach, nor where the eye's zone is open to the sky and its sun
/// outshines the strongest lamp (daylight would grey).
fn fog_density(level: u8, open_to_sky: bool, sun: f32, strongest_lamp: Option<f32>) -> f32 {
    match strongest_lamp {
        Some(lamp) if level > 0 && !(open_to_sky && sun >= lamp) => f32::from(level) * DENSITY_PER_LEVEL,
        _ => 0.0,
    }
}

/// Which zones see the sky through a backdrop.
fn sky_zones(model: &Model) -> Vec<bool> {
    let mut open = vec![false; model.zones.len()];
    for (index, node) in model.nodes.iter().enumerate() {
        if p2_collision::is_sky(model, index)
            && let Some(zone) = open.get_mut(usize::from(node.zones[1]))
        {
            *zone = true;
        }
    }
    open
}

fn follow_lamp_fog_settings(
    mut commands: Commands,
    settings: Option<Res<Settings>>,
    scene: (Option<Res<ShownScene>>, Option<Res<LevelCollision>>),
    mut open_zones: Local<(Option<u32>, Vec<bool>)>,
    lamps: Query<(&Transform, &Placed, &InheritedVisibility), With<Lamp>>,
    views: Query<(Entity, &Camera, &GlobalTransform, Has<PassView>, Option<&LampFog>), With<SceneView>>,
) {
    let level = settings.as_ref().map_or(0, |settings| settings.modern_level(MODERN_VOLUMETRIC_FOG));
    let (shown, collision) = scene;
    let model = collision.as_ref().and_then(|collision| collision.model());
    let sources: Vec<FogSource> = if level > 0 {
        lamps
            .iter()
            .filter(|(.., seen)| seen.get())
            .map(|(at, placed, _)| FogSource { at: at.translation, color: placed.color, range: placed.range, spot: placed.spot })
            .collect()
    } else {
        Vec::new()
    };
    if let (Some(collision), Some(model)) = (&collision, model)
        && !sources.is_empty()
        && open_zones.0 != Some(collision.last_changed().get())
    {
        *open_zones = (Some(collision.last_changed().get()), sky_zones(model));
    }
    let sun = shown.as_ref().and_then(|shown| shown.lighting.as_ref()).and_then(|lighting| {
        lighting.sun.as_ref().map(|(light, _)| Vec3::from(linear_light(light.color(lighting.level_brightness))).element_sum())
    });
    for (entity, camera, place, drawn_in, shown_fog) in &views {
        let world_from_view = place.to_matrix();
        let eye = world_from_view.w_axis.truncate();
        let picked = if drawn_in { pick_lamps(&sources, eye, -world_from_view.z_axis.truncate().normalize_or_zero()) } else { Vec::new() };
        let zone = model.zip(shown.as_ref()).map(|(model, shown)| p2_collision::point_zone(model, p2_bevy_assets::to_bevy(eye - shown.origin).into()));
        let open = zone.is_some_and(|zone| open_zones.1.get(usize::from(zone)).copied().unwrap_or(false));
        let density = fog_density(level, open, sun.unwrap_or(0.0), picked.iter().map(|source| source.color.element_sum()).reduce(f32::max));
        if density <= 0.0 {
            follow_view_pass(commands.entity(entity), shown_fog, None);
            continue;
        }
        let view_from_world = world_from_view.inverse();
        let lamps = std::array::from_fn(|index| {
            picked.get(index).map_or(FogLamp::default(), |source| FogLamp {
                place: view_from_world.transform_point3(source.at).extend(source.range * FOG_REACH),
                color: source.color.extend(source.spot.map_or(-2.0, |(_, cos)| cos)),
                along: source.spot.map_or(Vec4::ZERO, |(along, _)| view_from_world.transform_vector3(along).extend(0.0)),
            })
        });
        let viewport = camera.physical_viewport_rect().map_or(Vec4::ZERO, |rect| UVec4::new(rect.min.x, rect.min.y, rect.width(), rect.height()).as_vec4());
        let fog = LampFog { view_from_clip: camera.clip_from_view().inverse(), lamps, look: Vec4::new(density, picked.len() as f32, STEPS, VIEW_REACH), viewport };
        follow_view_pass(commands.entity(entity), shown_fog, Some(fog));
    }
}

#[derive(Resource)]
pub struct LampFogPipeline {
    march: BindGroupLayoutDescriptor,
    composite: BindGroupLayoutDescriptor,
    fullscreen: FullscreenShader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FogStage {
    March,
    Composite(TextureFormat),
}

#[derive(Component)]
pub struct LampFogPipelineIds {
    march: CachedRenderPipelineId,
    composite: CachedRenderPipelineId,
}

/// The half-size march.
#[derive(Component)]
pub struct LampFogTexture(CachedTexture);

fn init_lamp_fog_pipeline(mut commands: Commands, fullscreen: Res<FullscreenShader>) {
    let march = BindGroupLayoutDescriptor::new(
        "lamp_fog_march",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (binding_types::texture_2d(TextureSampleType::Depth), binding_types::uniform_buffer::<LampFog>(true)),
        ),
    );
    let composite = BindGroupLayoutDescriptor::new(
        "lamp_fog",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                binding_types::texture_2d(TextureSampleType::Depth),
                binding_types::uniform_buffer::<LampFog>(true),
                binding_types::texture_2d(TextureSampleType::Float { filterable: false }),
            ),
        ),
    );
    commands.insert_resource(LampFogPipeline { march, composite, fullscreen: fullscreen.clone() });
}

impl SpecializedRenderPipeline for LampFogPipeline {
    type Key = FogStage;

    fn specialize(&self, stage: FogStage) -> RenderPipelineDescriptor {
        let (layout, entry, target) = match stage {
            FogStage::March => (&self.march, "march", ColorTargetState { format: MARCH_FORMAT, blend: None, write_mask: ColorWrites::ALL }),
            FogStage::Composite(format) => (&self.composite, "composite", ColorTargetState { format, blend: Some(ADD), write_mask: ColorWrites::COLOR }),
        };
        RenderPipelineDescriptor {
            label: Some(entry.into()),
            layout: vec![layout.clone()],
            vertex: self.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState { shader: LAMP_FOG_SHADER, entry_point: Some(entry.into()), targets: vec![Some(target)], ..Default::default() }),
            ..Default::default()
        }
    }
}

fn prepare_pipelines(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<LampFogPipeline>>,
    pipeline: Res<LampFogPipeline>,
    views: Query<(Entity, &ExtractedView), With<LampFog>>,
) {
    for (entity, view) in &views {
        let march = pipelines.specialize(&pipeline_cache, &pipeline, FogStage::March);
        let composite = pipelines.specialize(&pipeline_cache, &pipeline, FogStage::Composite(view.target_format));
        commands.entity(entity).insert(LampFogPipelineIds { march, composite });
    }
}

fn prepare_textures(mut commands: Commands, mut cache: ResMut<TextureCache>, render_device: Res<RenderDevice>, views: Query<(Entity, &ExtractedView), With<LampFog>>) {
    for (entity, view) in &views {
        let size = UVec2::new(view.viewport.z.div_ceil(2).max(1), view.viewport.w.div_ceil(2).max(1));
        let texture = half_texture(&mut cache, &render_device, "lamp_fog_march", MARCH_FORMAT, size);
        commands.entity(entity).insert(LampFogTexture(texture));
    }
}

fn draw_fog(
    view: ViewQuery<(&ViewTarget, &ExtractedCamera, &ViewPrepassTextures, &LampFogPipelineIds, &PassUniformOffset<LampFog>, &LampFogTexture)>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<LampFogPipeline>,
    buffer: Res<PassUniforms<LampFog>>,
    mut march_groups: Local<ViewBindGroups<(TextureId, BufferId)>>,
    mut composite_groups: Local<ViewBindGroups<(TextureId, BufferId, TextureViewId)>>,
    mut ctx: RenderContext,
) {
    let entity = view.entity();
    let (target, camera, prepass, ids, offset, marched) = view.into_inner();
    let (Some(march), Some(composite), Some(depth), Some(depth_texture), Some(uniform), Some(uniform_again), Some(fogs)) = (
        pipeline_cache.get_render_pipeline(ids.march),
        pipeline_cache.get_render_pipeline(ids.composite),
        prepass.depth_only_view(),
        prepass.depth.as_ref().map(|depth| depth.texture.texture.id()),
        buffer.0.binding(),
        buffer.0.binding(),
        buffer.0.buffer().map(|fogs| fogs.id()),
    ) else {
        return;
    };
    let march_group = march_groups.get_or_make(entity, (depth_texture, fogs), || {
        ctx.render_device().create_bind_group("lamp_fog_march", &pipeline_cache.get_bind_group_layout(&pipeline.march), &BindGroupEntries::sequential((depth, uniform)))
    });
    let composite_group = composite_groups.get_or_make(entity, (depth_texture, fogs, marched.0.default_view.id()), || {
        ctx.render_device().create_bind_group(
            "lamp_fog",
            &pipeline_cache.get_bind_group_layout(&pipeline.composite),
            &BindGroupEntries::sequential((depth, uniform_again, &marched.0.default_view)),
        )
    });
    HalfThenFull {
        span: "lamp_fog",
        half: &marched.0.default_view,
        march: ScreenStep { label: "lamp_fog_march", pipeline: march, bind_group: march_group },
        march_blend_constant: None,
        composite: ScreenStep { label: "lamp_fog", pipeline: composite, bind_group: composite_group },
        offset: offset.0,
    }
    .draw(&mut ctx, target, camera);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lamp(at: Vec3, strength: f32, spot: Option<(Vec3, f32)>) -> FogSource {
        FogSource { at, color: Vec3::splat(strength), range: 500.0, spot }
    }

    #[test]
    fn the_strongest_lamps_along_the_view_are_picked() {
        let ahead = lamp(Vec3::new(0.0, 0.0, -1000.0), 1.0, None);
        let behind_far = lamp(Vec3::new(0.0, 0.0, 2000.0), 1.0, None);
        let beside_far = lamp(Vec3::new(3000.0, 0.0, -1000.0), 1.0, None);
        let near_dim = lamp(Vec3::new(0.0, 100.0, 0.0), 0.1, None);
        let picked = pick_lamps(&[behind_far, near_dim, beside_far, ahead], Vec3::ZERO, Vec3::NEG_Z);
        assert_eq!(picked, vec![ahead, near_dim]);
        let many: Vec<FogSource> = (0..8).map(|i| lamp(Vec3::new(0.0, 0.0, -200.0 * i as f32), 1.0 + i as f32, None)).collect();
        let picked = pick_lamps(&many, Vec3::ZERO, Vec3::NEG_Z);
        assert_eq!(picked.len(), FOG_LAMPS);
        assert_eq!(picked[0], many[7]);
    }

    #[test]
    fn a_spot_counts_where_its_cone_crosses_the_view() {
        let down = lamp(Vec3::new(0.0, 250.0, -800.0), 1.0, Some((Vec3::NEG_Y, 0.8)));
        let up = lamp(Vec3::new(0.0, 250.0, -800.0), 1.0, Some((Vec3::Y, 0.8)));
        assert_eq!(pick_lamps(&[down, up], Vec3::ZERO, Vec3::NEG_Z), vec![down]);
    }

    #[test]
    fn density_follows_the_zone_and_the_lamps() {
        assert_eq!(fog_density(0, false, 0.0, Some(1.0)), 0.0);
        assert_eq!(fog_density(5, false, 0.0, None), 0.0);
        assert_eq!(fog_density(5, true, 2.0, Some(1.0)), 0.0);
        assert!(fog_density(5, true, 0.2, Some(1.0)) > 0.0);
        assert!(fog_density(5, false, 2.0, Some(1.0)) > 0.0);
        assert!(fog_density(10, false, 0.0, Some(1.0)) > fog_density(3, false, 0.0, Some(1.0)));
    }
}
