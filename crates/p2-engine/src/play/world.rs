use p2_math::{Rotator, Vector};
use super::*;

#[derive(Clone, PartialEq)]
pub struct ActorFrame {
    pub location: Vector,
    pub rotation: Rotator,
    pub scale: Vector,
    pub pre_pivot: Vector,
    /// An emitter spawns its particles along its path through the frame.
    pub velocity: Vector,
    /// Player-owned emitters ignore `TimeDilation` (`Vm::particle_time_scale`).
    pub time_scale: f32,
    pub gone: bool,
    /// Not drawn for the player: `bHidden`, or the owner rules (the player's own pawn in first person).
    pub hidden: bool,
    pub attachment: Option<BoneAttachment>,
    pub emitters_disabled: Vec<bool>,
    /// When it changes, the host rebuilds the model ([`Spawned::restyled`]).
    pub look: u64,
    pub glow: u8,
}

/// Channels above 0 blend over channel 0. Bone names are lower case and shared, so frames allocate
/// no strings. An empty start bone means the root.
pub struct ActorPose {
    pub base: ChannelPose,
    pub blends: Vec<(ChannelPose, f32, Arc<str>)>,
    pub scales: Vec<(Arc<str>, f32)>,
    pub turns: Vec<(Arc<str>, [i32; 3])>,
}

pub struct Spawned {
    pub actor: ObjectId,
    pub mesh: Option<MeshActor>,
    pub skeletal: Option<SkeletalActor>,
    pub emitter: Option<EmitterActor>,
    pub sprite: Option<crate::actors::SpriteActor>,
    pub corona: Option<crate::map::CoronaActor>,
    pub restyled: bool,
}

#[derive(Clone, PartialEq)]
pub struct Overlay {
    /// How much of the scene shows (1 = all, 0 = only the fog colour).
    pub flash_scale: f32,
    pub flash_fog: [f32; 3],
    pub titles: Vec<OverlayTitle>,
}

#[derive(Clone, PartialEq)]
pub struct OverlayTitle {
    pub material: String,
    pub color: [u8; 4],
    /// Width relative to the screen width.
    pub size: f32,
    /// Centre as fractions of the screen width and height.
    pub at: [f32; 2],
    pub alpha: f32,
}

pub(super) struct PlayingAmbient {
    pub(super) sound: ObjectId,
    pub(super) entity: Entity,
    pub(super) said: (Vector, f32, f32),
}

pub struct PlayWorld {
    pub(super) sim: Slot,
    pub(super) worker: Option<Worker>,
    pub(super) loader: Arc<Loader>,
    pub(super) frame: VmFrame,
    pub(super) ready: Option<VmFrame>,
    pub(super) presented: bool,
    pub(super) spare: Option<VmFrame>,
    pub(super) actors: FxHashMap<ObjectId, ActorFrame>,
    pub(super) changed: FxHashSet<ObjectId>,
    pub(super) attached: FxHashSet<ObjectId>,
    pub(super) canvas_models: FxHashMap<ObjectId, CanvasModel>,
    pub(super) origin: Vec3,
    pub(super) followed: FxHashSet<ObjectId>,
    pub(super) visible: FxHashSet<ObjectId>,
    pub(super) watched: FxHashSet<ObjectId>,
    pub(crate) wants_forces: bool,
    pub(crate) finished: Vec<ObjectId>,
    pub(crate) movie_status: Vec<MovieStatus>,
    pub(super) songs: HashMap<i32, Entity>,
    pub(super) zone_sounds: FxHashMap<(ObjectId, usize), (Entity, f32)>,
    pub(super) flashbang_until: Option<f32>,
    pub(super) flashbang_on: bool,
    pub(super) ambients: FxHashMap<ObjectId, PlayingAmbient>,
    pub(super) heard_now: FxHashSet<ObjectId>,
    pub(super) voices: FxHashMap<(ObjectId, u8), Entity>,
    pub(super) sounds: HashMap<String, Option<LoadedSound>>,
    pub(super) seconds: f32,
}

impl PlayWorld {
    pub fn new(
        vm: Vm<Arc<Loader>>,
        loader: Arc<Loader>,
        center: Vec3,
        player: Option<(ObjectId, Option<ObjectId>)>,
        music_dir: PathBuf,
    ) -> Self {
        let (player, player_pawn) = (player.map(|(controller, _)| controller), player.and_then(|(_, pawn)| pawn));
        let seen = vm.object_ids().count();
        let lights = vm.object_ids().filter(|&actor| crate::actors::mesh_light(&vm, actor).is_some_and(|light| light.dynamic)).collect();
        let fluid_surfaces = vm.actors().filter(|&actor| vm.is_a(actor, "FluidSurfaceInfo") && !vm.is_deleted(actor)).collect();
        let corona_lights = vm.object_ids().filter(|&actor| crate::map::is_corona(&vm, actor)).collect();
        let volumes = vm.actors().filter(|&actor| vm.is_a(actor, Kind::Volume) && !vm.is_deleted(actor)).collect();
        let switches = vm
            .object_ids()
            .filter(|&object| is_switch(&vm, object))
            .map(|object| (object, vm.object(object).class.map(|_| switch_material(&vm, object))))
            .collect();
        let sim = Simulation {
            vm,
            player,
            player_pawn,
            view: None,
            seen,
            recent: Default::default(),
            recent_read: Vec::new(),
            lights,
            light_clock: Default::default(),
            volumes,
            switches,
            fluid_surfaces,
            corona_lights,
            posed: FxHashSet::default(),
            animations: FxHashMap::default(),
            sequences: FxHashMap::default(),
            emitter_values: Default::default(),
            sent: FxHashMap::default(),
            lower_names: FxHashMap::default(),
            settings_sent: false,
            music_dir,
            zone_songs: HashMap::new(),
            zone_keys: Vec::new(),
            ambient_paths: FxHashMap::default(),
            sound_paths: HashSet::new(),
            work_since: None,
            work_second: Default::default(),
            hud_drawn: Vec::new(),
            hud_looks: FxHashMap::default(),
            unmeshed: Default::default(),
            emitterless: Default::default(),
            quit: false,
        };
        Self {
            sim: Slot::Here(Box::new(sim)),
            worker: None,
            loader,
            frame: VmFrame::default(),
            ready: None,
            presented: false,
            spare: None,
            actors: FxHashMap::default(),
            changed: FxHashSet::default(),
            attached: FxHashSet::default(),
            canvas_models: FxHashMap::default(),
            origin: -center,
            followed: FxHashSet::default(),
            wants_forces: false,
            finished: Vec::new(),
            movie_status: Vec::new(),
            visible: FxHashSet::default(),
            watched: FxHashSet::default(),
            songs: HashMap::new(),
            ambients: FxHashMap::default(),
            heard_now: FxHashSet::default(),
            zone_sounds: FxHashMap::default(),
            flashbang_until: None,
            flashbang_on: false,
            voices: FxHashMap::default(),
            sounds: HashMap::new(),
            seconds: 0.0,
        }
    }

    pub(super) fn wait(&mut self) {
        if !matches!(self.sim, Slot::Away) {
            return;
        }
        match self.worker.as_ref().and_then(|worker| worker.done.lock().ok().map(|done| done.recv())) {
            Some(Ok((sim, frame))) => {
                self.sim = Slot::Here(sim);
                self.ready = Some(frame);
            }
            _ => {
                bevy::log::error!("the VM's thread died; the world stops");
                self.sim = Slot::Lost;
                self.worker = None;
            }
        }
    }

    pub(super) fn start(&mut self, job: Job, threaded: bool) {
        let sim = match std::mem::replace(&mut self.sim, Slot::Away) {
            Slot::Here(sim) => sim,
            other => {
                self.sim = other;
                return;
            }
        };
        if threaded && self.worker.is_none() {
            self.worker = Worker::spawn();
        }
        let (mut sim, job) = match self.worker.as_ref().filter(|_| threaded) {
            Some(worker) => match worker.jobs.send((sim, job)) {
                Ok(()) => return,
                Err(std::sync::mpsc::SendError(back)) => back,
            },
            None => (sim, job),
        };
        self.ready = Some(sim.run(job));
        self.sim = Slot::Here(sim);
    }

    pub(super) fn present(&mut self) -> bool {
        self.changed.clear();
        let Some(mut frame) = self.ready.take() else { return false };
        let mut old = std::mem::take(&mut self.frame);
        if frame.travel.is_none() {
            frame.travel = old.travel.take();
        }
        frame.spawned.splice(0..0, old.spawned.drain(..));
        frame.projections.splice(0..0, old.projections.drain(..));
        frame.particles.splice(0..0, old.particles.drain(..));
        frame.plings.splice(0..0, old.plings.drain(..));
        frame.material_switches.splice(0..0, old.material_switches.drain(..));
        frame.emitter_changes.splice(0..0, old.emitter_changes.drain(..));
        if frame.window.is_none() {
            frame.window = old.window.take();
        }
        if frame.projector_actors.is_none() {
            frame.projector_actors = old.projector_actors.take();
        }
        if frame.settings.is_none() {
            frame.settings = old.settings.take();
        }
        if frame.set_mouse.is_none() {
            frame.set_mouse = old.set_mouse.take();
        }
        for (actor, now) in frame.actor_changes.drain(..) {
            self.changed.insert(actor);
            if now.attachment.is_some() {
                self.attached.insert(actor);
            } else {
                self.attached.remove(&actor);
            }
            self.actors.insert(actor, now);
        }
        self.canvas_models.extend(frame.canvas_models.drain(..));
        self.spare = Some(old);
        self.frame = frame;
        self.presented = true;
        true
    }

    pub fn vm_lost(&self) -> bool {
        matches!(self.sim, Slot::Lost)
    }

    pub(super) fn sim_mut(&mut self) -> Option<&mut Simulation> {
        self.wait();
        match &mut self.sim {
            Slot::Here(sim) => Some(sim),
            Slot::Away | Slot::Lost => None,
        }
    }

    pub fn with_vm<R>(&mut self, f: impl FnOnce(&mut Vm<Arc<Loader>>) -> R) -> Option<R> {
        self.sim_mut().map(|sim| f(&mut sim.vm))
    }

    pub fn loader(&self) -> &Arc<Loader> {
        &self.loader
    }

    pub fn actor(&self, actor: ObjectId) -> Option<&ActorFrame> {
        self.actors.get(&actor)
    }

    pub fn changed(&self, actor: ObjectId) -> bool {
        self.changed.contains(&actor)
    }

    pub fn attached(&self, actor: ObjectId) -> bool {
        self.attached.contains(&actor)
    }

    /// `Some(None)` if it does not animate, `None` if not read (no camera saw it).
    pub fn pose(&self, actor: ObjectId) -> Option<Option<&ActorPose>> {
        self.frame.poses.get(&actor).map(Option::as_ref)
    }

    pub fn ragdoll(&self, actor: ObjectId) -> Option<&[p2_assets::pose::Frame]> {
        self.frame.ragdolls.get(&actor).map(Vec::as_slice)
    }

    pub fn canvas_mesh(&self, actor: ObjectId) -> Option<&CanvasMesh> {
        match self.canvas_models.get(&actor) {
            Some(CanvasModel::Skeletal(mesh)) => Some(mesh),
            _ => None,
        }
    }

    pub fn canvas_static(&self, actor: ObjectId) -> Option<&MeshActor> {
        match self.canvas_models.get(&actor) {
            Some(CanvasModel::Static(model)) => Some(model),
            _ => None,
        }
    }

    pub fn fov(&self) -> Option<f32> {
        self.frame.fov
    }

    pub fn origin(&self) -> Vec3 {
        self.origin
    }

    pub fn take_spawned(&mut self) -> Vec<Spawned> {
        std::mem::take(&mut self.frame.spawned)
    }

    pub fn is_gone(&self, actor: ObjectId) -> bool {
        self.actor(actor).is_some_and(|a| a.gone)
    }

    pub fn watch(&mut self, actor: ObjectId) {
        self.watched.insert(actor);
    }

    pub fn unwatch(&mut self, actor: ObjectId) {
        self.watched.remove(&actor);
    }

    pub fn watched(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.watched.iter().copied()
    }

    pub fn dynamic_lights(&self) -> &[p2_render_cpu::MeshLight] {
        &self.frame.dynamic_lights
    }

    pub fn actor_forces(&self) -> &[ActorForce] {
        &self.frame.forces
    }

    pub fn take_particle_commands(&mut self) -> Vec<ParticleCommand<EmitterProperties>> {
        std::mem::take(&mut self.frame.particles)
    }

    pub fn step(&self) -> f32 {
        self.frame.step
    }

    pub fn take_plings(&mut self) -> Vec<p2_vm::FluidPling> {
        std::mem::take(&mut self.frame.plings)
    }

    pub fn fluid_touchers(&self, surface: ObjectId) -> &[p2_particles::FluidToucher] {
        self.frame.fluid_touchers.iter().find(|(s, _)| *s == surface).map_or(&[][..], |(_, touchers)| touchers.as_slice())
    }

    pub fn corona_blocked(&self, light: ObjectId) -> bool {
        self.frame.corona_blocked.binary_search(&light).is_ok()
    }

    pub fn take_material_switches(&mut self) -> Vec<(String, Option<String>)> {
        std::mem::take(&mut self.frame.material_switches)
    }

    pub fn take_emitter_changes(&mut self) -> Vec<(ObjectId, EmitterProperties)> {
        std::mem::take(&mut self.frame.emitter_changes)
    }

    pub fn screen_overlay(&self) -> Option<Overlay> {
        self.frame.overlay.clone()
    }

    pub fn take_set_mouse(&mut self) -> Option<[f32; 2]> {
        self.frame.set_mouse.take()
    }

    pub fn wants_quit(&self) -> bool {
        self.frame.quit
    }

    pub fn take_display(&mut self) -> (Option<p2_vm::WindowRequest>, Option<crate::display::Settings>) {
        (self.frame.window.take(), self.frame.settings.take())
    }

    pub fn shows_mouse(&self) -> bool {
        self.frame.shows_mouse
    }

    /// In seconds, once: later calls return 0 until the next frame (egui may run a frame's UI pass twice, which must not
    /// advance what follows the game twice).
    pub fn take_step(&mut self) -> f32 {
        std::mem::take(&mut self.frame.step)
    }

    pub fn take_projections(&mut self) -> Vec<(ProjectorEvent, Option<ProjectorActor>)> {
        std::mem::take(&mut self.frame.projections)
    }

    pub fn take_projector_actors(&mut self) -> Option<FxHashSet<ObjectId>> {
        self.frame.projector_actors.take()
    }

    pub fn take_travel(&mut self) -> Option<String> {
        self.frame.travel.take()
    }

    pub fn presented(&self) -> bool {
        self.presented
    }

    pub fn hud_items(&self) -> Arc<[CanvasItem]> {
        Arc::clone(&self.frame.hud)
    }

    pub fn movies(&self) -> &[(String, ObjectId, p2_vm::MovieState)] {
        &self.frame.movies
    }

    pub fn game_paused(&self) -> bool {
        self.frame.paused
    }

    pub fn hud(&mut self, width: f32, height: f32) -> Vec<CanvasItem> {
        self.sim_mut().map(|sim| sim.hud([width, height])).unwrap_or_default()
    }

    pub fn summary(&self) -> String {
        format!("Playing: {:.0} s", self.seconds)
    }

    pub(super) fn add_sound_file(&mut self, path: String, file: Option<p2_assets::Sound>, sources: &mut Assets<AudioSource>) {
        if file.is_none() {
            bevy::log::warn!("sound {path}: not found in a loaded package or its package file");
        }
        let loaded = file.and_then(|sound| loaded_sound(&path, sound, sources));
        self.sounds.entry(path).or_insert(loaded);
    }

    pub(super) fn sound(&self, path: &str) -> Option<LoadedSound> {
        self.sounds.get(path).cloned().flatten()
    }

    pub(super) fn scene_position(&self, location: Vector) -> Vec3 {
        Vec3::from(to_bevy(location)) + self.origin
    }

    pub(super) fn flashbang_playing(&self) -> bool {
        self.flashbang_until.is_some_and(|until| self.seconds < until)
    }
}

/// `None` (warned once, the result kept per path) when Bevy's decoder cannot open its file: `AudioSource::decoder`
/// unwraps the decoder, so such a sound would panic the frame it plays.
fn loaded_sound(path: &str, sound: p2_assets::Sound, sources: &mut Assets<AudioSource>) -> Option<LoadedSound> {
    let (duration, looping) = (sound.duration().unwrap_or(0.0), sound.loops());
    let bytes: Arc<[u8]> = sound.data.into();
    if !opens(&bytes) {
        bevy::log::warn!("sound {path}: {} file the audio decoder cannot open; not played", sound.file_type);
        return None;
    }
    Some(LoadedSound { source: sources.add(AudioSource { bytes }), duration, looping })
}

pub(super) fn opens(bytes: &Arc<[u8]>) -> bool {
    decodable(bytes) && rodio::Decoder::builder().with_byte_len(bytes.len() as u64).with_data(std::io::Cursor::new(Arc::clone(bytes))).build().is_ok()
}

/// Whether rodio (`hound` for WAV, `lewton` for Ogg Vorbis) opens `data` without panicking: an Ogg stream, or a RIFF WAVE
/// whose `fmt ` chunk (before its `data` chunk) has format PCM (1), IEEE float (3) or extensible (0xFFFE), at least one
/// channel and a sample rate. Others fail `hound::WavReader::new`; no channels or rate fail `WavDecoder::new`'s asserts.
fn decodable(data: &[u8]) -> bool {
    if data.starts_with(b"OggS") {
        return true;
    }
    if data.get(0..4) != Some(b"RIFF") || data.get(8..12) != Some(b"WAVE") {
        return false;
    }
    let (mut format_ok, mut at) = (false, 12);
    while let Some(header) = data.get(at..at + 8) {
        let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        match &header[..4] {
            b"fmt " => {
                let Some(fmt) = data.get(at + 8..at + 16) else { return false };
                let (tag, channels) = (u16::from_le_bytes([fmt[0], fmt[1]]), u16::from_le_bytes([fmt[2], fmt[3]]));
                let rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
                format_ok = matches!(tag, 1 | 3 | 0xFFFE) && channels > 0 && rate > 0;
            }
            b"data" => return format_ok,
            _ => {}
        }
        // Chunks are padded to even sizes.
        at += 8 + length + (length & 1);
    }
    false
}

#[derive(Clone)]
pub(super) struct LoadedSound {
    pub(super) source: Handle<AudioSource>,
    pub(super) duration: f32,
    pub(super) looping: bool,
}

pub struct PlayedAnimation {
    pub path: Arc<str>,
    pub animation: Arc<MeshAnimation>,
    pub bone_names: Vec<String>,
    pub(super) sequences: HashMap<String, usize>,
}

impl PlayedAnimation {
    pub(super) fn load(vm: &mut Vm<Arc<Loader>>, object: ObjectId, path: &str) -> Option<Arc<Self>> {
        let animation = vm.mesh_animation(object)?;
        let loader = vm.loader();
        let (package, _) = loader.loaded_export_by_path_of_class(path, "MeshAnimation")?;
        let pkg = &loader.package(package).package;
        let bone_names = animation.bones.iter().map(|b| pkg.name(b.name).to_ascii_lowercase()).collect();
        let sequences = animation.sequences.iter().enumerate().map(|(i, s)| (pkg.name(s.name).to_ascii_lowercase(), i)).collect();
        Some(Arc::new(Self { path: Arc::from(path), animation, bone_names, sequences }))
    }
}

pub struct ChannelPose {
    pub animation: Arc<PlayedAnimation>,
    pub sequence: usize,
    pub time: f32,
    pub tween: Option<(Arc<[p2_assets::pose::LocalBone]>, f32)>,
}

#[cfg(test)]
mod tests {
    use super::{decodable, opens};

    fn wav(tag: u16, channels: u16, rate: u32) -> Vec<u8> {
        let mut body = b"WAVEfmt ".to_vec();
        body.extend(16u32.to_le_bytes());
        body.extend(tag.to_le_bytes());
        body.extend(channels.to_le_bytes());
        body.extend(rate.to_le_bytes());
        body.extend([0; 8]);
        body.extend(b"data");
        body.extend(0u32.to_le_bytes());
        let mut file = b"RIFF".to_vec();
        file.extend((body.len() as u32).to_le_bytes());
        file.extend(body);
        file
    }

    #[test]
    fn only_files_the_decoder_opens_are_decodable() {
        assert!(decodable(&wav(1, 1, 22050)));
        assert!(decodable(&wav(3, 2, 44100)));
        assert!(decodable(b"OggS and the rest of the stream"));
        assert!(!decodable(&wav(0x11, 1, 22050)));
        assert!(!decodable(&wav(0x55, 1, 22050)));
        assert!(!decodable(&wav(1, 0, 22050)));
        assert!(!decodable(&wav(1, 1, 0)));
        assert!(!decodable(b"ID3 an mp3 file"));
    }

    #[test]
    fn a_broken_song_does_not_open() {
        assert!(!opens(&std::sync::Arc::from(&b"OggS and the rest of the stream"[..])));
    }
}
