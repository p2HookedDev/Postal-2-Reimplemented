//! Modern light maps, off the main thread. With the relit world on, atlases come from our own bake. Otherwise they keep
//! retail's RGB and carry in alpha where the bake's sun reached each lumel. Either way shaders darken only the baked sun.

use std::sync::Arc;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::ecs::prelude::{Res, ResMut, Resource};
use bevy::image::Image;
use bevy::log::{info, warn};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use p2_assets::{Model, poly_flags};
use p2_math::Vector;

use crate::display::Settings;
use crate::game_dir::config_folder;
use crate::level_collision::{LevelCollision, StaticCollision};
use crate::render::actor_light::LightEnvironment;
use crate::shown_scene::ShownScene;

const LUMEL_LIFT: f32 = 2.0;

const SUN_DISTANCE: f32 = 65536.0;

pub fn faces_sun(direction: Vector, normal: Vector) -> bool {
    direction.dot(normal) < 0.0
}

pub fn masked(lumel: [u8; 4], reached: bool) -> [u8; 4] {
    let [r, g, b, _] = lumel;
    [r, g, b, if reached { 255 } else { 0 }]
}

pub(crate) fn sun_reaches(collision: &StaticCollision, direction: Vector, position: Vector, normal: Vector, lift: f32) -> bool {
    let start = position + normal * lift;
    let far = start - direction * SUN_DISTANCE;
    let end = match p2_collision::line_check_node(&collision.model, start, far) {
        None => far,
        Some((hit, Some(node))) if sky(&collision.model, node) => hit.location,
        Some(_) => return false,
    };
    p2_collision::mesh_line_check(&collision.meshes, start, end).is_none()
        && collision.terrains.iter().all(|terrain| p2_collision::terrain_line_check(terrain, start, end).is_none())
}

fn sky(model: &Model, node: usize) -> bool {
    model.nodes.get(node).and_then(|node| model.surfaces.get(node.surface)).is_some_and(|surface| surface.poly_flags & poly_flags::FAKE_BACKDROP != 0)
}

pub struct Traced {
    atlases: Vec<Option<(u32, u32, Vec<[u8; 4]>)>>,
    report: String,
}

fn trace(collision: &StaticCollision, sun: Vector) -> Traced {
    let start = Instant::now();
    let model = &collision.model;
    let mut atlases: Vec<_> =
        model.light_map_textures.iter().map(|atlas| atlas.texture().and_then(|texture| p2_assets::texture_rgba(&texture, None))).collect();
    for (_, _, texels) in atlases.iter_mut().flatten() {
        for texel in texels.iter_mut() {
            *texel = masked(*texel, false);
        }
    }
    let mut normals = vec![None; model.light_maps.len()];
    for node in &model.nodes {
        if let Some(slot) = node.light_map.and_then(|map| normals.get_mut(map)) {
            let [x, y, z, _] = node.plane;
            *slot = Some(Vector::new(x, y, z));
        }
    }
    let (mut lumels, mut sunlit) = (0, 0);
    for (map, normal) in model.light_maps.iter().zip(normals) {
        let Some(normal) = normal else { continue };
        let atlas = usize::try_from(map.texture).ok().and_then(|atlas| atlases.get_mut(atlas)).and_then(Option::as_mut);
        let Some((width, height, texels)) = atlas else { continue };
        let (width, height) = (*width as usize, *height as usize);
        let (Ok(ox), Ok(oy), Ok(sx), Ok(sy)) =
            (usize::try_from(map.offset[0]), usize::try_from(map.offset[1]), usize::try_from(map.size[0]), usize::try_from(map.size[1]))
        else {
            continue;
        };
        let position_of = p2_bake::lumel_positions(map).filter(|_| faces_sun(sun, normal));
        for y in 0..sy {
            for x in 0..sx {
                let (ax, ay) = (ox + x, oy + y);
                if ax >= width || ay >= height {
                    continue;
                }
                lumels += 1;
                let Some(position_of) = &position_of else { continue };
                let reached = sun_reaches(collision, sun, position_of(x as f32 + 0.5, y as f32 + 0.5), normal, LUMEL_LIFT);
                sunlit += usize::from(reached);
                if let Some(texel) = texels.get_mut(ay * width + ax) {
                    *texel = masked(*texel, reached);
                }
            }
        }
    }
    Traced { atlases, report: format!("modern sun mask: {lumels} lumels, {sunlit} sunlit, {} ms", start.elapsed().as_millis()) }
}

fn relit(collision: &StaticCollision, environment: &LightEnvironment) -> Traced {
    let mut lights: Vec<_> = environment.lights.values().filter(|light| !light.dynamic).copied().collect();
    // The lights come in hash order, which differs between runs; the cache key must not.
    lights.sort_by(|a, b| {
        <[f32; 3]>::from(a.location).partial_cmp(&<[f32; 3]>::from(b.location)).unwrap_or(std::cmp::Ordering::Equal)
    });
    let shadow_sun = environment.sun.as_ref().and_then(|(sun, _)| lights.iter().position(|light| light.effect == sun.effect && light.location == sun.location));
    let input = p2_bake::BakeInput {
        model: &collision.model,
        terrains: &collision.terrains,
        meshes: &collision.meshes,
        lights: &lights,
        shadow_sun,
        zone_ambients: &environment.zone_ambients,
        level_brightness: environment.level_brightness,
    };
    let file = config_folder().map(|folder| folder.join("bake").join(format!("{:016x}.bake", p2_bake::cache_key(&input))));
    let as_traced = |atlases: Vec<Option<p2_bake::Atlas>>, report: String| Traced {
        atlases: atlases.into_iter().map(|atlas| atlas.map(|atlas| (atlas.width, atlas.height, atlas.texels))).collect(),
        report,
    };
    if let Some(atlases) = file.as_deref().and_then(p2_bake::load) {
        return as_traced(atlases, "modern bake: loaded from the cache".to_owned());
    }
    let baked = p2_bake::bake(&input);
    let saved = file.as_deref().map(|file| p2_bake::save(file, &baked.atlases));
    if let Some(Err(error)) = saved {
        warn!("modern bake not cached: {error}");
    }
    let report = format!(
        "modern bake: {} lights, {} texels, {} ms; mean light retail {:.3}, ours direct {:.3}, ours final {:.3}",
        lights.len(),
        baked.texels,
        baked.millis,
        baked.mean_retail,
        baked.mean_direct,
        baked.mean_final
    );
    as_traced(baked.atlases, report)
}

#[derive(Resource, Default)]
pub struct SunMask {
    job: Job,
}

#[derive(Default)]
enum Job {
    #[default]
    Idle,
    Running { key: JobKey, task: Task<std::thread::Result<Traced>> },
    Done { key: JobKey, collision: Arc<StaticCollision>, swapped: Vec<(usize, Handle<Image>)> },
}

type JobKey = (usize, bool);

impl SunMask {
    fn key(&self) -> Option<JobKey> {
        match &self.job {
            Job::Idle => None,
            Job::Running { key, .. } | Job::Done { key, .. } => Some(*key),
        }
    }
}

fn restore(collision: &StaticCollision, swapped: &[(usize, Handle<Image>)], images: &mut Assets<Image>) {
    let model = &collision.model;
    for (index, handle) in swapped {
        let retail = model.light_map_textures.get(*index).and_then(|atlas| atlas.texture()).and_then(|texture| p2_bevy_assets::texture_to_image(&texture, None));
        if let (Some(mut retail), Some(mut slot)) = (retail, images.get_mut(handle)) {
            retail.sampler = slot.sampler.clone();
            *slot = retail;
        }
    }
}

pub fn sun_mask(
    settings: Res<Settings>,
    shown: Option<Res<ShownScene>>,
    level: Option<Res<LevelCollision>>,
    mut mask: ResMut<SunMask>,
    images: Option<ResMut<Assets<Image>>>,
) {
    let (Some(shown), Some(level), Some(mut images)) = (shown, level, images) else { return };
    let lit = |collision: &&Arc<StaticCollision>| !collision.model.light_map_textures.is_empty();
    let sun = shown
        .lighting
        .as_deref()
        .and_then(|e| e.sun.as_ref().map(|(_, hardware)| hardware.direction));
    let environment = shown.lighting.clone();
    let bake = settings.modern_bake() && environment.is_some();
    let wanted = level.0.as_ref().filter(lit).filter(|_| bake || (settings.modern_shadows() && sun.is_some()));
    let key = wanted.map(|collision| (Arc::as_ptr(collision).addr(), bake));
    if mask.key() != key {
        match std::mem::take(&mut mask.job) {
            Job::Done { collision, swapped, .. } => restore(&collision, &swapped, &mut images),
            Job::Running { .. } | Job::Idle => {}
        }
    }
    let (Some(collision), Some(key)) = (wanted, key) else { return };
    match std::mem::take(&mut mask.job) {
        Job::Idle => {
            let level = collision.clone();
            let task = AsyncComputeTaskPool::get().spawn(async move {
                catch_unwind(AssertUnwindSafe(|| match (environment.filter(|_| bake), sun) {
                    (Some(environment), _) => relit(&level, &environment),
                    (None, Some(sun)) => trace(&level, sun),
                    (None, None) => Traced { atlases: Vec::new(), report: String::new() },
                }))
            });
            mask.job = Job::Running { key, task };
        }
        Job::Running { key, task } if task.is_finished() => {
            let swapped = match block_on(task) {
                Ok(traced) => {
                    let Traced { atlases, report } = traced;
                    info!("{report}");
                    let mut swapped = Vec::new();
                    for (index, atlas) in atlases.into_iter().enumerate() {
                        let (Some((width, height, texels)), Some(Some(handle))) = (atlas, shown.light_maps.get(index)) else { continue };
                        let mut image = Image::new(
                            Extent3d { width, height, depth_or_array_layers: 1 },
                            TextureDimension::D2,
                            texels.into_iter().flatten().collect(),
                            TextureFormat::Rgba8UnormSrgb,
                            RenderAssetUsages::default(),
                        );
                        if let Some(mut slot) = images.get_mut(handle) {
                            image.sampler = slot.sampler.clone();
                            *slot = image;
                            swapped.push((index, handle.clone()));
                        }
                    }
                    swapped
                }
                Err(_) => {
                    warn!("modern light maps failed: the level keeps retail's");
                    Vec::new()
                }
            };
            mask.job = Job::Done { key, collision: collision.clone(), swapped };
        }
        job => mask.job = job,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sun() -> Vector {
        Vector::new(0.0, 0.0, -1.0)
    }

    #[test]
    fn only_surfaces_facing_the_sun_can_hold_it() {
        assert!(faces_sun(sun(), Vector::new(0.0, 0.0, 1.0)));
        assert!(!faces_sun(sun(), Vector::new(1.0, 0.0, 0.0)));
        assert!(!faces_sun(sun(), Vector::new(0.0, 0.0, -1.0)));
    }

    #[test]
    fn the_mask_goes_into_alpha_and_rgb_stay_retail() {
        assert_eq!(masked([200, 150, 120, 7], true), [200, 150, 120, 255]);
        assert_eq!(masked([200, 150, 120, 255], false), [200, 150, 120, 0]);
    }
}
