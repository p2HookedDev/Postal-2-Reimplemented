//! Dynamic light on terrain, re-lit per frame as retail when a `bDynamicLight` light touches a sector.
//! RGB is retail's; alpha is the modern sun mask (1 where the map's sun reached the vertex, else 0).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use p2_render_cpu::LE_SUNLIGHT;
use bevy::asset::{Assets, Handle};
use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::prelude::{Local, Res, ResMut};
use bevy::mesh::Mesh;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use p2_math::Vector;
use p2_render_cpu::MeshLight;

use crate::display::Settings;
use crate::level_collision::LevelCollision;
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

const TRACE_LIFT: f32 = 4.0;

pub fn vertex_sun_mask(positions: &[[f32; 3]], normals: &[[f32; 3]], sun: &MeshLight, mut visible: impl FnMut(Vector, Vector) -> bool) -> Vec<f32> {
    positions
        .iter()
        .zip(normals)
        .map(|(&position, &normal)| {
            let (position, normal) = (Vector::from(position), Vector::from(normal));
            let reached = sun.sample_intensity(position, normal) > 0.0 && visible(position, normal);
            if reached { 1.0 } else { 0.0 }
        })
        .collect()
}

fn masked_colors(baked: &[[f32; 4]], mask: Option<&[f32]>, out: &mut Vec<[f32; 4]>) {
    out.clear();
    out.extend(baked.iter().enumerate().map(|(index, &[r, g, b, _])| [r, g, b, mask.and_then(|mask| mask.get(index)).copied().unwrap_or(0.0)]));
}

const MASK_SWAPS_PER_FRAME: usize = 16;

#[derive(Default, Clone)]
struct SectorLight {
    lit: bool,
    mask: Option<Vec<f32>>,
    masked: bool,
}

#[derive(Default)]
pub(crate) struct TerrainLight {
    lights: Vec<MeshLight>,
    sectors: Vec<SectorLight>,
    base: Vec<[f32; 4]>,
    buffer: Vec<[f32; 4]>,
    job: Option<Task<std::thread::Result<Vec<Vec<f32>>>>>,
}

pub(crate) fn light_terrain(
    frame: Res<crate::render::bsp_light::FrameLights>,
    shown: Res<ShownScene>,
    collision: Res<LevelCollision>,
    settings: Option<Res<Settings>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut state: Local<TerrainLight>,
) {
    let Some(environment) = shown.lighting.as_ref() else { return };
    let TerrainLight { lights: last_lights, sectors, base, buffer, job } = &mut *state;
    let lights: &[MeshLight] = &frame.0;
    let sun = environment
        .sun
        .as_ref()
        .map(|(sun, _)| *sun)
        .filter(|sun| !sun.dynamic && settings.as_deref().is_some_and(Settings::modern_shadows));
    let want = sun.is_some();
    let reset = shown.is_changed() || collision.is_changed();
    if reset {
        last_lights.clear();
        sectors.clear();
        *job = None;
    }
    sectors.resize(environment.terrain.len(), SectorLight::default());
    if job.as_ref().is_some_and(Task::is_finished)
        && let Some(task) = job.take()
    {
        let mut masks = block_on(task)
            .unwrap_or_else(|_| {
                bevy::log::warn!("terrain sun mask failed: the terrain's alpha stays 0, no sun mask");
                Vec::new()
            })
            .into_iter();
        for kept in sectors.iter_mut() {
            kept.mask = Some(masks.next().unwrap_or_default());
        }
    }
    if let Some(sun) = sun
        && job.is_none()
        && sectors.iter().any(|kept| kept.mask.is_none())
        && let Some(level) = collision.0.clone()
    {
        let environment = Arc::clone(environment);
        *job = Some(AsyncComputeTaskPool::get().spawn(async move {
            catch_unwind(AssertUnwindSafe(|| {
                let reaches = |position, normal| crate::render::sun_mask::sun_reaches(&level, sun.rotation.vector(), position, normal, TRACE_LIFT);
                environment.terrain.iter().map(|sector| vertex_sun_mask(&sector.positions, &sector.normals, &sun, reaches)).collect()
            }))
        }));
    }
    let relight = reset || last_lights.as_slice() != lights;
    if !relight && sectors.iter().all(|kept| kept.masked == want) {
        return;
    }
    if relight {
        last_lights.clear();
        last_lights.extend_from_slice(lights);
    }
    let lights: Vec<(&MeshLight, [f32; 3])> =
        lights.iter().filter(|light| light.dynamic).map(|light| (light, light.color(environment.level_brightness))).collect();
    let mut swaps = 0;
    for (sector, kept) in environment.terrain.iter().zip(sectors.iter_mut()) {
        let mut sector_masked = kept.masked;
        if sector_masked != want && swaps < MASK_SWAPS_PER_FRAME && (!want || kept.mask.is_some()) {
            sector_masked = want;
            swaps += 1;
        }
        let swapped = sector_masked != kept.masked;
        kept.masked = sector_masked;
        if !relight && !swapped {
            continue;
        }
        let reaching: Vec<(&MeshLight, [f32; 3])> =
            lights.iter().filter(|(light, _)| reaches_box(light, sector.min, sector.max)).copied().collect();
        if reaching.is_empty() && !kept.lit && !swapped {
            continue;
        }
        masked_colors(&sector.baked, kept.mask.as_deref().filter(|_| sector_masked), base);
        let colors = if reaching.is_empty() {
            &base[..]
        } else {
            lit_colors(&base[..], &sector.positions, &sector.normals, &reaching, buffer);
            &buffer[..]
        };
        for handle in &sector.meshes {
            if let Some(mut mesh) = meshes.get_mut(handle) {
                p2_bevy_assets::set_vertex_colors(&mut mesh, colors);
            }
        }
        kept.lit = !reaching.is_empty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level_collision::StaticCollision;

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

    #[test]
    fn the_sun_mask_is_one_only_where_the_sun_faces_and_reaches() {
        let sun = MeshLight { effect: LE_SUNLIGHT, rotation: p2_math::Rotator { pitch: -16384, yaw: 0, roll: 0 }, ..MeshLight::default() };
        let positions = [[0.0, 0.0, 0.0], [1000.0, 0.0, 0.0], [0.0, 1000.0, 0.0]];
        let normals = [[0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0]];
        let mask = vertex_sun_mask(&positions, &normals, &sun, |position, _| position.x < 500.0);
        assert_eq!(mask, [1.0, 0.0, 0.0]);
        let baked = [[0.75, 0.5, 0.25, 0.0]; 3];
        let mut out = Vec::new();
        masked_colors(&baked, Some(&mask), &mut out);
        assert_eq!(out, [[0.75, 0.5, 0.25, 1.0], [0.75, 0.5, 0.25, 0.0], [0.75, 0.5, 0.25, 0.0]]);
        masked_colors(&baked, None, &mut out);
        assert_eq!(out, baked);
    }

    #[test]
    fn the_sun_reaches_everywhere_without_collision() {
        let sun = MeshLight { effect: LE_SUNLIGHT, rotation: p2_math::Rotator { pitch: -16384, yaw: 0, roll: 0 }, ..MeshLight::default() };
        let empty = StaticCollision { model: p2_assets::Model::default(), terrains: Vec::new(), meshes: Vec::new() };
        assert!(crate::render::sun_mask::sun_reaches(&empty, sun.rotation.vector(), Vector::default(), [0.0, 0.0, 1.0].into(), TRACE_LIFT));
    }
}
