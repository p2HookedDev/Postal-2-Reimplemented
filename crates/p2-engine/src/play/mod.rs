//! Play mode: the shown map's VM runs on a thread of its own while the main thread presents the frame before it: at the
//! hand-off (`PlaySystems::Tick`) the main thread takes the finished frame's [`VmFrame`] and gives the VM the next input.
//! A job that takes longer than a frame is not waited for: frames are drawn without it, the view turning with the mouse.

use std::any::TypeId;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use bevy::app::{App, AppExit, First, Last, Plugin, PostUpdate, Startup, Update};
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
    BoneAttachment, EmitterActor, MeshActor, ProjectorActor, SkeletalActor, actor_look, actor_scale, bone_attachment, emitter_actor,
    emitter_properties, emitters_disabled, location_rotation, mesh_actor, object_path, projector_actor, skeletal_actor, skin_paths,
};
use crate::animation::{AttachmentPlaces, BoneFrame, FollowBone};
use crate::components::{ActorModel, CanvasDrawn, FollowActor, OverlayLook, SceneView};
use crate::mesh_view::ShadowOnly;

mod times;
mod vm_thread;
mod world;
mod sound;

pub use times::*;
pub use vm_thread::*;
pub use world::*;
use sound::{ambient_gain, pitch_factor};
pub(crate) use sound::{MovieSound, sound_listener};

const MAX_STEP: f32 = 0.1;

/// The shortest time between two game ticks, in seconds. Frames are drawn apart from ticks and the world is blended
/// between them, so ticking faster than this only repeats work (scripts, physics, the reads after them) that no
/// frame shows sooner.
const TICK_PERIOD: f32 = 1.0 / 120.0;

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
        crate::add_once(app, sound::SoundPlugin);
        app.init_resource::<Play>()
            .init_resource::<AttachmentPlaces>()
            .init_resource::<ShadowOnly>()
            .init_resource::<FrameTimes>()
            .init_resource::<PlayerInput>()
            .configure_sets(Update, (PlaySystems::Input, PlaySystems::Tick).chain())
            .add_systems(First, end_frame)
            .add_systems(Last, finish_saves_on_exit)
            .add_systems(
                Update,
                (gather_input, hand_off, look_ahead, publish_frame_lights, publish_actor_frames, follow_actors, crate::map::face_sprites, crate::map::draw_coronas)
                    .chain()
                    .in_set(PlaySystems::Tick)
                    .after(sound::SoundSystems::Stop)
                    .before(sound::SoundSystems::Mix),
            )
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
    frame_clock: StepClock,
    /// Frame time the host's own simulations (particles, fluids) have not stepped yet: they step once per game tick,
    /// as the engine's, by the frame time since the tick before.
    unstepped: f32,
    /// Frame time not yet paid for with a game tick; a tick is due once a [`TICK_PERIOD`] of it has gathered.
    tick_debt: f32,
    pub smooth_step: bool,
    waited: f32,
    backlog: InputBacklog,
    look: LookAhead,
}

#[derive(Default)]
struct InputBacklog {
    typed: String,
    key_events: Vec<(&'static str, bool)>,
    mouse: [f32; 2],
}

impl Default for Play {
    fn default() -> Self {
        Self {
            world: None,
            generation: 0,
            threaded: true,
            canvas: [1024.0, 768.0],
            clock: StepClock::default(),
            frame_clock: StepClock::default(),
            unstepped: 0.0,
            tick_debt: 0.0,
            smooth_step: true,
            waited: 0.0,
            backlog: InputBacklog::default(),
            look: LookAhead::default(),
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

/// Turns the drawn view with the mouse before the game's scripts have: by how far the game's view turned for each
/// mouse count in its last ticks. Off (the game's own rotation, glided between ticks) whenever more than one of those
/// ticks disagrees: a cutscene, recoil, a pitch limit, mouse smoothing.
#[derive(Default)]
struct LookAhead {
    /// The mouse counts the running job took.
    in_flight: [f32; 2],
    /// Yaw and pitch, in rotator units per mouse count.
    samples: [std::collections::VecDeque<f32>; 2],
    seen: Option<p2_math::Rotator>,
    /// The actor the view is from: the samples start over when it changes (a cutscene's camera).
    viewer: Option<ObjectId>,
}

const LOOK_SAMPLES: usize = 5;
/// Samples that may disagree with the rest while the look still runs ahead: more keeps it on through a recoil tick, but
/// keeps a wrong scale longer after a zoom.
const LOOK_OUTLIERS: usize = 1;
/// Mouse counts below which a tick's turn says nothing about the scale.
const LOOK_MIN_COUNTS: f32 = 1.0;
/// How far the samples may differ from their median for the look to run ahead.
const LOOK_AGREEMENT: f32 = 0.1;
/// The pitch the look ahead stops at, down and up: `PlayerController` keeps the view's within it (unverified).
const LOOK_PITCH_LIMITS: [i32; 2] = [-16384, 18000];

fn shortest_turn(from: i32, to: i32) -> i32 {
    i32::from(to.wrapping_sub(from) as i16)
}

impl LookAhead {
    fn learn(&mut self, now: p2_math::Rotator, viewer: Option<ObjectId>) {
        if self.viewer != viewer {
            *self = Self { in_flight: self.in_flight, viewer, ..Self::default() };
        }
        if let Some(before) = self.seen.replace(now) {
            let turns = [shortest_turn(before.yaw, now.yaw), shortest_turn(before.pitch, now.pitch)];
            for ((samples, counts), turn) in self.samples.iter_mut().zip(self.in_flight).zip(turns) {
                if counts.abs() >= LOOK_MIN_COUNTS {
                    if samples.len() == LOOK_SAMPLES {
                        samples.pop_front();
                    }
                    samples.push_back(turn as f32 / counts);
                }
            }
        }
    }

    fn scale(samples: &std::collections::VecDeque<f32>) -> f32 {
        if samples.len() < LOOK_SAMPLES {
            return 0.0;
        }
        let mut sorted: Vec<f32> = samples.iter().copied().collect();
        sorted.sort_by(f32::total_cmp);
        let median = sorted[LOOK_SAMPLES / 2];
        let agree = sorted.iter().filter(|&&sample| (sample - median).abs() <= median.abs() * LOOK_AGREEMENT).count();
        if agree + LOOK_OUTLIERS >= LOOK_SAMPLES { median } else { 0.0 }
    }

    /// `rotation` (the newest tick's) turned by the mouse counts the game has not shown yet: the running job's and
    /// `waiting`. An axis that does not run ahead shows `glided`, the rotation between the last ticks.
    fn ahead(&self, rotation: p2_math::Rotator, glided: p2_math::Rotator, waiting: [f32; 2]) -> p2_math::Rotator {
        let scales = [0, 1].map(|i| Self::scale(&self.samples[i]));
        if scales == [0.0; 2] {
            return glided;
        }
        let [yaw, pitch] = [0, 1].map(|i| (scales[i] * (self.in_flight[i] + waiting[i])) as i32);
        let axes = |rotation: p2_math::Rotator| [rotation.yaw, rotation.pitch];
        let [base_yaw, base_pitch] = [0, 1].map(|i| if scales[i] == 0.0 { axes(glided)[i] } else { axes(rotation)[i] });
        let level = shortest_turn(0, base_pitch);
        let [down, up] = LOOK_PITCH_LIMITS;
        p2_math::Rotator { yaw: base_yaw.wrapping_add(yaw), pitch: (level + pitch).clamp(down.min(level), up.max(level)), ..rotation }
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
        self.frame_clock = StepClock::default();
        self.unstepped = 0.0;
        self.tick_debt = 0.0;
        self.waited = 0.0;
        self.backlog = InputBacklog::default();
        self.look = LookAhead::default();
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
        world.wait(VmWait::UntilBack);
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
struct PlayCameras<'w, 's> {
    views: Query<'w, 's, &'static mut SceneView>,
    overlay_look: ResMut<'w, OverlayLook>,
}

impl PlayCameras<'_, '_> {
    /// The game's view, its rotation run ahead by the mouse the game has not shown yet.
    fn look(&mut self, world: &PlayWorld, look: &LookAhead, waiting: [f32; 2], playing: bool) {
        let view = world.frame.view.filter(|_| playing);
        let held = world.frame.hud_view.or(world.frame.view).filter(|_| playing).map(|view| SceneView::look_from(view.location, view.rotation, world.origin)).map(|game| Transform::from_translation(game.position).with_rotation(game.rotation));
        if self.overlay_look.0 != held {
            self.overlay_look.0 = held;
        }
        let Some(view) = view else { return };
        let (location, rotation) = world.shown_view((view.location, view.rotation));
        let shown = SceneView::look_from(location, look.ahead(view.rotation, rotation, waiting), world.origin);
        for mut scene_view in &mut self.views {
            if scene_view.position != shown.position || scene_view.rotation != shown.rotation {
                *scene_view = shown;
            }
        }
    }
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
fn camera_drew(seen: &[&[Entity]], shadow_only: &bevy::ecs::entity::EntityHashSet, entity: Entity, root: Option<Entity>) -> bool {
    !root.is_some_and(|root| shadow_only.contains(&root)) && seen.iter().any(|meshes| meshes.binary_search(&entity).is_ok())
}

fn gather_input(time: Res<Time>, mut play: ResMut<Play>, mut input: ResMut<PlayerInput>) {
    let play = &mut *play;
    let Some(world) = play.world.as_mut() else { return };
    play.waited += time.delta_secs();
    play.backlog.typed.push_str(&std::mem::take(&mut input.typed));
    play.backlog.key_events.append(&mut input.key_events);
    play.backlog.mouse = [0, 1].map(|i| play.backlog.mouse[i] + input.mouse[i]);
    play.unstepped += if play.smooth_step { play.frame_clock.step(time.delta_secs()) } else { time.delta_secs().min(MAX_STEP) };
    play.tick_debt = (play.tick_debt + time.delta_secs()).min(TICK_PERIOD * 2.0);
    world.set_glide(time.delta_secs());
}

fn hand_off(mut times: ResMut<FrameTimes>, mut play: ResMut<Play>, input: Res<PlayerInput>, mut drawn: DrawnActors) {
    let (threaded, canvas) = (play.threaded, play.canvas);
    let play = &mut *play;
    let Some(world) = play.world.as_mut() else {
        drawn.places.0.clear();
        return;
    };
    if !world.wait(VmWait::Look) {
        world.changed.clear();
        world.frame.step = 0.0;
        world.fresh = false;
        return;
    }
    let inline = !threaded || !world.presented;
    let due = inline || play.tick_debt >= TICK_PERIOD;
    let (mut job, mut sent) = (None, [0.0; 2]);
    if due {
        play.tick_debt = (play.tick_debt - TICK_PERIOD).max(0.0);
        let waited = std::mem::take(&mut play.waited);
        let step = if play.smooth_step { play.clock.step(waited) } else { waited.min(MAX_STEP) };
        sent = play.backlog.mouse;
        job = Some(next_job(world, drawn, &input, &mut play.backlog, &times, step, canvas));
    }
    if inline
        && world.frame.travel.is_none()
        && let Some(job) = job.take()
    {
        world.start(job, JobThread::Main);
    }
    let new = world.present();
    world.fresh = new;
    if world.frame.travel.is_none()
        && let Some(job) = job
    {
        world.start(job, JobThread::Worker);
    }
    // A job run here is the frame presented: its view has the mouse just sent.
    if inline {
        play.look.in_flight = sent;
    }
    if new && let Some(view) = world.frame.view {
        play.look.learn(view.rotation, view.actor);
    }
    if due {
        play.look.in_flight = if inline { [0.0; 2] } else { sent };
    } else if new {
        play.look.in_flight = [0.0; 2];
    }
    if !new {
        return;
    }
    times.add_vm(&mut world.frame.times, &mut world.frame.profile);
    world.seconds += world.frame.step;
    world.frame.step = std::mem::take(&mut play.unstepped).min(MAX_STEP);
}

fn look_ahead(play: Res<Play>, input: Res<PlayerInput>, mut play_cameras: PlayCameras) {
    if let Some(world) = play.world.as_ref() {
        play_cameras.look(world, &play.look, play.backlog.mouse, input.playing);
    }
}

fn next_job(world: &mut PlayWorld, drawn: DrawnActors, input: &PlayerInput, backlog: &mut InputBacklog, times: &FrameTimes, step: f32, canvas: [f32; 2]) -> Job {
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
        mouse: std::mem::take(&mut backlog.mouse),
        typed: std::mem::take(&mut backlog.typed),
        key_events: std::mem::take(&mut backlog.key_events),
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
        if !(follow.is_added() || world.changed(follow.actor) || world.glides(follow.actor)) {
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
        let (location, rotation) = if canvas_drawn { (actor.location, actor.rotation) } else { world.shown_place(follow.actor, actor) };
        transform.set_if_neq(follow.transform(location, rotation));
    }
    times.add("follow actors", started);
}

fn publish_actor_frames(play: Res<Play>, mut lit: Query<(&crate::render::actor_light::ActorLit, &mut crate::render::actor_light::ActorFrame)>) {
    use crate::render::actor_light::{ActorFrame, ActorPlace};
    let world = play.world();
    for (lit, mut frame) in &mut lit {
        let actor = world.and_then(|world| world.actor(lit.actor));
        frame.set_if_neq(ActorFrame {
            place: actor.map(|actor| ActorPlace { location: actor.location, rotation: actor.rotation, glow: actor.glow }),
            base: actor.and_then(|actor| actor.attachment.as_ref().map(|attachment| attachment.base)),
        });
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
