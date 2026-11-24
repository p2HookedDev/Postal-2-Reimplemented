//! The overlay cameras (the weapon, the foot) read the scene camera's sun cascades, so their actors take per-
//! pixel sun shadows without shadow passes of their own.

use bevy::app::{App, Plugin};
use bevy::ecs::prelude::{Entity, Query, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::SystemChangeTick;
use bevy::pbr::{ExtractedDirectionalLight, LightEntity, ViewLightEntities, prepare_lights};
use bevy::render::view::{ExtractedView, RenderShadowMapVisibleEntities, RenderVisibleEntities};
use bevy::render::{Render, RenderApp, RenderSystems};

use crate::view::OverlayCamera;
use crate::world_pass::WorldCamera;

pub struct OverlaySunPlugin;

impl Plugin for OverlaySunPlugin {
    fn build(&self, app: &mut App) {
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.add_systems(
                Render,
                (share_sun_cascades.before(prepare_lights), hold_overlay_sun_passes.after(prepare_lights)).in_set(RenderSystems::CreateViews),
            );
        }
    }
}

/// Every view starts its cascades at layer 0 of the one shared texture, so a second shadowed sun would not line
/// up.
fn one_shadowed_sun<'a>(mut shadowed: impl Iterator<Item = &'a ExtractedDirectionalLight>) -> bool {
    shadowed.next().is_some() && shadowed.next().is_none()
}

/// The main world builds no cascades for the overlays, and `prepare_lights` unwraps a view's entry for every
/// shadowed sun it sees. A missing scene entry still gets an empty list rather than that panic.
fn share_sun_cascades(mut lights: Query<&mut ExtractedDirectionalLight>, scenes: Query<Entity, With<WorldCamera>>, overlays: Query<Entity, With<OverlayCamera>>) {
    if !one_shadowed_sun(lights.iter().filter(|light| light.shadow_maps_enabled)) {
        return;
    }
    let scene = scenes.single().ok();
    for mut light in &mut lights {
        if !light.shadow_maps_enabled {
            continue;
        }
        let cascades = scene.and_then(|scene| light.cascades.get(&scene).cloned()).unwrap_or_default();
        let frusta = scene.and_then(|scene| light.frusta.get(&scene).cloned()).unwrap_or_default();
        for overlay in &overlays {
            light.cascades.insert(overlay, cascades.clone());
            light.frusta.insert(overlay, frusta.clone());
        }
    }
}

/// The overlays' own cascade views would clear the shared layers before the overlays draw; they have nothing to
/// draw. Bevy's shadow systems still look up every light view's visible entities, which the main world never
/// culls for an overlay, so each gets an empty list (the extract drops it again every frame).
fn hold_overlay_sun_passes(
    ticks: SystemChangeTick,
    lights: Query<&ExtractedDirectionalLight>,
    light_views: Query<(&LightEntity, Option<&ExtractedView>)>,
    mut visible: Query<&mut RenderShadowMapVisibleEntities>,
    mut overlays: Query<&mut ViewLightEntities, With<OverlayCamera>>,
) {
    if !one_shadowed_sun(lights.iter().filter(|light| light.shadow_maps_enabled)) {
        return;
    }
    for mut views in &mut overlays {
        views.lights.retain(|&view| {
            let Ok((&LightEntity::Directional { light_entity, .. }, extracted)) = light_views.get(view) else { return true };
            if let (Some(extracted), Ok(mut seen)) = (extracted, visible.get_mut(light_entity)) {
                seen.subviews.entry(extracted.retained_view_entity).or_insert_with(|| (ticks.this_run(), RenderVisibleEntities::default()));
            }
            false
        });
    }
}

#[cfg(test)]
mod tests {
    use bevy::camera::primitives::Frustum;
    use bevy::color::LinearRgba;
    use bevy::ecs::entity::EntityHashMap;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::ecs::world::World;
    use bevy::light::CascadeShadowConfig;
    use bevy::light::cascade::Cascade;
    use bevy::transform::components::GlobalTransform;
    use bevy::math::{Mat4, UVec4};
    use bevy::mesh::Mesh3d;
    use bevy::render::render_resource::TextureFormat;
    use bevy::render::sync_world::MainEntity;
    use bevy::render::view::{ColorGrading, RetainedViewEntity};

    use super::*;

    const SCENE_TEXEL: f32 = 3.0;

    fn shadowed_sun(scene: Entity) -> ExtractedDirectionalLight {
        ExtractedDirectionalLight {
            color: LinearRgba::WHITE,
            illuminance: 1.0,
            transform: GlobalTransform::default(),
            shadow_maps_enabled: true,
            contact_shadows_enabled: false,
            volumetric: false,
            affects_lightmapped_mesh_diffuse: false,
            shadow_depth_bias: 0.0,
            shadow_normal_bias: 0.0,
            cascade_shadow_config: CascadeShadowConfig::default(),
            cascades: EntityHashMap::from_iter([(scene, vec![Cascade { texel_size: SCENE_TEXEL, ..Cascade::default() }; 2])]),
            frusta: EntityHashMap::from_iter([(scene, vec![Frustum::default(); 2])]),
            soft_shadow_size: None,
            occlusion_culling: false,
            sun_disk_angular_size: 0.0,
            sun_disk_intensity: 0.0,
        }
    }

    fn scene_and_overlay(world: &mut World) -> (Entity, Entity) {
        (world.spawn(WorldCamera).id(), world.spawn(OverlayCamera(0)).id())
    }

    #[test]
    fn the_overlay_gets_the_scene_cascades_under_its_own_entity() {
        let mut world = World::new();
        let (scene, overlay) = scene_and_overlay(&mut world);
        let sun = world.spawn(shadowed_sun(scene)).id();
        world.run_system_once(share_sun_cascades).expect("runs");
        let light = world.get::<ExtractedDirectionalLight>(sun);
        let cascades = light.and_then(|light| light.cascades.get(&overlay));
        assert_eq!(cascades.map(|cascades| cascades.iter().map(|cascade| cascade.texel_size).collect::<Vec<_>>()), Some(vec![SCENE_TEXEL; 2]));
        assert_eq!(light.and_then(|light| light.frusta.get(&overlay)).map(Vec::len), Some(2));
        assert_eq!(light.map(|light| light.cascades.len()), Some(2));
    }

    #[test]
    fn nothing_is_shared_with_shadows_off_or_two_shadowed_suns() {
        let mut world = World::new();
        let (scene, overlay) = scene_and_overlay(&mut world);
        let mut unshadowed = shadowed_sun(scene);
        unshadowed.shadow_maps_enabled = false;
        let off = world.spawn(unshadowed).id();
        world.run_system_once(share_sun_cascades).expect("runs");
        assert!(world.get::<ExtractedDirectionalLight>(off).is_some_and(|light| !light.cascades.contains_key(&overlay)));
        let suns = [world.spawn(shadowed_sun(scene)).id(), world.spawn(shadowed_sun(scene)).id()];
        world.run_system_once(share_sun_cascades).expect("runs");
        for sun in suns {
            assert!(world.get::<ExtractedDirectionalLight>(sun).is_some_and(|light| !light.cascades.contains_key(&overlay)));
        }
    }

    #[test]
    fn only_the_overlay_drops_its_cascade_views() {
        let mut world = World::new();
        let (scene, _) = scene_and_overlay(&mut world);
        let sun = world.spawn(shadowed_sun(scene)).id();
        let cascade = world.spawn(LightEntity::Directional { light_entity: sun, cascade_index: 0 }).id();
        let spot = world.spawn(LightEntity::Spot { light_entity: sun }).id();
        let overlay = world.spawn((OverlayCamera(0), ViewLightEntities { lights: vec![cascade, spot] })).id();
        let scene_view = world.spawn((WorldCamera, ViewLightEntities { lights: vec![cascade, spot] })).id();
        world.run_system_once(hold_overlay_sun_passes).expect("runs");
        assert_eq!(world.get::<ViewLightEntities>(overlay).map(|views| views.lights.clone()), Some(vec![spot]));
        assert_eq!(world.get::<ViewLightEntities>(scene_view).map(|views| views.lights.clone()), Some(vec![cascade, spot]));
    }

    #[test]
    fn an_overlay_cascade_view_finds_an_empty_list_of_casters() {
        let mut world = World::new();
        let (scene, _) = scene_and_overlay(&mut world);
        let sun = world.spawn((shadowed_sun(scene), RenderShadowMapVisibleEntities::default())).id();
        let retained = RetainedViewEntity::new(MainEntity::from(sun), None, 0);
        let view = ExtractedView {
            retained_view_entity: retained,
            clip_from_view: Mat4::IDENTITY,
            world_from_view: GlobalTransform::default(),
            clip_from_world: None,
            target_format: TextureFormat::Depth32Float,
            viewport: UVec4::ZERO,
            color_grading: ColorGrading::default(),
            invert_culling: false,
        };
        let cascade = world.spawn((LightEntity::Directional { light_entity: sun, cascade_index: 0 }, view)).id();
        world.spawn((OverlayCamera(0), ViewLightEntities { lights: vec![cascade] }));
        world.run_system_once(hold_overlay_sun_passes).expect("runs");
        let seen = world.get::<RenderShadowMapVisibleEntities>(sun).and_then(|seen| seen.subviews.get(&retained).map(|(_, list)| list.get::<Mesh3d>().is_none()));
        assert_eq!(seen, Some(true));
    }

    #[test]
    fn cascade_views_stay_with_two_shadowed_suns() {
        let mut world = World::new();
        let (scene, _) = scene_and_overlay(&mut world);
        let sun = world.spawn(shadowed_sun(scene)).id();
        world.spawn(shadowed_sun(scene));
        let cascade = world.spawn(LightEntity::Directional { light_entity: sun, cascade_index: 0 }).id();
        let overlay = world.spawn((OverlayCamera(0), ViewLightEntities { lights: vec![cascade] })).id();
        world.run_system_once(hold_overlay_sun_passes).expect("runs");
        assert_eq!(world.get::<ViewLightEntities>(overlay).map(|views| views.lights.clone()), Some(vec![cascade]));
    }
}
