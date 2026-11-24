//! Draws each mover into its tiles of the lamp atlas, and writes the tiles and each lamp's run of them into the book.

use std::num::NonZero;
use std::ops::Range;
use std::sync::Arc;

use bevy::asset::{AssetId, Handle, uuid_handle};
use bevy::ecs::prelude::{Local, Res, ResMut};
use bevy::image::Image;
use bevy::mesh::{Mesh, MeshVertexBufferLayoutRef, VertexBufferLayout};
use bevy::render::diagnostic::DiagnosticsRecorder;
use bevy::render::mesh::allocator::{MeshAllocator, MeshBufferSlice};
use bevy::render::mesh::{RenderMesh, RenderMeshBufferInfo};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_phase::TrackedRenderPass;
use bevy::render::render_resource::binding_types::{storage_buffer_read_only_sized, uniform_buffer_sized};
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, Buffer, BufferBinding,
    BufferDescriptor, BufferUsages, CachedRenderPipelineId, ColorTargetState, ColorWrites, CommandEncoder, FragmentState, IndexFormat, LoadOp,
    MultisampleState, Operations, PipelineCache, PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline, RenderPipelineDescriptor,
    ShaderStages, StoreOp, TextureFormat, VertexState,
};
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::texture::GpuImage;
use bevy::shader::Shader;
use rustc_hash::FxHashMap;

use super::LampAtlas;
use crate::stage_encoder::{Prepared, StageEncoder};
use super::atlas::{GUARD_TEXELS, NEAR, BookRecords, PlainTilesDrawn, record_timed, write_rows};
use super::cells::{CAPSULES_FROM_END, HEADER_TEXELS, RECORD_TEXELS, TILE_GRID_FROM_END, TILES_FROM_END};
use super::faces::DETAIL_UNIT;
use super::movers::{Drawn, MOST_MOVER_TILES, MoverCaster, MoverTile, MoverTiles};

pub(super) const MOVER_CASTER_SHADER: Handle<Shader> = uuid_handle!("b3e7c2a9-41d6-4f08-9a5e-6c1d82f37b04");

const CASTER_STRIDE: u64 = 256;
const CASTER_SIZE: u64 = 144;
const JOINT_SIZE: u64 = 64;

const MIN: BlendComponent = BlendComponent { src_factor: BlendFactor::One, dst_factor: BlendFactor::One, operation: BlendOperation::Min };
pub(super) const NEAREST: BlendState = BlendState { color: MIN, alpha: MIN };

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Skin {
    Rigid,
    Skinned,
}

/// The first five rows of the view a lamp's square is drawn with, as `lamp_atlas.wesl` and `mover_caster.wesl` read them.
pub(super) fn view_rows(axes: [[f32; 3]; 3], lamp_at: [f32; 3], radius: f32, size: u32, tan_half: f32, first_joint: f32) -> [[f32; 4]; 5] {
    let half = 0.5 * size as f32;
    let depth_a = radius / (radius - NEAR).max(1.0);
    let [[ax, ay, az], [bx, by, bz], [cx, cy, cz]] = axes;
    let [px, py, pz] = lamp_at;
    [[ax, ay, az, 0.0], [bx, by, bz, 0.0], [cx, cy, cz, 0.0], [px, py, pz, 1.0 / radius], [(half - GUARD_TEXELS) / half / tan_half, depth_a, -NEAR * depth_a, first_joint]]
}

/// What `mover_caster.wesl` draws one caster into one tile with.
fn caster_uniform(bytes: &mut Vec<u8>, tile: &MoverTile, caster: &MoverCaster) {
    let (right, up) = tile.sides();
    let first_joint = caster.joints.as_ref().map_or(0, |joints| joints.start);
    let start = bytes.len();
    let rows = view_rows([right, up, tile.axis].map(|axis| axis.to_array()), tile.lamp_at.to_array(), tile.lamp_radius, tile.size, tile.tan_half, first_joint as f32);
    let world_from_mesh = caster.world_from_mesh.to_cols_array();
    bytes.extend(rows.iter().flatten().chain(&world_from_mesh).flat_map(|value| value.to_le_bytes()));
    bytes.resize(start + CASTER_STRIDE as usize, 0);
}

/// A square of the atlas that holds a mover's tile from one lamp in full. Not which mover: its number changes as
/// others come and go, and a tile whose square changed hands is drawn again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    x: u32,
    y: u32,
    size: u32,
    lamp: u32,
}

impl Held {
    fn of(tile: &MoverTile) -> Self {
        Self { x: tile.x, y: tile.y, size: tile.size, lamp: tile.lamp }
    }

    fn overlaps(&self, other: &Self) -> bool {
        self.x < other.x + other.size && other.x < self.x + self.size && self.y < other.y + other.size && other.y < self.y + self.size
    }
}

/// Drops every held square that shares texels with `square`. Squares are powers of two aligned to their size, so one
/// that overlaps another has its corner inside it or sits at the other's corner rounded down to its own size.
fn forget_under(held: &mut FxHashMap<(u32, u32), Held>, square: &Held) {
    let units = square.size / DETAIL_UNIT;
    for corner in (0..units).flat_map(|y| (0..units).map(move |x| (square.x + x * DETAIL_UNIT, square.y + y * DETAIL_UNIT))) {
        held.remove(&corner);
    }
    for bit in square.size.trailing_zeros() + 1..u32::BITS {
        let corner = (square.x >> bit << bit, square.y >> bit << bit);
        if held.get(&corner).is_some_and(|old| old.overlaps(square)) {
            held.remove(&corner);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LampRange {
    lamp: u32,
    first: u32,
    count: u32,
}

fn lamp_ranges(tiles: &[MoverTile]) -> impl Iterator<Item = LampRange> + '_ {
    let mut first = 0;
    tiles.chunk_by(|a, b| a.lamp == b.lamp).map(move |run| {
        let range = LampRange { lamp: run[0].lamp, first, count: run.len() as u32 };
        first += range.count;
        range
    })
}

/// The sixth texel of each lamp's record that must change: its run of tiles, or zero for a lamp that lost them.
fn changed_ranges<'a>(before: &'a [LampRange], now: &'a [LampRange]) -> impl Iterator<Item = (u32, [f32; 4])> + 'a {
    let lost = before.iter().filter(move |old| now.iter().all(|range| range.lamp != old.lamp)).map(|old| (old.lamp, [0.0; 4]));
    let fresh = now.iter().filter(move |range| !before.contains(range)).map(|range| (range.lamp, [range.first as f32, range.count as f32, 0.0, 0.0]));
    lost.chain(fresh)
}

struct CasterLayouts {
    caster: BindGroupLayoutDescriptor,
    joints: BindGroupLayoutDescriptor,
    clear: CachedRenderPipelineId,
}

#[derive(Default)]
pub(super) struct MoverDraw {
    /// The capsules' rows as the book holds them.
    grounding: Vec<[f32; 4]>,
    tile_grid: Vec<[f32; 4]>,
    layouts: Option<CasterLayouts>,
    /// By [`Skin`]; `None` for a mesh layout without the attributes its entry point reads.
    pipelines: [FxHashMap<MeshVertexBufferLayoutRef, Option<CachedRenderPipelineId>>; 2],
    uniforms: Option<(Buffer, BindGroup)>,
    joints: Option<(Buffer, BindGroup)>,
    key: Option<(usize, AssetId<Image>, AssetId<Image>)>,
    /// By corner.
    held: FxHashMap<(u32, u32), Held>,
    ranges: Vec<LampRange>,
    next_ranges: Vec<LampRange>,
    /// The casters' places, ordered by mover.
    by_mover: Vec<usize>,
    bytes: Vec<u8>,
    row: Vec<[f32; 4]>,
}

pub(super) fn pipeline(
    cache: &PipelineCache,
    shader: Handle<Shader>,
    label: &'static str,
    layout: Vec<BindGroupLayoutDescriptor>,
    [vertex, fragment]: [&'static str; 2],
    buffers: Vec<VertexBufferLayout>,
    blend: Option<BlendState>,
) -> CachedRenderPipelineId {
    cache.queue_render_pipeline(RenderPipelineDescriptor {
        label: Some(label.into()),
        layout,
        vertex: VertexState { shader: shader.clone(), entry_point: Some(vertex.into()), buffers, ..Default::default() },
        fragment: Some(FragmentState {
            shader,
            entry_point: Some(fragment.into()),
            targets: vec![Some(ColorTargetState { format: TextureFormat::R16Float, blend, write_mask: ColorWrites::ALL })],
            ..Default::default()
        }),
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        ..Default::default()
    })
}

fn caster_layouts(cache: &PipelineCache) -> CasterLayouts {
    CasterLayouts {
        caster: BindGroupLayoutDescriptor::new("mover_caster", &BindGroupLayoutEntries::single(ShaderStages::VERTEX_FRAGMENT, uniform_buffer_sized(true, NonZero::new(CASTER_SIZE)))),
        joints: BindGroupLayoutDescriptor::new("mover_caster_joints", &BindGroupLayoutEntries::single(ShaderStages::VERTEX, storage_buffer_read_only_sized(false, None))),
        clear: pipeline(cache, MOVER_CASTER_SHADER, "mover tile clear", Vec::new(), ["clear_vertex", "clear_fragment"], Vec::new(), None),
    }
}

fn caster_pipeline(cache: &PipelineCache, layouts: &CasterLayouts, layout: &MeshVertexBufferLayoutRef, skin: Skin) -> Option<CachedRenderPipelineId> {
    let (attributes, entry, groups) = match skin {
        Skin::Rigid => (vec![Mesh::ATTRIBUTE_POSITION.at_shader_location(0)], "rigid_vertex", vec![layouts.caster.clone()]),
        Skin::Skinned => (
            vec![Mesh::ATTRIBUTE_POSITION.at_shader_location(0), Mesh::ATTRIBUTE_JOINT_INDEX.at_shader_location(1), Mesh::ATTRIBUTE_JOINT_WEIGHT.at_shader_location(2)],
            "skinned_vertex",
            vec![layouts.caster.clone(), layouts.joints.clone()],
        ),
    };
    let buffer = layout.0.get_layout(&attributes).ok()?;
    Some(pipeline(cache, MOVER_CASTER_SHADER, "mover tile", groups, [entry, "fragment"], vec![buffer], Some(NEAREST)))
}

/// A buffer kept across frames, replaced by one of the next power of two in bytes once it is too small.
fn kept<'a>(slot: &'a mut Option<(Buffer, BindGroup)>, device: &RenderDevice, label: &'static str, size: u64, usage: BufferUsages, group: impl FnOnce(&Buffer) -> BindGroup) -> &'a (Buffer, BindGroup) {
    *slot = slot.take().filter(|(buffer, _)| buffer.size() >= size);
    slot.get_or_insert_with(|| {
        let buffer = device.create_buffer(&BufferDescriptor { label: Some(label), size: size.next_power_of_two(), usage: usage | BufferUsages::COPY_DST, mapped_at_creation: false });
        let group = group(&buffer);
        (buffer, group)
    })
}

struct Sources<'a> {
    meshes: &'a RenderAssets<RenderMesh>,
    allocator: &'a MeshAllocator,
    cache: &'a PipelineCache,
}

struct CasterDraw<'a> {
    pipeline: &'a RenderPipeline,
    skin: Skin,
    uniform: u32,
    vertices: MeshBufferSlice<'a>,
    indices: Option<(MeshBufferSlice<'a>, IndexFormat, u32)>,
}

enum Ready<'a> {
    Draw(CasterDraw<'a>),
    Never,
    Waiting,
}

fn caster_draw<'a>(
    caster: &MoverCaster,
    sources: &Sources<'a>,
    layouts: &CasterLayouts,
    pipelines: &mut [FxHashMap<MeshVertexBufferLayoutRef, Option<CachedRenderPipelineId>>; 2],
) -> Ready<'a> {
    let Some(mesh) = sources.meshes.get(caster.mesh) else { return Ready::Waiting };
    let skin = if caster.joints.is_some() { Skin::Skinned } else { Skin::Rigid };
    let pipelines = &mut pipelines[skin as usize];
    let id = match pipelines.get(&mesh.layout) {
        Some(&id) => id,
        None => *pipelines.entry(mesh.layout.clone()).or_insert_with(|| caster_pipeline(sources.cache, layouts, &mesh.layout, skin)),
    };
    let Some(id) = id else { return Ready::Never };
    let (Some(pipeline), Some(vertices)) = (sources.cache.get_render_pipeline(id), sources.allocator.mesh_vertex_slice(&caster.mesh)) else { return Ready::Waiting };
    let indices = match mesh.buffer_info {
        RenderMeshBufferInfo::Indexed { count, index_format } => match sources.allocator.mesh_index_slice(&caster.mesh) {
            Some(indices) => Some((indices, index_format, count)),
            None => return Ready::Waiting,
        },
        RenderMeshBufferInfo::NonIndexed => None,
    };
    Ready::Draw(CasterDraw { pipeline, skin, uniform: 0, vertices, indices })
}

#[expect(clippy::too_many_arguments, reason = "one render pass, its target and everything it binds")]
fn draw_squares(
    encoder: &mut CommandEncoder,
    device: &RenderDevice,
    target: &GpuImage,
    clear: &RenderPipeline,
    draws: &[(&MoverTile, Range<usize>)],
    casts: &[CasterDraw],
    casters: &BindGroup,
    joints: Option<&BindGroup>,
) {
    let pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("mover tiles"),
        color_attachments: &[Some(RenderPassColorAttachment { view: &target.texture_view, depth_slice: None, resolve_target: None, ops: Operations { load: LoadOp::Load, store: StoreOp::Store } })],
        ..Default::default()
    });
    let mut pass = TrackedRenderPass::new(device, pass);
    for (tile, tile_draws) in draws {
        pass.set_viewport(tile.x as f32, tile.y as f32, tile.size as f32, tile.size as f32, 0.0, 1.0);
        pass.set_scissor_rect(tile.x, tile.y, tile.size, tile.size);
        pass.set_render_pipeline(clear);
        pass.draw(0..3, 0..1);
        for draw in &casts[tile_draws.clone()] {
            pass.set_render_pipeline(draw.pipeline);
            pass.set_bind_group(0, casters, &[draw.uniform]);
            if let (Skin::Skinned, Some(joints)) = (draw.skin, joints) {
                pass.set_bind_group(1, joints, &[]);
            }
            pass.set_vertex_buffer(0, draw.vertices.buffer.slice(..));
            match &draw.indices {
                Some((indices, format, count)) => {
                    pass.set_index_buffer(indices.buffer.slice(..), *format);
                    pass.draw_indexed(indices.range.start..indices.range.start + count, draw.vertices.range.start as i32, 0..1);
                }
                None => pass.draw(draw.vertices.range.clone(), 0..1),
            }
        }
    }
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn draw_mover_tiles(
    movers: Res<MoverTiles>,
    atlas: Res<LampAtlas>,
    images: Res<RenderAssets<GpuImage>>,
    meshes: Res<RenderAssets<RenderMesh>>,
    allocator: Res<MeshAllocator>,
    cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut stage: ResMut<StageEncoder<Prepared>>,
    recorder: Option<Res<DiagnosticsRecorder>>,
    plain: Res<PlainTilesDrawn>,
    mut records: ResMut<BookRecords>,
    mut state: Local<MoverDraw>,
) {
    let (Some(bake), Some(image), Some(book)) = (&atlas.bake, &atlas.image, &atlas.book) else {
        state.key = None;
        return;
    };
    if plain.0 != Some(Arc::as_ptr(bake).addr()) {
        return;
    }
    let (Some(target), Some(book_image)) = (images.get(image), images.get(book)) else { return };
    let key = (Arc::as_ptr(bake).addr(), image.id(), book.id());
    let state = &mut *state;
    if state.key != Some(key) {
        state.key = Some(key);
        state.held.clear();
        state.ranges.clear();
        state.grounding.clear();
        state.tile_grid.clear();
    }
    let height = book_image.texture_descriptor.size.height;
    if !movers.grounding.is_empty() && state.grounding != movers.grounding {
        write_rows(&queue, book_image, height.saturating_sub(CAPSULES_FROM_END), &movers.grounding);
        state.grounding.clone_from(&movers.grounding);
    }
    if !movers.tile_grid.is_empty() && state.tile_grid != movers.tile_grid {
        write_rows(&queue, book_image, height.saturating_sub(TILE_GRID_FROM_END), &movers.tile_grid);
        state.tile_grid.clone_from(&movers.tile_grid);
    }
    let tiles = &movers.tiles[..movers.tiles.len().min(MOST_MOVER_TILES)];
    state.next_ranges.clear();
    state.next_ranges.extend(lamp_ranges(tiles));
    for (lamp, texel) in changed_ranges(&state.ranges, &state.next_ranges) {
        records.set(bake, book.id(), HEADER_TEXELS + lamp as usize * RECORD_TEXELS + 5, texel);
    }
    records.write(&queue, book_image);
    std::mem::swap(&mut state.ranges, &mut state.next_ranges);
    if tiles.is_empty() {
        return;
    }
    let MoverDraw { layouts, pipelines, uniforms, joints, held, by_mover, bytes, row, .. } = state;
    // A tile to draw again that cannot be drawn yet must not pass for what its square held before.
    for again in tiles.iter().filter(|tile| tile.drawn == Drawn::Again).map(Held::of) {
        forget_under(held, &again);
    }
    let layouts = layouts.get_or_insert_with(|| caster_layouts(&cache));
    if let Some(clear) = cache.get_render_pipeline(layouts.clear) {
        let sources = Sources { meshes: &meshes, allocator: &allocator, cache: &cache };
        bytes.clear();
        by_mover.clear();
        let mut casts: Vec<CasterDraw> = Vec::new();
        let mut draws: Vec<(&MoverTile, Range<usize>)> = Vec::new();
        'tiles: for tile in tiles {
            if tile.drawn == Drawn::Kept && held.get(&(tile.x, tile.y)) == Some(&Held::of(tile)) {
                continue;
            }
            if by_mover.is_empty() {
                by_mover.extend(0..movers.casters.len());
                by_mover.sort_unstable_by_key(|&at| (movers.casters[at].mover, at));
            }
            let first = by_mover.partition_point(|&at| movers.casters[at].mover < tile.mover);
            let count = by_mover[first..].partition_point(|&at| movers.casters[at].mover == tile.mover);
            let start = casts.len();
            for caster in by_mover[first..first + count].iter().map(|&at| &movers.casters[at]) {
                match caster_draw(caster, &sources, layouts, pipelines) {
                    Ready::Draw(mut draw) => {
                        draw.uniform = bytes.len() as u32;
                        caster_uniform(bytes, tile, caster);
                        casts.push(draw);
                    }
                    Ready::Never => {}
                    Ready::Waiting => continue 'tiles,
                }
            }
            // Blend is MIN, so the order inside a tile does not change a texel; sorted, the pass skips repeated binds.
            casts[start..].sort_unstable_by_key(|draw| (draw.pipeline.id(), draw.vertices.buffer.id(), draw.indices.as_ref().map(|(indices, ..)| indices.buffer.id())));
            draws.push((tile, start..casts.len()));
        }
        if !draws.is_empty() {
            let caster_layout = cache.get_bind_group_layout(&layouts.caster);
            let (uniform_buffer, uniform_group) = kept(uniforms, &device, "mover tile casters", (bytes.len() as u64).max(CASTER_STRIDE), BufferUsages::UNIFORM, |buffer| {
                device.create_bind_group(None, &caster_layout, &BindGroupEntries::single(BufferBinding { buffer, offset: 0, size: NonZero::new(CASTER_SIZE) }))
            });
            queue.write_buffer(uniform_buffer, 0, bytes);
            let joint_group = if draws.iter().flat_map(|(_, range)| &casts[range.clone()]).any(|draw| draw.skin == Skin::Skinned) {
                let joint_layout = cache.get_bind_group_layout(&layouts.joints);
                bytes.clear();
                bytes.extend(movers.joints.iter().flat_map(|joint| joint.to_cols_array()).flat_map(f32::to_le_bytes));
                let (joint_buffer, group) = kept(joints, &device, "mover tile joints", (bytes.len() as u64).max(JOINT_SIZE), BufferUsages::STORAGE, |buffer| {
                    device.create_bind_group(None, &joint_layout, &BindGroupEntries::single(buffer.as_entire_binding()))
                });
                queue.write_buffer(joint_buffer, 0, bytes);
                Some(group)
            } else {
                None
            };
            record_timed(&device, &mut stage, recorder.as_deref(), |encoder| draw_squares(encoder, &device, target, clear, &draws, &casts, uniform_group, joint_group));
            for (tile, _) in &draws {
                let drawn = Held::of(tile);
                forget_under(held, &drawn);
                held.insert((drawn.x, drawn.y), drawn);
            }
        }
    }
    row.clear();
    row.extend(tiles.iter().flat_map(|tile| {
        let mut texels = tile.texels();
        if held.get(&(tile.x, tile.y)) != Some(&Held::of(tile)) {
            texels[1][3] = 0.0;
        }
        texels
    }));
    write_rows(&queue, book_image, height.saturating_sub(TILES_FROM_END), row);
}

#[cfg(test)]
mod tests {
    use bevy::math::{Mat4, Vec3};

    use super::*;

    fn tile(lamp: u32) -> MoverTile {
        MoverTile { mover: 0, lamp, lamp_at: Vec3::ZERO, lamp_radius: 100.0, axis: Vec3::Z, tan_half: 0.5, near: 0.0, x: 0, y: 0, size: 64, fade: 1.0, drawn: Drawn::Again }
    }

    #[test]
    fn a_casters_uniform_holds_the_tiles_view_its_first_joint_and_its_placing() {
        let tile = MoverTile { lamp_at: Vec3::new(1.0, 2.0, 3.0), lamp_radius: 108.0, ..tile(0) };
        let caster = MoverCaster { mesh: AssetId::default(), world_from_mesh: Mat4::from_translation(Vec3::new(4.0, 5.0, 6.0)), joints: Some(7..9), mover: 0 };
        let mut bytes = Vec::new();
        caster_uniform(&mut bytes, &tile, &caster);
        assert_eq!(bytes.len(), CASTER_STRIDE as usize);
        let floats: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|chunk| f32::from_le_bytes(*chunk)).collect();
        let expected = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 2.0, 3.0, 1.0 / 108.0, 1.8125, 1.08, -8.64, 7.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0,
            4.0, 5.0, 6.0, 1.0,
        ];
        assert_eq!(expected.len() * 4, CASTER_SIZE as usize);
        for (at, (got, want)) in floats.iter().zip(expected).enumerate() {
            assert!((got - want).abs() < 1e-5, "float {at}: {got} against {want}");
        }
        assert!(floats[expected.len()..].iter().all(|&value| value == 0.0));
    }

    #[test]
    fn each_lamp_gets_the_run_of_tiles_it_owns_in_the_row() {
        let tiles = [2, 2, 5, 7, 7, 7].map(tile);
        let runs: Vec<(u32, u32, u32)> = lamp_ranges(&tiles).map(|range| (range.lamp, range.first, range.count)).collect();
        assert_eq!(runs, [(2, 0, 2), (5, 2, 1), (7, 3, 3)]);
    }

    #[test]
    fn only_lamps_whose_run_changed_are_written_and_a_lamp_that_lost_its_tiles_is_zeroed() {
        let before: Vec<LampRange> = lamp_ranges(&[2, 2, 5, 7, 7, 7].map(tile)).collect();
        let now: Vec<LampRange> = lamp_ranges(&[2, 2, 7, 7, 7, 7].map(tile)).collect();
        assert_eq!(changed_ranges(&before, &now).collect::<Vec<_>>(), [(5, [0.0; 4]), (7, [2.0, 4.0, 0.0, 0.0])]);
        assert!(changed_ranges(&now, &now).next().is_none());
    }

    #[test]
    fn when_the_tiles_go_from_some_to_none_every_lamp_that_had_tiles_is_zeroed() {
        let before: Vec<LampRange> = lamp_ranges(&[2, 5, 5].map(tile)).collect();
        assert_eq!(changed_ranges(&before, &[]).collect::<Vec<_>>(), [(2, [0.0; 4]), (5, [0.0; 4])]);
    }

    #[test]
    fn a_square_overlaps_the_squares_it_shares_texels_with_and_not_those_it_only_touches() {
        let square = |x: u32, y: u32, size: u32| Held { x, y, size, lamp: 0 };
        assert!(square(0, 0, 64).overlaps(&square(32, 32, 64)));
        assert!(square(0, 0, 128).overlaps(&square(64, 64, 32)));
        assert!(!square(0, 0, 64).overlaps(&square(64, 0, 64)));
        assert!(!square(0, 0, 64).overlaps(&square(0, 64, 32)));
    }

    #[test]
    fn a_drawn_square_forgets_the_held_squares_inside_it_and_around_it_and_no_others() {
        let square = |x: u32, y: u32, size: u32| Held { x, y, size, lamp: 0 };
        let mut held: FxHashMap<(u32, u32), Held> = [square(0, 0, 512), square(512, 0, 64), square(640, 0, 128), square(768, 128, 64), square(512, 256, 64)].into_iter().map(|held| ((held.x, held.y), held)).collect();
        forget_under(&mut held, &square(64, 64, 64));
        forget_under(&mut held, &square(512, 0, 256));
        let mut left: Vec<(u32, u32)> = held.into_keys().collect();
        left.sort_unstable();
        assert_eq!(left, [(512, 256), (768, 128)]);
    }
}
