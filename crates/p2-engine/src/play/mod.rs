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
use bevy::audio::{AudioPlayer, AudioSink, AudioSinkPlayback, AudioSource, PlaybackSettings, SpatialAudioSink, SpatialListener, SpatialScale, Volume};
use bevy::camera::Camera;
use bevy::camera::visibility::{Visibility, VisibleEntities};
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut};
use bevy::ecs::prelude::{ChildOf, Commands, Component, Entity, Has, Local, MessageReader, Query, Res, ResMut, Resource, SystemSet, With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::SystemParam;
use bevy::math::Vec3;
use bevy::mesh::Mesh3d;
use bevy::time::Time;
use bevy::transform::TransformSystems;
use bevy::transform::components::Transform;
use p2_assets::MeshAnimation;
use p2_bevy_assets::to_bevy;
use p2_package::Loader;
use p2_particles::{ActorForce, EmitterProperties};
use p2_vm::{CanvasItem, HostCommand, Kind, MusicCommand, ObjectId, ParticleCommand, PlayedSound, ProjectorEvent, Prop, Vm};
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

pub use times::*;
pub use vm_thread::*;
pub use world::*;

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
    let threads = (bevy::tasks::available_parallelism() / 2).max(1);
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

#[derive(Component)]
struct Song(u64);

#[derive(Component)]
pub(crate) struct MovieSound(pub(crate) ObjectId);

fn pause_movie_sound(play: Res<Play>, sinks: Query<(&MovieSound, &AudioSink)>) {
    let Some(world) = play.world.as_ref() else { return };
    for (sound, sink) in &sinks {
        let script_paused = world.movies().iter().any(|(_, movie, state)| *movie == sound.0 && state.paused);
        if world.game_paused() || script_paused { sink.pause() } else { sink.play() }
    }
}

/// The quietest give way when more than `Channels` play. One on an actor's ambient slot stops when its
/// actor goes; any other plays on where it was.
#[derive(Component)]
struct Channel {
    loudness: f32,
    looping: bool,
    generation: u64,
    actor: Option<ObjectId>,
}

const SLOT_AMBIENT: u8 = 4;

#[derive(Component)]
struct BaseVolume(f32);

#[derive(Component)]
struct Fade {
    from: f32,
    to: f32,
    seconds: f32,
    elapsed: f32,
    despawn: bool,
}

impl Fade {
    fn new(from: f32, to: f32, seconds: f32, despawn: bool) -> Self {
        Self { from, to, seconds, elapsed: 0.0, despawn }
    }
}

fn fade_sounds(mut commands: Commands, time: Res<Time>, mut fades: Query<(Entity, &mut Fade, Option<&BaseVolume>, Option<&mut AudioSink>)>, mut heard: Query<&mut HeardSound>) {
    for (entity, mut fade, base, sink) in &mut fades {
        fade.elapsed += time.delta_secs();
        let t = if fade.seconds > 0.0 { (fade.elapsed / fade.seconds).min(1.0) } else { 1.0 };
        let level = fade.from + (fade.to - fade.from) * t;
        if let (Some(base), Some(mut sink)) = (base, sink) {
            sink.set_volume(Volume::Linear(base.0 * level));
        }
        if let Ok(mut heard) = heard.get_mut(entity) {
            heard.fade = level;
        }
        if t >= 1.0 {
            if fade.despawn {
                commands.entity(entity).despawn();
            } else {
                commands.entity(entity).remove::<Fade>();
            }
        }
    }
}

fn sound_limits(
    mut commands: Commands,
    mut play: ResMut<Play>,
    channels: Query<(Entity, &Channel)>,
    mut loops: Query<&mut AudioSink, With<Song>>,
    mut spatial_loops: Query<&mut SpatialAudioSink, With<Song>>,
    mut playing: Local<Vec<(Entity, f32)>>,
) {
    let Some(world) = play.world.as_mut() else { return };
    let flashbang = world.flashbang_playing();
    if flashbang != world.flashbang_on {
        world.flashbang_on = flashbang;
        for sink in &mut loops {
            if flashbang { sink.pause() } else { sink.play() }
        }
        for sink in &mut spatial_loops {
            if flashbang { sink.pause() } else { sink.play() }
        }
        if flashbang {
            for (entity, ..) in &channels {
                commands.entity(entity).despawn();
            }
            return;
        }
    }
    playing.clear();
    for (entity, channel) in &channels {
        if channel.actor.is_some_and(|actor| world.is_gone(actor)) {
            commands.entity(entity).despawn();
        } else {
            playing.push((entity, channel.loudness));
        }
    }
    let limit = world.frame.audio_channels;
    if limit > 0 && playing.len() > limit {
        playing.sort_by(|a, b| a.1.total_cmp(&b.1));
        for &(entity, _) in &playing[..playing.len() - limit] {
            commands.entity(entity).despawn();
        }
    }
}

/// Spatial sounds only pan: positions shrink until every distance is under one unit, where rodio's own
/// distance fade is 1, so the gain stays [`distance_gain`]'s.
const PAN_SCALE: f32 = 1e-6;

fn pitch_factor(pitch: f32) -> f32 {
    pitch.clamp(0.5, 2.0)
}

fn spatial(settings: PlaybackSettings) -> PlaybackSettings {
    PlaybackSettings { spatial: true, spatial_scale: Some(SpatialScale(Vec3::splat(PAN_SCALE))), ..settings }
}

#[derive(Component)]
struct HeardSound {
    actor: Option<ObjectId>,
    position: Vec3,
    volume: f32,
    radius: f32,
    fade: f32,
}

fn heard_volume(
    play: Res<Play>,
    cameras: Query<&Transform, (With<SceneView>, Without<HeardSound>)>,
    mut sounds: Query<(&mut HeardSound, &mut Transform, &mut SpatialAudioSink)>,
) {
    let (Some(world), Some(camera)) = (&play.world, cameras.iter().next()) else { return };
    for (mut heard, mut transform, mut sink) in &mut sounds {
        if let Some(actor) = heard.actor.and_then(|actor| world.actor(actor)) {
            heard.position = world.scene_position(actor.location);
        }
        if transform.translation != heard.position {
            transform.translation = heard.position;
        }
        sink.set_volume(Volume::Linear(heard.volume * heard.fade * distance_gain(camera.translation.distance(heard.position), heard.radius)));
    }
}

fn stop_old_songs(mut commands: Commands, play: Res<Play>, songs: Query<(Entity, &Song)>, channels: Query<(Entity, &Channel)>) {
    for (entity, song) in &songs {
        if song.0 != play.generation {
            commands.entity(entity).despawn();
        }
    }
    for (entity, channel) in &channels {
        if channel.looping && channel.generation != play.generation {
            commands.entity(entity).despawn();
        }
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
        world.start(job, false);
    }
    let new = world.present();
    if world.frame.travel.is_none()
        && let Some(job) = job
    {
        world.start(job, true);
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

fn play_music(commands: &mut Commands, world: &mut PlayWorld, sources: &mut Assets<AudioSource>, camera: Option<Vec3>, generation: u64) {
    for (command, position, song) in std::mem::take(&mut world.frame.music) {
        match command {
            MusicCommand::Play { handle, actor, volume, radius, pitch, fade_in, .. } => {
                let Some(bytes) = song else { continue };
                let place = position.map(|position| world.scene_position(position));
                let gain = match (camera, place) {
                    (Some(camera), Some(place)) if radius > 0.0 => distance_gain(camera.distance(place), radius),
                    _ => 1.0,
                };
                let source = sources.add(AudioSource { bytes });
                let settings = PlaybackSettings::LOOP.with_volume(Volume::Linear(volume * gain)).with_speed(pitch_factor(pitch));
                let entity = match place.filter(|_| radius > 0.0) {
                    Some(position) => {
                        let fading_in = fade_in > 0.0;
                        let heard = HeardSound { actor, position, volume, radius, fade: if fading_in { 0.0 } else { 1.0 } };
                        let settings = if fading_in { settings.with_volume(Volume::SILENT) } else { settings };
                        let entity = commands.spawn((AudioPlayer::new(source), spatial(settings), Song(generation), heard, Transform::from_translation(position))).id();
                        if fading_in {
                            commands.entity(entity).insert(Fade::new(0.0, 1.0, fade_in, false));
                        }
                        entity
                    }
                    None if fade_in > 0.0 => {
                        let faded = (BaseVolume(volume * gain), Fade::new(0.0, 1.0, fade_in, false));
                        commands.spawn((AudioPlayer::new(source), settings.with_volume(Volume::SILENT), Song(generation), faded)).id()
                    }
                    None => commands.spawn((AudioPlayer::new(source), settings, Song(generation), BaseVolume(volume * gain))).id(),
                };
                world.songs.insert(handle, entity);
            }
            MusicCommand::StopAll { fade_out } => {
                for (_, entity) in world.songs.drain() {
                    commands.entity(entity).insert(Fade::new(1.0, 0.0, fade_out, true));
                }
            }
            MusicCommand::Stop { handle, fade_out } => {
                if let Some(entity) = world.songs.remove(&handle) {
                    commands.entity(entity).insert(Fade::new(1.0, 0.0, fade_out, true));
                }
            }
        }
    }
}

fn play_world_sounds(commands: &mut Commands, world: &mut PlayWorld, sources: &mut Assets<AudioSource>, camera: Option<Vec3>, generation: u64) {
    for (path, file) in std::mem::take(&mut world.frame.sound_files) {
        world.add_sound_file(path, file, sources);
    }
    play_sounds(commands, world, camera, generation);
    play_ambients(commands, world, generation);
    play_zone_sounds(commands, world, sources, generation);
}

fn play_ambients(commands: &mut Commands, world: &mut PlayWorld, generation: u64) {
    let mut ambients = std::mem::take(&mut world.frame.ambients);
    let mut heard_now = std::mem::take(&mut world.heard_now);
    heard_now.clear();
    for (actor, ambient, path) in ambients.drain(..) {
        heard_now.insert(actor);
        let position = world.scene_position(ambient.location);
        let radius = if ambient.radius > 0.0 { ambient.radius } else { p2_vm::AUDIO_DEFAULT_RADIUS };
        let heard = HeardSound { actor: Some(actor), position, volume: ambient.volume, radius, fade: 1.0 };
        let said = (ambient.location, ambient.volume, radius);
        if let Some(playing) = world.ambients.get_mut(&actor)
            && playing.sound == ambient.sound
        {
            if playing.said != said {
                playing.said = said;
                commands.entity(playing.entity).insert(heard);
            }
            continue;
        }
        if let Some(old) = world.ambients.remove(&actor) {
            commands.entity(old.entity).despawn();
        }
        let Some(source) = world.sound(&path).map(|loaded| loaded.source) else { continue };
        let settings = spatial(PlaybackSettings::LOOP.with_volume(Volume::SILENT).with_speed(pitch_factor(ambient.pitch)));
        let entity = commands.spawn((AudioPlayer::new(source), settings, heard, Song(generation), Transform::from_translation(position))).id();
        world.ambients.insert(actor, PlayingAmbient { sound: ambient.sound, entity, said });
    }
    world.ambients.retain(|actor, playing| {
        let keep = heard_now.contains(actor);
        if !keep {
            commands.entity(playing.entity).despawn();
        }
        keep
    });
    world.frame.ambients = ambients;
    world.heard_now = heard_now;
}

fn play_zone_sounds(commands: &mut Commands, world: &mut PlayWorld, sources: &mut Assets<AudioSource>, generation: u64) {
    let Some(heard) = world.frame.zone_sounds.take() else { return };
    let keys: FxHashSet<(ObjectId, usize)> = heard.iter().map(|zone_sound| zone_sound.key).collect();
    world.zone_sounds.retain(|key, &mut (entity, fade_out)| {
        let keep = keys.contains(key);
        if !keep {
            commands.entity(entity).insert(Fade::new(1.0, 0.0, fade_out, true));
        }
        keep
    });
    for zone_sound in heard {
        if world.zone_sounds.contains_key(&zone_sound.key) {
            continue;
        }
        let source = match (zone_sound.song, &zone_sound.sound) {
            (Some(bytes), _) => sources.add(AudioSource { bytes }),
            (None, Some(path)) => {
                let Some(loaded) = world.sound(path) else { continue };
                loaded.source
            }
            (None, None) => continue,
        };
        let (fade_in, fade_out) = (zone_sound.fade_in, zone_sound.fade_out);
        let settings = PlaybackSettings::LOOP.with_volume(Volume::SILENT).with_speed(zone_sound.pitch);
        let faded = (BaseVolume(zone_sound.volume), Fade::new(0.0, 1.0, fade_in, false));
        let entity = commands.spawn((AudioPlayer::new(source), settings, Song(generation), faded)).id();
        world.zone_sounds.insert(zone_sound.key, (entity, fade_out));
    }
}

fn play_sounds(commands: &mut Commands, world: &mut PlayWorld, camera: Option<Vec3>, generation: u64) {
    world.voices.retain(|_, &mut entity| commands.get_entity(entity).is_ok());
    for (played, position) in std::mem::take(&mut world.frame.sounds) {
        if played.flashbang.is_none() && world.flashbang_playing() {
            continue;
        }
        let Some(loaded) = world.sound(&played.sound) else { continue };
        let source = loaded.source;
        if let Some(duration) = played.flashbang {
            world.flashbang_until = Some(world.seconds + if duration > 0.0 { duration } else { loaded.duration });
            let settings = PlaybackSettings::DESPAWN.with_volume(Volume::Linear(played.volume)).with_speed(pitch_factor(played.pitch));
            commands.spawn((AudioPlayer::new(source), settings));
            continue;
        }
        // A sound on an actor's slot (not slot 0) replaces the one still playing there,
        // or with `bNoOverride` is dropped.
        let voice = played.actor.filter(|_| played.slot != 0).map(|actor| (actor, played.slot));
        if let Some(mut playing) = voice.and_then(|voice| world.voices.get(&voice)).and_then(|&entity| commands.get_entity(entity).ok()) {
            if played.no_override {
                continue;
            }
            playing.despawn();
        }
        let place = position.map(|position| world.scene_position(position));
        let gain = match (camera, place) {
            (Some(camera), Some(place)) if played.attenuate => distance_gain(camera.distance(place), played.radius),
            _ => 1.0,
        };
        let mode = if loaded.looping { PlaybackSettings::LOOP } else { PlaybackSettings::DESPAWN };
        let settings = mode.with_volume(Volume::Linear(played.volume * gain)).with_speed(pitch_factor(played.pitch));
        let channel = Channel { loudness: played.volume * gain, looping: loaded.looping, generation, actor: played.actor.filter(|_| played.slot == SLOT_AMBIENT) };
        let entity = match place.filter(|_| played.attenuate) {
            Some(position) => {
                let heard = HeardSound { actor: played.actor, position, volume: played.volume, radius: played.radius, fade: 1.0 };
                commands.spawn((AudioPlayer::new(source), spatial(settings), heard, channel, Transform::from_translation(position))).id()
            }
            None => commands.spawn((AudioPlayer::new(source), settings, channel)).id(),
        };
        if let Some(voice) = voice {
            world.voices.insert(voice, entity);
        }
    }
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

const ROLLOFF: f32 = 0.9;

/// Gain of a sound of `radius` heard at `distance`: OpenAL's inverse-distance-clamped model
/// with the radius as reference distance, as retail.
pub(crate) fn distance_gain(distance: f32, radius: f32) -> f32 {
    radius / (radius + ROLLOFF * (distance.max(radius) - radius))
}

pub(crate) fn ambient_gain(volume: f32, ambient_volume: f32, sound_volume: f32) -> f32 {
    (volume * ambient_volume * sound_volume).clamp(0.0, 1.0)
}

/// Retail hears the player's right (Unreal +Y, the camera's +X) in the right channel. rodio 0.22's
/// `Spatial` gives the louder channel to the ear FARTHER from the sound, so the ears are swapped.
pub(crate) fn sound_listener() -> SpatialListener {
    let ears = SpatialListener::default();
    SpatialListener { left_ear_offset: ears.right_ear_offset, right_ear_offset: ears.left_ear_offset }
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
mod tests {
    use bevy::app::{Last, TaskPoolPlugin};
    use bevy::asset::{AssetApp, AssetEvent, AssetPlugin, RenderAssetUsages};
    use bevy::ecs::prelude::{MessageReader, World};
    use bevy::mesh::{Indices, Mesh, PrimitiveTopology};

    use super::*;

    #[test]
    fn gain_is_full_inside_the_radius_then_inverse() {
        assert_eq!(distance_gain(0.0, 200.0), 1.0);
        assert_eq!(distance_gain(200.0, 200.0), 1.0);
        assert!((distance_gain(400.0, 200.0) - 1.0 / 1.9).abs() < 1e-6);
    }

    #[test]
    fn ambient_gain_takes_the_sound_volume_setting_too() {
        assert!((ambient_gain(1.0, 1.0, 0.1) - 0.1).abs() < 1e-6);
        assert_eq!(ambient_gain(1.0, 2.0, 1.0), 1.0);
    }

    #[test]
    fn the_left_channel_ear_sits_on_the_cameras_right() {
        let ears = sound_listener();
        assert_eq!(ears.left_ear_offset, Vec3::X * 2.0);
        assert_eq!(ears.right_ear_offset, Vec3::X * -2.0);
    }

    #[test]
    fn only_the_cameras_draw_never_the_sun_alone() {
        let e = |n| Entity::from_raw_u32(n).unwrap();
        let (mut camera, overlay) = ([e(1), e(3), e(5)], [e(7)]);
        camera.sort_unstable();
        let seen: [&[Entity]; 2] = [&camera, &overlay];
        let shadow_only = HashSet::from([e(20)]);
        assert!(camera_drew(&seen, &shadow_only, e(3), Some(e(10))));
        assert!(camera_drew(&seen, &shadow_only, e(7), None));
        assert!(!camera_drew(&seen, &shadow_only, e(4), Some(e(10))));
        assert!(!camera_drew(&seen, &shadow_only, e(5), Some(e(20))));
        assert!(!camera_drew(&[], &HashSet::new(), e(1), None));
    }

    #[derive(Resource, Default)]
    struct Seen(Vec<usize>);

    #[derive(Resource)]
    struct Particles(Handle<Mesh>);

    fn mesh_changes_per_frame(mut app: App) -> Vec<usize> {
        app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default())).init_asset::<Mesh>().init_resource::<Seen>();
        let mesh = app.world_mut().resource_mut::<Assets<Mesh>>().add(Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default()));
        app.insert_resource(Particles(mesh));
        let rewrite = |world: &mut World| {
            let handle = world.resource::<Particles>().0.clone();
            if let Some(mut mesh) = world.resource_mut::<Assets<Mesh>>().get_mut(&handle) {
                mesh.insert_indices(Indices::U32(vec![0, 1, 2]));
            }
        };
        app.add_systems(PostUpdate, rewrite.in_set(PlaySystems::Present));
        app.add_systems(Last, |mut events: MessageReader<AssetEvent<Mesh>>, mut seen: ResMut<Seen>| {
            seen.0.push(events.read().filter(|event| matches!(event, AssetEvent::Modified { .. })).count());
        });
        app.finish();
        app.cleanup();
        for _ in 0..10 {
            app.world_mut().run_schedule(PostUpdate);
            app.world_mut().run_schedule(Last);
        }
        std::mem::take(&mut app.world_mut().resource_mut::<Seen>().0)
    }

    #[test]
    fn weapon_moved_in_the_pass_is_drawn_there_that_frame() {
        use bevy::transform::components::GlobalTransform;
        let mut app = App::new();
        app.add_plugins((TaskPoolPlugin::default(), bevy::transform::TransformPlugin, PlayPlugin));
        let weapon = app.world_mut().spawn(Transform::default()).id();
        app.add_systems(PostUpdate, (move |mut moved: Query<&mut Transform>| {
            if let Ok(mut transform) = moved.get_mut(weapon) {
                transform.translation.x += 1.0;
            }
        })
        .in_set(PlaySystems::Present));
        app.finish();
        app.cleanup();
        for frame in 1..=5 {
            app.world_mut().run_schedule(PostUpdate);
            let global = app.world().get::<GlobalTransform>(weapon).map(|g| g.translation().x);
            assert_eq!(global, Some(frame as f32));
        }
    }

    #[test]
    fn particle_meshes_reach_the_renderer_in_the_frame_they_change() {
        let mut app = App::new();
        app.add_plugins(PlayPlugin);
        assert_eq!(mesh_changes_per_frame(app), vec![1; 10]);
        let mut app = App::new();
        app.configure_sets(PostUpdate, PlaySystems::Present.after(AssetEventSystems));
        assert_eq!(mesh_changes_per_frame(app)[0], 0);
    }

    #[test]
    fn steps_are_even_over_alternating_frame_times() {
        let mut clock = StepClock::default();
        let (short, long) = (3.0 / 240.0, 4.0 / 240.0);
        let steps: Vec<f32> = (0..60).map(|i| clock.step(if i % 2 == 0 { short } else { long })).collect();
        for &step in &steps[STEP_FRAMES..] {
            assert!((step - (short + long) / 2.0).abs() < 1e-6, "{step}");
        }
        let mut clock = StepClock::default();
        let (mut real, mut game) = (0.0f32, 0.0f32);
        for i in 0..1000 {
            let delta = 0.010 + 0.004 * ((i * 7919) % 13) as f32 / 12.0;
            real += delta;
            game += clock.step(delta);
            assert!((real - game).abs() < STEP_FRAMES as f32 * 0.014, "frame {i}: real {real}, game {game}");
        }
        assert_eq!(StepClock::default().step(1.0), MAX_STEP);
    }

    #[test]
    fn the_vm_can_move_to_its_thread() {
        fn send<T: Send>() {}
        send::<Simulation>();
        send::<Job>();
        send::<VmFrame>();
    }
}
