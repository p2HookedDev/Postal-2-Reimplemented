//! The step runs on its own thread: scripts see results a step late; body changes queue until it ends.

mod shapes;

use std::collections::{BTreeMap, HashSet};
use std::f32::consts::PI;
use std::sync::Arc;

use p2_assets::{KMeshProps, read_kmesh_props, read_static_mesh};
use p2_package::Package;
use p2_physics::{Background, BodyId, BodyParams, Shape};
use rustc_hash::FxHashMap;

use p2_math::{Rotator, Vector};
use crate::enums::EPhysics;
use crate::native_id::Native;
use crate::interp::float;
use crate::natives::vec3;
use crate::physics::DEFAULT_GRAVITY;
use crate::profiling::Work;
use crate::{Error, LoaderRef, ObjectId, Value, Vm, Prop};

/// The impulse is scaled to Karma units, then by 0.01: in Unreal units dv = impulse × 0.01 / mass.
pub(crate) const IMPULSE_SCALE: f32 = 0.01;
/// The force reaches Karma unscaled (Karma lengths are 1/50 of Unreal's), so in Unreal units
/// the acceleration is force × 50 / mass.
const MAX_KARMA_TICK: f32 = 0.05;
const FORCE_SCALE: f32 = 50.0;
/// The torque reaches Karma unscaled; inertia in Unreal units is 50² times Karma's (unverified).
const TORQUE_SCALE: f32 = 2500.0;

#[derive(Default)]
pub(crate) struct Karma {
    pub(crate) world: Option<Background>,
    bodies: BTreeMap<ObjectId, Body>,
    pub(crate) actors: FxHashMap<BodyId, ObjectId>,
    pub(crate) ragdolls: crate::ragdoll::Ragdolls,
    pub(crate) limbs: crate::limbs::Limbs,
    failed: HashSet<ObjectId>,
    meshes: FxHashMap<ObjectId, Option<Arc<MeshShape>>>,
}

impl Karma {
    pub(crate) fn for_next_level(&mut self) -> Self {
        Self { ragdolls: self.ragdolls.for_next_level(), ..Default::default() }
    }
}

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
        let mut errors = self.karma_results();
        let (mut actors, mut ragdolls, mut flying) = (Vec::new(), Vec::new(), Vec::new());
        for o in self.actors() {
            let list = match self.physics_of(o) {
                Some(EPhysics::Karma) => &mut actors,
                Some(EPhysics::KarmaRagDoll) => &mut ragdolls,
                Some(EPhysics::Projectile) => &mut flying,
                _ => continue,
            };
            if self.is_a(o, crate::Kind::Actor) && !self.is_deleted(o) {
                list.push(o);
            }
        }
        let gone: Vec<ObjectId> = self.karma.bodies.keys().filter(|&&a| self.is_deleted(a) || self.physics_of(a) != Some(EPhysics::Karma)).copied().collect();
        for actor in gone {
            self.remove_karma_body(actor);
        }
        for &actor in &actors {
            self.karma_body(actor);
        }
        self.ragdolls_before_step(ragdolls);
        self.ragdoll_leftover_hits();
        errors.extend(self.limbs_before_step(flying, delta.min(MAX_KARMA_TICK)));
        if let Some(world) = &mut self.karma.world
            && (!self.karma.bodies.is_empty() || !self.karma.ragdolls.is_empty() || !self.karma.limbs.is_empty())
        {
            world.start(delta.min(MAX_KARMA_TICK));
        }
        errors
    }

    fn karma_results(&mut self) -> Vec<(String, Error)> {
        let Some(world) = &mut self.karma.world else { return Vec::new() };
        let impacts = world.take_impacts();
        let (waits, waited) = world.take_waits();
        self.count_n(Work::KarmaWait, waits as usize);
        self.count_n(Work::KarmaWaitMicros, usize::try_from(waited.as_micros()).unwrap_or(usize::MAX));
        let mut errors = self.ragdolls_after_step();
        errors.extend(self.limbs_after_step());
        let bodies: Vec<(ObjectId, BodyId)> = self.karma.bodies.iter().map(|(&actor, body)| (actor, body.id)).collect();
        for (actor, id) in bodies {
            if let Err(e) = self.follow_body(actor, id) {
                errors.push((self.object(actor).path.clone(), e));
            }
        }
        for impact in impacts {
            let Some(&actor) = self.karma.actors.get(&impact.body) else { continue };
            let threshold = self.object_of(actor, Prop::KParams).map_or(f32::MAX, |k| self.float_of(k, Prop::KImpactThreshold));
            let speed = impact.velocity.size();
            if self.is_deleted(actor) || speed <= threshold {
                continue;
            }
            let other = impact.other.and_then(|other| self.karma.actors.get(&other).copied());
            let args = vec![Value::Object(other), Value::Vector(impact.position), Value::Vector(impact.velocity), Value::Vector(impact.normal)];
            if let Err(e) = self.event(actor, "KImpact", args) {
                errors.push((self.object(actor).path.clone(), e));
            }
        }
        errors
    }

    /// Entering `PHYS_Karma` makes the body now, so `KAddImpulse` right after it reaches the body,
    /// and retries an actor that had none.
    pub(crate) fn karma_physics_changed(&mut self, actor: ObjectId, old: Option<EPhysics>) {
        if old == Some(EPhysics::Karma) {
            self.remove_karma_body(actor);
        }
        self.ragdoll_physics_changed(actor, old);
        if self.physics_of(actor) == Some(EPhysics::Karma) {
            self.karma.failed.remove(&actor);
            self.karma_body(actor);
        }
    }

    fn follow_body(&mut self, actor: ObjectId, id: BodyId) -> Result<(), Error> {
        let Some(world) = self.karma.world.as_mut().and_then(Background::world) else { return Ok(()) };
        if world.is_sleeping(id) {
            self.set_vector(actor, Prop::Velocity, [0.0; 3].into());
            return Ok(());
        }
        let max_speed = self.get(actor, Prop::KarmaMaxSpeed).as_ref().map_or(2500.0, float);
        let Some(world) = self.karma.world.as_mut().and_then(Background::world) else { return Ok(()) };
        world.limit_speed(id, max_speed);
        let (Some((location, quat)), Some(velocity)) = (world.pose(id), world.velocity(id)) else { return Ok(()) };
        let rotation = quat_to_rotator(quat);
        self.set_vector(actor, Prop::Location, location);
        self.set(actor, Prop::Rotation, Value::Rotator(rotation));
        self.relocate(actor);
        self.set_vector(actor, Prop::Velocity, velocity);
        let moved = self.karma.bodies.get_mut(&actor).is_some_and(|body| {
            let moved = body.location != location;
            (body.location, body.rotation) = (location, rotation);
            moved
        });
        if moved { self.update_touching(actor) } else { Ok(()) }
    }

    fn karma_body(&mut self, actor: ObjectId) -> Option<BodyId> {
        if self.physics_of(actor) != Some(EPhysics::Karma) || self.is_deleted(actor) || self.karma.failed.contains(&actor) {
            return None;
        }
        let (location, rotation) = (self.vector(actor, Prop::Location), self.rotator_of(actor, Prop::Rotation));
        if let Some(body) = self.karma.bodies.get_mut(&actor) {
            let id = body.id;
            if (body.location, body.rotation) != (location, rotation) {
                (body.location, body.rotation) = (location, rotation);
                if let Some(world) = &mut self.karma.world {
                    let quat = rotator_to_quat(rotation);
                    world.change(move |w| w.set_pose(id, location, quat));
                }
            }
            return Some(id);
        }
        let Some(params) = self.karma_params(actor) else {
            self.diagnostics.log.push(format!("(Karma:) physKarma: Actor has no KParams: {}", self.object(actor).path));
            self.karma.failed.insert(actor);
            return None;
        };
        let shapes = self.karma_shapes(actor);
        let body = BodyParams {
            shapes,
            location,
            rotation: rotator_to_quat(rotation),
            mass: params.mass,
            // Karma damps with a force, −damping × velocity (unverified); Rapier's damping is per mass.
            linear_damping: params.linear_damping / params.mass,
            angular_damping: params.angular_damping / params.mass,
            friction: params.friction,
            restitution: params.restitution,
            asleep: !params.start_enabled,
            stay_upright: params.stay_upright,
            allow_yaw: params.allow_rotate,
        };
        self.karma_world(actor);
        let world = self.karma.world.as_mut()?.world()?;
        let id = world.add_body(&body);
        self.karma.bodies.insert(actor, Body { id, location, rotation });
        self.karma.actors.insert(id, actor);
        Some(id)
    }

    /// One gravity for all bodies: this actor's physics volume's. Retail applies each actor's own.
    pub(crate) fn karma_world(&mut self, actor: ObjectId) {
        if self.karma.world.is_some() {
            return;
        }
        let phase = self.profile_start();
        let gravity = self.object_of(actor, Prop::PhysicsVolume).map_or(DEFAULT_GRAVITY, |v| self.vector(v, Prop::Gravity));
        let mut world = p2_physics::World::new(gravity);
        for crate::world::LevelMesh { vertices, triangles } in self.level_meshes() {
            if let Err(e) = world.add_static_mesh(&vertices, triangles) {
                self.diagnostics.log.push(format!("Karma level geometry: {e}"));
            }
        }
        for points in self.karma_blocking_hulls() {
            if let Err(e) = world.add_static_hull(&points) {
                self.diagnostics.log.push(format!("Karma blocking volume: {e}"));
            }
        }
        for (shape, location, rotation) in self.karma_static_shapes() {
            if let Err(e) = world.add_static_shape(&shape, location, rotation) {
                self.diagnostics.log.push(format!("Karma static mesh: {e}"));
            }
        }
        self.karma.world = Some(Background::new(world));
        self.profile_phase("load: karma world", phase);
    }

    fn remove_karma_body(&mut self, actor: ObjectId) {
        if let Some(body) = self.karma.bodies.remove(&actor) {
            self.karma.actors.remove(&body.id);
            if let Some(world) = &mut self.karma.world {
                world.change(move |w| w.remove_body(body.id));
            }
        }
    }

    fn karma_params(&self, actor: ObjectId) -> Option<Params> {
        let k = self.object_of(actor, Prop::KParams)?;
        let or = |name: Prop, default: f32| self.get(k, name).as_ref().map_or(default, float);
        Some(Params {
            mass: or(Prop::KMass, 1.0).max(1e-3),
            linear_damping: or(Prop::KLinearDamping, 0.0),
            angular_damping: or(Prop::KAngularDamping, 0.0),
            start_enabled: self.flag_of(k, Prop::KStartEnabled),
            stay_upright: self.flag_of(k, Prop::bKStayUpright),
            allow_rotate: self.flag_of(k, Prop::bKAllowRotate),
            friction: or(Prop::KFriction, 0.7),
            restitution: or(Prop::KRestitution, 0.3),
        })
    }

    /// As retail, never by triangles: `KPhysicsProps` shapes with simple collision, otherwise one convex hull.
    fn karma_static_shapes(&mut self) -> Vec<(Shape, Vector, [f32; 4])> {
        let actors: Vec<ObjectId> = self
            .actors()
            .filter(|&a| !self.is_deleted(a) && self.flag_of(a, Prop::bStatic) && self.flag_of(a, Prop::bBlockKarma))
            .filter(|&a| self.draw_type_of(a) == Some(crate::enums::EDrawType::StaticMesh))
            .collect();
        let mut placed = Vec::new();
        for actor in actors {
            let Some(mesh) = self.object_of(actor, Prop::StaticMesh).and_then(|mesh| self.karma_mesh(mesh)) else { continue };
            let placement = self.drawn_placement(actor);
            let shapes = match (mesh.use_simple_collision, &mesh.props) {
                (true, Some(props)) => shapes::mesh_shapes(Some(props), (mesh.min, mesh.max), placement.scale, placement.pre_pivot),
                (true, None) => continue,
                (false, _) => {
                    let place = |p: &[f32; 3]| [0, 1, 2].map(|i| (p[i] - placement.pre_pivot[i]) * placement.scale[i]);
                    vec![Shape::Hull(mesh.points.iter().map(place).collect())]
                }
            };
            let rotation = matrix_to_quat(placement.rotation.axes().map(Vector::from));
            placed.extend(shapes.into_iter().map(|shape| (shape, placement.location, rotation)));
        }
        placed
    }

    fn karma_shapes(&mut self, actor: ObjectId) -> Vec<Shape> {
        let placement = self.drawn_placement(actor);
        let mesh = self.object_of(actor, Prop::StaticMesh).and_then(|mesh| self.karma_mesh(mesh));
        let Some(mesh) = mesh else {
            let (r, h) = (self.float_of(actor, Prop::CollisionRadius), self.float_of(actor, Prop::CollisionHeight));
            return vec![Shape::Box([r.max(1.0), r.max(1.0), h.max(1.0)].into())];
        };
        shapes::mesh_shapes(mesh.props.as_ref(), (mesh.min, mesh.max), placement.scale, placement.pre_pivot)
    }

    pub(crate) fn karma_mesh(&mut self, mesh: ObjectId) -> Option<Arc<MeshShape>> {
        if let Some(shape) = self.karma.meshes.get(&mesh) {
            return shape.clone();
        }
        let path = self.object(mesh).path.clone();
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
        let params = self.object_of(this, Prop::KParams);
        let body = self.karma_body(this);
        if body_command(&mut self.karma.world, native, body, args) {
            return Ok(None);
        }
        match native {
            Native::KSetBlockKarma => {
                self.set(this, Prop::bBlockKarma, Value::Bool(args.first().is_some_and(crate::interp::truthy)));
            }
            Native::KEnableCollision | Native::KDisableCollision => {
                let other = match args.first() {
                    Some(&Value::Object(Some(other))) => self.karma_body(other),
                    _ => None,
                };
                if let (Some(world), Some(a), Some(other)) = (self.karma.world.as_mut(), body, other) {
                    let enabled = native == Native::KEnableCollision;
                    world.change(move |w| w.set_pair_collision(a, other, enabled));
                }
            }
            Native::KGetCOMPosition => {
                let centre = match (self.karma.world.as_mut().and_then(Background::world), body) {
                    (Some(w), Some(id)) => w.center_of_mass(id),
                    _ => None,
                };
                let centre = centre.unwrap_or_else(|| self.vector(this, Prop::Location));
                if let Some(out) = args.first_mut() {
                    *out = Value::Vector(centre);
                }
            }
            _ => return self.karma_params_native(native, params, body, args),
        }
        Ok(None)
    }

    fn karma_params_native(&mut self, native: Native, params: Option<ObjectId>, body: Option<BodyId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let f = |n: usize| args.get(n).map_or(0.0, float);
        let b = |n: usize| args.get(n).is_some_and(crate::interp::truthy);
        let param = |vm: &Self, name: Prop| params.map_or(0.0, |k| vm.float_of(k, name));
        let set_param = |vm: &mut Self, name: Prop, value: Value| {
            if let Some(k) = params {
                vm.set(k, name, value);
            }
        };
        let world = &mut self.karma.world;
        let value = match native {
            Native::KSetMass => {
                let mass = f(0).max(1e-3);
                change_body(world, body, move |w, id| w.set_mass(id, mass));
                set_param(self, Prop::KMass, Value::Float(mass));
                None
            }
            Native::KGetMass => Some(Value::Float(match (world.as_mut().and_then(Background::world), body) {
                (Some(w), Some(id)) => w.mass(id).unwrap_or(0.0),
                _ => param(self, Prop::KMass),
            })),
            Native::KSetFriction => {
                let friction = f(0);
                change_body(world, body, move |w, id| w.set_friction(id, friction));
                set_param(self, Prop::KFriction, Value::Float(friction));
                None
            }
            Native::KSetRestitution => {
                let restitution = f(0);
                change_body(world, body, move |w, id| w.set_restitution(id, restitution));
                set_param(self, Prop::KRestitution, Value::Float(restitution));
                None
            }
            Native::KSetDampingProps => {
                let (linear, angular) = (f(0), f(1));
                let mass = param(self, Prop::KMass).max(1e-3);
                change_body(&mut self.karma.world, body, move |w, id| w.set_damping(id, linear / mass, angular / mass));
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
                set_param(self, Prop::KImpactThreshold, Value::Float(f(0)));
                None
            }
            Native::KGetImpactThreshold => Some(Value::Float(param(self, Prop::KImpactThreshold))),
            Native::KSetStayUpright => {
                let (upright, allow) = (b(0), b(1));
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

fn body_command(world: &mut Option<Background>, native: Native, body: Option<BodyId>, args: &[Value]) -> bool {
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
        Native::KWake => change_body(world, body, p2_physics::World::wake),
        // `KDisableKarma` makes the body sleep (unverified).
        Native::KDisableKarma => change_body(world, body, p2_physics::World::sleep),
        _ => return false,
    }
    true
}

fn change_body(world: &mut Option<Background>, body: Option<BodyId>, change: impl FnOnce(&mut p2_physics::World, BodyId) + Send + Sync + 'static) {
    if let (Some(world), Some(id)) = (world.as_mut(), body) {
        world.change(move |w| change(w, id));
    }
}

pub(crate) fn rotator_to_quat(rotation: Rotator) -> [f32; 4] {
    matrix_to_quat(rotation.axes())
}

pub(crate) fn matrix_to_quat([x, y, z]: [Vector; 3]) -> [f32; 4] {
    let m = [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]];
    let trace = m[0][0] + m[1][1] + m[2][2];
    if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [(m[2][1] - m[1][2]) / s, (m[0][2] - m[2][0]) / s, (m[1][0] - m[0][1]) / s, s / 4.0]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [s / 4.0, (m[0][1] + m[1][0]) / s, (m[0][2] + m[2][0]) / s, (m[2][1] - m[1][2]) / s]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [(m[0][1] + m[1][0]) / s, s / 4.0, (m[1][2] + m[2][1]) / s, (m[0][2] - m[2][0]) / s]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [(m[0][2] + m[2][0]) / s, (m[1][2] + m[2][1]) / s, s / 4.0, (m[1][0] - m[0][1]) / s]
    }
}

pub(crate) fn quat_to_rotator(q: [f32; 4]) -> Rotator {
    let [x, y, z] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|v| rotate(q, v.into()));
    let units = |angle: f32| (angle * 32768.0 / PI).round() as i32;
    [units(x[2].atan2(x[0].hypot(x[1]))), units(x[1].atan2(x[0])), units((-y[2]).atan2(z[2]))].into()
}

fn rotate([qx, qy, qz, w]: [f32; 4], v: Vector) -> Vector {
    let cross = |a: Vector, b: Vector| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let u = [qx, qy, qz];
    let t = cross(u.into(), v).map(|c| c * 2.0);
    let ut = cross(u.into(), t.into());
    [0, 1, 2].map(|i| v[i] + w * t[i] + ut[i]).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotator_quaternion_round_trip() {
        for r in [[0, 0, 0], [0, 16384, 0], [4000, -9000, 2000], [-12000, 30000, -15000], [8192, 0, 32000]] {
            let q = rotator_to_quat(r.into());
            for v in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.3, -0.5, 0.8]] {
                let (a, b) = (rotate(q, v.into()), Rotator::from(r).rotate(v.into()));
                assert!((0..3).all(|i| (a[i] - b[i]).abs() < 1e-4), "{r:?} {v:?}: {a:?} vs {b:?}");
            }
            let back = quat_to_rotator(q);
            assert!((0..3).all(|i| (back[i] - r[i]).rem_euclid(65536).min((r[i] - back[i]).rem_euclid(65536)) <= 2), "{r:?} -> {back:?}");
        }
    }

}
