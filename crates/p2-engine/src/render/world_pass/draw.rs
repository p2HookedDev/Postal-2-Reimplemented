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
        let bucket = &built.buckets[bucket.0];
        let Some(allocator) = allocators.into_inner().get(&bucket.type_id) else { return RenderCommandResult::Skip };
        let Some(bind_group) = allocator.get(bucket.group).and_then(|group| group.bind_group()) else { return RenderCommandResult::Skip };
        pass.set_bind_group(I, bind_group, &[]);
        RenderCommandResult::Success
    }
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

pub(super) type DrawWorld = (SetItemPipeline, SetMeshViewBindGroup<0>, SetMeshViewBindingArrayBindGroup<1>, SetWorldMesh<2>, SetWorldMaterial<3>, DrawWorldBucket);
pub(super) type DrawWorldPrepass = (SetItemPipeline, SetPrepassViewBindGroup<0>, SetPrepassViewEmptyBindGroup<1>, SetWorldMesh<2>, SetWorldMaterial<3>, DrawWorldBucket);
pub(super) type DrawWorldDepthOnly = (SetItemPipeline, SetPrepassViewBindGroup<0>, SetPrepassViewEmptyBindGroup<1>, SetWorldMesh<2>, SetPrepassEmptyMaterialBindGroup<3>, DrawWorldBucket);
