use super::*;

pub(crate) struct SetWorldMesh<const I: usize>;

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

pub(crate) struct SetWorldMaterial<const I: usize>;

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
        let Some(bind_group) = material_group(built, allocators.into_inner(), bucket.0) else { return RenderCommandResult::Skip };
        pass.set_bind_group(I, bind_group, &[]);
        RenderCommandResult::Success
    }
}

pub(super) fn material_group<'w>(built: &'w Built, allocators: &'w MaterialBindGroupAllocators, index: usize) -> Option<&'w BindGroup> {
    let bucket = &built.buckets[index];
    allocators.get(&bucket.type_id)?.get(bucket.group)?.bind_group()
}

pub(super) fn draw_bucket<'w>(pass: &mut TrackedRenderPass<'w>, built: &'w Built, allocator: &'w MeshAllocator, index: usize, slot: u32) -> RenderCommandResult {
    let bucket = &built.buckets[index];
    let (Some(vertices), Some(indices)) = (allocator.mesh_vertex_slice(&bucket.mesh), allocator.mesh_index_slice(&bucket.mesh)) else { return RenderCommandResult::Skip };
    pass.set_vertex_buffer(0, vertices.buffer.slice(..));
    pass.set_index_buffer(indices.buffer.slice(..), bucket.index_format);
    let args_offset = (u64::from(slot) * u64::from(built.chunk_count) + u64::from(bucket.first_draw)) * ARGS_SIZE;
    let count_offset = (u64::from(slot) * built.buckets.len() as u64 + index as u64) * COUNT_SIZE;
    pass.multi_draw_indexed_indirect_count(&built.args, args_offset, &built.counts, count_offset, bucket.draws);
    RenderCommandResult::Success
}

pub(crate) struct DrawWorldBucket;

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
        draw_bucket(pass, built, allocator.into_inner(), index.0, slot.0)
    }
}

pub(super) type DrawWorld = (SetItemPipeline, SetMeshViewBindGroup<0>, SetMeshViewBindingArrayBindGroup<1>, SetWorldMesh<2>, SetWorldMaterial<3>, DrawWorldBucket);
pub(super) type DrawWorldPrepass = (SetItemPipeline, SetPrepassViewBindGroup<0>, SetPrepassViewEmptyBindGroup<1>, SetWorldMesh<2>, SetWorldMaterial<3>, DrawWorldBucket);
pub(super) type DrawWorldDepthOnly = (SetItemPipeline, SetPrepassViewBindGroup<0>, SetPrepassViewEmptyBindGroup<1>, SetWorldMesh<2>, SetPrepassEmptyMaterialBindGroup<3>, DrawWorldBucket);
