//! The static world drawn from one table instead of one entity per section, culled in a compute pass. Sections keep
//! their Bevy mesh and material assets, so pipelines, shaders and bindless slots stay Bevy's.

use std::any::TypeId;
use std::num::NonZero;
use std::sync::Arc;

use bevy::app::{App, Plugin};
use bevy::asset::uuid::Uuid;
use bevy::asset::{AssetEvent, AssetId, Handle, UntypedAssetId, uuid_handle};
use bevy::core_pipeline::core_3d::{AlphaMask3d, Opaque3d, Opaque3dBatchSetKey, Opaque3dBinKey};
use bevy::core_pipeline::prepass::{AlphaMask3dPrepass, Opaque3dPrepass, OpaqueNoLightmap3dBatchSetKey, OpaqueNoLightmap3dBinKey};
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::ecs::prelude::{Commands, Component, Entity, Local, MessageReader, Mut, Query, Res, ResMut, Resource, With, World};
use bevy::ecs::query::QueryState;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::SystemParam;
use bevy::ecs::system::lifetimeless::{Read, SRes};
use bevy::image::Image;
use bevy::log::{error, info, warn};
use bevy::material::AlphaMode;
use bevy::material::key::{ErasedMaterialKey, ErasedMaterialPipelineKey, ErasedMeshPipelineKey};
use bevy::material::MaterialProperties;
use bevy::math::{Affine3, Affine3A, Mat4, UVec2, Vec3, Vec4};
use bevy::mesh::{Indices, Mesh, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{
    LIGHTMAPS_PER_SLAB,
    MeshFlags, MeshPipeline, MeshPipelineKey,
    MeshTransforms, MeshUniform, PreparedMaterial, PrepassFragmentShader, PrepassPipeline, PrepassVertexShader, SetMeshViewBindGroup, SetMeshViewBindingArrayBindGroup, SetPrepassEmptyMaterialBindGroup, SetPrepassViewBindGroup,
    SetPrepassViewEmptyBindGroup, ViewKeyCache, ViewKeyPrepassCache, alpha_mode_pipeline_key,
};
use bevy::platform::collections::HashMap;
use bevy::render::erased_render_asset::ErasedRenderAssets;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::material_bind_groups::{MaterialBindGroupAllocators, MaterialBindGroupIndex, MaterialBindingId};
use bevy::render::mesh::{MeshMetadataFallbackBuffer, RenderMesh};
use bevy::render::mesh::allocator::{MeshAllocator, MeshSlabId, MeshSlabs};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_phase::{
    AddRenderCommand, DrawFunctions, NonMeshEntities, PhaseItem, RenderCommand, RenderCommandResult, SetItemPipeline, TrackedRenderPass, ViewBinnedRenderPhases,
};
use bevy::render::render_resource::binding_types::{storage_buffer_read_only_sized, storage_buffer_sized, uniform_buffer_sized};
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BindingResource, Buffer, BufferDescriptor, BufferUsages, CachedComputePipelineId,
    BufferId, TextureViewId, ComputePassDescriptor, ComputePipelineDescriptor, IndexFormat, PipelineCache, ShaderStages, StorageBuffer,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::settings::WgpuFeatures;
use bevy::render::sync_world::MainEntity;
use bevy::render::texture::{FallbackImage, GpuImage};
use bevy::render::view::{ExtractedView, Msaa, RetainedViewEntity};
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderSystems};
use bevy::shader::Shader;

use crate::mesh_view::SectionMaterial;
use crate::render::shaders::add_shaders;

/// In Unreal units: small enough to cull well, large enough to keep draws few.
const CELL: f32 = 2048.0;
const MAX_VIEWS: usize = 1;
const VIEW_STRIDE: u64 = 256;
const VIEW_SIZE: u64 = 144;
const ARGS_SIZE: u64 = 20;
const COUNT_SIZE: u64 = 4;
const WORKGROUP: u32 = 64;
const ZONED: u32 = 2;
const WAIT_LOG_FRAMES: u32 = 300;
const CHUNK_SIZE: u64 = 48;
const CHUNK_FIRST_INDEX_AT: u64 = 32;
const CHUNK_BASE_VERTEX_AT: u64 = 40;

const CULL_SHADER: Handle<Shader> = uuid_handle!("6d2f8e14-3b7a-4c59-9e01-c47a15d8b2f3");

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chunk {
    pub first_index: u32,
    pub index_count: u32,
    pub center: Vec3,
    pub half: Vec3,
}

/// Sorts a mesh's triangles into cells, one index range each. `None`: not a triangle list.
pub fn chunks_of(mesh: &mut Mesh) -> Option<Arc<[Chunk]>> {
    if mesh.primitive_topology() != PrimitiveTopology::TriangleList {
        return None;
    }
    let positions = mesh.attribute(Mesh::ATTRIBUTE_POSITION)?.as_float3()?;
    let indices: Vec<u32> = match mesh.indices() {
        Some(indices) => indices.iter().map(|index| index as u32).collect(),
        None => (0..positions.len() as u32).collect(),
    };
    let wide = !matches!(mesh.indices(), Some(Indices::U16(_)));
    let at = |index: u32| Vec3::from(positions[index as usize]);
    let mut triangles: Vec<([i32; 3], [u32; 3])> = indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|&[a, b, c]| {
            let cell = ((at(a) + at(b) + at(c)) / (3.0 * CELL)).floor().as_ivec3().to_array();
            (cell, [a, b, c])
        })
        .collect();
    triangles.sort_by_key(|(cell, _)| *cell);
    let mut chunks = Vec::new();
    let mut sorted = Vec::with_capacity(indices.len());
    for group in triangles.chunk_by(|a, b| a.0 == b.0) {
        let points = || group.iter().flat_map(|(_, triangle)| triangle).map(|&index| at(index));
        let (min, max) = points().fold((Vec3::MAX, Vec3::MIN), |(min, max), point| (min.min(point), max.max(point)));
        chunks.push(Chunk { first_index: sorted.len() as u32, index_count: group.len() as u32 * 3, center: (min + max) / 2.0, half: (max - min) / 2.0 });
        sorted.extend(group.iter().flat_map(|(_, triangle)| triangle));
    }
    mesh.insert_indices(if wide { Indices::U32(sorted) } else { Indices::U16(sorted.into_iter().map(|index| index as u16).collect()) });
    Some(chunks.into())
}

#[derive(Clone)]
pub struct WorldSection {
    pub mesh: AssetId<Mesh>,
    pub material: UntypedAssetId,
    pub world_from_local: Affine3A,
    pub lightmap: Option<AssetId<Image>>,
    pub chunks: Arc<[Chunk]>,
    /// Drawn only while the eye's portals reach this zone. `None`: always.
    pub zone: Option<u8>,
}

/// One bit per zone the eye's portals reach.
#[derive(Resource, Clone, Copy, PartialEq, Eq, ExtractResource)]
#[extract_app(RenderApp)]
pub struct WorldSeenZones(pub [u32; 8]);

impl Default for WorldSeenZones {
    fn default() -> Self {
        Self([u32::MAX; 8])
    }
}

pub struct WorldPlacement {
    pub world_from_local: Affine3A,
    pub chunks: Arc<[Chunk]>,
    pub zone: Option<u8>,
}

#[derive(Resource, Clone, ExtractResource)]
#[extract_app(RenderApp)]
pub struct WorldDraws {
    pub sections: Arc<[WorldSection]>,
}

impl Default for WorldDraws {
    fn default() -> Self {
        Self { sections: Arc::new([]) }
    }
}

/// Keeps the assets alive, which no entity holds.
#[derive(Resource, Default)]
struct WorldAssets {
    meshes: Vec<Handle<Mesh>>,
    images: Vec<Handle<Image>>,
    materials: Vec<SectionMaterial>,
}

#[derive(SystemParam)]
pub struct WorldTargets<'w, 's> {
    draws: ResMut<'w, WorldDraws>,
    assets: ResMut<'w, WorldAssets>,
    device: Option<Res<'w, RenderDevice>>,
    pending: Local<'s, Vec<WorldSection>>,
}

impl WorldTargets<'_, '_> {
    pub fn clear(&mut self) {
        self.draws.sections = Arc::new([]);
        self.assets.meshes.clear();
        self.assets.images.clear();
        self.assets.materials.clear();
        self.pending.clear();
    }

    pub fn supported(&self) -> bool {
        self.device.as_ref().is_some_and(|device| device.features().contains(WgpuFeatures::MULTI_DRAW_INDIRECT_COUNT | WgpuFeatures::INDIRECT_FIRST_INSTANCE))
    }

    pub fn add(&mut self, mesh: &Handle<Mesh>, material: &SectionMaterial, lightmap: Option<&Handle<Image>>, placement: WorldPlacement) {
        let WorldPlacement { world_from_local, chunks, zone } = placement;
        if chunks.is_empty() {
            return;
        }
        self.assets.meshes.push(mesh.clone());
        self.assets.materials.push(material.clone());
        self.assets.images.extend(lightmap.cloned());
        self.pending.push(WorldSection { mesh: mesh.id(), material: material.id(), world_from_local, lightmap: lightmap.map(Handle::id), chunks, zone });
    }

    pub fn finish(&mut self) {
        if !self.pending.is_empty() {
            self.draws.sections = std::mem::take(&mut *self.pending).into();
        }
    }
}

#[derive(Component, Clone, Default, ExtractComponent)]
#[extract_app(RenderApp)]
pub struct WorldCamera;

#[derive(Component)]
struct WorldBucketIndex(usize);

#[derive(Component, Clone, Copy, PartialEq, Eq)]
struct WorldCullSlot(u32);

#[derive(PartialEq, Eq, Hash)]
struct BucketKey {
    type_id: TypeId,
    material_key: ErasedMaterialKey,
    material_bits: ErasedMeshPipelineKey,
    alpha: MeshPipelineKey,
    mask: bool,
    mesh_bits: u64,
    layout: MeshVertexBufferLayoutRef,
    group: MaterialBindGroupIndex,
    slabs: MeshSlabs,
    lightmap_slab: Option<usize>,
}

struct Bucket {
    entity: Entity,
    type_id: TypeId,
    mesh: AssetId<Mesh>,
    layout: MeshVertexBufferLayoutRef,
    mesh_bits: MeshPipelineKey,
    properties: Arc<MaterialProperties>,
    group: MaterialBindGroupIndex,
    slabs: MeshSlabs,
    index_format: IndexFormat,
    mask: bool,
    lightmapped: bool,
    first_draw: u32,
    draws: u32,
    mesh_group: usize,
}

struct Built {
    sections: Arc<[WorldSection]>,
    buckets: Vec<Bucket>,
    chunk_count: u32,
    instances: StorageBuffer<Vec<MeshUniform>>,
    chunks: Buffer,
    args: Buffer,
    counts: Buffer,
    views: Buffer,
    cull_group: BindGroup,
    mesh_groups: Vec<BindGroup>,
    materials: Vec<(UntypedAssetId, MaterialBindingId, Arc<MaterialProperties>)>,
    mesh_slices: HashMap<AssetId<Mesh>, (u32, u32, MeshSlabs)>,
    mesh_chunks: HashMap<AssetId<Mesh>, Vec<(u32, u32)>>,
    mesh_sections: HashMap<AssetId<Mesh>, Vec<u32>>,
    metadata_buffers: Vec<(MeshSlabId, BufferId)>,
    lightmap_views: Vec<(AssetId<Image>, TextureViewId)>,
    live_views: u32,
}

#[derive(Resource, Default)]
struct WorldPass {
    built: Option<Built>,
    pipeline: Option<(BindGroupLayoutDescriptor, CachedComputePipelineId)>,
    queued: HashMap<RetainedViewEntity, (MeshPipelineKey, Option<MeshPipelineKey>)>,
    waited_frames: u32,
}

#[derive(Resource, Default)]
struct ModifiedMeshes(Vec<AssetId<Mesh>>);

fn cull_pipeline(cache: &PipelineCache) -> (BindGroupLayoutDescriptor, CachedComputePipelineId) {
    let layout = BindGroupLayoutDescriptor::new(
        "world cull",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                storage_buffer_read_only_sized(false, None),
                storage_buffer_read_only_sized(false, None),
                storage_buffer_sized(false, None),
                storage_buffer_sized(false, None),
                uniform_buffer_sized(true, NonZero::new(VIEW_SIZE)),
            ),
        ),
    );
    let pipeline = cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some("world cull".into()),
        layout: vec![layout.clone()],
        shader: CULL_SHADER,
        ..Default::default()
    });
    (layout, pipeline)
}

fn world_box(world_from_local: &Affine3A, chunk: &Chunk) -> (Vec3, Vec3) {
    let m = world_from_local.matrix3;
    let half = Vec3::from(m.x_axis).abs() * chunk.half.x + Vec3::from(m.y_axis).abs() * chunk.half.y + Vec3::from(m.z_axis).abs() * chunk.half.z;
    (world_from_local.transform_point3(chunk.center), half)
}

fn push_u32s(bytes: &mut Vec<u8>, values: &[u32]) {
    bytes.extend(values.iter().flat_map(|value| value.to_le_bytes()));
}

fn push_vec3(bytes: &mut Vec<u8>, value: Vec3) {
    bytes.extend(value.to_array().iter().flat_map(|value| value.to_le_bytes()));
}

fn lightmap_slots(draws: &WorldDraws) -> HashMap<AssetId<Image>, usize> {
    let mut slots = HashMap::default();
    for image in draws.sections.iter().filter_map(|section| section.lightmap) {
        let next = slots.len();
        slots.entry(image).or_insert(next);
    }
    slots
}

/// As Bevy decides it for its light map layout, which it keeps private: 16 material textures and 8 probes.
fn lightmap_arrays_usable(device: &RenderDevice) -> bool {
    let limits = device.limits();
    limits.max_binding_array_elements_per_shader_stage >= 24
        && limits.max_binding_array_sampler_elements_per_shader_stage >= 24
        && device.features().contains(WgpuFeatures::TEXTURE_BINDING_ARRAY | WgpuFeatures::SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING)
}

fn prepass_reads_material(properties: &MaterialProperties) -> bool {
    properties.get_shader(PrepassVertexShader).is_some() || properties.get_shader(PrepassFragmentShader).is_some()
}

fn section_uniform(section: &WorldSection, material: &PreparedMaterial, first_vertex: u32, metadata_index: Option<u32>, light_slot: Option<usize>) -> MeshUniform {
    let mut flags = MeshFlags::SHADOW_RECEIVER | MeshFlags::LOD_INDEX_MASK;
    if section.world_from_local.matrix3.determinant().is_sign_positive() {
        flags |= MeshFlags::SIGN_DETERMINANT_MODEL_3X3;
    }
    let affine = Affine3::from(section.world_from_local);
    let transforms = MeshTransforms { world_from_local: affine, previous_world_from_local: affine, flags: flags.bits() };
    let mut uniform = MeshUniform::new(&transforms, first_vertex, material.binding.slot, None, None, None, None, metadata_index);
    if let Some(slot) = light_slot {
        uniform.material_and_lightmap_bind_group_slot = u32::from(material.binding.slot) | ((slot as u32) << 16);
        uniform.lightmap_uv_rect = UVec2::new(0, u32::MAX);
    }
    uniform
}

fn chunk_table(
    draws: &WorldDraws,
    buckets: &mut [Bucket],
    chunk_lists: &[Vec<(usize, usize)>],
    mesh_chunks: &mut HashMap<AssetId<Mesh>, Vec<(u32, u32)>>,
    allocator: &MeshAllocator,
) -> Option<(Vec<u8>, Vec<u32>, u32)> {
    let mut chunk_bytes = Vec::new();
    let mut firsts = Vec::new();
    let mut next_draw = 0u32;
    for (bucket_index, (bucket, list)) in buckets.iter_mut().zip(chunk_lists).enumerate() {
        bucket.first_draw = next_draw;
        bucket.draws = list.len() as u32;
        firsts.push(next_draw);
        next_draw += bucket.draws;
        for (position, &(section_index, chunk_index)) in list.iter().enumerate() {
            let section = &draws.sections[section_index];
            let chunk = &section.chunks[chunk_index];
            mesh_chunks.entry(section.mesh).or_default().push((bucket.first_draw + position as u32, chunk.first_index));
            let (center, half) = world_box(&section.world_from_local, chunk);
            let first_index = allocator.mesh_index_slice(&section.mesh)?.range.start + chunk.first_index;
            let base_vertex = allocator.mesh_vertex_slice(&section.mesh)?.range.start;
            push_vec3(&mut chunk_bytes, center);
            push_u32s(&mut chunk_bytes, &[bucket_index as u32]);
            push_vec3(&mut chunk_bytes, half);
            let zone = section.zone.map_or(0, |zone| ZONED | (u32::from(zone) << 8));
            push_u32s(&mut chunk_bytes, &[section_index as u32, first_index, chunk.index_count, base_vertex, zone]);
        }
    }
    Some((chunk_bytes, firsts, next_draw))
}

fn cull_buffers(world: &World, layout: &BindGroupLayoutDescriptor, chunk_bytes: &[u8], firsts: &[u32], draws: u64, buckets: u64) -> (Buffer, Buffer, Buffer, Buffer, BindGroup) {
    let device = world.resource::<RenderDevice>();
    let queue = world.resource::<RenderQueue>();
    let empty = |label: &'static str, size: u64, usage: BufferUsages| device.create_buffer(&BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false });
    let filled = |label: &'static str, contents: &[u8]| {
        let buffer = empty(label, contents.len() as u64, BufferUsages::STORAGE | BufferUsages::COPY_DST);
        queue.write_buffer(&buffer, 0, contents);
        buffer
    };
    let chunks = filled("world chunks", chunk_bytes);
    let mut first_bytes = Vec::with_capacity(firsts.len() * 4);
    push_u32s(&mut first_bytes, firsts);
    let firsts = filled("world bucket firsts", &first_bytes);
    let args = empty("world draw args", MAX_VIEWS as u64 * draws * ARGS_SIZE, BufferUsages::STORAGE | BufferUsages::INDIRECT);
    let counts = empty("world draw counts", MAX_VIEWS as u64 * buckets * COUNT_SIZE, BufferUsages::STORAGE | BufferUsages::INDIRECT | BufferUsages::COPY_DST);
    let views = empty("world cull views", MAX_VIEWS as u64 * VIEW_STRIDE, BufferUsages::UNIFORM | BufferUsages::COPY_DST);
    let view_binding = BindingResource::Buffer(bevy::render::render_resource::BufferBinding { buffer: &views, offset: 0, size: NonZero::new(VIEW_SIZE) });
    let group = device.create_bind_group(
        "world cull",
        &world.resource::<PipelineCache>().get_bind_group_layout(layout),
        &BindGroupEntries::sequential((chunks.as_entire_binding(), firsts.as_entire_binding(), args.as_entire_binding(), counts.as_entire_binding(), view_binding)),
    );
    (chunks, args, counts, views, group)
}

/// Bevy's light map layout needs its arrays full.
fn mesh_bind_groups(
    world: &World,
    model: &BindingResource,
    keys: &[(Option<MeshSlabId>, Option<usize>)],
    images_by_slot: &[Option<&GpuImage>],
    slab_size: usize,
) -> Vec<BindGroup> {
    let device = world.resource::<RenderDevice>();
    let cache = world.resource::<PipelineCache>();
    let allocator = world.resource::<MeshAllocator>();
    let fallback = world.resource::<FallbackImage>();
    let metadata_fallback = world.resource::<MeshMetadataFallbackBuffer>();
    let layouts = &world.resource::<MeshPipeline>().mesh_layouts;
    let mut groups = Vec::with_capacity(keys.len());
    for &(metadata_slab, lightmap_slab) in keys {
        let metadata = metadata_slab.and_then(|slab| allocator.buffer_for_slab(slab)).unwrap_or(&metadata_fallback.buffer);
        let Some(slab) = lightmap_slab else {
            groups.push(layouts.model_only(device, cache, model, metadata));
            continue;
        };
        let image_at = |slot: usize| images_by_slot.get(slab * slab_size + slot).copied().flatten().unwrap_or(&fallback.d2);
        let layout = cache.get_bind_group_layout(&layouts.lightmapped);
        let model_entry = BindGroupEntry { binding: 0, resource: model.clone() };
        let metadata_entry = BindGroupEntry { binding: 9, resource: metadata.as_entire_binding() };
        let views: [_; LIGHTMAPS_PER_SLAB] = std::array::from_fn(|slot| &*image_at(slot).texture_view);
        let samplers: [_; LIGHTMAPS_PER_SLAB] = std::array::from_fn(|slot| &*image_at(slot).sampler);
        let (view_resource, sampler_resource) = if slab_size > 1 {
            (BindingResource::TextureViewArray(&views), BindingResource::SamplerArray(&samplers))
        } else {
            (BindingResource::TextureView(&image_at(0).texture_view), BindingResource::Sampler(&image_at(0).sampler))
        };
        groups.push(device.create_bind_group(
            "world lightmapped",
            &layout,
            &[model_entry, metadata_entry, BindGroupEntry { binding: 4, resource: view_resource }, BindGroupEntry { binding: 5, resource: sampler_resource }],
        ));
    }
    groups
}

/// For the log: a missing asset here means nothing of the world draws.
fn waiting_for(world: &World, draws: &WorldDraws) -> String {
    let meshes = world.resource::<RenderAssets<RenderMesh>>();
    let allocator = world.resource::<MeshAllocator>();
    let materials = world.resource::<ErasedRenderAssets<PreparedMaterial>>();
    let images = world.resource::<RenderAssets<GpuImage>>();
    let count = |test: &dyn Fn(&WorldSection) -> bool| draws.sections.iter().filter(|section| test(section)).count();
    format!(
        "{} of {} sections: {} meshes, {} allocations, {} index lists, {} materials, {} light maps",
        count(&|section| meshes.get(section.mesh).is_none() || allocator.mesh_index_slice(&section.mesh).is_none() || materials.get(section.material).is_none()),
        draws.sections.len(),
        count(&|section| meshes.get(section.mesh).is_none()),
        count(&|section| allocator.mesh_vertex_slice(&section.mesh).is_none()),
        count(&|section| allocator.mesh_index_slice(&section.mesh).is_none()),
        count(&|section| materials.get(section.material).is_none()),
        count(&|section| section.lightmap.is_some_and(|image| images.get(image).is_none())),
    )
}

/// `None` until every mesh and material has reached the render world.
fn build(world: &mut World, draws: &WorldDraws) -> Option<Built> {
    let meshes = world.resource::<RenderAssets<RenderMesh>>();
    let allocator = world.resource::<MeshAllocator>();
    let materials = world.resource::<ErasedRenderAssets<PreparedMaterial>>();
    let images = world.resource::<RenderAssets<GpuImage>>();
    let device = world.resource::<RenderDevice>();
    let queue = world.resource::<RenderQueue>();
    let (cull_layout, _) = world.resource::<WorldPass>().pipeline.clone()?;

    let light_slots = lightmap_slots(draws);
    let bindless_lightmaps = lightmap_arrays_usable(device);
    let slab_size = if bindless_lightmaps { LIGHTMAPS_PER_SLAB } else { 1 };

    let mut bucket_of = HashMap::<BucketKey, usize>::default();
    let mut buckets = Vec::<Bucket>::new();
    let mut uniforms = Vec::with_capacity(draws.sections.len());
    let mut chunk_lists: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut material_list = HashMap::<UntypedAssetId, (MaterialBindingId, Arc<MaterialProperties>)>::default();
    let mut mesh_slices = HashMap::<AssetId<Mesh>, (u32, u32, MeshSlabs)>::default();
    let mut mesh_sections = HashMap::<AssetId<Mesh>, Vec<u32>>::default();
    let mut mesh_group_of = HashMap::<(Option<MeshSlabId>, Option<usize>), usize>::default();
    let mut mesh_group_keys = Vec::new();

    for (index, section) in draws.sections.iter().enumerate() {
        let mesh = meshes.get(section.mesh)?;
        let material = materials.get(section.material)?;
        let slabs = allocator.mesh_slabs(&section.mesh)?;
        let vertices = allocator.mesh_vertex_slice(&section.mesh)?;
        let index_start = allocator.mesh_index_slice(&section.mesh)?.range.start;
        let vertex_start = vertices.range.start;
        let bevy::render::mesh::RenderMeshBufferInfo::Indexed { index_format, .. } = mesh.buffer_info else { return None };
        material_list.insert(section.material, (material.binding, material.properties.clone()));
        mesh_slices.insert(section.mesh, (vertex_start, index_start, slabs));
        mesh_sections.entry(section.mesh).or_default().push(index as u32);

        let light_slot = section.lightmap.and_then(|image| light_slots.get(&image).copied());
        let lightmap_slab = light_slot.map(|slot| slot / slab_size);
        let alpha_mode = material.properties.alpha_mode;
        let alpha = alpha_mode_pipeline_key(alpha_mode, &Msaa::Off);
        let mask = matches!(alpha_mode, AlphaMode::Mask(_));
        let key = BucketKey {
            type_id: section.material.type_id(),
            material_key: material.properties.material_key.clone(),
            material_bits: material.properties.mesh_pipeline_key_bits,
            alpha,
            mask,
            mesh_bits: mesh.key_bits.bits(),
            layout: mesh.layout.clone(),
            group: material.binding.group,
            slabs,
            lightmap_slab,
        };
        let bucket = *bucket_of.entry(key).or_insert_with(|| {
            let group_key = (slabs.metadata_slab_id, lightmap_slab);
            let next = mesh_group_of.len();
            let mesh_group = *mesh_group_of.entry(group_key).or_insert_with(|| {
                mesh_group_keys.push(group_key);
                next
            });
            buckets.push(Bucket {
                entity: Entity::PLACEHOLDER,
                type_id: section.material.type_id(),
                mesh: section.mesh,
                layout: mesh.layout.clone(),
                mesh_bits: MeshPipelineKey::from_bits_retain(mesh.key_bits.bits()),
                properties: material.properties.clone(),
                group: material.binding.group,
                slabs,
                index_format,
                mask,
                lightmapped: lightmap_slab.is_some(),
                first_draw: 0,
                draws: 0,
                mesh_group,
            });
            chunk_lists.push(Vec::new());
            buckets.len() - 1
        });
        chunk_lists[bucket].extend((0..section.chunks.len()).map(|chunk| (index, chunk)));

        let metadata_index = allocator.mesh_metadata_slice(&section.mesh).map(|slice| slice.range.start);
        uniforms.push(section_uniform(section, material, vertex_start, metadata_index, light_slot.map(|slot| slot % slab_size)));
    }

    let mut mesh_chunks = HashMap::default();
    let (chunk_bytes, firsts, next_draw) = chunk_table(draws, &mut buckets, &chunk_lists, &mut mesh_chunks, allocator)?;
    if next_draw == 0 {
        return None;
    }

    let mut images_by_slot = vec![None; light_slots.len()];
    for (&image, &slot) in &light_slots {
        images_by_slot[slot] = Some(images.get(image)?);
    }

    let mut instances = StorageBuffer::from(uniforms);
    instances.set_label(Some("world instances"));
    instances.write_buffer(device, queue);
    let model = instances.binding()?;

    let (chunks, args, counts, views, cull_group) = cull_buffers(world, &cull_layout, &chunk_bytes, &firsts, u64::from(next_draw), buckets.len() as u64);

    let mesh_groups = mesh_bind_groups(world, &model, &mesh_group_keys, &images_by_slot, slab_size);
    let metadata_buffers = mesh_group_keys.iter().filter_map(|(slab, _)| *slab).filter_map(|slab| Some((slab, allocator.buffer_for_slab(slab)?.id()))).collect();
    let lightmap_views = light_slots.keys().filter_map(|&image| Some((image, images.get(image)?.texture_view.id()))).collect();
    let materials = material_list.into_iter().map(|(id, (binding, properties))| (id, binding, properties)).collect();
    info!("world pass: {} sections, {next_draw} chunks, {} buckets, {} light maps", draws.sections.len(), buckets.len(), light_slots.len());
    Some(Built {
        sections: draws.sections.clone(),
        buckets,
        chunk_count: next_draw,
        instances,
        chunks,
        args,
        counts,
        views,
        cull_group,
        mesh_groups,
        materials,
        mesh_slices,
        mesh_chunks,
        mesh_sections,
        metadata_buffers,
        lightmap_views,
        live_views: 0,
    })
}

fn is_stale(world: &World, built: &Built, draws: &WorldDraws) -> bool {
    if !Arc::ptr_eq(&built.sections, &draws.sections) {
        return true;
    }
    // The modern look's sun mask swaps a light map's image under the same handle.
    let images = world.resource::<RenderAssets<GpuImage>>();
    if built.lightmap_views.iter().any(|(image, view)| images.get(*image).is_some_and(|image| image.texture_view.id() != *view)) {
        return true;
    }
    let materials = world.resource::<ErasedRenderAssets<PreparedMaterial>>();
    let same = |now: &PreparedMaterial, binding: &MaterialBindingId, built: &MaterialProperties| {
        now.binding.group == binding.group
            && now.binding.slot == binding.slot
            && now.properties.alpha_mode == built.alpha_mode
            && now.properties.material_key == built.material_key
            && now.properties.mesh_pipeline_key_bits == built.mesh_pipeline_key_bits
    };
    if built.materials.iter().any(|(id, binding, properties)| materials.get(*id).is_some_and(|now| !same(now, binding, properties))) {
        return true;
    }
    let allocator = world.resource::<MeshAllocator>();
    built.metadata_buffers.iter().any(|(slab, buffer)| allocator.buffer_for_slab(*slab).map(Buffer::id) != Some(*buffer))
}

/// A relit terrain sector is uploaded again and can land elsewhere in its slab. Only its draws' offsets change, so only those
/// are written. `false`: a mesh left its slab, and the buckets must be built again.
fn follow_moved_meshes(world: &mut World) -> bool {
    let Some(mut built) = world.resource_mut::<WorldPass>().built.take() else { return true };
    let allocator = world.resource::<MeshAllocator>();
    let queue = world.resource::<RenderQueue>();
    let ids = world.resource::<ModifiedMeshes>().0.iter().copied().chain(allocator.meshes_displaced_by_slab_growth());
    let mut same_slabs = true;
    let mut rewrite_instances = false;
    for id in ids {
        let Some(&(vertex, index, slabs)) = built.mesh_slices.get(&id) else { continue };
        let now = allocator.mesh_vertex_slice(&id).map(|slice| slice.range.start).zip(allocator.mesh_index_slice(&id).map(|slice| slice.range.start));
        let Some((new_vertex, new_index)) = now.filter(|_| allocator.mesh_slabs(&id) == Some(slabs)) else {
            same_slabs = false;
            break;
        };
        if (new_vertex, new_index) == (vertex, index) {
            continue;
        }
        for &(slot, first_index) in built.mesh_chunks.get(&id).into_iter().flatten() {
            let at = u64::from(slot) * CHUNK_SIZE;
            queue.write_buffer(&built.chunks, at + CHUNK_FIRST_INDEX_AT, &(new_index + first_index).to_le_bytes());
            queue.write_buffer(&built.chunks, at + CHUNK_BASE_VERTEX_AT, &new_vertex.to_le_bytes());
        }
        let metadata_index = allocator.mesh_metadata_slice(&id).map_or(0, |slice| slice.range.start);
        for &section in built.mesh_sections.get(&id).into_iter().flatten() {
            let uniform = &mut built.instances.get_mut()[section as usize];
            uniform.first_vertex_index = new_vertex;
            uniform.metadata_index = metadata_index;
        }
        built.mesh_slices.insert(id, (new_vertex, new_index, slabs));
        rewrite_instances = true;
    }
    if rewrite_instances {
        built.instances.write_buffer(world.resource::<RenderDevice>(), queue);
    }
    world.resource_mut::<WorldPass>().built = Some(built);
    same_slabs
}

fn clear_phases(world: &mut World) {
    fn clear<P: bevy::render::render_phase::BinnedPhaseItem>(world: &mut World) {
        for phase in world.resource_mut::<ViewBinnedRenderPhases<P>>().values_mut() {
            phase.non_mesh_items.clear();
        }
    }
    clear::<Opaque3d>(world);
    clear::<AlphaMask3d>(world);
    clear::<Opaque3dPrepass>(world);
    clear::<AlphaMask3dPrepass>(world);
}

fn rebuild_if_stale(world: &mut World) {
    let Some(draws) = world.get_resource::<WorldDraws>().cloned() else { return };
    let stale = match &world.resource::<WorldPass>().built {
        Some(built) => is_stale(world, built, &draws),
        None => !draws.sections.is_empty(),
    };
    if !stale && follow_moved_meshes(world) {
        return;
    }
    if world.resource::<WorldPass>().pipeline.is_none() {
        let pipeline = cull_pipeline(world.resource::<PipelineCache>());
        world.resource_mut::<WorldPass>().pipeline = Some(pipeline);
    }
    let old = world.resource_mut::<WorldPass>().built.take();
    if let Some(old) = old {
        for bucket in &old.buckets {
            world.despawn(bucket.entity);
        }
        clear_phases(world);
        world.resource_mut::<WorldPass>().queued.clear();
    }
    if draws.sections.is_empty() {
        return;
    }
    let Some(mut built) = build(world, &draws) else {
        let frames = &mut world.resource_mut::<WorldPass>().waited_frames;
        *frames += 1;
        if *frames == WAIT_LOG_FRAMES {
            warn!("world pass still waiting for {}", waiting_for(world, &draws));
        }
        return;
    };
    for (index, bucket) in built.buckets.iter_mut().enumerate() {
        bucket.entity = world.spawn(WorldBucketIndex(index)).id();
    }
    let mut pass = world.resource_mut::<WorldPass>();
    pass.built = Some(built);
    pass.queued.clear();
    pass.waited_frames = 0;
}

/// Unique per bucket, so no two buckets share a bin even with the same pipeline and slabs.
fn draw_key(bucket: usize) -> UntypedAssetId {
    AssetId::<Mesh>::Uuid { uuid: Uuid::from_u128(bucket as u128 + 1) }.untyped()
}

fn one_item(entity: Entity) -> NonMeshEntities {
    let mut entities = bevy::render::sync_world::MainEntityHashMap::default();
    entities.insert(MainEntity::from(Entity::PLACEHOLDER), entity);
    NonMeshEntities { entities }
}

fn erased_key(bucket: &Bucket, mesh_key: MeshPipelineKey) -> ErasedMaterialPipelineKey {
    ErasedMaterialPipelineKey { type_id: bucket.type_id, mesh_key: ErasedMeshPipelineKey::new(mesh_key), material_key: bucket.properties.material_key.clone() }
}

fn with_buckets<R>(world: &mut World, work: impl FnOnce(&mut World, &[Bucket]) -> R) -> Option<R> {
    let built = world.resource_mut::<WorldPass>().built.take()?;
    let result = work(world, &built.buckets);
    world.resource_mut::<WorldPass>().built = Some(built);
    Some(result)
}

fn queue_main(world: &mut World, buckets: &[Bucket], view: RetainedViewEntity, view_key: MeshPipelineKey) {
    let msaa = Msaa::from_samples(view_key.msaa_samples());
    let draw_opaque = world.resource::<DrawFunctions<Opaque3d>>().read().id::<DrawWorld>();
    let draw_masked = world.resource::<DrawFunctions<AlphaMask3d>>().read().id::<DrawWorld>();
    let mut opaque = Vec::new();
    let mut masked = Vec::new();
    for (index, bucket) in buckets.iter().enumerate() {
        let mut mesh_key = view_key | bucket.mesh_bits | bucket.properties.mesh_pipeline_key_bits.downcast::<MeshPipelineKey>() | alpha_mode_pipeline_key(bucket.properties.alpha_mode, &msaa);
        if bucket.lightmapped {
            mesh_key |= MeshPipelineKey::LIGHTMAPPED;
        }
        let Some(specialize) = bucket.properties.base_specialize else { continue };
        match specialize(world, erased_key(bucket, mesh_key), &bucket.layout, &bucket.properties) {
            Ok(pipeline) if bucket.mask => masked.push((index, pipeline)),
            Ok(pipeline) => opaque.push((index, pipeline)),
            Err(error) => error!("world pass: {error}"),
        }
    }
    if let Some(phase) = world.resource_mut::<ViewBinnedRenderPhases<Opaque3d>>().get_mut(&view) {
        phase.non_mesh_items.clear();
        for (index, pipeline) in opaque {
            let bucket = &buckets[index];
            let key = Opaque3dBatchSetKey { pipeline, draw_function: draw_opaque, material_bind_group_index: Some(bucket.group.0), slabs: bucket.slabs, lightmap_slab: None };
            phase.non_mesh_items.insert((key, Opaque3dBinKey { asset_id: draw_key(index) }), one_item(bucket.entity));
        }
    }
    if let Some(phase) = world.resource_mut::<ViewBinnedRenderPhases<AlphaMask3d>>().get_mut(&view) {
        phase.non_mesh_items.clear();
        for (index, pipeline) in masked {
            let bucket = &buckets[index];
            let key = OpaqueNoLightmap3dBatchSetKey { pipeline, draw_function: draw_masked, material_bind_group_index: Some(bucket.group.0), slabs: bucket.slabs };
            phase.non_mesh_items.insert((key, OpaqueNoLightmap3dBinKey { asset_id: draw_key(index) }), one_item(bucket.entity));
        }
    }
}

fn depth_only(world: &World, bucket: &Bucket, mesh_key: MeshPipelineKey) -> bool {
    let supported = world.resource::<PrepassPipeline>().depth_clip_control_supported;
    let emulated = mesh_key.contains(MeshPipelineKey::UNCLIPPED_DEPTH_ORTHO) && !supported;
    !emulated && !prepass_reads_material(&bucket.properties) && !mesh_key.intersects(MeshPipelineKey::MAY_DISCARD | MeshPipelineKey::PREPASS_READS_MATERIAL)
}

fn queue_prepass(world: &mut World, buckets: &[Bucket], view: RetainedViewEntity, view_key: MeshPipelineKey, msaa: Msaa) {
    let draw_opaque = world.resource::<DrawFunctions<Opaque3dPrepass>>().read().id::<DrawWorldPrepass>();
    let draw_opaque_depth = world.resource::<DrawFunctions<Opaque3dPrepass>>().read().id::<DrawWorldDepthOnly>();
    let draw_masked = world.resource::<DrawFunctions<AlphaMask3dPrepass>>().read().id::<DrawWorldPrepass>();
    let mut opaque = Vec::new();
    let mut masked = Vec::new();
    for (index, bucket) in buckets.iter().enumerate() {
        if !bucket.properties.prepass_enabled || !matches!(bucket.properties.alpha_mode, AlphaMode::Opaque | AlphaMode::Mask(_) | AlphaMode::AlphaToCoverage) {
            continue;
        }
        let mut mesh_key = view_key | bucket.mesh_bits | alpha_mode_pipeline_key(bucket.properties.alpha_mode, &msaa);
        if bucket.lightmapped {
            mesh_key |= MeshPipelineKey::LIGHTMAPPED;
        }
        if prepass_reads_material(&bucket.properties) {
            mesh_key |= MeshPipelineKey::PREPASS_READS_MATERIAL;
        }
        let only_depth = depth_only(world, bucket, mesh_key) && mesh_key.intersection(MeshPipelineKey::ALL_PREPASS_BITS) == MeshPipelineKey::DEPTH_PREPASS;
        let Some(specialize) = bucket.properties.prepass_specialize else { continue };
        match specialize(world, &erased_key(bucket, mesh_key), &bucket.layout, &bucket.properties) {
            Ok(pipeline) if bucket.mask => masked.push((index, pipeline)),
            Ok(pipeline) => opaque.push((index, pipeline, only_depth)),
            Err(error) => error!("world pass: {error}"),
        }
    }
    if let Some(phase) = world.resource_mut::<ViewBinnedRenderPhases<Opaque3dPrepass>>().get_mut(&view) {
        phase.non_mesh_items.clear();
        for (index, pipeline, only_depth) in opaque {
            let bucket = &buckets[index];
            let key = OpaqueNoLightmap3dBatchSetKey {
                pipeline,
                draw_function: if only_depth { draw_opaque_depth } else { draw_opaque },
                material_bind_group_index: (!only_depth).then_some(bucket.group.0),
                slabs: bucket.slabs,
            };
            phase.non_mesh_items.insert((key, OpaqueNoLightmap3dBinKey { asset_id: draw_key(index) }), one_item(bucket.entity));
        }
    }
    if let Some(phase) = world.resource_mut::<ViewBinnedRenderPhases<AlphaMask3dPrepass>>().get_mut(&view) {
        phase.non_mesh_items.clear();
        for (index, pipeline) in masked {
            let bucket = &buckets[index];
            let key = OpaqueNoLightmap3dBatchSetKey { pipeline, draw_function: draw_masked, material_bind_group_index: Some(bucket.group.0), slabs: bucket.slabs };
            phase.non_mesh_items.insert((key, OpaqueNoLightmap3dBinKey { asset_id: draw_key(index) }), one_item(bucket.entity));
        }
    }
}

fn queue_world(
    world: &mut World,
    cameras: &mut QueryState<(&ExtractedView, Option<&Msaa>), With<WorldCamera>>,
) {
    rebuild_if_stale(world);
    world.resource_mut::<ModifiedMeshes>().0.clear();
    if world.resource::<WorldPass>().built.is_none() {
        return;
    }
    // Bevy drops an inactive camera's phases, and with them the items queued here.
    world.resource_scope(|world, mut pass: Mut<WorldPass>| {
        let phases = world.resource::<ViewBinnedRenderPhases<Opaque3d>>();
        pass.queued.retain(|view, _| phases.contains_key(view));
    });
    let camera_views: Vec<(RetainedViewEntity, Msaa)> = cameras.iter(world).map(|(view, msaa)| (view.retained_view_entity, msaa.copied().unwrap_or(Msaa::Off))).collect();
    for (view, msaa) in camera_views {
        let Some(&view_key) = world.resource::<ViewKeyCache>().get(&view) else { continue };
        if !world.resource::<ViewBinnedRenderPhases<Opaque3d>>().contains_key(&view) {
            continue;
        }
        let prepass_key = world.resource::<ViewKeyPrepassCache>().get(&view).copied().filter(|_| world.resource::<ViewBinnedRenderPhases<Opaque3dPrepass>>().contains_key(&view));
        if world.resource::<WorldPass>().queued.get(&view) == Some(&(view_key, prepass_key)) {
            continue;
        }
        with_buckets(world, |world, buckets| {
            queue_main(world, buckets, view, view_key);
            if let Some(prepass_key) = prepass_key {
                queue_prepass(world, buckets, view, prepass_key, msaa);
            }
        });
        world.resource_mut::<WorldPass>().queued.insert(view, (view_key, prepass_key));
    }
}

/// `Vec4(normal, d)`: a point is inside at `dot(normal, p) + d >= 0`.
fn view_planes(clip_from_world: Mat4) -> [Vec4; 6] {
    let row = |index: usize| clip_from_world.row(index);
    let plane = |raw: Vec4| {
        let length = raw.truncate().length();
        if length > 1e-6 { raw / length } else { Vec4::W }
    };
    [plane(row(3) + row(0)), plane(row(3) - row(0)), plane(row(3) + row(1)), plane(row(3) - row(1)), plane(row(3) - row(2)), plane(row(2))]
}

fn prepare_world_views(
    mut commands: Commands,
    mut pass: ResMut<WorldPass>,
    queue: Res<RenderQueue>,
    cameras: Query<(Entity, &ExtractedView, Option<&WorldCullSlot>), With<WorldCamera>>,
    seen_zones: Option<Res<WorldSeenZones>>,
) {
    let Some(built) = pass.built.as_mut() else { return };
    let seen_zones = seen_zones.map_or(WorldSeenZones::default().0, |seen| seen.0);
    let clip_from_world = |view: &ExtractedView| view.clip_from_world.unwrap_or_else(|| view.clip_from_view * view.world_from_view.to_matrix().inverse());
    let mut bytes = Vec::with_capacity(MAX_VIEWS * VIEW_STRIDE as usize);
    let mut slot = 0u32;
    for (entity, view, had) in cameras.iter().take(MAX_VIEWS) {
        let start = bytes.len();
        bytes.extend(view_planes(clip_from_world(view)).iter().flat_map(|plane| plane.to_array()).flat_map(|value: f32| value.to_le_bytes()));
        push_u32s(&mut bytes, &[slot * built.chunk_count, slot * built.buckets.len() as u32, 0, 0]);
        push_u32s(&mut bytes, &seen_zones);
        bytes.resize(start + VIEW_STRIDE as usize, 0);
        if had != Some(&WorldCullSlot(slot)) {
            commands.entity(entity).insert(WorldCullSlot(slot));
        }
        slot += 1;
    }
    built.live_views = slot;
    if !bytes.is_empty() {
        queue.write_buffer(&built.views, 0, &bytes);
    }
}

fn cull_world(_view: ViewQuery<Entity, With<WorldCamera>>, pass: Res<WorldPass>, cache: Res<PipelineCache>, mut context: RenderContext) {
    let (Some(built), Some((_, pipeline))) = (&pass.built, &pass.pipeline) else { return };
    let Some(pipeline) = cache.get_compute_pipeline(*pipeline) else { return };
    let diagnostics = context.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let span = bevy::render::diagnostic::RecordDiagnostics::time_span(&diagnostics, context.command_encoder(), "world_cull");
    let encoder = context.command_encoder();
    encoder.clear_buffer(&built.counts, 0, None);
    let mut compute = encoder.begin_compute_pass(&ComputePassDescriptor { label: Some("world cull"), timestamp_writes: None });
    compute.set_pipeline(pipeline);
    let groups = built.chunk_count.div_ceil(WORKGROUP);
    for slot in 0..built.live_views {
        compute.set_bind_group(0, &built.cull_group, &[slot * VIEW_STRIDE as u32]);
        compute.dispatch_workgroups(groups, 1, 1);
    }
    drop(compute);
    span.end(context.command_encoder());
}

fn extract_modified_meshes(mut events: Extract<MessageReader<AssetEvent<Mesh>>>, mut modified: ResMut<ModifiedMeshes>) {
    modified.0.extend(events.read().filter_map(|event| match event {
        AssetEvent::Modified { id } => Some(*id),
        _ => None,
    }));
}

struct SetWorldMesh<const I: usize>;

impl<P: PhaseItem, const I: usize> RenderCommand<P> for SetWorldMesh<I> {
    type Param = SRes<WorldPass>;
    type ViewQuery = ();
    type ItemQuery = Read<WorldBucketIndex>;

    fn render<'w>(
        _item: &P,
        _view: (),
        bucket: Option<&'w WorldBucketIndex>,
        pass_data: bevy::ecs::system::SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let Some((built, bucket)) = pass_data.into_inner().built.as_ref().zip(bucket) else { return RenderCommandResult::Skip };
        pass.set_bind_group(I, &built.mesh_groups[built.buckets[bucket.0].mesh_group], &[]);
        RenderCommandResult::Success
    }
}

struct SetWorldMaterial<const I: usize>;

impl<P: PhaseItem, const I: usize> RenderCommand<P> for SetWorldMaterial<I> {
    type Param = (SRes<WorldPass>, SRes<MaterialBindGroupAllocators>);
    type ViewQuery = ();
    type ItemQuery = Read<WorldBucketIndex>;

    fn render<'w>(
        _item: &P,
        _view: (),
        bucket: Option<&'w WorldBucketIndex>,
        (pass_data, allocators): bevy::ecs::system::SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let Some((built, bucket)) = pass_data.into_inner().built.as_ref().zip(bucket) else { return RenderCommandResult::Skip };
        let bucket = &built.buckets[bucket.0];
        let Some(allocator) = allocators.into_inner().get(&bucket.type_id) else { return RenderCommandResult::Skip };
        let Some(bind_group) = allocator.get(bucket.group).and_then(|group| group.bind_group()) else { return RenderCommandResult::Skip };
        pass.set_bind_group(I, bind_group, &[]);
        RenderCommandResult::Success
    }
}

struct DrawWorldBucket;

impl<P: PhaseItem> RenderCommand<P> for DrawWorldBucket {
    type Param = (SRes<WorldPass>, SRes<MeshAllocator>);
    type ViewQuery = Read<WorldCullSlot>;
    type ItemQuery = Read<WorldBucketIndex>;

    fn render<'w>(
        _item: &P,
        slot: &'w WorldCullSlot,
        bucket: Option<&'w WorldBucketIndex>,
        (pass_data, allocator): bevy::ecs::system::SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let Some((built, index)) = pass_data.into_inner().built.as_ref().zip(bucket) else { return RenderCommandResult::Skip };
        let allocator = allocator.into_inner();
        let bucket = &built.buckets[index.0];
        let (Some(vertices), Some(indices)) = (allocator.mesh_vertex_slice(&bucket.mesh), allocator.mesh_index_slice(&bucket.mesh)) else { return RenderCommandResult::Skip };
        pass.set_vertex_buffer(0, vertices.buffer.slice(..));
        pass.set_index_buffer(indices.buffer.slice(..), bucket.index_format);
        let args_offset = (u64::from(slot.0) * u64::from(built.chunk_count) + u64::from(bucket.first_draw)) * ARGS_SIZE;
        let count_offset = (u64::from(slot.0) * built.buckets.len() as u64 + index.0 as u64) * COUNT_SIZE;
        pass.multi_draw_indexed_indirect_count(&built.args, args_offset, &built.counts, count_offset, bucket.draws);
        RenderCommandResult::Success
    }
}

type DrawWorld = (SetItemPipeline, SetMeshViewBindGroup<0>, SetMeshViewBindingArrayBindGroup<1>, SetWorldMesh<2>, SetWorldMaterial<3>, DrawWorldBucket);
type DrawWorldPrepass = (SetItemPipeline, SetPrepassViewBindGroup<0>, SetPrepassViewEmptyBindGroup<1>, SetWorldMesh<2>, SetWorldMaterial<3>, DrawWorldBucket);
type DrawWorldDepthOnly = (SetItemPipeline, SetPrepassViewBindGroup<0>, SetPrepassViewEmptyBindGroup<1>, SetWorldMesh<2>, SetPrepassEmptyMaterialBindGroup<3>, DrawWorldBucket);

pub struct WorldPassPlugin;

impl Plugin for WorldPassPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WorldDraws>().init_resource::<WorldAssets>().init_resource::<WorldSeenZones>();
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [(CULL_SHADER, Shader::from_wesl(include_str!("world_cull.wesl"), "p2_engine/world_cull.wesl"))]);
        app.add_plugins((ExtractResourcePlugin::<WorldDraws>::default(), ExtractResourcePlugin::<WorldSeenZones>::default(), ExtractComponentPlugin::<WorldCamera>::default()));
    }

    fn finish(&self, app: &mut App) {
        let Some(render) = app.get_sub_app_mut(RenderApp) else { return };
        render
            .init_resource::<WorldPass>()
            .init_resource::<ModifiedMeshes>()
            .add_render_command::<Opaque3d, DrawWorld>()
            .add_render_command::<AlphaMask3d, DrawWorld>()
            .add_render_command::<Opaque3dPrepass, DrawWorldPrepass>()
            .add_render_command::<Opaque3dPrepass, DrawWorldDepthOnly>()
            .add_render_command::<AlphaMask3dPrepass, DrawWorldPrepass>()
            .add_systems(ExtractSchedule, extract_modified_meshes)
            .add_systems(Render, queue_world.in_set(RenderSystems::QueueMeshes))
            .add_systems(Render, prepare_world_views.in_set(RenderSystems::PrepareResources))
            .add_systems(Core3d, cull_world.before(Core3dSystems::Prepass));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::RenderAssetUsages;
    use bevy::camera::CameraProjection;

    fn strip(x_starts: &[f32]) -> Mesh {
        let positions: Vec<[f32; 3]> = x_starts.iter().flat_map(|&x| [[x, 0.0, 0.0], [x + 1.0, 0.0, 0.0], [x, 1.0, 0.0]]).collect();
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh
    }

    #[test]
    fn triangles_gather_by_cell_and_every_chunk_keeps_its_box() {
        let mut mesh = strip(&[0.0, 5000.0, 10.0]);
        let chunks = chunks_of(&mut mesh).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks.iter().map(|chunk| chunk.index_count).sum::<u32>(), 9);
        assert_eq!(chunks[0].first_index, 0);
        assert_eq!(chunks[0].index_count, 6);
        assert_eq!(chunks[0].center.x, 5.5);
        assert_eq!(chunks[1].center.x, 5000.5);
        let indices: Vec<usize> = mesh.indices().unwrap().iter().collect();
        assert_eq!(indices, [0, 1, 2, 6, 7, 8, 3, 4, 5]);
    }

    #[test]
    fn a_box_follows_rotation_and_scale() {
        let chunk = Chunk { first_index: 0, index_count: 3, center: Vec3::X, half: Vec3::new(1.0, 2.0, 3.0) };
        let turned = Affine3A::from_scale_rotation_translation(Vec3::splat(2.0), bevy::math::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2), Vec3::Z);
        let (center, half) = world_box(&turned, &chunk);
        assert!(center.abs_diff_eq(Vec3::new(0.0, 2.0, 1.0), 1e-5));
        assert!(half.abs_diff_eq(Vec3::new(4.0, 2.0, 6.0), 1e-5));
    }

    #[test]
    fn planes_keep_what_the_camera_sees_and_drop_what_is_behind_it() {
        let clip = bevy::camera::PerspectiveProjection { near: 1.0, ..Default::default() }.get_clip_from_view();
        let planes = view_planes(clip);
        let seen = |point: Vec3| planes.iter().all(|plane| plane.truncate().dot(point) + plane.w >= 0.0);
        assert!(seen(Vec3::new(0.0, 0.0, -100.0)));
        assert!(seen(Vec3::new(0.0, 0.0, -100_000.0)));
        assert!(!seen(Vec3::new(0.0, 0.0, 100.0)));
        assert!(!seen(Vec3::new(1000.0, 0.0, -100.0)));
    }
}
