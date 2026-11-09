use p2_math::{Rotator, Vector};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::enums::EPhysics;
use crate::native_id::Native;
use crate::interp::{int, truthy};
use crate::natives::{float_arg, given, int_arg, object_arg, optional_float, rot3, vec3};
use crate::{Error, HostCommand, Kind, LoaderRef, NameId, ObjectId, ParticleCommand, Prop, Value, Vm};

const MAX_BONE_CONTROLS: usize = 256;
const MAX_SPAWN: i32 = 1024;
const MAX_QUEUED: usize = 256;

fn queue<T>(queue: &mut Vec<T>, item: T) {
    if queue.len() >= MAX_QUEUED {
        queue.remove(0);
    }
    queue.push(item);
}

fn control_slot<T>(list: &mut Vec<T>, matches: impl Fn(&T) -> bool, new: impl FnOnce() -> T) -> Option<&mut T> {
    let at = match list.iter().position(matches) {
        Some(at) => at,
        None if list.len() < MAX_BONE_CONTROLS => {
            list.push(new());
            list.len() - 1
        }
        None => return None,
    };
    list.get_mut(at)
}

/// `SetBoneRotation` and `SetBoneLocation` share one entry per bone; alpha 0 has no effect. State
/// only: no shipped script calls either native.
#[derive(Debug, Clone, PartialEq)]
pub struct BoneControl {
    pub bone: NameId,
    pub rotation: Rotator,
    /// `SetBoneRotation`'s `Space` (0: the engine's default, bone-local; others unverified).
    pub space: i32,
    pub rotation_alpha: f32,
    pub location: Vector,
    pub location_alpha: f32,
}

/// `SetBoneDirection` entry. State only: `AWPerson` calls it with alpha 0, and the one call with alpha
/// 1, `P2MoCapPawn.RotateTowards`, says its native caller is switched off.
#[derive(Debug, Clone, PartialEq)]
pub struct BoneDirection {
    pub bone: NameId,
    pub turn: Rotator,
    pub translation: Vector,
    pub alpha: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[expect(clippy::struct_excessive_bools, reason = "mirrors `UMovie`'s independent flags")]
pub struct MovieState {
    pub file: String,
    pub use_sound: bool,
    pub looping: bool,
    pub playing: bool,
    pub paused: bool,
    pub width: i32,
    pub height: i32,
    pub plays: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FluidPling {
    pub surface: ObjectId,
    pub position: Vector,
    pub strength: f32,
    pub radius: f32,
}

#[derive(Debug, Default)]
pub struct ActorFx {
    controls: FxHashMap<ObjectId, Vec<BoneControl>>,
    directions: FxHashMap<ObjectId, Vec<BoneDirection>>,
    projected: Vec<(ObjectId, ObjectId)>,
    projected_sent: FxHashSet<ObjectId>,
    following: FxHashMap<ObjectId, ObjectId>,
    movies: FxHashMap<ObjectId, MovieState>,
    movie_textures: FxHashMap<ObjectId, ObjectId>,
    pub plings: Vec<FluidPling>,
    com_offsets: FxHashMap<ObjectId, Vector>,
    inertia: FxHashMap<ObjectId, [Vector; 2]>,
    lods: FxHashMap<ObjectId, Vec<(f32, Option<ObjectId>)>>,
}

impl ActorFx {
    pub(crate) fn forget(&mut self, gone: &FxHashSet<ObjectId>) {
        self.controls.retain(|id, _| !gone.contains(id));
        self.directions.retain(|id, _| !gone.contains(id));
        self.projected.retain(|(projector, actor)| !gone.contains(projector) && !gone.contains(actor));
        self.following.retain(|projector, actor| !gone.contains(projector) && !gone.contains(actor));
        self.movies.retain(|id, _| !gone.contains(id));
        self.movie_textures.retain(|texture, movie| !gone.contains(texture) && !gone.contains(movie));
        self.com_offsets.retain(|id, _| !gone.contains(id));
        self.inertia.retain(|id, _| !gone.contains(id));
        self.lods.retain(|id, _| !gone.contains(id));
        self.plings.clear();
    }
}

impl<L: LoaderRef> Vm<L> {
    pub fn projector_actors(&mut self) -> Option<FxHashSet<ObjectId>> {
        let actors: FxHashSet<ObjectId> =
            self.actor_fx.projected.iter().filter(|&&(projector, actor)| !self.store.is_deleted(projector) && !self.store.is_deleted(actor)).map(|&(_, actor)| actor).collect();
        if actors == self.actor_fx.projected_sent {
            return None;
        }
        self.actor_fx.projected_sent.clone_from(&actors);
        Some(actors)
    }

    pub fn projector_follows(&self, projector: ObjectId) -> Option<ObjectId> {
        self.actor_fx.following.get(&projector).copied()
    }

    /// Our addition: retail leaves Karma props out (`bAcceptsProjectors` off) and decals where they were cast.
    pub(crate) fn moving_receiver(&self, actor: ObjectId) -> bool {
        self.store.is_a(actor, Kind::Mover) || self.store.is_a(actor, "KActor") || self.store.physics_of(actor) == Some(EPhysics::Karma)
    }

    pub(crate) fn attach_static_meshes(&mut self, projector: ObjectId) {
        if self.store.get(projector, "bProjectStaticMesh") != Some(Value::Bool(true)) {
            return;
        }
        let Some(texture) = self.store.object_of(projector, Prop::ProjTexture) else { return };
        let Some(frustum) = self.projector_frustum(projector, texture) else {
            self.diagnostics.log.push(format!("Projector texture {} has no size: it attaches to no actor", self.store.object(texture).path));
            return;
        };
        let (mut min, mut max) = ([f32::MAX; 2], [f32::MIN; 2]);
        for vertex in frustum.vertices {
            min = [min[0].min(vertex.x), min[1].min(vertex.y)];
            max = [max[0].max(vertex.x), max[1].max(vertex.y)];
        }
        let name = |value: Option<Value>| match value {
            Some(Value::Name(name)) => name,
            _ => NameId::NONE,
        };
        let project_tag = name(self.store.get(projector, "ProjectTag"));
        let nearby = self.colliders_near(min, max, 0.0);
        let attached: Vec<ObjectId> = nearby
            .iter()
            .map(|e| e.actor)
            .filter(|&actor| {
                actor != projector
                    && self.store.is_a(actor, Kind::Actor)
                    && !self.store.is_deleted(actor)
                    && attach_takes(
                        project_tag,
                        self.store.get(actor, "bAcceptsProjectors") == Some(Value::Bool(true)) || self.moving_receiver(actor),
                        name(self.store.get(actor, Prop::Tag)),
                        self.store.object_of(actor, Prop::StaticMesh).is_some(),
                    )
                    && self.shapes_overlap(projector, actor)
            })
            .collect();
        self.give_entries(nearby);
        let location = self.store.vector(projector, Prop::Location);
        let follows = attached.iter().copied().find(|&actor| {
            self.moving_receiver(actor) && {
                let (center, extent) = self.encroach_box(actor);
                on_receiver(location, center, extent)
            }
        });
        for &actor in &attached {
            if !self.actor_fx.projected.contains(&(projector, actor)) {
                self.actor_fx.projected.push((projector, actor));
            }
        }
        match follows {
            Some(receiver) => {
                self.actor_fx.following.insert(projector, receiver);
            }
            None => {
                self.actor_fx.following.remove(&projector);
            }
        }
    }

    pub(crate) fn detach_projected_actors(&mut self, projector: ObjectId) {
        self.actor_fx.projected.retain(|&(p, _)| p != projector);
        self.actor_fx.following.remove(&projector);
    }

    /// `Movie` and `TextureMovie`'s `Movie` live in the transient package. Script cannot assign `Movie`:
    /// it is `const`. `PlayMovieScaled` replaces `TextureMovie`.
    pub(crate) fn construct_hud_movies(&mut self, hud: ObjectId) -> Result<(), Error> {
        let movie_class = self.find_class("Engine", "Movie")?;
        let texture_class = self.find_class("Engine", "MovieTexture")?;
        let movie = self.new_object(movie_class, None);
        self.store.set(hud, "Movie", Value::Object(Some(movie)));
        let texture = self.new_object(texture_class, None);
        let texture_movie = self.new_object(movie_class, None);
        self.store.set(texture, "Movie", Value::Object(Some(texture_movie)));
        self.store.set(hud, "TextureMovie", Value::Object(Some(texture)));
        Ok(())
    }

    pub fn movie(&self, movie: ObjectId) -> Option<&MovieState> {
        self.actor_fx.movies.get(&movie)
    }

    pub fn set_movie_status(&mut self, movie: ObjectId, width: i32, height: i32, playing: bool) {
        let state = self.actor_fx.movies.entry(movie).or_default();
        (state.width, state.height, state.playing) = (width, height, playing);
    }

    pub fn movie_textures(&self) -> impl Iterator<Item = (ObjectId, ObjectId)> + '_ {
        self.actor_fx.movie_textures.iter().map(|(&texture, &movie)| (texture, movie))
    }

    pub(crate) fn actor_native(&mut self, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let Some(this) = this else { return Ok(None) };
        let unknown = || Error::UnknownNative(format!("{native:?}"));
        let value = match native {
            // Not modelled: a controller's pawn joining with it, and retail doing nothing while the projector
            // has no projection yet.
            Native::AttachActor if self.store.is_a(this, Kind::Projector) => {
                if let Some(actor) = object_arg(args, 0)
                    && !self.actor_fx.projected.contains(&(this, actor))
                {
                    self.actor_fx.projected.push((this, actor));
                }
                return Ok(None);
            }
            Native::DetachActor if self.store.is_a(this, Kind::Projector) => {
                if let Some(actor) = object_arg(args, 0) {
                    self.actor_fx.projected.retain(|&pair| pair != (this, actor));
                }
                return Ok(None);
            }
            Native::Kill if self.store.is_a(this, Kind::Emitter) => {
                self.host.commands.push(HostCommand::Particle(ParticleCommand::Kill(this)));
                return Ok(None);
            }
            // Spawns wrap over the oldest; the VM counts, the host simulates.
            Native::SpawnParticle if self.store.is_a(this, self.script.lookups.class_names.particle_emitter) => {
                let max = self.store.int_of(this, Prop::MaxParticles).clamp(0, MAX_SPAWN);
                let amount = args.first().map_or(0, int).clamp(0, max);
                for _ in 0..amount {
                    self.host.commands.push(HostCommand::Particle(ParticleCommand::Spawn { emitter: this, offset: Vector::ZERO, line: None }));
                }
                if amount > 0 {
                    let active = self.store.int_of(this, Prop::ActiveParticles);
                    self.store.set(this, Prop::ActiveParticles, Value::Int((active + amount).min(max)));
                }
                return Ok(None);
            }
            Native::Pling if self.store.is_a(this, self.script.lookups.class_names.fluid_surface_info) => {
                let pling = FluidPling { surface: this, position: vec3(args.first()), strength: float_arg(args, 1), radius: float_arg(args, 2) };
                queue(&mut self.actor_fx.plings, pling);
                return Ok(None);
            }
            Native::GetCameraLocation if self.store.is_a(this, self.script.lookups.class_names.canvas) => {
                let (location, rotation) = self.camera();
                if let Some(out) = args.get_mut(0) {
                    *out = Value::Vector(location);
                }
                if let Some(out) = args.get_mut(1) {
                    *out = Value::Rotator(rotation);
                }
                return Ok(None);
            }
            // X, Y in canvas pixels.
            Native::ScreenToWorld => {
                let Vector { x, y, .. } = vec3(args.first());
                let (_, rotation) = self.camera();
                let fov = self.player().map_or(90.0, |p| self.store.float_of(p, Prop::FovAngle));
                let (width, height) = self.media.canvas.object.map_or((0, 0), |c| (self.store.int_of(c, Prop::SizeX), self.store.int_of(c, Prop::SizeY)));
                Value::Vector(screen_to_near_plane([x, y], [width as f32, height as f32], if fov > 0.0 { fov } else { 90.0 }, rotation))
            }
            // `PlayFlashbangSound(Sound, OverrideDuration)`: the host stops the other sounds, silences the loops
            // and plays no new ones for the duration.
            Native::PlayFlashbangSound => {
                if let Some(sound) = object_arg(args, 0) {
                    let volume = self.sound_volume().clamp(0.0, 1.0);
                    let sound = self.store.object(sound).path.clone();
                    let played = crate::PlayedSound { actor: Some(this), sound, slot: 0, volume, radius: crate::natives::AUDIO_DEFAULT_RADIUS, pitch: 1.0, attenuate: false, no_override: false, flashbang: Some(float_arg(args, 1).max(0.0)) };
                    self.subtitle_insert_sound(played.actor, &played.sound);
                    self.host.commands.push(HostCommand::Sound(played));
                }
                return Ok(None);
            }
            Native::GetCacheEntry | Native::MoveCacheEntry => Value::Bool(false),
            // No shipped script calls the last two and nothing draws a `ScriptedTexture`'s text: no effect.
            Native::AddSpawnProperty | Native::GraphData | Native::DrawColoredText | Native::ReplaceTexture => return Ok(None),
            _ => return Err(unknown()),
        };
        Ok(Some(value))
    }

    pub(crate) fn bone_native(&mut self, native: Native, this: ObjectId, args: &[Value]) -> Result<Option<Value>, Error> {
        match native {
            Native::SetBoneRotation => {
                let turn = if given(args, 1) { rot3(args.get(1)) } else { Rotator::ZERO };
                let (space, alpha) = (int_arg(args, 2), optional_float(args, 3, 1.0));
                if let Some(entry) = self.bone_control(this, args.first()) {
                    (entry.rotation, entry.space, entry.rotation_alpha) = (turn, space, alpha);
                }
            }
            Native::SetBoneLocation => {
                let location = if given(args, 1) { vec3(args.get(1)) } else { Vector::ZERO };
                let alpha = optional_float(args, 2, 0.0);
                if let Some(entry) = self.bone_control(this, args.first()) {
                    (entry.location, entry.location_alpha) = (location, alpha);
                }
            }
            Native::SetBoneDirection => {
                let turn = rot3(args.get(1));
                let translation = if given(args, 2) { vec3(args.get(2)) } else { Vector::ZERO };
                let alpha = optional_float(args, 3, 1.0);
                if let Some(bone) = self.skeleton_bone(this, args.first()) {
                    let list = self.actor_fx.directions.entry(this).or_default();
                    let entry = BoneDirection { bone, turn, translation, alpha };
                    if let Some(slot) = control_slot(list, |d| d.bone == bone, || entry.clone()) {
                        *slot = entry;
                    }
                }
            }
            _ => return Err(Error::UnknownNative(format!("{native:?}"))),
        }
        Ok(None)
    }

    pub(crate) fn lod_native(&mut self, native: Native, this: ObjectId, args: &[Value]) -> Result<Option<Value>, Error> {
        let value = match native {
            // No shipped script calls these three natives and the host draws no LOD, so the list only answers
            // `DeleteLodStaticMesh`.
            Native::SetLodStaticMesh => {
                let (distance, mesh) = (float_arg(args, 0), object_arg(args, 1));
                let list = self.actor_fx.lods.entry(this).or_default();
                match list.iter_mut().find(|(d, _)| *d == distance) {
                    Some(entry) => entry.1 = mesh,
                    None => list.push((distance, mesh)),
                }
                return Ok(None);
            }
            Native::ClearLodStaticMesh => {
                self.actor_fx.lods.remove(&this);
                return Ok(None);
            }
            Native::DeleteLodStaticMesh => {
                let (distance, mesh) = (given(args, 0).then(|| float_arg(args, 0)), object_arg(args, 1));
                let list = self.actor_fx.lods.entry(this).or_default();
                let before = list.len();
                list.retain(|&(d, m)| !(distance.is_none_or(|x| x == d) && (mesh.is_none() || m == mesh)));
                Value::Int((before - list.len()) as i32)
            }
            _ => return Err(Error::UnknownNative(format!("{native:?}"))),
        };
        Ok(Some(value))
    }

    pub(crate) fn karma_state_native(&mut self, native: Native, this: ObjectId, args: &mut [Value]) -> Result<Option<Value>, Error> {
        match native {
            // No shipped script calls these or their getters and the bodies in `karma.rs` read neither property,
            // so the values only answer the getters.
            Native::KSetCOMOffset => {
                self.actor_fx.com_offsets.insert(this, vec3(args.first()));
            }
            Native::KGetCOMOffset => {
                let offset = self.actor_fx.com_offsets.get(&this).copied().unwrap_or_default();
                if let Some(out) = args.get_mut(0) {
                    *out = Value::Vector(offset);
                }
            }
            Native::KSetInertiaTensor => {
                self.actor_fx.inertia.insert(this, [vec3(args.first()), vec3(args.get(1))]);
            }
            Native::KGetInertiaTensor => {
                let tensor = self.actor_fx.inertia.get(&this).copied().unwrap_or_default();
                for (n, row) in tensor.into_iter().enumerate() {
                    if let Some(out) = args.get_mut(n) {
                        *out = Value::Vector(row);
                    }
                }
            }
            // The joint's force and torque come from the host's solver (not read yet): zero.
            Native::KGetConstraintForce | Native::KGetConstraintTorque => {
                if let Some(out) = args.get_mut(0) {
                    *out = Value::Vector(Vector::ZERO);
                }
            }
            // `KHinge` calls this; the VM models no Karma joints, so there is nothing to update (retail not read).
            Native::KUpdateConstraintParams => {}
            _ => return Err(Error::UnknownNative(format!("{native:?}"))),
        }
        Ok(None)
    }

    pub(crate) fn movie_native(&mut self, native: Native, this: ObjectId, args: &[Value]) -> Result<Option<Value>, Error> {
        if native != Native::InitializeMovie && !self.store.is_a(this, Kind::Movie) {
            return Err(Error::UnknownNative(format!("{native:?}")));
        }
        let value = match native {
            // Defaults UseSound 0, Loop 0 (the third default is 30, unverified).
            Native::Play => {
                let file = match args.first() {
                    Some(Value::Str(s)) => s.clone(),
                    _ => String::new(),
                };
                let previous = self.actor_fx.movies.get(&this).map_or((0, 0, 0), |m| (m.width, m.height, m.plays));
                let state = MovieState {
                    file,
                    use_sound: args.get(1).is_some_and(truthy),
                    looping: args.get(2).is_some_and(truthy),
                    playing: true,
                    paused: false,
                    width: previous.0,
                    height: previous.1,
                    plays: previous.2.wrapping_add(1),
                };
                self.actor_fx.movies.insert(this, state);
                return Ok(None);
            }
            Native::StopNow => {
                self.actor_fx.movies.entry(this).or_default().playing = false;
                return Ok(None);
            }
            // The movie ends after its last frame (no more looping; unverified).
            Native::StopAtEnd => {
                self.actor_fx.movies.entry(this).or_default().looping = false;
                return Ok(None);
            }
            Native::Pause => {
                let paused = args.first().is_some_and(truthy);
                self.actor_fx.movies.entry(this).or_default().paused = paused;
                return Ok(None);
            }
            Native::IsPlaying => Value::Bool(self.actor_fx.movies.get(&this).is_some_and(|m| m.playing)),
            Native::IsPaused => Value::Bool(self.actor_fx.movies.get(&this).is_some_and(|m| m.paused)),
            Native::GetWidth => Value::Int(self.actor_fx.movies.get(&this).map_or(0, |m| m.width)),
            Native::GetHeight => Value::Int(self.actor_fx.movies.get(&this).map_or(0, |m| m.height)),
            // Retail also opens the texture's `MovieFilename` here; whether that starts it is unverified,
            // script's `Play` follows at once.
            Native::InitializeMovie => {
                let movie = match self.store.get(this, "Movie") {
                    Some(Value::Object(Some(movie))) => movie,
                    _ => {
                        let class = self.find_class("Engine", "Movie")?;
                        let movie = self.new_object(class, None);
                        self.store.set(this, "Movie", Value::Object(Some(movie)));
                        movie
                    }
                };
                let plays = self.actor_fx.movies.get(&movie).map_or(0, |m| m.plays);
                self.actor_fx.movies.insert(movie, MovieState { plays, ..MovieState::default() });
                self.actor_fx.movie_textures.insert(this, movie);
                return Ok(None);
            }
            _ => return Err(Error::UnknownNative(format!("{native:?}"))),
        };
        Ok(Some(value))
    }

    fn camera(&mut self) -> (Vector, Rotator) {
        match self.player_view() {
            Ok(Some(view)) => (view.location, view.rotation),
            _ => (Vector::ZERO, [0; 3].into()),
        }
    }

    fn skeleton_bone(&mut self, actor: ObjectId, bone: Option<&Value>) -> Option<NameId> {
        let Some(&Value::Name(bone)) = bone else { return None };
        let mesh = self.store.object_of(actor, Prop::Mesh)?;
        let mesh = self.posed_mesh(mesh)?;
        let wanted = self.store.name(bone).to_ascii_lowercase();
        mesh.match_ref_bone(&wanted).map(|_| bone)
    }

    fn bone_control(&mut self, actor: ObjectId, bone: Option<&Value>) -> Option<&mut BoneControl> {
        let bone = self.skeleton_bone(actor, bone)?;
        let list = self.actor_fx.controls.entry(actor).or_default();
        control_slot(list, |c| c.bone == bone, || BoneControl { bone, rotation: [0; 3].into(), space: 0, rotation_alpha: 0.0, location: Vector::ZERO, location_alpha: 0.0 })
    }
}

fn attach_takes(project_tag: NameId, accepts: bool, tag: NameId, has_static_mesh: bool) -> bool {
    accepts && has_static_mesh && (project_tag == NameId::NONE || project_tag == tag)
}

/// Unreal units. Splats are spawned at the hit, on the receiver's surface.
const ON_SURFACE: f32 = 8.0;

fn on_receiver(location: Vector, center: Vector, extent: Vector) -> bool {
    (0..3).all(|i| (location[i] - center[i]).abs() <= extent[i] + ON_SURFACE)
}

/// Unreal units. Retail uses 5 for every scene: the view, the sky and each `DrawActor`.
pub const NEAR_CLIPPING_PLANE: f32 = 5.0;

/// `fov` is horizontal, in degrees. Not a unit vector, as retail: it is as long as the near plane is
/// far (at least 5), and `MapScreen.DrawMarkerOnMap` doubles it to put its markers past the plane.
fn screen_to_near_plane(pixel: [f32; 2], size: [f32; 2], fov: f32, rotation: Rotator) -> Vector {
    let [forward, right, up] = rotation.axes();
    let half = (fov.to_radians() / 2.0).tan();
    let (nx, ny) = if size[0] > 0.0 && size[1] > 0.0 {
        ((pixel[0] / size[0] * 2.0 - 1.0) * half, (1.0 - pixel[1] / size[1] * 2.0) * half * size[1] / size[0])
    } else {
        (0.0, 0.0)
    };
    [0, 1, 2].map(|i| (forward[i] + right[i] * nx + up[i] * ny) * NEAR_CLIPPING_PLANE).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_takes_accepting_static_meshes_with_the_tag() {
        let (tag, other) = (NameId(7), NameId(8));
        assert!(attach_takes(NameId::NONE, true, other, true));
        assert!(attach_takes(tag, true, tag, true));
        assert!(!attach_takes(tag, true, other, true));
        assert!(!attach_takes(NameId::NONE, false, other, true));
        assert!(!attach_takes(NameId::NONE, true, other, false));
    }

    #[test]
    fn a_splat_on_a_door_face_is_on_the_door() {
        let (center, extent) = (Vector::new(100.0, 0.0, 50.0), Vector::new(4.0, 32.0, 50.0));
        assert!(on_receiver(Vector::new(104.0, 10.0, 60.0), center, extent));
        assert!(on_receiver(Vector::new(110.0, 10.0, 60.0), center, extent));
        assert!(!on_receiver(Vector::new(120.0, 10.0, 60.0), center, extent));
        assert!(!on_receiver(Vector::new(100.0, 50.0, 60.0), center, extent));
    }

    #[test]
    fn centre_is_the_near_plane_straight_ahead() {
        let d = screen_to_near_plane([320.0, 240.0], [640.0, 480.0], 90.0, [0; 3].into());
        assert!((d[0] - NEAR_CLIPPING_PLANE).abs() < 1e-5 && d[1].abs() < 1e-5 && d[2].abs() < 1e-5);
    }

    #[test]
    fn right_edge_at_ninety_degrees_is_forty_five_degrees_right_on_the_near_plane() {
        let d = screen_to_near_plane([640.0, 240.0], [640.0, 480.0], 90.0, [0; 3].into());
        assert!((d[0] - NEAR_CLIPPING_PLANE).abs() < 1e-5 && (d[0] - d[1]).abs() < 1e-5 && d[2].abs() < 1e-5);
    }

    #[test]
    fn top_of_screen_looks_up() {
        let d = screen_to_near_plane([320.0, 0.0], [640.0, 480.0], 90.0, [0; 3].into());
        assert!(d[2] > 0.0);
    }
}
