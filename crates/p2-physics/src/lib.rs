//! Rigid-body physics for the game's Karma actors, on Rapier. Engine-agnostic: positions are Unreal
//! units and axes (Z up), rotations quaternions (X, Y, Z, W); the VM converts rotators and owns which actor a body
//! belongs to.

use std::sync::Mutex;

use rapier3d::prelude::*;
use rustc_hash::FxHashSet;

mod background;
mod hit;
mod ragdoll;

pub use background::Background;
pub use hit::{Hit, Throw};
pub use ragdoll::{JointLimit, JointParams, PartParams, RagdollParams, shapes_inertia};

/// Unreal units per meter, for Rapier's length-dependent tolerances (Karma's scale is 50, unverified
/// as the right choice here).
const UNITS_PER_METER: f32 = 50.0;
/// Longest step Rapier is given; longer frames are split. Rapier's contacts and joints are tuned for
/// 60 Hz: at 30 Hz ragdoll parts sink into the ground by over half their radius on landing.
const MAX_STEP: f32 = 1.0 / 60.0;
const MAX_SUBSTEPS: f32 = 30.0;
/// Friction and restitution of level geometry. Pairs multiply their coefficients (like Karma's `KFriction`,
/// "multiplied pairwise"), so the level leaves a body's own values in charge (unverified for Karma's level).
const LEVEL_FRICTION: f32 = 1.0;
const LEVEL_RESTITUTION: f32 = 1.0;

fn to_rapier(v: p2_math::Vector) -> Vector {
    Vector::from_array(v.into())
}

fn from_rapier(v: Vector) -> p2_math::Vector {
    v.to_array().into()
}

fn body_pair(a: RigidBodyHandle, b: RigidBodyHandle) -> (RigidBodyHandle, RigidBodyHandle) {
    if a.into_raw_parts() <= b.into_raw_parts() { (a, b) } else { (b, a) }
}

fn with_material(builder: ColliderBuilder, friction: f32, restitution: f32) -> ColliderBuilder {
    builder
        .friction(friction)
        .restitution(restitution)
        .friction_combine_rule(CoefficientCombineRule::Multiply)
        .restitution_combine_rule(CoefficientCombineRule::Multiply)
}

fn body_collider(builder: ColliderBuilder, friction: f32, restitution: f32) -> ColliderBuilder {
    with_material(builder, friction, restitution).active_events(ActiveEvents::COLLISION_EVENTS).active_hooks(ActiveHooks::FILTER_CONTACT_PAIRS)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("static mesh: {0}")]
    Mesh(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Box(p2_math::Vector),
    Sphere(p2_math::Vector, f32),
    Capsule(p2_math::Vector, p2_math::Vector, f32),
    Hull(Vec<[f32; 3]>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct BodyParams {
    pub shapes: Vec<Shape>,
    pub location: p2_math::Vector,
    pub rotation: [f32; 4],
    /// Mass; 0 or less: from the shapes at density 1.
    pub mass: f32,
    pub linear_damping: f32,
    pub angular_damping: f32,
    pub friction: f32,
    pub restitution: f32,
    pub asleep: bool,
    pub stay_upright: bool,
    pub allow_yaw: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BodyId(RigidBodyHandle);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Impact {
    pub body: BodyId,
    pub other: Option<BodyId>,
    pub position: p2_math::Vector,
    pub velocity: p2_math::Vector,
    pub normal: p2_math::Vector,
}

struct Started {
    colliders: [ColliderHandle; 2],
    point: Vector,
    normal: Vector,
    velocities: [Vector; 2],
}

#[derive(Default)]
struct Hooks {
    started: Mutex<Vec<Started>>,
    disabled: FxHashSet<(RigidBodyHandle, RigidBodyHandle)>,
}

impl EventHandler for Hooks {
    fn handle_collision_event(&self, bodies: &RigidBodySet, colliders: &ColliderSet, event: CollisionEvent, pair: Option<&ContactPair>) {
        let (CollisionEvent::Started(..), Some(pair)) = (event, pair) else { return };
        let Some((manifold, contact)) = pair.find_deepest_contact() else { return };
        let Some(first) = colliders.get(pair.collider1) else { return };
        let point = first.position().transform_point(contact.local_p1);
        let velocity = |c: ColliderHandle| {
            let body = colliders.get(c).and_then(Collider::parent).and_then(|h| bodies.get(h));
            body.map_or(Vector::ZERO, RigidBody::linvel)
        };
        let colliders = [pair.collider1, pair.collider2];
        let velocities = colliders.map(velocity);
        if let Ok(mut started) = self.started.lock() {
            started.push(Started { colliders, point, normal: manifold.data.normal, velocities });
        }
    }

    fn handle_contact_force_event(&self, _: Real, _: &RigidBodySet, _: &ColliderSet, _: &ContactPair, _: Real) {}

    fn handle_soft_body_tear_event(&self, _: &SoftBodySet, _: &SoftBodyTearEvent) {}
}

impl PhysicsHooks for Hooks {
    fn filter_contact_pair(&self, context: &PairFilterContext) -> Option<SolverFlags> {
        if let (Some(a), Some(b)) = (context.rigid_body1, context.rigid_body2)
            && self.disabled.contains(&body_pair(a, b))
        {
            return None;
        }
        Some(SolverFlags::COMPUTE_RIGID_IMPULSES)
    }
}

pub struct World {
    gravity: Vector,
    parameters: IntegrationParameters,
    pipeline: PhysicsPipeline,
    islands: IslandManager,
    broad_phase: DefaultBroadPhase,
    narrow_phase: NarrowPhase,
    bodies: RigidBodySet,
    colliders: ColliderSet,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    soft_bodies: SoftBodySet,
    ccd: CCDSolver,
    hooks: Hooks,
    /// Dormant bodies (Karma's disabled bodies: added asleep or put to sleep) are fixed until woken: by a call, or by
    /// an awake body touching them. Rapier would wake a sleeping body as soon as its first contacts appear (a prop
    /// resting on the floor).
    dormant: FxHashSet<RigidBodyHandle>,
    forced: Vec<RigidBodyHandle>,
    level: Vec<ColliderHandle>,
    guards: Vec<ragdoll::Guard>,
}

impl World {
    pub fn new(gravity: p2_math::Vector) -> Self {
        let parameters = IntegrationParameters { length_unit: UNITS_PER_METER, ..IntegrationParameters::default() };
        let mut pipeline = PhysicsPipeline::new();
        let threads = std::thread::available_parallelism().map_or(2, |cores| cores.get() / 4).clamp(2, 4);
        pipeline.configure_thread_pool(threads).ok();
        Self {
            gravity: to_rapier(gravity),
            parameters,
            pipeline,
            islands: IslandManager::new(),
            broad_phase: DefaultBroadPhase::new(),
            narrow_phase: NarrowPhase::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            soft_bodies: SoftBodySet::new(),
            ccd: CCDSolver::new(),
            hooks: Hooks::default(),
            dormant: FxHashSet::default(),
            forced: Vec::new(),
            level: Vec::new(),
            guards: Vec::new(),
        }
    }

    /// Fixed level geometry (BSP, terrain, static meshes) as a triangle mesh. `one_sided`: a triangle only pushes
    /// bodies out of its front (the side its corners run counter-clockwise from, about the right-handed normal), and
    /// contacts on edges between triangles take the faces' normals, like Karma's level triangles:
    /// a body part that got behind the surface is not held there. Otherwise both sides
    /// push.
    pub fn add_static_mesh(&mut self, vertices: &[[f32; 3]], triangles: Vec<[u32; 3]>, one_sided: bool) -> Result<(), Error> {
        let vertices = vertices.iter().copied().map(Vector::from_array).collect();
        let flags = if one_sided { TriMeshFlags::FIX_INTERNAL_EDGES | TriMeshFlags::DELETE_DEGENERATE_TRIANGLES } else { TriMeshFlags::empty() };
        let collider = ColliderBuilder::trimesh_with_flags(vertices, triangles, flags).map_err(|e| Error::Mesh(e.to_string()))?;
        self.level.push(self.colliders.insert(with_material(collider, LEVEL_FRICTION, LEVEL_RESTITUTION)));
        Ok(())
    }

    pub fn add_static_hull(&mut self, points: &[[f32; 3]]) -> Result<(), Error> {
        let points: Vec<Vector> = points.iter().copied().map(Vector::from_array).collect();
        let collider = ColliderBuilder::convex_hull(&points).ok_or_else(|| Error::Mesh(format!("no convex hull of {} points", points.len())))?;
        self.level.push(self.colliders.insert(with_material(collider, LEVEL_FRICTION, LEVEL_RESTITUTION)));
        Ok(())
    }

    pub fn add_body(&mut self, params: &BodyParams) -> BodyId {
        let pose = Pose::from_parts(to_rapier(params.location), Rotation::from_array(params.rotation).normalize());
        let mut body = RigidBodyBuilder::dynamic()
            .pose(pose)
            .linear_damping(params.linear_damping)
            .angular_damping(params.angular_damping)
            .ccd_enabled(true)
            .build();
        if params.stay_upright {
            body.set_enabled_rotations(false, false, params.allow_yaw, false);
        }
        if params.asleep {
            body.set_body_type(RigidBodyType::Fixed, false);
        }
        let handle = self.bodies.insert(body);
        if params.asleep {
            self.dormant.insert(handle);
        }
        let mut builders: Vec<ColliderBuilder> = params.shapes.iter().filter_map(collider).collect();
        if builders.is_empty() {
            builders.push(ColliderBuilder::ball(1.0));
        }
        let colliders: Vec<Collider> = builders.into_iter().map(|builder| body_collider(builder, params.friction, params.restitution).build()).collect();
        let volume: f32 = colliders.iter().map(Collider::volume).sum();
        for mut collider in colliders {
            if params.mass > 0.0 && volume > 0.0 {
                collider.set_mass(params.mass * collider.volume() / volume);
            }
            self.colliders.insert_with_parent(collider, handle, &mut self.bodies);
        }
        // Rapier applies the mass at the next step; until then an impulse would move nothing.
        if let Some(body) = self.bodies.get_mut(handle) {
            body.recompute_mass_properties_from_colliders(&self.colliders);
        }
        BodyId(handle)
    }

    pub fn remove_body(&mut self, id: BodyId) {
        self.hooks.disabled.retain(|&(a, b)| a != id.0 && b != id.0);
        self.take_out_body(id);
    }

    fn take_out_body(&mut self, id: BodyId) {
        self.dormant.remove(&id.0);
        self.bodies.remove(id.0, &mut self.islands, &mut self.colliders, &mut self.impulse_joints, &mut self.multibody_joints, &mut self.soft_bodies, true);
    }

    pub fn apply_impulse(&mut self, id: BodyId, impulse: p2_math::Vector, point: Option<p2_math::Vector>) {
        self.push(id, to_rapier(impulse), point.map(to_rapier));
    }

    pub fn add_force(&mut self, id: BodyId, force: p2_math::Vector, point: Option<p2_math::Vector>) {
        self.wake(id);
        let Some(body) = self.bodies.get_mut(id.0) else { return };
        self.forced.push(id.0);
        match point {
            Some(point) => body.add_force_at_point(to_rapier(force), to_rapier(point), true),
            None => body.add_force(to_rapier(force), true),
        }
    }

    pub fn add_torque(&mut self, id: BodyId, torque: p2_math::Vector) {
        self.wake(id);
        if let Some(body) = self.bodies.get_mut(id.0) {
            self.forced.push(id.0);
            body.add_torque(to_rapier(torque), true);
        }
    }

    pub fn wake(&mut self, id: BodyId) {
        let dormant = self.dormant.remove(&id.0);
        if let Some(body) = self.bodies.get_mut(id.0) {
            if dormant {
                body.set_body_type(RigidBodyType::Dynamic, false);
                body.recompute_mass_properties_from_colliders(&self.colliders);
            }
            body.wake_up(true);
        }
    }

    pub fn sleep(&mut self, id: BodyId) {
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.set_linvel(Vector::ZERO, false);
            body.set_angvel(Vector::ZERO, false);
            body.set_body_type(RigidBodyType::Fixed, false);
            self.dormant.insert(id.0);
        }
    }

    pub fn mass(&self, id: BodyId) -> Option<f32> {
        Some(self.bodies.get(id.0)?.mass())
    }

    pub fn set_mass(&mut self, id: BodyId, mass: f32) {
        let Some(body) = self.bodies.get(id.0) else { return };
        let old = body.mass();
        if old <= 0.0 || mass <= 0.0 {
            return;
        }
        for &handle in body.colliders() {
            if let Some(collider) = self.colliders.get_mut(handle) {
                collider.set_mass(collider.mass() * mass / old);
            }
        }
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.recompute_mass_properties_from_colliders(&self.colliders);
        }
    }

    pub fn set_friction(&mut self, id: BodyId, friction: f32) {
        self.each_collider(id, |c| c.set_friction(friction));
    }

    pub fn set_restitution(&mut self, id: BodyId, restitution: f32) {
        self.each_collider(id, |c| c.set_restitution(restitution));
    }

    pub fn set_damping(&mut self, id: BodyId, linear: f32, angular: f32) {
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.set_linear_damping(linear);
            body.set_angular_damping(angular);
        }
    }

    pub fn set_stay_upright(&mut self, id: BodyId, upright: bool, allow_yaw: bool) {
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.set_enabled_rotations(!upright, !upright, !upright || allow_yaw, true);
        }
    }

    pub fn set_pair_collision(&mut self, a: BodyId, b: BodyId, enabled: bool) {
        let pair = body_pair(a.0, b.0);
        if enabled {
            self.hooks.disabled.remove(&pair);
        } else {
            self.hooks.disabled.insert(pair);
        }
    }

    pub fn set_pose(&mut self, id: BodyId, location: p2_math::Vector, rotation: [f32; 4]) {
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.set_translation(to_rapier(location), true);
            body.set_rotation(Rotation::from_array(rotation).normalize(), true);
        }
    }

    pub fn set_velocity(&mut self, id: BodyId, velocity: p2_math::Vector) {
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.set_linvel(to_rapier(velocity), true);
        }
    }

    pub fn set_spin(&mut self, id: BodyId, spin: p2_math::Vector) {
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.set_angvel(to_rapier(spin), true);
        }
    }

    pub fn gravity(&self) -> p2_math::Vector {
        from_rapier(self.gravity)
    }

    pub fn limit_speed(&mut self, id: BodyId, max: f32) {
        let Some(body) = self.bodies.get_mut(id.0) else { return };
        let velocity = body.linvel();
        let squared = velocity.length_squared();
        if squared > max * max {
            body.set_linvel(velocity * (max / squared.sqrt()), false);
        }
    }

    pub fn step(&mut self, dt: f32) -> Vec<Impact> {
        if dt <= 0.0 {
            return Vec::new();
        }
        let steps = (dt / MAX_STEP).ceil().clamp(1.0, MAX_SUBSTEPS);
        self.parameters.dt = (dt / steps).min(MAX_STEP);
        let mut impacts = Vec::new();
        for _ in 0..steps as u32 {
            self.pipeline.step(
                self.gravity,
                &self.parameters,
                &mut self.islands,
                &mut self.broad_phase,
                &mut self.narrow_phase,
                &mut self.bodies,
                &mut self.colliders,
                &mut self.impulse_joints,
                &mut self.multibody_joints,
                &mut self.soft_bodies,
                &mut self.ccd,
                &self.hooks,
                &self.hooks,
            );
            self.guard_ragdolls();
            let started = self.hooks.started.lock().map(|mut s| std::mem::take(&mut *s)).unwrap_or_default();
            for s in started {
                let bodies = s.colliders.map(|c| self.colliders.get(c).and_then(Collider::parent));
                for (this, other) in [(0, 1), (1, 0)] {
                    let awake = bodies[other].and_then(|h| self.bodies.get(h)).is_some_and(|b| b.is_dynamic() && !b.is_sleeping());
                    if let Some(this) = bodies[this].filter(|h| awake && self.dormant.contains(h)) {
                        self.wake(BodyId(this));
                    }
                }
                for (this, other, normal) in [(0, 1, -s.normal), (1, 0, s.normal)] {
                    let Some(body) = bodies[this].filter(|&h| self.bodies.get(h).is_some_and(RigidBody::is_dynamic)) else { continue };
                    impacts.push(Impact {
                        body: BodyId(body),
                        other: bodies[other].map(BodyId),
                        position: from_rapier(s.point),
                        velocity: from_rapier(s.velocities[this] - s.velocities[other]),
                        normal: from_rapier(normal),
                    });
                }
            }
        }
        for handle in self.forced.drain(..) {
            if let Some(body) = self.bodies.get_mut(handle) {
                body.reset_forces(false);
                body.reset_torques(false);
            }
        }
        impacts
    }

    pub fn pose(&self, id: BodyId) -> Option<(p2_math::Vector, [f32; 4])> {
        let body = self.bodies.get(id.0)?;
        Some((from_rapier(body.translation()), body.rotation().to_array()))
    }

    pub fn velocity(&self, id: BodyId) -> Option<p2_math::Vector> {
        Some(from_rapier(self.bodies.get(id.0)?.linvel()))
    }

    pub fn center_of_mass(&self, id: BodyId) -> Option<p2_math::Vector> {
        Some(from_rapier(self.bodies.get(id.0)?.center_of_mass()))
    }

    pub fn is_sleeping(&self, id: BodyId) -> bool {
        self.dormant.contains(&id.0) || self.bodies.get(id.0).is_some_and(RigidBody::is_sleeping)
    }

    pub fn parts(&self, bodies: &[BodyId]) -> Parts {
        let mut parts = Parts::default();
        self.parts_into(bodies, &mut parts);
        parts
    }

    pub fn parts_into(&self, bodies: &[BodyId], parts: &mut Parts) {
        parts.colliders.clear();
        parts.centres.clear();
        parts.bounds = None;
        for &id in bodies {
            let Some(body) = self.bodies.get(id.0) else { continue };
            parts.centres.push((id, from_rapier(body.center_of_mass())));
            for part in body.colliders().iter().filter_map(|&c| self.colliders.get(c)) {
                let aabb = part.compute_aabb();
                parts.bounds = Some(parts.bounds.map_or(aabb, |bounds| bounds.merged(&aabb)));
                parts.colliders.push(Part { body: id, shape: part.shared_shape().clone(), pose: *part.position(), aabb });
            }
        }
    }

    fn each_collider(&mut self, id: BodyId, mut change: impl FnMut(&mut Collider)) {
        let Some(body) = self.bodies.get(id.0) else { return };
        for &handle in body.colliders() {
            if let Some(collider) = self.colliders.get_mut(handle) {
                change(collider);
            }
        }
    }
}

#[derive(Clone)]
struct Part {
    body: BodyId,
    shape: SharedShape,
    pose: Pose,
    aabb: Aabb,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentHit {
    pub body: BodyId,
    pub time: f32,
    pub normal: p2_math::Vector,
}

#[derive(Clone, Default)]
pub struct Parts {
    colliders: Vec<Part>,
    bounds: Option<Aabb>,
    centres: Vec<(BodyId, p2_math::Vector)>,
}

impl Parts {
    pub fn bounds(&self) -> Option<(p2_math::Vector, p2_math::Vector)> {
        self.bounds.map(|b| (b.mins.to_array().into(), b.maxs.to_array().into()))
    }

    pub fn segment_hit(&self, start: p2_math::Vector, end: p2_math::Vector) -> Option<SegmentHit> {
        use rapier3d::parry::query::{Ray, RayCast};
        let origin = to_rapier(start);
        let ray = Ray::new(origin, to_rapier(end) - origin);
        if !self.bounds.is_some_and(|b| b.intersects_local_ray(&ray, 1.0)) {
            return None;
        }
        self.colliders
            .iter()
            .filter(|part| part.aabb.intersects_local_ray(&ray, 1.0))
            .filter_map(|part| {
                let hit = part.shape.cast_ray_and_get_normal(&part.pose, &ray, 1.0, true)?;
                Some(SegmentHit { body: part.body, time: hit.time_of_impact, normal: hit.normal.to_array().into() })
            })
            .min_by(|a, b| a.time.total_cmp(&b.time))
    }

    pub fn nearest_body(&self, point: p2_math::Vector) -> Option<BodyId> {
        let distance = |c: &p2_math::Vector| (*c - point).size_squared();
        self.centres.iter().min_by(|a, b| distance(&a.1).total_cmp(&distance(&b.1))).map(|&(id, _)| id)
    }
}

fn collider(shape: &Shape) -> Option<ColliderBuilder> {
    match shape {
        Shape::Box(p2_math::Vector { x, y, z }) => Some(ColliderBuilder::cuboid(*x, *y, *z)),
        Shape::Sphere(center, radius) => Some(ColliderBuilder::ball(*radius).translation(to_rapier(*center))),
        Shape::Capsule(a, b, radius) => Some(ColliderBuilder::capsule_from_endpoints(to_rapier(*a), to_rapier(*b), *radius)),
        Shape::Hull(points) => {
            let points: Vec<Vector> = points.iter().copied().map(Vector::from_array).collect();
            ColliderBuilder::convex_hull(&points)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crate_box(location: p2_math::Vector) -> BodyParams {
        BodyParams {
            shapes: vec![Shape::Box([16.0; 3].into())],
            location,
            rotation: [0.0, 0.0, 0.0, 1.0],
            mass: 10.0,
            linear_damping: 0.0,
            angular_damping: 0.0,
            friction: 0.8,
            restitution: 0.0,
            asleep: false,
            stay_upright: false,
            allow_yaw: false,
        }
    }

    fn floor(world: &mut World) {
        let vertices = [[-1000.0, -1000.0, 0.0], [1000.0, -1000.0, 0.0], [1000.0, 1000.0, 0.0], [-1000.0, 1000.0, 0.0]];
        world.add_static_mesh(&vertices, vec![[0, 1, 2], [0, 2, 3]], false).unwrap();
    }

    #[test]
    fn box_falls_onto_the_floor_and_rests() {
        let mut world = World::new([0.0, 0.0, -950.0].into());
        floor(&mut world);
        let body = world.add_body(&crate_box([0.0, 0.0, 200.0].into()));
        let mut impacts = Vec::new();
        for _ in 0..180 {
            impacts.extend(world.step(1.0 / 60.0));
        }
        let (p2_math::Vector { x, y, z }, _) = world.pose(body).unwrap();
        assert!((z - 16.0).abs() < 2.0 && x.abs() < 1.0 && y.abs() < 1.0, "rests at {x} {y} {z}");
        let hit = impacts.first().expect("impact");
        assert!(hit.body == body && hit.other.is_none() && hit.velocity[2] < -300.0 && hit.normal[2] > 0.9, "{hit:?}");
    }

    #[test]
    fn a_spun_hull_turns_about_its_axis() {
        let mut world = World::new([0.0; 3].into());
        let corners = vec![[-8.0, -8.0, -8.0], [8.0, -8.0, -8.0], [8.0, 8.0, -8.0], [-8.0, 8.0, -8.0], [-8.0, -8.0, 8.0], [8.0, -8.0, 8.0], [8.0, 8.0, 8.0], [-8.0, 8.0, 8.0]];
        let body = world.add_body(&BodyParams { shapes: vec![Shape::Hull(corners)], ..crate_box([0.0; 3].into()) });
        world.set_spin(body, [0.0, 0.0, std::f32::consts::PI].into());
        world.step(0.5);
        let (_, [x, y, z, w]) = world.pose(body).unwrap();
        assert!(x.abs() < 0.01 && y.abs() < 0.01 && (z - w).abs() < 0.02 && z > 0.6, "{x} {y} {z} {w}");
        assert_eq!(world.gravity(), [0.0; 3].into());
    }

    #[test]
    fn asleep_body_waits_until_woken() {
        let mut world = World::new([0.0, 0.0, -950.0].into());
        floor(&mut world);
        let body = world.add_body(&BodyParams { asleep: true, ..crate_box([0.0, 0.0, 200.0].into()) });
        let resting = world.add_body(&BodyParams { asleep: true, ..crate_box([100.0, 0.0, 15.5].into()) });
        for _ in 0..30 {
            world.step(1.0 / 30.0);
        }
        assert_eq!(world.pose(body).unwrap().0, [0.0, 0.0, 200.0].into());
        assert_eq!(world.pose(resting).unwrap().0, [100.0, 0.0, 15.5].into());
        world.wake(body);
        world.step(0.1);
        assert!(world.pose(body).unwrap().0[2] < 200.0);
        let dropped = world.add_body(&crate_box([100.0, 0.0, 120.0].into()));
        for _ in 0..30 {
            world.step(1.0 / 30.0);
        }
        assert!(world.pose(dropped).unwrap().0[2] < 100.0);
        assert!(!world.dormant.contains(&resting.0));
    }

    #[test]
    fn segment_hits_the_nearest_part() {
        let mut world = World::new([0.0; 3].into());
        let near = world.add_body(&BodyParams { asleep: true, ..crate_box([100.0, 0.0, 0.0].into()) });
        let far = world.add_body(&crate_box([200.0, 0.0, 0.0].into()));
        let parts = world.parts(&[far, near]);
        let SegmentHit { body, time, normal } = parts.segment_hit([0.0; 3].into(), [400.0, 0.0, 0.0].into()).unwrap();
        assert!(body == near && (time - 84.0 / 400.0).abs() < 1e-4 && normal[0] < -0.99, "{time} {normal:?}");
        assert!(parts.segment_hit([0.0, 50.0, 0.0].into(), [400.0, 50.0, 0.0].into()).is_none());
        assert!(world.parts(&[far]).segment_hit([0.0; 3].into(), [150.0, 0.0, 0.0].into()).is_none());
        assert_eq!(parts.nearest_body([190.0, 0.0, 0.0].into()), Some(far));
        world.set_pose(near, [100.0, 300.0, 0.0].into(), [0.0, 0.0, 0.0, 1.0]);
        assert!(parts.segment_hit([0.0; 3].into(), [150.0, 0.0, 0.0].into()).is_some());
    }

    #[test]
    fn impulse_pushes_the_box() {
        let mut world = World::new([0.0; 3].into());
        let body = world.add_body(&crate_box([0.0; 3].into()));
        world.apply_impulse(body, [1000.0, 0.0, 0.0].into(), None);
        world.step(0.1);
        let velocity = world.velocity(body).unwrap();
        assert!((velocity[0] - 100.0).abs() < 1.0, "velocity {velocity:?}");
    }

    #[test]
    fn parts_share_the_mass_and_forces_last_one_step() {
        let mut world = World::new([0.0; 3].into());
        let mut params = crate_box([0.0; 3].into());
        params.shapes.push(Shape::Sphere([0.0, 0.0, 40.0].into(), 8.0));
        let body = world.add_body(&params);
        assert!((world.mass(body).unwrap() - 10.0).abs() < 1e-3);
        world.set_mass(body, 20.0);
        assert!((world.mass(body).unwrap() - 20.0).abs() < 1e-3);
        world.add_force(body, [200.0, 0.0, 0.0].into(), None);
        world.step(0.1);
        world.step(0.1);
        let velocity = world.velocity(body).unwrap();
        assert!((velocity[0] - 1.0).abs() < 0.05, "velocity {velocity:?}");
    }

    #[test]
    fn upright_body_does_not_tip() {
        let mut world = World::new([0.0; 3].into());
        let mut params = crate_box([0.0; 3].into());
        params.stay_upright = true;
        let body = world.add_body(&params);
        world.apply_impulse(body, [0.0, 0.0, 100.0].into(), Some([16.0, 0.0, 0.0].into()));
        world.step(0.2);
        let (_, [x, y, z, w]) = world.pose(body).unwrap();
        assert!(x.abs() < 1e-4 && y.abs() < 1e-4 && z.abs() < 1e-4 && (w.abs() - 1.0).abs() < 1e-4);
    }
}
