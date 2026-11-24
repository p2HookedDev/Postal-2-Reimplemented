//! The static world's shadow of every lamp, drawn once a map into one atlas of six square tiles a lamp. Tiles are
//! packed in units of the atlas side over [`ATLAS_UNITS`], so the packing is the same at every atlas size.

use std::num::NonZero;
use std::sync::Arc;

use bevy::asset::{AssetId, Assets, Handle, RenderAssetUsages, uuid_handle};
use bevy::ecs::prelude::{Local, Res};
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::mesh::VertexBufferLayout;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer_sized};
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState, BufferBinding, BufferInitDescriptor,
    BufferUsages, CachedRenderPipelineId, ColorTargetState, ColorWrites, CommandEncoderDescriptor, Extent3d, FragmentState, IndexFormat, LoadOp, MultisampleState, Operations,
    PipelineCache, PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, SamplerBindingType, ShaderStages, StoreOp, TextureDimension,
    TextureFormat, TextureSampleType, TextureUsages, VertexAttribute, VertexFormat, VertexState, VertexStepMode,
};
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::texture::GpuImage;
use bevy::shader::Shader;
use p2_assets::{Model, Terrain, fan_triangles, poly_flags};
use p2_math::Vector;

use super::LampAtlas;
use crate::level_collision::{MaskedCaster, StaticCollision};

pub(super) const ATLAS_SHADER: Handle<Shader> = uuid_handle!("6d2f91c4-3e7a-4b58-9a10-c7e45f28d316");

/// The atlas side in packing units; a tile's corner is stored in 8 bits an axis. `modern.wesl` holds the same number.
pub(super) const ATLAS_UNITS: u32 = 128;
const TOP_LEVEL: u32 = 3;
const UNMAPPED_LEVEL: u32 = 7;

/// Shaders tell a lamp from a retail dynamic light by this bit in the shadow depth bias field, which holds its tile.
/// Under 2^24, so it stays exact as a float, and far above any real bias.
const LAMP_MARK: u32 = 1 << 20;
pub(super) const UNMAPPED: u32 = LAMP_MARK | (UNMAPPED_LEVEL << 16);

const NEAR: f32 = 8.0;
const GUARD_TEXELS: f32 = 2.0;

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
        if y + height > ATLAS_UNITS {
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
    vertices: Vec<[f32; 3]>,
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
const FACE_AXES: [[[f32; 3]; 3]; 6] = [
    [[0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
    [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]],
    [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]],
    [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]],
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]],
];

fn face_uniforms(draws: &[LampDraw], unit: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(draws.len() * 6 * FACE_STRIDE as usize);
    for draw in draws {
        let size = draw.tile.size(unit) as f32;
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

pub(super) struct AtlasPipelines {
    faces: BindGroupLayoutDescriptor,
    solid: CachedRenderPipelineId,
    image: BindGroupLayoutDescriptor,
    masked: CachedRenderPipelineId,
}

#[derive(Default)]
pub(super) struct AtlasDraw {
    pipelines: Option<AtlasPipelines>,
    drawn: Option<(usize, AssetId<Image>)>,
    waited: u32,
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
    let pipeline = |label: &'static str, layout: Vec<BindGroupLayoutDescriptor>, vertex: &'static str, fragment: &'static str, buffer: VertexBufferLayout| {
        cache.queue_render_pipeline(RenderPipelineDescriptor {
            label: Some(label.into()),
            layout,
            vertex: VertexState { shader: ATLAS_SHADER, entry_point: Some(vertex.into()), buffers: vec![buffer], ..Default::default() },
            fragment: Some(FragmentState {
                shader: ATLAS_SHADER,
                entry_point: Some(fragment.into()),
                targets: vec![Some(ColorTargetState { format: TextureFormat::R16Float, blend: Some(BlendState { color: min, alpha: min }), write_mask: ColorWrites::ALL })],
                ..Default::default()
            }),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            ..Default::default()
        })
    };
    let attribute = |format: VertexFormat, offset: u64, shader_location: u32| VertexAttribute { format, offset, shader_location };
    let solid = pipeline(
        "lamp atlas",
        vec![faces.clone()],
        "vertex",
        "fragment",
        VertexBufferLayout { array_stride: 12, step_mode: VertexStepMode::Vertex, attributes: vec![attribute(VertexFormat::Float32x3, 0, 0)] },
    );
    let masked = pipeline(
        "lamp atlas masked",
        vec![faces.clone(), image.clone()],
        "masked_vertex",
        "masked_fragment",
        VertexBufferLayout {
            array_stride: 24,
            step_mode: VertexStepMode::Vertex,
            attributes: vec![attribute(VertexFormat::Float32x3, 0, 0), attribute(VertexFormat::Float32x2, 12, 1), attribute(VertexFormat::Float32, 20, 2)],
        },
    );
    AtlasPipelines { faces, solid, image, masked }
}

/// A lamp's texture may reach the GPU a little after the bake; past this many frames its casters are left out.
const ATLAS_WAIT_FRAMES: u32 = 240;

fn bytes_of<const N: usize>(values: &[[f32; N]]) -> Vec<u8> {
    values.as_flattened().iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn index_bytes(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

pub(super) fn draw_lamp_atlas(atlas: Res<LampAtlas>, images: Res<RenderAssets<GpuImage>>, cache: Res<PipelineCache>, device: Res<RenderDevice>, queue: Res<RenderQueue>, mut state: Local<AtlasDraw>) {
    let (Some(bake), Some(image)) = (&atlas.bake, &atlas.image) else { return };
    let key = (Arc::as_ptr(bake).addr(), image.id());
    if state.drawn == Some(key) {
        return;
    }
    let unit = atlas.side / ATLAS_UNITS;
    let pipelines = state.pipelines.get_or_insert_with(|| atlas_pipelines(&cache));
    let (Some(solid), Some(masked), Some(target)) = (cache.get_render_pipeline(pipelines.solid), cache.get_render_pipeline(pipelines.masked), images.get(image)) else { return };
    let image_layout = cache.get_bind_group_layout(&pipelines.image);
    let face_layout = cache.get_bind_group_layout(&pipelines.faces);
    state.waited += 1;
    if state.waited < ATLAS_WAIT_FRAMES && bake.masked_images.iter().any(|image| images.get(image).is_none()) {
        return;
    }
    state.waited = 0;
    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor { label: Some("lamp atlas") });
    {
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("lamp atlas"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &target.texture_view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations { load: LoadOp::Clear(wgpu_types::Color::WHITE), store: StoreOp::Store },
            })],
            ..Default::default()
        });
        if !bake.draws.is_empty() {
            let faces = device.create_buffer_with_data(&BufferInitDescriptor { label: Some("lamp atlas faces"), contents: &face_uniforms(&bake.draws, unit), usage: BufferUsages::UNIFORM });
            let group = device.create_bind_group(None, &face_layout, &BindGroupEntries::single(BufferBinding { buffer: &faces, offset: 0, size: NonZero::new(80) }));
            if !bake.indices.is_empty() {
                let vertices = device.create_buffer_with_data(&BufferInitDescriptor { label: Some("lamp atlas vertices"), contents: &bytes_of(&bake.vertices), usage: BufferUsages::VERTEX });
                let indices = device.create_buffer_with_data(&BufferInitDescriptor { label: Some("lamp atlas indices"), contents: &index_bytes(&bake.indices), usage: BufferUsages::INDEX });
                pass.set_pipeline(solid);
                pass.set_vertex_buffer(0, *vertices.slice(..));
                pass.set_index_buffer(*indices.slice(..), IndexFormat::Uint32);
                for (lamp, draw) in bake.draws.iter().enumerate().filter(|(_, draw)| draw.count > 0) {
                    let size = draw.tile.size(unit);
                    for face in 0..6u32 {
                        let (x, y) = face_corner(draw.tile, face, unit);
                        pass.set_viewport(x as f32, y as f32, size as f32, size as f32, 0.0, 1.0);
                        pass.set_scissor_rect(x, y, size, size);
                        pass.set_bind_group(0, &group, &[((lamp as u64 * 6 + u64::from(face)) * FACE_STRIDE) as u32]);
                        pass.draw_indexed(draw.first..draw.first + draw.count, 0, 0..1);
                    }
                }
            }
            if !bake.masked_indices.is_empty() {
                let vertices = device.create_buffer_with_data(&BufferInitDescriptor { label: Some("lamp atlas masked vertices"), contents: &bytes_of(&bake.masked_vertices), usage: BufferUsages::VERTEX });
                let indices = device.create_buffer_with_data(&BufferInitDescriptor { label: Some("lamp atlas masked indices"), contents: &index_bytes(&bake.masked_indices), usage: BufferUsages::INDEX });
                let image_groups: Vec<_> = bake
                    .masked_images
                    .iter()
                    .map(|image| images.get(image).map(|image| device.create_bind_group(None, &image_layout, &BindGroupEntries::sequential((&image.texture_view, &image.sampler)))))
                    .collect();
                pass.set_pipeline(masked);
                pass.set_vertex_buffer(0, *vertices.slice(..));
                pass.set_index_buffer(*indices.slice(..), IndexFormat::Uint32);
                for range in &bake.masked_ranges {
                    let Some(image_group) = &image_groups[range.group] else { continue };
                    let tile = bake.draws[range.lamp].tile;
                    let size = tile.size(unit);
                    pass.set_bind_group(1, image_group, &[]);
                    for face in 0..6u32 {
                        let (x, y) = face_corner(tile, face, unit);
                        pass.set_viewport(x as f32, y as f32, size as f32, size as f32, 0.0, 1.0);
                        pass.set_scissor_rect(x, y, size, size);
                        pass.set_bind_group(0, &group, &[((range.lamp as u64 * 6 + u64::from(face)) * FACE_STRIDE) as u32]);
                        pass.draw_indexed(range.first..range.first + range.count, 0, 0..1);
                    }
                }
            }
        }
    }
    queue.submit([encoder.finish()]);
    state.drawn = Some(key);
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
            assert!(ax2 <= ATLAS_UNITS && ay2 <= ATLAS_UNITS);
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
