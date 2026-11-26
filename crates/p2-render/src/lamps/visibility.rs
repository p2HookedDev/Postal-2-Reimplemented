//! Each surface patch's lamp visibility, cached once a map: a block of texels for every four lamps that light the
//! patch, one lamp a channel.

use std::ops::Range;
use std::sync::Arc;

use bevy::asset::{AssetId, Handle, uuid_handle};
use bevy::ecs::prelude::{Local, Res, ResMut};
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::render::diagnostic::DiagnosticsRecorder;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, Buffer, BufferBinding, BufferInitDescriptor, BufferUsages, CachedComputePipelineId, ComputePassDescriptor,
    ComputePipelineDescriptor, PipelineCache, SamplerBindingType, ShaderStages, StorageTextureAccess, TextureFormat, TextureSampleType, binding_types,
};
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::texture::GpuImage;
use bevy::shader::Shader;

use super::atlas::{BookRecords, PlainTilesDrawn, record_timed};
use super::cells::PatchBox;
use super::grid::{LAMP_TEXEL, PATCH_TEXELS, ShadowSurface, Sides};
use super::{LampAtlas, LampVisibility};
use crate::stage_encoder::{Prepared, StageEncoder};

pub(super) const VISIBILITY_SHADER: Handle<Shader> = uuid_handle!("b8e3c1d7-5a94-4f26-8e0b-3d71a9c4f582");
const VISIBILITY_WIDTH: u32 = 4096;
const MOST_VISIBILITY_BYTES: u64 = 64 << 20;
/// A patch's texels and a guard texel each side.
pub const BLOCK_TEXELS: u32 = PATCH_TEXELS + 2;
const GROUP_SIDE: u32 = 8;
const BLOCK_TILES: u32 = BLOCK_TEXELS.div_ceil(GROUP_SIDE);
const MOST_GROUPS: u32 = 65535;
const JOBS_A_FRAME: usize = 4096;
const JOB_BYTES: u64 = size_of::<PatchJob>() as u64;
// The header texel that holds where the surfaces and patches start, then 1 once every patch is cached.
const READY_TEXEL: usize = 2;

/// One patch's work for the visibility pass, laid out as the shader reads it (64 bytes). Block texel (i, j) is the
/// point `origin + i * u + j * v`; `atlas` packs the block's x in the low 16 bits, y in the high.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PatchJob {
    pub origin: [f32; 3],
    pub groups: u32,
    pub u: [f32; 3],
    pub list: u32,
    pub v: [f32; 3],
    pub size: u32,
    pub normal: [f32; 3],
    pub atlas: u32,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VisibilityPlan {
    pub size: [u32; 2],
    pub jobs: Vec<PatchJob>,
}

/// Each surface's first patch and patches across, and each patch's surface and first grid texel, in patch order.
/// Each surface's first patch, patches across and 1 when two-sided; then each patch's surface and corner.
pub(super) fn cut(surfaces: &[ShadowSurface]) -> (Vec<[u32; 3]>, Vec<(u32, [u32; 2])>) {
    let (mut firsts, mut patches) = (Vec::with_capacity(surfaces.len()), Vec::new());
    for (index, surface) in surfaces.iter().enumerate() {
        let [across, down] = surface.size.map(|side| side.div_ceil(PATCH_TEXELS));
        firsts.push([patches.len() as u32, across, u32::from(surface.sides == Sides::Two)]);
        for pv in 0..down {
            patches.extend((0..across).map(|pu| (index as u32, [pu * PATCH_TEXELS, pv * PATCH_TEXELS])));
        }
    }
    (firsts, patches)
}

pub(super) fn grid_point(surface: &ShadowSurface, [u, v]: [f32; 2]) -> Vec3 {
    Vec3::from(surface.origin) + (Vec3::from(surface.u) * u + Vec3::from(surface.v) * v) * LAMP_TEXEL
}

pub(super) fn patch_box(surface: &ShadowSurface, corner: [u32; 2]) -> PatchBox {
    let end = [0, 1].map(|axis| (corner[axis] + PATCH_TEXELS).min(surface.size[axis]) as f32);
    let [u, v] = corner.map(|at| at as f32);
    let points = [[u, v], [end[0], v], [u, end[1]], end].map(|at| grid_point(surface, at));
    let low = points.iter().fold(Vec3::MAX, |low, &point| low.min(point)) - LAMP_TEXEL;
    let high = points.iter().fold(Vec3::MIN, |high, &point| high.max(point)) + LAMP_TEXEL;
    PatchBox { low: low.to_array(), high: high.to_array(), normal: surface.normal, sides: surface.sides }
}

/// Shelf packs each patch's blocks side by side, a row of blocks `BLOCK_TEXELS` high. Gives the texture's size and
/// each patch's first block, or `None` when nothing is lit or the texture would pass the cap.
pub(super) fn pack_blocks(groups: &[u32]) -> Option<([u32; 2], Vec<[u32; 2]>)> {
    let (mut x, mut y) = (0, 0);
    let mut places = Vec::with_capacity(groups.len());
    for &count in groups {
        let width = count * BLOCK_TEXELS;
        if width == 0 {
            places.push([0, 0]);
            continue;
        }
        if width > VISIBILITY_WIDTH {
            return None;
        }
        if x + width > VISIBILITY_WIDTH {
            (x, y) = (0, y + BLOCK_TEXELS);
        }
        places.push([x, y]);
        x += width;
    }
    let height = y + BLOCK_TEXELS;
    (x > 0 && u64::from(VISIBILITY_WIDTH) * u64::from(height) * 4 <= MOST_VISIBILITY_BYTES).then_some(([VISIBILITY_WIDTH, height], places))
}

/// `patch_texels` are the book's patch texels: block place, groups and list start.
pub(super) fn patch_jobs(surfaces: &[ShadowSurface], patches: &[(u32, [u32; 2])], patch_texels: &[[f32; 4]]) -> Vec<PatchJob> {
    patches
        .iter()
        .zip(patch_texels)
        .filter(|(_, texel)| texel[2] > 0.0)
        .map(|(&(surface, corner), &[x, y, groups, list])| {
            let surface = &surfaces[surface as usize];
            PatchJob {
                origin: grid_point(surface, corner.map(|at| at as f32 - 0.5)).to_array(),
                groups: groups as u32,
                u: (Vec3::from(surface.u) * LAMP_TEXEL).to_array(),
                list: list as u32,
                v: (Vec3::from(surface.v) * LAMP_TEXEL).to_array(),
                size: BLOCK_TEXELS,
                normal: surface.normal,
                atlas: x as u32 | (y as u32) << 16,
            }
        })
        .collect()
}

fn job_bytes(jobs: &[PatchJob]) -> Vec<u8> {
    jobs.iter()
        .flat_map(|job| [(job.origin, job.groups), (job.u, job.list), (job.v, job.size), (job.normal, job.atlas)])
        .flat_map(|(vector, word)| vector.map(f32::to_bits).into_iter().chain([word]))
        .flat_map(u32::to_le_bytes)
        .collect()
}

fn next_batch(done: usize, total: usize) -> Range<usize> {
    done..total.min(done + JOBS_A_FRAME)
}

/// A group for each of a block's 8x8 tiles along x, a job for each along y, then z.
fn dispatch_shape(jobs: u32) -> [u32; 3] {
    [BLOCK_TILES * BLOCK_TILES, jobs.min(MOST_GROUPS), jobs.div_ceil(MOST_GROUPS)]
}

struct VisibilityPipeline {
    empty: BindGroupLayoutDescriptor,
    layout: BindGroupLayoutDescriptor,
    id: CachedComputePipelineId,
}

#[derive(Default)]
pub(super) struct VisibilityCache {
    pipeline: Option<VisibilityPipeline>,
    key: Option<(usize, usize, AssetId<Image>, AssetId<Image>)>,
    jobs: Option<Buffer>,
    done: usize,
}

fn queue_pipeline(cache: &PipelineCache) -> VisibilityPipeline {
    let empty = BindGroupLayoutDescriptor::new("lamp visibility empty", &[]);
    let layout = BindGroupLayoutDescriptor::new(
        "lamp visibility",
        &BindGroupLayoutEntries::with_indices(
            ShaderStages::COMPUTE,
            (
                (0, binding_types::storage_buffer_read_only_sized(false, None)),
                (1, binding_types::texture_storage_2d(TextureFormat::Rgba8Unorm, StorageTextureAccess::WriteOnly)),
                (3, binding_types::texture_2d(TextureSampleType::Float { filterable: false })),
                (4, binding_types::texture_2d(TextureSampleType::Float { filterable: true })),
                (5, binding_types::sampler(SamplerBindingType::Filtering)),
            ),
        ),
    );
    let id = cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some("lamp visibility".into()),
        layout: vec![empty.clone(), layout.clone()],
        shader: VISIBILITY_SHADER,
        // The shared shadow code is built with a filter set, as the mask pass builds it; this pass never samples the sun.
        shader_defs: vec!["LAMP_MASK_PASS".into(), "SHADOW_FILTER_METHOD_HARDWARE_2X2".into()],
        entry_point: Some("cache".into()),
        ..Default::default()
    });
    VisibilityPipeline { empty, layout, id }
}

/// Caches the patches' visibility once the plain tiles of the map's bake are drawn, a batch of jobs a frame, then
/// marks the book ready for the lit surfaces.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn cache_lamp_visibility(
    visibility: Res<LampVisibility>,
    atlas: Res<LampAtlas>,
    plain: Res<PlainTilesDrawn>,
    images: Res<RenderAssets<GpuImage>>,
    cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut stage: ResMut<StageEncoder<Prepared>>,
    recorder: Option<Res<DiagnosticsRecorder>>,
    mut records: ResMut<BookRecords>,
    mut state: Local<VisibilityCache>,
) {
    let (Some(bake), Some(shadows), Some(book), Some(target)) = (&atlas.bake, &atlas.image, &atlas.book, &visibility.image) else { return };
    let bake_key = Arc::as_ptr(bake).addr();
    if plain.0 != Some(bake_key) {
        return;
    }
    let state = &mut *state;
    let key = (bake_key, Arc::as_ptr(&visibility.jobs).addr(), target.id(), book.id());
    if state.key != Some(key) {
        state.key = Some(key);
        state.done = 0;
        state.jobs = (!visibility.jobs.is_empty())
            .then(|| device.create_buffer_with_data(&BufferInitDescriptor { label: Some("lamp visibility jobs"), contents: &job_bytes(&visibility.jobs), usage: BufferUsages::STORAGE }));
    }
    if state.done >= visibility.jobs.len() {
        return;
    }
    let pipeline = state.pipeline.get_or_insert_with(|| queue_pipeline(&cache));
    let (Some(compute), Some(jobs), Some(target), Some(book_image), Some(shadows)) = (cache.get_compute_pipeline(pipeline.id), &state.jobs, images.get(target), images.get(book), images.get(shadows)) else {
        return;
    };
    let batch = next_batch(state.done, visibility.jobs.len());
    let empty = device.create_bind_group("lamp visibility empty", &cache.get_bind_group_layout(&pipeline.empty), &[]);
    let group = device.create_bind_group(
        "lamp visibility",
        &cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::with_indices((
            (0, BufferBinding { buffer: jobs, offset: batch.start as u64 * JOB_BYTES, size: None }),
            (1, &target.texture_view),
            (3, &book_image.texture_view),
            (4, &shadows.texture_view),
            (5, &shadows.sampler),
        )),
    );
    let [x, y, z] = dispatch_shape(batch.len() as u32);
    record_timed(&device, &mut stage, recorder.as_deref(), |encoder| {
        let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor { label: Some("lamp visibility"), timestamp_writes: None });
        pass.set_pipeline(compute);
        pass.set_bind_group(0, &empty, &[]);
        pass.set_bind_group(1, &group, &[]);
        pass.dispatch_workgroups(x, y, z);
    });
    state.done = batch.end;
    if state.done < visibility.jobs.len() {
        return;
    }
    let Some(&[surfaces, patches, _, last]) = bake.records.get(READY_TEXEL) else { return };
    records.set(bake, book.id(), READY_TEXEL, [surfaces, patches, 1.0, last]);
    records.write(&queue, book_image);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::shader_const;

    fn surface(size: [u32; 2]) -> ShadowSurface {
        ShadowSurface { origin: [10.0, 20.0, 30.0], u: [1.0, 0.0, 0.0], v: [0.0, 0.0, 1.0], normal: [0.0, -1.0, 0.0], size, sides: Sides::One }
    }

    #[test]
    fn a_job_is_64_bytes() {
        assert_eq!(size_of::<PatchJob>(), 64);
    }

    #[test]
    fn surfaces_are_cut_into_patches_row_by_row() {
        let (firsts, patches) = cut(&[surface([40, 17]), surface([3, 3])]);
        assert_eq!(firsts, [[0, 3, 0], [6, 1, 0]]);
        assert_eq!(patches.len(), 7);
        assert_eq!(patches[4], (0, [16, 16]));
        assert_eq!(patches[6], (1, [0, 0]));
    }

    #[test]
    fn a_patch_box_is_its_texels_grown_by_a_guard_texel() {
        let last = patch_box(&surface([40, 10]), [32, 0]);
        assert_eq!(last.low, [10.0 + 31.0 * 16.0, 4.0, 14.0]);
        assert_eq!(last.high, [10.0 + 41.0 * 16.0, 36.0, 30.0 + 11.0 * 16.0]);
    }

    #[test]
    fn blocks_shelf_pack_side_by_side_and_skip_unlit_patches() {
        let (size, places) = pack_blocks(&[2, 0, 200, 30]).unwrap();
        assert_eq!(places, [[0, 0], [0, 0], [36, 0], [0, 18]]);
        assert_eq!(size, [4096, 36]);
        assert!(pack_blocks(&[0, 0]).is_none());
        assert!(pack_blocks(&[228]).is_none());
        assert!(pack_blocks(&vec![227; 4000]).is_none());
    }

    #[test]
    fn a_job_starts_half_a_texel_before_its_patch() {
        let surfaces = [surface([40, 40])];
        let jobs = patch_jobs(&surfaces, &[(0, [0, 0]), (0, [16, 16])], &[[0.0, 0.0, 0.0, 9.0], [36.0, 18.0, 2.0, 9.0]]);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].origin, [10.0 + 15.5 * 16.0, 20.0, 30.0 + 15.5 * 16.0]);
        assert_eq!((jobs[0].groups, jobs[0].list, jobs[0].size, jobs[0].atlas), (2, 9, 18, 36 | 18 << 16));
        assert_eq!(jobs[0].u, [16.0, 0.0, 0.0]);
    }

    #[test]
    fn the_pass_dispatches_the_groups_the_shader_declares() {
        let shader = include_str!("../lamp_visibility.wesl");
        assert_eq!(shader_const(shader, "GROUP_SIDE"), f64::from(GROUP_SIDE));
        assert_eq!(shader_const(shader, "BLOCK_TILES"), f64::from(BLOCK_TILES));
        assert_eq!(shader_const(shader, "MOST_GROUPS"), f64::from(MOST_GROUPS));
        const { assert!(BLOCK_TILES * GROUP_SIDE >= BLOCK_TEXELS) };
        assert_eq!(dispatch_shape(4096), [9, 4096, 1]);
        assert_eq!(dispatch_shape(70000), [9, 65535, 2]);
        let lit = include_str!("../lamp_shadow.wesl");
        assert_eq!(shader_const(lit, "CACHED_PATCH"), f64::from(PATCH_TEXELS));
        assert_eq!(shader_const(lit, "CACHED_BLOCK"), f64::from(BLOCK_TEXELS));
    }

    #[test]
    fn jobs_go_a_batch_a_frame_from_an_aligned_offset() {
        assert_eq!(next_batch(0, 10000), 0..4096);
        assert_eq!(next_batch(8192, 10000), 8192..10000);
        assert_eq!(JOBS_A_FRAME as u64 * JOB_BYTES % 256, 0);
    }

    #[test]
    fn a_job_uploads_as_the_shader_reads_it() {
        let job = PatchJob { origin: [1.0, 2.0, 3.0], groups: 4, u: [5.0; 3], list: 6, v: [7.0; 3], size: 18, normal: [0.0, 0.0, 1.0], atlas: 9 };
        let bytes = job_bytes(&[job, job]);
        assert_eq!(bytes.len(), 128);
        assert_eq!(bytes[4..8], 2.0f32.to_le_bytes());
        assert_eq!(bytes[12..16], 4u32.to_le_bytes());
        assert_eq!(bytes[28..32], 6u32.to_le_bytes());
        assert_eq!(bytes[60..64], 9u32.to_le_bytes());
    }
}
