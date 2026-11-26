use super::*;
use bevy::audio::AddAudioSource;
use p2_render::level_collision::{LevelCollision, StaticCollision};
use p2_vm::ESoundOcclusion;

mod occlusion;
mod panned;
use occlusion::{Muffling, ZoneSteps};
use panned::{Pan, PannedSound};

pub struct SoundPlugin;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum SoundSystems {
    Stop,
    Start,
    Mix,
}

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SoundBook>()
            .init_resource::<Assets<PannedSound>>()
            .init_resource::<ZoneSteps>()
            .configure_sets(Update, (SoundSystems::Stop, SoundSystems::Start, SoundSystems::Mix).chain().in_set(PlaySystems::Tick))
            .add_systems(Startup, watch_output_device)
            .add_systems(Update, watch_audio)
            .add_systems(Update, stop_old_songs.in_set(SoundSystems::Stop))
            .add_systems(Update, start_sounds.in_set(SoundSystems::Start).after(hand_off).before(look_ahead))
            .add_systems(Update, (heard_volume, sound_limits, fade_sounds, pause_movie_sound, pause_with_game).chain().in_set(SoundSystems::Mix));
    }

    fn finish(&self, app: &mut App) {
        if app.is_plugin_added::<bevy::audio::AudioPlugin>() {
            app.add_audio_source::<PannedSound>();
        }
    }
}

struct PlayingZoneSound {
    entity: Entity,
    volume: f32,
    fade_out: f32,
}

struct PlayingAmbient {
    sound: ObjectId,
    entity: Entity,
    said: (p2_math::Vector, f32, f32, f32, bool),
    pan: Pan,
}

#[derive(Resource, Default)]
pub(crate) struct SoundBook {
    generation: u64,
    songs: HashMap<i32, Entity>,
    zone_sounds: FxHashMap<(ObjectId, usize), PlayingZoneSound>,
    ambients: FxHashMap<ObjectId, PlayingAmbient>,
    heard_now: FxHashSet<ObjectId>,
    voices: FxHashMap<(ObjectId, u8), Entity>,
    sounds: HashMap<String, Option<LoadedSound>>,
    flashbang_until: Option<f32>,
    flashbang_on: bool,
    weakest_kept: Option<f32>,
}

impl SoundBook {
    fn add_sound_file(&mut self, path: String, file: Option<p2_assets::Sound>, sources: &mut Assets<AudioSource>) {
        if file.is_none() {
            bevy::log::warn!("sound {path}: not found in a loaded package or its package file");
        }
        let loaded = file.and_then(|sound| loaded_sound(&path, sound, sources));
        self.sounds.entry(path).or_insert(loaded);
    }

    fn sound(&self, path: &str) -> Option<LoadedSound> {
        self.sounds.get(path).cloned().flatten()
    }

    fn flashbang_playing(&self, seconds: f32) -> bool {
        self.flashbang_until.is_some_and(|until| seconds < until)
    }
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn start_sounds(
    mut commands: Commands,
    mut play: ResMut<Play>,
    mut book: ResMut<SoundBook>,
    mut sources: ResMut<Assets<AudioSource>>,
    mut panned: ResMut<Assets<PannedSound>>,
    collision: Option<Res<LevelCollision>>,
    mut steps: ResMut<ZoneSteps>,
    cameras: Query<&Transform, With<SceneView>>,
) {
    let generation = play.generation;
    let eye = cameras.iter().next().copied().unwrap_or_default();
    if book.generation != generation {
        *book = SoundBook { generation, ..Default::default() };
    }
    let Some(world) = play.world.as_mut().filter(|world| world.fresh) else { return };
    let camera = cameras.iter().next().map(|camera| camera.translation);
    let level = collision.as_ref().and_then(|collision| collision.0.as_deref()).zip(world.view_location());
    let mut hearing = Hearing { camera, eye, level, steps: &mut steps, generation, panned: &mut panned };
    apply_music_commands(&mut commands, world, &mut book, &mut sources, &mut hearing);
    play_world_sounds(&mut commands, world, &mut book, &mut sources, hearing);
}

struct Hearing<'a> {
    camera: Option<Vec3>,
    eye: Transform,
    level: Option<(&'a StaticCollision, p2_math::Vector)>,
    steps: &'a mut ZoneSteps,
    generation: u64,
    panned: &'a mut Assets<PannedSound>,
}

impl Hearing<'_> {
    fn pan(&self, at: Option<Vec3>) -> Pan {
        let pan = Pan::default();
        if let (Some(at), Some(_)) = (at, self.camera) {
            pan.set(side_of(&self.eye, at));
        }
        pan
    }

    fn panned(&mut self, bytes: &Arc<[u8]>, pan: &Pan) -> AudioPlayer<PannedSound> {
        AudioPlayer(self.panned.add(PannedSound { bytes: Arc::clone(bytes), pan: pan.clone() }))
    }
}

#[derive(Component)]
pub(crate) struct Song(u64);

#[derive(Component)]
pub(crate) struct MovieSound(pub(crate) ObjectId);

fn pause_movie_sound(play: Res<Play>, sinks: Query<(&MovieSound, &AudioSink)>) {
    let Some(world) = play.world.as_ref() else { return };
    for (sound, sink) in &sinks {
        let script_paused = world.movies().iter().any(|(_, movie, state)| *movie == sound.0 && state.paused);
        if world.game_paused() || script_paused { sink.pause() } else { sink.play() }
    }
}

/// Paused while the game is: retail pauses every source started with `bAllowPause` when the game pauses and
/// resumes them after. A sound started during the pause plays.
#[derive(Component)]
pub(crate) struct Pausable;

#[derive(Component)]
pub(crate) struct PausedWithGame;

fn pause_with_game(
    mut commands: Commands,
    play: Res<Play>,
    mut was_paused: Local<bool>,
    sinks: Query<(Entity, &AudioSink, Has<PausedWithGame>), With<Pausable>>,
) {
    let paused = play.world.as_ref().is_some_and(PlayWorld::game_paused);
    if paused == *was_paused {
        return;
    }
    *was_paused = paused;
    for (entity, sink, held) in &sinks {
        if paused && !sink.is_paused() {
            sink.pause();
            commands.entity(entity).try_insert(PausedWithGame);
        } else if !paused && held {
            sink.play();
            commands.entity(entity).try_remove::<PausedWithGame>();
        }
    }
}

#[derive(Component)]
pub(crate) struct Channel {
    priority: f32,
    looping: bool,
    generation: u64,
    actor: Option<ObjectId>,
}

const SLOT_AMBIENT: u8 = 4;

#[derive(Component)]
pub(crate) struct BaseVolume(f32);

#[derive(Component)]
pub(crate) struct Fade {
    from: f32,
    to: f32,
    seconds: f32,
    elapsed: f32,
    end: FadeEnd,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FadeEnd {
    Keep,
    Despawn,
}

impl Fade {
    fn new(from: f32, to: f32, seconds: f32, end: FadeEnd) -> Self {
        Self { from, to, seconds, elapsed: 0.0, end }
    }

    fn progress(&self) -> f32 {
        if self.seconds > 0.0 { (self.elapsed / self.seconds).min(1.0) } else { 1.0 }
    }

    fn level(&self) -> f32 {
        self.from + (self.to - self.from) * self.progress()
    }
}

fn stop_fading(commands: &mut Commands, entity: Entity, seconds: f32) {
    commands.entity(entity).queue(move |mut entity: EntityWorldMut| {
        let from = entity.get::<Fade>().map_or(1.0, Fade::level);
        entity.insert(Fade::new(from, 0.0, seconds, FadeEnd::Despawn));
    });
}

fn fade_to(commands: &mut Commands, entity: Entity, volume: f32, seconds: f32) {
    commands.entity(entity).queue(move |mut entity: EntityWorldMut| {
        let base = entity.get::<BaseVolume>().map_or(1.0, |base| base.0);
        let now = base * entity.get::<Fade>().map_or(1.0, Fade::level);
        if volume > 0.0 {
            entity.insert((BaseVolume(volume), Fade::new(now / volume, 1.0, seconds, FadeEnd::Keep)));
        } else {
            entity.insert((BaseVolume(now), Fade::new(1.0, 0.0, seconds, FadeEnd::Keep)));
        }
    });
}

fn fade_sounds(mut commands: Commands, time: Res<Time>, mut fades: Query<(Entity, &mut Fade, Option<&BaseVolume>, Option<&mut AudioSink>)>, mut heard: Query<&mut HeardSound>) {
    for (entity, mut fade, base, sink) in &mut fades {
        if base.is_some() && sink.is_none() {
            continue;
        }
        fade.elapsed += time.delta_secs();
        let level = fade.level();
        if let (Some(base), Some(mut sink)) = (base, sink) {
            sink.set_volume(Volume::Linear(base.0 * level));
        }
        if let Ok(mut heard) = heard.get_mut(entity) {
            heard.fade = level;
        }
        if fade.progress() >= 1.0 {
            if fade.end == FadeEnd::Despawn {
                commands.entity(entity).despawn();
            } else {
                commands.entity(entity).remove::<Fade>();
            }
        }
    }
}

fn sound_limits(
    mut commands: Commands,
    play: Res<Play>,
    mut book: ResMut<SoundBook>,
    channels: Query<(Entity, &Channel, Has<Song>)>,
    mut loops: Query<&mut AudioSink, (With<Song>, Without<PausedWithGame>)>,
    mut playing: Local<Vec<(Entity, f32)>>,
) {
    let Some(world) = play.world.as_ref() else { return };
    let flashbang = book.flashbang_playing(world.seconds);
    if flashbang != book.flashbang_on {
        book.flashbang_on = flashbang;
        for sink in &mut loops {
            if flashbang { sink.pause() } else { sink.play() }
        }
        if flashbang {
            for (entity, ..) in channels.iter().filter(|(.., ambient)| !ambient) {
                commands.entity(entity).despawn();
            }
            return;
        }
    }
    playing.clear();
    for (entity, channel, _) in &channels {
        if channel.actor.is_some_and(|actor| world.is_gone(actor)) {
            commands.entity(entity).despawn();
        } else {
            playing.push((entity, channel.priority));
        }
    }
    let limit = world.frame.audio_channels;
    book.weakest_kept = None;
    if limit > 0 && playing.len() >= limit {
        playing.sort_by(|a, b| a.1.total_cmp(&b.1));
        let dropped = playing.len() - limit;
        for &(entity, _) in &playing[..dropped] {
            commands.entity(entity).despawn();
        }
        book.weakest_kept = Some(playing[dropped].1);
    }
}

pub(super) fn pitch_factor(pitch: f32) -> f32 {
    pitch.clamp(0.5, 2.0)
}

fn listener_gain(camera: Option<Vec3>, heard_at: Option<Vec3>, radius: f32) -> f32 {
    camera.zip(heard_at).map_or(1.0, |(camera, place)| distance_gain(camera.distance(place), radius))
}

#[derive(Component)]
pub(crate) struct HeardSound {
    actor: Option<ObjectId>,
    position: Vec3,
    volume: f32,
    radius: f32,
    fade: f32,
    speed: f32,
    centred: bool,
    stream: StreamFade,
    pan: Pan,
}

#[derive(Clone, Copy, PartialEq)]
enum StreamFade {
    No,
    Legacy,
    FromVolume,
}

impl StreamFade {
    fn factor(self, volume: f32, distance: f32, radius: f32) -> f32 {
        let past = (distance - radius) / ((AUDIO_MAX_RADIUS_MULTIPLIER - 1.0) * radius);
        match self {
            Self::No => 1.0,
            Self::Legacy => (1.0 - past.max(0.0)).clamp(0.0, 1.0),
            Self::FromVolume => (volume - past.max(0.0)).clamp(0.0, 1.0),
        }
    }
}

fn heard_volume(
    time: Res<Time>,
    play: Res<Play>,
    collision: Option<Res<LevelCollision>>,
    cameras: Query<&Transform, (With<SceneView>, Without<HeardSound>)>,
    mut sounds: Query<(&mut HeardSound, &mut AudioSink, Option<&mut Muffling>)>,
    mut steps: ResMut<ZoneSteps>,
) {
    let (Some(world), Some(camera)) = (&play.world, cameras.iter().next()) else { return };
    let view = world.view_location();
    let listener_velocity = world.view_target_velocity();
    let level = collision.as_ref().and_then(|collision| collision.0.as_deref()).zip(view);
    for (mut heard, mut sink, muffling) in &mut sounds {
        if heard.centred {
            heard.position = camera.translation;
        } else if let Some((location, _)) = heard.actor.and_then(|actor| world.place(actor)) {
            heard.position = world.scene_position(location);
        }
        heard.pan.set(side_of(camera, heard.position));
        let mut radius = heard.radius;
        if let (Some(mut muffling), Some((level, listener))) = (muffling, level) {
            radius = muffling.heard_radius(level, &mut steps, play.generation, world.level_position(heard.position), listener, radius, time.delta_secs());
        }
        let mut speed = heard.speed;
        if let (false, Some(listener)) = (heard.centred, view) {
            let source = heard.actor.and_then(|actor| world.actor(actor));
            let velocity = source.map_or(p2_math::Vector::default(), |source| source.velocity) - listener_velocity;
            speed *= doppler_shift(velocity, listener - world.level_position(heard.position));
        }
        if sink.speed() != speed {
            sink.set_speed(speed);
        }
        let distance = camera.translation.distance(heard.position);
        let gain = if heard.centred { 1.0 } else { distance_gain(distance, radius) * heard.stream.factor(heard.volume, distance, radius) };
        sink.set_volume(Volume::Linear(heard.volume * heard.fade * gain));
    }
}

fn stop_old_songs(mut commands: Commands, play: Res<Play>, songs: Query<(Entity, &Song)>, channels: Query<(Entity, &Channel), Without<Song>>) {
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

fn apply_music_commands(commands: &mut Commands, world: &mut PlayWorld, book: &mut SoundBook, sources: &mut Assets<AudioSource>, hearing: &mut Hearing) {
    let (camera, generation) = (hearing.camera, hearing.generation);
    for (command, position, song) in std::mem::take(&mut world.frame.music) {
        match command {
            MusicCommand::Play { handle, actor, volume, radius, pitch, fade_in, legacy, .. } => {
                let Some(bytes) = song else { continue };
                let heard_at = position.map(|position| world.scene_position(position)).filter(|_| radius > 0.0);
                let gain = listener_gain(camera, heard_at, radius);
                let settings = PlaybackSettings::LOOP.with_volume(Volume::Linear(volume * gain)).with_speed(pitch_factor(pitch));
                let entity = match heard_at {
                    Some(position) => {
                        let fading_in = fade_in > 0.0;
                        let stream = if legacy { StreamFade::Legacy } else { StreamFade::FromVolume };
                        let pan = Pan::default();
                        let player = hearing.panned(&bytes, &pan);
                        let heard = HeardSound { actor, position, volume, radius, fade: if fading_in { 0.0 } else { 1.0 }, speed: pitch_factor(pitch), centred: false, stream, pan };
                        let settings = if fading_in { settings.with_volume(Volume::SILENT) } else { settings };
                        let entity = commands.spawn((player, settings, Song(generation), heard)).id();
                        if fading_in {
                            commands.entity(entity).insert(Fade::new(0.0, 1.0, fade_in, FadeEnd::Keep));
                        }
                        entity
                    }
                    None if fade_in > 0.0 => {
                        let faded = (BaseVolume(volume * gain), Fade::new(0.0, 1.0, fade_in, FadeEnd::Keep));
                        commands.spawn((AudioPlayer::new(sources.add(AudioSource { bytes })), settings.with_volume(Volume::SILENT), Song(generation), faded)).id()
                    }
                    None => commands.spawn((AudioPlayer::new(sources.add(AudioSource { bytes })), settings, Song(generation), BaseVolume(volume * gain))).id(),
                };
                book.songs.insert(handle, entity);
            }
            MusicCommand::StopAll { fade_out } => {
                for (_, entity) in book.songs.drain() {
                    stop_fading(commands, entity, fade_out);
                }
            }
            MusicCommand::Stop { handle, fade_out } => {
                if let Some(entity) = book.songs.remove(&handle) {
                    stop_fading(commands, entity, fade_out);
                }
            }
        }
    }
}

fn play_world_sounds(commands: &mut Commands, world: &mut PlayWorld, book: &mut SoundBook, sources: &mut Assets<AudioSource>, mut hearing: Hearing) {
    for (path, file) in std::mem::take(&mut world.frame.sound_files) {
        book.add_sound_file(path, file, sources);
    }
    play_sounds(commands, world, book, &mut hearing);
    play_ambients(commands, world, book, &mut hearing);
    play_zone_sounds(commands, world, book, sources, hearing.generation);
}

fn play_ambients(commands: &mut Commands, world: &mut PlayWorld, book: &mut SoundBook, hearing: &mut Hearing) {
    let generation = hearing.generation;
    let mut ambients = std::mem::take(&mut world.frame.ambients);
    let mut heard_now = std::mem::take(&mut book.heard_now);
    heard_now.clear();
    book.ambients.retain(|_, playing| commands.get_entity(playing.entity).is_ok());
    let target = world.view_target_location();
    for (actor, ambient, path) in ambients.drain(..) {
        heard_now.insert(actor);
        let position = world.scene_position(ambient.location);
        let radius = if ambient.radius > 0.0 { ambient.radius } else { p2_vm::AUDIO_DEFAULT_RADIUS };
        let speed = pitch_factor(ambient.pitch);
        let pan = book.ambients.get(&actor).map_or_else(|| hearing.pan((!ambient.attached).then_some(position)), |playing| playing.pan.clone());
        let heard = HeardSound { actor: Some(actor), position, volume: ambient.volume, radius, fade: 1.0, speed, centred: ambient.attached, stream: StreamFade::No, pan: pan.clone() };
        let said = (ambient.location, ambient.volume, radius, speed, ambient.attached);
        let muffled = !ambient.attached && ambient.occlusion != ESoundOcclusion::None;
        let priority = sound_priority(ambient.volume, target.map(|target| (ambient.location - target).size()), radius, ambient.attached);
        let channel = Channel { priority, looping: true, generation, actor: None };
        if let Some(playing) = book.ambients.get_mut(&actor)
            && playing.sound == ambient.sound
        {
            commands.entity(playing.entity).try_insert(channel);
            if playing.said != said {
                let mut entity = commands.entity(playing.entity);
                entity.try_insert(heard);
                if playing.said.4 != ambient.attached {
                    entity.try_remove::<Muffling>();
                    if muffled {
                        entity.try_insert(Muffling::new(ambient.occlusion));
                    }
                }
                playing.said = said;
            }
            continue;
        }
        if let Some(old) = book.ambients.remove(&actor) {
            commands.entity(old.entity).despawn();
        }
        if book.weakest_kept.is_some_and(|weakest| priority <= weakest) {
            continue;
        }
        let Some(loaded) = book.sound(&path) else { continue };
        let settings = PlaybackSettings::LOOP.with_volume(Volume::SILENT).with_speed(speed);
        let entity = commands.spawn((hearing.panned(&loaded.bytes, &pan), settings, heard, channel, Song(generation), Pausable)).id();
        if muffled {
            commands.entity(entity).insert(Muffling::new(ambient.occlusion));
        }
        book.ambients.insert(actor, PlayingAmbient { sound: ambient.sound, entity, said, pan });
    }
    book.ambients.retain(|actor, playing| {
        let keep = heard_now.contains(actor);
        if !keep {
            commands.entity(playing.entity).despawn();
        }
        keep
    });
    world.frame.ambients = ambients;
    book.heard_now = heard_now;
}

fn play_zone_sounds(commands: &mut Commands, world: &mut PlayWorld, book: &mut SoundBook, sources: &mut Assets<AudioSource>, generation: u64) {
    let Some(heard) = world.frame.zone_sounds.take() else { return };
    let keys: FxHashSet<(ObjectId, usize)> = heard.iter().map(|zone_sound| zone_sound.key).collect();
    book.zone_sounds.retain(|key, playing| {
        let keep = keys.contains(key);
        if !keep {
            stop_fading(commands, playing.entity, playing.fade_out);
        }
        keep
    });
    for zone_sound in heard {
        if let Some(playing) = book.zone_sounds.get_mut(&zone_sound.key) {
            if playing.volume != zone_sound.volume {
                playing.volume = zone_sound.volume;
                fade_to(commands, playing.entity, zone_sound.volume, zone_sound.fade_in);
            }
            continue;
        }
        let pausable = zone_sound.song.is_none();
        let source = match (zone_sound.song, &zone_sound.sound) {
            (Some(bytes), _) => sources.add(AudioSource { bytes }),
            (None, Some(path)) => {
                let Some(loaded) = book.sound(path) else { continue };
                loaded.source
            }
            (None, None) => continue,
        };
        let (fade_in, fade_out) = (zone_sound.fade_in, zone_sound.fade_out);
        let settings = PlaybackSettings::LOOP.with_volume(Volume::SILENT).with_speed(zone_sound.pitch);
        let faded = (BaseVolume(zone_sound.volume), Fade::new(0.0, 1.0, fade_in, FadeEnd::Keep));
        let entity = commands.spawn((AudioPlayer::new(source), settings, Song(generation), faded)).id();
        if pausable {
            commands.entity(entity).insert(Pausable);
        }
        book.zone_sounds.insert(zone_sound.key, PlayingZoneSound { entity, volume: zone_sound.volume, fade_out });
    }
}

fn play_sounds(commands: &mut Commands, world: &mut PlayWorld, book: &mut SoundBook, hearing: &mut Hearing) {
    let (camera, generation) = (hearing.camera, hearing.generation);
    book.voices.retain(|_, &mut entity| commands.get_entity(entity).is_ok());
    for (played, position) in std::mem::take(&mut world.frame.sounds) {
        if played.flashbang.is_none() && book.flashbang_playing(world.seconds) {
            continue;
        }
        let Some(loaded) = book.sound(&played.sound) else { continue };
        if let Some(duration) = played.flashbang {
            book.flashbang_until = Some(world.seconds + if duration > 0.0 { duration } else { loaded.duration });
            let settings = PlaybackSettings::DESPAWN.with_volume(Volume::Linear(played.volume)).with_speed(pitch_factor(played.pitch));
            commands.spawn((hearing.panned(&loaded.bytes, &Pan::default()), settings));
            continue;
        }
        let voice = played.actor.filter(|_| played.slot != 0).map(|actor| (actor, played.slot));
        if let Some(mut playing) = voice.and_then(|voice| book.voices.get(&voice)).and_then(|&entity| commands.get_entity(entity).ok()) {
            if played.no_override {
                continue;
            }
            playing.despawn();
        }
        let heard_at = position.map(|position| world.scene_position(position)).filter(|_| played.attenuate);
        let mut muffling = Muffling::new(played.occlusion);
        let mut radius = played.radius;
        if let (Some(at), Some((level, listener))) = (position.filter(|_| played.attenuate && played.occlusion != ESoundOcclusion::None), hearing.level) {
            radius = muffling.heard_radius(level, hearing.steps, generation, at, listener, radius, 0.0);
        }
        let gain = listener_gain(camera, heard_at, radius);
        let mode = if loaded.looping { PlaybackSettings::LOOP } else { PlaybackSettings::DESPAWN };
        let settings = mode.with_volume(Volume::Linear(played.volume * gain)).with_speed(pitch_factor(played.pitch));
        let distance = position.zip(world.view_target_location()).map(|(at, target)| (at - target).size());
        let priority = sound_priority(played.volume, distance, radius, heard_at.is_none());
        let channel = Channel { priority, looping: loaded.looping, generation, actor: played.actor.filter(|_| played.slot == SLOT_AMBIENT) };
        let pan = hearing.pan(heard_at);
        let player = hearing.panned(&loaded.bytes, &pan);
        let entity = match heard_at {
            Some(position) => {
                let heard = HeardSound { actor: played.actor, position, volume: played.volume, radius: played.radius, fade: 1.0, speed: pitch_factor(played.pitch), centred: false, stream: StreamFade::No, pan };
                let entity = commands.spawn((player, settings, heard, channel)).id();
                if played.occlusion != ESoundOcclusion::None {
                    commands.entity(entity).insert(muffling);
                }
                entity
            }
            None => commands.spawn((player, settings, channel)).id(),
        };
        if played.allow_pause {
            commands.entity(entity).insert(Pausable);
        }
        if let Some(voice) = voice {
            book.voices.insert(voice, entity);
        }
    }
}

#[derive(Default)]
pub(crate) struct AudioWatch {
    seconds: f32,
    since_count: f32,
    sample: Option<(Entity, Duration)>,
    stalled: bool,
}

/// Bevy opens the output device once and never reopens it, and rodio reports a dead stream only on stderr:
/// a sound that stops advancing is how a lost device shows in the log.
fn watch_audio(
    time: Res<Time<bevy::time::Real>>,
    play: Res<Play>,
    sinks: Query<(Entity, &AudioSink)>,
    channels: Query<(), With<Channel>>,
    mut watch: Local<AudioWatch>,
) {
    watch.seconds += time.delta_secs();
    if watch.seconds < 1.0 {
        return;
    }
    watch.since_count += watch.seconds;
    watch.seconds = 0.0;
    let all = || sinks.iter().map(|(entity, sink)| (entity, sink as &dyn AudioSinkPlayback));
    let advancing = |sink: &dyn AudioSinkPlayback| !sink.is_paused() && !sink.empty();
    let limit = play.world.as_ref().map_or(0, |world| world.frame.audio_channels);
    let counts = format!("{} channels playing (limit {limit}), {} sinks", channels.iter().count(), all().count());
    if let Some((entity, position)) = watch.sample
        && let Some((_, sink)) = all().find(|(playing, _)| *playing == entity)
        && advancing(sink)
    {
        let stalled = sink.position() == position;
        if stalled && !watch.stalled {
            bevy::log::warn!("audio output stalled: a playing sound did not advance for a second; {counts}");
        } else if !stalled && watch.stalled {
            bevy::log::info!("audio output advancing again; {counts}");
        }
        watch.stalled = stalled;
    }
    watch.sample = all().find(|(_, sink)| advancing(*sink)).map(|(entity, sink)| (entity, sink.position()));
    if watch.since_count >= 60.0 {
        watch.since_count = 0.0;
        bevy::log::info!("audio: {counts}");
    }
}

fn watch_output_device() {
    use rodio::cpal::traits::{DeviceTrait, HostTrait};
    let watching = std::thread::Builder::new().name("audio device watch".into()).spawn(|| {
        let host = rodio::cpal::default_host();
        let default_device = || host.default_output_device().map(|device| (device.id().map(|id| id.to_string()).ok(), device.description().map(|description| description.to_string()).ok()));
        let mut last = default_device();
        bevy::log::info!("audio output device at start: {last:?}");
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let now = default_device();
            if now.as_ref().map(|device| &device.0) != last.as_ref().map(|device| &device.0) {
                bevy::log::warn!("default audio output device changed from {last:?} to {now:?}; sound stays on the device opened at start");
                last = now;
            }
        }
    });
    if let Err(error) = watching {
        bevy::log::warn!("audio device watch did not start: {error}");
    }
}

const ROLLOFF: f32 = 0.9;

/// Gain of a sound of `radius` heard at `distance`: OpenAL's inverse-distance-clamped model
/// with the radius as reference distance, as retail.
pub(crate) fn distance_gain(distance: f32, radius: f32) -> f32 {
    radius / (radius + ROLLOFF * (distance.max(radius) - radius))
}

fn side_of(camera: &Transform, at: Vec3) -> f32 {
    (at - camera.translation).normalize_or_zero().dot(camera.rotation * Vec3::X)
}

const SPEED_OF_SOUND: f32 = 343.3;
const UNITS_TO_METRES: f32 = 0.0127;
/// Retail gives OpenAL no velocity for a source moving faster than this against the view, in units a second.
const FASTEST_DOPPLER: f32 = 300.0;

fn doppler_shift(velocity: p2_math::Vector, to_listener: p2_math::Vector) -> f32 {
    let distance = to_listener.size();
    if velocity.size() > FASTEST_DOPPLER || distance <= 0.0 {
        return 1.0;
    }
    SPEED_OF_SOUND / (SPEED_OF_SOUND - velocity.dot(to_listener) / distance * UNITS_TO_METRES)
}

pub(super) const AUDIO_MAX_RADIUS_MULTIPLIER: f32 = 100.0;

/// Retail's channel priority: a new sound takes the channel of the lowest below it, or does not play. Nearer
/// than its radius times 100 to the view target counts, and an unpositioned sound ranks higher.
fn sound_priority(volume: f32, distance: Option<f32>, radius: f32, unpositioned: bool) -> f32 {
    let reach = distance.map_or(1.0, |distance| (1.0 - (distance / (AUDIO_MAX_RADIUS_MULTIPLIER * radius)).powi(2)).clamp(0.01, 1.0));
    volume * reach + if unpositioned { 1.0 } else { 0.0 }
}

pub(crate) fn ambient_gain(volume: f32, ambient_volume: f32, sound_volume: f32) -> f32 {
    (volume * ambient_volume * sound_volume).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn a_near_loud_sound_outranks_a_far_one_and_an_unpositioned_one_outranks_both() {
        let near = sound_priority(1.0, Some(100.0), 80.0, false);
        let far = sound_priority(1.0, Some(7000.0), 80.0, false);
        assert!(near > far && far > 0.0);
        assert_eq!(sound_priority(1.0, Some(9000.0), 80.0, false), 0.01);
        assert_eq!(sound_priority(0.5, None, 80.0, true), 1.5);
    }

    #[test]
    fn a_source_coming_closer_sounds_higher_and_one_too_fast_sounds_plain() {
        let ahead = p2_math::Vector::new(1000.0, 0.0, 0.0);
        let towards = doppler_shift(p2_math::Vector::new(300.0, 0.0, 0.0), ahead);
        assert!((towards - 343.3 / (343.3 - 3.81)).abs() < 1e-4);
        assert!(doppler_shift(p2_math::Vector::new(-300.0, 0.0, 0.0), ahead) < 1.0);
        assert_eq!(doppler_shift(p2_math::Vector::new(0.0, 300.0, 0.0), ahead), 1.0);
        assert_eq!(doppler_shift(p2_math::Vector::new(301.0, 0.0, 0.0), ahead), 1.0);
    }

    #[test]
    fn a_sound_to_the_cameras_right_is_on_the_right_however_the_camera_turns() {
        let ahead = Transform::from_xyz(10.0, 0.0, 10.0);
        assert_eq!(side_of(&ahead, Vec3::new(20.0, 0.0, 10.0)), 1.0);
        assert_eq!(side_of(&ahead, Vec3::new(10.0, 0.0, 0.0)), 0.0);
        let turned = ahead.looking_to(Vec3::X, Vec3::Y);
        assert!((side_of(&turned, Vec3::new(10.0, 0.0, 20.0)) - 1.0).abs() < 1e-5);
        assert!((side_of(&turned, Vec3::new(10.0, 0.0, 0.0)) + 1.0).abs() < 1e-5);
    }

    #[test]
    fn streamed_music_fades_out_linearly_to_a_hundred_times_its_radius() {
        assert_eq!(StreamFade::Legacy.factor(0.5, 50.0, 100.0), 1.0);
        assert!((StreamFade::Legacy.factor(0.5, 5050.0, 100.0) - 0.5).abs() < 1e-5);
        assert_eq!(StreamFade::Legacy.factor(0.5, 20000.0, 100.0), 0.0);
        assert_eq!(StreamFade::FromVolume.factor(0.5, 50.0, 100.0), 0.5);
        assert!(StreamFade::FromVolume.factor(0.5, 5050.0, 100.0).abs() < 1e-5);
        assert_eq!(StreamFade::No.factor(0.5, 20000.0, 100.0), 1.0);
    }

    #[test]
    fn what_plays_is_forgotten_when_another_world_is_played() {
        let mut world = bevy::ecs::world::World::new();
        world.init_resource::<Play>();
        world.init_resource::<SoundBook>();
        world.init_resource::<ZoneSteps>();
        world.init_resource::<Assets<PannedSound>>();
        world.init_resource::<Assets<AudioSource>>();
        let song = world.spawn_empty().id();
        world.resource_mut::<SoundBook>().songs.insert(1, song);
        world.run_system_once(start_sounds).expect("the sound system runs");
        assert_eq!(world.resource::<SoundBook>().songs.len(), 1);
        world.resource_mut::<Play>().set(None);
        world.run_system_once(start_sounds).expect("the sound system runs");
        assert!(world.resource::<SoundBook>().songs.is_empty());
    }
}
