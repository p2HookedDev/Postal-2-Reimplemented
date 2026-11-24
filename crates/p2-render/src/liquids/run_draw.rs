//! Runs drawn on their walls: each run is a decal facing out of its wall over its trail, and a row of the run atlas
//! holds its points in the decal's uv with their radii and wetness. The world's surfaces read it in `run_at`.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{AssetId, Assets, Handle, RenderAssetUsages};
use bevy::ecs::prelude::{Commands, DetectChanges, Entity, Res, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::ClusteredDecal;
use bevy::math::{Mat3, Quat, Vec3};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{Extent3d, Origin3d, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::renderer::RenderQueue;
use bevy::render::texture::GpuImage;
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::transform::components::Transform;
use p2_bevy_assets::to_bevy;

use super::runs::{RUN_POINTS, Run};
use super::world::{LiquidRuns, step_liquids};
use crate::frame_facts::FrameFacts;

/// Rows of the run atlas: the most runs drawn.
pub const RUN_ROWS: usize = 64;
/// A liquid decal (`layer.rs`) with this bit is a run: its row in bits 8 to 13, its kind in 22 to 25, its point count
/// in 26 to 30.
const RUN_TAG: u32 = 0x8000_0000 | 0x10_0000 | 0xf_0000 | 7 | 0x20_0000;
/// Unreal units the decal's box reaches past the trail, and its depth either side of the wall.
const MARGIN: f32 = 3.0;
const DEPTH: f32 = 6.0;

pub struct RunDrawPlugin;

impl Plugin for RunDrawPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        app.add_plugins(ExtractResourcePlugin::<RunRows>::default()).init_resource::<RunRows>().init_resource::<RunDecals>().add_systems(PostUpdate, draw_runs.after(step_liquids));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.add_systems(Render, upload_runs.in_set(RenderSystems::PrepareResources));
    }
}

#[derive(Resource, Default)]
struct RunDecals {
    atlas: Option<Handle<Image>>,
    decals: Vec<Entity>,
}

/// The run atlas's texels as half floats, each row a run.
#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub struct RunRows {
    image: Option<AssetId<Image>>,
    texels: Vec<u8>,
}

fn new_run_atlas(images: &mut Assets<Image>) -> Handle<Image> {
    let size = Extent3d { width: RUN_POINTS as u32, height: RUN_ROWS as u32, depth_or_array_layers: 1 };
    let mut image = Image::new_uninit(size, TextureDimension::D2, TextureFormat::Rgba16Float, RenderAssetUsages::RENDER_WORLD);
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
    images.add(image)
}

fn half(value: f32) -> [u8; 2] {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    let half = match exponent {
        ..=0 => sign,
        31.. => sign | 0x7bff,
        _ => sign | ((exponent as u16) << 10) | ((mantissa >> 13) as u16),
    };
    half.to_le_bytes()
}

/// A run's decal over its trail, facing out of its wall, and each point's (u, v, radius, wetness).
pub fn run_box(run: &Run, origin: Vec3) -> (Transform, Vec<[f32; 4]>) {
    let bevy = |v: p2_math::Vector| Vec3::from(to_bevy(v));
    let out = bevy(run.normal).normalize_or_zero();
    let up = (Vec3::Y - out * Vec3::Y.dot(out)).normalize_or(Vec3::Z);
    let side = up.cross(out);
    let places: Vec<Vec3> = run.points.iter().map(|&(at, _)| bevy(at) + origin).collect();
    let reach = run.points.iter().map(|&(_, radius)| radius).fold(0.0, f32::max) + MARGIN;
    let (low, high) = places.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(low, high), &at| {
        let local = Vec3::new(at.dot(side), at.dot(up), at.dot(out));
        (low.min(local), high.max(local))
    });
    let (low, high) = (low - Vec3::new(reach, reach, 0.0), high + Vec3::new(reach, reach, 0.0));
    let size = Vec3::new(high.x - low.x, high.y - low.y, DEPTH);
    let middle = (low + high) * 0.5;
    let center = side * middle.x + up * middle.y + out * middle.z;
    let transform = Transform { translation: center, rotation: Quat::from_mat3(&Mat3::from_cols(side, up, out)), scale: size };
    let points = places
        .iter()
        .zip(&run.points)
        .map(|(&at, &(_, radius))| {
            let local = Vec3::new((at - center).dot(side) / size.x, (at - center).dot(up) / size.y, 0.0);
            [local.x + 0.5, 0.5 - local.y, radius, run.wet]
        })
        .collect();
    (transform, points)
}

fn draw_runs(mut commands: Commands, runs: Res<LiquidRuns>, facts: Res<FrameFacts>, mut decals: ResMut<RunDecals>, mut images: ResMut<Assets<Image>>, mut rows: ResMut<RunRows>) {
    if !runs.is_changed() && decals.decals.len() == runs.0.list.len().min(RUN_ROWS) {
        return;
    }
    let atlas = decals.atlas.get_or_insert_with(|| new_run_atlas(&mut images)).clone();
    let drawn = runs.0.list.len().min(RUN_ROWS);
    while decals.decals.len() > drawn {
        if let Some(decal) = decals.decals.pop() {
            commands.entity(decal).despawn();
        }
    }
    let mut texels = vec![0_u8; RUN_POINTS * RUN_ROWS * 8];
    for (row, run) in runs.0.list.iter().take(RUN_ROWS).enumerate() {
        let (transform, points) = run_box(run, facts.origin);
        for (k, point) in points.iter().enumerate() {
            let at = (row * RUN_POINTS + k) * 8;
            for (c, value) in point.iter().enumerate() {
                texels[at + 2 * c..at + 2 * c + 2].copy_from_slice(&half(*value));
            }
        }
        let tag = RUN_TAG | (row as u32) << 8 | u32::from(run.kind as u8) << 22 | (points.len() as u32) << 26;
        let decal = ClusteredDecal { base_color_texture: Some(atlas.clone()), tag, ..Default::default() };
        match decals.decals.get(row) {
            Some(&entity) => {
                commands.entity(entity).insert((decal, transform));
            }
            None => {
                let entity = commands.spawn((decal, transform)).id();
                decals.decals.push(entity);
            }
        }
    }
    *rows = RunRows { image: Some(atlas.id()), texels };
}

fn upload_runs(rows: Option<Res<RunRows>>, images: Res<RenderAssets<GpuImage>>, queue: Res<RenderQueue>) {
    let Some(rows) = rows.filter(DetectChanges::is_changed) else { return };
    let Some(image) = rows.image.and_then(|id| images.get(id)) else { return };
    queue.write_texture(
        TexelCopyTextureInfo { texture: &image.texture, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
        &rows.texels,
        TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(RUN_POINTS as u32 * 8), rows_per_image: None },
        Extent3d { width: RUN_POINTS as u32, height: RUN_ROWS as u32, depth_or_array_layers: 1 },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::liquids::runs::{Runs, Surfaces, WallHit};
    use crate::shaders::shader_const;
    use p2_math::Vector;
    use p2_vm::FluidType;

    struct Wall;

    impl Surfaces for Wall {
        fn hit(&self, from: Vector, to: Vector) -> Option<(Vector, Vector)> {
            (from.y < 0.0 && to.y >= 0.0).then(|| (from + (to - from) * (-from.y / (to.y - from.y)), Vector::new(0.0, -1.0, 0.0)))
        }
    }

    #[test]
    fn a_run_s_points_lie_in_its_box_and_halves_round_trip() {
        let mut runs = Runs::default();
        runs.splat(WallHit { at: Vector::new(10.0, 0.0, 100.0), normal: Vector::new(0.0, -1.0, 0.0), kind: FluidType::Blood }, 30.0, 0.0, 4);
        let mut pours = Vec::new();
        for _ in 0..30 {
            runs.step(1.0 / 30.0, &Wall, 0.0, &mut pours);
        }
        let (transform, points) = run_box(&runs.list[0], Vec3::new(5.0, -3.0, 2.0));
        assert!(points.len() > 1);
        assert!(points.iter().all(|&[u, v, ..]| (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v)), "{points:?}");
        assert!(points[points.len() - 1][1] > points[0][1], "the head lies below the start: v grows downward");
        assert!((transform.rotation * Vec3::Z).dot(Vec3::new(0.0, 0.0, -1.0)) > 0.99, "the box faces out of the wall: Unreal -y is Bevy -z");
        assert_eq!(half(1.0), [0x00, 0x3c]);
        assert_eq!(half(-2.5), [0x00, 0xc1]);
        assert_eq!(shader_const(include_str!("../projector.wesl"), "RUN_POINTS"), RUN_POINTS as f64);
    }
}
