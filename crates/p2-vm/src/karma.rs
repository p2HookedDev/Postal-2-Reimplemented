mod shapes;

use std::collections::BTreeMap;
use std::sync::Arc;

use p2_assets::{KMeshProps, read_kmesh_props, read_static_mesh};
use p2_package::Package;
use p2_physics::{BodyId, BodyParams, Impact, Shape, StaticId, World};
use rustc_hash::{FxHashMap, FxHashSet};

use p2_math::{Rotator, Vector, quat_to_rotator, rotator_to_quat};
use p2_math::matrix_to_quat;
use crate::enums::EPhysics;
use crate::native_id::Native;
use crate::interp::float;
use crate::natives::{bool_arg, float_arg, vec3};
use crate::object_table::ObjectTable;
use crate::profiling::Work;
use crate::{Error, LoaderRef, ObjectId, Value, Vm, Prop};

/// The impulse is scaled to Karma units, then by 0.01: in Unreal units dv = impulse × 0.01 / mass.
pub(crate) const IMPULSE_SCALE: f32 = 0.01;
/// The force reaches Karma unscaled (Karma lengths are 1/50 of Unreal's), so in Unreal units
/// the acceleration is force × 50 / mass.
const MAX_KARMA_TICK: f32 = 0.05;
const OLD_CORPSE_TICKS: u32 = 1000;
const FORCE_SCALE: f32 = p2_physics::UNITS_PER_METER;
/// The torque reaches Karma unscaled; inertia in Unreal units is 50² times Karma's (unverified).
const TORQUE_SCALE: f32 = p2_physics::UNITS_PER_METER * p2_physics::UNITS_PER_METER;

#[derive(Default)]
pub(crate) struct Karma {
    pub(crate) world: Option<World>,
    impacts: Vec<Impact>,
    bodies: BTreeMap<ObjectId, Body>,
    pub(crate) actors: FxHashMap<BodyId, ObjectId>,
    pub(crate) ragdolls: crate::ragdoll::Ragdolls,
    pub(crate) limbs: crate::limbs::Limbs,
    pub(crate) boltons: crate::bolton::Boltons,
    failed: FxHashSet<ObjectId>,
    meshes: ObjectTable<Option<Arc<MeshShape>>>,
    movers: FxHashMap<ObjectId, Option<Body>>,
    statics: FxHashMap<ObjectId, Vec<StaticId>>,
}

impl Karma {
    pub(crate) fn for_next_level(&mut self) -> Self {
        Self { ragdolls: self.ragdolls.for_next_level(), ..Default::default() }
    }
}

impl<L: LoaderRef> Vm<L> {
    pub fn karma_census(&mut self) -> String {
        let (wakes, [moved, turned]) = self.karma.world.as_mut().map_or_else(Default::default, |world| (std::mem::take(&mut world.wakes), std::mem::take(&mut world.rest_broken)));
        let Some(world) = &self.karma.world else { return String::new() };
        let count = |bodies: &mut dyn Iterator<Item = (ObjectId, BodyId)>| {
            let (mut actors, mut parts, mut awake) = (FxHashSet::default(), 0, 0);
            for (actor, id) in bodies {
                actors.insert(actor);
                parts += 1;
                awake += usize::from(!world.is_sleeping(id));
            }
            (actors.len(), parts, awake)
        };
        let (ragdolls, parts, parts_awake) = count(&mut self.karma.ragdolls.bodies());
        let slow = self.karma.ragdolls.bodies().filter(|&(_, id)| !world.is_sleeping(id) && world.is_slow(id)).count();
        let (old, old_awake) = self.karma.ragdolls.bodies_older_than(OLD_CORPSE_TICKS).fold((0, 0), |(parts, awake), id| (parts + 1, awake + usize::from(!world.is_sleeping(id))));
        let (limbs, _, limbs_awake) = count(&mut self.karma.limbs.bodies());
        let flying = self.karma.limbs.bodies().filter(|&(actor, _)| self.store.physics_of(actor) == Some(EPhysics::Projectile)).count();
        let (others, _, others_awake) = count(&mut self.karma.bodies.iter().map(|(&actor, body)| (actor, body.id)));
        format!(
            "{ragdolls} ragdolls of {parts} parts ({parts_awake} awake, {slow} of them slow; over {OLD_CORPSE_TICKS} ticks old: {old} parts, {old_awake} awake), {wakes} wake calls, rest broken {moved} times by a move and {turned} by a turn, {limbs} limbs and bolt-ons ({limbs_awake} awake, {flying} flying), {others} other bodies ({others_awake} awake)"
        )
    }
}

/// A static mesh asking for simple Karma collision without `KPhysicsProps`: retail gives level meshes none, our
/// movers a hull.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WithoutProps {
    Skip,
    Hull,
}

#[derive(Clone, Copy)]
struct Body {
    id: BodyId,
    location: Vector,
    rotation: Rotator,
}

pub(crate) struct MeshShape {
    props: Option<KMeshProps>,
    use_simple_collision: bool,
    min: Vector,
    max: Vector,
    pub(crate) points: Vec<[f32; 3]>,
}

struct Params {
    mass: f32,
    linear_damping: f32,
    angular_damping: f32,
    start_enabled: bool,
    stay_upright: bool,
    allow_rotate: bool,
    friction: f32,
    restitution: f32,
}

impl<L: LoaderRef> Vm<L> {
    pub fn karma_actors(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.karma.bodies.keys().copied()
    }

    /// Runs before actors tick (unverified). Covers every level actor, static ones in `PHYS_Karma` too;
    /// `Physics` is read first, so an actor in no such mode costs one word.
    pub(crate) fn karma_tick(&mut self, delta: f32) -> Vec<(String, Error)> {
        let phase = self.profile_start();
        let (mut actors, mut ragdolls, mut flying) = (Vec::new(), Vec::new(), Vec::new());
        for o in self.actors() {
            let list = match self.store.physics_of(o) {
                Some(EPhysics::Karma) => &mut actors,
                Some(EPhysics::KarmaRagDoll) => &mut ragdolls,
                Some(EPhysics::Projectile) => &mut flying,
                _ => continue,
            };
            if self.store.is_a(o, crate::Kind::Actor) && !self.store.is_deleted(o) {
                list.push(o);
            }
        }
        let gone: Vec<ObjectId> = self.karma.bodies.keys().filter(|&&a| self.store.is_deleted(a) || self.store.physics_of(a) != Some(EPhysics::Karma)).copied().collect();
        for actor in gone {
            self.remove_karma_body(actor);
        }
        for &actor in &actors {
            self.karma_body(actor);
        }
        self.ragdolls_before_step(ragdolls);
        self.ragdoll_leftover_hits();
        let mut errors = self.limbs_before_step(flying, delta.min(MAX_KARMA_TICK));
        self.karma_movers_tick();
        self.karma_gravity();
        self.profile_phase("karma: before step", phase);
        let phase = self.profile_start();
        let Some(world) = &mut self.karma.world else { return errors };
        if self.karma.bodies.is_empty() && self.karma.ragdolls.is_empty() && self.karma.limbs.is_empty() {
            return errors;
        }
        let timed = self.diagnostics.count_work.then(std::time::Instant::now);
        world.step_into(delta.min(MAX_KARMA_TICK), &mut self.karma.impacts);
        if let Some(started) = timed {
            self.count_n(Work::KarmaStepMicros, usize::try_from(started.elapsed().as_micros()).unwrap_or(usize::MAX));
        }
        self.profile_phase("karma: step", phase);
        let phase = self.profile_start();
        let impacts = std::mem::take(&mut self.karma.impacts);
        let part = self.profile_start();
        errors.extend(self.ragdolls_after_step());
        self.profile_phase("karma: ragdolls after step", part);
        let part = self.profile_start();
        errors.extend(self.limbs_after_step());
        self.profile_phase("karma: limbs after step", part);
        let bodies: Vec<(ObjectId, BodyId)> = self.karma.bodies.iter().map(|(&actor, body)| (actor, body.id)).collect();
        for (actor, id) in bodies {
            if let Err(e) = self.follow_body(actor, id) {
                errors.push((self.store.object(actor).path.clone(), e));
            }
        }
        for impact in impacts {
            let Some(&actor) = self.karma.actors.get(&impact.body) else { continue };
            let threshold = self.store.object_of(actor, Prop::KParams).map_or(f32::MAX, |k| self.store.float_of(k, Prop::KImpactThreshold));
            let speed = impact.velocity.size();
            if self.store.is_deleted(actor) || speed <= threshold {
                continue;
            }
            let other = impact.other.and_then(|other| self.karma.actors.get(&other).copied());
            let args = vec![Value::Object(other), Value::Vector(impact.position), Value::Vector(impact.velocity), Value::Vector(impact.normal)];
            if let Err(e) = self.event(actor, crate::states::Event::KImpact, args) {
                errors.push((self.store.object(actor).path.clone(), e));
            }
        }
        self.profile_phase("karma: after step", phase);
        errors
    }

    /// Entering `PHYS_Karma` makes the body now, so `KAddImpulse` right after it reaches the body,
    /// and retries an actor that had none.
    pub(crate) fn karma_physics_changed(&mut self, actor: ObjectId, old: Option<EPhysics>) {
        if old == Some(EPhysics::Karma) {
            self.remove_karma_body(actor);
        }
        self.ragdoll_physics_changed(actor, old);
        if self.store.physics_of(actor) == Some(EPhysics::Karma) {
            self.karma.failed.remove(&actor);
            self.karma_body(actor);
        }
    }

    fn follow_body(&mut self, actor: ObjectId, id: BodyId) -> Result<(), Error> {
        let Some(world) = self.karma.world.as_mut() else { return Ok(()) };
        if world.is_sleeping(id) {
            self.set_vector(actor, Prop::Velocity, [0.0; 3].into());
            return Ok(());
        }
        let max_speed = self.store.get(actor, Prop::KarmaMaxSpeed).as_ref().map_or(2500.0, float);
        let Some(world) = self.karma.world.as_mut() else { return Ok(()) };
        world.limit_speed(id, max_speed);
        let (Some((location, quat)), Some(velocity)) = (world.pose(id), world.velocity(id)) else { return Ok(()) };
        let rotation = quat_to_rotator(quat);
        self.set_vector(actor, Prop::Velocity, velocity);
        let shown = (self.store.vector(actor, Prop::Location), self.store.rotator_of(actor, Prop::Rotation));
        // As retail's move of a Karma actor: blockers in the way are pushed and told, and a refusal leaves it in place.
        if shown != (location, rotation) && self.check_encroachment(actor, location, rotation)? {
            return Ok(());
        }
        self.set_vector(actor, Prop::Location, location);
        self.store.set(actor, Prop::Rotation, Value::Rotator(rotation));
        self.relocate(actor);
        let moved = self.karma.bodies.get_mut(&actor).is_some_and(|body| {
            let moved = body.location != location;
            (body.location, body.rotation) = (location, rotation);
            moved
        });
        if !moved {
            return Ok(());
        }
        self.update_touching(actor)?;
        self.set_zone(actor, crate::volumes::ZoneEvents::Sent, crate::volumes::OldZone::Region)
    }

    fn karma_body(&mut self, actor: ObjectId) -> Option<BodyId> {
        if self.store.physics_of(actor) != Some(EPhysics::Karma) || self.store.is_deleted(actor) || self.karma.failed.contains(&actor) {
            return None;
        }
        let (location, rotation) = (self.store.vector(actor, Prop::Location), self.store.rotator_of(actor, Prop::Rotation));
        if let Some(body) = self.karma.bodies.get_mut(&actor) {
            let id = body.id;
            if (body.location, body.rotation) != (location, rotation) {
                (body.location, body.rotation) = (location, rotation);
                if let Some(world) = &mut self.karma.world {
                    let quat = rotator_to_quat(rotation);
                    world.set_pose(id, location, quat);
                }
            }
            return Some(id);
        }
        let Some(params) = self.karma_params(actor) else {
            self.diagnostics.log.push(format!("(Karma:) physKarma: Actor has no KParams: {}", self.store.object(actor).path));
            self.karma.failed.insert(actor);
            return None;
        };
        let shapes = self.karma_shapes(actor);
        let body = BodyParams {
            shapes,
            location,
            rotation: rotator_to_quat(rotation),
            mass: params.mass,
            linear_damping: params.linear_damping,
            angular_damping: params.angular_damping,
            friction: params.friction,
            restitution: params.restitution,
            asleep: !params.start_enabled,
            stay_upright: params.stay_upright,
            allow_yaw: params.allow_rotate,
        };
        self.karma_world();
        let world = self.karma.world.as_mut()?;
        let id = world.add_body(&body);
        self.karma.bodies.insert(actor, Body { id, location, rotation });
        self.karma.actors.insert(id, actor);
        Some(id)
    }

    pub(crate) fn karma_world(&mut self) {
        if self.karma.world.is_some() {
            return;
        }
        let phase = self.profile_start();
        let mut world = World::new(crate::physics::DEFAULT_GRAVITY);
        for crate::world::LevelMesh { vertices, triangles } in self.level_meshes() {
            if let Err(e) = world.add_static_mesh(&vertices, triangles) {
                self.diagnostics.log.push(format!("Karma level geometry: {e}"));
            }
        }
        self.karma.world = Some(world);
        let models: Vec<ObjectId> = self
            .actors()
            .filter(|&a| !self.store.is_deleted(a) && (self.store.is_a(a, crate::Kind::Mover) || self.store.flag_of(a, Prop::bBlockKarma)))
            .collect();
        for actor in models {
            self.karma_update_model(actor);
        }
        self.profile_phase("load: karma world", phase);
    }

    /// As retail, any actor with `bBlockKarma` has a Karma model, except that pawns get none here. A static one is
    /// level geometry; a mover or other non-static one is a kinematic body that follows the actor.
    fn karma_update_model(&mut self, actor: ObjectId) {
        if let Some(ids) = self.karma.statics.remove(&actor)
            && let Some(world) = &mut self.karma.world
        {
            for id in ids {
                world.remove_static(id);
            }
        }
        let volume = self.store.is_a(actor, crate::Kind::BlockingVolume);
        if self.store.is_a(actor, crate::Kind::Pawn) {
            return;
        }
        if !volume && (self.store.is_a(actor, crate::Kind::Mover) || !self.store.flag_of(actor, Prop::bStatic)) {
            if let Some(Some(body)) = self.karma.movers.insert(actor, None)
                && let Some(world) = &mut self.karma.world
            {
                world.remove_body(body.id);
            }
            return;
        }
        if !self.store.flag_of(actor, Prop::bBlockKarma) || matches!(self.store.physics_of(actor), Some(EPhysics::Karma | EPhysics::KarmaRagDoll)) {
            return;
        }
        let added: Vec<_> = if volume {
            let Some(points) = self.karma_blocking_hull(actor) else { return };
            let Some(world) = &mut self.karma.world else { return };
            vec![world.add_static_hull(&points)]
        } else {
            if self.store.object_of(actor, Prop::StaticMesh).is_none() {
                return;
            }
            let Some((shapes, location, rotation)) = self.karma_placed_shapes(actor, WithoutProps::Skip) else { return };
            let Some(world) = &mut self.karma.world else { return };
            shapes.iter().map(|shape| world.add_static_shape(shape, location, rotation)).collect()
        };
        let mut ids = Vec::new();
        for result in added {
            match result {
                Ok(id) => ids.push(id),
                Err(e) => self.diagnostics.log.push(format!("Karma static model {}: {e}", self.store.object(actor).path)),
            }
        }
        if !ids.is_empty() {
            self.karma.statics.insert(actor, ids);
        }
    }

    fn karma_follower_blocks(&self, actor: ObjectId) -> bool {
        if self.store.is_deleted(actor) {
            return false;
        }
        if self.store.is_a(actor, crate::Kind::Mover) {
            return self.store.flag_of(actor, Prop::bCollideActors) && self.store.flag_of(actor, Prop::bBlockActors);
        }
        self.store.flag_of(actor, Prop::bBlockKarma)
            && self.store.object_of(actor, Prop::StaticMesh).is_some()
            && !matches!(self.store.physics_of(actor), Some(EPhysics::Karma | EPhysics::KarmaRagDoll))
    }

    fn karma_gravity(&mut self) {
        let base = crate::physics::DEFAULT_GRAVITY.z;
        let props = self.karma.bodies.iter().map(|(&actor, body)| (actor, body.id));
        let pulls: Vec<(BodyId, Vector)> = props.chain(self.karma.ragdolls.bodies()).map(|(actor, id)| (id, self.gravity_of(actor))).collect();
        let Some(world) = &mut self.karma.world else { return };
        for (id, pull) in pulls {
            world.set_gravity_scale(id, pull.z / base);
            if pull.x != 0.0 || pull.y != 0.0 {
                world.add_acceleration(id, Vector::new(pull.x, pull.y, 0.0));
            }
        }
    }

    /// Unlike retail, every mover that blocks actors blocks Karma bodies, not only those with `bBlockKarma`.
    fn karma_movers_tick(&mut self) {
        let movers: Vec<ObjectId> = self.karma.movers.keys().copied().collect();
        for mover in movers {
            let deleted = self.store.is_deleted(mover);
            let blocks = self.karma_follower_blocks(mover);
            let (location, rotation) = (self.store.vector(mover, Prop::Location), self.store.rotator_of(mover, Prop::Rotation));
            let body = match (self.karma.movers.get(&mover).copied().flatten(), blocks) {
                (Some(body), false) => {
                    if let Some(world) = &mut self.karma.world {
                        world.remove_body(body.id);
                    }
                    None
                }
                (Some(body), true) => {
                    if (body.location, body.rotation) != (location, rotation)
                        && let Some(world) = &mut self.karma.world
                    {
                        let quat = matrix_to_quat(rotation.axes().map(Vector::from));
                        world.move_mover(body.id, location, quat);
                    }
                    Some(Body { id: body.id, location, rotation })
                }
                (None, true) => {
                    let Some(body) = self.karma_mover_body(mover) else {
                        self.karma.movers.remove(&mover);
                        continue;
                    };
                    Some(body)
                }
                (None, false) => None,
            };
            if deleted {
                self.karma.movers.remove(&mover);
            } else {
                self.karma.movers.insert(mover, body);
            }
        }
    }

    pub(crate) fn karma_mover_spawned(&mut self, actor: ObjectId) {
        if self.karma.world.is_some() && (self.store.is_a(actor, crate::Kind::Mover) || self.store.flag_of(actor, Prop::bBlockKarma)) {
            self.karma_update_model(actor);
        }
    }

    /// With `bBlockKarma`, retail's Karma shapes; blocking only by our rule, the triangles pawns collide with.
    fn karma_mover_body(&mut self, mover: ObjectId) -> Option<Body> {
        let path = self.store.object(mover).path.clone();
        let shapes = if self.store.flag_of(mover, Prop::bBlockKarma) {
            let without_props = if self.store.is_a(mover, crate::Kind::Mover) { WithoutProps::Hull } else { WithoutProps::Skip };
            self.karma_placed_shapes(mover, without_props)
        } else {
            self.body_triangles(mover).filter(|(triangles, _)| !triangles.is_empty()).map(|(triangles, placement)| {
                (vec![Shape::Mesh(triangles)], placement.location, matrix_to_quat(placement.rotation.axes().map(Vector::from)))
            })
        };
        let Some((shapes, location, quat)) = shapes else {
            self.diagnostics.log.push(format!("Karma mover {path}: no collision shape"));
            return None;
        };
        let added = self.karma.world.as_mut()?.add_mover(&shapes, location, quat);
        match added {
            Ok(id) => Some(Body { id, location, rotation: self.store.rotator_of(mover, Prop::Rotation) }),
            Err(e) => {
                self.diagnostics.log.push(format!("Karma mover {path}: {e}"));
                None
            }
        }
    }

    fn remove_karma_body(&mut self, actor: ObjectId) {
        if let Some(body) = self.karma.bodies.remove(&actor) {
            self.karma.actors.remove(&body.id);
            if let Some(world) = &mut self.karma.world {
                world.remove_body(body.id);
            }
        }
    }

    fn karma_params(&self, actor: ObjectId) -> Option<Params> {
        let k = self.store.object_of(actor, Prop::KParams)?;
        let or = |name: Prop, default: f32| self.store.get(k, name).as_ref().map_or(default, float);
        Some(Params {
            mass: or(Prop::KMass, 1.0).max(1e-3),
            linear_damping: or(Prop::KLinearDamping, 0.2),
            angular_damping: or(Prop::KAngularDamping, 0.2),
            start_enabled: self.store.flag_of(k, Prop::KStartEnabled),
            stay_upright: self.store.flag_of(k, Prop::bKStayUpright),
            allow_rotate: self.store.flag_of(k, Prop::bKAllowRotate),
            friction: or(Prop::KFriction, 0.7),
            restitution: or(Prop::KRestitution, 0.3),
        })
    }

    /// As retail, never by triangles and whatever the draw type: `KPhysicsProps` shapes with simple collision,
    /// otherwise one convex hull. A brush is one hull of its corners, so a concave brush is filled in.
    fn karma_placed_shapes(&mut self, actor: ObjectId, without_props: WithoutProps) -> Option<(Vec<Shape>, Vector, [f32; 4])> {
        let placement = self.drawn_placement(actor);
        let place = |p: &[f32; 3]| placement.in_body(*p);
        let shapes = if let Some(mesh) = self.store.object_of(actor, Prop::StaticMesh) {
            let mesh = self.karma_mesh(mesh)?;
            match (mesh.use_simple_collision, &mesh.props) {
                (true, Some(props)) => shapes::mesh_shapes(Some(props), (mesh.min, mesh.max), placement.scale, placement.pre_pivot),
                (true, None) if without_props == WithoutProps::Skip => return None,
                _ => vec![Shape::Hull(mesh.points.iter().map(place).collect())],
            }
        } else {
            let model = self.store.object_of(actor, Prop::Brush).and_then(|brush| self.brush_model(brush))?;
            vec![Shape::Hull(crate::world::brush_corners(&model).map(|p| place(&p)).collect())]
        };
        Some((shapes, placement.location, matrix_to_quat(placement.rotation.axes().map(Vector::from))))
    }

    fn karma_shapes(&mut self, actor: ObjectId) -> Vec<Shape> {
        let placement = self.drawn_placement(actor);
        let mesh = self.store.object_of(actor, Prop::StaticMesh).and_then(|mesh| self.karma_mesh(mesh));
        let Some(mesh) = mesh else {
            let (r, h) = self.store.collision_size(actor);
            return vec![Shape::Box([r.max(1.0), r.max(1.0), h.max(1.0)].into())];
        };
        shapes::mesh_shapes(mesh.props.as_ref(), (mesh.min, mesh.max), placement.scale, placement.pre_pivot)
    }

    pub(crate) fn karma_mesh(&mut self, mesh: ObjectId) -> Option<Arc<MeshShape>> {
        if let Some(shape) = self.karma.meshes.get(mesh) {
            return shape.clone();
        }
        let path = self.store.object(mesh).path.clone();
        let read = |pkg: &Package, data: &[u8], export: usize| {
            let e = pkg.exports.get(export)?;
            let m = read_static_mesh(pkg, data, e).ok()?;
            let reference = m.physics_props;
            let props = usize::try_from(reference - 1).ok().and_then(|i| pkg.exports.get(i));
            let props = props.and_then(|p| read_kmesh_props(pkg, data, p).ok());
            let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
            for p in &m.positions {
                for i in 0..3 {
                    min[i] = min[i].min(p[i]);
                    max[i] = max[i].max(p[i]);
                }
            }
            (!m.positions.is_empty()).then(|| Arc::new(MeshShape { props, use_simple_collision: m.use_simple_karma_collision, min: min.into(), max: max.into(), points: m.positions }))
        };
        let shape = match self.loader.loaded_export_by_path_of_class(&path, "StaticMesh") {
            Some((package, export)) => {
                let loaded = self.loader.package(package);
                read(&loaded.package, &loaded.data, export)
            }
            None => self.side_export(&path).and_then(|(side, export)| read(&side.package, &side.data, export)),
        };
        self.karma.meshes.insert(mesh, shape.clone());
        shape
    }

    pub(crate) fn karma_native(&mut self, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let Some(this) = this else { return Ok(None) };
        if let Some(result) = self.ragdoll_native(native, this, args) {
            return result;
        }
        let params = self.store.object_of(this, Prop::KParams);
        let body = self.karma_body(this);
        if body_command(&mut self.karma.world, native, body, args) {
            return Ok(None);
        }
        match native {
            Native::KSetBlockKarma => {
                let block = bool_arg(args, 0);
                if block != self.store.flag_of(this, Prop::bBlockKarma) && !self.store.is_a(this, crate::Kind::KConstraint) {
                    self.store.set(this, Prop::bBlockKarma, Value::Bool(block));
                    if self.karma.world.is_some() {
                        self.karma_update_model(this);
                    }
                }
            }
            Native::KEnableCollision | Native::KDisableCollision => {
                let other = match args.first() {
                    Some(&Value::Object(Some(other))) => self.karma_body(other),
                    _ => None,
                };
                if let (Some(world), Some(a), Some(other)) = (self.karma.world.as_mut(), body, other) {
                    let enabled = native == Native::KEnableCollision;
                    world.set_pair_collision(a, other, enabled);
                }
            }
            Native::KGetCOMPosition => {
                let centre = match (self.karma.world.as_mut(), body) {
                    (Some(w), Some(id)) => w.center_of_mass(id),
                    _ => None,
                };
                let centre = centre.unwrap_or_else(|| self.store.vector(this, Prop::Location));
                if let Some(out) = args.first_mut() {
                    *out = Value::Vector(centre);
                }
            }
            _ => return self.karma_params_native(native, params, body, args),
        }
        Ok(None)
    }

    fn karma_params_native(&mut self, native: Native, params: Option<ObjectId>, body: Option<BodyId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let param = |vm: &Self, name: Prop| params.map_or(0.0, |k| vm.store.float_of(k, name));
        let set_param = |vm: &mut Self, name: Prop, value: Value| {
            if let Some(k) = params {
                vm.store.set(k, name, value);
            }
        };
        let world = &mut self.karma.world;
        let value = match native {
            Native::KSetMass => {
                let mass = float_arg(args, 0).max(1e-3);
                change_body(world, body, move |w, id| w.set_mass(id, mass));
                set_param(self, Prop::KMass, Value::Float(mass));
                None
            }
            Native::KGetMass => Some(Value::Float(match (world.as_mut(), body) {
                (Some(w), Some(id)) => w.mass(id).unwrap_or(0.0),
                _ => param(self, Prop::KMass),
            })),
            Native::KSetFriction => {
                let friction = float_arg(args, 0);
                change_body(world, body, move |w, id| w.set_friction(id, friction));
                set_param(self, Prop::KFriction, Value::Float(friction));
                None
            }
            Native::KSetRestitution => {
                let restitution = float_arg(args, 0);
                change_body(world, body, move |w, id| w.set_restitution(id, restitution));
                set_param(self, Prop::KRestitution, Value::Float(restitution));
                None
            }
            Native::KSetDampingProps => {
                let (linear, angular) = (float_arg(args, 0), float_arg(args, 1));
                change_body(&mut self.karma.world, body, move |w, id| w.set_damping(id, linear, angular));
                set_param(self, Prop::KLinearDamping, Value::Float(linear));
                set_param(self, Prop::KAngularDamping, Value::Float(angular));
                None
            }
            Native::KGetDampingProps => {
                for (slot, property) in [(0, Prop::KLinearDamping), (1, Prop::KAngularDamping)] {
                    if let Some(out) = args.get_mut(slot) {
                        *out = Value::Float(param(self, property));
                    }
                }
                None
            }
            Native::KGetFriction => Some(Value::Float(param(self, Prop::KFriction))),
            Native::KGetRestitution => Some(Value::Float(param(self, Prop::KRestitution))),
            Native::KSetImpactThreshold => {
                set_param(self, Prop::KImpactThreshold, Value::Float(float_arg(args, 0)));
                None
            }
            Native::KGetImpactThreshold => Some(Value::Float(param(self, Prop::KImpactThreshold))),
            Native::KSetStayUpright => {
                let (upright, allow) = (bool_arg(args, 0), bool_arg(args, 1));
                change_body(world, body, move |w, id| w.set_stay_upright(id, upright, allow));
                set_param(self, Prop::bKStayUpright, Value::Bool(upright));
                set_param(self, Prop::bKAllowRotate, Value::Bool(allow));
                None
            }
            _ => return Err(Error::UnknownNative(format!("{native:?}"))),
        };
        Ok(value)
    }
}

fn body_command(world: &mut Option<World>, native: Native, body: Option<BodyId>, args: &[Value]) -> bool {
    let v = |n: usize| vec3(args.get(n));
    match native {
        Native::KAddImpulse => {
            let (impulse, at) = (v(0).map(|c| c * IMPULSE_SCALE), v(1));
            change_body(world, body, move |w, id| w.apply_impulse(id, impulse, Some(at)));
        }
        // An omitted or zero position pushes through the centre of mass. The engine passes the position to Karma
        // unscaled (Unreal units where Karma's are expected); here it is used as a world point.
        Native::KAddForce => {
            let force = v(0).map(|c| c * FORCE_SCALE);
            let at = Some(v(1)).filter(|p| *p != [0.0; 3].into());
            change_body(world, body, move |w, id| w.add_force(id, force, at));
        }
        Native::KAddTorque => {
            let torque = v(0).map(|c| c * TORQUE_SCALE);
            change_body(world, body, move |w, id| w.add_torque(id, torque));
        }
        Native::KWake => change_body(world, body, World::wake),
        // `KDisableKarma` makes the body sleep (unverified).
        Native::KDisableKarma => change_body(world, body, World::sleep),
        _ => return false,
    }
    true
}

fn change_body(world: &mut Option<World>, body: Option<BodyId>, change: impl FnOnce(&mut World, BodyId)) {
    if let (Some(world), Some(id)) = (world.as_mut(), body) {
        change(world, id);
    }
}
