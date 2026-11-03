use p2_math::Vector;
use super::*;

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
        Self { step, view: None, zone_sounds: None, shows_mouse: false, travel: None, fov: None, overlay: None, quit: false, window: None, settings: None, set_mouse: None, ..self }
    }
}

pub(super) struct Job {
    pub(super) frame: VmFrame,
    pub(super) step: f32,
    pub(super) playing: bool,
    pub(super) held: Vec<&'static str>,
    pub(super) mouse: [f32; 2],
    pub(super) typed: String,
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

impl Simulation {
    fn pass_switches(vm: &Vm<Arc<Loader>>, switches: &mut Vec<(ObjectId, Option<Option<ObjectId>>)>, new: &[ObjectId], frame: &mut VmFrame) {
        switches.extend(new.iter().filter(|&&object| is_switch(vm, object)).map(|&object| (object, None)));
        for (switch, last) in switches {
            if vm.object(*switch).class.is_none() {
                continue;
            }
            let now = switch_material(vm, *switch);
            if *last != Some(now) {
                *last = Some(now);
                frame.material_switches.push((vm.object(*switch).path.clone(), now.map(|material| vm.object(material).path.clone())));
            }
        }
    }

    fn set_vm_profile(&mut self, profile: VmProfile) {
        if profile == VmProfile::TickByClass {
            self.vm.diagnostics.tick_profile.get_or_insert_default();
        }
        if profile == VmProfile::Work {
            if self.work_since.is_none() {
                self.vm.diagnostics.count_work = true;
                self.vm.profile_functions(true);
                self.work_since = Some((Instant::now(), 0));
            }
        } else if self.work_since.take().is_some() {
            self.vm.diagnostics.count_work = false;
            self.vm.profile_functions(false);
            self.work_second = WorkSecond::default();
        }
    }

    pub(super) fn run(&mut self, mut job: Job) -> VmFrame {
        let mut frame = std::mem::take(&mut job.frame).recycle(job.step);
        let playing = job.playing && self.player_pawn.is_some();
        self.vm.options.player_physics = playing;
        self.set_vm_profile(job.vm_profile);
        let started = Instant::now();
        for &MovieStatus { movie, width, height, playing } in &job.movie_status {
            self.vm.set_movie_status(movie, width, height, playing);
        }
        for &actor in &job.finished {
            if let Err(error) = self.vm.destroy(actor) {
                bevy::log::warn!("{actor:?}: {error}");
            }
        }
        if playing && let Err(error) = self.vm.player_input(&job.held, job.mouse) {
            bevy::log::warn!("input: {error}");
        }
        let errors = self.vm.tick_level(job.step);
        frame.times.push(("vm tick", started.elapsed()));
        for (actor, error) in errors.iter().take(3) {
            bevy::log::warn!("{actor}: {error}");
        }
        self.vm.diagnostics.log.clear();
        let started = Instant::now();
        // Played, the player's controller decides the view (`PlayerCalcView`: first person, a Matinee camera, ...),
        // before the actors are read: what is drawn depends on it.
        if job.playing {
            match self.vm.player_view() {
                Ok(Some(view)) => {
                    self.view = Some(view);
                    frame.view = Some(view);
                }
                Ok(None) => {}
                Err(error) => bevy::log::warn!("view: {error}"),
            }
        }
        let commands: Vec<_> = self.vm.take_commands(|command| matches!(command, HostCommand::Sound(_) | HostCommand::Music(_))).collect();
        let at = |actor: Option<ObjectId>| actor.map(|actor| self.vm.location(actor));
        for command in commands {
            match command {
                HostCommand::Sound(played) => {
                    let position = at(played.actor);
                    frame.sounds.push((played, position));
                }
                HostCommand::Music(command) => {
                    let (actor, song) = match &command {
                        MusicCommand::Play { actor, song, .. } => (*actor, read_song(&self.music_dir, song)),
                        _ => (None, None),
                    };
                    frame.music.push((command, at(actor), song));
                }
                _ => {}
            }
        }
        // Actors a camera saw last frame are marked rendered, or POSTAL 2's pawns go into stasis.
        for &actor in &job.visible {
            if let Err(error) = self.vm.mark_rendered(actor) {
                bevy::log::warn!("{actor:?}: {error}");
            }
        }
        for &(actor, BoneFrame { location, axes }) in &job.placed {
            self.vm.place_attachment(actor, location, axes.map(Into::into));
        }
        frame.times.push(("vm view", started.elapsed()));
        let started = Instant::now();
        let followed = &job.followed;
        let mut recent = std::mem::take(&mut self.recent_read);
        recent.clear();
        recent.extend(self.recent.iter().flatten().copied().filter(|actor| !followed.contains(actor)));
        for &actor in followed.iter().chain(&recent) {
            self.pass_actor(actor, &mut frame.actor_changes, &mut frame.spawned);
        }
        frame.times.push(("vm read actors", started.elapsed()));
        let started = Instant::now();
        for &actor in followed {
            if job.visible.contains(&actor) || !self.posed.contains(&actor) {
                self.read_pose(actor, &mut frame);
            }
        }
        for &actor in &recent {
            self.read_pose(actor, &mut frame);
        }
        self.recent_read = recent;
        frame.times.push(("vm read poses", started.elapsed()));
        // The client's input (menus, console), then the HUD; a map change goes first, as retail loads the
        // map before it draws again.
        self.vm.set_video_modes(crate::display::video_modes());
        if let Some([x, y]) = job.cursor {
            self.vm.set_mouse_position(x, y);
        }
        frame.shows_mouse = self.vm.shows_mouse();
        if !job.typed.is_empty()
            && let Err(error) = self.vm.typed(&job.typed)
        {
            bevy::log::warn!("typed: {error}");
        }
        frame.travel = self
            .vm
            .take_commands(|command| matches!(command, HostCommand::Travel(_)))
            .filter_map(|command| if let HostCommand::Travel(url) = command { Some(url) } else { None })
            .last();
        if frame.travel.is_none() {
            let started = Instant::now();
            frame.hud = self.hud(job.canvas).into();
            frame.times.push(("hud scripts", started.elapsed()));
        }
        let started = Instant::now();
        self.read_after_hud(&mut frame, &job.watched, job.forces);
        frame.times.push(("vm read after hud", started.elapsed()));
        frame.profile = self.vm.diagnostics.tick_profile.take().unwrap_or_default();
        let vm_time = frame.times.iter().map(|(_, time)| *time).sum();
        self.log_work(vm_time);
        frame.job_buffers = JobBuffers { held: job.held, followed: job.followed, visible: job.visible, placed: job.placed, watched: job.watched };
        frame
    }

    fn read_pose(&mut self, actor: ObjectId, frame: &mut VmFrame) {
        let pose = self.actor_pose(actor);
        frame.poses.insert(actor, pose);
        if let Some(bones) = self.vm.ragdoll_pose(actor) {
            frame.ragdolls.insert(actor, bones);
        }
        self.posed.insert(actor);
    }

    pub(super) fn log_work(&mut self, vm_time: Duration) {
        let Some((since, frames)) = &mut self.work_since else { return };
        *frames += 1;
        let (since, frames) = (*since, *frames);
        let counts = self.vm.take_work_counts();
        let functions = self.vm.take_function_profile();
        let second = &mut self.work_second;
        if second.counts.is_empty() {
            second.counts = counts.iter().map(|&(name, _)| (name, 0)).collect();
        }
        for (sum, (_, n)) in second.counts.iter_mut().zip(&counts) {
            sum.1 += n;
        }
        for (name, time) in &functions {
            let sum = second.functions.entry(name.clone()).or_default();
            (sum.calls, sum.total, sum.own, sum.native) = (sum.calls + time.calls, sum.total + time.total, sum.own + time.own, time.native);
        }
        if second.slowest.as_ref().is_none_or(|(slowest, ..)| vm_time > *slowest) {
            second.slowest = Some((vm_time, counts, functions));
        }
        if since.elapsed() < Duration::from_secs(1) {
            return;
        }
        let per = f64::from(frames);
        self.work_since = Some((Instant::now(), 0));
        let second = std::mem::take(&mut self.work_second);
        let work = |counts: &[(&str, u64)], per: f64| -> String {
            counts.iter().filter(|(_, n)| *n > 0).map(|(name, n)| format!("{name} {:.0}", *n as f64 / per)).collect::<Vec<_>>().join(", ")
        };
        let top = |functions: &mut Vec<(String, p2_vm::FunctionTime)>, per: f64| -> String {
            functions.sort_by_key(|(_, time)| std::cmp::Reverse(time.own));
            let ms = |time: Duration| time.as_secs_f64() * 1000.0 / per;
            functions
                .iter()
                .take(20)
                .map(|(name, time)| format!("{name} {:.3} ({:.0} calls, {:.3} total)", ms(time.own), time.calls as f64 / per, ms(time.total)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        bevy::log::info!("vm work per frame: {}", work(&second.counts, per));
        let (placed, always, stale) = self.vm.collision_hash_census();
        bevy::log::info!("vm collision hash: {placed} actors, {always} always candidates, {stale} stale");
        let names: Vec<String> = self.vm.take_property_names().iter().take(15).map(|(name, n)| format!("{name} {:.0}", *n as f64 / per)).collect();
        bevy::log::info!("vm properties by name per frame: {}", names.join(", "));
        let mut functions: Vec<(String, p2_vm::FunctionTime)> = second.functions.into_iter().collect();
        bevy::log::info!("vm functions by own time (ms per frame): {}", top(&mut functions, per));
        if let Some((time, counts, mut functions)) = second.slowest {
            bevy::log::info!(
                "slowest vm frame {:.2} ms: work: {}; functions by own time (ms): {}",
                time.as_secs_f64() * 1000.0,
                work(&counts, 1.0),
                top(&mut functions, 1.0)
            );
        }
    }

    fn zone_sounds(&mut self, listener: Vector) -> Option<Vec<ZoneSound>> {
        let vm = &mut self.vm;
        let mut sources: Vec<ObjectId> = vm.zone_at(listener).into_iter().collect();
        sources.extend(self.volumes.iter().copied().filter(|&volume| matches!(vm.get(volume, Prop::ZoneSounds), Some(p2_vm::Value::Array(ref entries)) if !entries.is_empty()) && vm.encompasses(volume, listener)));
        let (sound_volume, music_volume) = (vm.sound_volume(), vm.music_volume());
        let mut heard: Vec<(ZoneSound, Option<ObjectId>)> = Vec::new();
        for source in sources {
            let Some(p2_vm::Value::Array(entries)) = vm.get(source, Prop::ZoneSounds) else { continue };
            let pitch = match vm.get(source, Prop::SoundPitch) {
                Some(p2_vm::Value::Byte(pitch)) => pitch_factor(f32::from(pitch) / 64.0),
                _ => 1.0,
            };
            for (index, entry) in entries.iter().enumerate() {
                let p2_vm::Value::Struct(fields) = entry else { continue };
                let sound = match fields.first() {
                    Some(p2_vm::Value::Object(Some(sound))) => Some(*sound),
                    _ => None,
                };
                let song_name = match fields.get(1) {
                    Some(p2_vm::Value::Str(song)) if sound.is_none() && !song.is_empty() => Some(song),
                    _ => None,
                };
                let entry_volume = match fields.get(2) {
                    Some(p2_vm::Value::Float(volume)) => *volume,
                    _ => 0.0,
                };
                let song = song_name.and_then(|name| match self.zone_songs.get(name) {
                    Some(song) => song.clone(),
                    None => {
                        let song = read_song(&self.music_dir, name);
                        self.zone_songs.insert(name.clone(), song.clone());
                        song
                    }
                });
                if sound.is_none() && song.is_none() {
                    continue;
                }
                let volume = if song.is_some() { music_volume } else { entry_volume * sound_volume };
                let float = |n: usize| match fields.get(n) {
                    Some(p2_vm::Value::Float(value)) => *value,
                    _ => 0.0,
                };
                heard.push((ZoneSound { key: (source, index), sound: None, song, volume: volume.clamp(0.0, 1.0), pitch, fade_in: float(3), fade_out: float(4) }, sound));
            }
        }
        if heard.iter().map(|(zone_sound, _)| zone_sound.key).eq(self.zone_keys.iter().copied()) {
            return None;
        }
        self.zone_keys.clear();
        self.zone_keys.extend(heard.iter().map(|(zone_sound, _)| zone_sound.key));
        let vm = &self.vm;
        Some(heard.into_iter().map(|(zone_sound, sound)| ZoneSound { sound: sound.map(|sound| vm.object(sound).path.clone()), ..zone_sound }).collect())
    }

    fn send_sound_files(&mut self, frame: &mut VmFrame, ambient_paths: &[Arc<str>]) {
        let mut named: Vec<String> = Vec::new();
        let paths = frame
            .sounds
            .iter()
            .map(|(played, _)| played.sound.as_str())
            .chain(ambient_paths.iter().map(|path| &**path))
            .chain(frame.zone_sounds.iter().flatten().filter_map(|zone_sound| zone_sound.sound.as_deref()));
        for path in paths {
            if !self.sound_paths.contains(path) {
                self.sound_paths.insert(path.to_owned());
                named.push(path.to_owned());
            }
        }
        for path in named {
            let file = self.vm.sound_file(&path);
            frame.sound_files.push((path, file));
        }
    }

    fn animate_dynamic_lights(&mut self, frame: &mut VmFrame) {
        let vm = &self.vm;
        let now = level_time(vm);
        let clock = &mut self.light_clock;
        frame.dynamic_lights.extend(
            self.lights
                .iter()
                .filter(|&&actor| vm.get(actor, Prop::bDynamicLight) == Some(p2_vm::Value::Bool(true)))
                .filter_map(|&actor| crate::actors::mesh_light(vm, actor))
                .map(|light| clock.animate(light, now)),
        );
    }

    pub(super) fn read_after_hud(&mut self, frame: &mut VmFrame, watched: &[ObjectId], forces: bool) {
        self.read_hud_actors(frame);
        let new: Vec<ObjectId> = self.vm.object_ids().skip(self.seen).collect();
        self.seen += new.len();
        self.read_spawned(frame, &new);
        for index in 0..self.hud_drawn.len() {
            let actor = self.hud_drawn[index];
            self.pass_actor(actor, &mut frame.actor_changes, &mut frame.spawned);
        }
        for &actor in watched.iter().chain(&new) {
            self.pass_actor(actor, &mut frame.actor_changes, &mut frame.spawned);
        }
        self.update_tracked_lists(frame, &new);
        self.animate_dynamic_lights(frame);
        self.recent.swap(0, 1);
        self.recent[0] = new;
        self.take_host_commands(frame);
        let vm = &self.vm;
        frame.emitter_changes = self.emitter_values.changes(vm, watched);
        if forces {
            frame.forces.extend(vm.drag_actors().into_iter().map(|(location, velocity, radius, scale)| ActorForce { location, velocity, radius, scale }));
        }
        frame.fov = self.player.and_then(|player| match self.vm.get(player, Prop::FovAngle) {
            Some(p2_vm::Value::Float(fov)) if fov > 0.0 => Some(fov),
            _ => None,
        });
        frame.overlay = self.screen_overlay();
        let vm = &self.vm;
        frame.movies.extend(vm.movie_textures().filter_map(|(texture, movie)| Some((vm.object(texture).path.clone(), movie, vm.movie(movie)?.clone()))));
        frame.paused = vm.paused();
        frame.quit = self.quit;
        self.read_audio(frame);
        self.read_settings(frame);
    }

    fn read_hud_actors(&mut self, frame: &mut VmFrame) {
        self.hud_drawn.clear();
        self.hud_drawn.extend(frame.hud.iter().filter_map(|item| if let CanvasItem::Actor { actor, .. } = item { Some(*actor) } else { None }));
        let drawn = &self.hud_drawn;
        self.hud_looks.retain(|actor, _| drawn.contains(actor));
        for index in 0..self.hud_drawn.len() {
            let actor = self.hud_drawn[index];
            let look = actor_look(&self.vm, actor);
            if self.hud_looks.insert(actor, look) != Some(look) {
                let model = self.canvas_model(actor);
                frame.canvas_models.push((actor, model));
            }
            // Posed as drawn (retail poses a mesh when it renders it): one no camera saw last frame (the foot before a
            // kick) was not posed above.
            if let std::collections::hash_map::Entry::Vacant(entry) = frame.poses.entry(actor) {
                entry.insert(self.actor_pose(actor));
                self.posed.insert(actor);
            }
        }
    }

    fn canvas_model(&mut self, actor: ObjectId) -> CanvasModel {
        if let Some(model) = mesh_actor(&self.vm, actor) {
            return CanvasModel::Static(model);
        }
        if let Some(mesh) = self.actor_mesh(actor) {
            return CanvasModel::Skeletal(mesh);
        }
        if self.unmeshed.insert(actor) {
            let vm = &self.vm;
            let path = |property: Prop| match vm.get(actor, property) {
                Some(p2_vm::Value::Object(Some(object))) => vm.object(object).path.clone(),
                _ => "None".to_owned(),
            };
            bevy::log::warn!(
                "HUD actor {} ({actor:?}) draws neither a static nor a skeletal mesh: DrawType {:?}, Mesh {}, StaticMesh {}",
                vm.object(actor).path,
                vm.get(actor, Prop::DrawType),
                path(Prop::Mesh),
                path(Prop::StaticMesh)
            );
        }
        CanvasModel::Nothing
    }

    fn read_spawned(&mut self, frame: &mut VmFrame, new: &[ObjectId]) {
        for &actor in new {
            let vm = &self.vm;
            if vm.is_a(actor, Kind::Emitter) && (crate::actors::emitter_objects(vm, actor).is_empty() || vm.is_deleted(actor)) {
                let class = vm.object(actor).class.map_or_else(String::new, |class| vm.name(vm.class(class).name).to_owned());
                if self.emitterless.insert(class.clone()) {
                    bevy::log::warn!("spawned {class} ({actor:?}) has no particle emitter to simulate: deleted {}, Emitters {:?}", vm.is_deleted(actor), vm.get(actor, Prop::Emitters));
                }
            }
        }
        frame.spawned.extend(
            new.iter()
                .filter(|&&actor| self.vm.is_a(actor, Kind::Actor))
                .map(|&actor| Spawned {
                    actor,
                    mesh: mesh_actor(&self.vm, actor),
                    skeletal: skeletal_actor(&self.vm, actor),
                    emitter: emitter_actor(&self.vm, actor),
                    sprite: crate::actors::sprite_actor(&self.vm, actor),
                    corona: crate::map::corona_actor(&self.vm, actor),
                    restyled: false,
                })
                .filter(|s| s.mesh.is_some() || s.skeletal.is_some() || s.emitter.is_some() || s.sprite.is_some() || s.corona.is_some()),
        );
    }

    fn update_tracked_lists(&mut self, frame: &mut VmFrame, new: &[ObjectId]) {
        let vm = &self.vm;
        let destroyed = |actor: ObjectId| vm.is_deleted(actor);
        track(
            &mut self.lights,
            new,
            |actor| vm.get(actor, Prop::bDynamicLight) == Some(p2_vm::Value::Bool(true)) || crate::actors::mesh_light(vm, actor).is_some(),
            destroyed,
        );
        track(&mut self.volumes, new, |actor| vm.is_a(actor, Kind::Volume), destroyed);
        track(&mut self.corona_lights, new, |actor| crate::map::is_corona(vm, actor), destroyed);
        Self::pass_switches(vm, &mut self.switches, new, frame);
        fluid_touchers(vm, &self.fluid_surfaces, &mut frame.fluid_touchers);
        // Retail's corona line, traced by the VM's level check so static meshes count as in retail.
        // Outside the view or in an unseen zone it fades as if blocked, with no trace.
        if let Some(view) = self.view {
            let (eye, forward) = (view.location, view.rotation.vector());
            let fov = self.player.and_then(|player| match self.vm.get(player, Prop::FovAngle) {
                Some(p2_vm::Value::Float(fov)) if fov > 0.0 => Some(fov),
                _ => None,
            });
            let reach = (fov.unwrap_or(90.0).clamp(1.0, 170.0).to_radians() * 0.5).tan() * CORONA_CONE_WIDTH;
            let seen_zones = self.vm.zones_seen_from(eye);
            for &light in &self.corona_lights {
                let at = self.vm.location(light);
                let in_view = view.behind || in_view_cone(at - eye, forward, reach);
                let zone_seen = seen_zones & (1u64 << (self.vm.zone_number(at) & 63)) != 0;
                if !in_view || !zone_seen || self.vm.corona_blocked(eye, at) {
                    frame.corona_blocked.push(light);
                }
            }
        }
    }

    fn take_host_commands(&mut self, frame: &mut VmFrame) {
        let commands: Vec<_> = self
            .vm
            .take_commands(|command| {
                matches!(
                    command,
                    HostCommand::Projector(_) | HostCommand::Particle(_) | HostCommand::Window(_) | HostCommand::Quit | HostCommand::SetMouse(_) | HostCommand::SaveGame { .. }
                )
            })
            .collect();
        frame.plings.append(&mut self.vm.actor_fx.plings);
        frame.projector_actors = self.vm.projector_actors();
        let vm = &self.vm;
        for command in commands {
            match command {
                HostCommand::Projector(event) => frame.projections.push(match event {
                    ProjectorEvent::Attach(actor) => (event, projector_actor(vm, actor)),
                    ProjectorEvent::Detach(_) => (event, None),
                }),
                HostCommand::Particle(command) => frame.particles.push(command.with_properties(|emitter| emitter_properties(vm, emitter))),
                HostCommand::Window(request) => frame.window = Some(request),
                HostCommand::Quit => self.quit = true,
                HostCommand::SetMouse([x, y]) => frame.set_mouse = Some([x as f32, y as f32]),
                HostCommand::SaveGame { slot, save } => crate::saves::write(slot, save),
                _ => {}
            }
        }
    }

    fn read_audio(&mut self, frame: &mut VmFrame) {
        let (ambient_volume, sound_volume) = (self.vm.ambient_volume(), self.vm.sound_volume());
        let mut new_paths: Vec<Arc<str>> = Vec::new();
        let (vm, ambient_paths) = (&self.vm, &mut self.ambient_paths);
        for actor in vm.actors() {
            let Some(ambient) = crate::actors::ambient_actor(vm, actor) else { continue };
            let path = ambient_paths.entry(ambient.sound).or_insert_with(|| {
                let path: Arc<str> = Arc::from(vm.object(ambient.sound).path.as_str());
                new_paths.push(Arc::clone(&path));
                path
            });
            let path = Arc::clone(path);
            let volume = ambient_gain(ambient.volume, ambient_volume, sound_volume);
            frame.ambients.push((actor, crate::actors::AmbientActor { volume, ..ambient }, path));
        }
        frame.zone_sounds = self.view.and_then(|view| self.zone_sounds(view.location));
        self.send_sound_files(frame, &new_paths);
        frame.audio_channels = self.vm.audio_channels();
    }

    fn read_settings(&mut self, frame: &mut VmFrame) {
        let changed = self.vm.take_changed_settings();
        if let Some(settings) = &changed {
            crate::display::save_settings(settings);
        }
        if changed.is_some() || !std::mem::replace(&mut self.settings_sent, true) {
            let vm = &mut self.vm;
            frame.settings = Some(crate::display::Settings::read(|query| vm.get_command(query).unwrap_or_default()));
        }
    }

    pub(super) fn pass_actor(&mut self, actor: ObjectId, changes: &mut Vec<(ObjectId, ActorFrame)>, restyled: &mut Vec<Spawned>) {
        let now = self.actor_frame(actor);
        match self.sent.get_mut(&actor) {
            Some(sent) if *sent == now => {}
            Some(sent) => {
                if sent.look != now.look {
                    let vm = &self.vm;
                    let (mesh, skeletal) = (mesh_actor(vm, actor), skeletal_actor(vm, actor));
                    restyled.push(Spawned { actor, mesh, skeletal, emitter: None, sprite: crate::actors::sprite_actor(vm, actor), corona: None, restyled: true });
                }
                sent.clone_from(&now);
                changes.push((actor, now));
            }
            None => {
                self.sent.insert(actor, now.clone());
                changes.push((actor, now));
            }
        }
    }

    pub(super) fn actor_frame(&mut self, actor: ObjectId) -> ActorFrame {
        let vm = &self.vm;
        let (location, rotation) = location_rotation(vm, actor);
        ActorFrame {
            location,
            rotation,
            scale: actor_scale(vm, actor),
            velocity: vm.velocity(actor),
            time_scale: vm.particle_time_scale(actor),
            gone: vm.is_deleted(actor),
            hidden: vm.filtered_from_view(actor, self.view.as_ref(), &self.hud_drawn),
            attachment: bone_attachment(vm, actor, &mut self.lower_names),
            emitters_disabled: emitters_disabled(vm, actor),
            look: actor_look(vm, actor),
            glow: actor_glow(vm, actor),
        }
    }

    pub(super) fn hud(&mut self, [width, height]: [f32; 2]) -> Vec<CanvasItem> {
        let Some(controller) = self.player else { return Vec::new() };
        self.vm.render_hud(controller, width, height).unwrap_or_else(|error| {
            bevy::log::warn!("HUD: {error}");
            Vec::new()
        })
    }

    pub(super) fn actor_mesh(&self, actor: ObjectId) -> Option<CanvasMesh> {
        let vm = &self.vm;
        let path = object_path(vm, vm.get(actor, Prop::Mesh).as_ref())?;
        let skins = skin_paths(vm, actor);
        let scale = actor_scale(vm, actor);
        let follow = FollowActor { actor, scale, pre_pivot: vector(vm, actor, Prop::PrePivot), skeletal: None };
        Some(CanvasMesh { path, skins, follow })
    }

    pub(super) fn screen_overlay(&self) -> Option<Overlay> {
        let overlay = self.vm.screen_overlay(self.vm.player()?);
        let titles = overlay
            .titles
            .iter()
            .filter_map(|t| Some(OverlayTitle { material: self.vm.object(t.material?).path.clone(), color: t.color, size: t.size, at: [t.x, t.y], alpha: t.alpha }))
            .collect();
        Some(Overlay { flash_scale: overlay.flash_scale, flash_fog: overlay.flash_fog, titles })
    }

    pub(super) fn actor_pose(&mut self, actor: ObjectId) -> Option<ActorPose> {
        let base = self.pose_of(actor, 0)?;
        let mut blends = Vec::new();
        for index in 1.. {
            let Some((channel, _)) = self.vm.anim_channel(actor, index) else { break };
            let (alpha, bone) = (p2_assets::pose::channel_weight(channel.blend_alpha, channel.frame, channel.blend_in_time), channel.blend_bone);
            if alpha > 0.0
                && let Some(pose) = self.pose_of(actor, index)
            {
                blends.push((pose, alpha, self.lower_name(bone)));
            }
        }
        #[expect(clippy::needless_collect, reason = "`lower_name` needs `self` mutably while the list borrows the VM")]
        let scales: Vec<_> = self.vm.bone_scales(actor).collect();
        let scales = scales.into_iter().map(|(bone, scale)| (self.lower_name(bone), scale)).collect();
        let turns = self.vm.bone_rotations(actor).to_vec();
        let turns = turns.into_iter().map(|(bone, turn)| (self.lower_name(bone), turn.into())).collect();
        Some(ActorPose { base, blends, scales, turns })
    }

    pub(super) fn lower_name(&mut self, name: p2_vm::NameId) -> Arc<str> {
        let vm = &self.vm;
        let made = self.lower_names.entry(name).or_insert_with(|| {
            if name == p2_vm::NameId::NONE { Arc::from("") } else { Arc::from(vm.name(name).to_ascii_lowercase()) }
        });
        Arc::clone(made)
    }

    pub(super) fn pose_of(&mut self, actor: ObjectId, index: usize) -> Option<ChannelPose> {
        let (channel, animation) = self.vm.anim_channel(actor, index)?;
        let (sequence, frame) = (channel.sequence, channel.frame.max(0.0));
        let tween = channel.tween().map(|(from, weight)| (Arc::clone(from), weight));
        let object = animation?;
        let vm = &mut self.vm;
        let animation = self
            .animations
            .entry(object)
            .or_insert_with(|| {
                let path = vm.object(object).path.clone();
                PlayedAnimation::load(vm, object, &path)
            })
            .clone()?;
        let index = *self.sequences.entry((object, sequence)).or_insert_with(|| animation.sequences.get(&self.vm.name(sequence).to_ascii_lowercase()).copied());
        let index = index?;
        let frames = animation.animation.sequences[index].num_frames.max(1) as f32;
        Some(ChannelPose { animation, sequence: index, time: frame * frames, tween })
    }
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

fn fluid_touchers(vm: &Vm<Arc<Loader>>, surfaces: &[ObjectId], touchers: &mut Vec<(ObjectId, Vec<p2_particles::FluidToucher>)>) {
    for &surface in surfaces {
        let Some(p2_vm::Value::Array(touching)) = vm.get(surface, Prop::Touching) else { continue };
        let disturbing: Vec<_> = touching
            .iter()
            .filter_map(|value| if let p2_vm::Value::Object(Some(actor)) = value { Some(*actor) } else { None })
            .filter(|&actor| vm.get(actor, "bDisturbFluidSurface") == Some(p2_vm::Value::Bool(true)))
            .map(|actor| p2_particles::FluidToucher {
                location: vm.location(actor),
                velocity: vm.velocity(actor),
                radius: match vm.get(actor, Prop::CollisionRadius) {
                    Some(p2_vm::Value::Float(radius)) => radius,
                    _ => 0.0,
                },
            })
            .collect();
        if !disturbing.is_empty() {
            touchers.push((surface, disturbing));
        }
    }
}

fn level_time(vm: &Vm<Arc<Loader>>) -> f32 {
    match vm.level().and_then(|level| vm.get(level, Prop::TimeSeconds)) {
        Some(p2_vm::Value::Float(time)) => time,
        _ => 0.0,
    }
}

pub(super) fn is_switch(vm: &Vm<Arc<Loader>>, object: ObjectId) -> bool {
    vm.name(vm.object(object).class_name).eq_ignore_ascii_case("MaterialSwitch")
}

pub(super) fn switch_material(vm: &Vm<Arc<Loader>>, switch: ObjectId) -> Option<ObjectId> {
    match vm.get(switch, Prop::Material) {
        Some(p2_vm::Value::Object(material)) => material,
        _ => None,
    }
}

/// How much wider than the tangent of half the field of view the cone tested for coronas is: it holds the whole frustum
/// at any screen shape up to 21:9, and a corona whose centre just left the screen is still partly on it.
const CORONA_CONE_WIDTH: f32 = 2.0;

fn in_view_cone(offset: Vector, forward: Vector, reach: f32) -> bool {
    let along = offset.dot(forward);
    along > 0.0 && (offset.size_squared() - along * along).max(0.0) <= (along * reach).powi(2)
}

fn track(list: &mut Vec<ObjectId>, new: &[ObjectId], joins: impl Fn(ObjectId) -> bool, gone: impl Fn(ObjectId) -> bool) {
    list.extend(new.iter().copied().filter(|&actor| joins(actor)));
    list.retain(|&actor| !gone(actor));
}

fn read_song(music_dir: &Path, song: &str) -> Option<Arc<[u8]>> {
    if song.is_empty() {
        return None;
    }
    let file = if Path::new(song).extension().is_some() { song.to_owned() } else { format!("{song}.ogg") };
    match std::fs::read(music_dir.join(&file)) {
        Ok(bytes) => Some(bytes.into()),
        Err(_) => {
            bevy::log::warn!("music {song} not found");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_corona_cone_holds_what_is_in_front_within_its_reach() {
        let forward = Vector::from([1.0, 0.0, 0.0]);
        assert!(in_view_cone([100.0, 150.0, 0.0].into(), forward, 2.0));
        assert!(!in_view_cone([100.0, 250.0, 0.0].into(), forward, 2.0));
        assert!(!in_view_cone([-100.0, 0.0, 0.0].into(), forward, 2.0));
    }

    #[test]
    fn tracked_list_adds_joiners_in_order_and_drops_the_gone() {
        let id = ObjectId::from_index;
        let mut list = vec![id(1), id(4)];
        track(&mut list, &[id(6), id(7), id(8)], |actor| [id(6), id(8)].contains(&actor), |actor| actor == id(4));
        assert_eq!(list, [id(1), id(6), id(8)]);
        track(&mut list, &[], |_| true, |_| false);
        assert_eq!(list, [id(1), id(6), id(8)]);
    }
}
