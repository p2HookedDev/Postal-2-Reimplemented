use std::sync::mpsc::TryRecvError;

use p2_math::{Rotator, Vector};
use super::*;
mod channel_glide;
mod hand_off;
mod sounds;
mod tick_clock;
pub(super) use channel_glide::{ChannelFrom, ChannelTime, glided_time};
pub(super) use sounds::{LoadedSound, loaded_sound, opens};
pub(super) use tick_clock::{Glide, TickClock};

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
    /// Per actor still on its way: the place it showed at when the newest tick came and its place at the tick before.
    /// It is drawn through both to the newest tick's place, so the world moves at the frame rate while ticks are
    /// slower than frames.
    pub(super) gliding: FxHashMap<ObjectId, [(Vector, Rotator); 2]>,
    view_from: Option<[(Vector, Rotator); 2]>,
    /// Per posed actor and channel: the animation (its address) and the sequence at the tick before the newest, the
    /// time shown when the newest tick came and that tick's time.
    pose_from: FxHashMap<ObjectId, Vec<ChannelFrom>>,
    ragdoll_from: FxHashMap<ObjectId, [Vec<p2_assets::pose::Frame>; 2]>,
    clock: TickClock,
    glide: Glide,
    pub(super) attached: FxHashSet<ObjectId>,
    pub(super) canvas_models: FxHashMap<ObjectId, CanvasModel>,
    pub(super) origin: Vec3,
    pub(super) followed: FxHashSet<ObjectId>,
    pub(super) visible: FxHashSet<ObjectId>,
    pub(super) watched: FxHashSet<ObjectId>,
    pub(crate) wants_forces: bool,
    pub(crate) finished: Vec<ObjectId>,
    pub(crate) movie_status: Vec<MovieStatus>,
    pub(super) seconds: f32,
    /// A frame of the game was presented in this update.
    pub(super) fresh: bool,
}

/// A move longer than this in one tick is a jump (a teleport, a camera cut), shown at once.
const GLIDE_REACH: f32 = 600.0;

pub(super) fn glided(from: (Vector, Rotator), to: (Vector, Rotator), t: f32) -> (Vector, Rotator) {
    let turn = |from: i32, to: i32| from.wrapping_add((shortest_turn(from, to) as f32 * t) as i32);
    let rotation = Rotator { pitch: turn(from.1.pitch, to.1.pitch), yaw: turn(from.1.yaw, to.1.yaw), roll: turn(from.1.roll, to.1.roll) };
    (from.0.lerp(to.0, t), rotation)
}

fn channels(pose: &ActorPose) -> impl Iterator<Item = &ChannelPose> {
    std::iter::once(&pose.base).chain(pose.blends.iter().map(|(channel, ..)| channel))
}

/// Each axis keeps its length (a scaled mesh's bones are not unit) while it turns.
pub(super) fn glided_bones(from: &[p2_assets::pose::Frame], to: &[p2_assets::pose::Frame], t: f32) -> Vec<p2_assets::pose::Frame> {
    if from.len() != to.len() {
        return to.to_vec();
    }
    let mix = |a: [f32; 3], b: [f32; 3]| [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t);
    let length = |v: [f32; 3]| v.iter().map(|c| c * c).sum::<f32>().sqrt();
    let turned = |a: [f32; 3], b: [f32; 3]| {
        let between = mix(a, b);
        let (wanted, has) = (length(a) + (length(b) - length(a)) * t, length(between));
        if has > 0.0 { between.map(|c| c * wanted / has) } else { b }
    };
    from.iter().zip(to).map(|(a, b)| p2_assets::pose::Frame { axes: [0, 1, 2].map(|i| turned(a.axes[i], b.axes[i])), origin: mix(a.origin, b.origin) }).collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum VmWait {
    Look,
    UntilBack,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobThread {
    Main,
    Worker,
}

impl PlayWorld {
    pub fn new(
        vm: Vm<Arc<Loader>>,
        loader: Arc<Loader>,
        center: Vec3,
        player: Option<(ObjectId, Option<ObjectId>)>,
        zone_actors: Vec<Option<ObjectId>>,
        music_dir: PathBuf,
    ) -> Self {
        let (player, player_pawn) = (player.map(|(controller, _)| controller), player.and_then(|(_, pawn)| pawn));
        let seen = vm.object_ids().count();
        let lights = vm.object_ids().filter(|&actor| crate::actors::mesh_light(&vm, actor).is_some_and(|light| light.dynamic)).collect();
        let fluid_surfaces = vm.actors().filter(|&actor| vm.store.is_a(actor, "FluidSurfaceInfo") && !vm.store.is_deleted(actor)).collect();
        let corona_lights = vm.object_ids().filter(|&actor| crate::map::is_corona(&vm, actor)).collect();
        let volumes = vm.actors().filter(|&actor| vm.store.is_a(actor, Kind::Volume) && !vm.store.is_deleted(actor)).collect();
        let switches = vm
            .object_ids()
            .filter(|&object| is_switch(&vm, object))
            .map(|object| (object, vm.store.object(object).class.map(|_| switch_material(&vm, object))))
            .collect();
        let sim = Simulation {
            vm,
            player,
            player_pawn,
            view: None,
            seen,
            recent: Default::default(),
            recent_read: Vec::new(),
            logged: FxHashSet::default(),
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
            mark: 0,
            time_dilation: 0,
            reads: [0; 3],
            reads_by_class: FxHashMap::default(),
            lower_names: FxHashMap::default(),
            settings_sent: false,
            zone_actors,
            zone_fogs_sent: Vec::new(),
            music_dir,
            zone_songs: FxHashMap::default(),
            zone_keys: Vec::new(),
            ambient_paths: FxHashMap::default(),
            sound_paths: FxHashSet::default(),
            work_since: None,
            work_second: Default::default(),
            census_at: None,
            hud_drawn: Vec::new(),
            hud_kept: Arc::default(),
            hud_view: None,
            hud_canvas: [0.0; 2],
            hud_age: 0.0,
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
            gliding: FxHashMap::default(),
            view_from: None,
            pose_from: FxHashMap::default(),
            ragdoll_from: FxHashMap::default(),
            clock: TickClock::default(),
            glide: Glide { to_mid: 1.0, to_newest: 1.0 },
            attached: FxHashSet::default(),
            canvas_models: FxHashMap::default(),
            origin: -center,
            followed: FxHashSet::default(),
            wants_forces: false,
            finished: Vec::new(),
            movie_status: Vec::new(),
            visible: FxHashSet::default(),
            watched: FxHashSet::default(),
            seconds: 0.0,
            fresh: false,
        }
    }

    /// Whether the VM is back from its job. A frame only looks: while the job runs the host draws frames of its own.
    pub(super) fn wait(&mut self, wait: VmWait) -> bool {
        if !matches!(self.sim, Slot::Away) {
            return true;
        }
        let received = match self.worker.as_ref().and_then(|worker| worker.done.lock().ok()) {
            None => Err(TryRecvError::Disconnected),
            Some(done) => match wait {
                VmWait::UntilBack => done.recv().map_err(|_| TryRecvError::Disconnected),
                VmWait::Look => done.try_recv(),
            },
        };
        match received {
            Ok((sim, frame)) => {
                self.sim = Slot::Here(sim);
                self.ready = Some(frame);
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                bevy::log::error!("the VM's thread died; the world stops");
                self.sim = Slot::Lost;
                self.worker = None;
                true
            }
        }
    }

    pub(super) fn start(&mut self, job: Job, thread: JobThread) {
        let threaded = thread == JobThread::Worker;
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
        let (glide, actors) = (self.glide, &self.actors);
        self.gliding.retain(|actor, [from, mid]| {
            let on_its_way = actors.get(actor).filter(|_| glide.to_newest < 1.0);
            on_its_way.is_some_and(|to| {
                let to = (to.location, to.rotation);
                *from = glide.pick(*from, *mid, to, glided);
                *mid = to;
                *from != to
            })
        });
        let shown = self.frame.view.map(|view| (self.shown_view((view.location, view.rotation)), (view.location, view.rotation)));
        self.view_from = shown.zip(frame.view).filter(|((_, mid), to)| (to.location - mid.0).size() <= GLIDE_REACH).map(|((from, mid), _)| [from, mid]);
        self.clock.tick(frame.step);
        self.glide = self.clock.glide();
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
        if frame.zone_fogs.is_none() {
            frame.zone_fogs = old.zone_fogs.take();
        }
        if frame.set_mouse.is_none() {
            frame.set_mouse = old.set_mouse.take();
        }
        for (actor, now) in frame.actor_changes.drain(..) {
            self.changed.insert(actor);
            if let Some(before) = self.actors.get(&actor) {
                if now.gone || (now.location - before.location).size() > GLIDE_REACH {
                    self.gliding.remove(&actor);
                } else if (before.location, before.rotation) != (now.location, now.rotation) {
                    self.gliding.entry(actor).or_insert([(before.location, before.rotation); 2]);
                }
            }
            if now.attachment.is_some() {
                self.attached.insert(actor);
            } else {
                self.attached.remove(&actor);
            }
            self.actors.insert(actor, now);
        }
        let before = std::mem::take(&mut self.pose_from);
        self.pose_from = old
            .poses
            .iter()
            .filter_map(|(&actor, pose)| {
                let pose = pose.as_ref()?;
                frame.poses.get(&actor)?.as_ref()?;
                let from = before.get(&actor);
                let shown = channels(pose).enumerate().map(|(index, channel)| ChannelFrom::new(from.and_then(|from| from.get(index)), ChannelTime::of(channel), glide));
                Some((actor, shown.collect()))
            })
            .collect();
        let before = std::mem::take(&mut self.ragdoll_from);
        self.ragdoll_from = old
            .ragdolls
            .iter()
            .filter(|(actor, bones)| frame.ragdolls.get(*actor).is_some_and(|now| now.len() == bones.len()))
            .map(|(&actor, bones)| {
                let shown = before.get(&actor).map_or_else(|| bones.clone(), |[from, mid]| glide.pick(&from[..], &mid[..], &bones[..], glided_bones));
                (actor, [shown, bones.clone()])
            })
            .collect();
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
        self.wait(VmWait::UntilBack);
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

    pub(super) fn set_glide(&mut self, real_delta: f32) {
        self.glide = self.clock.frame(real_delta);
    }

    pub fn glides(&self, actor: ObjectId) -> bool {
        self.gliding.contains_key(&actor)
    }

    pub fn shown_place(&self, actor: ObjectId, now: &ActorFrame) -> (Vector, Rotator) {
        let to = (now.location, now.rotation);
        self.gliding.get(&actor).map_or(to, |&[from, mid]| self.glide.pick(from, mid, to, glided))
    }

    pub(super) fn shown_view(&self, to: (Vector, Rotator)) -> (Vector, Rotator) {
        self.view_from.map_or(to, |[from, mid]| self.glide.pick(from, mid, to, glided))
    }

    pub fn attached(&self, actor: ObjectId) -> bool {
        self.attached.contains(&actor)
    }

    /// `Some(None)` if it does not animate, `None` if not read (no camera saw it).
    pub fn pose(&self, actor: ObjectId) -> Option<Option<&ActorPose>> {
        self.frame.poses.get(&actor).map(Option::as_ref)
    }

    pub fn shown_ragdoll(&self, actor: ObjectId) -> Option<std::borrow::Cow<'_, [p2_assets::pose::Frame]>> {
        let to = self.frame.ragdolls.get(&actor)?;
        let from = self.ragdoll_from.get(&actor).filter(|_| self.glide.to_newest < 1.0);
        Some(from.map_or(to.into(), |[from, mid]| self.glide.pick(&from[..], &mid[..], &to[..], glided_bones).into()))
    }

    /// The time to show channel `index` of the actor's pose at: on its way from the last tick's while both play the
    /// same sequence forward, across a loop's wrap too.
    pub fn shown_time(&self, actor: ObjectId, index: usize, channel: &ChannelPose) -> f32 {
        glided_time(self.pose_from.get(&actor).and_then(|from| from.get(index)), ChannelTime::of(channel), self.glide)
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
