use bevy::ecs::prelude::{Res, ResMut};
use bevy::time::{Real, Time};
use p2_render::compiling::CompilingPipelines;

use super::{Play, PlayWorld};
use crate::map::SpawnedActors;

/// The renderer meets a new mesh or material a few frames after it is spawned, so one quiet frame says little.
const CALM_FRAMES: u32 = 20;
/// Seconds; a pipeline or texture that never finishes must not keep the load screen up.
const HOLD_LIMIT: f32 = 10.0;

/// After a load the map's first frame is presented behind the load screen and the game waits while the renderer
/// builds the pipelines that frame draws with and the textures and materials it names reach the GPU, so the first
/// seconds of play neither skip draws, pop in nor hitch.
#[derive(Default)]
pub(super) struct RevealHold {
    held: f32,
    calm: u32,
    frames: u32,
}

impl RevealHold {
    fn over_after(&mut self, seconds: f32, unready: usize) -> bool {
        // The first frame counted is the map's own heavy first frame, not time spent waiting.
        if self.frames > 0 {
            self.held += seconds;
        }
        self.frames += 1;
        self.calm = if unready == 0 { self.calm + 1 } else { 0 };
        self.calm >= CALM_FRAMES || self.held >= HOLD_LIMIT
    }
}

impl PlayWorld {
    pub fn hold_reveal(&mut self) {
        self.reveal = Some(RevealHold::default());
    }

    pub(super) fn holding(&self) -> bool {
        self.presented && self.reveal.is_some()
    }
}

fn unready(compiling: Option<&CompilingPipelines>, spawned: Option<&SpawnedActors>) -> usize {
    compiling.map_or(0, |compiling| compiling.count() + compiling.waiting_draws()) + spawned.map_or(0, SpawnedActors::pending)
}

pub(super) fn release_reveal(time: Res<Time<Real>>, compiling: Option<Res<CompilingPipelines>>, spawned: Option<Res<SpawnedActors>>, mut play: ResMut<Play>) {
    if !play.world().is_some_and(PlayWorld::holding) {
        return;
    }
    let unready = unready(compiling.as_deref(), spawned.as_deref());
    let Some(world) = play.world_mut() else { return };
    if world.reveal.as_mut().is_some_and(|hold| hold.over_after(time.delta_secs(), unready)) {
        world.reveal = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hold_ends_after_enough_quiet_frames_in_a_row() {
        let mut hold = RevealHold::default();
        assert!(!(1..CALM_FRAMES).any(|_| hold.over_after(0.01, 0)));
        assert!(!hold.over_after(0.01, 3), "a pipeline queued again starts the count over");
        assert!(!(1..CALM_FRAMES).any(|_| hold.over_after(0.01, 0)));
        assert!(hold.over_after(0.01, 0));
    }

    #[test]
    fn a_pipeline_that_never_finishes_ends_the_hold_at_the_limit() {
        let mut hold = RevealHold::default();
        assert!(!hold.over_after(HOLD_LIMIT * 2.0, 1), "the map's first frame is not waiting");
        assert!(!hold.over_after(HOLD_LIMIT * 0.5, 1));
        assert!(!hold.over_after(HOLD_LIMIT * 0.25, 1));
        assert!(hold.over_after(HOLD_LIMIT * 0.25, 1));
    }

    #[test]
    fn without_a_shown_world_nothing_is_held() {
        let mut world = bevy::ecs::world::World::new();
        world.init_resource::<Play>();
        world.init_resource::<Time<Real>>();
        world.init_resource::<CompilingPipelines>();
        world.init_resource::<SpawnedActors>();
        world.run_system_cached(release_reveal).expect("running the release");
        assert!(world.resource::<Play>().world().is_none());
    }

    #[test]
    fn nothing_is_unready_in_a_bare_world() {
        let mut world = bevy::ecs::world::World::new();
        assert_eq!(unready(world.get_resource::<CompilingPipelines>(), world.get_resource::<SpawnedActors>()), 0);
        world.init_resource::<CompilingPipelines>();
        world.init_resource::<SpawnedActors>();
        assert_eq!(unready(world.get_resource::<CompilingPipelines>(), world.get_resource::<SpawnedActors>()), 0);
    }
}
