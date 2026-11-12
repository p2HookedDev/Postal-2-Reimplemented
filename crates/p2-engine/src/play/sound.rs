use super::*;

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
            .configure_sets(Update, (SoundSystems::Stop, SoundSystems::Start, SoundSystems::Mix).chain().in_set(PlaySystems::Tick))
            .add_systems(Startup, watch_output_device)
            .add_systems(Update, watch_audio)
            .add_systems(Update, stop_old_songs.in_set(SoundSystems::Stop))
            .add_systems(Update, start_sounds.in_set(SoundSystems::Start).after(hand_off).before(look_ahead))
            .add_systems(Update, (heard_volume, sound_limits, fade_sounds, pause_movie_sound).chain().in_set(SoundSystems::Mix));
    }
}

struct PlayingAmbient {
    sound: ObjectId,
    entity: Entity,
    said: (p2_math::Vector, f32, f32),
}

/// What is playing for the world of `generation`, and the sounds read for it. Emptied when another world is played.
#[derive(Resource, Default)]
pub(crate) struct SoundBook {
    generation: u64,
    songs: HashMap<i32, Entity>,
    zone_sounds: FxHashMap<(ObjectId, usize), (Entity, f32)>,
    ambients: FxHashMap<ObjectId, PlayingAmbient>,
    heard_now: FxHashSet<ObjectId>,
    voices: FxHashMap<(ObjectId, u8), Entity>,
    sounds: HashMap<String, Option<LoadedSound>>,
    /// In the world's seconds played.
    flashbang_until: Option<f32>,
    flashbang_on: bool,
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

fn start_sounds(
    mut commands: Commands,
    mut play: ResMut<Play>,
    mut book: ResMut<SoundBook>,
    mut sources: ResMut<Assets<AudioSource>>,
    cameras: Query<&Transform, With<SceneView>>,
) {
    let generation = play.generation;
    if book.generation != generation {
        *book = SoundBook { generation, ..Default::default() };
    }
    let Some(world) = play.world.as_mut().filter(|world| world.fresh) else { return };
    let camera = cameras.iter().next().map(|camera| camera.translation);
    play_world_sounds(&mut commands, world, &mut book, &mut sources, camera, generation);
    play_music(&mut commands, world, &mut book, &mut sources, camera, generation);
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

/// The quietest give way when more than `Channels` play. One on an actor's ambient slot stops when its
/// actor goes; any other plays on where it was.
#[derive(Component)]
pub(crate) struct Channel {
    loudness: f32,
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
    channels: Query<(Entity, &Channel)>,
    mut loops: Query<&mut AudioSink, With<Song>>,
    mut spatial_loops: Query<&mut SpatialAudioSink, With<Song>>,
    mut playing: Local<Vec<(Entity, f32)>>,
) {
    let Some(world) = play.world.as_ref() else { return };
    let flashbang = book.flashbang_playing(world.seconds);
    if flashbang != book.flashbang_on {
        book.flashbang_on = flashbang;
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

pub(super) fn pitch_factor(pitch: f32) -> f32 {
    pitch.clamp(0.5, 2.0)
}

fn spatial(settings: PlaybackSettings) -> PlaybackSettings {
    PlaybackSettings { spatial: true, spatial_scale: Some(SpatialScale(Vec3::splat(PAN_SCALE))), ..settings }
}

#[derive(Component)]
pub(crate) struct HeardSound {
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
        if let Some((location, _)) = heard.actor.and_then(|actor| world.place(actor)) {
            heard.position = world.scene_position(location);
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

fn play_music(commands: &mut Commands, world: &mut PlayWorld, book: &mut SoundBook, sources: &mut Assets<AudioSource>, camera: Option<Vec3>, generation: u64) {
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
                            commands.entity(entity).insert(Fade::new(0.0, 1.0, fade_in, FadeEnd::Keep));
                        }
                        entity
                    }
                    None if fade_in > 0.0 => {
                        let faded = (BaseVolume(volume * gain), Fade::new(0.0, 1.0, fade_in, FadeEnd::Keep));
                        commands.spawn((AudioPlayer::new(source), settings.with_volume(Volume::SILENT), Song(generation), faded)).id()
                    }
                    None => commands.spawn((AudioPlayer::new(source), settings, Song(generation), BaseVolume(volume * gain))).id(),
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

fn play_world_sounds(commands: &mut Commands, world: &mut PlayWorld, book: &mut SoundBook, sources: &mut Assets<AudioSource>, camera: Option<Vec3>, generation: u64) {
    for (path, file) in std::mem::take(&mut world.frame.sound_files) {
        book.add_sound_file(path, file, sources);
    }
    play_sounds(commands, world, book, camera, generation);
    play_ambients(commands, world, book, generation);
    play_zone_sounds(commands, world, book, sources, generation);
}

fn play_ambients(commands: &mut Commands, world: &mut PlayWorld, book: &mut SoundBook, generation: u64) {
    let mut ambients = std::mem::take(&mut world.frame.ambients);
    let mut heard_now = std::mem::take(&mut book.heard_now);
    heard_now.clear();
    for (actor, ambient, path) in ambients.drain(..) {
        heard_now.insert(actor);
        let position = world.scene_position(ambient.location);
        let radius = if ambient.radius > 0.0 { ambient.radius } else { p2_vm::AUDIO_DEFAULT_RADIUS };
        let heard = HeardSound { actor: Some(actor), position, volume: ambient.volume, radius, fade: 1.0 };
        let said = (ambient.location, ambient.volume, radius);
        if let Some(playing) = book.ambients.get_mut(&actor)
            && playing.sound == ambient.sound
        {
            if playing.said != said {
                playing.said = said;
                commands.entity(playing.entity).insert(heard);
            }
            continue;
        }
        if let Some(old) = book.ambients.remove(&actor) {
            commands.entity(old.entity).despawn();
        }
        let Some(source) = book.sound(&path).map(|loaded| loaded.source) else { continue };
        let settings = spatial(PlaybackSettings::LOOP.with_volume(Volume::SILENT).with_speed(pitch_factor(ambient.pitch)));
        let entity = commands.spawn((AudioPlayer::new(source), settings, heard, Song(generation), Transform::from_translation(position))).id();
        book.ambients.insert(actor, PlayingAmbient { sound: ambient.sound, entity, said });
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
    book.zone_sounds.retain(|key, &mut (entity, fade_out)| {
        let keep = keys.contains(key);
        if !keep {
            stop_fading(commands, entity, fade_out);
        }
        keep
    });
    for zone_sound in heard {
        if book.zone_sounds.contains_key(&zone_sound.key) {
            continue;
        }
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
        book.zone_sounds.insert(zone_sound.key, (entity, fade_out));
    }
}

fn play_sounds(commands: &mut Commands, world: &mut PlayWorld, book: &mut SoundBook, camera: Option<Vec3>, generation: u64) {
    book.voices.retain(|_, &mut entity| commands.get_entity(entity).is_ok());
    for (played, position) in std::mem::take(&mut world.frame.sounds) {
        if played.flashbang.is_none() && book.flashbang_playing(world.seconds) {
            continue;
        }
        let Some(loaded) = book.sound(&played.sound) else { continue };
        let source = loaded.source;
        if let Some(duration) = played.flashbang {
            book.flashbang_until = Some(world.seconds + if duration > 0.0 { duration } else { loaded.duration });
            let settings = PlaybackSettings::DESPAWN.with_volume(Volume::Linear(played.volume)).with_speed(pitch_factor(played.pitch));
            commands.spawn((AudioPlayer::new(source), settings));
            continue;
        }
        // A sound on an actor's slot (not slot 0) replaces the one still playing there,
        // or with `bNoOverride` is dropped.
        let voice = played.actor.filter(|_| played.slot != 0).map(|actor| (actor, played.slot));
        if let Some(mut playing) = voice.and_then(|voice| book.voices.get(&voice)).and_then(|&entity| commands.get_entity(entity).ok()) {
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
    spatial_sinks: Query<(Entity, &SpatialAudioSink)>,
    channels: Query<(), With<Channel>>,
    mut watch: Local<AudioWatch>,
) {
    watch.seconds += time.delta_secs();
    if watch.seconds < 1.0 {
        return;
    }
    watch.since_count += watch.seconds;
    watch.seconds = 0.0;
    let all = || sinks.iter().map(|(entity, sink)| (entity, sink as &dyn AudioSinkPlayback)).chain(spatial_sinks.iter().map(|(entity, sink)| (entity, sink as &dyn AudioSinkPlayback)));
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

pub(crate) fn ambient_gain(volume: f32, ambient_volume: f32, sound_volume: f32) -> f32 {
    (volume * ambient_volume * sound_volume).clamp(0.0, 1.0)
}

/// Retail hears the player's right (Unreal +Y, the camera's +X) in the right channel. rodio 0.22's
/// `Spatial` gives the louder channel to the ear FARTHER from the sound, so the ears are swapped.
pub(crate) fn sound_listener() -> SpatialListener {
    let ears = SpatialListener::default();
    SpatialListener { left_ear_offset: ears.right_ear_offset, right_ear_offset: ears.left_ear_offset }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn what_plays_is_forgotten_when_another_world_is_played() {
        let mut world = bevy::ecs::world::World::new();
        world.init_resource::<Play>();
        world.init_resource::<SoundBook>();
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
