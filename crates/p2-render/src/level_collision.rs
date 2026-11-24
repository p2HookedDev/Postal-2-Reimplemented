use std::ops::RangeInclusive;
use std::sync::Arc;

use bevy::asset::Handle;
use bevy::ecs::prelude::Resource;
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::mesh::{Mesh, VertexAttributeValues};
use bevy::render::render_resource::TextureFormat;
use bevy::transform::components::Transform;
use p2_assets::{Model, Terrain, fan_triangles, poly_flags};
use p2_collision::{BspHulls, Hit, MeshCollision, MeshSet, Sides, line_check, terrain_line_check};
use p2_math::Vector;
use rustc_hash::FxHashMap;

pub struct StaticCollision {
    pub model: Model,
    pub hulls: BspHulls,
    pub terrains: Vec<Terrain>,
    pub meshes: Vec<MeshCollision>,
    /// Built from `meshes`.
    pub mesh_set: MeshSet,
    /// Opaque triangles of static meshes that never move: what stops a lamp's light, blocking or not.
    pub lamp_casters: Vec<[[f32; 3]; 3]>,
    pub lamp_masked: Vec<MaskedCaster>,
    pub albedos: LevelAlbedos,
}

impl StaticCollision {
    pub fn first_hit(&self, start: Vector, end: Vector) -> Option<Hit> {
        let bsp = line_check(&self.model, start, end);
        let terrain = self.terrains.iter().filter_map(|t| terrain_line_check(t, start, end, Sides::Both));
        let mesh = self.mesh_set.line_check_at(&self.meshes, start, end).map(|(hit, ..)| hit);
        bsp.into_iter().chain(terrain).chain(mesh).min_by(|a, b| a.time.total_cmp(&b.time))
    }

    /// Whether light gets from `from` to `to`: the sky's backdrop surfaces let it through, and what stands before
    /// them still blocks.
    pub fn light_passes(&self, from: Vector, to: Vector) -> bool {
        let reach = match p2_collision::line_check_node(&self.model, from, to) {
            None => 1.0,
            Some((hit, node)) if node.is_some_and(|node| p2_collision::is_sky(&self.model, node)) => hit.time,
            Some(_) => return false,
        };
        !self.terrains.iter().any(|terrain| terrain_line_check(terrain, from, to, Sides::Both).is_some_and(|hit| hit.time < reach))
            && self.mesh_set.line_check_at(&self.meshes, from, to).is_none_or(|(hit, ..)| hit.time >= reach)
    }
}

/// Mean texture colours in linear light.
#[derive(Default)]
pub struct LevelAlbedos {
    pub bsp: FxHashMap<i32, [f32; 3]>,
    /// One per triangle of `lamp_casters`.
    pub casters: Vec<[f32; 3]>,
    pub terrains: Vec<[f32; 3]>,
}

/// Alpha-tested triangles sharing one texture, in the scene's space before its origin. Each corner is x, y, z, u, v.
pub struct MaskedCaster {
    pub image: Handle<Image>,
    pub cutoff: f32,
    pub triangles: Vec<[[f32; 5]; 3]>,
}

pub(crate) const GREY_ALBEDO: [f32; 3] = [0.3; 3];
/// Blocks read from a texture's largest mip for its mean.
const MEAN_SAMPLES: usize = 4096;

/// In linear light by gamma 2. Compressed blocks count only their two end colours: enough for bounced light.
pub(crate) fn mean_albedo(image: &Image) -> [f32; 3] {
    let Some(data) = image.data.as_deref() else { return GREY_ALBEDO };
    let (width, height) = (image.texture_descriptor.size.width as usize, image.texture_descriptor.size.height as usize);
    let mut sum = Vec3::ZERO;
    let mut count = 0.0;
    let mut add = |r: f32, g: f32, b: f32| {
        sum += Vec3::new(r * r, g * g, b * b);
        count += 1.0;
    };
    let wide = |bits: u16, shift: u16, mask: u16| f32::from((bits >> shift) & mask) / f32::from(mask);
    let block = |stride: usize, colors_at: usize, add: &mut dyn FnMut(f32, f32, f32)| {
        let blocks = width.div_ceil(4) * height.div_ceil(4);
        for chunk in data.chunks_exact(stride).take(blocks).step_by((blocks / MEAN_SAMPLES).max(1)) {
            for at in [colors_at, colors_at + 2] {
                let bits = u16::from_le_bytes([chunk[at], chunk[at + 1]]);
                add(wide(bits, 11, 31), wide(bits, 5, 63), wide(bits, 0, 31));
            }
        }
    };
    match image.texture_descriptor.format {
        TextureFormat::Bc1RgbaUnormSrgb | TextureFormat::Bc1RgbaUnorm => block(8, 0, &mut add),
        TextureFormat::Bc2RgbaUnormSrgb | TextureFormat::Bc2RgbaUnorm | TextureFormat::Bc3RgbaUnormSrgb | TextureFormat::Bc3RgbaUnorm => block(16, 8, &mut add),
        format @ (TextureFormat::Rgba8UnormSrgb | TextureFormat::Rgba8Unorm | TextureFormat::Bgra8UnormSrgb | TextureFormat::Bgra8Unorm) => {
            let bgra = matches!(format, TextureFormat::Bgra8UnormSrgb | TextureFormat::Bgra8Unorm);
            let texels = width * height;
            for texel in data.as_chunks::<4>().0.iter().take(texels).step_by((texels / MEAN_SAMPLES).max(1)) {
                let channel = |index: usize| f32::from(texel[index]) / 255.0;
                let (r, b) = if bgra { (channel(2), channel(0)) } else { (channel(0), channel(2)) };
                add(r, channel(1), b);
            }
        }
        _ => return GREY_ALBEDO,
    }
    if count == 0.0 { GREY_ALBEDO } else { (sum / count).to_array() }
}

/// A section's triangles as corners of x, y, z, u, v, placed by `transform`.
pub(crate) fn masked_triangles(mesh: &Mesh, transform: &Transform) -> Vec<[[f32; 5]; 3]> {
    let (Some(VertexAttributeValues::Float32x3(positions)), Some(VertexAttributeValues::Float32x2(uvs)), Some(indices)) =
        (mesh.attribute(Mesh::ATTRIBUTE_POSITION), mesh.attribute(Mesh::ATTRIBUTE_UV_0), mesh.indices())
    else {
        return Vec::new();
    };
    let corners: Vec<usize> = indices.iter().collect();
    corners
        .as_chunks::<3>()
        .0
        .iter()
        .map(|triangle| {
            triangle.map(|corner| {
                let [x, y, z] = transform.transform_point(Vec3::from(positions[corner])).to_array();
                let [u, v] = uvs[corner];
                [x, y, z, u, v]
            })
        })
        .collect()
}

#[derive(Resource, Default)]
pub struct LevelCollision(pub Option<Arc<StaticCollision>>);

impl LevelCollision {
    pub fn model(&self) -> Option<&Model> {
        self.0.as_ref().map(|collision| &collision.model)
    }
}

/// The node flag of a BSP node that does not divide solid from empty space.
const NOT_CSG: u8 = 0x01;
/// The side of a cell of the triangle grid, in Unreal units.
const CELL: f32 = 500.0;
/// A triangle whose bounds cover more cells than this is tested for every box instead of listed in each cell.
const CELLS_MOST: i64 = 4096;

/// The level's triangles by place, for small physics worlds: the BSP's sorted once into a grid, kept with the
/// collision they were taken from.
pub(crate) struct LevelTriangles {
    collision: Arc<StaticCollision>,
    bsp: TriangleGrid,
}

impl LevelTriangles {
    pub(crate) fn new(collision: Arc<StaticCollision>) -> Self {
        Self { bsp: TriangleGrid::new(bsp_triangles(&collision.model)), collision }
    }

    pub(crate) fn holds(&self, collision: &Arc<StaticCollision>) -> bool {
        Arc::ptr_eq(&self.collision, collision)
    }

    /// The BSP's, the static meshes' and the terrains' triangles reaching into the box `reach` around `centre`.
    pub(crate) fn near(&self, centre: Vector, reach: f32) -> Vec<[[f32; 3]; 3]> {
        let centre: [f32; 3] = centre.into();
        let (min, max) = (centre.map(|c| c - reach), centre.map(|c| c + reach));
        let mut near: Vec<[[f32; 3]; 3]> = self.bsp.reaching(min, max).collect();
        for mesh in &self.collision.meshes {
            let (low, high) = mesh.bounds();
            if (0..3).all(|k| low[k] <= max[k] && high[k] >= min[k]) {
                near.extend(reaching(mesh.triangles(), min, max));
            }
        }
        for terrain in &self.collision.terrains {
            near.extend(terrain_near(terrain, min, max));
        }
        near
    }
}

/// As the game's own physics takes the level: zone portals and the sky's backdrop do not stop a piece.
fn bsp_triangles(model: &Model) -> Vec<[[f32; 3]; 3]> {
    let skip = poly_flags::PORTAL | poly_flags::FAKE_BACKDROP;
    model
        .polygons()
        .into_iter()
        .filter(|polygon| polygon.poly_flags & skip == 0 && model.nodes.get(polygon.node).is_some_and(|node| node.node_flags & NOT_CSG == 0))
        .flat_map(|polygon| fan_triangles(polygon.positions.len()).map(move |triangle| triangle.map(|i| polygon.positions[i])))
        .collect()
}

fn reaching(triangles: &[[[f32; 3]; 3]], min: [f32; 3], max: [f32; 3]) -> impl Iterator<Item = [[f32; 3]; 3]> {
    triangles.iter().copied().filter(move |triangle| p2_collision::triangle_reaches(min.into(), max.into(), triangle))
}

fn cells_of(min: [f32; 3], max: [f32; 3]) -> [RangeInclusive<i32>; 3] {
    let cell = |value: f32| (value / CELL).floor() as i32;
    std::array::from_fn(|k| cell(min[k])..=cell(max[k]))
}

fn each_cell([xs, ys, zs]: [RangeInclusive<i32>; 3]) -> impl Iterator<Item = [i32; 3]> {
    xs.flat_map(move |x| {
        let zs = zs.clone();
        ys.clone().flat_map(move |y| zs.clone().map(move |z| [x, y, z]))
    })
}

/// The map's BSP triangles, sorted once into a grid so a box tests only those near it.
#[derive(Default)]
struct TriangleGrid {
    triangles: Vec<[[f32; 3]; 3]>,
    cells: FxHashMap<[i32; 3], Vec<u32>>,
    wide: Vec<u32>,
}

impl TriangleGrid {
    fn new(triangles: Vec<[[f32; 3]; 3]>) -> Self {
        let mut grid = Self::default();
        for (at, triangle) in triangles.iter().enumerate() {
            let min = std::array::from_fn(|k| triangle.iter().map(|point| point[k]).fold(f32::INFINITY, f32::min));
            let max = std::array::from_fn(|k| triangle.iter().map(|point| point[k]).fold(f32::NEG_INFINITY, f32::max));
            let span = cells_of(min, max);
            let count: i64 = span.iter().map(|range| (i64::from(*range.end()) - i64::from(*range.start()) + 1).max(0)).product();
            if count > CELLS_MOST {
                grid.wide.push(at as u32);
                continue;
            }
            for cell in each_cell(span) {
                grid.cells.entry(cell).or_default().push(at as u32);
            }
        }
        grid.triangles = triangles;
        grid
    }

    /// The same triangles in the same order as `reaching` over the whole list.
    fn reaching(&self, min: [f32; 3], max: [f32; 3]) -> impl Iterator<Item = [[f32; 3]; 3]> {
        let mut listed = self.wide.clone();
        for cell in each_cell(cells_of(min, max)) {
            listed.extend(self.cells.get(&cell).into_iter().flatten());
        }
        listed.sort_unstable();
        listed.dedup();
        listed
            .into_iter()
            .filter_map(move |at| self.triangles.get(at as usize).copied())
            .filter(move |triangle| p2_collision::triangle_reaches(min.into(), max.into(), triangle))
    }
}

fn terrain_near(terrain: &Terrain, min: [f32; 3], max: [f32; 3]) -> Vec<[[f32; 3]; 3]> {
    if terrain.vertices.len() != terrain.width * terrain.height {
        return Vec::new();
    }
    let span = |k: usize, size: usize| {
        let cell = |value: f32| (value - terrain.location[k]) / terrain.scale[k] + (size / 2) as f32;
        let (a, b, last) = (cell(min[k]), cell(max[k]), size.saturating_sub(2) as f32);
        (a.min(b).floor().clamp(0.0, last) as usize)..=(a.max(b).floor().clamp(0.0, last) as usize)
    };
    let (xs, ys) = (span(0, terrain.width), span(1, terrain.height));
    let triangles: Vec<[[f32; 3]; 3]> =
        ys.flat_map(|y| xs.clone().map(move |x| (x, y))).flat_map(|(x, y)| terrain.quad_triangles(x, y)).map(|triangle| triangle.map(|i| terrain.vertices[i])).collect();
    reaching(&triangles, min, max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu_particles::next_unit;
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Indices, PrimitiveTopology};
    use bevy::render::render_resource::{Extent3d, TextureDimension};

    #[test]
    fn the_mean_is_taken_in_linear_light() {
        let size = Extent3d { width: 2, height: 1, depth_or_array_layers: 1 };
        let pixels = vec![255, 0, 0, 255, 0, 0, 0, 255];
        let image = Image::new(size, TextureDimension::D2, pixels, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default());
        assert_eq!(mean_albedo(&image), [0.5, 0.0, 0.0]);
        let mut unknown = image;
        unknown.texture_descriptor.format = TextureFormat::R8Unorm;
        assert_eq!(mean_albedo(&unknown), GREY_ALBEDO);
    }

    #[test]
    fn masked_triangles_are_placed_and_keep_their_uvs() {
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
        mesh.insert_indices(Indices::U16(vec![0, 2, 1]));
        let triangles = masked_triangles(&mesh, &Transform::from_xyz(10.0, 0.0, 0.0));
        assert_eq!(triangles, vec![[[10.0, 0.0, 0.0, 0.0, 0.0], [10.0, 1.0, 0.0, 0.0, 1.0], [11.0, 0.0, 0.0, 1.0, 0.0]]]);
    }

    #[test]
    fn a_box_of_level_triangles_picks_those_reaching_into_it() {
        let inside = [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]];
        let crossing = [[400.0, 0.0, 0.0], [5000.0, 0.0, 0.0], [400.0, 10.0, 0.0]];
        let far = [[9000.0, 9000.0, 0.0], [9010.0, 9000.0, 0.0], [9000.0, 9010.0, 0.0]];
        let above = [[0.0, 0.0, 2000.0], [10.0, 0.0, 2000.0], [0.0, 10.0, 2000.0]];
        let picked: Vec<[[f32; 3]; 3]> = reaching(&[inside, crossing, far, above], [-CELL; 3], [CELL; 3]).collect();
        assert_eq!(picked, vec![inside, crossing]);
    }

    #[test]
    fn the_grid_gives_the_triangles_the_whole_list_does() {
        let mut seed = 11;
        let mut unit = || next_unit(&mut seed) * 2.0 - 1.0;
        let mut soup: Vec<[[f32; 3]; 3]> = (0..400)
            .map(|_| {
                let corner = [unit() * 3000.0, unit() * 3000.0, unit() * 800.0];
                let size = unit().abs() * 900.0;
                [corner, [corner[0] + size * unit(), corner[1] + size * unit(), corner[2]], [corner[0], corner[1] + size * unit(), corner[2] + size * unit()]]
            })
            .collect();
        soup.push([[-1e6, -1e6, 0.0], [1e6, -1e6, 0.0], [0.0, 1e6, 1e6]]);
        soup.push([[CELL, 0.0, 0.0], [CELL, 10.0, 0.0], [CELL, 0.0, 10.0]]);
        let grid = TriangleGrid::new(soup.clone());
        assert_eq!(grid.wide.len(), 1);
        let centres: [[f32; 3]; 6] = [[0.0; 3], [2.0 * CELL, 0.0, 0.0], [-1234.5, 777.0, 100.0], [2900.0, -2900.0, -700.0], [9000.0, 9000.0, 0.0], [10.0, -20.0, 300.0]];
        for centre in centres {
            let (min, max) = (centre.map(|c| c - CELL), centre.map(|c| c + CELL));
            let linear: Vec<[[f32; 3]; 3]> = reaching(&soup, min, max).collect();
            assert_eq!(grid.reaching(min, max).collect::<Vec<_>>(), linear, "around {centre:?}");
        }
    }
}
