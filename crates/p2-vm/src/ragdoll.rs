use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use rustc_hash::FxHashMap;

use p2_assets::pose::Frame;
use p2_assets::{KaConeType, KaJointKind, KaShape, KaTwistType, KarmaAsset, read_karma_assets};
use p2_physics::{Background, BodyId, Hit, JointLimit, JointParams, PartParams, Parts, RagdollParams, Shape, Throw};

use p2_math::Vector;
use crate::enums::EPhysics;
use crate::native_id::Native;
use crate::bones::PosedMesh;
use crate::karma::{IMPULSE_SCALE, matrix_to_quat};
use crate::{ClassId, Encroachment, Error, LoaderRef, NameId, ObjectId, Value, Vm, Prop, Kind};

const MAX_SPEED: f32 = 1000.0;
/// In place of `KFriction` (0.7 on a pawn): Karma corpses slide 200-500 units to rest (unverified).
const CORPSE_FRICTION: f32 = 0.3;
/// Corpses sprawl rather than fold ...
const CONE_SCALE: f32 = 1.3;
/// ... but wider cones let limbs wrap around the body.
const MAX_CONE: f32 = 1.55;
/// A fixed twist held at exactly 0 fights the cone and makes limbs flip.
const MIN_TWIST: f32 = 0.1;
const THROWS: [(&str, Throw); 9] = [
    ("ShotGunDamage", Throw { per_damage: 11.0, ceiling: 650.0, lift: 0.3, falloff: true, tumble: false }),
    ("SuperShotgunDamage", Throw { per_damage: 11.0, ceiling: 650.0, lift: 0.3, falloff: true, tumble: false }),
    ("RifleDamage", Throw { per_damage: 14.0, ceiling: 520.0, lift: 0.2, falloff: false, tumble: false }),
    ("MachineGunDamage", Throw { per_damage: 10.0, ceiling: 350.0, lift: 0.1, falloff: false, tumble: false }),
    ("BulletDamage", Throw { per_damage: 6.0, ceiling: 260.0, lift: 0.1, falloff: false, tumble: false }),
    ("ExplodedDamage", Throw { per_damage: 5.0, ceiling: 700.0, lift: 0.6, falloff: false, tumble: true }),
    ("SmashDamage", Throw { per_damage: 6.0, ceiling: 700.0, lift: 0.5, falloff: false, tumble: true }),
    ("CuttingDamage", Throw { per_damage: 3.0, ceiling: 400.0, lift: 0.1, falloff: false, tumble: false }),
    ("BludgeonDamage", Throw { per_damage: 10.0, ceiling: 550.0, lift: 0.25, falloff: false, tumble: false }),
];
const HIT_TICKS: u64 = 2;
/// Scripts cut with 0 or 1/1000; a 0.2 stump keeps its body.
const CUT_SCALE: f32 = 0.01;

struct SeenHit {
    actor: ObjectId,
    tick: u64,
    hit: Hit,
}
const MAX_JOINT_ERROR: f32 = 50.0;

#[derive(Default)]
pub(crate) struct Ragdolls {
    assets: Option<HashMap<String, Arc<KarmaAsset>>>,
    live: BTreeMap<ObjectId, Ragdoll>,
    kept: FxHashMap<ObjectId, (Arc<PosedMesh>, Arc<[Frame]>)>,
    frozen: HashSet<ObjectId>,
    failed: HashSet<ObjectId>,
    throws: FxHashMap<ClassId, Option<Throw>>,
    tick: u64,
    hits: Vec<SeenHit>,
}

impl Ragdolls {
    pub(crate) fn for_next_level(&mut self) -> Self {
        Self { assets: self.assets.take(), ..Default::default() }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}

#[derive(Clone, Copy)]
struct TrackedJoint {
    child: usize,
    parent: usize,
    at_child: Vector,
    at_parent: Vector,
}

struct Ragdoll {
    bodies: Vec<Option<BodyId>>,
    all: Arc<[BodyId]>,
    joints: Vec<TrackedJoint>,
    mesh: Arc<PosedMesh>,
    scale: f32,
    frames: Arc<[Frame]>,
    fresh: bool,
    posed_asleep: Option<bool>,
    /// Lines and impulses test these while the world steps.
    parts: Parts,
    scales: ResolvedScales,
    cut_refused: bool,
    /// A bodiless root moves with this body, or `SetBoneScale` pins a cut corpse's drawn top half at the cut.
    root_offset: Option<(usize, Frame)>,
}

#[derive(Default)]
struct ResolvedScales {
    read: Vec<(NameId, u32)>,
    own: Option<Vec<f32>>,
    cut: Vec<bool>,
}

impl ResolvedScales {
    fn holds(&self, scales: &[(NameId, f32)]) -> bool {
        self.read.len() == scales.len() && self.read.iter().zip(scales).all(|(&(a, x), &(b, y))| a == b && x == y.to_bits())
    }
}

impl<L: LoaderRef> Vm<L> {
    pub fn ragdoll_pose(&mut self, actor: ObjectId) -> Option<Vec<Frame>> {
        let world = self.ragdoll_world_frames(actor)?;
        let mesh = self.object_of(actor, Prop::Mesh).and_then(|m| self.posed_mesh(m))?;
        let to_mesh = self.mesh_to_world(actor, &mesh).inverse()?;
        Some(world.iter().map(|frame| to_mesh.then(frame)).collect())
    }

    pub(crate) fn ragdoll_world_frames(&self, actor: ObjectId) -> Option<Arc<[Frame]>> {
        let ragdolls = &self.karma.ragdolls;
        let live = ragdolls.live.get(&actor);
        let (mesh, frames) = match (live, ragdolls.kept.get(&actor)) {
            (Some(ragdoll), _) if ragdoll.fresh => return Some(ragdoll.frames.clone()),
            (Some(ragdoll), _) => (&ragdoll.mesh, &ragdoll.frames),
            (None, Some((mesh, frames))) => (mesh, frames),
            (None, None) => return None,
        };
        let scales: Vec<_> = self.bone_scales(actor).collect();
        if scales.is_empty() {
            return Some(frames.clone());
        }
        let resolved = live.filter(|ragdoll| ragdoll.scales.holds(&scales)).and_then(|ragdoll| ragdoll.scales.own.as_deref());
        let computed;
        let own: &[f32] = match resolved {
            Some(own) => own,
            None => {
                computed = self.scales_on_mesh(&scales, mesh)?;
                &computed
            }
        };
        let mut scaled = frames.to_vec();
        scale_frames(&mut scaled, own, &mesh.parents);
        Some(scaled.into())
    }

    fn bone_scale_factors(&self, actor: ObjectId, mesh: &PosedMesh) -> Option<Vec<f32>> {
        self.scales_on_mesh(&self.bone_scales(actor).collect::<Vec<_>>(), mesh)
    }

    fn scales_on_mesh(&self, scales: &[(NameId, f32)], mesh: &PosedMesh) -> Option<Vec<f32>> {
        let scales: Vec<(String, f32)> = scales.iter().map(|&(bone, scale)| (self.name(bone).to_ascii_lowercase(), scale)).collect();
        (!scales.is_empty()).then(|| mesh.names.iter().map(|name| scales.iter().filter(|(n, _)| n == name).map(|(_, s)| s).product()).collect())
    }

    /// Retail's frozen-ragdoll box is not modelled: a kept pose keeps the cylinder.
    pub(crate) fn ragdoll_box(&self, actor: ObjectId) -> Option<(Vector, Vector)> {
        self.karma.ragdolls.live.get(&actor)?.parts.bounds()
    }

    pub(crate) fn ragdoll_line_check(&self, actor: ObjectId, start: Vector, end: Vector) -> Option<Option<p2_collision::Hit>> {
        if self.physics_of(actor) != Some(EPhysics::KarmaRagDoll) {
            return None;
        }
        let Some(ragdoll) = self.karma.ragdolls.live.get(&actor) else { return Some(None) };
        Some(ragdoll.parts.segment_hit(start, end).map(|hit| p2_collision::Hit {
            time: hit.time,
            location: [0, 1, 2].map(|i| start[i] + (end[i] - start[i]) * hit.time).into(),
            normal: hit.normal,
        }))
    }

    /// Now, so that `KSetSkelVel` and `KAddImpulse` right after it reach the bodies.
    pub(crate) fn ragdoll_physics_changed(&mut self, actor: ObjectId, old: Option<EPhysics>) {
        let physics = self.physics_of(actor);
        if old == Some(EPhysics::KarmaRagDoll) && physics != Some(EPhysics::KarmaRagDoll) {
            self.end_ragdoll(actor, true);
        }
        if physics == Some(EPhysics::KarmaRagDoll) && !self.karma.ragdolls.live.contains_key(&actor) {
            let ragdolls = &mut self.karma.ragdolls;
            ragdolls.frozen.remove(&actor);
            ragdolls.failed.remove(&actor);
            let phase = self.profile_start();
            self.init_ragdoll(actor, None);
            self.profile_phase("ragdoll made", phase);
        }
    }

    pub(crate) fn ragdolls_before_step(&mut self, actors: Vec<ObjectId>) {
        let gone: Vec<ObjectId> = self
            .karma
            .ragdolls
            .live
            .keys()
            .copied()
            .filter(|&a| self.is_deleted(a) || self.physics_of(a) != Some(EPhysics::KarmaRagDoll))
            .collect();
        for actor in gone {
            let keep = !self.is_deleted(actor);
            self.end_ragdoll(actor, keep);
        }
        let ragdolls = &self.karma.ragdolls;
        let destroyed: Vec<ObjectId> =
            ragdolls.kept.keys().chain(&ragdolls.frozen).chain(&ragdolls.failed).copied().filter(|&a| self.is_deleted(a)).collect();
        for actor in destroyed {
            let ragdolls = &mut self.karma.ragdolls;
            ragdolls.kept.remove(&actor);
            ragdolls.frozen.remove(&actor);
            ragdolls.failed.remove(&actor);
        }
        let ragdolls = &self.karma.ragdolls;
        let wanted: Vec<ObjectId> = actors
            .into_iter()
            .filter(|a| !ragdolls.live.contains_key(a) && !ragdolls.frozen.contains(a) && !ragdolls.failed.contains(a))
            .collect();
        for actor in wanted {
            let phase = self.profile_start();
            self.init_ragdoll(actor, None);
            self.profile_phase("ragdoll made", phase);
        }
        self.rebuild_cut_ragdolls();
    }

    fn rebuild_cut_ragdolls(&mut self) {
        let changed: Vec<(ObjectId, ResolvedScales)> = self
            .karma
            .ragdolls
            .live
            .iter()
            .filter_map(|(&actor, ragdoll)| {
                let scales: Vec<_> = self.bone_scales(actor).collect();
                (!ragdoll.scales.holds(&scales)).then(|| {
                    let own = self.scales_on_mesh(&scales, &ragdoll.mesh);
                    let cut = cut_of(own.as_deref(), &ragdoll.mesh.parents);
                    (actor, ResolvedScales { read: scales.iter().map(|&(bone, scale)| (bone, scale.to_bits())).collect(), own, cut })
                })
            })
            .collect();
        for (actor, scales) in changed {
            if let Some(ragdoll) = self.karma.ragdolls.live.get_mut(&actor) {
                ragdoll.scales = scales;
            }
        }
        let cut: Vec<(ObjectId, usize)> = self
            .karma
            .ragdolls
            .live
            .iter()
            .filter(|(_, ragdoll)| !ragdoll.cut_refused && !ragdoll.scales.cut.is_empty())
            .filter_map(|(&actor, ragdoll)| {
                let cut = &ragdoll.scales.cut;
                let is_cut = |bone: usize| cut.get(bone).copied().unwrap_or(false);
                let bodied = || ragdoll.bodies.iter().enumerate().filter(|(_, body)| body.is_some()).map(|(bone, _)| bone);
                bodied().any(is_cut).then(|| (actor, bodied().filter(|&b| !is_cut(b)).count()))
            })
            .collect();
        for (actor, kept) in cut {
            let Some(ragdoll) = self.karma.ragdolls.live.get_mut(&actor) else { continue };
            if kept < 2 {
                ragdoll.cut_refused = true;
                continue;
            }
            let frames = ragdoll.frames.to_vec();
            self.end_ragdoll(actor, false);
            self.init_ragdoll(actor, Some(frames));
        }
    }

    pub(crate) fn ragdolls_after_step(&mut self) -> Vec<(String, Error)> {
        let mut errors = Vec::new();
        let actors: Vec<ObjectId> = self.karma.ragdolls.live.keys().copied().collect();
        for actor in actors {
            let Some((location, velocity, torn)) = self.pose_ragdoll(actor) else { continue };
            self.set_vector(actor, Prop::Velocity, velocity);
            self.set(actor, Prop::Rotation, Value::Rotator([0; 3].into()));
            if self.vector(actor, Prop::Location) != location
                && let Err(e) = self.place_far_moved(actor, location, Encroachment::Ignored).and_then(|()| self.update_touching(actor))
            {
                errors.push((self.object(actor).path.clone(), e));
            }
            if torn && let Err(e) = self.event(actor, "KExcessiveJointError", Vec::new()) {
                errors.push((self.object(actor).path.clone(), e));
            }
        }
        errors
    }

    fn pose_ragdoll(&mut self, actor: ObjectId) -> Option<(Vector, Vector, bool)> {
        let world = self.karma.world.as_mut()?.world()?;
        let ragdoll = self.karma.ragdolls.live.get_mut(&actor)?;
        ragdoll.fresh = false;
        let asleep = ragdoll.all.iter().all(|&id| world.is_sleeping(id));
        if asleep && ragdoll.posed_asleep.is_some() {
            let root = ragdoll.bodies.iter().flatten().next().copied()?;
            return Some(((world.pose(root)?.0), [0.0; 3].into(), ragdoll.posed_asleep.unwrap_or(false)));
        }
        for &id in ragdoll.all.iter() {
            world.limit_speed(id, MAX_SPEED);
        }
        let (mesh, scale) = (ragdoll.mesh.clone(), ragdoll.scale);
        let root = ragdoll.root_offset.and_then(|(anchor, offset)| {
            let (location, rotation) = world.pose(ragdoll.bodies[anchor]?)?;
            Some(Frame::from_quat(rotation, scale, location.into()).then(&offset))
        });
        let frames = Arc::make_mut(&mut ragdoll.frames);
        for bone in 0..frames.len() {
            let frame = match ragdoll.bodies[bone].and_then(|id| world.pose(id)) {
                Some((location, rotation)) => Frame::from_quat(rotation, scale, location.into()),
                None => {
                    let parent = mesh.parents.get(bone).copied().filter(|&p| p < bone);
                    let local = mesh.reference.get(bone).copied().unwrap_or(Frame::IDENTITY);
                    match parent {
                        Some(parent) => frames[parent].then(&local),
                        None => root.unwrap_or(frames[bone]),
                    }
                }
            };
            frames[bone] = frame;
        }
        let unit = |frame: &Frame| Frame { axes: frame.axes.map(|a| a.map(|c| c / scale)), origin: frame.origin };
        let torn = ragdoll.joints.iter().any(|joint| {
            let (a, b) = (unit(&ragdoll.frames[joint.child]).transform_point(joint.at_child.into()), unit(&ragdoll.frames[joint.parent]).transform_point(joint.at_parent.into()));
            (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f32>() > MAX_JOINT_ERROR * MAX_JOINT_ERROR
        });
        world.parts_into(&ragdoll.all, &mut ragdoll.parts);
        let root = ragdoll.bodies.iter().flatten().next().copied()?;
        ragdoll.posed_asleep = asleep.then_some(torn);
        let velocity = if asleep { [0.0; 3] } else { world.velocity(root)?.into() };
        Some((world.pose(root)?.0, velocity.into(), torn))
    }

    fn init_ragdoll(&mut self, actor: ObjectId, pose: Option<Vec<Frame>>) {
        let params = self.object_of(actor, Prop::KParams).filter(|&k| self.is_a(k, Kind::KarmaParamsSkel));
        let Some(params) = params else {
            self.diagnostics.log.push("(Karma): KInitSkeletonKarma: No KParams, or KParams is not a KarmaParamsSkel.".to_owned());
            self.karma.ragdolls.failed.insert(actor);
            return;
        };
        let skeleton = match self.get(params, Prop::KSkeleton) {
            Some(Value::Name(name)) => self.name(name).to_ascii_lowercase(),
            _ => String::new(),
        };
        let asset = self.karma_assets().get(&skeleton).cloned();
        let (Some(asset), Some((mesh, animated))) = (asset, self.world_bone_frames(actor)) else {
            self.diagnostics.log.push("(Karma): KInitSkeletonKarma: Asset not found.".to_owned());
            self.karma.ragdolls.failed.insert(actor);
            return;
        };
        // Or the cut limb shows again until the first step.
        let frames: Arc<[Frame]> = match pose.filter(|pose| pose.len() == animated.len()) {
            Some(mut pose) => {
                if let Some(own) = self.bone_scale_factors(actor, &mesh) {
                    scale_frames(&mut pose, &own, &mesh.parents);
                }
                pose.into()
            }
            None => animated,
        };
        let own = self.bone_scale_factors(actor, &mesh);
        let cut = cut_of(own.as_deref(), &mesh.parents);
        let k = |name: Prop, default: f32| self.get(params, name).as_ref().map_or(default, crate::interp::float);
        let (linear_damping, angular_damping) = (k(Prop::KLinearDamping, 0.2), k(Prop::KAngularDamping, 0.2));
        // `KRestitution` made corpses hop.
        let (friction, restitution) = (CORPSE_FRICTION, 0.0);
        let asleep = !self.flag_of(params, Prop::KStartEnabled);
        let velocity = self.vector(actor, Prop::Velocity);
        let Some(built) = build(&asset, &mesh, &frames, velocity, &cut) else {
            self.diagnostics.log.push(format!("(Karma): KInitSkeletonKarma: no part of {} fits a bone of {}", asset.id, self.object(actor).path));
            self.karma.ragdolls.failed.insert(actor);
            return;
        };
        let params = RagdollParams { parts: built.parts, joints: built.joint_params, linear_damping, angular_damping, friction, restitution, asleep };
        self.karma_world(actor);
        let Some(world) = self.karma.world.as_mut().and_then(Background::world) else { return };
        let all = world.add_ragdoll(&params);
        let parts = world.parts(&all);
        let bodies = built.part_of_bone.iter().map(|part| part.map(|p| all[p])).collect();
        for &id in &all {
            self.karma.actors.insert(id, actor);
        }
        let scale = frame_scale(&frames[0]);
        let root_offset = built.part_of_bone.iter().position(Option::is_some).and_then(|anchor| {
            let body = Frame::from_quat(matrix_to_quat(orthonormal(frames[anchor].axes.map(Vector::from))), scale, frames[anchor].origin);
            Some((anchor, body.inverse()?.then(&frames[0])))
        });
        let ragdoll =
            Ragdoll { bodies, all: all.into(), joints: built.joints, mesh, scale, frames, fresh: true, posed_asleep: None, parts, scales: ResolvedScales::default(), cut_refused: false, root_offset };
        self.karma.ragdolls.live.insert(actor, ragdoll);
        self.karma.ragdolls.kept.remove(&actor);
        for (property, value) in [("bCollideActors", true), ("bBlockActors", false), ("bBlockPlayers", false), ("bCollideWorld", false), ("bProjTarget", true)] {
            self.set(actor, property, Value::Bool(value));
        }
        self.relocate(actor);
        if let Err(e) = self.update_touching(actor) {
            self.diagnostics.log.push(format!("{}: {e}", self.object(actor).path));
        }
    }

    fn end_ragdoll(&mut self, actor: ObjectId, keep: bool) {
        let Some(ragdoll) = self.karma.ragdolls.live.remove(&actor) else { return };
        if let Some(world) = self.karma.world.as_mut() {
            let all = ragdoll.all.clone();
            world.change(move |w| w.remove_ragdoll(&all));
        }
        for id in ragdoll.all.iter() {
            self.karma.actors.remove(id);
        }
        if keep {
            self.karma.ragdolls.kept.insert(actor, (ragdoll.mesh, ragdoll.frames));
        }
    }

    pub(crate) fn ragdoll_native(&mut self, native: Native, this: ObjectId, args: &[Value]) -> Option<Result<Option<Value>, Error>> {
        let live = self.karma.ragdolls.live.contains_key(&this);
        match native {
            Native::KSetSkelVel => {
                if !live {
                    self.diagnostics.log.push("(Karma:) KSetSkelVelocity: Ragdoll not initialised.".to_owned());
                    return Some(Ok(None));
                }
                let velocity = crate::natives::vec3(args.first());
                let (Some(world), Some(ragdoll)) = (self.karma.world.as_mut(), self.karma.ragdolls.live.get(&this)) else { return Some(Ok(None)) };
                let all = ragdoll.all.clone();
                world.change(move |w| {
                    for &id in all.iter() {
                        w.set_velocity(id, velocity);
                    }
                });
                Some(Ok(None))
            }
            Native::KFreezeRagdoll => {
                if !live {
                    self.diagnostics.log.push("(Karma:) KFreezeRagdoll: Ragdoll not initialised.".to_owned());
                    return Some(Ok(None));
                }
                self.end_ragdoll(this, true);
                self.karma.ragdolls.frozen.insert(this);
                Some(Ok(None))
            }
            Native::KWake if live => {
                if let (Some(world), Some(ragdoll)) = (self.karma.world.as_mut(), self.karma.ragdolls.live.get(&this)) {
                    let all = ragdoll.all.clone();
                    world.change(move |w| w.wake_ragdoll(&all));
                }
                Some(Ok(None))
            }
            Native::KAddImpulse if live => {
                let impulse = crate::natives::vec3(args.first()).map(|c| c * IMPULSE_SCALE);
                let at = crate::natives::vec3(args.get(1));
                let named = match args.get(2) {
                    Some(&Value::Name(name)) if name != NameId::NONE => {
                        let bone = self.name(name);
                        Some(self.karma.ragdolls.live.get(&this).and_then(|ragdoll| ragdoll.mesh.names.iter().position(|n| n.eq_ignore_ascii_case(bone))))
                    }
                    _ => None,
                };
                let seen = self.take_hit(this);
                let (Some(world), Some(ragdoll)) = (self.karma.world.as_mut(), self.karma.ragdolls.live.get(&this)) else { return Some(Ok(None)) };
                let target = if let Some(index) = named {
                    // Else every shot and kick on a cut corpse's top half is lost.
                    index.and_then(|i| ragdoll.bodies.get(i).copied().flatten()).or_else(|| ragdoll.parts.nearest_body(at))
                } else if ragdoll.fresh {
                    ragdoll.bodies.iter().flatten().next().copied()
                } else {
                    ragdoll.parts.nearest_body(at)
                };
                if let Some(id) = target {
                    let all = ragdoll.all.clone();
                    match seen {
                        Some(hit) => {
                            let hit = Hit { impulse, body: Some(id), ..hit };
                            world.change(move |w| w.ragdoll_hit(&all, &hit));
                        }
                        None => world.change(move |w| w.ragdoll_impulse(&all, impulse, Some(id))),
                    }
                }
                Some(Ok(None))
            }
            _ => None,
        }
    }

    pub(crate) fn hit_seen(&mut self, victim: ObjectId, args: &[Value]) {
        let damage = args.first().map_or(0.0, crate::interp::float);
        let instigator = match args.get(1) {
            Some(&Value::Object(Some(by))) => Some(by),
            _ => None,
        };
        let (at, momentum) = (crate::natives::vec3(args.get(2)), crate::natives::vec3(args.get(3)));
        let throw = match args.get(4) {
            Some(&Value::Object(Some(class))) => self.throw_of(class),
            _ => None,
        };
        let range = instigator.map(|by| {
            let from = self.vector(by, Prop::Location);
            (0..3).map(|i| (from[i] - at[i]).powi(2)).sum::<f32>().sqrt()
        });
        let length = momentum.size();
        let dir = if length > 0.0 { momentum.map(|c| c / length) } else { [0.0; 3].into() };
        let hit = Hit { impulse: [0.0; 3].into(), body: None, damage, throw, range, at, dir };
        let ragdolls = &mut self.karma.ragdolls;
        let tick = ragdolls.tick;
        ragdolls.hits.retain(|h| tick - h.tick <= HIT_TICKS);
        ragdolls.hits.push(SeenHit { actor: victim, tick, hit });
    }

    fn take_hit(&mut self, actor: ObjectId) -> Option<Hit> {
        let ragdolls = &mut self.karma.ragdolls;
        let tick = ragdolls.tick;
        let index = ragdolls.hits.iter().rposition(|h| h.actor == actor && tick - h.tick <= HIT_TICKS)?;
        Some(ragdolls.hits.remove(index).hit)
    }

    fn throw_of(&mut self, class: ObjectId) -> Option<Throw> {
        let class = self.class_of_object(class).ok().flatten()?;
        if let Some(&throw) = self.karma.ragdolls.throws.get(&class) {
            return throw;
        }
        let throw = THROWS.iter().find(|(name, _)| self.class_is_a(class, *name)).map(|(_, throw)| *throw);
        self.karma.ragdolls.throws.insert(class, throw);
        throw
    }

    pub(crate) fn ragdoll_leftover_hits(&mut self) {
        let ragdolls = &mut self.karma.ragdolls;
        let tick = ragdolls.tick;
        ragdolls.tick += 1;
        let hits = std::mem::take(&mut ragdolls.hits);
        for seen in hits {
            match ragdolls.live.get(&seen.actor) {
                Some(ragdoll) if seen.hit.throw.is_some() => {
                    if let Some(world) = self.karma.world.as_mut() {
                        let all = ragdoll.all.clone();
                        world.change(move |w| w.ragdoll_hit(&all, &seen.hit));
                    }
                }
                None if tick - seen.tick < HIT_TICKS => ragdolls.hits.push(seen),
                _ => {}
            }
        }
    }

    /// In file name order, as retail: `Avg_Fem.ka` wins over `Avg_Fem_Skel.ka`.
    pub(crate) fn karma_assets(&mut self) -> &HashMap<String, Arc<KarmaAsset>> {
        if self.karma.ragdolls.assets.is_none() {
            let phase = self.profile_start();
            let mut files: Vec<std::path::PathBuf> = self
                .loader
                .package_names()
                .filter_map(|name| self.loader.find(name))
                .filter(|path| path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ka")))
                .map(std::path::Path::to_path_buf)
                .collect();
            files.sort_by_key(|path| path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()));
            let mut assets = HashMap::new();
            for path in files {
                let read = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|text| read_karma_assets(&text).map_err(|e| e.to_string()));
                match read {
                    Ok(list) => {
                        for asset in list {
                            assets.entry(asset.id.to_ascii_lowercase()).or_insert_with(|| Arc::new(asset));
                        }
                    }
                    Err(e) => self.diagnostics.log.push(format!("(Karma): Non-fatal error in MeFileLoad: {}: {e}", path.display())),
                }
            }
            self.karma.ragdolls.assets = Some(assets);
            self.profile_phase("load: karma assets", phase);
        }
        self.karma.ragdolls.assets.get_or_insert_default()
    }
}

fn cut_of(own: Option<&[f32]>, parents: &[usize]) -> Vec<bool> {
    let Some(own) = own else { return Vec::new() };
    let mut effective = own.to_vec();
    for bone in 0..effective.len() {
        if let Some(parent) = parents.get(bone).copied().filter(|&p| p < bone) {
            effective[bone] *= effective[parent];
        }
    }
    effective.into_iter().map(|s| s.abs() < CUT_SCALE).collect()
}

struct Built {
    parts: Vec<PartParams>,
    joint_params: Vec<JointParams>,
    part_of_bone: Vec<Option<usize>>,
    joints: Vec<TrackedJoint>,
}

fn build(asset: &KarmaAsset, mesh: &PosedMesh, frames: &[Frame], velocity: Vector, cut: &[bool]) -> Option<Built> {
    let to_unreal = if asset.scale > 0.0 { 1.0 / asset.scale } else { 1.0 };
    let unreal = |v: Vector| v.map(|c| c * to_unreal);
    let mut parts = Vec::new();
    let mut part_of_bone = vec![None; mesh.names.len()];
    let mut part_names: Vec<&str> = Vec::new();
    let bodied = |name: &str| {
        mesh.names.iter().position(|n| n.eq_ignore_ascii_case(name)).is_some_and(|bone| !cut.get(bone).copied().unwrap_or(false))
            && asset
                .parts
                .iter()
                .find(|p| p.id.eq_ignore_ascii_case(name))
                .and_then(|p| asset.models.iter().find(|m| m.id == p.model))
                .is_some_and(|m| m.dynamic)
    };
    for (bone, name) in mesh.names.iter().enumerate() {
        if cut.get(bone).copied().unwrap_or(false) {
            continue;
        }
        let Some(part) = asset.parts.iter().find(|p| p.id.eq_ignore_ascii_case(name)) else { continue };
        let Some(model) = asset.models.iter().find(|m| m.id == part.model).filter(|m| m.dynamic) else { continue };
        let geometry = model.geometry.as_ref().filter(|_| model.collides).and_then(|g| asset.geometries.iter().find(|x| x.id == *g));
        let shapes = geometry.map_or_else(Vec::new, |g| {
            g.primitives
                .iter()
                .filter_map(|p| {
                    let origin = unreal([p.tm[12], p.tm[13], p.tm[14]].into());
                    match p.shape {
                        KaShape::Sphyl { radius, height } => {
                            let half = [p.tm[8], p.tm[9], p.tm[10]].map(|c| c * height * to_unreal / 2.0);
                            let (a, b) = ([0, 1, 2].map(|i| origin[i] - half[i]), [0, 1, 2].map(|i| origin[i] + half[i]));
                            Some(Shape::Capsule(a.into(), b.into(), radius * to_unreal))
                        }
                        KaShape::Sphere { radius } => Some(Shape::Sphere(origin, radius * to_unreal)),
                        KaShape::Other(_) => None,
                    }
                })
                .collect()
        });
        // Colliding with nothing, such a body falls through the level and takes the drawn top half with it.
        let jointed = || {
            asset.joints.iter().any(|joint| {
                let [a, b] = &joint.parts;
                (a.eq_ignore_ascii_case(&part.id) && bodied(b)) || (b.eq_ignore_ascii_case(&part.id) && bodied(a))
            })
        };
        if shapes.is_empty() && cut.contains(&true) && !jointed() {
            continue;
        }
        let frame = &frames[bone];
        // The file's inertia is a placeholder.
        let file_inertia = [model.inertia[0], model.inertia[3], model.inertia[5]].map(|i| i * asset.mass_scale * to_unreal * to_unreal).into();
        let inertia = p2_physics::shapes_inertia(&shapes, model.mass * asset.mass_scale).unwrap_or(file_inertia);
        part_of_bone[bone] = Some(parts.len());
        part_names.push(&part.id);
        parts.push(PartParams {
            shapes,
            location: frame.origin.into(),
            rotation: matrix_to_quat(orthonormal(frame.axes.map(Vector::from))),
            mass: model.mass * asset.mass_scale,
            center_of_mass: unreal(model.mass_offset.into()),
            inertia,
            velocity,
        });
    }
    if parts.is_empty() || (parts.len() < 2 && cut.contains(&true)) {
        return None;
    }
    let part_index = |name: &str| part_names.iter().position(|p| p.eq_ignore_ascii_case(name));
    let bone_of = |part: usize| part_of_bone.iter().position(|&p| p == Some(part));
    let unit = |bone: usize| {
        let axes = orthonormal(frames[bone].axes.map(Vector::from)).map(<[f32; 3]>::from);
        Frame { axes, origin: frames[bone].origin }
    };
    let mut joints = Vec::new();
    let mut tracked = Vec::new();
    for joint in &asset.joints {
        let (Some(child), Some(parent)) = (part_index(&joint.parts[0]), part_index(&joint.parts[1])) else { continue };
        let (Some(child_bone), Some(parent_bone)) = (bone_of(child), bone_of(parent)) else { continue };
        let limit = match joint.kind {
            KaJointKind::Hinge { low, high, limited, .. } => JointLimit::Hinge(limited.then_some([low, high])),
            // Cone half angle X about the third axis, Y about the orthogonal axis (unverified); a slot limits X only.
            KaJointKind::Skeletal { cone_type, cone_half_angles, twist_type, twist_half_angle, .. } => {
                let [x, y] = cone_half_angles.map(|a| (a * CONE_SCALE).min(MAX_CONE));
                JointLimit::Cone {
                    swing: match cone_type {
                        KaConeType::Free => None,
                        KaConeType::Slot => Some([0.0, x]),
                        KaConeType::Fixed => Some([0.0, 0.0]),
                        KaConeType::Cone => Some([y, x]),
                    },
                    twist: match twist_type {
                        KaTwistType::Free => None,
                        KaTwistType::Limited => Some(twist_half_angle.max(MIN_TWIST)),
                        KaTwistType::Fixed => Some(MIN_TWIST),
                    },
                }
            }
            KaJointKind::Other(_) => JointLimit::Cone { swing: None, twist: None },
        };
        // Retail takes the parent's end from the asset, up to ~8 units off the bones; ours has no jolt at the start.
        let child_anchor = unreal(joint.positions[0].into());
        let world_anchor = unit(child_bone).transform_point(child_anchor.into());
        let parent_anchor = unit(parent_bone).inverse().map_or(child_anchor, |inv| inv.transform_point(world_anchor).into());
        tracked.push(TrackedJoint { child: child_bone, parent: parent_bone, at_child: child_anchor, at_parent: parent_anchor });
        joints.push(JointParams {
            child,
            parent,
            child_anchor,
            child_axes: [joint.primary_axes[0].into(), joint.orthogonal_axes[0].into()],
            parent_anchor,
            parent_axes: [joint.primary_axes[1].into(), joint.orthogonal_axes[1].into()],
            limit,
        });
    }
    Some(Built { parts, joint_params: joints, part_of_bone, joints: tracked })
}

fn orthonormal([x, y, _]: [Vector; 3]) -> [Vector; 3] {
    let unit = |a: Vector| a / a.size().max(1e-8);
    let x = unit(x);
    let y = unit(y - x * y.dot(x));
    [x, y, x.cross(y)]
}

/// A bone at 0 collapses its whole chain onto its joint. Parents come before their children.
fn scale_frames(frames: &mut [Frame], own: &[f32], parents: &[usize]) {
    let original = frames.to_vec();
    let mut changed = vec![false; frames.len()];
    for bone in 0..frames.len() {
        let parent = parents.get(bone).copied().filter(|&p| p < bone);
        if let Some(p) = parent.filter(|&p| changed[p])
            && let Some(local) = original[p].inverse().map(|inverse| inverse.then(&original[bone]))
        {
            frames[bone] = frames[p].then(&local);
            changed[bone] = true;
        }
        let s = own.get(bone).copied().unwrap_or(1.0);
        if s != 1.0 {
            frames[bone].axes = frames[bone].axes.map(|a| a.map(|c| c * s));
            changed[bone] = true;
        }
    }
}

fn frame_scale(frame: &Frame) -> f32 {
    let [x, y, z] = frame.axes[0];
    (x * x + y * y + z * z).sqrt().max(1e-6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orthonormal_keeps_x() {
        let [x, y, z] = orthonormal([[2.0, 0.0, 0.0].into(), [1.0, 3.0, 0.0].into(), [0.0; 3].into()]);
        assert_eq!((x, y, z), (Vector::new(1.0, 0.0, 0.0), Vector::new(0.0, 1.0, 0.0), Vector::new(0.0, 0.0, 1.0)));
    }

    #[test]
    fn a_scaled_bone_shrinks_its_chain_onto_its_joint() {
        let at = |x: f32| Frame { origin: [x, 0.0, 0.0], ..Frame::IDENTITY };
        let parents = [0, 0, 1, 0];
        let original = [at(0.0), at(10.0), at(20.0), at(5.0)];
        let mut frames = original;
        scale_frames(&mut frames, &[1.0, 0.5, 1.0, 1.0], &parents);
        assert_eq!(frames[2].origin, [15.0, 0.0, 0.0]);
        assert_eq!((frames[1].axes[0], frames[2].axes[0]), ([0.5, 0.0, 0.0], [0.5, 0.0, 0.0]));
        assert_eq!(frames[3], original[3]);
        let mut frames = original;
        scale_frames(&mut frames, &[1.0, 0.0, 1.0, 1.0], &parents);
        assert_eq!(frames[2].origin, [10.0, 0.0, 0.0]);
        assert_eq!(frames[2].axes, [[0.0; 3]; 3]);
    }
}

#[cfg(test)]
mod rig_tests {
    use super::*;
    use p2_assets::pose::{LocalBone, bone_frames, mesh_to_world};
    use p2_assets::{MeshAnimation, read_mesh_animation, read_skeletal_mesh};
    use p2_package::Package;
    use p2_physics::World;

    const GRAVITY: [f32; 3] = [0.0, 0.0, -950.0];
    const STANDING_HEIGHT: f32 = 72.0;
    const CROUCH_HEIGHT: f32 = 52.0;
    const FLOOR_GAP: f32 = 2.0;

    struct Rig {
        mesh: PosedMesh,
        animation: MeshAnimation,
        names: Vec<String>,
        tracks: Vec<Option<usize>>,
        asset: KarmaAsset,
    }

    fn rig() -> Option<Rig> {
        let dir = std::path::PathBuf::from(std::env::var_os("P2_GAME_DIR")?);
        let data = std::fs::read(dir.join("Animations").join("Characters.ukx")).ok()?;
        let package = Package::parse(&data).ok()?;
        let export = |name: &str| package.exports.iter().find(|e| package.name(e.object_name) == name);
        let read = read_skeletal_mesh(&package, &data, export("Avg_M_SS_Shorts_PB_D")?).ok()?;
        let lower = |index: usize| package.name(index).to_ascii_lowercase();
        let animation = package
            .exports
            .iter()
            .filter(|e| package.object_name(e.class_index) == Some("MeshAnimation"))
            .filter_map(|e| read_mesh_animation(&package, &data, e).ok())
            .find(|a| a.sequences.iter().any(|s| lower(s.name) == "c_beg1"))?;
        let names: Vec<String> = read.bones.iter().map(|b| lower(b.name)).collect();
        let parents = read.bones.iter().map(|b| b.parent).collect();
        let reference = read.bones.iter().enumerate().map(|(bone, b)| crate::bones::reference_frame(bone, b.orientation, b.position)).collect();
        let tracks = names.iter().map(|name| animation.bones.iter().position(|a| lower(a.name) == *name)).collect();
        let sequences = animation.sequences.iter().map(|s| lower(s.name)).collect();
        let mesh = PosedMesh { mesh: read, names, parents, aliases: Vec::new(), reference };
        let text = std::fs::read_to_string(dir.join("KarmaData").join("Avg_Dude.ka")).ok()?;
        let asset = read_karma_assets(&text).ok()?.into_iter().find(|a| a.id.eq_ignore_ascii_case("avg_dude_skel"))?;
        Some(Rig { mesh, animation, names: sequences, tracks, asset })
    }

    fn params(rig: &Rig, sequence: &str, fraction: f32, crouched: bool) -> Option<RagdollParams> {
        let index = rig.names.iter().position(|n| n == sequence)?;
        let frames = rig.animation.sequences[index].num_frames.max(1) as f32;
        let pose = rig.animation.pose_at(index, fraction * frames);
        let local: Vec<LocalBone> = rig.tracks.iter().zip(&rig.mesh.mesh.bones).map(|(&track, bone)| track.and_then(|t| pose.get(t)).unwrap_or((bone.orientation, bone.position))).collect();
        let bones = bone_frames(&rig.mesh.parents, &local, |_| 1.0);
        let (height, pre_pivot) = if crouched { (CROUCH_HEIGHT, STANDING_HEIGHT - CROUCH_HEIGHT) } else { (STANDING_HEIGHT, 0.0) };
        let location: Vector = [0.0, 0.0, height + FLOOR_GAP].into();
        let world = mesh_to_world(location, p2_math::Rotator::default(), [1.0; 3].into(), [0.0, 0.0, pre_pivot].into(), rig.mesh.mesh.scale, rig.mesh.mesh.origin, rig.mesh.mesh.rot_origin);
        let frames: Vec<Frame> = bones.iter().map(|frame| world.then(frame)).collect();
        let built = build(&rig.asset, &rig.mesh, &frames, [0.0; 3].into(), &[])?;
        Some(RagdollParams { parts: built.parts, joints: built.joint_params, linear_damping: 0.2, angular_damping: 0.2, friction: CORPSE_FRICTION, restitution: 0.0, asleep: false })
    }

    const CELL: f32 = 64.0;
    const CELLS: i32 = 60;

    fn bump(x: f32, y: f32, amplitude: f32) -> f32 {
        amplitude * ((x / 90.0).sin() + (y / 70.0).cos() + (x / 37.0 + y / 53.0).sin()) / 3.0
    }

    fn ground_height(x: f32, y: f32, amplitude: f32) -> f32 {
        let (cx, cy) = (x / CELL, y / CELL);
        let (i, j) = (cx.floor(), cy.floor());
        let (u, v) = (cx - i, cy - j);
        let corner = |a: f32, b: f32| bump((i + a) * CELL, (j + b) * CELL, amplitude);
        if u >= v {
            corner(0.0, 0.0) + (corner(1.0, 0.0) - corner(0.0, 0.0)) * u + (corner(1.0, 1.0) - corner(1.0, 0.0)) * v
        } else {
            corner(0.0, 0.0) + (corner(0.0, 1.0) - corner(0.0, 0.0)) * v + (corner(1.0, 1.0) - corner(0.0, 1.0)) * u
        }
    }

    fn ground(amplitude: f32) -> World {
        let mut world = World::new(GRAVITY.into());
        let side = CELLS * 2 + 1;
        let mut vertices = Vec::new();
        for j in -CELLS..=CELLS {
            for i in -CELLS..=CELLS {
                let (x, y) = (i as f32 * CELL, j as f32 * CELL);
                vertices.push([x, y, bump(x, y, amplitude)]);
            }
        }
        let at = |i: i32, j: i32| (j * side + i) as u32;
        let mut triangles = Vec::new();
        for j in 0..side - 1 {
            for i in 0..side - 1 {
                triangles.push([at(i, j), at(i + 1, j), at(i + 1, j + 1)]);
                triangles.push([at(i, j), at(i + 1, j + 1), at(i, j + 1)]);
            }
        }
        world.add_static_mesh(&vertices, triangles).unwrap();
        world
    }

    fn lowest_axis_end(world: &World, bodies: &[BodyId], params: &RagdollParams, amplitude: f32) -> f32 {
        let mut lowest = f32::MAX;
        for (&body, part) in bodies.iter().zip(&params.parts) {
            let (location, rotation) = world.pose(body).unwrap();
            let frame = Frame::from_quat(rotation, 1.0, location.into());
            for shape in &part.shapes {
                if let Shape::Capsule(a, b, _) = shape {
                    for end in [*a, *b] {
                        let p = frame.transform_point(end.into());
                        lowest = lowest.min(p[2] - ground_height(p[0], p[1], amplitude));
                    }
                }
            }
        }
        lowest
    }

    #[test]
    fn real_skeleton_hit_hard_keeps_its_limbs_above_the_ground() {
        let Some(rig) = rig() else { return };
        let mut seed = 12345u32;
        let mut unit = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            ((seed >> 16) & 0x7fff) as f32 / 16383.5 - 1.0
        };
        let (mut limbs_under, mut cases, mut worst) = (0, 0, f32::MAX);
        for sequence in ["c_beg1", "c_death", "s_fall_fore"] {
            for fraction in [0.25, 0.75] {
                for amplitude in [0.0, 15.0, 40.0] {
                    for _ in 0..6 {
                        let params = params(&rig, sequence, fraction, sequence.starts_with("c_")).expect("a sequence of this skeleton");
                        let mut world = ground(amplitude);
                        let bodies = world.add_ragdoll(&params);
                        for &body in &bodies {
                            world.set_velocity(body, [unit() * 900.0, unit() * 900.0, -300.0 - (unit() + 1.0) * 350.0].into());
                        }
                        for _ in 0..240 {
                            world.step(1.0 / 60.0);
                            for &b in &bodies {
                                world.limit_speed(b, MAX_SPEED);
                            }
                        }
                        let axis = lowest_axis_end(&world, &bodies, &params, amplitude);
                        cases += 1;
                        worst = worst.min(axis);
                        limbs_under += usize::from(axis < -1.0);
                    }
                }
            }
        }
        assert_eq!(limbs_under, 0, "{limbs_under} of {cases} corpses end with a limb under the ground, worst {worst}");
    }

    fn joint_gap(world: &World, bodies: &[BodyId], params: &RagdollParams) -> f32 {
        let at = |part: usize, anchor: Vector| {
            let (location, rotation) = world.pose(bodies[part]).unwrap();
            Frame::from_quat(rotation, 1.0, location.into()).transform_point(anchor.into())
        };
        params.joints.iter().map(|joint| {
            let (a, b) = (at(joint.child, joint.child_anchor), at(joint.parent, joint.parent_anchor));
            (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f32>().sqrt()
        }).fold(0.0, f32::max)
    }

    #[test]
    fn real_skeleton_stays_on_the_ground() {
        let Some(rig) = rig() else { return };
        let mut failures = Vec::new();
        let mut worst_gap = 0.0f32;
        for (sequence, crouched) in [("c_beg1", true), ("c_beg2", true), ("c_base_beg", true), ("c_death", true), ("s_fall_fore", false)] {
            for fraction in [0.0, 0.25, 0.5, 0.75] {
                for amplitude in [0.0, 15.0, 40.0] {
                    for shot in [0.0, 60.0, 300.0] {
                        let params = params(&rig, sequence, fraction, crouched).expect("a sequence of this skeleton");
                        let mut world = ground(amplitude);
                        let bodies = world.add_ragdoll(&params);
                        if shot > 0.0 {
                            let pelvis = bodies[1];
                            world.apply_impulse(pelvis, [shot, shot * 0.5, shot * 0.2].into(), None);
                        }
                        let height = |world: &World| bodies.iter().map(|&b| world.center_of_mass(b).unwrap()).map(|c| c[2] - ground_height(c[0], c[1], amplitude)).collect::<Vec<f32>>();
                        let start = height(&world);
                        let (mut top, mut rise, mut gap) = (0.0f32, 0.0f32, 0.0f32);
                        for _ in 0..420 {
                            world.step(1.0 / 60.0);
                            gap = gap.max(joint_gap(&world, &bodies, &params));
                            for &b in &bodies {
                                world.limit_speed(b, MAX_SPEED);
                                let v = world.velocity(b).unwrap();
                                top = top.max((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt());
                            }
                            rise = height(&world).iter().zip(&start).map(|(n, s)| n - s).fold(rise, f32::max);
                        }
                        let end = height(&world);
                        let axis = lowest_axis_end(&world, &bodies, &params, amplitude);
                        let under = end.iter().filter(|&&h| h < 0.0).count();
                        let lowest = end.iter().copied().fold(f32::MAX, f32::min);
                        let asleep = bodies.iter().all(|&b| world.is_sleeping(b));
                        let before: Vec<_> = bodies.iter().map(|&b| world.pose(b).unwrap()).collect();
                        world.wake_ragdoll(&bodies);
                        for _ in 0..30 {
                            world.step(1.0 / 60.0);
                        }
                        let turned = bodies.iter().zip(&before).map(|(&b, (_, q0))| {
                            let q1 = world.pose(b).unwrap().1;
                            2.0 * (0..4).map(|k| q0[k] * q1[k]).sum::<f32>().abs().min(1.0).acos().to_degrees()
                        }).fold(0.0f32, f32::max);
                        worst_gap = worst_gap.max(gap);
                        if under > 0 || lowest < 0.0 || axis < -1.0 || gap > 30.0 || (shot == 0.0 && top > 600.0) || (asleep && turned > 20.0) {
                            failures.push(format!("{sequence} {fraction} bumps {amplitude} shot {shot}: {under}/{} under, lowest {lowest:.1}, axis end {axis:.1}, top {top:.0}, rise {rise:.0}, gap {gap:.1}, asleep {asleep}, woken turn {turned:.1}", bodies.len()));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{} cases, worst joint gap {worst_gap}:\n{}", failures.len(), failures.join("\n"));
    }
}
