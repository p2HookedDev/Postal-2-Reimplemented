use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology};
use wgpu_types::TextureFormat as GpuFormat;
use crate::coords::to_bevy;
use crate::texture::{image_2d, with_mips};

pub struct SectorVertices<'a> {
    triangles: &'a [[usize; 3]],
    used: Vec<usize>,
    remap: Vec<u32>,
}

impl<'a> SectorVertices<'a> {
    pub fn new(triangles: &'a [[usize; 3]], vertex_count: usize) -> Self {
        let used = terrain_sector_vertices(triangles, vertex_count);
        let mut remap = vec![u32::MAX; vertex_count];
        for (at, &i) in used.iter().enumerate() {
            remap[i] = at as u32;
        }
        Self { triangles, used, remap }
    }
}

/// Vertex colour: display-space lighting bytes / 255 (none: 0.5), applied per pixel as `Modulate2X`.
/// Holds all vertices, drawn or not: equal bounds per layer keep Bevy's blended sort in bias order.
pub fn terrain_layer_mesh(
    vertices: &[[f32; 3]],
    normals: &[[f32; 3]],
    sector: &SectorVertices,
    uvs: &[[f32; 2]],
    alpha_uvs: &[[f32; 2]],
    lighting: Option<&[[u8; 4]]>,
    alpha: Option<&[f32]>,
) -> Mesh {
    let opaque = |i: usize| alpha.is_none_or(|alpha| alpha.get(i).is_some_and(|&a| a > 0.0));
    let SectorVertices { triangles, used, remap } = sector;
    let indices: Vec<u32> =
        triangles.iter().filter(|triangle| triangle.iter().any(|&i| opaque(i))).flatten().map(|&i| remap[i]).collect();
    let colors: Vec<[f32; 4]> = used.iter().map(|&i| terrain_vertex_color(lighting, i)).collect();
    let uv = |uvs: &[[f32; 2]]| used.iter().map(|&i| uvs.get(i).copied().unwrap_or_default()).collect::<Vec<_>>();
    let mut out = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    out.insert_attribute(Mesh::ATTRIBUTE_POSITION, used.iter().map(|&i| to_bevy(vertices[i])).collect::<Vec<_>>());
    out.insert_attribute(Mesh::ATTRIBUTE_NORMAL, used.iter().map(|&i| to_bevy(normals.get(i).copied().unwrap_or([0.0, 0.0, 1.0]))).collect::<Vec<_>>());
    out.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv(uvs));
    out.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv(alpha_uvs));
    out.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    out.insert_indices(Indices::U32(indices));
    out
}

pub fn terrain_sector_vertices(triangles: &[[usize; 3]], vertex_count: usize) -> Vec<usize> {
    let mut taken = vec![false; vertex_count];
    triangles.iter().flatten().copied().filter(|&i| !std::mem::replace(&mut taken[i], true)).collect()
}

pub fn terrain_vertex_color(lighting: Option<&[[u8; 4]]>, index: usize) -> [f32; 4] {
    let [r, g, b] = lighting.and_then(|l| l.get(index)).map_or([0.5; 3], |&[r, g, b, _]| [r, g, b].map(|c| f32::from(c) / 255.0));
    [r, g, b, 0.0]
}

pub fn alpha_map_image(width: u32, height: u32, alpha: &[u8]) -> Image {
    let mips = with_mips(width, height, alpha.iter().map(|&a| [a]).collect::<Vec<[u8; 1]>>());
    image_2d([width, height, 1], GpuFormat::R8Unorm, mips, ImageSampler::Descriptor(ImageSamplerDescriptor::linear()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec3;
    use bevy::mesh::VertexAttributeValues;

    #[test]
    fn terrain_sector_vertices_keep_first_use_order() {
        assert_eq!(terrain_sector_vertices(&[[2, 0, 3], [0, 3, 1]], 4), vec![2, 0, 3, 1]);
    }

    #[test]
    fn terrain_layers_share_their_sector_bounds() {
        let vertices = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]];
        let triangles = [[0, 2, 3], [0, 3, 1]];
        let positions = |alpha: &[f32]| {
            let mesh = terrain_layer_mesh(&vertices, &[], &SectorVertices::new(&triangles, vertices.len()), &[[0.0; 2]; 4], &[], None, Some(alpha));
            let indices = mesh.indices().map_or(0, |indices| indices.len());
            let Some(VertexAttributeValues::Float32x3(positions)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else { panic!() };
            (positions.clone(), indices)
        };
        let (full, full_indices) = positions(&[1.0; 4]);
        let (corner, corner_indices) = positions(&[0.0, 1.0, 0.0, 0.0]);
        assert_eq!(full, corner);
        assert_eq!((full_indices, corner_indices), (6, 3));
    }

    #[test]
    fn terrain_lighting_is_raw() {
        let vertices = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let colors = |lighting: Option<&[[u8; 4]]>| {
            let mesh = terrain_layer_mesh(&vertices, &[], &SectorVertices::new(&[[0, 1, 2]], vertices.len()), &[], &[], lighting, None);
            let Some(VertexAttributeValues::Float32x4(colors)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR) else { panic!() };
            colors.clone()
        };
        let lit = colors(Some(&[[255, 128, 0, 7]; 3][..]));
        assert_eq!(lit[0], [1.0, 128.0 / 255.0, 0.0, 0.0]);
        assert_eq!(colors(None)[0], [0.5, 0.5, 0.5, 0.0]);
    }

    #[test]
    fn alpha_maps_have_mips() {
        let image = alpha_map_image(4, 2, &[0, 255, 255, 255, 0, 255, 255, 255]);
        assert_eq!(image.texture_descriptor.format, GpuFormat::R8Unorm);
        assert_eq!(image.texture_descriptor.mip_level_count, 2);
        assert_eq!(image.data.unwrap()[8..], [128, 255]);
    }

    #[test]
    fn terrain_faces_up() {
        use bevy::mesh::VertexAttributeValues;

        let vertices = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]];
        let triangles = [[0, 2, 3], [0, 3, 1]];
        let mesh = terrain_layer_mesh(&vertices, &[], &SectorVertices::new(&triangles, vertices.len()), &[[0.0; 2]; 4], &[], None, None);
        let Some(VertexAttributeValues::Float32x3(positions)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else {
            panic!("positions missing");
        };
        let indices: Vec<usize> = mesh.indices().unwrap().iter().collect();
        for triangle in indices.chunks(3) {
            let [a, b, c] = [0, 1, 2].map(|k| Vec3::from(positions[triangle[k]]));
            assert!((b - a).cross(c - a).y > 0.0, "triangle {triangle:?} faces down");
        }
    }
}
