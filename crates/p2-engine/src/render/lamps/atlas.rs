//! The static world's shadow of every lamp, drawn once a map into one atlas of six square tiles a lamp. Tiles are
//! packed in units of the atlas side over [`ATLAS_UNITS`], so the packing is the same at every atlas size. They take
//! the atlas's upper three quarters; the last holds the sharp tiles, each redrawn for the lamp that wants it, one lamp
//! a frame.

use std::num::NonZero;
use std::ops::Range;
use std::sync::Arc;

use bevy::asset::{AssetId, Assets, Handle, RenderAssetUsages, uuid_handle};
use bevy::ecs::prelude::{Local, Res};
use bevy::time::Time;
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::mesh::VertexBufferLayout;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer_sized};
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, Buffer, BufferBinding,
    BufferInitDescriptor, BufferUsages, CachedRenderPipelineId, ColorTargetState, ColorWrites, CommandEncoderDescriptor, Extent3d, FragmentState, IndexFormat, LoadOp,
    MultisampleState, Operations, Origin3d, PipelineCache, PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline, RenderPipelineDescriptor,
    SamplerBindingType, ShaderStages, StoreOp, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect, TextureDimension, TextureFormat, TextureSampleType, TextureUsages,
    VertexAttribute, VertexFormat, VertexState, VertexStepMode,
};
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::texture::GpuImage;
use bevy::shader::Shader;
use p2_assets::{Model, Terrain, fan_triangles, poly_flags};
use p2_math::Vector;

use super::LampAtlas;
use super::cells::{BOOK_WIDTH, HEADER_TEXELS, RECORD_TEXELS};
use crate::level_collision::{MaskedCaster, StaticCollision};

pub(super) const ATLAS_SHADER: Handle<Shader> = uuid_handle!("6d2f91c4-3e7a-4b58-9a10-c7e45f28d316");

/// The atlas side in packing units; a tile's corner is stored in 8 bits an axis. `modern.wesl` holds the same number.
pub(super) const ATLAS_UNITS: u32 = 128;
const ATLAS_ROWS: u32 = ATLAS_UNITS * 3 / 4;
const TOP_LEVEL: u32 = 3;
const UNMAPPED_LEVEL: u32 = 7;

/// Shaders tell a lamp from a retail dynamic light by this bit in the shadow depth bias field, which holds its tile.
/// Under 2^24, so it stays exact as a float, and far above any real bias.
pub(super) const LAMP_MARK: u32 = 1 << 20;
pub(super) const UNMAPPED: u32 = LAMP_MARK | (UNMAPPED_LEVEL << 16);

/// Sharp tiles: the first few large, ten across, a face this share of the atlas side; the rest half that, twenty
/// across. `modern.wesl` reads a tile's place from the book, so it holds none of these.
pub(super) const SHARP_TILES: usize = 100;
const LARGE_TILES: u32 = 20;
const LARGE_ACROSS: u32 = 10;
const FACE_SHARE_OF_SIDE: u32 = 32;
/// A lamp's shadow goes from its plain tile to its sharp one, and back, over this long.
const SHARP_FADE_SECONDS: f32 = 0.3;

pub(super) const NEAR: f32 = 8.0;
pub(super) const GUARD_TEXELS: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
        LAMP_MARK | self.x | (self.y << 8) | (self.level << 16)
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

/// A map with more lamps than fit at full size takes all of them a size smaller, before any lamp goes without.
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

fn solid_polygons(model: &Model) -> impl Iterator<Item = [[f32; 3]; 3]> + '_ {
    model
        .polygons()
        .into_iter()
        .filter(|polygon| polygon.poly_flags & OPEN_POLYGONS == 0)
        .flat_map(|polygon| fan_triangles(polygon.positions.len()).map(move |[a, b, c]| [polygon.positions[a], polygon.positions[b], polygon.positions[c]]).collect::<Vec<_>>())
}

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

fn near_sphere(triangle: &[[f32; 3]; 3], center: [f32; 3], radius: f32) -> bool {
    let mut gap = 0.0;
    for axis in 0..3 {
        let low = triangle.iter().map(|point| point[axis]).fold(f32::MAX, f32::min);
        let high = triangle.iter().map(|point| point[axis]).fold(f32::MIN, f32::max);
        let outside = (low - center[axis]).max(center[axis] - high).max(0.0);
        gap += outside * outside;
    }
    gap <= radius * radius
}

#[derive(Clone, Copy, Debug)]
struct LampDraw {
    position: [f32; 3],
    radius: f32,
    tile: Tile,
    first: u32,
    count: u32,
}

#[derive(Clone, Copy, Debug)]
struct MaskedRange {
    lamp: usize,
    group: usize,
    first: u32,
    count: u32,
}

pub(super) struct LampBake {
    pub(super) vertices: Vec<[f32; 3]>,
    indices: Vec<u32>,
    draws: Vec<LampDraw>,
    tiles: Vec<Option<Tile>>,
    masked_vertices: Vec<[f32; 6]>,
    masked_indices: Vec<u32>,
    masked_ranges: Vec<MaskedRange>,
    masked_images: Vec<Handle<Image>>,
}

impl LampBake {
    pub(super) fn tile_bits(&self, lamp: usize) -> u32 {
        self.tiles.get(lamp).copied().flatten().map_or(UNMAPPED, Tile::packed)
    }

    /// Where a lamp is among the draws, which hold only lamps with a tile.
    fn draw_of(&self, lamp: usize) -> Option<usize> {
        self.tiles.get(lamp)?.as_ref()?;
        Some(self.tiles[..lamp].iter().flatten().count())
    }

    /// For each lamp the side of a face of its tile in texels and its casters' corners in the vertices.
    pub(super) fn drawn(&self, unit: u32) -> impl Iterator<Item = Option<(u32, &[u32])>> {
        let mut draws = self.draws.iter();
        self.tiles.iter().map(move |tile| tile.and_then(|_| draws.next()).map(|draw| (draw.tile.size(unit), &self.indices[draw.first as usize..(draw.first + draw.count) as usize])))
    }
}

/// Lamp positions and triangles in the scene's space.
pub(super) fn lamp_bake(triangles: &[[[f32; 3]; 3]], masked: &[MaskedCaster], lamps: &[([f32; 3], f32)]) -> LampBake {
    let radii: Vec<f32> = lamps.iter().map(|&(_, radius)| radius).collect();
    let tiles = pack(&radii);
    let mut indices = Vec::new();
    let mut draws = Vec::new();
    let (mut masked_vertices, mut masked_indices, mut masked_ranges) = (Vec::new(), Vec::new(), Vec::new());
    for (&(position, radius), tile) in lamps.iter().zip(&tiles) {
        let Some(tile) = *tile else { continue };
        let first = indices.len() as u32;
        for (index, _) in triangles.iter().enumerate().filter(|(_, triangle)| near_sphere(triangle, position, radius)) {
            let corner = index as u32 * 3;
            indices.extend([corner, corner + 1, corner + 2]);
        }
        let lamp = draws.len();
        draws.push(LampDraw { position, radius, tile, first, count: indices.len() as u32 - first });
        for (group, caster) in masked.iter().enumerate() {
            let first = masked_indices.len() as u32;
            for triangle in &caster.triangles {
                if near_sphere(&triangle.map(|[x, y, z, ..]| [x, y, z]), position, radius) {
                    masked_indices.extend((0..3).map(|corner| masked_vertices.len() as u32 + corner));
                    masked_vertices.extend(triangle.map(|[x, y, z, u, v]| [x, y, z, u, v, caster.cutoff]));
                }
            }
            let count = masked_indices.len() as u32 - first;
            if count > 0 {
                masked_ranges.push(MaskedRange { lamp, group, first, count });
            }
        }
    }
    LampBake {
        vertices: triangles.iter().flatten().copied().collect(),
        indices,
        draws,
        tiles,
        masked_vertices,
        masked_indices,
        masked_ranges,
        masked_images: masked.iter().map(|caster| caster.image.clone()).collect(),
    }
}

/// Level, then static meshes, then terrain.
pub(crate) fn world_triangles(collision: &StaticCollision, origin: Vec3) -> Vec<[[f32; 3]; 3]> {
    let moved = |point: [f32; 3]| -> [f32; 3] { (Vec3::from(p2_bevy_assets::to_bevy(Vector::from(point))) + origin).into() };
    let mut triangles: Vec<[[f32; 3]; 3]> = solid_polygons(&collision.model).map(|triangle| triangle.map(moved)).collect();
    triangles.extend(collision.lamp_casters.iter().map(|triangle| triangle.map(moved)));
    for terrain in &collision.terrains {
        triangles.extend(terrain_triangles(terrain).map(|triangle| triangle.map(moved)));
    }
    triangles
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

pub(super) fn new_atlas(images: &mut Assets<Image>, side: u32) -> Handle<Image> {
    let mut image = Image::new_uninit(
        Extent3d { width: side, height: side, depth_or_array_layers: 1 },
        TextureDimension::D2,
        TextureFormat::R16Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::RENDER_ATTACHMENT;
    images.add(image)
}

const FACE_STRIDE: u64 = 256;

/// World axes of a cube face's camera x, y and z. `modern.wesl` picks the same face and axes.
pub(super) const FACE_AXES: [[[f32; 3]; 3]; 6] = [
    [[0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
    [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]],
    [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]],
    [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]],
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]],
];

fn face_uniforms(draws: &[LampDraw], face_size: impl Fn(&LampDraw) -> u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(draws.len() * 6 * FACE_STRIDE as usize);
    for draw in draws {
        let size = face_size(draw) as f32;
        let scale = (0.5 * size - GUARD_TEXELS) / (0.5 * size);
        let depth_a = draw.radius / (draw.radius - NEAR).max(1.0);
        for axes in FACE_AXES {
            let start = bytes.len();
            let [px, py, pz] = draw.position;
            let rows = [
                [axes[0][0], axes[0][1], axes[0][2], 0.0],
                [axes[1][0], axes[1][1], axes[1][2], 0.0],
                [axes[2][0], axes[2][1], axes[2][2], 0.0],
                [px, py, pz, 1.0 / draw.radius],
                [scale, depth_a, -NEAR * depth_a, 0.0],
            ];
            bytes.extend(rows.iter().flatten().flat_map(|value: &f32| value.to_le_bytes()));
            bytes.resize(start + FACE_STRIDE as usize, 0);
        }
    }
    bytes
}

/// Where a lamp's face lands in the atlas, in texels.
fn face_corner(tile: Tile, face: u32, unit: u32) -> (u32, u32) {
    let size = tile.size(unit);
    (tile.x * unit + (face % 3) * size, tile.y * unit + (face / 3) * size)
}

/// A sharp tile's corner and the side of one of its faces in an atlas of this side, in texels.
fn sharp_tile(slot: usize, side: u32) -> (u32, u32, u32) {
    let large = side / FACE_SHARE_OF_SIDE;
    let top = side / ATLAS_UNITS * ATLAS_ROWS;
    let slot = slot as u32;
    if slot < LARGE_TILES {
        return (slot % LARGE_ACROSS * 3 * large, top + slot / LARGE_ACROSS * 2 * large, large);
    }
    let (small, at) = (large / 2, slot - LARGE_TILES);
    (at % (2 * LARGE_ACROSS) * 3 * small, top + LARGE_TILES / LARGE_ACROSS * 2 * large + at / (2 * LARGE_ACROSS) * 2 * small, small)
}

pub(super) struct AtlasPipelines {
    faces: BindGroupLayoutDescriptor,
    solid: CachedRenderPipelineId,
    image: BindGroupLayoutDescriptor,
    masked: CachedRenderPipelineId,
    clear: CachedRenderPipelineId,
}

/// Kept for the map: the sharp tiles are drawn from them long after the plain ones.
struct AtlasBuffers {
    faces: BindGroup,
    /// For the large sharp tiles, then the small.
    sharp_faces: [BindGroup; 2],
    vertices: Buffer,
    masked: Option<(Buffer, Buffer)>,
}

#[derive(Default)]
pub(super) struct AtlasDraw {
    pipelines: Option<AtlasPipelines>,
    drawn: Option<(usize, AssetId<Image>)>,
    waited: u32,
    buffers: Option<AtlasBuffers>,
    /// The lamp each sharp tile holds, and how far its shadow has gone over to the tile, 0 to 1.
    sharp: Vec<Option<u32>>,
    blend: Vec<f32>,
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
    let min = BlendComponent { src_factor: BlendFactor::One, dst_factor: BlendFactor::One, operation: BlendOperation::Min };
    let pipeline = |label: &'static str, layout: Vec<BindGroupLayoutDescriptor>, vertex: &'static str, fragment: &'static str, buffers: Vec<VertexBufferLayout>, blend: Option<BlendState>| {
        cache.queue_render_pipeline(RenderPipelineDescriptor {
            label: Some(label.into()),
            layout,
            vertex: VertexState { shader: ATLAS_SHADER, entry_point: Some(vertex.into()), buffers, ..Default::default() },
            fragment: Some(FragmentState {
                shader: ATLAS_SHADER,
                entry_point: Some(fragment.into()),
                targets: vec![Some(ColorTargetState { format: TextureFormat::R16Float, blend, write_mask: ColorWrites::ALL })],
                ..Default::default()
            }),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            ..Default::default()
        })
    };
    let nearest = Some(BlendState { color: min, alpha: min });
    let attribute = |format: VertexFormat, offset: u64, shader_location: u32| VertexAttribute { format, offset, shader_location };
    let solid = pipeline(
        "lamp atlas",
        vec![faces.clone()],
        "vertex",
        "fragment",
        vec![VertexBufferLayout { array_stride: 12, step_mode: VertexStepMode::Vertex, attributes: vec![attribute(VertexFormat::Float32x3, 0, 0)] }],
        nearest,
    );
    let masked = pipeline(
        "lamp atlas masked",
        vec![faces.clone(), image.clone()],
        "masked_vertex",
        "masked_fragment",
        vec![VertexBufferLayout {
            array_stride: 24,
            step_mode: VertexStepMode::Vertex,
            attributes: vec![attribute(VertexFormat::Float32x3, 0, 0), attribute(VertexFormat::Float32x2, 12, 1), attribute(VertexFormat::Float32, 20, 2)],
        }],
        nearest,
    );
    let clear = pipeline("lamp atlas clear", Vec::new(), "clear_vertex", "clear_fragment", Vec::new(), None);
    AtlasPipelines { faces, solid, image, masked, clear }
}

/// A lamp's texture may reach the GPU a little after the bake; past this many frames its casters are left out.
const ATLAS_WAIT_FRAMES: u32 = 240;

fn bytes_of<const N: usize>(values: &[[f32; N]]) -> Vec<u8> {
    values.as_flattened().iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn index_bytes(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

/// One lamp's six faces to draw. `clear` is the tile's rectangle where what it held must go first.
struct TileJob<'a> {
    lamp: usize,
    faces: &'a BindGroup,
    size: u32,
    corners: [(u32, u32); 6],
    clear: Option<[u32; 4]>,
    indices: &'a Buffer,
    range: Range<u32>,
}

struct Ready<'a> {
    solid: &'a RenderPipeline,
    masked: &'a RenderPipeline,
    clear: &'a RenderPipeline,
    target: &'a GpuImage,
    buffers: &'a AtlasBuffers,
    image_groups: Vec<Option<BindGroup>>,
}

fn draw_tiles(device: &RenderDevice, queue: &RenderQueue, ready: &Ready, bake: &LampBake, load: LoadOp<wgpu_types::Color>, jobs: &[TileJob]) {
    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor { label: Some("lamp atlas") });
    {
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("lamp atlas"),
            color_attachments: &[Some(RenderPassColorAttachment { view: &ready.target.texture_view, depth_slice: None, resolve_target: None, ops: Operations { load, store: StoreOp::Store } })],
            ..Default::default()
        });
        for job in jobs {
            if let Some([x, y, width, height]) = job.clear {
                pass.set_pipeline(ready.clear);
                pass.set_viewport(x as f32, y as f32, width as f32, height as f32, 0.0, 1.0);
                pass.set_scissor_rect(x, y, width, height);
                pass.draw(0..3, 0..1);
            }
            let solid = (!job.range.is_empty()).then(|| (None, job.range.clone()));
            let masked = ready.buffers.masked.iter().flat_map(|_| &bake.masked_ranges).filter(|range| range.lamp == job.lamp).map(|range| (Some(range.group), range.first..range.first + range.count));
            for (group, range) in solid.into_iter().chain(masked) {
                match (group, &ready.buffers.masked) {
                    (Some(group), Some((vertices, indices))) => {
                        let Some(image_group) = &ready.image_groups[group] else { continue };
                        pass.set_pipeline(ready.masked);
                        pass.set_vertex_buffer(0, *vertices.slice(..));
                        pass.set_index_buffer(*indices.slice(..), IndexFormat::Uint32);
                        pass.set_bind_group(1, image_group, &[]);
                    }
                    _ => {
                        pass.set_pipeline(ready.solid);
                        pass.set_vertex_buffer(0, *ready.buffers.vertices.slice(..));
                        pass.set_index_buffer(*job.indices.slice(..), IndexFormat::Uint32);
                    }
                }
                for (face, &(x, y)) in job.corners.iter().enumerate() {
                    pass.set_viewport(x as f32, y as f32, job.size as f32, job.size as f32, 0.0, 1.0);
                    pass.set_scissor_rect(x, y, job.size, job.size);
                    pass.set_bind_group(0, job.faces, &[((job.lamp as u64 * 6 + face as u64) * FACE_STRIDE) as u32]);
                    pass.draw_indexed(range.clone(), 0, 0..1);
                }
            }
        }
    }
    queue.submit([encoder.finish()]);
}

/// The last texel of a lamp's record in the book: its sharp tile's corner and face side in texels and how far its
/// shadow has gone over to it, all 0 without one.
fn write_sharp(queue: &RenderQueue, book: &GpuImage, lamp: u32, tile: [f32; 4]) {
    let texel = HEADER_TEXELS + (lamp as usize + 1) * RECORD_TEXELS - 1;
    queue.write_texture(
        TexelCopyTextureInfo { texture: &book.texture, mip_level: 0, origin: Origin3d { x: (texel % BOOK_WIDTH) as u32, y: (texel / BOOK_WIDTH) as u32, z: 0 }, aspect: TextureAspect::All },
        &bytes_of(&[tile]),
        TexelCopyBufferLayout { offset: 0, bytes_per_row: None, rows_per_image: None },
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
    );
}

pub(super) fn draw_lamp_atlas(
    atlas: Res<LampAtlas>,
    images: Res<RenderAssets<GpuImage>>,
    cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    time: Res<Time>,
    mut state: Local<AtlasDraw>,
) {
    let (Some(bake), Some(image)) = (&atlas.bake, &atlas.image) else { return };
    let key = (Arc::as_ptr(bake).addr(), image.id());
    let state = &mut *state;
    let unit = atlas.side / ATLAS_UNITS;
    let pipelines = state.pipelines.get_or_insert_with(|| atlas_pipelines(&cache));
    let (Some(solid), Some(masked), Some(clear), Some(target)) =
        (cache.get_render_pipeline(pipelines.solid), cache.get_render_pipeline(pipelines.masked), cache.get_render_pipeline(pipelines.clear), images.get(image))
    else {
        return;
    };
    let image_groups = || -> Vec<Option<BindGroup>> {
        let layout = cache.get_bind_group_layout(&pipelines.image);
        bake.masked_images.iter().map(|image| images.get(image).map(|image| device.create_bind_group(None, &layout, &BindGroupEntries::sequential((&image.texture_view, &image.sampler))))).collect()
    };
    let index_buffer = |indices: &[u32]| device.create_buffer_with_data(&BufferInitDescriptor { label: Some("lamp atlas indices"), contents: &index_bytes(indices), usage: BufferUsages::INDEX });
    if state.drawn != Some(key) {
        state.waited += 1;
        if state.waited < ATLAS_WAIT_FRAMES && bake.masked_images.iter().any(|image| images.get(image).is_none()) {
            return;
        }
        state.waited = 0;
        let face_layout = cache.get_bind_group_layout(&pipelines.faces);
        let faces = |label: &'static str, bytes: Vec<u8>| {
            let buffer = device.create_buffer_with_data(&BufferInitDescriptor { label: Some(label), contents: if bytes.is_empty() { &[0; FACE_STRIDE as usize] } else { &bytes }, usage: BufferUsages::UNIFORM });
            device.create_bind_group(None, &face_layout, &BindGroupEntries::single(BufferBinding { buffer: &buffer, offset: 0, size: NonZero::new(80) }))
        };
        let vertex_buffer = |label: &'static str, bytes: Vec<u8>| device.create_buffer_with_data(&BufferInitDescriptor { label: Some(label), contents: &bytes, usage: BufferUsages::VERTEX });
        let buffers = state.buffers.insert(AtlasBuffers {
            faces: faces("lamp atlas faces", face_uniforms(&bake.draws, |draw| draw.tile.size(unit))),
            sharp_faces: [1, 2].map(|share| faces("lamp atlas sharp faces", face_uniforms(&bake.draws, |_| atlas.side / FACE_SHARE_OF_SIDE / share))),
            vertices: vertex_buffer("lamp atlas vertices", bytes_of(&bake.vertices)),
            masked: (!bake.masked_indices.is_empty()).then(|| (vertex_buffer("lamp atlas masked vertices", bytes_of(&bake.masked_vertices)), index_buffer(&bake.masked_indices))),
        });
        let indices = index_buffer(&bake.indices);
        let jobs: Vec<TileJob> = bake
            .draws
            .iter()
            .enumerate()
            .map(|(lamp, draw)| TileJob {
                lamp,
                faces: &buffers.faces,
                size: draw.tile.size(unit),
                corners: std::array::from_fn(|face| face_corner(draw.tile, face as u32, unit)),
                clear: None,
                indices: &indices,
                range: draw.first..draw.first + draw.count,
            })
            .collect();
        draw_tiles(&device, &queue, &Ready { solid, masked, clear, target, buffers, image_groups: image_groups() }, bake, LoadOp::Clear(wgpu_types::Color::WHITE), &jobs);
        state.drawn = Some(key);
        state.sharp = vec![None; SHARP_TILES];
        state.blend = vec![0.0; SHARP_TILES];
        return;
    }
    let (Some(buffers), Some(book)) = (&state.buffers, atlas.book.as_ref().and_then(|book| images.get(book))) else { return };
    let step = time.delta_secs() / SHARP_FADE_SECONDS;
    let mut redrawn = false;
    for slot in 0..state.sharp.len().min(atlas.sharp.len()) {
        let (held, wanted, blend) = (state.sharp[slot], atlas.sharp[slot], state.blend[slot]);
        let (x, y, face) = sharp_tile(slot, atlas.side);
        let place = |blend: f32| if blend > 0.0 { [x as f32, y as f32, face as f32, blend] } else { [0.0; 4] };
        if held == wanted {
            if let Some(lamp) = held.filter(|_| blend < 1.0) {
                state.blend[slot] = (blend + step).min(1.0);
                write_sharp(&queue, book, lamp, place(state.blend[slot]));
            }
            continue;
        }
        // The lamp that held the tile fades back to its plain one before another is drawn here.
        if let Some(lamp) = held.filter(|_| blend > 0.0) {
            state.blend[slot] = (blend - step).max(0.0);
            write_sharp(&queue, book, lamp, place(state.blend[slot]));
            continue;
        }
        state.sharp[slot] = None;
        let Some((lamp, draw_index)) = wanted.filter(|_| !redrawn).and_then(|lamp| Some((lamp, bake.draw_of(lamp as usize)?))) else { continue };
        redrawn = true;
        state.sharp[slot] = Some(lamp);
        let draw = &bake.draws[draw_index];
        let indices = index_buffer(&bake.indices[draw.first as usize..(draw.first + draw.count) as usize]);
        let job = TileJob {
            lamp: draw_index,
            faces: &buffers.sharp_faces[usize::from(slot as u32 >= LARGE_TILES)],
            size: face,
            corners: std::array::from_fn(|at| (x + at as u32 % 3 * face, y + at as u32 / 3 * face)),
            clear: Some([x, y, 3 * face, 2 * face]),
            indices: &indices,
            range: 0..draw.count,
        };
        draw_tiles(&device, &queue, &Ready { solid, masked, clear, target, buffers, image_groups: image_groups() }, bake, LoadOp::Load, &[job]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn sharp_tiles_lie_under_the_plain_ones_large_then_small_and_none_overlaps() {
        let side = 8192;
        let tiles: Vec<(u32, u32, u32)> = (0..SHARP_TILES).map(|slot| sharp_tile(slot, side)).collect();
        assert_eq!(tiles[..2], [(0, 6144, 256), (768, 6144, 256)]);
        assert_eq!(tiles[LARGE_TILES as usize], (0, 6144 + 1024, 128));
        for (index, &(ax, ay, a)) in tiles.iter().enumerate() {
            assert!(ax + 3 * a <= side && ay + 2 * a <= side && ay >= 6144);
            for &(bx, by, b) in &tiles[index + 1..] {
                assert!(ax + 3 * a <= bx || bx + 3 * b <= ax || ay + 2 * a <= by || by + 2 * b <= ay);
            }
        }
        assert_eq!(tiles[SHARP_TILES - 1].1 + 2 * tiles[SHARP_TILES - 1].2, side);
    }

    #[test]
    fn a_lamp_without_a_tile_is_in_no_draw_and_the_rest_keep_their_order() {
        let bake = LampBake {
            vertices: Vec::new(),
            indices: Vec::new(),
            draws: Vec::new(),
            tiles: vec![Some(Tile { x: 0, y: 0, level: 0 }), None, Some(Tile { x: 3, y: 0, level: 0 })],
            masked_vertices: Vec::new(),
            masked_indices: Vec::new(),
            masked_ranges: Vec::new(),
            masked_images: Vec::new(),
        };
        assert_eq!([0, 1, 2, 3].map(|lamp| bake.draw_of(lamp)), [Some(0), None, Some(1), None]);
    }

    #[test]
    fn tile_sizes_follow_the_atlas_side() {
        let sizes = |side: u32| [100.0, 500.0, 5000.0].map(|radius| Tile { x: 0, y: 0, level: wanted_level(radius) }.size(side / ATLAS_UNITS));
        assert_eq!(sizes(4096), [64, 128, 256]);
        assert_eq!(sizes(8192), [128, 256, 512]);
    }

    #[test]
    fn a_packed_tile_stays_exact_as_a_float_and_keeps_its_mark() {
        let tile = Tile { x: 127, y: 126, level: 3 };
        let through_float = tile.packed() as f32 as u32;
        assert_eq!(through_float, tile.packed());
        assert!(through_float >= LAMP_MARK);
        assert_eq!((through_float & 255, (through_float >> 8) & 255, (through_float >> 16) & 15), (127, 126, 3));
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
