use std::f32::consts::TAU;

use bevy::math::Vec3;

use super::grid::ShadowSurface;
use super::on_threads;
use super::visibility::{BLOCK_TEXELS, grid_point};

const SKY_RAYS: usize = 16;
const SKY_WIDTH: u32 = 4096;
const SKY_BLOCKS_A_ROW: u32 = SKY_WIDTH / BLOCK_TEXELS;
const LATTICE_STEP: u32 = 2;
const LIFT: f32 = 2.0;

pub(super) struct SkyBake {
    pub(super) size: [u32; 2],
    pub(super) texels: Vec<u8>,
}

pub(super) fn sky_block(patch: u32) -> [u32; 2] {
    [patch % SKY_BLOCKS_A_ROW * BLOCK_TEXELS, patch / SKY_BLOCKS_A_ROW * BLOCK_TEXELS]
}

fn hemisphere() -> [Vec3; SKY_RAYS] {
    std::array::from_fn(|ray| {
        let off_pole = (ray as f32 + 0.5) / SKY_RAYS as f32;
        let around = TAU * (ray as u32).reverse_bits() as f32 / 4_294_967_296.0;
        let out = off_pole.sqrt();
        Vec3::new(out * around.cos(), out * around.sin(), (1.0 - off_pole).sqrt())
    })
}

fn open_share(point: Vec3, normal: Vec3, rays: &[Vec3], open: &impl Fn(Vec3, Vec3) -> bool, solid: &impl Fn(Vec3) -> bool) -> Option<f32> {
    let (tangent, bitangent) = normal.any_orthonormal_pair();
    let from = point + normal * LIFT;
    (!solid(from)).then(|| rays.iter().filter(|ray| open(from, tangent * ray.x + bitangent * ray.y + normal * ray.z)).count() as f32 / rays.len() as f32)
}

fn lattice_side(size: [u32; 2]) -> [u32; 2] {
    size.map(|side| side / LATTICE_STEP + 2)
}

fn bilinear(values: &[Option<f32>], across: u32, [u, v]: [f32; 2]) -> f32 {
    let low = |at: f32, side: usize| at.floor().min((side - 2) as f32);
    let (low_u, low_v) = (low(u, across as usize), low(v, values.len() / across as usize));
    let (fu, fv) = ((u - low_u).min(1.0), (v - low_v).min(1.0));
    let (sum, weight) = [(0, 0, (1.0 - fu) * (1.0 - fv)), (1, 0, fu * (1.0 - fv)), (0, 1, (1.0 - fu) * fv), (1, 1, fu * fv)].into_iter().fold((0.0, 0.0), |(sum, weight), (du, dv, share)| {
        match values[((low_v as u32 + dv) * across + low_u as u32 + du) as usize] {
            Some(value) => (sum + value * share, weight + share),
            None => (sum, weight),
        }
    });
    if weight > 0.0 { sum / weight } else { 0.0 }
}

pub(super) fn bake_sky(surfaces: &[ShadowSurface], patches: &[(u32, [u32; 2])], open: impl Fn(Vec3, Vec3) -> bool + Sync, solid: impl Fn(Vec3) -> bool + Sync) -> SkyBake {
    let rays = hemisphere();
    let sides: Vec<[u32; 2]> = surfaces.iter().map(|surface| lattice_side(surface.size)).collect();
    let mut starts = vec![0];
    starts.extend(sides.iter().scan(0, |end, &[across, down]| {
        *end += (across * down) as usize;
        Some(*end)
    }));
    let lattice = on_threads(starts[surfaces.len()], |point| {
        let chart = starts.partition_point(|&start| start <= point) - 1;
        let (surface, local) = (&surfaces[chart], (point - starts[chart]) as u32);
        let across = sides[chart][0];
        let at = [local % across, local / across].map(|k| (k * LATTICE_STEP) as f32 - 0.5);
        open_share(grid_point(surface, at), Vec3::from(surface.normal), &rays, &open, &solid)
    });
    let size = [SKY_WIDTH, (patches.len() as u32).div_ceil(SKY_BLOCKS_A_ROW) * BLOCK_TEXELS];
    let mut texels = vec![0; (size[0] * size[1]) as usize];
    for (patch, &(chart, corner)) in patches.iter().enumerate() {
        let chart = chart as usize;
        let values = &lattice[starts[chart]..starts[chart + 1]];
        let [x, y] = sky_block(patch as u32);
        for j in 0..BLOCK_TEXELS {
            for i in 0..BLOCK_TEXELS {
                let at = [corner[0] + i, corner[1] + j].map(|texel| texel as f32 / LATTICE_STEP as f32);
                texels[((y + j) * SKY_WIDTH + x + i) as usize] = (bilinear(values, sides[chart][0], at) * 255.0).round() as u8;
            }
        }
    }
    SkyBake { size, texels }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lighting::lamps::grid::{PATCH_TEXELS, Sides};
    use crate::lighting::lamps::visibility::cut;
    use crate::gpu::shaders::shader_const;

    fn floor(size: [u32; 2]) -> ShadowSurface {
        ShadowSurface { origin: [0.0; 3], u: [1.0, 0.0, 0.0], v: [0.0, 0.0, 1.0], normal: [0.0, 1.0, 0.0], size, sides: Sides::One }
    }

    fn baked(size: [u32; 2], open: impl Fn(Vec3, Vec3) -> bool + Sync) -> SkyBake {
        let surfaces = [floor(size)];
        bake_sky(&surfaces, &cut(&surfaces).1, open, |_| false)
    }

    fn block_row(bake: &SkyBake, patch: u32, row: u32) -> &[u8] {
        let [x, y] = sky_block(patch);
        let start = ((y + row) * bake.size[0] + x) as usize;
        &bake.texels[start..start + BLOCK_TEXELS as usize]
    }

    #[test]
    fn an_open_floor_sees_all_sky() {
        let bake = baked([16, 16], |_, way| way.y > 0.0);
        assert!(bake.texels[..BLOCK_TEXELS as usize].iter().all(|&texel| texel == 255));
        assert!(block_row(&bake, 0, BLOCK_TEXELS - 1).iter().all(|&texel| texel == 255));
    }

    #[test]
    fn a_small_chart_traces_only_its_own_texels_and_fills_its_whole_block() {
        let mut traced = std::sync::atomic::AtomicUsize::new(0);
        let bake = baked([3, 3], |_, _| {
            traced.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            true
        });
        assert_eq!(*traced.get_mut(), 3 * 3 * SKY_RAYS);
        assert!(bake.texels[..BLOCK_TEXELS as usize].iter().all(|&texel| texel == 255));
        assert!(block_row(&bake, 0, BLOCK_TEXELS - 1).iter().all(|&texel| texel == 255));
    }

    #[test]
    fn a_floor_in_a_closed_box_sees_none() {
        let bake = baked([32, 32], |from, _| from.abs().max_element() > 1000.0);
        assert!(bake.texels.iter().all(|&texel| texel == 0));
    }

    #[test]
    fn a_texel_by_solid_takes_the_open_side_alone() {
        let surfaces = [floor([16, 16])];
        let bake = bake_sky(&surfaces, &cut(&surfaces).1, |_, _| true, |from| from.x > 30.0);
        assert_eq!(&block_row(&bake, 0, 5)[..5], &[255, 255, 255, 255, 0]);
    }

    #[test]
    fn block_texels_blend_the_lattice_between_its_points() {
        let bake = baked([16, 16], |from, _| from.x < 30.0);
        assert_eq!(&block_row(&bake, 0, 5)[..6], &[255, 255, 255, 128, 0, 0]);
    }

    #[test]
    fn blocks_fill_rows_of_the_texture_in_patch_order() {
        assert_eq!(sky_block(0), [0, 0]);
        assert_eq!(sky_block(SKY_BLOCKS_A_ROW - 1), [(SKY_BLOCKS_A_ROW - 1) * BLOCK_TEXELS, 0]);
        assert_eq!(sky_block(SKY_BLOCKS_A_ROW), [0, BLOCK_TEXELS]);
        const { assert!(SKY_BLOCKS_A_ROW * BLOCK_TEXELS <= SKY_WIDTH) };
        let bake = baked([PATCH_TEXELS * 2, PATCH_TEXELS], |from, _| from.x < PATCH_TEXELS as f32 * 16.0);
        assert_eq!(bake.size, [SKY_WIDTH, BLOCK_TEXELS]);
        assert_eq!(block_row(&bake, 0, 0)[0], 255);
        assert_eq!(block_row(&bake, 1, 0)[BLOCK_TEXELS as usize - 1], 0);
    }

    #[test]
    fn the_lit_shader_finds_the_blocks_where_they_are_baked() {
        assert_eq!(shader_const(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/shaders/lamp_shadow.wesl")), "SKY_BLOCKS_A_ROW"), f64::from(SKY_BLOCKS_A_ROW));
    }
}
