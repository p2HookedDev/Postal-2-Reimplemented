use bevy::asset::RenderAssetUsages;
use bevy::math::Vec3;
use bevy::mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues};
use bevy::transform::components::Transform;
use p2_assets::{ModelPolygon, StaticMesh, fan_triangles};
use crate::coords::to_bevy;

/// One mesh per section, so each can get its own material. Index order is kept: after the axis swap, Unreal's front
/// faces match Bevy's (checked visually).
pub fn static_mesh_to_meshes(mesh: &StaticMesh) -> Vec<Mesh> {
    let positions: Vec<_> = mesh.positions.iter().copied().map(to_bevy).collect();
    let normals: Vec<_> = mesh.normals.iter().copied().map(to_bevy).collect();
    mesh.sections
        .iter()
        .map(|s| {
            let start = usize::from(s.first_index);
            let end = (start + usize::from(s.num_primitives) * 3).min(mesh.indices.len());
            let indices = mesh.indices.get(start..end).unwrap_or_default().to_vec();

            let mut out = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
            out.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone());
            out.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals.clone());
            if let Some(uvs) = mesh.uvs.first() {
                out.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs.clone());
            }
            out.insert_indices(Indices::U16(indices));
            out
        })
        .collect()
}

/// Whether `transform` mirrors (negative `DrawScale3D` product): its triangles turn round, so the engine flips the cull
/// mode (`CM_CCW`) and we reverse their winding (Bevy culls by winding alone).
pub fn mirrors(transform: &Transform) -> bool {
    transform.scale.x * transform.scale.y * transform.scale.z < 0.0
}

#[derive(Default)]
pub struct MeshBatch {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
    remap: Vec<u32>,
}

impl MeshBatch {
    pub fn push(&mut self, mesh: &Mesh, transform: &Transform, colors: Option<&[[f32; 4]]>) {
        let attribute = |id| match mesh.attribute(id) {
            Some(VertexAttributeValues::Float32x3(values)) => values.as_slice(),
            _ => &[],
        };
        let (positions, normals) = (attribute(Mesh::ATTRIBUTE_POSITION), attribute(Mesh::ATTRIBUTE_NORMAL));
        let uvs = match mesh.attribute(Mesh::ATTRIBUTE_UV_0) {
            Some(VertexAttributeValues::Float32x2(values)) => Some(values.as_slice()),
            _ => None,
        };
        let Some(indices) = mesh.indices() else { return };
        let matrix = transform.to_matrix();
        let first = self.indices.len();
        let remap = &mut self.remap;
        remap.clear();
        remap.resize(positions.len(), u32::MAX);
        for index in indices.iter() {
            let Some(&position) = positions.get(index) else { continue };
            if remap[index] == u32::MAX {
                remap[index] = self.positions.len() as u32;
                self.positions.push(matrix.transform_point3(Vec3::from(position)).to_array());
                let normal = Vec3::from(normals.get(index).copied().unwrap_or([0.0, 1.0, 0.0]));
                self.normals.push((transform.rotation * (normal / transform.scale)).normalize_or(normal).to_array());
                if let Some(uvs) = uvs {
                    self.uvs.push(uvs.get(index).copied().unwrap_or_default());
                }
                if let Some(colors) = colors {
                    self.colors.push(colors.get(index).copied().unwrap_or([1.0; 4]));
                }
            }
            self.indices.push(remap[index]);
        }
        if mirrors(transform) {
            self.indices[first..].as_chunks_mut::<3>().0.iter_mut().for_each(|triangle| triangle.swap(1, 2));
        }
    }

    pub fn len(&self) -> usize {
        self.positions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    pub fn into_mesh(self) -> Mesh {
        let mut out = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
        out.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        out.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals);
        if !self.uvs.is_empty() {
            out.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs);
        }
        if !self.colors.is_empty() {
            out.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.colors);
        }
        out.insert_indices(Indices::U32(self.indices));
        out
    }
}

/// `texel_scale` turns the polygons' texel UVs into texture UVs: 1 / (width, height) of the texture. Light map atlas
/// UVs are stored as `UV_1` when every polygon has them. Unlike static meshes, BSP vertex order must be reversed after
/// the axis swap to face front (checked visually: `Church.fuk` buildings and ground).
pub fn model_polygons_to_mesh<'a>(
    polygons: impl IntoIterator<Item = (&'a ModelPolygon, Option<&'a [[f32; 2]]>)>,
    texel_scale: [f32; 2],
) -> Mesh {
    let (mut positions, mut normals, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut light_map_uvs = Some(Vec::new());
    for (polygon, light_map) in polygons {
        let first = positions.len() as u32;
        let normal = to_bevy(polygon.normal);
        for (&position, &[u, v]) in polygon.positions.iter().zip(&polygon.uvs) {
            positions.push(to_bevy(position));
            normals.push(normal);
            uvs.push([u * texel_scale[0], v * texel_scale[1]]);
        }
        match (light_map, &mut light_map_uvs) {
            (Some(light_map), Some(all)) if light_map.len() == polygon.positions.len() => all.extend_from_slice(light_map),
            _ => light_map_uvs = None,
        }
        indices.extend(fan_triangles(polygon.positions.len()).flat_map(|[a, b, c]| [a, c, b]).map(|i| first + i as u32));
    }
    let mut out = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    out.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    out.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    out.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    if let Some(light_map_uvs) = light_map_uvs.filter(|uvs| !uvs.is_empty()) {
        out.insert_attribute(Mesh::ATTRIBUTE_UV_1, light_map_uvs);
    }
    out.insert_indices(Indices::U32(indices));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::unreal_transform;

    #[test]
    fn static_mesh_axes_and_sections() {
        use bevy::mesh::VertexAttributeValues;
        use p2_assets::{MeshSection, StaticMesh};

        let section = |first_index, num_primitives| MeshSection {
            is_strip: false,
            first_index,
            min_vertex_index: 0,
            max_vertex_index: 2,
            num_triangles: num_primitives + 1,
            num_primitives,
        };
        let mesh = StaticMesh {
            physics_props: 0,
            sections: vec![section(0, 1), section(3, 0)],
            positions: vec![[1.0, 2.0, 3.0], [0.0; 3], [0.0; 3]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            colors: vec![],
            use_vertex_color: false,
            uvs: vec![],
            indices: vec![0, 1, 2, 2, 1, 0],
            ..StaticMesh::default()
        };
        let out = static_mesh_to_meshes(&mesh);
        assert_eq!(out.len(), 2);
        let Some(VertexAttributeValues::Float32x3(positions)) = out[0].attribute(Mesh::ATTRIBUTE_POSITION) else {
            panic!("positions missing");
        };
        assert_eq!(positions[0], [1.0, 3.0, 2.0]);
        assert_eq!(out[0].indices().unwrap().iter().collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(out[1].indices().unwrap().len(), 0);
    }

    #[test]
    fn batch_places_and_compacts() {
        let mut section = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
        section.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [9.0, 9.0, 9.0]]);
        section.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 4]);
        section.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [5.0, 5.0]]);
        section.insert_indices(Indices::U16(vec![0, 2, 1]));
        let mut batch = MeshBatch::default();
        batch.push(&section, &Transform::from_xyz(10.0, 0.0, 0.0), Some(&[[0.5; 4]; 4]));
        batch.push(&section, &Transform::from_scale(Vec3::new(2.0, 1.0, 1.0)), Some(&[[1.0; 4]; 4]));
        assert_eq!(batch.len(), 6);
        let mesh = batch.into_mesh();
        assert_eq!(mesh.indices().unwrap().iter().collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5]);
        let Some(VertexAttributeValues::Float32x3(positions)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else { panic!() };
        assert_eq!(positions[..3], [[10.0, 0.0, 0.0], [10.0, 1.0, 0.0], [11.0, 0.0, 0.0]]);
        assert_eq!(positions[5], [2.0, 0.0, 0.0]);
        let Some(VertexAttributeValues::Float32x4(colors)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR) else { panic!() };
        assert_eq!((colors[0], colors[3]), ([0.5; 4], [1.0; 4]));
    }

    #[test]
    fn batch_reverses_mirrored_winding() {
        let mut section = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
        section.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
        section.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 3]);
        section.insert_indices(Indices::U16(vec![0, 2, 1]));
        let mirrored = unreal_transform([0.0; 3].into(), [0; 3].into(), [-1.0, 1.0, 1.0].into(), [0.0; 3].into());
        assert!(mirrors(&mirrored) && !mirrors(&Transform::IDENTITY));
        let mut batch = MeshBatch::default();
        batch.push(&section, &Transform::IDENTITY, None);
        batch.push(&section, &mirrored, None);
        assert_eq!(batch.into_mesh().indices().unwrap().iter().collect::<Vec<_>>(), [0, 1, 2, 3, 5, 4]);
    }
}
