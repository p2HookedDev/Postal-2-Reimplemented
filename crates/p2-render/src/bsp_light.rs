//! Dynamic lights as Bevy point and spot lights, which our shaders read with the engine's formulas;
//! a point light's `radius` carries its `LightEffect`.

use p2_render_cpu::{LE_SPOTLIGHT, LE_STATIC_SPOT, LE_SUNLIGHT};
use bevy::color::{Color, LinearRgba};
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::prelude::{Commands, Component, Entity, Local, Query, Res, ResMut, With};
use bevy::ecs::system::SystemParam;
use bevy::light::{PointLight, SpotLight};
use bevy::math::Vec3;
use bevy::camera::visibility::RenderLayers;
use bevy::transform::components::Transform;
use p2_render_cpu::MeshLight;

use crate::components::{OVERLAY_LAYERS, SKY_LAYER};
use crate::glow_lights::GlowLights;
use crate::view::ShownScene;

#[derive(Component)]
pub struct DynamicLight;

pub(crate) enum BevyLight {
    Point(PointLight),
    Spot(SpotLight),
}

/// `None` for sunlight. Colour in display space; intensity 4π undoes Bevy's lumens to candela. Spots:
/// cone ((cos − sine) / (1 − sine))², sine = 1 − `LightCone` / 256: Bevy's falloff, inner angle 0.
pub(crate) fn gpu_light(light: &MeshLight, level_brightness: f32) -> Option<BevyLight> {
    let [r, g, b] = light.color(level_brightness);
    let color = Color::LinearRgba(LinearRgba::rgb(r, g, b));
    let (intensity, range) = (4.0 * std::f32::consts::PI, light.world_radius());
    match light.effect {
        LE_SUNLIGHT => None,
        LE_STATIC_SPOT | LE_SPOTLIGHT => Some(BevyLight::Spot(SpotLight {
            color,
            intensity,
            range,
            radius: 0.0,
            shadow_maps_enabled: false,
            contact_shadows_enabled: false,
            affects_lightmapped_mesh_diffuse: false,
            inner_angle: 0.0,
            outer_angle: (1.0 - f32::from(light.cone) / 256.0).acos(),
            ..SpotLight::default()
        })),
        effect => Some(BevyLight::Point(PointLight {
            color,
            intensity,
            range,
            radius: f32::from(effect.0),
            shadow_maps_enabled: false,
            contact_shadows_enabled: false,
            affects_lightmapped_mesh_diffuse: false,
            ..PointLight::default()
        })),
    }
}

pub(crate) fn place(light: &MeshLight, gpu: &BevyLight, origin: Vec3) -> Transform {
    let at = Transform::from_translation(Vec3::from(p2_bevy_assets::to_bevy(light.location)) + origin);
    match gpu {
        BevyLight::Point(_) => at,
        BevyLight::Spot(_) => at.looking_to(Vec3::from(p2_bevy_assets::to_bevy(light.rotation.vector())), Vec3::Y),
    }
}

/// The script lights the game reported; read through [`DynamicLightList`], which adds the modern look's own.
#[derive(bevy::ecs::prelude::Resource, Default)]
pub struct FrameLights(Vec<MeshLight>);

impl FrameLights {
    /// Touched only when they changed, so the render systems' change detection stays meaningful.
    pub fn publish(mut frame: ResMut<Self>, lights: &[MeshLight]) {
        if frame.0 != lights {
            frame.0.clear();
            frame.0.extend_from_slice(lights);
        }
    }
}

/// The frame's dynamic lights: the script lights, then the fire and flash lights of the modern look.
#[derive(SystemParam)]
pub struct DynamicLightList<'w> {
    script: Res<'w, FrameLights>,
    glows: Res<'w, GlowLights>,
}

impl DynamicLightList<'_> {
    pub fn iter(&self) -> impl Iterator<Item = &MeshLight> {
        self.script.0.iter().chain(&self.glows.0)
    }
}

impl std::ops::Index<usize> for DynamicLightList<'_> {
    type Output = MeshLight;

    fn index(&self, index: usize) -> &MeshLight {
        let script = &self.script.0;
        script.get(index).unwrap_or_else(|| &self.glows.0[index - script.len()])
    }
}

pub(crate) fn dynamic_lights(
    mut commands: Commands,
    frame: DynamicLightList,
    shown: Res<ShownScene>,
    mut existing: Query<(Entity, Option<&mut PointLight>, Option<&mut SpotLight>, &mut Transform), With<DynamicLight>>,
    mut by_kind: Local<LightsByKind>,
) {
    let level_brightness = shown.lighting.as_ref().map_or(1.0, |environment| environment.level_brightness);
    let lights = frame.iter().enumerate().filter(|(_, light)| light.dynamic).filter_map(|(index, light)| Some((index, gpu_light(light, level_brightness)?)));
    // Per kind: the query yields the entities archetype by archetype, not in the frame's order, so one cursor over all
    // lights would pair a point light with a spot entity whenever the kinds interleave.
    let LightsByKind { points, spots } = &mut *by_kind;
    for (index, gpu) in lights {
        let list = if matches!(gpu, BevyLight::Point(_)) { &mut *points } else { &mut *spots };
        list.push((index, gpu));
    }
    let (mut points, mut spots) = (points.drain(..), spots.drain(..));
    for (entity, point, spot, mut transform) in &mut existing {
        let next = if point.is_some() { points.next() } else { spots.next() };
        let Some((index, gpu)) = next else {
            commands.entity(entity).despawn();
            continue;
        };
        let light = &frame[index];
        transform.set_if_neq(place(light, &gpu, shown.origin));
        match (gpu, point, spot) {
            (BevyLight::Point(new), Some(mut point), _) => {
                if (point.color, point.range, point.radius) != (new.color, new.range, new.radius) {
                    *point = new;
                }
            }
            (BevyLight::Spot(new), _, Some(mut spot))
                if (spot.color, spot.range, spot.outer_angle) != (new.color, new.range, new.outer_angle) => {
                    *spot = new;
                }
            _ => {}
        }
    }
    for (index, gpu) in points.chain(spots) {
        let at = place(&frame[index], &gpu, shown.origin);
        spawn(&mut commands, gpu, at);
    }
}

#[derive(Default)]
pub(crate) struct LightsByKind {
    points: Vec<(usize, BevyLight)>,
    spots: Vec<(usize, BevyLight)>,
}

fn spawn(commands: &mut Commands, gpu: BevyLight, place: Transform) {
    let layers = RenderLayers::from_layers(&[0, SKY_LAYER]).union(&RenderLayers::from_layers(&OVERLAY_LAYERS));
    match gpu {
        BevyLight::Point(point) => commands.spawn((DynamicLight, point, place, layers)),
        BevyLight::Spot(spot) => commands.spawn((DynamicLight, spot, place, layers)),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_lights_carry_the_retail_colour_radius_and_effect() {
        let light = MeshLight { radius: 3.0, brightness: 255.0, dynamic: true, effect: p2_render_cpu::LightEffect(13), ..MeshLight::default() };
        let Some(BevyLight::Point(point)) = gpu_light(&light, 1.0) else { panic!("not a point light") };
        assert_eq!(point.range, 100.0);
        assert_eq!(point.radius, 13.0);
        let [r, g, b] = light.color(1.0);
        assert_eq!(point.color, Color::LinearRgba(LinearRgba::rgb(r, g, b)));
        assert!(!point.shadow_maps_enabled);
    }

    #[test]
    fn spot_effects_are_spot_lights_with_the_engine_cone_and_sunlight_is_left_out() {
        let light = |effect| MeshLight { effect, cone: 64, dynamic: true, ..MeshLight::default() };
        for effect in [LE_STATIC_SPOT, LE_SPOTLIGHT] {
            let Some(BevyLight::Spot(spot)) = gpu_light(&light(effect), 1.0) else { panic!("not a spot light") };
            assert_eq!(spot.inner_angle, 0.0);
            assert!((spot.outer_angle.cos() - 0.75).abs() < 1e-6);
        }
        assert!(gpu_light(&light(LE_SUNLIGHT), 1.0).is_none());
    }
}
