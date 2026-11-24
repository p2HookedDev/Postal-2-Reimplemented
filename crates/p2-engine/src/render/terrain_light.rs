//! Dynamic light on terrain, re-lit per frame as retail when a `bDynamicLight` light touches a sector. Modern graphics
//! light terrain per pixel instead and read no vertex light.

use p2_render_cpu::LE_SUNLIGHT;
use bevy::asset::{Assets, Handle};
use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::prelude::{Local, Res, ResMut};
use bevy::mesh::Mesh;
use p2_math::Vector;
use p2_render_cpu::MeshLight;

use crate::display::Settings;
use crate::shown_scene::ShownScene;

pub struct TerrainSector {
    pub meshes: Vec<Handle<Mesh>>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub baked: Vec<[f32; 4]>,
    pub min: Vector,
    pub max: Vector,
}

pub fn reaches_box(light: &MeshLight, min: Vector, max: Vector) -> bool {
    if light.effect == LE_SUNLIGHT {
        return true;
    }
    let distance_squared: f32 = (0..3).map(|i| (light.location[i] - light.location[i].clamp(min[i], max[i])).powi(2)).sum();
    distance_squared <= light.world_radius().powi(2)
}

pub fn lit_colors(baked: &[[f32; 4]], positions: &[[f32; 3]], normals: &[[f32; 3]], lights: &[(&MeshLight, [f32; 3])], out: &mut Vec<[f32; 4]>) {
    out.clear();
    out.extend(baked.iter().zip(positions).zip(normals).map(|((&[r, g, b, a], &position), &normal)| {
        let mut color = [r, g, b];
        for (light, light_color) in lights {
            let intensity = light.sample_intensity(position.into(), normal.into());
            if intensity > 0.0 {
                for (channel, add) in color.iter_mut().zip(light_color) {
                    *channel += add * 0.5 * intensity;
                }
            }
        }
        let [r, g, b] = color.map(|c| c.min(1.0));
        [r, g, b, a]
    }));
}

#[derive(Default)]
pub(crate) struct TerrainLight {
    lights: Vec<MeshLight>,
    lit: Vec<bool>,
    buffer: Vec<[f32; 4]>,
}

pub(crate) fn light_terrain(
    frame: Res<crate::render::bsp_light::FrameLights>,
    shown: Res<ShownScene>,
    settings: Option<Res<Settings>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut state: Local<TerrainLight>,
) {
    let Some(environment) = shown.lighting.as_ref() else { return };
    let TerrainLight { lights: last_lights, lit, buffer } = &mut *state;
    if shown.is_changed() {
        last_lights.clear();
        lit.clear();
    }
    let lights: &[MeshLight] = &frame.0;
    if settings.as_deref().is_some_and(Settings::modern_graphics) || last_lights.as_slice() == lights {
        return;
    }
    last_lights.clear();
    last_lights.extend_from_slice(lights);
    lit.resize(environment.terrain.len(), false);
    let lights: Vec<(&MeshLight, [f32; 3])> =
        lights.iter().filter(|light| light.dynamic).map(|light| (light, light.color(environment.level_brightness))).collect();
    for (sector, was_lit) in environment.terrain.iter().zip(lit.iter_mut()) {
        let reaching: Vec<(&MeshLight, [f32; 3])> =
            lights.iter().filter(|(light, _)| reaches_box(light, sector.min, sector.max)).copied().collect();
        if reaching.is_empty() && !*was_lit {
            continue;
        }
        let colors = if reaching.is_empty() {
            &sector.baked[..]
        } else {
            lit_colors(&sector.baked, &sector.positions, &sector.normals, &reaching, buffer);
            &buffer[..]
        };
        for handle in &sector.meshes {
            if let Some(mut mesh) = meshes.get_mut(handle) {
                crate::diagnostics::mesh_written(crate::diagnostics::MeshWrite::TerrainLight);
                p2_bevy_assets::set_vertex_colors(&mut mesh, colors);
            }
        }
        *was_lit = !reaching.is_empty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(location: Vector) -> MeshLight {
        MeshLight { location, radius: 3.0, brightness: 255.0, dynamic: true, ..MeshLight::default() }
    }

    #[test]
    fn a_light_reaches_boxes_within_its_radius() {
        let (min, max) = ([0.0; 3], [100.0, 100.0, 10.0]);
        assert!(reaches_box(&light([50.0, 50.0, 50.0].into()), min.into(), max.into()));
        assert!(reaches_box(&light([-99.0, 50.0, 0.0].into()), min.into(), max.into()));
        assert!(!reaches_box(&light([-101.0, 50.0, 0.0].into()), min.into(), max.into()));
        assert!(reaches_box(&MeshLight { effect: LE_SUNLIGHT, ..light([1e6; 3].into()) }, min.into(), max.into()));
    }

    #[test]
    fn lit_colors_add_the_light_over_the_baked_ones_clamped() {
        let baked = [[0.25, 0.5, 0.9, 1.0], [0.25, 0.5, 0.9, 1.0]];
        let positions = [[0.0, 0.0, 0.0], [1000.0, 0.0, 0.0]];
        let normals = [[0.0, 0.0, 1.0]; 2];
        let above = light([0.0, 0.0, 50.0].into());
        let mut out = Vec::new();
        lit_colors(&baked, &positions, &normals, &[(&above, [1.0; 3])], &mut out);
        let add = 0.5 * above.sample_intensity(positions[0].into(), normals[0].into());
        assert!(add > 0.0);
        assert_eq!(out[0], [0.25 + add, 0.5 + add, (0.9 + add).min(1.0), 1.0]);
        assert_eq!(out[1], baked[1]);
        lit_colors(&baked, &positions, &normals, &[], &mut out);
        assert_eq!(out, baked);
    }
}
