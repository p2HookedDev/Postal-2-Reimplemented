use std::num::NonZero;
use std::ops::Range;
use std::sync::Arc;

use bevy::asset::{AssetId, Assets, Handle, RenderAssetUsages};
use bevy::ecs::prelude::{Local, Res, ResMut, Resource};
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::mesh::VertexBufferLayout;
use bevy::render::diagnostic::{DiagnosticsRecorder, RecordDiagnostics};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer_sized};
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, Buffer, BufferBinding, BufferInitDescriptor, BufferUsages, CachedRenderPipelineId,
    CommandEncoder, Extent3d, IndexFormat, LoadOp, Operations, Origin3d, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline,
    SamplerBindingType, ShaderStages, StoreOp, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect, TextureDimension, TextureFormat, TextureSampleType, TextureUsages,
    VertexAttribute, VertexFormat, VertexStepMode,
};
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::texture::GpuImage;
use bevy::shader::Shader;
use p2_assets::{Terrain, fan_triangles, poly_flags};
use p2_math::Vector;
use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};

use super::{LAMP_SHADOW_SPAN, LampAtlas};
use super::cells::{BOOK_WIDTH, HEADER_TEXELS, RECORD_TEXELS};
use super::faces::{DETAIL_UNIT, FaceSlot, atlas_height};
use super::mover_pass::{NEAREST, pipeline, view_rows};
use crate::gpu::stage_encoder::{Prepared, StageEncoder};
use crate::world::level_collision::{GREY_ALBEDO, MaskedCaster, StaticCollision};

pub(super) const ATLAS_SHADER: Handle<Shader> = crate::gpu::shaders::shader_handle("lamp_atlas.wesl");

/// The atlas side in packing units; a tile's corner is stored in 8 bits an axis. `lamp_shadow.wesl` holds the
/// same number.
pub(super) const ATLAS_UNITS: u32 = 128;
const ATLAS_ROWS: u32 = ATLAS_UNITS * 3 / 4;
const TOP_LEVEL: u32 = 3;
const UNMAPPED_LEVEL: u32 = 7;

pub(super) const UNMAPPED: u32 = UNMAPPED_LEVEL << 16;

pub(super) const NEAR: f32 = 8.0;
pub(super) const GUARD_TEXELS: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Tile {
    x: u32,
    y: u32,
    level: u32,
}

impl Tile {
    fn size(self, unit: u32) -> u32 {
        unit << self.level
    }

    fn packed(self) -> u32 {
        self.x | (self.y << 8) | (self.level << 16)
    }
}

fn wanted_level(radius: f32) -> u32 {
    if radius <= 256.0 {
        1
    } else if radius <= 1024.0 {
        2
    } else {
        TOP_LEVEL
    }
}

fn shelves(levels: &[u32]) -> Vec<Option<Tile>> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    order.sort_by_key(|&index| std::cmp::Reverse(levels[index]));
    let mut tiles = vec![None; levels.len()];
    let (mut x, mut y, mut shelf) = (0, 0, 0);
    for index in order {
        let level = levels[index];
        let (width, height) = (3 << level, 2 << level);
        if x + width > ATLAS_UNITS {
            x = 0;
            y += shelf;
            shelf = 0;
        }
        if y + height > ATLAS_ROWS {
            continue;
        }
        tiles[index] = Some(Tile { x, y, level });
        x += width;
        shelf = shelf.max(height);
    }
    tiles
}

fn pack(radii: &[f32]) -> Vec<Option<Tile>> {
    let mut tiles = Vec::new();
    for shrink in 0..=TOP_LEVEL {
        let levels: Vec<u32> = radii.iter().map(|&radius| wanted_level(radius).saturating_sub(shrink)).collect();
        tiles = shelves(&levels);
        if tiles.iter().all(Option::is_some) {
            break;
        }
    }
    tiles
}

pub(crate) const OPEN_POLYGONS: u32 = poly_flags::INVISIBLE | poly_flags::PORTAL | poly_flags::FAKE_BACKDROP | poly_flags::MASKED | poly_flags::TRANSLUCENT;

pub(crate) fn terrain_triangles(terrain: &Terrain) -> impl Iterator<Item = [[f32; 3]; 3]> + '_ {
    let (width, height) = (terrain.width, terrain.height);
    let ready = terrain.vertices.len() == width * height;
    (0..height.saturating_sub(1)).filter(move |_| ready).flat_map(move |y| {
        (0..width.saturating_sub(1)).flat_map(move |x| {
            let at = |x: usize, y: usize| terrain.vertices[y * width + x];
            [[at(x, y), at(x + 1, y), at(x + 1, y + 1)], [at(x, y), at(x + 1, y + 1), at(x, y + 1)]]
        })
    })
}

fn point_bounds<const N: usize>(points: impl IntoIterator<Item = [f32; N]>) -> ([f32; 3], [f32; 3]) {
    points.into_iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(low, high), point| ([0, 1, 2].map(|a| low[a].min(point[a])), [0, 1, 2].map(|a| high[a].max(point[a]))))
}

fn box_near_sphere((low, high): ([f32; 3], [f32; 3]), center: [f32; 3], radius: f32) -> bool {
    let gap: f32 = (0..3).map(|axis| (low[axis] - center[axis]).max(center[axis] - high[axis]).max(0.0).powi(2)).sum();
    gap <= radius * radius
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct LampDraw {
    position: [f32; 3],
    radius: f32,
    tile: Tile,
    first: u32,
    count: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct MaskedRange {
    lamp: usize,
    group: usize,
    first: u32,
    count: u32,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct LampBake {
    #[serde(skip)]
    pub(super) vertices: Vec<[f32; 3]>,
    #[serde(with = "whole_triangles")]
    indices: Vec<u32>,
    draws: Vec<LampDraw>,
    draw_index: Vec<Option<usize>>,
    tiles: Vec<Option<Tile>>,
    #[serde(skip)]
    masked_vertices: Vec<[f32; 6]>,
    #[serde(skip)]
    masked_indices: Vec<u32>,
    masked_ranges: Vec<MaskedRange>,
    #[serde(with = "differences")]
    masked_triangles: Vec<u32>,
    #[serde(skip)]
    masked_images: Vec<Handle<Image>>,
    pub(super) records: Vec<[f32; 4]>,
}

impl LampBake {
    pub(super) fn restore(&mut self, triangles: &[[[f32; 3]; 3]], masked: &[MaskedCaster]) {
        self.vertices = triangles.as_flattened().to_vec();
        self.masked_images = masked.iter().map(|caster| caster.image.clone()).collect();
        let mut taken = self.masked_triangles.iter();
        self.masked_vertices.clear();
        for range in &self.masked_ranges {
            let Some(caster) = masked.get(range.group) else { continue };
            for &triangle in taken.by_ref().take(range.count as usize / 3) {
                let corners = caster.triangles.get(triangle as usize).into_iter().flatten();
                self.masked_vertices.extend(corners.map(|&[x, y, z, u, v]| [x, y, z, u, v, caster.cutoff]));
            }
        }
        self.masked_indices = (0..self.masked_vertices.len() as u32).collect();
    }

    pub(super) fn keep_record_rows(&mut self, book: &[[f32; 4]]) {
        let rows = (HEADER_TEXELS + self.tiles.len() * RECORD_TEXELS).div_ceil(BOOK_WIDTH);
        self.records = book.iter().copied().chain(std::iter::repeat([0.0; 4])).take(rows * BOOK_WIDTH).collect();
    }

    pub(super) fn tile_bits(&self, lamp: usize) -> u32 {
        self.tiles.get(lamp).copied().flatten().map_or(UNMAPPED, Tile::packed)
    }

    pub(super) fn atlas_height(&self, side: u32) -> u32 {
        let rows = self.tiles.iter().flatten().map(|tile| tile.y + (2 << tile.level)).max().unwrap_or(0);
        atlas_height(side, rows * (side / ATLAS_UNITS))
    }

    pub(super) fn plain_face(&self, lamp: usize, unit: u32) -> Option<u32> {
        Some(self.tiles.get(lamp)?.as_ref()?.size(unit))
    }

    pub(super) fn drawn(&self, unit: u32) -> impl Iterator<Item = Option<(u32, &[u32])>> {
        let mut draws = self.draws.iter();
        self.tiles.iter().map(move |tile| tile.and_then(|_| draws.next()).map(|draw| (draw.tile.size(unit), &self.indices[draw.first as usize..(draw.first + draw.count) as usize])))
    }
}

mod differences {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(values: &[u32], serializer: S) -> Result<S::Ok, S::Error> {
        let mut last = 0;
        serializer.collect_seq(values.iter().map(|&value| {
            let step = i64::from(value) - last;
            last = i64::from(value);
            step
        }))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u32>, D::Error> {
        let mut last = 0i64;
        Vec::<i64>::deserialize(deserializer)?
            .into_iter()
            .map(|step| {
                last += step;
                u32::try_from(last).map_err(D::Error::custom)
            })
            .collect()
    }
}

mod whole_triangles {
    use serde::ser::Error as _;
    use serde::{Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(indices: &[u32], serializer: S) -> Result<S::Ok, S::Error> {
        let triangles = indices
            .chunks(3)
            .map(|corners| match *corners {
                [a, b, c] if a % 3 == 0 && b == a + 1 && c == a + 2 => Ok(a / 3),
                _ => Err(S::Error::custom("lamp indices that are not whole triangles")),
            })
            .collect::<Result<Vec<u32>, _>>()?;
        super::differences::serialize(&triangles, serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u32>, D::Error> {
        Ok(super::differences::deserialize(deserializer)?.into_iter().flat_map(|triangle| [3 * triangle, 3 * triangle + 1, 3 * triangle + 2]).collect())
    }
}

fn draw_index(tiles: &[Option<Tile>]) -> Vec<Option<usize>> {
    let mut next = 0;
    tiles.iter().map(|tile| tile.map(|_| { next += 1; next - 1 })).collect()
}

pub(super) fn lamp_bake(triangles: &[[[f32; 3]; 3]], masked: &[MaskedCaster], lamps: &[([f32; 3], f32)]) -> LampBake {
    let radii: Vec<f32> = lamps.iter().map(|&(_, radius)| radius).collect();
    let tiles = pack(&radii);
    let tree = p2_collision::TriangleTree::new(triangles);
    let caster_bounds: Vec<_> = masked.iter().map(|caster| point_bounds(caster.triangles.iter().flatten().copied())).collect();
    let near = super::on_threads(lamps.len(), |lamp| {
        let (position, radius) = lamps[lamp];
        tiles[lamp]?;
        let mut found = Vec::new();
        tree.any(position.map(|c| c - radius).into(), position.map(|c| c + radius).into(), |index| {
            if box_near_sphere(point_bounds(triangles[index]), position, radius) {
                found.push(index as u32);
            }
            false
        });
        found.sort_unstable();
        let own: Vec<u32> = found.into_iter().flat_map(|index| [0, 1, 2].map(|corner| index * 3 + corner)).collect();
        let groups: Vec<(usize, Vec<u32>)> = masked
            .iter()
            .zip(&caster_bounds)
            .enumerate()
            .filter(|(_, (_, reach))| box_near_sphere(**reach, position, radius))
            .map(|(group, (caster, _))| {
                let near = caster.triangles.iter().enumerate().filter(|(_, triangle)| box_near_sphere(point_bounds(triangle.iter().copied()), position, radius));
                (group, near.map(|(index, _)| index as u32).collect::<Vec<_>>())
            })
            .filter(|(_, near)| !near.is_empty())
            .collect();
        Some((own, groups))
    });
    let mut indices = Vec::new();
    let mut draws = Vec::new();
    let (mut masked_triangles, mut masked_ranges) = (Vec::new(), Vec::new());
    for ((&(position, radius), tile), near) in lamps.iter().zip(&tiles).zip(near) {
        let (Some(tile), Some((own, groups))) = (*tile, near) else { continue };
        let lamp = draws.len();
        draws.push(LampDraw { position, radius, tile, first: indices.len() as u32, count: own.len() as u32 });
        indices.extend(own);
        for (group, near) in groups {
            let (first, count) = (masked_triangles.len() as u32 * 3, near.len() as u32 * 3);
            masked_triangles.extend(near);
            masked_ranges.push(MaskedRange { lamp, group, first, count });
        }
    }
    let mut bake = LampBake {
        vertices: Vec::new(),
        indices,
        draws,
        draw_index: draw_index(&tiles),
        tiles,
        masked_vertices: Vec::new(),
        masked_indices: Vec::new(),
        masked_ranges,
        masked_triangles,
        masked_images: Vec::new(),
        records: Vec::new(),
    };
    bake.restore(triangles, masked);
    bake
}

pub(crate) struct WorldTriangles {
    pub(crate) triangles: Vec<[[f32; 3]; 3]>,
    pub(crate) albedos: Vec<[f32; 3]>,
    pub(crate) terrain_from: usize,
}

pub(crate) fn world_triangles(collision: &StaticCollision, origin: Vec3) -> WorldTriangles {
    let moved = |point: [f32; 3]| -> [f32; 3] { (Vec3::from(p2_bevy_assets::to_bevy(Vector::from(point))) + origin).into() };
    let albedos = &collision.albedos;
    let known = |found: Option<&[f32; 3]>| found.copied().unwrap_or(GREY_ALBEDO);
    let bsp = collision.model.polygons().into_iter().filter(|polygon| polygon.poly_flags & OPEN_POLYGONS == 0).flat_map(|polygon| {
        let albedo = known(albedos.bsp.get(&polygon.material));
        fan_triangles(polygon.positions.len()).map(|[a, b, c]| ([polygon.positions[a], polygon.positions[b], polygon.positions[c]], albedo)).collect::<Vec<_>>()
    });
    let mut walked: Vec<([[f32; 3]; 3], [f32; 3])> = bsp.chain(collision.lamp_casters.iter().enumerate().map(|(index, triangle)| (*triangle, known(albedos.casters.get(index))))).collect();
    let terrain_from = walked.len();
    walked.extend(collision.terrains.iter().enumerate().flat_map(|(index, terrain)| terrain_triangles(terrain).map(move |triangle| (triangle, known(albedos.terrains.get(index))))));
    let (triangles, albedos): (Vec<_>, Vec<_>) = walked.into_iter().map(|(triangle, albedo)| (triangle.map(moved), albedo)).unzip();
    WorldTriangles { triangles, albedos, terrain_from }
}

pub(super) fn world_masked(collision: &StaticCollision, origin: Vec3) -> Vec<MaskedCaster> {
    let [ox, oy, oz] = origin.to_array();
    let moved = |corner: [f32; 5]| [corner[0] + ox, corner[1] + oy, corner[2] + oz, corner[3], corner[4]];
    collision
        .lamp_masked
        .iter()
        .map(|caster| MaskedCaster {
            image: caster.image.clone(),
            cutoff: caster.cutoff,
            triangles: caster.triangles.iter().map(|triangle| triangle.map(moved)).collect(),
        })
        .collect()
}

pub(crate) fn new_atlas(images: &mut Assets<Image>, [width, height]: [u32; 2], format: TextureFormat) -> Handle<Image> {
    let mut image = Image::new_uninit(Extent3d { width, height, depth_or_array_layers: 1 }, TextureDimension::D2, format, RenderAssetUsages::RENDER_WORLD);
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_DST;
    images.add(image)
}

const FACE_STRIDE: u64 = 256;

/// World axes of a cube face's camera x, y and z. `lamp_shadow.wesl` picks the same face and axes.
pub(super) const FACE_AXES: [[[f32; 3]; 3]; 6] = [
    [[0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
    [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]],
    [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]],
    [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]],
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]],
];

fn face_uniform(bytes: &mut Vec<u8>, draw: &LampDraw, axes: [[f32; 3]; 3], size: u32) {
    let start = bytes.len();
    bytes.extend(view_rows(axes, draw.position, draw.radius, size, 1.0, 0.0).iter().flatten().flat_map(|value| value.to_le_bytes()));
    bytes.resize(start + FACE_STRIDE as usize, 0);
}

fn plain_uniforms(draws: &[LampDraw], unit: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(draws.len() * 6 * FACE_STRIDE as usize);
    for draw in draws {
        for axes in FACE_AXES {
            face_uniform(&mut bytes, draw, axes, draw.tile.size(unit));
        }
    }
    bytes
}

fn face_corner(tile: Tile, face: u32, unit: u32) -> (u32, u32) {
    let size = tile.size(unit);
    (tile.x * unit + (face % 3) * size, tile.y * unit + (face / 3) * size)
}

pub(super) struct AtlasPipelines {
    faces: BindGroupLayoutDescriptor,
    solid: CachedRenderPipelineId,
    image: BindGroupLayoutDescriptor,
    masked: CachedRenderPipelineId,
    clear: CachedRenderPipelineId,
}

struct AtlasBuffers {
    faces: BindGroup,
    vertices: Buffer,
    indices: Buffer,
    masked: Option<(Buffer, Buffer)>,
    image_groups: Vec<Option<BindGroup>>,
}

#[derive(Resource, Default)]
pub(super) struct PlainTilesDrawn(pub(super) Option<usize>);

#[derive(Default)]
pub(super) struct AtlasDraw {
    pipelines: Option<AtlasPipelines>,
    drawn: Option<(usize, AssetId<Image>)>,
    waited: u32,
    buffers: Option<AtlasBuffers>,
    detail: Vec<FaceSlot>,
}

fn atlas_pipelines(cache: &PipelineCache) -> AtlasPipelines {
    let faces = BindGroupLayoutDescriptor::new(
        "lamp_atlas",
        &BindGroupLayoutEntries::single(ShaderStages::VERTEX_FRAGMENT, uniform_buffer_sized(true, NonZero::new(80))),
    );
    let image = BindGroupLayoutDescriptor::new(
        "lamp_atlas_image",
        &BindGroupLayoutEntries::sequential(ShaderStages::FRAGMENT, (texture_2d(TextureSampleType::Float { filterable: true }), sampler(SamplerBindingType::Filtering))),
    );
    let attribute = |format: VertexFormat, offset: u64, shader_location: u32| VertexAttribute { format, offset, shader_location };
    let solid = pipeline(
        cache,
        ATLAS_SHADER,
        "lamp atlas",
        vec![faces.clone()],
        ["vertex", "fragment"],
        vec![VertexBufferLayout { array_stride: 12, step_mode: VertexStepMode::Vertex, attributes: vec![attribute(VertexFormat::Float32x3, 0, 0)] }],
        Some(NEAREST),
    );
    let masked = pipeline(
        cache,
        ATLAS_SHADER,
        "lamp atlas masked",
        vec![faces.clone(), image.clone()],
        ["masked_vertex", "masked_fragment"],
        vec![VertexBufferLayout {
            array_stride: 24,
            step_mode: VertexStepMode::Vertex,
            attributes: vec![attribute(VertexFormat::Float32x3, 0, 0), attribute(VertexFormat::Float32x2, 12, 1), attribute(VertexFormat::Float32, 20, 2)],
        }],
        Some(NEAREST),
    );
    let clear = pipeline(cache, ATLAS_SHADER, "lamp atlas clear", Vec::new(), ["clear_vertex", "clear_fragment"], Vec::new(), None);
    AtlasPipelines { faces, solid, image, masked, clear }
}

const ATLAS_WAIT_FRAMES: u32 = 240;

pub(super) fn bytes_of<const N: usize>(values: &[[f32; N]]) -> Vec<u8> {
    values.as_flattened().iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn index_bytes(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

struct FaceView {
    uniform: u32,
    x: u32,
    y: u32,
    size: u32,
}

enum Before {
    Keep,
    Whiten,
}

struct TileJob<'a> {
    lamp: usize,
    faces: &'a BindGroup,
    views: Vec<FaceView>,
    before: Before,
    range: Range<u32>,
}

struct Ready<'a> {
    solid: &'a RenderPipeline,
    masked: &'a RenderPipeline,
    clear: &'a RenderPipeline,
    target: &'a GpuImage,
    buffers: &'a AtlasBuffers,
}

pub(super) fn record_timed(device: &RenderDevice, stage: &mut StageEncoder<Prepared>, recorder: Option<&DiagnosticsRecorder>, encode: impl FnOnce(&mut CommandEncoder)) {
    let encoder = stage.encoder(device);
    let span = recorder.time_span(encoder, LAMP_SHADOW_SPAN);
    encode(encoder);
    span.end(encoder);
}

fn draw_tiles(encoder: &mut CommandEncoder, ready: &Ready, bake: &LampBake, load: LoadOp<wgpu_types::Color>, jobs: &[TileJob]) {
    let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("lamp atlas"),
        color_attachments: &[Some(RenderPassColorAttachment { view: &ready.target.texture_view, depth_slice: None, resolve_target: None, ops: Operations { load, store: StoreOp::Store } })],
        ..Default::default()
    });
    for job in jobs {
        if matches!(job.before, Before::Whiten) {
            pass.set_pipeline(ready.clear);
            for view in &job.views {
                pass.set_viewport(view.x as f32, view.y as f32, view.size as f32, view.size as f32, 0.0, 1.0);
                pass.set_scissor_rect(view.x, view.y, view.size, view.size);
                pass.draw(0..3, 0..1);
            }
        }
        let solid = (!job.range.is_empty()).then(|| (None, job.range.clone()));
        let masked = ready.buffers.masked.iter().flat_map(|_| &bake.masked_ranges).filter(|range| range.lamp == job.lamp).map(|range| (Some(range.group), range.first..range.first + range.count));
        for (group, range) in solid.into_iter().chain(masked) {
            match (group, &ready.buffers.masked) {
                (Some(group), Some((vertices, indices))) => {
                    let Some(image_group) = &ready.buffers.image_groups[group] else { continue };
                    pass.set_pipeline(ready.masked);
                    pass.set_vertex_buffer(0, *vertices.slice(..));
                    pass.set_index_buffer(*indices.slice(..), IndexFormat::Uint32);
                    pass.set_bind_group(1, image_group, &[]);
                }
                _ => {
                    pass.set_pipeline(ready.solid);
                    pass.set_vertex_buffer(0, *ready.buffers.vertices.slice(..));
                    pass.set_index_buffer(*ready.buffers.indices.slice(..), IndexFormat::Uint32);
                }
            }
            for view in &job.views {
                pass.set_viewport(view.x as f32, view.y as f32, view.size as f32, view.size as f32, 0.0, 1.0);
                pass.set_scissor_rect(view.x, view.y, view.size, view.size);
                pass.set_bind_group(0, job.faces, &[view.uniform]);
                pass.draw_indexed(range.clone(), 0, 0..1);
            }
        }
    }
}

pub(super) fn write_rows(queue: &RenderQueue, book: &GpuImage, row: u32, texels: &[[f32; 4]]) {
    let whole = texels.len() / BOOK_WIDTH;
    let (full, rest) = texels.split_at(whole * BOOK_WIDTH);
    for (row, texels, rows) in [(row, full, whole), (row + whole as u32, rest, 1)] {
        if texels.is_empty() {
            continue;
        }
        queue.write_texture(
            TexelCopyTextureInfo { texture: &book.texture, mip_level: 0, origin: Origin3d { x: 0, y: row, z: 0 }, aspect: TextureAspect::All },
            &bytes_of(texels),
            TexelCopyBufferLayout { offset: 0, bytes_per_row: Some((texels.len() / rows * 16) as u32), rows_per_image: None },
            Extent3d { width: (texels.len() / rows) as u32, height: rows as u32, depth_or_array_layers: 1 },
        );
    }
}

#[derive(Resource, Default)]
pub(super) struct BookRecords {
    key: Option<(usize, AssetId<Image>)>,
    texels: Vec<[f32; 4]>,
    dirty: Option<Range<usize>>,
}

impl BookRecords {
    pub(super) fn set(&mut self, bake: &Arc<LampBake>, book: AssetId<Image>, index: usize, texel: [f32; 4]) {
        let key = (Arc::as_ptr(bake).addr(), book);
        if self.key != Some(key) {
            self.key = Some(key);
            self.texels.clone_from(&bake.records);
            self.dirty = None;
        }
        let Some(held) = self.texels.get_mut(index) else { return };
        if held.map(f32::to_bits) == texel.map(f32::to_bits) {
            return;
        }
        *held = texel;
        self.dirty = Some(self.dirty.take().map_or(index..index + 1, |dirty| dirty.start.min(index)..dirty.end.max(index + 1)));
    }

    pub(super) fn write(&mut self, queue: &RenderQueue, book: &GpuImage) {
        let Some(dirty) = self.dirty.take() else { return };
        let rows = dirty.start / BOOK_WIDTH..(dirty.end - 1) / BOOK_WIDTH + 1;
        write_rows(queue, book, rows.start as u32, &self.texels[rows.start * BOOK_WIDTH..rows.end * BOOK_WIDTH]);
    }
}

fn set_faces(records: &mut BookRecords, bake: &Arc<LampBake>, book: AssetId<Image>, lamp: u32, faces: &[FaceSlot]) {
    let mut packed = [[0.0f32; 4]; 2];
    for slot in faces.iter().filter(|slot| slot.lamp == lamp) {
        packed[usize::from(slot.face) / 4][usize::from(slot.face) % 4] = slot.packed();
    }
    for (at, texel) in packed.into_iter().enumerate() {
        records.set(bake, book, HEADER_TEXELS + lamp as usize * RECORD_TEXELS + 3 + at, texel);
    }
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn draw_lamp_atlas(
    atlas: Res<LampAtlas>,
    images: Res<RenderAssets<GpuImage>>,
    cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut stage: ResMut<StageEncoder<Prepared>>,
    recorder: Option<Res<DiagnosticsRecorder>>,
    mut plain: ResMut<PlainTilesDrawn>,
    mut records: ResMut<BookRecords>,
    mut state: Local<AtlasDraw>,
) {
    let state = &mut *state;
    let (Some(bake), Some(image)) = (&atlas.bake, &atlas.image) else {
        state.drawn = None;
        plain.0 = None;
        return;
    };
    let key = (Arc::as_ptr(bake).addr(), image.id());
    let unit = atlas.side / ATLAS_UNITS;
    let pipelines = state.pipelines.get_or_insert_with(|| atlas_pipelines(&cache));
    let (Some(solid), Some(masked), Some(clear), Some(target)) =
        (cache.get_render_pipeline(pipelines.solid), cache.get_render_pipeline(pipelines.masked), cache.get_render_pipeline(pipelines.clear), images.get(image))
    else {
        return;
    };
    let image_group = |image: &Handle<Image>| -> Option<BindGroup> {
        let image = images.get(image)?;
        Some(device.create_bind_group(None, &cache.get_bind_group_layout(&pipelines.image), &BindGroupEntries::sequential((&image.texture_view, &image.sampler))))
    };
    let index_buffer = |indices: &[u32]| device.create_buffer_with_data(&BufferInitDescriptor { label: Some("lamp atlas indices"), contents: &index_bytes(indices), usage: BufferUsages::INDEX });
    let face_layout = cache.get_bind_group_layout(&pipelines.faces);
    let faces = |label: &'static str, bytes: Vec<u8>| {
        let buffer = device.create_buffer_with_data(&BufferInitDescriptor { label: Some(label), contents: if bytes.is_empty() { &[0; FACE_STRIDE as usize] } else { &bytes }, usage: BufferUsages::UNIFORM });
        device.create_bind_group(None, &face_layout, &BindGroupEntries::single(BufferBinding { buffer: &buffer, offset: 0, size: NonZero::new(80) }))
    };
    if state.drawn != Some(key) {
        state.waited += 1;
        if state.waited < ATLAS_WAIT_FRAMES && bake.masked_images.iter().any(|image| images.get(image).is_none()) {
            return;
        }
        state.waited = 0;
        let vertex_buffer = |label: &'static str, bytes: Vec<u8>| device.create_buffer_with_data(&BufferInitDescriptor { label: Some(label), contents: &bytes, usage: BufferUsages::VERTEX });
        let buffers = state.buffers.insert(AtlasBuffers {
            faces: faces("lamp atlas faces", plain_uniforms(&bake.draws, unit)),
            vertices: vertex_buffer("lamp atlas vertices", bytes_of(&bake.vertices)),
            indices: index_buffer(&bake.indices),
            masked: (!bake.masked_indices.is_empty()).then(|| (vertex_buffer("lamp atlas masked vertices", bytes_of(&bake.masked_vertices)), index_buffer(&bake.masked_indices))),
            image_groups: bake.masked_images.iter().map(&image_group).collect(),
        });
        let jobs: Vec<TileJob> = bake
            .draws
            .iter()
            .enumerate()
            .map(|(lamp, draw)| TileJob {
                lamp,
                faces: &buffers.faces,
                views: (0..6)
                    .map(|face| {
                        let (x, y) = face_corner(draw.tile, face, unit);
                        FaceView { uniform: ((lamp as u64 * 6 + u64::from(face)) * FACE_STRIDE) as u32, x, y, size: draw.tile.size(unit) }
                    })
                    .collect(),
                before: Before::Keep,
                range: draw.first..draw.first + draw.count,
            })
            .collect();
        let ready = Ready { solid, masked, clear, target, buffers };
        record_timed(&device, &mut stage, recorder.as_deref(), |encoder| draw_tiles(encoder, &ready, bake, LoadOp::Clear(wgpu_types::Color::WHITE), &jobs));
        state.drawn = Some(key);
        plain.0 = Some(key.0);
        state.detail.clear();
        return;
    }
    if state.detail == atlas.faces {
        return;
    }
    let (Some(buffers), Some(book)) = (state.buffers.as_mut(), atlas.book.as_ref().and_then(|book| images.get(book))) else { return };
    let slot_key = |slot: &FaceSlot| (slot.lamp, slot.face, slot.x, slot.y, slot.level);
    let (drawn, wanted): (FxHashSet<_>, FxHashSet<_>) = (state.detail.iter().map(slot_key).collect(), atlas.faces.iter().map(slot_key).collect());
    let mut fresh: Vec<(usize, FaceSlot)> = atlas.faces.iter().filter(|&slot| !drawn.contains(&slot_key(slot))).filter_map(|&slot| Some((bake.draw_index.get(slot.lamp as usize).copied().flatten()?, slot))).collect();
    let mut changed: Vec<u32> = state.detail.iter().filter(|&slot| !wanted.contains(&slot_key(slot))).chain(fresh.iter().map(|(_, slot)| slot)).map(|slot| slot.lamp).collect();
    changed.sort_unstable();
    changed.dedup();
    fresh.sort_by_key(|&(draw, _)| draw);
    let mut uniforms = Vec::with_capacity(fresh.len() * FACE_STRIDE as usize);
    for &(draw, slot) in &fresh {
        face_uniform(&mut uniforms, &bake.draws[draw], FACE_AXES[usize::from(slot.face)], slot.side());
    }
    let group = faces("lamp atlas detail faces", uniforms);
    let mut first = 0;
    let jobs: Vec<TileJob> = fresh
        .chunk_by(|a, b| a.0 == b.0)
        .map(|run| {
            let (lamp, at) = (run[0].0, first);
            first += run.len();
            TileJob {
                lamp,
                faces: &group,
                views: run
                    .iter()
                    .zip(at..)
                    .map(|((_, slot), at)| FaceView { uniform: (at as u64 * FACE_STRIDE) as u32, x: u32::from(slot.x) * DETAIL_UNIT, y: u32::from(slot.y) * DETAIL_UNIT, size: slot.side() })
                    .collect(),
                before: Before::Whiten,
                range: bake.draws[lamp].first..bake.draws[lamp].first + bake.draws[lamp].count,
            }
        })
        .collect();
    if !jobs.is_empty() {
        for (made, image) in buffers.image_groups.iter_mut().zip(&bake.masked_images).filter(|(made, _)| made.is_none()) {
            *made = image_group(image);
        }
        let ready = Ready { solid, masked, clear, target, buffers };
        record_timed(&device, &mut stage, recorder.as_deref(), |encoder| draw_tiles(encoder, &ready, bake, LoadOp::Load, &jobs));
    }
    if let Some(book_id) = atlas.book.as_ref().map(Handle::id) {
        for lamp in changed {
            set_faces(&mut records, bake, book_id, lamp, &atlas.faces);
        }
        records.write(&queue, book);
    }
    state.detail.clone_from(&atlas.faces);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::shaders::shader_const;

    #[test]
    fn the_shader_reads_the_atlas_as_it_is_drawn() {
        let shader = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/shaders/lamp_shadow.wesl"));
        assert_eq!(shader_const(shader, "ATLAS_UNITS"), f64::from(ATLAS_UNITS));
        assert_eq!(shader_const(shader, "ATLAS_GUARD") as f32, GUARD_TEXELS);
        assert_eq!(shader_const(shader, "ATLAS_UNMAPPED"), f64::from(UNMAPPED_LEVEL));
    }

    #[test]
    fn tiles_never_overlap_and_stay_in_the_atlas() {
        let radii: Vec<f32> = (0..300).map(|index| 100.0 + index as f32 * 10.0).collect();
        let tiles: Vec<Tile> = pack(&radii).into_iter().flatten().collect();
        assert_eq!(tiles.len(), radii.len());
        let span = |tile: &Tile| (tile.x, tile.y, tile.x + (3 << tile.level), tile.y + (2 << tile.level));
        for (index, a) in tiles.iter().enumerate() {
            let (ax, ay, ax2, ay2) = span(a);
            assert!(ax2 <= ATLAS_UNITS && ay2 <= ATLAS_ROWS);
            for b in &tiles[index + 1..] {
                let (bx, by, bx2, by2) = span(b);
                assert!(ax2 <= bx || bx2 <= ax || ay2 <= by || by2 <= ay, "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn too_many_lamps_shrink_before_any_goes_without() {
        assert!(shelves(&vec![TOP_LEVEL; 200]).iter().any(Option::is_none));
        assert!(pack(&vec![5000.0; 200]).iter().all(Option::is_some));
        assert!(pack(&vec![5000.0; 5000]).iter().any(Option::is_none));
    }

    #[test]
    fn a_lamp_without_a_tile_is_in_no_draw_and_the_rest_keep_their_order() {
        assert_eq!(draw_index(&[Some(Tile { x: 0, y: 0, level: 0 }), None, Some(Tile { x: 3, y: 0, level: 0 })]), [Some(0), None, Some(1)]);
    }

    #[test]
    fn tile_sizes_follow_the_atlas_side() {
        let sizes = |side: u32| [100.0, 500.0, 5000.0].map(|radius| Tile { x: 0, y: 0, level: wanted_level(radius) }.size(side / ATLAS_UNITS));
        assert_eq!(sizes(4096), [64, 128, 256]);
        assert_eq!(sizes(8192), [128, 256, 512]);
    }

    #[test]
    fn the_atlas_ends_under_the_tiles_it_holds() {
        let near = [[10.0, 0.0, 0.0], [10.0, 5.0, 0.0], [10.0, 0.0, 5.0]];
        assert_eq!(lamp_bake(&[near], &[], &[([0.0; 3], 5000.0)]).atlas_height(16384), 2048 + 4096);
        assert_eq!(lamp_bake(&[near], &[], &[([0.0; 3], 100.0); 2]).atlas_height(16384), 1024 + 4096);
        assert_eq!(lamp_bake(&[near], &[], &[]).atlas_height(4096), 1024);
        assert_eq!(lamp_bake(&[near], &[], &vec![([0.0; 3], 5000.0); 504]).atlas_height(8192), 8192);
    }

    #[test]
    fn a_packed_tile_stays_exact_as_a_float() {
        let tile = Tile { x: 127, y: 126, level: 3 };
        let through_float = tile.packed() as f32 as u32;
        assert_eq!(through_float, tile.packed());
        assert_eq!((through_float & 255, (through_float >> 8) & 255, (through_float >> 16) & 15), (127, 126, 3));
    }

    #[test]
    fn a_stored_bake_restores_its_indices_and_masked_vertices() {
        let triangle = |x: f32| [[x, 0.0, 0.0], [x, 5.0, 0.0], [x, 0.0, 5.0]];
        let world: Vec<_> = [900.0, 10.0, 20.0, -30.0, 2000.0].map(triangle).to_vec();
        let leaf = |x: f32| [[x, 0.0, 0.0, 0.0, 0.0], [x, 5.0, 0.0, 1.0, 0.0], [x, 0.0, 5.0, 0.0, 1.0]];
        let masked = [
            MaskedCaster { image: Handle::default(), cutoff: 0.5, triangles: vec![leaf(5.0), leaf(950.0), leaf(-40.0)] },
            MaskedCaster { image: Handle::default(), cutoff: 0.25, triangles: vec![leaf(1990.0), leaf(15.0)] },
        ];
        let fresh = lamp_bake(&world, &masked, &[([0.0; 3], 100.0), ([2000.0, 0.0, 0.0], 50.0), ([0.0; 3], 0.0)]);
        assert_eq!(fresh.masked_vertices.len(), 12);
        let stored = postcard::to_stdvec(&fresh).unwrap();
        let mut loaded: LampBake = postcard::from_bytes(&stored).unwrap();
        loaded.restore(&world, &masked);
        assert_eq!(loaded.indices, fresh.indices);
        assert_eq!(loaded.masked_vertices, fresh.masked_vertices);
        assert_eq!(loaded.masked_indices, fresh.masked_indices);
        assert_eq!(loaded.vertices, fresh.vertices);
        assert!(stored.len() < 4 * fresh.indices.len() + 24 * fresh.masked_vertices.len(), "{} bytes", stored.len());
    }

    #[test]
    fn a_lamp_gets_only_triangles_in_its_reach() {
        let near = [[10.0, 0.0, 0.0], [10.0, 5.0, 0.0], [10.0, 0.0, 5.0]];
        let far = [[900.0, 0.0, 0.0], [900.0, 5.0, 0.0], [900.0, 0.0, 5.0]];
        let bake = lamp_bake(&[far, near], &[], &[([0.0; 3], 100.0)]);
        assert_eq!(bake.draws[0].count, 3);
        assert_eq!(bake.indices, [3, 4, 5]);
        assert_eq!(bake.vertices.len(), 6);
    }

    #[test]
    fn each_face_looks_along_its_own_axis_and_covers_a_different_direction() {
        for (face, [x, y, z]) in FACE_AXES.iter().enumerate() {
            let cross = [x[1] * y[2] - x[2] * y[1], x[2] * y[0] - x[0] * y[2], x[0] * y[1] - x[1] * y[0]];
            let dot: f32 = (0..3).map(|axis| cross[axis] * z[axis]).sum();
            assert!(dot.abs() > 0.99, "face {face}");
        }
        for (index, a) in FACE_AXES.iter().enumerate() {
            assert!(FACE_AXES[index + 1..].iter().all(|b| a[2] != b[2]));
        }
    }
}
