use p2_math::Vector;
use super::*;
mod after_hud;
mod host_reads;
mod run;
mod sounds;
use sounds::read_song;

/// Read in the frame's own order: after the tick the view, sounds and followed actors; after the HUD's scripts what it
/// drew, spawned actors, projections and particles. Its buffers are reused: the host sends the frame it presented last
/// back with the next job.
#[derive(Default)]
pub struct VmFrame {
    pub(super) step: f32,
    pub(super) view: Option<p2_vm::PlayerView>,
    pub(super) sounds: Vec<(PlayedSound, Option<Vector>)>,
    pub(super) sound_files: Vec<(String, Option<p2_assets::Sound>)>,
    pub(super) ambients: Vec<(ObjectId, crate::actors::AmbientActor, Arc<str>)>,
    pub(super) zone_sounds: Option<Vec<ZoneSound>>,
    pub(super) zone_fogs: Option<Vec<Option<crate::actors::ZoneFog>>>,
    pub(super) audio_channels: usize,
    pub(super) music: Vec<(MusicCommand, Option<Vector>, Option<Arc<[u8]>>)>,
    pub(super) actor_changes: Vec<(ObjectId, ActorFrame)>,
    pub(super) poses: FxHashMap<ObjectId, Option<ActorPose>>,
    pub(super) ragdolls: FxHashMap<ObjectId, Vec<p2_assets::pose::Frame>>,
    pub(super) shows_mouse: bool,
    pub(super) travel: Option<String>,
    pub(super) hud: Arc<[CanvasItem]>,
    pub(super) canvas_models: Vec<(ObjectId, CanvasModel)>,
    pub(super) spawned: Vec<Spawned>,
    pub(super) projections: Vec<(ProjectorEvent, Option<ProjectorActor>)>,
    pub(super) projector_actors: Option<FxHashSet<ObjectId>>,
    pub(super) particles: Vec<ParticleCommand<EmitterProperties>>,
    pub(super) forces: Vec<ActorForce>,
    pub(super) emitter_changes: Vec<(ObjectId, EmitterProperties)>,
    pub(super) dynamic_lights: Vec<p2_render_cpu::MeshLight>,
    pub(super) plings: Vec<p2_vm::FluidPling>,
    pub(super) fluid_touchers: Vec<(ObjectId, Vec<p2_particles::FluidToucher>)>,
    pub(super) corona_blocked: Vec<ObjectId>,
    pub(super) material_switches: Vec<(String, Option<String>)>,
    pub(super) fov: Option<f32>,
    pub(super) overlay: Option<Overlay>,
    pub(super) movies: Vec<(String, ObjectId, p2_vm::MovieState)>,
    pub(super) paused: bool,
    pub(super) quit: bool,
    pub(super) window: Option<p2_vm::WindowRequest>,
    pub(super) settings: Option<crate::display::Settings>,
    pub(super) set_mouse: Option<[f32; 2]>,
    pub(super) times: Vec<(&'static str, Duration)>,
    pub(super) profile: HashMap<String, Duration>,
    pub(super) job_buffers: JobBuffers,
}

pub(super) enum CanvasModel {
    Static(MeshActor),
    Skeletal(CanvasMesh),
    Nothing,
}

pub struct CanvasMesh {
    pub path: String,
    pub skins: Vec<Option<String>>,
    pub follow: FollowActor,
}

pub(crate) struct MovieStatus {
    pub(crate) movie: ObjectId,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) playing: bool,
}

pub(super) struct ZoneSound {
    pub(super) key: (ObjectId, usize),
    pub(super) sound: Option<String>,
    pub(super) song: Option<Arc<[u8]>>,
    pub(super) volume: f32,
    pub(super) pitch: f32,
    pub(super) fade_in: f32,
    pub(super) fade_out: f32,
}

#[derive(Default)]
pub(super) struct JobBuffers {
    pub(super) held: Vec<&'static str>,
    pub(super) followed: FxHashSet<ObjectId>,
    pub(super) visible: FxHashSet<ObjectId>,
    pub(super) placed: Vec<(ObjectId, BoneFrame)>,
    pub(super) watched: Vec<ObjectId>,
}

impl VmFrame {
    pub(super) fn recycle(mut self, step: f32) -> Self {
        self.sounds.clear();
        self.sound_files.clear();
        self.ambients.clear();
        self.music.clear();
        self.actor_changes.clear();
        self.poses.clear();
        self.ragdolls.clear();
        self.hud = Arc::default();
        self.canvas_models.clear();
        self.spawned.clear();
        self.projections.clear();
        self.projector_actors = None;
        self.particles.clear();
        self.forces.clear();
        self.emitter_changes.clear();
        self.dynamic_lights.clear();
        self.material_switches.clear();
        self.plings.clear();
        self.fluid_touchers.clear();
        self.corona_blocked.clear();
        self.movies.clear();
        self.times.clear();
        self.profile.clear();
        Self { step, view: None, zone_sounds: None, zone_fogs: None, shows_mouse: false, travel: None, fov: None, overlay: None, quit: false, window: None, settings: None, set_mouse: None, ..self }
    }
}

pub(super) struct Job {
    pub(super) frame: VmFrame,
    pub(super) step: f32,
    pub(super) playing: bool,
    pub(super) held: Vec<&'static str>,
    pub(super) mouse: [f32; 2],
    pub(super) typed: String,
    pub(super) key_events: Vec<(&'static str, bool)>,
    pub(super) cursor: Option<[f32; 2]>,
    pub(super) canvas: [f32; 2],
    pub(super) followed: FxHashSet<ObjectId>,
    pub(super) visible: FxHashSet<ObjectId>,
    pub(super) placed: Vec<(ObjectId, BoneFrame)>,
    pub(super) watched: Vec<ObjectId>,
    pub(super) finished: Vec<ObjectId>,
    pub(super) vm_profile: VmProfile,
    pub(super) forces: bool,
    pub(super) movie_status: Vec<MovieStatus>,
}

pub(super) struct Simulation {
    pub(super) vm: Vm<Arc<Loader>>,
    pub(super) player: Option<ObjectId>,
    pub(super) player_pawn: Option<ObjectId>,
    pub(super) view: Option<p2_vm::PlayerView>,
    pub(super) hud_drawn: Vec<ObjectId>,
    pub(super) hud_looks: FxHashMap<ObjectId, u64>,
    pub(super) seen: usize,
    pub(super) recent: [Vec<ObjectId>; 2],
    pub(super) recent_read: Vec<ObjectId>,
    pub(super) lights: Vec<ObjectId>,
    pub(super) light_clock: p2_render_cpu::LightClock,
    pub(super) volumes: Vec<ObjectId>,
    pub(super) switches: Vec<(ObjectId, Option<Option<ObjectId>>)>,
    pub(super) fluid_surfaces: Vec<ObjectId>,
    pub(super) corona_lights: Vec<ObjectId>,
    pub(super) posed: FxHashSet<ObjectId>,
    pub(super) animations: FxHashMap<ObjectId, Option<Arc<PlayedAnimation>>>,
    pub(super) sequences: FxHashMap<(ObjectId, p2_vm::NameId), Option<usize>>,
    pub(super) emitter_values: crate::emitter_changes::EmitterValues,
    pub(super) sent: FxHashMap<ObjectId, ActorFrame>,
    pub(super) lower_names: FxHashMap<p2_vm::NameId, Arc<str>>,
    pub(super) settings_sent: bool,
    pub(super) zone_actors: Vec<Option<ObjectId>>,
    pub(super) zone_fogs_sent: Vec<Option<crate::actors::ZoneFog>>,
    pub(super) music_dir: PathBuf,
    pub(super) zone_songs: HashMap<String, Option<Arc<[u8]>>>,
    pub(super) zone_keys: Vec<(ObjectId, usize)>,
    pub(super) ambient_paths: FxHashMap<ObjectId, Arc<str>>,
    pub(super) sound_paths: HashSet<String>,
    pub(super) work_since: Option<(Instant, u32)>,
    pub(super) work_second: WorkSecond,
    pub(super) unmeshed: FxHashSet<ObjectId>,
    pub(super) emitterless: HashSet<String>,
    pub(super) quit: bool,
}

#[derive(Default)]
pub(super) struct WorkSecond {
    counts: Vec<(&'static str, u64)>,
    functions: HashMap<String, p2_vm::FunctionTime>,
    slowest: Option<(Duration, Vec<(&'static str, u64)>, Vec<(String, p2_vm::FunctionTime)>)>,
}

pub(super) enum Slot {
    Here(Box<Simulation>),
    Away,
    Lost,
}

pub(super) struct Worker {
    pub(super) jobs: Sender<(Box<Simulation>, Job)>,
    pub(super) done: Mutex<Receiver<(Box<Simulation>, VmFrame)>>,
}

impl Worker {
    pub(super) fn spawn() -> Option<Self> {
        let (jobs, inbox) = std::sync::mpsc::channel::<(Box<Simulation>, Job)>();
        let (outbox, done) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("vm".into()).stack_size(SCRIPT_STACK).spawn(move || {
            while let Ok((mut sim, job)) = inbox.recv() {
                let frame = sim.run(job);
                if outbox.send((sim, frame)).is_err() {
                    break;
                }
            }
        });
        match spawned {
            Ok(_) => Some(Self { jobs, done: Mutex::new(done) }),
            Err(error) => {
                bevy::log::warn!("VM thread: {error}; frames run on the main thread");
                None
            }
        }
    }
}

pub(super) fn is_switch(vm: &Vm<Arc<Loader>>, object: ObjectId) -> bool {
    vm.store.name(vm.store.object(object).class_name).eq_ignore_ascii_case("MaterialSwitch")
}

pub(super) fn switch_material(vm: &Vm<Arc<Loader>>, switch: ObjectId) -> Option<ObjectId> {
    match vm.store.get(switch, Prop::Material) {
        Some(p2_vm::Value::Object(material)) => material,
        _ => None,
    }
}
