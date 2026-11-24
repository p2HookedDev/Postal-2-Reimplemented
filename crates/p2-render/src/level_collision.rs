use std::sync::Arc;

use bevy::asset::Handle;
use bevy::ecs::prelude::Resource;
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::mesh::{Mesh, VertexAttributeValues};
use bevy::render::render_resource::TextureFormat;
use bevy::transform::components::Transform;
use p2_assets::{Model, Terrain};
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
