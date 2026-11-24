//! The first-person weapon's light under the modern look. Its camera sees neither the sun, the fills nor the map's lamps,
//! so the light the world takes at the eye is picked here and `actor_light.wesl` lights the weapon as a world surface.

use bevy::math::{Vec3, Vec4};
use p2_assets::poly_flags::FAKE_BACKDROP;
use p2_bevy_assets::to_bevy;
use p2_collision::{Sides, terrain_line_check};
use p2_math::Vector;
use p2_render_cpu::{LE_SPOTLIGHT, LE_STATIC_SPOT, MAX_HARDWARE_LIGHTS, MeshLight};

use super::{HardwareLightUniform, LightEnvironment, SkinnedLighting};
use crate::level_collision::StaticCollision;
use crate::render::lamps::is_lamp;
use crate::render::map_lights::MapLights;
use crate::render::modern::linear_light;

/// Its visibility rides in the colour's alpha, the share of open sky above the eye in the attenuation's x.
const SUN_KIND: u32 = 2;
/// The attenuation holds the cone's cosine, 1 on a spot, and the source's radius.
const LAMP_KIND: u32 = 3;
const FILL_KIND: u32 = 4;

const SUN_TRACE: f32 = 200_000.0;
/// In world units around the eye, about a weapon's size: a shadow's edge dims it in steps as it passes.
const SUN_SPREAD: f32 = 24.0;
const LAMP_CHECKS: usize = 12;
/// A lamp's fitting often holds it inside its own collision, so the trace stops this short of it.
const LAMP_CLEARANCE: f32 = 16.0;
/// Matches `lamp_shadow.wesl`.
const LAMP_SOURCE_SHARE: f32 = 0.08;

/// A fake backdrop is the sky, however solid the level is behind it. Terrain and static meshes stop the ray before it.
fn reaches_sky(level: &StaticCollision, from: Vector, way: Vector) -> bool {
    let to = from + way * SUN_TRACE;
    let model = &level.model;
    let (reach, sky) = p2_collision::line_check_node(model, from, to).map_or((1.0, true), |(hit, node)| {
        let surface = node.and_then(|node| model.nodes.get(node)).and_then(|node| model.surfaces.get(node.surface));
        (hit.time, surface.is_some_and(|surface| surface.poly_flags & FAKE_BACKDROP != 0))
    });
    sky && !level.terrains.iter().any(|terrain| terrain_line_check(terrain, from, to, Sides::Both).is_some_and(|hit| hit.time < reach))
        && level.mesh_set.line_check_at(&level.meshes, from, to).is_none_or(|(hit, ..)| hit.time >= reach)
}

/// How much of the weapon a sky along `way` reaches, from rays around the eye.
pub(super) fn sun_share(level: &StaticCollision, eye: Vector, way: Vector) -> f32 {
    let along = Vec3::from(<[f32; 3]>::from(way)).normalize_or_zero();
    let side = along.any_orthonormal_vector() * SUN_SPREAD;
    let up = along.cross(side);
    let way = Vector::from(along.to_array());
    let diagonal = (side + up) * std::f32::consts::FRAC_1_SQRT_2;
    let across = (side - up) * std::f32::consts::FRAC_1_SQRT_2;
    let points = [Vec3::ZERO, side, -side, up, -up, diagonal, -diagonal, across, -across].map(|offset| eye + Vector::from(offset.to_array()));
    points.iter().filter(|&&point| reaches_sky(level, point, way)).count() as f32 / points.len() as f32
}

fn reaches_lamp(level: &StaticCollision, eye: Vector, lamp: Vector) -> bool {
    let to_lamp = lamp - eye;
    let distance = to_lamp.dot(to_lamp).sqrt();
    distance <= LAMP_CLEARANCE || level.first_hit(eye, eye + to_lamp * (1.0 - LAMP_CLEARANCE / distance)).is_none()
}

fn nearest_lamps<'a>(environment: &'a LightEnvironment, level: &StaticCollision, eye: Vector) -> Vec<&'a MeshLight> {
    let brightness = environment.level_brightness;
    let mut lamps: Vec<(f32, &MeshLight)> = environment
        .lights
        .values()
        .filter(|light| is_lamp(light))
        .filter_map(|light| {
            let to_light = light.location - eye;
            let toward = to_light / to_light.dot(to_light).sqrt().max(1.0);
            let strength = light.sample_intensity(eye, toward) * light.color(brightness).iter().sum::<f32>();
            (strength > 0.0).then_some((strength, light))
        })
        .collect();
    lamps.sort_by(|a, b| b.0.total_cmp(&a.0));
    lamps.into_iter().take(LAMP_CHECKS).filter(|(_, lamp)| reaches_lamp(level, eye, lamp.location)).map(|(_, lamp)| lamp).collect()
}

/// Colours in linear light and the ambient in display light, as the world's lights carry them.
pub fn view_model_lighting(environment: &LightEnvironment, map: &MapLights, level: &StaticCollision, origin: Vec3, eye: Vector) -> SkinnedLighting {
    let zone = p2_collision::point_zone(&level.model, eye);
    let ambient = environment.zone_ambients.get(usize::from(zone)).copied().unwrap_or_default();
    let brightness = environment.level_brightness;
    let sun = map.sun.as_ref().map(|(_, hardware)| {
        let [r, g, b] = map.sun_color;
        HardwareLightUniform {
            direction: Vec3::from(to_bevy(hardware.direction)),
            kind: SUN_KIND,
            color: Vec4::new(r, g, b, sun_share(level, eye, -hardware.direction)),
            attenuation: Vec4::new(sun_share(level, eye, Vector::new(0.0, 0.0, 1.0)), 0.0, 0.0, 0.0),
            ..Default::default()
        }
    });
    let fills = map.fills.iter().map(|&(direction, [r, g, b])| HardwareLightUniform {
        direction,
        kind: FILL_KIND,
        color: Vec4::new(r, g, b, 0.0),
        ..Default::default()
    });
    let lamps = nearest_lamps(environment, level, eye).into_iter().map(|lamp| {
        let [r, g, b] = linear_light(lamp.color(brightness));
        let spot = matches!(lamp.effect, LE_SPOTLIGHT | LE_STATIC_SPOT);
        HardwareLightUniform {
            position: (Vec3::from(to_bevy(lamp.location)) + origin).extend(lamp.world_radius()),
            direction: Vec3::from(to_bevy(lamp.rotation.vector())),
            kind: LAMP_KIND,
            color: Vec4::new(r, g, b, 0.0),
            attenuation: Vec4::new(lamp.cone_sine(), f32::from(u8::from(spot)), (lamp.world_radius() * LAMP_SOURCE_SHARE).max(1.0), 0.0),
        }
    });
    let mut lights = [HardwareLightUniform::default(); MAX_HARDWARE_LIGHTS];
    let mut count = 0;
    for (slot, light) in lights.iter_mut().zip(sun.into_iter().chain(fills).chain(lamps)) {
        *slot = light;
        count += 1;
    }
    SkinnedLighting { ambient: Vec3::from(ambient), count, lights }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level_collision::LevelAlbedos;
    use p2_assets::Model;
    use p2_collision::{BspHulls, MeshCollision, MeshSet};
    use p2_math::Rotator;
    use p2_render_cpu::LE_SUNLIGHT;
    use rustc_hash::FxHashMap;
    use std::sync::Arc;

    const DOWN: Rotator = Rotator { pitch: -16384, yaw: 0, roll: 0 };

    fn level(meshes: Vec<MeshCollision>) -> StaticCollision {
        StaticCollision {
            model: Model::default(),
            hulls: BspHulls::default(),
            terrains: Vec::new(),
            mesh_set: MeshSet::new(&meshes),
            meshes,
            lamp_casters: Vec::new(),
            lamp_masked: Vec::new(),
            albedos: LevelAlbedos::default(),
        }
    }

    fn roof() -> MeshCollision {
        let corners = [[-1000.0, -1000.0, 100.0], [1000.0, -1000.0, 100.0], [1000.0, 1000.0, 100.0], [-1000.0, 1000.0, 100.0]];
        let [a, b, c, d] = corners;
        MeshCollision::placed([[a, b, c], [a, c, d], [c, b, a], [d, c, a]], Vector::ZERO, Rotator::ZERO, Vector::new(1.0, 1.0, 1.0), Vector::ZERO)
    }

    fn lamp(location: Vector) -> MeshLight {
        MeshLight { location, radius: 8.0, brightness: 255.0, ..MeshLight::default() }
    }

    fn environment(extra: Vec<MeshLight>) -> Arc<LightEnvironment> {
        let sun = MeshLight { effect: LE_SUNLIGHT, rotation: DOWN, brightness: 128.0, ..MeshLight::default() };
        let lights = FxHashMap::from_iter([sun].into_iter().chain(extra).enumerate().map(|(key, light)| (key as i32, light)));
        Arc::new(LightEnvironment::new(lights, vec![[0.1, 0.2, 0.3]], 1.0))
    }

    fn kinds(lighting: &SkinnedLighting) -> Vec<u32> {
        lighting.lights[..lighting.count as usize].iter().map(|light| light.kind).collect()
    }

    #[test]
    fn the_weapon_takes_the_sun_its_fills_and_the_near_lamps() {
        let fill = MeshLight { effect: LE_SUNLIGHT, rotation: Rotator { pitch: -8192, yaw: 0, roll: 0 }, brightness: 64.0, ..MeshLight::default() };
        let special = MeshLight { special_lit: true, ..lamp(Vector::new(120.0, 0.0, 0.0)) };
        let environment = environment(vec![fill, lamp(Vector::new(100.0, 0.0, 0.0)), lamp(Vector::new(5000.0, 0.0, 0.0)), special]);
        let lighting = view_model_lighting(&environment, &MapLights::new(&environment), &level(Vec::new()), Vec3::ZERO, Vector::ZERO);
        assert_eq!(kinds(&lighting), [SUN_KIND, FILL_KIND, LAMP_KIND]);
        assert_eq!(lighting.ambient, Vec3::new(0.1, 0.2, 0.3));
        let sun = lighting.lights[0];
        assert_eq!((sun.color.w, sun.attenuation.x), (1.0, 1.0));
        assert!(sun.color.x > 0.0 && sun.direction.y < -0.99);
        assert_eq!(lighting.lights[2].position.w, 225.0);
        assert_eq!(lighting.lights[2].attenuation.y, 0.0);
    }

    #[test]
    fn a_roof_takes_the_sun_the_sky_and_the_lamps_above_it() {
        let environment = environment(vec![lamp(Vector::new(0.0, 0.0, 200.0)), lamp(Vector::new(0.0, 0.0, 50.0))]);
        let lighting = view_model_lighting(&environment, &MapLights::new(&environment), &level(vec![roof()]), Vec3::ZERO, Vector::ZERO);
        assert_eq!(kinds(&lighting), [SUN_KIND, LAMP_KIND]);
        assert_eq!((lighting.lights[0].color.w, lighting.lights[0].attenuation.x), (0.0, 0.0));
        assert_eq!(lighting.lights[1].position.y, 50.0);
    }

    #[test]
    fn a_spot_lamp_carries_its_cone() {
        let spot = MeshLight { effect: LE_SPOTLIGHT, cone: 64, rotation: DOWN, ..lamp(Vector::new(0.0, 0.0, 50.0)) };
        let environment = environment(vec![spot]);
        let lighting = view_model_lighting(&environment, &MapLights::new(&environment), &level(Vec::new()), Vec3::ZERO, Vector::ZERO);
        let lamp = lighting.lights[1];
        assert_eq!((lamp.attenuation.x, lamp.attenuation.y), (0.75, 1.0));
        assert!(lamp.direction.y < -0.99);
    }

    #[test]
    fn an_open_level_lets_all_the_sun_through() {
        assert_eq!(sun_share(&level(Vec::new()), Vector::ZERO, Vector::new(0.3, 0.2, 1.0)), 1.0);
    }
}
