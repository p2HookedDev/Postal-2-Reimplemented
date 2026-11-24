//! Pools drawn as clustered decals pointing down, one per wet chunk with a texture slot of its own. The surface
//! shaders light a pool with the light already on the floor under it; only the rows that changed are uploaded.

use std::f32::consts::FRAC_PI_2;
use std::ops::Range;

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{AssetId, Assets, Handle, RenderAssetUsages};
use bevy::ecs::prelude::{Commands, DetectChanges, DetectChangesMut, Entity, Query, Res, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::ClusteredDecal;
use bevy::math::{Quat, Vec3};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{Extent3d, Origin3d, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect, TextureDimension, TextureFormat};
use bevy::render::renderer::RenderQueue;
use bevy::render::texture::GpuImage;
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::transform::components::Transform;
use p2_bevy_assets::to_bevy;
use p2_vm::FluidType;
use rustc_hash::FxHashMap;

use super::pools::{CELL, CHUNK, Cell, Chunk, FILM, PoolStore, locate};
use super::world::{LiquidWorld, step_liquids};
use crate::frame_facts::FrameFacts;

const TEXELS_PER_CELL: i32 = 2;
const SIDE: u32 = (CHUNK * TEXELS_PER_CELL) as u32;
const CHUNK_SIDE: f32 = CHUNK as f32 * CELL;
/// A projector decal whose frame buffer blending is 7, which no game projector uses: the surface shaders read it as
/// a pool. It takes every kind of surface.
const LIQUID_DECAL_TAG: u32 = 0x8000_0000 | 0x10_0000 | 0xf_0000 | 7;
/// Unreal units across which a pool's edge fades, and how far world-anchored noise moves it.
const EDGE_SOFTNESS: f32 = 1.5;
const EDGE_WARP: f32 = 3.0;
/// How far the decal's box reaches above and below the chunk's wet floors.
const BOX_MARGIN: f32 = 6.0;
/// Linear albedo of fresh and dried blood.
const BLOOD: [f32; 3] = [0.12, 0.001, 0.0006];
const DRIED_BLOOD: [f32; 3] = [0.05, 0.012, 0.005];

/// A kind's linear albedo and how much it covers the floor: clear liquids darken and tint it rather than paint it.
fn look(kind: FluidType) -> ([f32; 3], f32) {
    match kind {
        FluidType::Blood => (BLOOD, 0.95),
        FluidType::BloodyPuke => ([0.10, 0.03, 0.005], 0.9),
        FluidType::BloodyUrine => ([0.14, 0.02, 0.004], 0.7),
        FluidType::Urine => ([0.30, 0.22, 0.03], 0.45),
        FluidType::Puke => ([0.25, 0.22, 0.06], 0.85),
        FluidType::Gonorrhea => ([0.35, 0.35, 0.15], 0.8),
        FluidType::Gas => ([0.04, 0.035, 0.03], 0.4),
        FluidType::Napalm => ([0.06, 0.03, 0.01], 0.6),
    }
}

pub struct PoolDrawPlugin;

impl Plugin for PoolDrawPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        app.add_plugins(ExtractResourcePlugin::<PoolRows>::default())
            .init_resource::<PoolRows>()
            .init_resource::<PoolDecals>()
            .add_systems(PostUpdate, draw_pools.after(step_liquids));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.add_systems(Render, upload_pool_rows.in_set(RenderSystems::PrepareResources));
    }
}

struct Drawn {
    entity: Entity,
    image: Handle<Image>,
    /// The lowest and highest wet floor of the chunk.
    span: [f32; 2],
}

#[derive(Resource, Default)]
struct PoolDecals {
    drawn: FxHashMap<(i32, i32, i32), Drawn>,
    spare: Vec<Handle<Image>>,
}

#[derive(Clone)]
struct RowUpload {
    image: AssetId<Image>,
    first: u32,
    texels: Vec<u8>,
}

/// Rows of chunk textures that changed this frame.
#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub struct PoolRows {
    rows: Vec<RowUpload>,
}

fn hash(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343) ^ (y as u32).wrapping_mul(0xD816_3841);
    h ^= h >> 13;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 16;
    (h & 0xFFFF) as f32 / 65535.0
}

fn noise(x: f32, y: f32) -> f32 {
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let ease = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (ease(x - x.floor()), ease(y - y.floor()));
    let row = |j: i32| hash(ix, iy + j) * (1.0 - sx) + hash(ix + 1, iy + j) * sx;
    row(0) * (1.0 - sy) + row(1) * sy
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t)
}

fn cell_at(pools: &PoolStore, storey: i32, at: [i32; 2]) -> Cell {
    let (key, index) = locate(at, storey);
    pools.chunks.get(&key).map_or_else(Cell::default, |chunk| chunk.cells[index])
}

/// One texel at world texel `[x, y]`: the depth between cell middles, its edge from the depth's signed distance to the
/// film's half, moved by world-anchored noise.
fn texel(pools: &PoolStore, storey: i32, [x, y]: [i32; 2]) -> [u8; 4] {
    let per = TEXELS_PER_CELL as f32;
    let (px, py) = ((x as f32 + 0.5) / per - 0.5, (y as f32 + 0.5) / per - 0.5);
    let (cx, cy) = (px.floor() as i32, py.floor() as i32);
    let (fx, fy) = (px - px.floor(), py - py.floor());
    let cells = [[0, 0], [1, 0], [0, 1], [1, 1]].map(|[i, j]| cell_at(pools, storey, [cx + i, cy + j]));
    let d = cells.map(|cell| cell.depth);
    let weights = [(1.0 - fx) * (1.0 - fy), fx * (1.0 - fy), (1.0 - fx) * fy, fx * fy];
    let depth: f32 = d.iter().zip(weights).map(|(d, w)| d * w).sum();
    if depth <= 0.0 {
        return [0; 4];
    }
    let gx = ((d[1] - d[0]) * (1.0 - fy) + (d[3] - d[2]) * fy) / CELL;
    let gy = ((d[2] - d[0]) * (1.0 - fx) + (d[3] - d[1]) * fx) / CELL;
    let distance = ((depth - FILM * 0.5) / gx.hypot(gy).max(1e-4)).clamp(-8.0, 8.0);
    let (wx, wy) = ((x as f32 + 0.5) * CELL / per, (y as f32 + 0.5) * CELL / per);
    let warp = (noise(wx / 24.0, wy / 24.0) * 0.65 + noise(wx / 9.0, wy / 9.0) * 0.35 - 0.5) * 2.0 * EDGE_WARP;
    let t = ((distance + warp + EDGE_SOFTNESS) / (2.0 * EDGE_SOFTNESS)).clamp(0.0, 1.0);
    let coverage = t * t * (3.0 - 2.0 * t);
    let (mut rgb, mut opacity, mut total) = ([0.0; 3], 0.0, 0.0);
    for (cell, w) in cells.iter().zip(weights) {
        let Some(kind) = cell.kind else { continue };
        let (own, cover) = look(kind);
        let dryness = 1.0 - ((cell.depth - FILM) / (3.0 * FILM)).clamp(0.0, 1.0);
        let blood = mix3(BLOOD, DRIED_BLOOD, dryness);
        let colour = mix3(own, blood, cell.blood);
        let w = w * cell.depth;
        rgb = std::array::from_fn(|k| rgb[k] + colour[k] * w);
        opacity += (cover + (0.95 - cover) * cell.blood) * w;
        total += w;
    }
    if total <= 0.0 {
        return [0; 4];
    }
    let byte = |v: f32| (v / total * 255.0).round().clamp(0.0, 255.0) as u8;
    [byte(rgb[0]), byte(rgb[1]), byte(rgb[2]), (opacity / total * coverage * 255.0).round().clamp(0.0, 255.0) as u8]
}

/// The texels of a chunk's cell `rows`, row by row.
fn texels(pools: &PoolStore, key: (i32, i32, i32), rows: Range<i32>) -> Vec<u8> {
    let (x0, y0) = (key.0 * CHUNK * TEXELS_PER_CELL, key.1 * CHUNK * TEXELS_PER_CELL);
    (rows.start * TEXELS_PER_CELL..rows.end * TEXELS_PER_CELL)
        .flat_map(|y| (0..SIDE as i32).map(move |x| [x0 + x, y0 + y]))
        .flat_map(|at| texel(pools, key.2, at))
        .collect()
}

fn wet_span(chunk: &Chunk) -> Option<[f32; 2]> {
    chunk.cells.iter().filter(|cell| cell.depth > 0.0 && cell.floor.is_finite()).fold(None, |span, cell| {
        let [lo, hi] = span.unwrap_or([cell.floor, cell.floor]);
        Some([lo.min(cell.floor), hi.max(cell.floor)])
    })
}

/// A box pointing down over the chunk: its local x is Unreal x, its local -y Unreal y, its local z up.
fn decal_transform(key: (i32, i32, i32), [lo, hi]: [f32; 2], origin: Vec3) -> Transform {
    let middle = [(key.0 as f32 + 0.5) * CHUNK_SIDE, (key.1 as f32 + 0.5) * CHUNK_SIDE, (lo + hi) * 0.5];
    Transform::from_translation(Vec3::from(to_bevy(middle)) + origin)
        .with_rotation(Quat::from_rotation_x(-FRAC_PI_2))
        .with_scale(Vec3::new(CHUNK_SIDE, CHUNK_SIDE, hi - lo + 2.0 * BOX_MARGIN))
}

/// Runs of set bits in `bits` as ranges of rows.
fn runs(bits: u32) -> impl Iterator<Item = Range<i32>> {
    let mut row = 0;
    std::iter::from_fn(move || {
        while row < CHUNK && bits & (1 << row) == 0 {
            row += 1;
        }
        let start = row;
        while row < CHUNK && bits & (1 << row) != 0 {
            row += 1;
        }
        (start < row).then_some(start..row)
    })
}

fn draw_pools(
    mut commands: Commands,
    mut liquids: ResMut<LiquidWorld>,
    facts: Res<FrameFacts>,
    mut decals: ResMut<PoolDecals>,
    mut images: ResMut<Assets<Image>>,
    mut rows: ResMut<PoolRows>,
    mut transforms: Query<&mut Transform>,
) {
    rows.bypass_change_detection().rows.clear();
    let PoolDecals { drawn, spare } = &mut *decals;
    let pools = &mut liquids.pools;
    for (_, gone) in drawn.extract_if(|key, _| !pools.chunks.contains_key(key)) {
        commands.entity(gone.entity).despawn();
        spare.push(gone.image);
    }
    let mut uploads = Vec::new();
    for (&key, chunk) in &pools.chunks {
        match drawn.get_mut(&key) {
            Some(slot) if chunk.dirty_rows != 0 => {
                slot.span = wet_span(chunk).unwrap_or(slot.span);
                let spread = chunk.dirty_rows | chunk.dirty_rows << 1 | chunk.dirty_rows >> 1;
                uploads.extend(runs(spread).map(|band| RowUpload { image: slot.image.id(), first: (band.start * TEXELS_PER_CELL) as u32, texels: texels(pools, key, band) }));
            }
            Some(_) => {}
            None => {
                let Some(span) = wet_span(chunk) else { continue };
                let data = texels(pools, key, 0..CHUNK);
                let reused = spare.pop().filter(|image| images.get_mut(image).map(|mut slot| slot.data = Some(data.clone())).is_some());
                let image = reused.unwrap_or_else(|| {
                    let size = Extent3d { width: SIDE, height: SIDE, depth_or_array_layers: 1 };
                    images.add(Image::new(size, TextureDimension::D2, data, TextureFormat::Rgba8Unorm, RenderAssetUsages::default()))
                });
                let decal = ClusteredDecal { base_color_texture: Some(image.clone()), tag: LIQUID_DECAL_TAG, ..Default::default() };
                let entity = commands.spawn((decal, decal_transform(key, span, facts.origin))).id();
                drawn.insert(key, Drawn { entity, image, span });
            }
        }
    }
    for chunk in pools.chunks.values_mut() {
        chunk.dirty_rows = 0;
    }
    for (&key, slot) in drawn.iter() {
        if let Ok(mut transform) = transforms.get_mut(slot.entity) {
            transform.set_if_neq(decal_transform(key, slot.span, facts.origin));
        }
    }
    if !uploads.is_empty() {
        rows.rows = uploads;
    }
}

fn upload_pool_rows(rows: Option<Res<PoolRows>>, images: Res<RenderAssets<GpuImage>>, queue: Res<RenderQueue>) {
    let Some(rows) = rows.filter(DetectChanges::is_changed) else { return };
    for row in &rows.rows {
        let Some(image) = images.get(row.image) else { continue };
        let height = row.texels.len() as u32 / (SIDE * 4);
        queue.write_texture(
            TexelCopyTextureInfo { texture: &image.texture, mip_level: 0, origin: Origin3d { x: 0, y: row.first, z: 0 }, aspect: TextureAspect::All },
            &row.texels,
            TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(SIDE * 4), rows_per_image: None },
            Extent3d { width: SIDE, height, depth_or_array_layers: 1 },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2_math::Vector;

    #[test]
    fn runs_of_dirty_rows() {
        assert_eq!(runs(0b0111_0010).collect::<Vec<_>>(), vec![1..2, 4..7]);
        assert_eq!(runs(1 << 31).collect::<Vec<_>>(), vec![31..32]);
        assert_eq!(runs(0).count(), 0);
    }

    #[test]
    fn a_blood_pool_is_dark_red_inside_and_clear_far_outside() {
        let mut pools = PoolStore::new(16, 1e6);
        for i in -2..=2 {
            for j in -2..=2 {
                pools.pour(Vector::new(4.0 + i as f32 * CELL, 4.0 + j as f32 * CELL, 0.0), FluidType::Blood, 64.0, 0.0);
            }
        }
        let [r, g, b, a] = texel(&pools, 0, [1, 1]);
        assert!(r > g && r > b && r < 60 && a > 200, "{r} {g} {b} {a}");
        assert_eq!(texel(&pools, 0, [40, 40])[3], 0);
        assert_eq!(texels(&pools, (0, 0, 0), 0..1).len(), (SIDE * 4 * TEXELS_PER_CELL as u32) as usize);
    }
}
