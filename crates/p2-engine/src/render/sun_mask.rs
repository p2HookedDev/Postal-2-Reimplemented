//! Modern sun mask: atlases keep retail's RGB and carry in alpha where the bake's sun reached each
//! lumel, so shaders darken only the baked sun. Traced off the main thread.

use std::sync::Arc;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::ecs::prelude::{Res, ResMut, Resource};
use bevy::image::Image;
use bevy::log::{info, warn};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use p2_assets::{LightMap, Model, poly_flags};
use p2_math::Vector;

use crate::display::Settings;
use crate::level_collision::{LevelCollision, StaticCollision};
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

pub fn lumel_positions(map: &LightMap) -> Option<impl Fn(usize, usize) -> Vector> {
    let m = &map.world_to_light_map;
    let uv = |[px, py, pz]: [f32; 3], w: f32| [px * m[0] + py * m[4] + pz * m[8] + w * m[12], px * m[1] + py * m[5] + pz * m[9] + w * m[13]];
    let [u0, v0] = uv(map.base, 1.0);
    let [ux, vx] = uv(map.axis_x, 0.0);
    let [uy, vy] = uv(map.axis_y, 0.0);
    let det = ux * vy - uy * vx;
    if det.abs() < 1e-12 {
        return None;
    }
    let (base, axis_x, axis_y) = (Vector::from(map.base), Vector::from(map.axis_x), Vector::from(map.axis_y));
    Some(move |x: usize, y: usize| {
        let (du, dv) = (x as f32 + 0.5 - u0, y as f32 + 0.5 - v0);
        let (a, b) = ((du * vy - uy * dv) / det, (ux * dv - du * vx) / det);
        base + axis_x * a + axis_y * b
    })
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
    lumels: usize,
    sunlit: usize,
    millis: u128,
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
        let position_of = lumel_positions(map).filter(|_| faces_sun(sun, normal));
        for y in 0..sy {
            for x in 0..sx {
                let (ax, ay) = (ox + x, oy + y);
                if ax >= width || ay >= height {
                    continue;
                }
                lumels += 1;
                let Some(position_of) = &position_of else { continue };
                let reached = sun_reaches(collision, sun, position_of(x, y), normal, LUMEL_LIFT);
                sunlit += usize::from(reached);
                if let Some(texel) = texels.get_mut(ay * width + ax) {
                    *texel = masked(*texel, reached);
                }
            }
        }
    }
    Traced { atlases, lumels, sunlit, millis: start.elapsed().as_millis() }
}

#[derive(Resource, Default)]
pub struct SunMask {
    job: Job,
}

#[derive(Default)]
enum Job {
    #[default]
    Idle,
    Running { key: usize, task: Task<std::thread::Result<Traced>> },
    Done { key: usize, collision: Arc<StaticCollision>, swapped: Vec<(usize, Handle<Image>)> },
}

impl SunMask {
    fn key(&self) -> Option<usize> {
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
    let wanted = level.0.as_ref().filter(lit).filter(|_| settings.modern_shadows()).zip(sun);
    let key = wanted.map(|(collision, _)| Arc::as_ptr(collision).addr());
    if mask.key() != key {
        match std::mem::take(&mut mask.job) {
            Job::Done { collision, swapped, .. } => restore(&collision, &swapped, &mut images),
            Job::Running { .. } | Job::Idle => {}
        }
    }
    let (Some((collision, sun)), Some(key)) = (wanted, key) else { return };
    match std::mem::take(&mut mask.job) {
        Job::Idle => {
            let level = collision.clone();
            let task = AsyncComputeTaskPool::get().spawn(async move { catch_unwind(AssertUnwindSafe(|| trace(&level, sun))) });
            mask.job = Job::Running { key, task };
        }
        Job::Running { key, task } if task.is_finished() => {
            let swapped = match block_on(task) {
                Ok(traced) => {
                    let Traced { atlases, lumels, sunlit, millis } = traced;
                    info!("modern sun mask: {lumels} lumels, {sunlit} sunlit, {millis} ms");
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
                    warn!("modern sun mask failed: the light maps keep alpha as retail's, no sun mask");
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

    #[test]
    fn a_lumel_lies_where_the_renderer_samples_it() {
        let mut matrix = [0.0; 16];
        matrix[0] = 0.5;
        matrix[5] = 0.5;
        matrix[12] = -50.0;
        matrix[13] = -25.0;
        let map = LightMap {
            texture: 0,
            surface: 0,
            zone: 0,
            offset: [0, 0],
            size: [4, 4],
            world_to_light_map: matrix,
            base: [100.0, 50.0, 16.0],
            axis_x: [2.0, 0.0, 0.0],
            axis_y: [0.0, 2.0, 0.0],
        };
        let at = lumel_positions(&map).expect("a lumel")(1, 2);
        assert!((at - Vector::new(103.0, 55.0, 16.0)).dot(at - Vector::new(103.0, 55.0, 16.0)) < 1e-6);
        let degenerate = LightMap { axis_y: [2.0, 0.0, 0.0], ..map };
        assert!(lumel_positions(&degenerate).is_none());
    }
}
