//! Play mode: the shown map's VM runs on a thread of its own while the main thread presents the frame before it: at the
//! hand-off (`PlaySystems::Tick`) the main thread takes the finished frame's [`VmFrame`] and gives the VM the next input.

use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use bevy::app::{App, AppExit, First, Last, Plugin, PostUpdate, Update};
use bevy::asset::{AssetEventSystems, Assets, Handle};
use bevy::audio::{
    AudioPlayer, AudioSink, AudioSinkPlayback, AudioSource, PlaybackSettings, SpatialAudioSink, SpatialListener,
    SpatialScale, Volume,
};
use bevy::camera::Camera;
use bevy::camera::visibility::{Visibility, VisibleEntities};
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut};
use bevy::ecs::prelude::{
    ChildOf, Commands, Component, Entity, Has, Local, MessageReader, Query, Res, ResMut, Resource, SystemSet, With,
    Without,
};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::SystemParam;
use bevy::ecs::world::EntityWorldMut;
use bevy::math::Vec3;
use bevy::mesh::Mesh3d;
use bevy::time::Time;
use bevy::transform::TransformSystems;
use bevy::transform::components::Transform;
use p2_assets::MeshAnimation;
use p2_bevy_assets::to_bevy;
use p2_package::Loader;
use p2_particles::{ActorForce, EmitterProperties};
use p2_vm::{
    CanvasItem, HostCommand, Kind, MusicCommand, ObjectId, ParticleCommand, PlayedSound, ProjectorEvent, Prop, Vm,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::actors::{
    BoneAttachment, EmitterActor, MeshActor, ProjectorActor, SkeletalActor, actor_glow, actor_look, actor_scale, bone_attachment, emitter_actor,
    emitter_properties, emitters_disabled, location_rotation, mesh_actor, object_path, projector_actor, skeletal_actor, skin_paths, vector,
};
use crate::animation::{AttachmentPlaces, BoneFrame, FollowBone};
use crate::components::{ActorModel, CanvasDrawn, FollowActor, SceneView};
use crate::mesh_view::ShadowOnly;

mod times;
mod vm_thread;
mod world;
mod sound;

pub use times::*;
pub use vm_thread::*;
pub use world::*;
use sound::{
    pause_movie_sound, fade_sounds, sound_limits, pitch_factor, heard_volume, stop_old_songs, play_music,
    play_world_sounds, ambient_gain,
};
pub(crate) use sound::{MovieSound, sound_listener};

const MAX_STEP: f32 = 0.1;

/// Frames whose times the game's step is the mean of ([`StepClock`]): a multiple of 2, 3 and 4, so frame times that
/// alternate in such a pattern (vsync: 3 and 4 refreshes of a 240 Hz display) give an even step.
const STEP_FRAMES: usize = 12;

/// Stack of every thread that runs scripts (bytes): scripts call by recursion, up to the engine's 1000 nested calls.
/// The VM's thread and the system threads ([`init_script_stacks`]) get it; each program's `build.rs` gives the main
/// thread the same by hand (`/STACK:67108864`): the three must match.
pub const SCRIPT_STACK: usize = 64 << 20;

/// Gives Bevy's system threads [`SCRIPT_STACK`] (the main thread helps run systems too, its stack is set at link
/// time). Call before the app is built: the compute pool made here is the one the app's `TaskPoolPlugin` keeps
/// (`get_or_init`).
pub fn init_script_stacks() {
    // Every core but those the main, render and VM threads keep busy.
    let threads = bevy::tasks::available_parallelism().saturating_sub(3).max(1);
    bevy::tasks::ComputeTaskPool::get_or_init(|| {
        bevy::tasks::TaskPoolBuilder::new().num_threads(threads).stack_size(SCRIPT_STACK).thread_name("Compute Task Pool".into()).build()
    });
}

pub struct PlayPlugin;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlaySystems {
    Input,
    Tick,
    /// (`PostUpdate`) Before the renderer copies changed assets (`AssetEventSystems`: the particle meshes rewritten every
    /// frame) and before transforms propagate (the first-person weapon placed after `RenderOverlays`), so both show this
    /// frame, not one late.
    Present,
}

#[derive(Resource, Default)]
pub struct PlayerInput {
    pub playing: bool,
    pub held: Vec<&'static str>,
    pub key_events: Vec<(&'static str, bool)>,
    pub mouse: [f32; 2],
    pub typed: String,
    pub cursor: Option<[f32; 2]>,
}

impl Plugin for PlayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Play>()
            .init_resource::<AttachmentPlaces>()
            .init_resource::<ShadowOnly>()
            .init_resource::<FrameTimes>()
            .init_resource::<PlayerInput>()
            .configure_sets(Update, (PlaySystems::Input, PlaySystems::Tick).chain())
            .add_systems(First, end_frame)
            .add_systems(Last, finish_saves_on_exit)
            .add_systems(Update, (stop_old_songs, tick_play, publish_frame_lights, follow_actors, crate::map::face_sprites, crate::map::draw_coronas, heard_volume, sound_limits, fade_sounds, pause_movie_sound).chain().in_set(PlaySystems::Tick))
            // The renderer copies a changed asset only in the frame its change event is sent; unordered, that ran
            // before the particle meshes were rewritten on some frames, and those frames showed the previous ones.
            .configure_sets(PostUpdate, PlaySystems::Present.before(AssetEventSystems).before(TransformSystems::Propagate));
    }
}

/// Exit waits for the queued save writes: the IO pool's threads die with the process.
fn finish_saves_on_exit(mut exits: MessageReader<AppExit>) {
    if exits.read().next().is_some() {
        crate::saves::finish_writes();
    }
}

#[derive(Resource)]
pub struct Play {
    world: Option<PlayWorld>,
    generation: u64,
    pub threaded: bool,
    canvas: [f32; 2],
    clock: StepClock,
    pub smooth_step: bool,
}

impl Default for Play {
    fn default() -> Self {
        Self {
            world: None,
            generation: 0,
            threaded: true,
            canvas: [1024.0, 768.0],
            clock: StepClock::default(),
            smooth_step: true,
        }
    }
}

/// The mean of the last [`STEP_FRAMES`] frame times (each at most [`MAX_STEP`]). A frame's picture shows a frame or more
/// after its step was measured, so a frame's own time is wrong whenever frame times alternate (vsync at 3 and 4
/// refreshes); the mean moves the world evenly, and game time stays within a few frames of real time.
#[derive(Default)]
struct StepClock {
    recent: std::collections::VecDeque<f32>,
}

impl StepClock {
    fn step(&mut self, delta: f32) -> f32 {
        if self.recent.len() == STEP_FRAMES {
            self.recent.pop_front();
        }
        self.recent.push_back(delta.min(MAX_STEP));
        self.recent.iter().sum::<f32>() / self.recent.len() as f32
    }
}

impl Play {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub fn set(&mut self, world: Option<PlayWorld>) {
        self.world = world;
        self.generation += 1;
        self.clock = StepClock::default();
    }

    fn stepped_world(&mut self, delta: f32) -> Option<(f32, &mut PlayWorld)> {
        let world = self.world.as_mut()?;
        let step = if self.smooth_step { self.clock.step(delta) } else { delta.min(MAX_STEP) };
        Some((step, world))
    }

    pub fn world(&self) -> Option<&PlayWorld> {
        self.world.as_ref()
    }

    pub fn world_mut(&mut self) -> Option<&mut PlayWorld> {
        self.world.as_mut()
    }

    pub fn take_vm(&mut self) -> Option<Vm<Arc<Loader>>> {
        let mut world = self.world.take()?;
        self.generation += 1;
        world.wait();
        match world.sim {
            Slot::Here(sim) => Some(sim.vm),
            Slot::Away | Slot::Lost => None,
        }
    }

    pub fn canvas_size(&self) -> [f32; 2] {
        self.canvas
    }

    pub fn set_canvas_size(&mut self, size: [f32; 2]) {
        self.canvas = size;
    }
}

#[derive(SystemParam)]
struct FrameClock<'w> {
    time: Res<'w, Time>,
    times: ResMut<'w, FrameTimes>,
}

#[derive(SystemParam)]
struct PlayCameras<'w, 's> {
    cameras: Query<'w, 's, &'static Transform, With<SceneView>>,
    views: Query<'w, 's, &'static mut SceneView>,
}

#[derive(SystemParam)]
struct DrawnActors<'w, 's> {
    models: Query<'w, 's, (Entity, &'static FollowActor, Option<&'static ChildOf>)>,
    attached: Query<'w, 's, (Entity, &'static FollowBone, Option<&'static ChildOf>)>,
    cameras: Query<'w, 's, (&'static Camera, &'static VisibleEntities)>,
    shadow_only: Res<'w, ShadowOnly>,
    places: ResMut<'w, AttachmentPlaces>,
}

/// Not `ViewVisibility`, which the sun's shadow cascades set too: retail draws no such shadows, so the modern graphics
/// must not change what the game sees (`LastRenderTime`). `seen`: each sorted as Bevy keeps them.
fn camera_drew(seen: &[&[Entity]], shadow_only: &HashSet<Entity>, entity: Entity, root: Option<Entity>) -> bool {
    !root.is_some_and(|root| shadow_only.contains(&root)) && seen.iter().any(|meshes| meshes.binary_search(&entity).is_ok())
}

fn tick_play(
    mut commands: Commands,
    clock: FrameClock,
    mut play: ResMut<Play>,
    mut sources: ResMut<Assets<AudioSource>>,
    play_cameras: PlayCameras,
    mut input: ResMut<PlayerInput>,
    mut drawn: DrawnActors,
) {
    let FrameClock { time, mut times } = clock;
    let (generation, threaded, canvas) = (play.generation, play.threaded, play.canvas);
    let Some((step, world)) = play.stepped_world(time.delta_secs()) else {
        drawn.places.0.clear();
        return;
    };
    let job = next_job(world, drawn, &mut input, &times, step, canvas);
    let started = Instant::now();
    world.wait();
    times.add("vm wait", started);
    let inline = !threaded || !world.presented;
    let mut job = Some(job);
    if inline
        && world.frame.travel.is_none()
        && let Some(job) = job.take()
    {
        world.start(job, JobThread::Main);
    }
    let new = world.present();
    if world.frame.travel.is_none()
        && let Some(job) = job
    {
        world.start(job, JobThread::Worker);
    }
    if !new {
        return;
    }
    present_frame(&mut commands, world, &mut sources, &mut times, input.playing, play_cameras, generation);
}

fn next_job(world: &mut PlayWorld, drawn: DrawnActors, input: &mut PlayerInput, times: &FrameTimes, step: f32, canvas: [f32; 2]) -> Job {
    let DrawnActors { models, attached, cameras: drawing_cameras, shadow_only, mut places } = drawn;
    world.followed.clear();
    world.visible.clear();
    // An inactive camera keeps the list it last had, so only the active ones count.
    let meshes: Vec<&[Entity]> = drawing_cameras.iter().filter(|(camera, _)| camera.is_active).map(|(_, seen)| seen.get(TypeId::of::<Mesh3d>())).collect();
    let drew = |entity, parent: Option<&ChildOf>| camera_drew(&meshes, &shadow_only.0, entity, parent.map(ChildOf::parent));
    for (entity, follow, parent) in &models {
        world.followed.insert(follow.actor);
        if drew(entity, parent) {
            world.visible.insert(follow.actor);
        }
    }
    let mut frame = world.spare.take().unwrap_or_default();
    let JobBuffers { mut held, mut followed, visible: mut seen, mut placed, mut watched } = std::mem::take(&mut frame.job_buffers);
    followed.clear();
    followed.extend(&world.followed);
    followed.extend(attached.iter().flat_map(|(_, follow, _)| [follow.base, follow.actor]));
    seen.clear();
    seen.extend(&world.visible);
    seen.extend(attached.iter().filter(|&(entity, _, parent)| drew(entity, parent)).map(|(_, follow, _)| follow.actor));
    let visible = &world.visible;
    placed.clear();
    placed.extend(places.0.drain().filter(|(_, (base, _))| visible.contains(base)).map(|(actor, (_, place))| (actor, place)));
    held.clear();
    held.extend_from_slice(&input.held);
    watched.clear();
    watched.extend(world.watched.iter().copied());
    Job {
        frame,
        step,
        playing: input.playing,
        held,
        mouse: input.mouse,
        typed: std::mem::take(&mut input.typed),
        key_events: std::mem::take(&mut input.key_events),
        cursor: input.cursor,
        canvas,
        followed,
        visible: seen,
        placed,
        watched,
        finished: std::mem::take(&mut world.finished),
        vm_profile: times.vm_profile,
        forces: world.wants_forces,
        movie_status: std::mem::take(&mut world.movie_status),
    }
}

fn present_frame(
    commands: &mut Commands,
    world: &mut PlayWorld,
    sources: &mut Assets<AudioSource>,
    times: &mut FrameTimes,
    playing: bool,
    play_cameras: PlayCameras,
    generation: u64,
) {
    let PlayCameras { cameras, mut views } = play_cameras;
    let frame = &mut world.frame;
    times.add_vm(&mut frame.times, &mut frame.profile);
    if playing
        && let Some(view) = frame.view
    {
        for mut scene_view in &mut views {
            *scene_view = SceneView::look_from(view.location, view.rotation, world.origin);
        }
    }
    world.seconds += frame.step;
    let camera = cameras.iter().next().map(|camera| camera.translation);
    play_world_sounds(commands, world, sources, camera, generation);
    play_music(commands, world, sources, camera, generation);
}

fn follow_actors(
    mut commands: Commands,
    play: Res<Play>,
    mut times: ResMut<FrameTimes>,
    mut models: Query<(&mut FollowActor, &mut Transform, &mut Visibility, Has<CanvasDrawn>), Without<SceneView>>,
    roots: Query<(Entity, &ActorModel)>,
) {
    let started = Instant::now();
    let Some(world) = &play.world else { return };
    for (entity, model) in &roots {
        if world.is_gone(model.0) {
            commands.entity(entity).despawn();
        }
    }
    for (mut follow, mut transform, mut shown, canvas_drawn) in &mut models {
        if !(follow.is_added() || world.changed(follow.actor)) {
            continue;
        }
        let Some(actor) = world.actor(follow.actor) else { continue };
        // Destroyed (e.g. not needed this day: `PrepActorsByGroup`) or hidden actors are not drawn, except what the
        // HUD draws itself.
        let hidden = actor.gone || (!canvas_drawn && actor.hidden);
        shown.set_if_neq(if hidden { Visibility::Hidden } else { Visibility::Inherited });
        if follow.scale != actor.scale {
            follow.scale = actor.scale;
        }
        if follow.pre_pivot != actor.pre_pivot {
            follow.pre_pivot = actor.pre_pivot;
        }
        transform.set_if_neq(follow.transform(actor.location, actor.rotation));
    }
    times.add("follow actors", started);
}

impl crate::render::actor_light::ActorFrames for Play {
    fn origin(&self) -> Option<Vec3> {
        self.world().map(PlayWorld::origin)
    }

    fn actor_place(&self, actor: ObjectId) -> Option<crate::render::actor_light::ActorPlace> {
        let frame = self.world()?.actor(actor)?;
        Some(crate::render::actor_light::ActorPlace { location: frame.location, rotation: frame.rotation, glow: frame.glow })
    }

    fn attachment_base(&self, actor: ObjectId) -> Option<ObjectId> {
        self.world()?.actor(actor)?.attachment.as_ref().map(|attachment| attachment.base)
    }
}

/// Touched only when they changed, so the render systems' change detection stays meaningful.
fn publish_frame_lights(play: Res<Play>, mut frame: ResMut<crate::render::bsp_light::FrameLights>) {
    let lights = play.world().map_or(&[][..], |world| world.dynamic_lights());
    if frame.0 != lights {
        frame.0.clear();
        frame.0.extend_from_slice(lights);
    }
}

#[cfg(test)]
mod tests;
