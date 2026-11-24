use core::hash::{Hash, Hasher};
use std::hash::DefaultHasher;

use bevy::app::{App, Plugin};
use bevy::core_pipeline::mip_generation::experimental::depth::ViewDepthPyramid;
use bevy::core_pipeline::prepass::{PreviousViewUniformOffset, PreviousViewUniforms};
use bevy::ecs::prelude::{Entity, Local, Query, Res};
use bevy::ecs::schedule::{IntoScheduleConfigs, ScheduleCleanupPolicy};
use bevy::pbr::{MeshCullingDataBuffer, MeshInputUniform, MeshUniform, prepare_preprocess_bind_groups, prepare_preprocess_pipelines};
use bevy::render::batching::gpu_preprocessing::{BatchedInstanceBuffers, IndirectParametersBuffers, IndirectParametersBuildJobs, PreprocessWorkItemBuffers, SceneUnpackingBuffers};
use bevy::render::render_resource::Buffer;
use bevy::render::view::{ExtractedView, RenderVisibilityRanges, ViewUniforms};
use bevy::render::{Render, RenderApp, RenderSystems};

/// Keeps last frame's mesh preprocess bind groups while nothing they bind has changed.
pub struct PreprocessReusePlugin;

impl Plugin for PreprocessReusePlugin {
    fn build(&self, _app: &mut App) {}

    // In cleanup: Bevy's preprocess plugin adds the system in its finish.
    fn cleanup(&self, app: &mut App) {
        let Some(render) = app.get_sub_app_mut(RenderApp) else { return };
        if render.remove_systems_in_set(Render, prepare_preprocess_bind_groups, ScheduleCleanupPolicy::RemoveSystemsOnly).is_ok_and(|removed| removed > 0) {
            render.add_systems(
                Render,
                prepare_preprocess_bind_groups.run_if(preprocess_inputs_changed).in_set(RenderSystems::PrepareBindGroups).after(prepare_preprocess_pipelines),
            );
        }
    }
}

// Every buffer id, bound size, offset and dispatch count Bevy's system reads; its bind groups and counts
// depend on these alone.
#[expect(clippy::too_many_arguments, reason = "the condition reads every input of the system it guards")]
fn preprocess_inputs_changed(
    views: Query<(Entity, &ExtractedView)>,
    pyramids: Query<(&ViewDepthPyramid, &PreviousViewUniformOffset)>,
    instances: Option<Res<BatchedInstanceBuffers<MeshUniform, MeshInputUniform>>>,
    indirect: Option<Res<IndirectParametersBuffers>>,
    build_jobs: Option<Res<IndirectParametersBuildJobs>>,
    unpacking: Option<Res<SceneUnpackingBuffers>>,
    culling: Option<Res<MeshCullingDataBuffer>>,
    ranges: Option<Res<RenderVisibilityRanges>>,
    view_uniforms: Option<Res<ViewUniforms>>,
    previous_view_uniforms: Option<Res<PreviousViewUniforms>>,
    mut last: Local<u64>,
) -> bool {
    let (Some(instances), Some(indirect), Some(build_jobs), Some(unpacking), Some(culling), Some(ranges), Some(view_uniforms), Some(previous_view_uniforms)) =
        (instances, indirect, build_jobs, unpacking, culling, ranges, view_uniforms, previous_view_uniforms)
    else {
        return false;
    };
    let id = |buffer: Option<&Buffer>| buffer.map(Buffer::id);
    let mut hasher = DefaultHasher::new();
    (id(instances.current_input_buffer.buffer().buffer()), id(instances.previous_input_buffer.buffer())).hash(&mut hasher);
    (id(view_uniforms.uniforms.buffer()), id(previous_view_uniforms.uniforms.buffer()), id(culling.buffer()), id(ranges.buffer().buffer()), id(build_jobs.buffer())).hash(&mut hasher);
    (id(unpacking.bin_unpacking_metadata.buffer()), id(unpacking.uniform_allocation_metadata.buffer())).hash(&mut hasher);
    for (entity, view) in &views {
        (entity, &view.retained_view_entity).hash(&mut hasher);
        pyramids.get(entity).ok().map(|(pyramid, previous)| (pyramid.all_mips.id(), previous.offset)).hash(&mut hasher);
    }
    for (phase, buffers) in &instances.phase_instance_buffers {
        (phase, id(buffers.data_buffer.buffer()), id(buffers.late_indexed_indirect_parameters_buffer.buffer()), id(buffers.late_non_indexed_indirect_parameters_buffer.buffer())).hash(&mut hasher);
        for (view, items) in &buffers.work_item_buffers {
            (view, core::mem::discriminant(items)).hash(&mut hasher);
            match items {
                PreprocessWorkItemBuffers::Direct(items) => (id(items.buffer()), items.len()).hash(&mut hasher),
                PreprocessWorkItemBuffers::Indirect { indexed, non_indexed, gpu_occlusion_culling } => {
                    (id(indexed.buffer()), indexed.len(), id(non_indexed.buffer()), non_indexed.len()).hash(&mut hasher);
                    gpu_occlusion_culling
                        .as_ref()
                        .map(|late| (id(late.late_indexed.buffer()), late.late_indexed.len(), id(late.late_non_indexed.buffer()), late.late_non_indexed.len()))
                        .hash(&mut hasher);
                }
            }
        }
    }
    for (phase, buffers) in &indirect.buffers {
        phase.hash(&mut hasher);
        (id(buffers.indexed.metadata_buffer()), id(buffers.indexed.data_buffer()), id(buffers.indexed.batch_sets_buffer()), buffers.indexed.batch_count()).hash(&mut hasher);
        (id(buffers.non_indexed.metadata_buffer()), id(buffers.non_indexed.data_buffer()), id(buffers.non_indexed.batch_sets_buffer()), buffers.non_indexed.batch_count()).hash(&mut hasher);
    }
    for (key, buffers) in &unpacking.view_phase_buffers {
        (key, buffers.indexed_unpacking_jobs.len()).hash(&mut hasher);
        for job in buffers.indexed_unpacking_jobs.iter().chain(&buffers.non_indexed_unpacking_jobs) {
            (job.render_binned_mesh_instance_buffer.id(), job.bin_metadata_buffer.id(), job.fan_buffer.id(), job.bin_index_to_bin_metadata_index_buffer.id()).hash(&mut hasher);
            (job.bin_unpacking_metadata_index.uniform_offset(), job.uniform_allocation_metadata_index.uniform_offset(), job.mesh_instance_count, job.bin_count).hash(&mut hasher);
        }
    }
    let now = hasher.finish();
    core::mem::replace(&mut *last, now) != now
}
