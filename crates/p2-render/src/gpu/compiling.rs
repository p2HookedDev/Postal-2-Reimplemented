use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bevy::app::{App, Plugin};
use bevy::ecs::prelude::{Res, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::pbr::PendingMeshMaterialQueues;
use bevy::render::render_resource::{CachedPipelineState, PipelineCache};
use bevy::render::{Render, RenderApp, RenderSystems};

#[derive(Resource, Clone, Default)]
pub struct CompilingPipelines {
    creating: Arc<AtomicUsize>,
    waiting: Arc<AtomicUsize>,
}

impl CompilingPipelines {
    pub fn count(&self) -> usize {
        self.creating.load(Ordering::Relaxed)
    }

    pub fn waiting_draws(&self) -> usize {
        self.waiting.load(Ordering::Relaxed)
    }
}

pub struct CompilingPipelinesPlugin;

impl Plugin for CompilingPipelinesPlugin {
    fn build(&self, app: &mut App) {
        let shared = CompilingPipelines::default();
        app.insert_resource(shared.clone());
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.insert_resource(shared).add_systems(Render, (count_compiling, count_waiting).in_set(RenderSystems::Cleanup));
    }
}

/// Only those being created: one still queued at the end of a frame waits on a shader that may never load.
fn count_compiling(cache: Res<PipelineCache>, shared: Res<CompilingPipelines>) {
    let compiling = cache.pipelines().filter(|pipeline| matches!(pipeline.state, CachedPipelineState::Creating(_))).count();
    shared.creating.store(compiling, Ordering::Relaxed);
}

fn count_waiting(pending: Option<Res<PendingMeshMaterialQueues>>, shared: Res<CompilingPipelines>) {
    let waiting = pending.map_or(0, |queues| queues.0.values().map(|view| view.current_frame.len()).sum());
    shared.waiting.store(waiting, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::world::World;
    use bevy::render::sync_world::MainEntity;
    use bevy::render::view::RetainedViewEntity;

    #[test]
    fn meshes_skipped_for_their_materials_are_counted_over_every_view() {
        let mut world = World::new();
        let shared = CompilingPipelines::default();
        world.insert_resource(shared.clone());
        world.run_system_cached(count_waiting).expect("counting without the queues");
        assert_eq!(shared.waiting_draws(), 0);

        let mut queues = PendingMeshMaterialQueues::default();
        let entities: Vec<_> = (0..3).map(|_| world.spawn_empty().id()).collect();
        for (subview, skipped) in [(0, &entities[..2]), (1, &entities[2..])] {
            let view = queues.0.prepare_for_new_frame(RetainedViewEntity::new(MainEntity::from(entities[0]), None, subview));
            view.current_frame.extend(skipped.iter().map(|&entity| (entity, MainEntity::from(entity))));
        }
        world.insert_resource(queues);
        world.run_system_cached(count_waiting).expect("counting the skipped meshes");
        assert_eq!(shared.waiting_draws(), 3);
        assert_eq!(shared.count(), 0);

        for view in world.resource_mut::<PendingMeshMaterialQueues>().0.values_mut() {
            view.current_frame.clear();
        }
        world.run_system_cached(count_waiting).expect("counting once they are drawn");
        assert_eq!(shared.waiting_draws(), 0);
    }
}
