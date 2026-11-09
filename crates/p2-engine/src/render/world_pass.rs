//! The static world drawn from one table instead of one entity per section, culled in a compute pass. Sections keep
//! their Bevy mesh and material assets, so pipelines, shaders and bindless slots stay Bevy's.

use std::any::TypeId;
use std::num::NonZero;
use std::sync::Arc;

use bevy::app::{App, Plugin};
use bevy::asset::uuid::Uuid;
use bevy::asset::{AssetEvent, AssetId, Handle, UntypedAssetId, uuid_handle};
use bevy::core_pipeline::core_3d::{AlphaMask3d, Opaque3d, Opaque3dBatchSetKey, Opaque3dBinKey};
use bevy::core_pipeline::prepass::{
    AlphaMask3dPrepass, Opaque3dPrepass, OpaqueNoLightmap3dBatchSetKey, OpaqueNoLightmap3dBinKey,
};
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::ecs::prelude::{
    Commands, Component, Entity, Local, MessageReader, Mut, Query, Res, ResMut, Resource, With, World,
};
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
use bevy::render::render_resource::binding_types::{
    storage_buffer_read_only_sized, storage_buffer_sized, uniform_buffer_sized,
};
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BindingResource, Buffer, BufferDescriptor, BufferUsages, CachedComputePipelineId,
    BufferId, SamplerId, TextureViewId, ComputePassDescriptor, ComputePipelineDescriptor, IndexFormat, PipelineCache, ShaderStages, StorageBuffer,
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
mod buffers;
mod build;
mod queue;
mod cull;
mod draw;
use buffers::{
    chunk_table, cull_buffers, cull_pipeline, lightmap_arrays_usable, lightmap_slots, mesh_bind_groups,
    prepass_reads_material, push_u32s, section_uniform,
};
use build::{extract_modified_meshes, rebuild_if_stale};
use queue::queue_world;
use cull::{cull_world, prepare_world_views};
use draw::{DrawWorld, DrawWorldDepthOnly, DrawWorldPrepass};

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
pub(crate) struct WorldBucketIndex(usize);

#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WorldCullSlot(u32);

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
    lightmap_views: Vec<(AssetId<Image>, TextureViewId, SamplerId)>,
    live_views: u32,
}

#[derive(Resource, Default)]
pub(crate) struct WorldPass {
    built: Option<Built>,
    pipeline: Option<(BindGroupLayoutDescriptor, CachedComputePipelineId)>,
    queued: HashMap<RetainedViewEntity, (MeshPipelineKey, Option<MeshPipelineKey>)>,
    waited_frames: u32,
}

#[derive(Resource, Default)]
struct ModifiedMeshes(Vec<AssetId<Mesh>>);

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
    use super::buffers::world_box;
    use super::cull::view_planes;
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
