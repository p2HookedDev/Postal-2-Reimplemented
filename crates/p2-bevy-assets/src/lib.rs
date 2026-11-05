use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::{Mat4, Quat, Vec3, Vec4};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues};
use bevy::transform::components::Transform;
use p2_assets::{ModelPolygon, SkeletalMesh, StaticMesh, Texture, TextureFormat, fan_triangles};
use p2_math::{Rotator, Vector};
use wgpu_types::{Extent3d, TextureDimension, TextureFormat as GpuFormat};

/// Unreal (X forward, Y right, Z up, left-handed) to Bevy (Y up, right-handed): swap Y and Z.
pub fn to_bevy(v: impl Into<[f32; 3]>) -> [f32; 3] {
    let [x, y, z] = v.into();
    [x, z, y]
}

fn unreal_vec3(v: Vector) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

pub fn unreal_transform(location: Vector, rotation: Rotator, scale: Vector, pre_pivot: Vector) -> Transform {
    let [x, y, z] = rotation.axes().map(unreal_vec3);
    let (x, y, z) = (x * scale.x, y * scale.y, z * scale.z);
    let Vector { x: px, y: py, z: pz } = pre_pivot;
    let translation = unreal_vec3(location) - (x * px + y * py + z * pz);
    let unreal = Mat4::from_cols(x.extend(0.0), y.extend(0.0), z.extend(0.0), translation.extend(1.0));
    unreal_to_bevy(unreal)
}

pub fn rotator_matrix(rotation: Rotator) -> Mat4 {
    let [x, y, z] = rotation.axes().map(|axis| unreal_vec3(axis).extend(0.0));
    Mat4::from_cols(x, y, z, Vec4::W)
}

/// world = `rotation` · R(`rot_origin`) · (scale ⊙ (v − `origin`)) + location + pre-pivot,
/// with scale = `DrawScale3D` · `DrawScale` · the mesh's `Scale`.
pub fn unreal_mesh_matrix(
    location: Vector,
    rotation: Mat4,
    scale: Vector,
    pre_pivot: Vector,
    mesh_scale: Vector,
    origin: Vector,
    rot_origin: Rotator,
) -> Mat4 {
    let scale = unreal_vec3(scale) * unreal_vec3(mesh_scale);
    Mat4::from_translation(unreal_vec3(location) + unreal_vec3(pre_pivot))
        * rotation
        * rotator_matrix(rot_origin)
        * Mat4::from_scale(scale)
        * Mat4::from_translation(-unreal_vec3(origin))
}

const AXIS_SWAP: Mat4 = Mat4::from_cols(Vec4::X, Vec4::Z, Vec4::Y, Vec4::W);

pub fn unreal_to_bevy(matrix: Mat4) -> Transform {
    Transform::from_matrix(AXIS_SWAP * matrix * AXIS_SWAP)
}

pub fn unreal_mesh_transform(
    location: Vector,
    rotation: Rotator,
    scale: Vector,
    pre_pivot: Vector,
    mesh_scale: Vector,
    origin: Vector,
    rot_origin: Rotator,
) -> Transform {
    unreal_to_bevy(unreal_mesh_matrix(location, rotator_matrix(rotation), scale, pre_pivot, mesh_scale, origin, rot_origin))
}
/// Mesh-space matrices (Unreal space), parents before children. As in the engine, the root's quaternion is used as
/// stored and every other bone's conjugated.
pub fn bone_matrices(parents: &[usize], local: &[([f32; 4], [f32; 3])]) -> Vec<Mat4> {
    scaled_bone_matrices(parents, local, |_| 1.0)
}

pub fn scaled_bone_matrices(parents: &[usize], local: &[([f32; 4], [f32; 3])], scale: impl Fn(usize) -> f32) -> Vec<Mat4> {
    let mut world = Vec::with_capacity(local.len());
    scaled_bone_matrices_into(parents, local, scale, &mut world);
    world
}

pub fn scaled_bone_matrices_into(parents: &[usize], local: &[([f32; 4], [f32; 3])], scale: impl Fn(usize) -> f32, world: &mut Vec<Mat4>) {
    world.clear();
    for (bone, &([x, y, z, w], position)) in local.iter().enumerate() {
        let rotation = Quat::from_xyzw(x, y, z, w).normalize();
        let rotation = if bone == 0 { rotation } else { rotation.conjugate() };
        let matrix = Mat4::from_scale_rotation_translation(Vec3::splat(scale(bone)), rotation, Vec3::from(position));
        let parent = parents.get(bone).copied().filter(|&p| p < bone).and_then(|p| world.get(p));
        let placed = parent.map_or(matrix, |parent| *parent * matrix);
        world.push(placed);
    }
}

pub fn skin_inverse_bindposes(mesh: &SkeletalMesh) -> Vec<Mat4> {
    skin_bind_matrices(mesh).iter().map(|bind| AXIS_SWAP * bind.inverse() * AXIS_SWAP).collect()
}

pub fn skin_bind_transforms(mesh: &SkeletalMesh) -> Vec<Transform> {
    skin_bind_matrices(mesh).into_iter().map(unreal_to_bevy).collect()
}

fn skin_bind_matrices(mesh: &SkeletalMesh) -> Vec<Mat4> {
    let parents: Vec<usize> = mesh.bones.iter().map(|b| b.parent).collect();
    let local: Vec<_> = mesh.bones.iter().map(|b| (b.orientation, b.position)).collect();
    bone_matrices(&parents, &local)
}

pub fn skin_points(mesh: &SkeletalMesh, inverse_bind: &[Mat4], pose: &[Mat4]) -> Vec<[f32; 3]> {
    let moves: Vec<Mat4> = inverse_bind.iter().zip(pose).map(|(inverse, pose)| *pose * *inverse).collect();
    mesh.positions
        .iter()
        .zip(mesh.point_influences())
        .map(|(&point, influences)| {
            let point = Vec3::from(point);
            let skinned: Vec3 = influences
                .iter()
                .filter_map(|i| moves.get(usize::from(i.bone)).map(|m| m.transform_point3(point) * i.weight))
                .sum();
            skinned.to_array()
        })
        .collect()
}
/// Index order is kept, like static meshes.
/// The meshes stay in the main world too, so animation can move them.
pub fn skeletal_mesh_to_meshes(mesh: &SkeletalMesh, gpu_skin: bool) -> Vec<(Mesh, u16)> {
    let position = |point: u32| mesh.positions.get(point as usize).copied().unwrap_or_default();
    let mut point_normals = vec![Vec3::ZERO; mesh.positions.len()];
    for face in mesh.indices.as_chunks::<3>().0 {
        let points: Vec<u32> = face.iter().filter_map(|&w| mesh.wedges.get(usize::from(w))).map(|w| w.point).collect();
        if let [a, b, c] = points[..] {
            let [pa, pb, pc] = [a, b, c].map(|p| Vec3::from(to_bevy(position(p))));
            let normal = (pb - pa).cross(pc - pa);
            for p in [a, b, c] {
                if let Some(n) = point_normals.get_mut(p as usize) {
                    *n += normal;
                }
            }
        }
    }
    let positions: Vec<[f32; 3]> = mesh.wedges.iter().map(|w| to_bevy(position(w.point))).collect();
    let normals: Vec<[f32; 3]> = mesh
        .wedges
        .iter()
        .map(|w| point_normals.get(w.point as usize).copied().unwrap_or(Vec3::Y).normalize_or(Vec3::Y).to_array())
        .collect();
    let uvs: Vec<[f32; 2]> = mesh.wedges.iter().map(|w| w.uv).collect();
    let (joints, weights): (Vec<[u16; 4]>, Vec<[f32; 4]>) = if gpu_skin {
        mesh
            .wedges
            .iter()
            .map(|w| {
                let mut influences = mesh.influences_of(w.point as usize).to_vec();
                influences.sort_by(|a, b| b.weight.total_cmp(&a.weight));
                let (mut joints, mut weights) = ([0u16; 4], [0f32; 4]);
                for (i, influence) in influences.iter().take(4).enumerate() {
                    (joints[i], weights[i]) = (influence.bone, influence.weight);
                }
                let total: f32 = weights.iter().sum();
                if total > 0.0 {
                    weights = weights.map(|w| w / total);
                } else {
                    weights[0] = 1.0;
                }
                (joints, weights)
            })
            .unzip()
    } else {
        (Vec::new(), Vec::new())
    };
    mesh.sections
        .iter()
        .map(|section| {
            let start = usize::from(section.first_face) * 3;
            let end = (start + usize::from(section.num_faces) * 3).min(mesh.indices.len());
            let indices = mesh.indices.get(start..end).unwrap_or_default().to_vec();
            let mut out = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
            out.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone());
            out.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals.clone());
            out.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs.clone());
            // Only for meshes drawn with a `SkinnedMesh`: Bevy draws any mesh with joints as skinned.
            if gpu_skin {
                out.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(joints.clone()));
                out.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, weights.clone());
            }
            out.insert_indices(Indices::U16(indices));
            if gpu_skin {
                let _ = out.generate_skinned_mesh_bounds();
            }
            (out, section.material)
        })
        .collect()
}
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
        let mut remap = vec![u32::MAX; positions.len()];
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

/// `Modulate2X` in linear space: the engine draws all baked lighting (BSP light maps, terrain and static mesh vertex
/// colours) as texture × 2 × lighting in display (gamma) space, so 128 is full brightness. As a factor on a linear
/// (sRGB-decoded) texture: (2 · lighting)^2.2 (unclamped; the output clamps).
pub const MODULATE_2X: f32 = 4.594_794;

pub fn modulate_2x(byte: u8) -> f32 {
    static TABLE: std::sync::LazyLock<[f32; 256]> =
        std::sync::LazyLock::new(|| std::array::from_fn(|byte| (2.0 * byte as f32 / 255.0).powf(2.2)));
    TABLE[usize::from(byte)]
}

pub fn lighting_colors(colors: &[[u8; 4]]) -> Vec<[f32; 4]> {
    colors.iter().map(|&[r, g, b, _]| [modulate_2x(r), modulate_2x(g), modulate_2x(b), 1.0]).collect()
}

pub fn set_vertex_colors(mesh: &mut Mesh, colors: &[[f32; 4]]) {
    if let Some(VertexAttributeValues::Float32x4(current)) = mesh.attribute_mut(Mesh::ATTRIBUTE_COLOR)
        && current.len() == colors.len()
    {
        current.copy_from_slice(colors);
        return;
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors.to_vec());
}

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
    let (data, levels) = with_mips(width, height, alpha.iter().map(|&a| [a]).collect::<Vec<[u8; 1]>>());
    let size = Extent3d { width, height, depth_or_array_layers: 1 };
    let mut image = Image::new_uninit(size, TextureDimension::D2, GpuFormat::R8Unorm, RenderAssetUsages::RENDER_WORLD);
    image.texture_descriptor.mip_level_count = levels;
    image.data = Some(data);
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::linear());
    image
}

const LOD_SET_LABEL: &str = "lod set ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureQuality {
    pub detail: [u8; 5],
    pub min_lod: i32,
    pub max_lod: i32,
    pub anisotropy: u16,
    pub trilinear: bool,
    pub detail_textures: bool,
}

impl Default for TextureQuality {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TextureQuality {
    const DEFAULT: Self = Self { detail: [0; 5], min_lod: 0, max_lod: i32::MAX, anisotropy: 1, trilinear: true, detail_textures: true };

    pub fn detail_level(text: &str) -> u8 {
        let text = text.trim();
        let named = ["high", "medium", "low", "ultralow"].iter().position(|name| name.eq_ignore_ascii_case(text));
        named.map_or_else(|| text.parse().unwrap_or(0).min(3), |index| index as u8)
    }

    pub fn skipped_mips(&self, lod_set: u8, mips: u32) -> u32 {
        let Some(&detail) = usize::from(lod_set).checked_sub(1).and_then(|set| self.detail.get(set)) else { return 0 };
        let mips = mips as i32;
        let mut skip = i32::from(detail).min(mips - 1);
        if mips - skip < self.min_lod {
            skip = (mips - self.min_lod).max(0);
        }
        if self.max_lod < mips - skip {
            skip = skip.max(mips - self.max_lod);
        }
        skip.clamp(0, (mips - 1).max(0)) as u32
    }

    /// `None` for an image whose sampler [`Self::sampler`] did not make.
    pub fn refreshed_sampler(&self, image: &Image) -> Option<ImageSamplerDescriptor> {
        let ImageSampler::Descriptor(now) = &image.sampler else { return None };
        let lod_set = now.label.as_deref()?.strip_prefix(LOD_SET_LABEL)?.parse().ok()?;
        let wrap = [now.address_mode_u, now.address_mode_v];
        match self.sampler(lod_set, image.texture_descriptor.mip_level_count, wrap) {
            ImageSampler::Descriptor(descriptor) => Some(descriptor),
            _ => None,
        }
    }

    /// Skipped mips become the lowest level used. `UseTrilinear` picks the mip filter, `LevelOfAnisotropy`
    /// the min and mag filter; Bevy needs every filter linear then.
    pub fn sampler(&self, lod_set: u8, mips: u32, wrap: [ImageAddressMode; 2]) -> ImageSampler {
        let anisotropy = self.anisotropy.clamp(1, 16);
        let mipmap_filter = if self.trilinear || anisotropy > 1 { ImageFilterMode::Linear } else { ImageFilterMode::Nearest };
        ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: wrap[0],
            address_mode_v: wrap[1],
            mipmap_filter,
            lod_min_clamp: self.skipped_mips(lod_set, mips) as f32,
            anisotropy_clamp: anisotropy,
            label: Some(format!("{LOD_SET_LABEL}{lod_set}")),
            ..ImageSamplerDescriptor::linear()
        })
    }
}

/// The quality new images are made with; `render::sampler_refresh` brings the ones already made up to it.
static QUALITY: Mutex<TextureQuality> = Mutex::new(TextureQuality::DEFAULT);

pub fn set_texture_quality(quality: TextureQuality) {
    *QUALITY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = quality;
}

pub fn texture_quality() -> TextureQuality {
    *QUALITY.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

static BLOCK_COMPRESSION: AtomicBool = AtomicBool::new(true);

pub fn set_block_compression(supported: bool) {
    BLOCK_COMPRESSION.store(supported, Ordering::Relaxed);
}

pub fn texture_to_image(texture: &Texture, palette: Option<&[[u8; 4]]>) -> Option<Image> {
    texture_image(texture, palette, BLOCK_COMPRESSION.load(Ordering::Relaxed))
}

pub fn texture_to_rgba_image(texture: &Texture, palette: Option<&[[u8; 4]]>) -> Option<Image> {
    texture_image(texture, palette, false)
}

fn texture_image(texture: &Texture, palette: Option<&[[u8; 4]]>, compressed: bool) -> Option<Image> {
    let base = texture.mips.first()?;
    let (format, block) = match texture.format {
        TextureFormat::Dxt1 if compressed => (GpuFormat::Bc1RgbaUnormSrgb, 4),
        TextureFormat::Dxt3 if compressed => (GpuFormat::Bc2RgbaUnormSrgb, 4),
        TextureFormat::Dxt5 if compressed => (GpuFormat::Bc3RgbaUnormSrgb, 4),
        TextureFormat::Dxt1 | TextureFormat::Dxt3 | TextureFormat::Dxt5 => (GpuFormat::Rgba8UnormSrgb, 4),
        TextureFormat::Rgba8 => (GpuFormat::Bgra8UnormSrgb, 1),
        TextureFormat::P8 | TextureFormat::G16 => (GpuFormat::Rgba8UnormSrgb, 1),
        _ => return None,
    };
    if base.width % block != 0 || base.height % block != 0 {
        return None;
    }
    let palette = match texture.format {
        TextureFormat::P8 => palette?,
        _ => &[],
    };

    let mut data = Vec::new();
    let mut levels = 0;
    for (level, mip) in texture.mips.iter().enumerate().take(32) {
        let expected = ((base.width >> level).max(1), (base.height >> level).max(1));
        if mip.data.is_empty() || (mip.width, mip.height) != expected || mip.data.len() < texture.format.mip_bytes(mip.width, mip.height) {
            break;
        }
        match texture.format {
            TextureFormat::Rgba8 => data.extend_from_slice(&mip.data),
            TextureFormat::Dxt1 | TextureFormat::Dxt3 | TextureFormat::Dxt5 if compressed => data.extend_from_slice(&mip.data),
            _ => {
                let Some((_, _, texels)) = p2_assets::mip_rgba(texture, level, Some(palette)) else { break };
                data.extend(texels.iter().flatten());
            }
        }
        levels += 1;
    }
    if levels == 0 {
        return None;
    }

    let size = Extent3d { width: base.width, height: base.height, depth_or_array_layers: 1 };
    let mut image = Image::new_uninit(size, TextureDimension::D2, format, RenderAssetUsages::RENDER_WORLD);
    image.texture_descriptor.mip_level_count = levels;
    image.data = Some(data);
    image.sampler = texture_quality().sampler(texture.lod_set, levels, [ImageAddressMode::Repeat; 2]);
    Some(image)
}

pub fn texture_with_opacity(
    texture: &Texture,
    palette: Option<&[[u8; 4]]>,
    opacity: &Texture,
    opacity_palette: Option<&[[u8; 4]]>,
) -> Option<Image> {
    let (width, height, mut texels) = p2_assets::texture_rgba(texture, palette)?;
    let (mask_width, mask_height, mask) = p2_assets::texture_rgba(opacity, opacity_palette)?;
    for (index, texel) in texels.iter_mut().enumerate() {
        let (x, y) = (index as u32 % width, index as u32 / width);
        let (mx, my) = (x * mask_width / width, y * mask_height / height);
        texel[3] = mask[(my * mask_width + mx) as usize][3];
    }
    Some(rgba_image(width, height, texels))
}

pub fn texture_combined(
    ops: &p2_assets::CombineOps,
    material1: (u32, u32, &[[u8; 4]]),
    material2: (u32, u32, &[[u8; 4]]),
    mask: Option<(u32, u32, &[[u8; 4]])>,
) -> Image {
    let (width, height) = if material2.0 * material2.1 > material1.0 * material1.1 { (material2.0, material2.1) } else { (material1.0, material1.1) };
    let at = |(w, h, texels): (u32, u32, &[[u8; 4]]), x: u32, y: u32| texels[((y * h / height) * w + x * w / width) as usize];
    let texels = (0..width * height)
        .map(|i| {
            let (x, y) = (i % width, i / width);
            ops.texel(at(material1, x, y), at(material2, x, y), mask.map(|m| at(m, x, y)))
        })
        .collect();
    rgba_image(width, height, texels)
}

pub fn texture_with_constant_alpha(texture: &Texture, palette: Option<&[[u8; 4]]>, alpha: u8) -> Option<Image> {
    let (width, height, mut texels) = p2_assets::texture_rgba(texture, palette)?;
    for texel in &mut texels {
        texel[3] = alpha;
    }
    Some(rgba_image(width, height, texels))
}

pub fn alpha_on_black_image(texture: &Texture, palette: Option<&[[u8; 4]]>) -> Option<Image> {
    let (width, height, mut texels) = p2_assets::texture_rgba(texture, palette)?;
    for texel in &mut texels {
        *texel = [0, 0, 0, texel[3]];
    }
    Some(rgba_image(width, height, texels))
}

/// Bakes masked self-illumination: base = `diffuse` × (1 − m), glow = `glow` × m, m = `mask` alpha.
/// Lit base plus glow equals the driver's lerp(lit diffuse, glow, m).
pub fn texture_glow(diffuse: (u32, u32, &[[u8; 4]]), glow: (u32, u32, &[[u8; 4]]), mask: (u32, u32, &[[u8; 4]])) -> (Image, Image) {
    let (width, height) = [diffuse, glow, mask].into_iter().map(|(w, h, _)| (w, h)).max_by_key(|&(w, h)| w * h).unwrap_or((1, 1));
    let at = |(w, h, texels): (u32, u32, &[[u8; 4]]), x: u32, y: u32| texels[((y * h / height) * w + x * w / width) as usize];
    let (base, lit): (Vec<_>, Vec<_>) = (0..width * height)
        .map(|i| {
            let (x, y) = (i % width, i / width);
            glow_texel(at(diffuse, x, y), at(glow, x, y), at(mask, x, y)[3])
        })
        .unzip();
    (rgba_image(width, height, base), rgba_image(width, height, lit))
}

fn glow_texel(diffuse: [u8; 4], glow: [u8; 4], m: u8) -> ([u8; 4], [u8; 4]) {
    let scale = |c: u8, f: u8| ((u16::from(c) * u16::from(f) + 127) / 255) as u8;
    let [r, g, b, _] = diffuse;
    let [gr, gg, gb, _] = glow;
    ([scale(r, 255 - m), scale(g, 255 - m), scale(b, 255 - m), m], [scale(gr, m), scale(gg, m), scale(gb, m), m])
}

pub fn cube_image(faces: [(u32, u32, &[[u8; 4]]); 6]) -> Image {
    let (width, height) = (faces[0].0.max(1), faces[0].1.max(1));
    let mut data = Vec::with_capacity((width * height * 24) as usize);
    for (w, h, texels) in faces {
        for i in 0..width * height {
            let (x, y) = (i % width, i / width);
            data.extend(texels.get(((y * h / height) * w + x * w / width) as usize).copied().unwrap_or([0; 4]));
        }
    }
    let size = Extent3d { width, height, depth_or_array_layers: 6 };
    let mut image = Image::new_uninit(size, TextureDimension::D2, GpuFormat::Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD);
    image.data = Some(data);
    image.texture_view_descriptor =
        Some(wgpu_types::TextureViewDescriptor { dimension: Some(wgpu_types::TextureViewDimension::Cube), ..Default::default() });
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::linear());
    image
}

fn rgba_image(width: u32, height: u32, texels: Vec<[u8; 4]>) -> Image {
    let (data, levels) = with_mips(width, height, texels);
    let size = Extent3d { width, height, depth_or_array_layers: 1 };
    let mut image = Image::new_uninit(size, TextureDimension::D2, GpuFormat::Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD);
    image.texture_descriptor.mip_level_count = levels;
    image.data = Some(data);
    image.sampler = texture_quality().sampler(0, levels, [ImageAddressMode::Repeat; 2]);
    image
}

fn with_mips<const N: usize>(width: u32, height: u32, mut texels: Vec<[u8; N]>) -> (Vec<u8>, u32) {
    let (mut w, mut h) = (width, height);
    let mut data: Vec<u8> = texels.iter().flatten().copied().collect();
    let mut levels = 1;
    while w > 1 && h > 1 && w % 2 == 0 && h % 2 == 0 {
        let previous = texels;
        let (nw, nh) = (w / 2, h / 2);
        texels = (0..nw * nh)
            .map(|i| {
                let (x, y) = (i % nw * 2, i / nw * 2);
                let at = |dx: u32, dy: u32, c: usize| previous.get(((y + dy) * w + x + dx) as usize).map_or(0, |texel| u32::from(texel[c]));
                std::array::from_fn(|c| ((at(0, 0, c) + at(1, 0, c) + at(0, 1, c) + at(1, 1, c) + 2) / 4) as u8)
            })
            .collect();
        data.extend(texels.iter().flatten());
        (w, h) = (nw, nh);
        levels += 1;
    }
    (data, levels)
}

/// Colour premultiplied by alpha (in gamma space), as egui blends (`One`, `OneMinusSrcAlpha`); straight alpha would add
/// the colour of transparent texels. `additive`: the colour as is with zero alpha, which egui adds. Box-filtered mips and
/// a trilinear sampler: a 512-pixel weapon icon drawn at ~40 pixels does not sparkle.
pub fn texture_to_ui_image(texture: &Texture, palette: Option<&[[u8; 4]]>, additive: bool) -> Option<Image> {
    let (width, height, texels) = p2_assets::texture_rgba(texture, palette)?;
    let texels = texels
        .iter()
        .map(|&[r, g, b, a]| {
            if additive {
                return [r, g, b, 0];
            }
            let premultiply = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
            [premultiply(r), premultiply(g), premultiply(b), a]
        })
        .collect();
    let mut image = rgba_image(width, height, texels);
    let mode = |clamp: bool| if clamp { ImageAddressMode::ClampToEdge } else { ImageAddressMode::Repeat };
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: mode(texture.clamp[0]),
        address_mode_v: mode(texture.clamp[1]),
        mipmap_filter: ImageFilterMode::Linear,
        ..ImageSamplerDescriptor::linear()
    });
    Some(image)
}

#[cfg(test)]
mod tests {
    use p2_assets::Mip;

    use super::*;

    fn mip(width: u32, height: u32, data: Vec<u8>) -> Mip {
        Mip { width, height, data }
    }

    #[test]
    fn opacity_replaces_alpha() {
        let rgba8 = |width, height, data| Texture { format: TextureFormat::Rgba8, palette: None, mips: vec![mip(width, height, data)], masked: false, clamp: [false; 2], lod_set: 0 };
        let texture = rgba8(2, 2, [[0, 0, 200, 255], [0, 0, 100, 255], [0, 0, 200, 255], [0, 0, 100, 255]].concat());
        let opacity = rgba8(1, 1, vec![9, 9, 9, 40]);
        let image = texture_with_opacity(&texture, None, &opacity, None).unwrap();
        assert_eq!(image.texture_descriptor.mip_level_count, 2);
        let data = image.data.unwrap();
        assert_eq!(&data[..8], [200, 0, 0, 40, 100, 0, 0, 40]);
        assert_eq!(&data[16..], [150, 0, 0, 40]);
    }

    #[test]
    fn constant_alpha_replaces_alpha() {
        let texture = Texture { format: TextureFormat::Rgba8, palette: None, mips: vec![mip(1, 1, vec![10, 20, 30, 77])], masked: false, clamp: [false; 2], lod_set: 0 };
        assert_eq!(texture_with_constant_alpha(&texture, None, 128).unwrap().data.unwrap(), [30, 20, 10, 128]);
    }

    #[test]
    fn alpha_on_black() {
        let texture = Texture { format: TextureFormat::Rgba8, palette: None, mips: vec![mip(1, 1, vec![10, 20, 30, 77])], masked: false, clamp: [false; 2], lod_set: 0 };
        assert_eq!(alpha_on_black_image(&texture, None).unwrap().data.unwrap(), [0, 0, 0, 77]);
    }

    #[test]
    fn glow_splits_by_mask() {
        assert_eq!(glow_texel([200, 100, 50, 255], [10, 20, 30, 0], 0), ([200, 100, 50, 0], [0, 0, 0, 0]));
        assert_eq!(glow_texel([200, 100, 50, 255], [10, 20, 30, 0], 255), ([0, 0, 0, 255], [10, 20, 30, 255]));
        assert_eq!(glow_texel([200, 0, 0, 255], [100, 0, 0, 0], 128), ([100, 0, 0, 128], [50, 0, 0, 128]));
        let (base, glow) = texture_glow((1, 1, &[[200, 0, 0, 255]]), (2, 1, &[[0, 0, 0, 0], [255, 0, 0, 0]]), (1, 1, &[[0, 0, 0, 255]]));
        assert_eq!(base.data.unwrap()[..8], [0, 0, 0, 255, 0, 0, 0, 255]);
        assert_eq!(glow.data.unwrap()[..8], [0, 0, 0, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn quality_picks_skipped_mips_and_filters() {
        let quality = TextureQuality { detail: [1, 2, 3, 0, 0], ..TextureQuality::default() };
        assert_eq!([(1, 8), (2, 8), (3, 8), (3, 2)].map(|(set, mips)| quality.skipped_mips(set, mips)), [1, 2, 3, 1]);
        assert_eq!([(0, 8), (9, 8), (4, 8)].map(|(set, mips)| quality.skipped_mips(set, mips)), [0, 0, 0]);
        assert_eq!(TextureQuality { min_lod: 7, ..quality }.skipped_mips(3, 8), 1);
        assert_eq!(quality.skipped_mips(p2_assets::LOD_SET_LIGHT_MAP, 2), 1);
        assert_eq!(TextureQuality { max_lod: 5, ..quality }.skipped_mips(4, 8), 3);
        assert_eq!([" low", "UltraLow", "high", "2", "x"].map(TextureQuality::detail_level), [2, 3, 0, 2, 0]);
        let wrap = [ImageAddressMode::Repeat; 2];
        let ImageSampler::Descriptor(d) = (TextureQuality { anisotropy: 8, trilinear: false, ..quality }).sampler(1, 8, wrap) else { panic!() };
        assert_eq!((d.anisotropy_clamp, d.mipmap_filter, d.lod_min_clamp), (8, ImageFilterMode::Linear, 1.0));
        let ImageSampler::Descriptor(d) = (TextureQuality { trilinear: false, ..quality }).sampler(0, 8, wrap) else { panic!() };
        assert_eq!((d.anisotropy_clamp, d.mipmap_filter), (1, ImageFilterMode::Nearest));
    }

    #[test]
    fn p8_uses_palette() {
        let mut palette = vec![[0; 4]; 256];
        palette[1] = [10, 20, 30, 255];
        let texture = Texture {
            format: TextureFormat::P8,
            palette: Some(1),
            mips: vec![mip(2, 1, vec![1, 0]), mip(1, 1, vec![1])],
            masked: false,
            clamp: [false; 2],
            lod_set: 0,
        };
        let image = texture_to_image(&texture, Some(&palette)).unwrap();
        assert_eq!(image.texture_descriptor.mip_level_count, 2);
        assert_eq!(image.data.unwrap(), [10, 20, 30, 255, 0, 0, 0, 0, 10, 20, 30, 255]);
        assert!(texture_to_image(&texture, None).is_none());
    }

    #[test]
    fn dxt_mips_stop_at_empty() {
        let texture = Texture {
            format: TextureFormat::Dxt1,
            palette: None,
            mips: vec![mip(8, 8, vec![0; 32]), mip(4, 4, vec![0; 8]), mip(2, 2, vec![])],
            masked: false,
            clamp: [false; 2],
            lod_set: 0,
        };
        let image = texture_to_image(&texture, None).unwrap();
        assert_eq!(image.texture_descriptor.mip_level_count, 2);
        assert_eq!(image.data.unwrap().len(), 40);
    }

    #[test]
    fn dxt_decoded_with_mips() {
        let block = vec![0x1f, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff, 0xff];
        let texture = Texture {
            format: TextureFormat::Dxt1,
            palette: None,
            mips: vec![mip(8, 8, block.repeat(4)), mip(4, 4, block.clone()), mip(2, 2, block.clone()), mip(1, 1, block)],
            masked: false,
            clamp: [false; 2],
            lod_set: 0,
        };
        let image = texture_to_rgba_image(&texture, None).unwrap();
        assert_eq!(image.texture_descriptor.format, GpuFormat::Rgba8UnormSrgb);
        assert_eq!(image.texture_descriptor.mip_level_count, 4);
        let data = image.data.unwrap();
        assert_eq!(data.len(), (64 + 16 + 4 + 1) * 4);
        assert!(data.iter().all(|&b| b == 0));
    }

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

    #[test]
    fn unreal_placement() {
        let identity = unreal_transform([1.0, 2.0, 3.0].into(), [0; 3].into(), [1.0; 3].into(), [0.0; 3].into());
        assert!(identity.translation.abs_diff_eq(Vec3::new(1.0, 3.0, 2.0), 1e-5));
        assert!(identity.rotation.abs_diff_eq(Quat::IDENTITY, 1e-5));
        let yawed = unreal_transform([0.0; 3].into(), [0, 16384, 0].into(), [2.0; 3].into(), [0.0; 3].into());
        assert!((yawed.rotation * Vec3::X).abs_diff_eq(Vec3::Z, 1e-5));
        assert!(yawed.scale.abs_diff_eq(Vec3::splat(2.0), 1e-5));
        let pitched = unreal_transform([0.0; 3].into(), [16384, 0, 0].into(), [1.0; 3].into(), [0.0; 3].into());
        assert!((pitched.rotation * Vec3::X).abs_diff_eq(Vec3::Y, 1e-5));
        let pivoted = unreal_transform([5.0, 0.0, 0.0].into(), [0, 16384, 0].into(), [3.0; 3].into(), [10.0, 0.0, 0.0].into());
        assert!(pivoted.transform_point(Vec3::new(10.0, 0.0, 0.0)).abs_diff_eq(Vec3::new(5.0, 0.0, 0.0), 1e-4));
    }

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
    fn combiners_bake_at_the_larger_size() {
        let ops = p2_assets::CombineOps { color: p2_assets::ColorOperation::Add, alpha: p2_assets::AlphaOperation::Material1, invert_mask: false, modulate: 1 };
        let small = [[10, 0, 0, 255]];
        let large = [[0, 20, 0, 0]; 4];
        let image = texture_combined(&ops, (1, 1, &small), (2, 2, &large), None);
        assert_eq!((image.width(), image.height()), (2, 2));
        assert_eq!(image.data.as_deref().map(|d| &d[..4]), Some(&[10, 20, 0, 255][..]));
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

    #[test]
    fn bone_scale_reaches_children() {
        let local = [([0.0, 0.0, 0.0, 1.0], [0.0; 3]), ([0.0, 0.0, 0.0, 1.0], [10.0, 0.0, 0.0])];
        let scaled = scaled_bone_matrices(&[0, 0], &local, |bone| if bone == 0 { 2.0 } else { 1.0 });
        assert!(scaled[1].transform_point3(Vec3::ZERO).abs_diff_eq(Vec3::new(20.0, 0.0, 0.0), 1e-5));
        assert_eq!(bone_matrices(&[0, 0], &local)[1].transform_point3(Vec3::ZERO), Vec3::new(10.0, 0.0, 0.0));
    }

    #[test]
    fn dxt_needs_whole_blocks() {
        let texture = Texture { format: TextureFormat::Dxt1, palette: None, mips: vec![mip(2, 2, vec![0; 8])], masked: false, clamp: [false; 2], lod_set: 0 };
        assert!(texture_to_image(&texture, None).is_none());
    }

    #[test]
    fn ui_image_follows_clamp_modes() {
        let texture = Texture {
            format: TextureFormat::Rgba8,
            palette: None,
            mips: vec![mip(1, 1, vec![0; 4])],
            masked: false,
            clamp: [true, false],
            lod_set: 0,
        };
        let ImageSampler::Descriptor(sampler) = texture_to_ui_image(&texture, None, false).unwrap().sampler else {
            panic!("no sampler descriptor")
        };
        assert_eq!((sampler.address_mode_u, sampler.address_mode_v), (ImageAddressMode::ClampToEdge, ImageAddressMode::Repeat));
    }

    #[test]
    fn gpu_joints_match_cpu_skinning() {
        use p2_assets::{Bone, Influence};
        let bone = |parent, orientation: [f32; 4], position| Bone { name: 0, parent, num_children: 0, orientation, position };
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let mesh = SkeletalMesh {
            materials: Vec::new(),
            scale: Vector::new(1.0, 1.0, 1.0),
            origin: Vector::ZERO,
            rot_origin: Rotator::ZERO,
            positions: vec![[10.0, 20.0, 30.0]],
            influences: vec![Influence { bone: 0, weight: 0.25 }, Influence { bone: 1, weight: 0.75 }],
            influence_starts: vec![0, 2],
            wedges: Vec::new(),
            indices: Vec::new(),
            sections: Vec::new(),
            bones: vec![bone(0, [0.0, 0.0, 0.0, 1.0], [1.0, 2.0, 3.0]), bone(0, [0.0, 0.0, half, half], [5.0, 0.0, 0.0])],
            default_animation: 0,
            attach_aliases: Vec::new(),
        };
        let parents = [0, 0];
        let posed = [([0.0, half, 0.0, half], [4.0, 0.0, -2.0]), ([half, 0.0, 0.0, half], [0.0, 7.0, 0.0])];
        let pose = bone_matrices(&parents, &posed);
        let bind = bone_matrices(&parents, &mesh.bones.iter().map(|b| (b.orientation, b.position)).collect::<Vec<_>>());
        let cpu = Vec3::from(to_bevy(skin_points(&mesh, &bind.iter().map(Mat4::inverse).collect::<Vec<_>>(), &pose)[0]));
        let inverse = skin_inverse_bindposes(&mesh);
        let vertex = Vec3::from(to_bevy(mesh.positions[0]));
        let gpu: Vec3 = mesh
            .influences_of(0)
            .iter()
            .map(|i| (unreal_to_bevy(pose[usize::from(i.bone)]).to_matrix() * inverse[usize::from(i.bone)]).transform_point3(vertex) * i.weight)
            .sum();
        assert!(cpu.distance(gpu) < 1e-3, "cpu {cpu} gpu {gpu}");
        for (at, inverse) in skin_bind_transforms(&mesh).iter().zip(&inverse) {
            let moved = (at.to_matrix() * *inverse).transform_point3(vertex);
            assert!(moved.distance(vertex) < 1e-3, "{moved} vs {vertex}");
        }
    }
}