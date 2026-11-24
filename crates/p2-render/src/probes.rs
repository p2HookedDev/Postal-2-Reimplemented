//! Light that reaches a point second hand, baked once per map into a grid on the compute pool: how much sky it sees
//! each way, and what the sun and lamps bounce to it. `probes.wesl` reads it for what lights a room beyond the lamps.

mod bake;

use std::sync::Arc;

use bevy::app::{App, Plugin, Update};
use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::ecs::prelude::{Local, Res, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::{Image, ImageSampler};
use bevy::math::Vec3;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use p2_math::Vector;

use p2_host::display::{MODERN_INDIRECT, Settings};
use crate::level_collision::{LevelCollision, StaticCollision};
use crate::actor_light::LightEnvironment;
use crate::lamps::atlas::{WorldTriangles, world_triangles};
use crate::lamps::{MaterialAtlas, MaterialSlot, follow_atlas, map_lamps};
use crate::modern::sun_color;
use crate::view::ShownScene;
use bake::{ProbeBake, ProbeGrid, ProbeWorld, bake_probes};

fn probe_grid(level: u8) -> Option<ProbeGrid> {
    match level {
        0 => None,
        1 => Some(ProbeGrid { columns: 128, smallest_spacing: 256.0, most_probes: 187_500 }),
        _ => Some(ProbeGrid { columns: 256, smallest_spacing: 128.0, most_probes: 1_500_000 }),
    }
}

/// `spacing` 0 while no bake is shown. The shaders learn it through the ambient light's alpha.
#[derive(Resource, Default)]
pub struct ProbeAtlas {
    image: Option<Handle<Image>>,
    pub(crate) spacing: f32,
}

impl MaterialAtlas for ProbeAtlas {
    const SLOT: MaterialSlot = MaterialSlot::MetallicRoughness;

    fn wanted(&self) -> Option<Handle<Image>> {
        // The shaders divide by the spacing: no atlas is bound without one.
        self.image.clone().filter(|_| self.spacing > 0.0)
    }
}

#[derive(Default)]
pub(crate) struct ProbeJob {
    key: Option<(usize, usize, i32)>,
    task: Option<Task<Option<ProbeBake>>>,
    ready: Option<Option<ProbeBake>>,
}

/// Keyed by the grid's columns.
pub(crate) type ProbePrebake = crate::lamps::Prebaked<i32, Option<ProbeBake>>;

pub(crate) fn prebake(environment: &LightEnvironment, collision: &StaticCollision, world: &WorldTriangles, origin: Vec3, level: u8) -> ProbePrebake {
    probe_grid(level).map_or_else(ProbePrebake::default, |grid| ProbePrebake::new(grid.columns, bake_map(environment, collision, world, origin, grid)))
}

fn atlas_image(grid: ProbeGrid, pixels: Vec<u8>) -> Image {
    let (width, height) = grid.atlas_size();
    let size = Extent3d { width: width as u32, height: height as u32, depth_or_array_layers: 1 };
    let mut image = Image::new(size, TextureDimension::D2, pixels, TextureFormat::Rgba8Unorm, RenderAssetUsages::RENDER_WORLD);
    image.sampler = ImageSampler::linear();
    image
}

fn bake_map(environment: &LightEnvironment, collision: &StaticCollision, world: &WorldTriangles, origin: Vec3, grid: ProbeGrid) -> Option<ProbeBake> {
    let lamps = map_lamps(environment);
    let sun = environment.sun.as_ref().map(|(_, hardware)| {
        (-Vec3::from(p2_bevy_assets::to_bevy(hardware.direction)).normalize_or_zero(), sun_color(environment.sun_set()))
    });
    let world = ProbeWorld {
        triangles: &world.triangles,
        albedos: &world.albedos,
        terrain_from: world.terrain_from,
        lamps: &lamps,
        level_brightness: environment.level_brightness,
        origin,
        sun,
        model: &collision.model,
        zone_ambients: &environment.zone_ambients,
    };
    let solid = |point: Vec3| {
        let at = Vector::from(p2_bevy_assets::to_bevy((point - origin).to_array()));
        p2_collision::line_check(&collision.model, at, Vector::new(at.x, at.y, at.z + 1.0)).is_some_and(|hit| hit.time == 0.0)
    };
    bake_probes(&world, grid, solid)
}

pub(crate) fn follow_probes(settings: Res<Settings>, shown: Option<Res<ShownScene>>, level: Option<Res<LevelCollision>>, mut atlas: ResMut<ProbeAtlas>, mut job: Local<ProbeJob>, mut images: ResMut<Assets<Image>>) {
    let grid = probe_grid(settings.modern_level(MODERN_INDIRECT)).filter(|_| settings.modern_graphics());
    let environment = shown.as_deref().and_then(|shown| shown.lighting.clone()).filter(|_| grid.is_some());
    let collision = level.as_ref().and_then(|level| level.0.clone());
    let key = environment.as_ref().zip(collision.as_ref()).zip(grid).map(|((environment, collision), grid)| (Arc::as_ptr(environment).addr(), Arc::as_ptr(collision).addr(), grid.columns));
    if job.key != key {
        job.key = key;
        job.task = None;
        job.ready = None;
        if atlas.image.is_some() || atlas.spacing != 0.0 {
            *atlas = ProbeAtlas::default();
        }
        if let (Some(environment), Some(collision), Some(shown), Some(grid)) = (environment, collision, shown.as_deref(), grid) {
            let origin = shown.origin;
            job.ready = shown.light_bakes.as_ref().and_then(|bakes| bakes.probes.take(&grid.columns));
            if job.ready.is_none() {
                job.task = Some(AsyncComputeTaskPool::get().spawn(async move { bake_map(&environment, &collision, &world_triangles(&collision, origin), origin, grid) }));
            }
        }
    }
    if job.ready.is_none() && !job.task.as_ref().is_some_and(Task::is_finished) {
        return;
    }
    let Some(grid) = grid else { return };
    let Some(Some(bake)) = job.ready.take().or_else(|| job.task.take().map(block_on)) else { return };
    *atlas = ProbeAtlas { image: Some(images.add(atlas_image(grid, bake.pixels))), spacing: bake.spacing };
}

pub struct ProbesPlugin;

impl Plugin for ProbesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ProbeAtlas>();
        app.add_systems(Update, follow_probes.in_set(super::RenderFollowSystems));
        follow_atlas::<ProbeAtlas>(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_indirect_level_sets_the_grid_and_its_atlas() {
        assert_eq!(probe_grid(0), None);
        let sizes = [1, 2, 9].map(|level| probe_grid(level).map(ProbeGrid::atlas_size));
        assert_eq!(sizes, [Some((3120, 780)), Some((6192, 2838)), Some((6192, 2838))]);
        let fine = probe_grid(2).expect("a grid");
        let wide = probe_grid(1).expect("a grid");
        assert_eq!(wide.smallest_spacing, 2.0 * fine.smallest_spacing);
        assert_eq!(wide.most_probes * 8, fine.most_probes);
    }
}
