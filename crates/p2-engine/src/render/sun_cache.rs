//! The static world's sun shadow, cached. Each level is a black sun with one cascade around the eye, so it gets a layer of
//! Bevy's shadow texture, its uniforms and its culling. A level draws only on the frame it is stale; every other frame it
//! has no casters and its layer is loaded instead of cleared. Movers stay in the real sun's cascades.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bevy::app::{App, Plugin, PostUpdate};
use bevy::camera::Camera;
use bevy::camera::visibility::{InheritedVisibility, RenderLayers};
use bevy::color::Color;
use bevy::core_pipeline::schedule::RootNonCameraView;
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::prelude::{Changed, Commands, Component, Entity, Local, Query, Res, Resource, With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::light::cascade::Cascade;
use bevy::light::{CascadeShadowConfig, Cascades, DirectionalLight, DirectionalLightShadowMap, SimulationLightSystems};
use bevy::math::{Mat4, Vec2, Vec3, Vec4};
use bevy::mesh::Mesh3d;
use bevy::pbr::{LightEntity, Shadow, ShadowView, ViewLightEntities, ViewShadowBindings};
use bevy::platform::collections::{HashMap, HashSet};
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_phase::{BinnedRenderPhase, ViewBinnedRenderPhases};
use bevy::render::render_resource::{PipelineCache, TextureId};
use bevy::render::texture::DepthStencilViewAttachment;
use bevy::render::view::{ExtractedView, RetainedViewEntity};
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::time::{Real, Time};
use bevy::transform::components::{GlobalTransform, Transform};

use crate::components::SceneView;
use crate::display::{MODERN_SHADOW_DISTANCE, MODERN_SHADOWS, Settings};
use crate::render::modern::{STATIC_CASTER_LAYER, SUN_VIEW_LAYER, StillProp, SunShadow, shadow_reach, sun_quality};

/// The level looking straight down, which tells a room from open sky. Not a power of two, so it never shares a size with
/// another level and the two never redraw on one frame.
const SKY_HALF_WIDTH: f32 = 24576.0;

/// What `modern.wesl` tells the sky level by; one cascade never reads its overlap.
const SKY_OVERLAP: f32 = 0.5;

/// Redraws asked for closer than this merge into one, unless the texture is gone and nothing else can show.
const SECONDS_BETWEEN_REDRAWS: f32 = 0.5;

/// The coarsest level a prop's change redraws.
const NEAR_REDRAW_HALF_WIDTH: f32 = 8192.0;

/// A level moves in steps of this share of its half width, once the eye is three quarters of a step off.
const STEP_SHARE: f32 = 0.25;
const MOVE_AT_STEPS: f32 = 0.75;

const DEPTH_REACH: f32 = 120_000.0;

/// Shared by both worlds: `whole` redraws every level, `near` the fine ones, `lost` everything at once (a new texture).
#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub struct SunCacheStale {
    whole: Arc<AtomicBool>,
    near: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
}

impl SunCacheStale {
    pub fn mark(&self) {
        self.whole.store(true, Ordering::Relaxed);
    }

    /// For one prop: the coarser levels' texels are as wide as it is, so its change is not worth their redraw.
    pub fn mark_near(&self) {
        self.near.store(true, Ordering::Relaxed);
    }
}

#[derive(Component, Clone, ExtractComponent)]
#[extract_app(RenderApp)]
pub struct CachedSunLevel {
    half_width: f32,
    center: Option<Vec3>,
    due: bool,
    drawn_now: bool,
}

impl CachedSunLevel {
    pub fn new(half_width: f32) -> Self {
        Self { half_width, center: None, due: true, drawn_now: false }
    }

    pub fn drawn_now(&self) -> bool {
        self.drawn_now
    }
}

fn level_light() -> DirectionalLight {
    DirectionalLight { color: Color::BLACK, illuminance: 0.0, shadow_maps_enabled: true, affects_lightmapped_mesh_diffuse: false, ..DirectionalLight::default() }
}

pub fn level_config(half_width: f32) -> CascadeShadowConfig {
    CascadeShadowConfig { bounds: vec![half_width], overlap_proportion: 0.0, minimum_distance: 0.1 }
}

/// Outermost first, each a quarter of the one before. Powers of two keep a level's moves whole texels, so its shadows do
/// not shimmer when it redraws.
fn level_half_widths(settings: &Settings) -> Vec<f32> {
    let outermost = shadow_reach(settings.modern_level(MODERN_SHADOW_DISTANCE));
    std::iter::successors(Some(outermost), |width| Some(width / 4.0)).take(sun_quality(settings.modern_level(MODERN_SHADOWS)).2).collect()
}

fn snapped(eye: Vec3, half_width: f32) -> Vec3 {
    let step = half_width * STEP_SHARE;
    let at = (Vec2::new(eye.x, eye.y) / step).round() * step;
    Vec3::new(at.x, at.y, eye.z)
}

fn needs_move(center: Vec3, eye: Vec3, half_width: f32) -> bool {
    (Vec2::new(eye.x, eye.y) - Vec2::new(center.x, center.y)).abs().max_element() > half_width * STEP_SHARE * MOVE_AT_STEPS
}

/// As Bevy's own cascade, for a square of fixed size around `center`, which is in the light's space.
fn level_cascade(world_from_light: Mat4, center: Vec3, half_width: f32, map_size: f32) -> Cascade {
    let near_center = Vec3::new(center.x, center.y, center.z + DEPTH_REACH);
    let light_from_world = world_from_light.transpose();
    let cascade_from_world = Mat4::from_cols(light_from_world.x_axis, light_from_world.y_axis, light_from_world.z_axis, (-near_center).extend(1.0));
    let world_from_cascade = Mat4::from_cols(world_from_light.x_axis, world_from_light.y_axis, world_from_light.z_axis, world_from_light * near_center.extend(1.0));
    let clip_from_cascade = Mat4::from_cols(
        Vec4::new(1.0 / half_width, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 1.0 / half_width, 0.0, 0.0),
        Vec4::new(0.0, 0.0, 0.5 / DEPTH_REACH, 0.0),
        Vec4::new(0.0, 0.0, 1.0, 1.0),
    );
    Cascade { world_from_cascade, clip_from_cascade, clip_from_world: clip_from_cascade * cascade_from_world, texel_size: 2.0 * half_width / map_size }
}

#[derive(Default)]
struct RedrawRequests {
    whole: bool,
    near: bool,
    last: Option<f32>,
}

impl RedrawRequests {
    /// Whether the whole cache and the near levels redraw now.
    fn step(&mut self, lost: bool, now: f32) -> (bool, bool) {
        let rested = self.last.is_none_or(|last| now - last >= SECONDS_BETWEEN_REDRAWS);
        if !lost && !(rested && (self.whole || self.near)) {
            return (false, false);
        }
        self.last = Some(now);
        let whole = lost || self.whole;
        let near = !whole && self.near;
        self.whole = false;
        self.near = false;
        (whole, near)
    }
}

/// At most one level draws a frame, the finest that is due; the others keep their layer.
fn place_levels(
    stale: Res<SunCacheStale>,
    map: Res<DirectionalLightShadowMap>,
    cameras: Query<(Entity, &GlobalTransform, &Camera), With<SceneView>>,
    casters: Query<&RenderLayers, (With<Mesh3d>, Changed<InheritedVisibility>, Without<CachedSunLevel>, Without<StillProp>)>,
    mut levels: Query<(&mut CachedSunLevel, &mut Cascades, &GlobalTransform, &mut RenderLayers)>,
    time: Res<Time<Real>>,
    mut asked: Local<RedrawRequests>,
) {
    let static_casters = RenderLayers::layer(STATIC_CASTER_LAYER);
    if casters.iter().any(|layers| layers.intersects(&static_casters)) {
        stale.mark();
    }
    asked.whole |= stale.whole.swap(false, Ordering::Relaxed);
    asked.near |= stale.near.swap(false, Ordering::Relaxed);
    let (redraw_all, redraw_near) = asked.step(stale.lost.swap(false, Ordering::Relaxed), time.elapsed_secs());
    let Some((camera, eye, _)) = cameras.iter().find(|(_, _, camera)| camera.is_active) else { return };
    let world_from_light = |light: &GlobalTransform| Mat4::from_quat(light.rotation());
    for (mut level, _, light, _) in &mut levels {
        let eye = world_from_light(light).transpose().transform_point3(eye.translation());
        let half_width = level.half_width;
        let moved = level.center.is_none_or(|center| needs_move(center, eye, half_width));
        if level.center.is_none() {
            level.center = Some(snapped(eye, half_width));
        }
        level.due |= redraw_all || moved || (redraw_near && half_width <= NEAR_REDRAW_HALF_WIDTH);
    }
    let finest = levels.iter().filter(|(level, ..)| level.due).map(|(level, ..)| level.half_width).min_by(f32::total_cmp);
    for (mut level, mut cascades, light, mut layers) in &mut levels {
        let drawn_now = finest == Some(level.half_width);
        let world_from_light = world_from_light(light);
        if drawn_now {
            level.center = Some(snapped(world_from_light.transpose().transform_point3(eye.translation()), level.half_width));
            level.due = false;
        }
        level.drawn_now = drawn_now;
        let idle = RenderLayers::layer(SUN_VIEW_LAYER);
        layers.set_if_neq(if drawn_now { idle.with(STATIC_CASTER_LAYER) } else { idle });
        cascades.cascades.clear();
        if let Some(center) = level.center {
            cascades.cascades.insert(camera, vec![level_cascade(world_from_light, center, level.half_width, map.size as f32)]);
        }
    }
}

/// Keeps one level per size, and the sky level, while the real sun casts. Runs after the system that owns that sun.
pub(crate) fn follow_sun(
    settings: Res<Settings>,
    stale: Res<SunCacheStale>,
    suns: Query<(&DirectionalLight, &Transform), With<SunShadow>>,
    mut levels: Query<(Entity, &CachedSunLevel, &mut Transform), Without<SunShadow>>,
    mut commands: Commands,
) {
    let place = suns.iter().find(|(light, _)| light.shadow_maps_enabled).map(|(_, place)| *place);
    let mut wanted = place.map(|_| level_half_widths(&settings)).unwrap_or_default();
    if place.is_some() {
        wanted.push(SKY_HALF_WIDTH);
    }
    wanted.sort_by(f32::total_cmp);
    let mut have: Vec<f32> = levels.iter().map(|(_, level, _)| level.half_width).collect();
    have.sort_by(f32::total_cmp);
    if have != wanted {
        for (entity, ..) in &levels {
            commands.entity(entity).despawn();
        }
        let idle = RenderLayers::layer(SUN_VIEW_LAYER);
        for &half_width in &wanted {
            let (config, place) = if half_width == SKY_HALF_WIDTH {
                (CascadeShadowConfig { overlap_proportion: SKY_OVERLAP, ..level_config(half_width) }, Transform::IDENTITY.looking_to(Vec3::NEG_Y, Vec3::Z))
            } else {
                (level_config(half_width), place.unwrap_or_default())
            };
            commands.spawn((CachedSunLevel::new(half_width), level_light(), config, place, idle.clone()));
        }
        return;
    }
    let Some(place) = place else { return };
    for (_, level, mut transform) in &mut levels {
        if level.half_width != SKY_HALF_WIDTH && transform.set_if_neq(place) {
            stale.mark();
        }
    }
}

#[derive(Default)]
struct CacheWatch {
    pipelines_waiting: bool,
    texture: Option<TextureId>,
    passes: Vec<String>,
}

/// The layers survive between frames only if Bevy hands back the same texture and the same layer order, which it does while
/// the set of shadowed lights stays the same; anything else asks for a redraw.
fn keep_cached_levels(
    stale: Res<SunCacheStale>,
    cache: Res<PipelineCache>,
    levels: Query<&CachedSunLevel>,
    bindings: Query<&ViewShadowBindings>,
    mut views: Query<(&LightEntity, &mut ShadowView)>,
    mut watch: Local<CacheWatch>,
) {
    let waiting = cache.waiting_pipelines().next().is_some();
    if std::mem::replace(&mut watch.pipelines_waiting, waiting) && !waiting {
        stale.mark();
    }
    let mut passes = Vec::new();
    for (light, mut view) in &mut views {
        let LightEntity::Directional { light_entity, .. } = light else { continue };
        let Ok(level) = levels.get(*light_entity) else { continue };
        passes.push(view.pass_name.clone());
        if !level.drawn_now {
            view.depth_attachment = DepthStencilViewAttachment::new(view.depth_attachment.depth_stencil_views.clone(), None, None);
        }
    }
    passes.sort_unstable();
    if watch.passes != passes {
        watch.passes = passes;
        stale.mark();
    }
    let texture = bindings.iter().next().map(|binding| binding.directional_light_depth_texture.id());
    if texture.is_some() && std::mem::replace(&mut watch.texture, texture) != texture {
        stale.lost.store(true, Ordering::Relaxed);
    }
}

/// A shadow view with nothing to draw still costs a render pass, and a lamp's cube face a whole run of the 3D schedule.
/// Its layer needs one clear after its last draw, which a pass without draws does (a cached level's pass loads instead);
/// from then on the layer is already right. A sun's view leaves its camera's list, a lamp's face loses the mark that
/// makes Bevy run it, which Bevy sets again each frame.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn skip_idle_shadow_views(
    mut commands: Commands,
    levels: Query<&CachedSunLevel>,
    phases: Res<ViewBinnedRenderPhases<Shadow>>,
    views: Query<(Entity, &ExtractedView, &ShadowView, &LightEntity)>,
    mut cameras: Query<(&mut ViewLightEntities, &ViewShadowBindings)>,
    mut cleared: Local<HashMap<RetainedViewEntity, String>>,
    mut texture: Local<Option<(TextureId, TextureId)>>,
    mut idle: Local<HashSet<Entity>>,
) {
    let now = cameras.iter().next().map(|(_, bindings)| (bindings.directional_light_depth_texture.id(), bindings.point_light_depth_texture.id()));
    if now.is_some() && std::mem::replace(&mut *texture, now) != now {
        cleared.clear();
    }
    idle.clear();
    let mut seen = HashMap::with_capacity(cleared.len());
    for (entity, view, shadow, light) in &views {
        let redrawing = matches!(light, LightEntity::Directional { light_entity, .. } if levels.get(*light_entity).is_ok_and(CachedSunLevel::drawn_now));
        if redrawing || !phases.get(&view.retained_view_entity).is_none_or(BinnedRenderPhase::is_empty) {
            continue;
        }
        if cleared.get(&view.retained_view_entity).is_some_and(|name| *name == shadow.pass_name) {
            idle.insert(entity);
        }
        seen.insert(view.retained_view_entity, shadow.pass_name.clone());
    }
    *cleared = seen;
    for (mut lights, _) in &mut cameras {
        lights.lights.retain(|light| !idle.contains(light));
    }
    for (entity, _, _, light) in &views {
        if idle.contains(&entity) && !matches!(light, LightEntity::Directional { .. }) {
            commands.entity(entity).remove::<RootNonCameraView>();
        }
    }
}

pub struct SunCachePlugin;

impl Plugin for SunCachePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SunCacheStale>();
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        app.add_plugins((ExtractResourcePlugin::<SunCacheStale>::default(), ExtractComponentPlugin::<CachedSunLevel>::default()));
        app.add_systems(
            PostUpdate,
            place_levels.after(SimulationLightSystems::UpdateDirectionalLightCascades).before(SimulationLightSystems::UpdateLightFrusta),
        );
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.add_systems(Render, (keep_cached_levels, skip_idle_shadow_views).chain().in_set(RenderSystems::PrepareResources));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redraws_merge_but_a_lost_texture_goes_at_once() {
        let mut asked = RedrawRequests { whole: true, ..RedrawRequests::default() };
        assert_eq!(asked.step(false, 10.0), (true, false));
        asked.near = true;
        assert_eq!(asked.step(false, 10.1), (false, false));
        assert_eq!(asked.step(false, 10.6), (false, true));
        assert_eq!(asked.step(false, 11.2), (false, false));
        assert_eq!(asked.step(true, 11.3), (true, false));
    }

    #[test]
    fn a_whole_redraw_takes_a_near_request_with_it() {
        let mut asked = RedrawRequests { whole: true, near: true, last: None };
        assert_eq!(asked.step(false, 1.0), (true, false));
        assert!(!asked.near);
    }

    #[test]
    fn a_level_moves_only_after_the_eye_leaves_its_step() {
        let center = snapped(Vec3::new(100.0, -100.0, 5.0), 512.0);
        assert_eq!(center, Vec3::new(128.0, -128.0, 5.0));
        assert!(!needs_move(center, Vec3::new(128.0 + 96.0, -128.0, 900.0), 512.0));
        assert!(needs_move(center, Vec3::new(128.0 + 97.0, -128.0, 5.0), 512.0));
    }

    #[test]
    fn the_levels_follow_the_settings_and_every_step_is_whole_texels() {
        let settings = |sun: &'static str, distance: &'static str| {
            Settings::read(move |query| match query {
                crate::display::MODERN_GRAPHICS => "True".to_owned(),
                MODERN_SHADOWS => sun.to_owned(),
                MODERN_SHADOW_DISTANCE => distance.to_owned(),
                _ => String::new(),
            })
        };
        assert_eq!(level_half_widths(&settings("1", "1")), [2048.0, 512.0, 128.0]);
        assert_eq!(level_half_widths(&settings("4", "5")), [32768.0, 8192.0, 2048.0, 512.0]);
        for level in 1..=4 {
            let (size, _, count) = sun_quality(level);
            for distance in 1..=5 {
                let outermost = shadow_reach(distance);
                for half_width in std::iter::successors(Some(outermost), |width| Some(width / 4.0)).take(count) {
                    let steps = half_width * STEP_SHARE / (2.0 * half_width / size as f32);
                    assert_eq!(steps.fract(), 0.0);
                    assert!(half_width != SKY_HALF_WIDTH);
                }
            }
        }
    }

    #[test]
    fn the_cascade_holds_the_eye_and_the_whole_depth_range() {
        let light = Mat4::from_rotation_x(0.7);
        let eye = Vec3::new(300.0, 40.0, -900.0);
        let center = snapped(light.transpose().transform_point3(eye), 512.0);
        let cascade = level_cascade(light, center, 512.0, 2048.0);
        assert_eq!(cascade.texel_size, 0.5);
        let at = |point: Vec3| cascade.clip_from_world.project_point3(point);
        assert!(at(eye).x.abs() < 0.25 && at(eye).y.abs() < 0.25);
        assert!((at(eye).z - 0.5).abs() < 1e-3);
        let along = light.transform_vector3(Vec3::Z);
        assert!((at(eye + along * DEPTH_REACH).z - 1.0).abs() < 1e-3);
        assert!(at(eye - along * DEPTH_REACH).z.abs() < 1e-3);
    }
}
