//! Rigid-body physics for the game's Karma actors, on Rapier. Engine-agnostic: positions are Unreal
//! units and axes (Z up), rotations quaternions (X, Y, Z, W); the VM converts rotators and owns which actor a body
//! belongs to.

use std::sync::Mutex;

use rapier3d::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

mod bodies;
mod hit;
mod level_contact;
mod parts;
mod ragdoll;
mod stepping;

pub use hit::{Hit, Throw};
pub use parts::{Parts, SegmentHit};
pub use ragdoll::{JointLimit, JointParams, PartParams, RagdollParams, shapes_inertia};

pub const UNITS_PER_METER: f32 = 50.0;
/// At 30 Hz ragdoll parts sink into the ground by over half their radius on landing.
/// Every step has this length: a step whose length changes from one to the next leaves resting bodies trembling, so a
/// pile of corpses never falls asleep. Time not stepped yet is owed to the next call.
const STEP: f32 = 1.0 / 60.0;
const MAX_STEPS: u32 = 30;

/// The whole steps `owed` holds, and what stays owed. A call past the limit loses time.
fn steps_owed(owed: f32) -> (u32, f32) {
    let steps = (owed / STEP + 1e-3) as u32;
    if steps > MAX_STEPS { (MAX_STEPS, 0.0) } else { (steps, (owed - steps as f32 * STEP).max(0.0)) }
}
/// Pairs multiply their coefficients, so the level leaves a body's own values in charge.
const LEVEL_FRICTION: f32 = 1.0;
const LEVEL_RESTITUTION: f32 = 1.0;
/// Unreal units past a mover's swept bounds within which its move wakes resting bodies.
const MOVER_REACH: f32 = 16.0;

fn to_rapier(v: p2_math::Vector) -> Vector {
    Vector::from_array(v.into())
}

fn from_rapier(v: Vector) -> p2_math::Vector {
    v.to_array().into()
}

fn pose(location: p2_math::Vector, rotation: [f32; 4]) -> Pose {
    Pose::from_parts(to_rapier(location), Rotation::from_array(rotation).normalize())
}

fn vectors(points: &[[f32; 3]]) -> Vec<Vector> {
    points.iter().copied().map(Vector::from_array).collect()
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
    /// Kinematic movers only: a triangle mesh has no mass.
    Mesh(Vec<[[f32; 3]; 3]>),
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
    level_only: FxHashSet<RigidBodyHandle>,
    movers: FxHashSet<RigidBodyHandle>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contacts {
    All,
    LevelOnly,
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
            && (self.disabled.contains(&body_pair(a, b))
                || (!self.movers.contains(&a) && !self.movers.contains(&b) && (self.level_only.contains(&a) || self.level_only.contains(&b))))
        {
            return None;
        }
        Some(SolverFlags::COMPUTE_RIGID_IMPULSES)
    }

    fn modify_solver_contacts(&self, context: &mut ContactModificationContext) {
        level_contact::push_to_front(context);
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
    started: Vec<Started>,
    moving: FxHashMap<RigidBodyHandle, Pose>,
    cones: Vec<ragdoll::Cone>,
    /// Each ragdoll's parts. A ragdoll at rest goes dormant on its own: Rapier sleeps touching bodies only all at
    /// once, so one moving corpse kept every corpse of its pile awake.
    ragdolls: Vec<ragdoll::Resting>,
    /// Rest marks moved since the profile log last asked: by a part's place, by its turn.
    pub rest_broken: [u32; 2],
    pub wakes: u32,
    owed: f32,
    /// Where each moving body was before the last step: its pose is given between that and the step's, by the time
    /// owed, so bodies move evenly however the calls fall on the steps.
    before: FxHashMap<RigidBodyHandle, Pose>,
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
            started: Vec::new(),
            moving: FxHashMap::default(),
            cones: Vec::new(),
            ragdolls: Vec::new(),
            rest_broken: [0; 2],
            wakes: 0,
            owed: 0.0,
            before: FxHashMap::default(),
        }
    }

    /// Up to one step late: between the last two steps' poses, by the time owed.
    pub fn pose(&self, id: BodyId) -> Option<(p2_math::Vector, [f32; 4])> {
        let body = self.bodies.get(id.0)?;
        let now = body.position();
        let shown = self.before.get(&id.0).map_or(*now, |before| before.lerp(now, self.owed / STEP));
        Some((from_rapier(shown.translation), shown.rotation.to_array()))
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
}

fn collider(shape: &Shape) -> Option<ColliderBuilder> {
    match shape {
        Shape::Box(p2_math::Vector { x, y, z }) => Some(ColliderBuilder::cuboid(*x, *y, *z)),
        Shape::Sphere(center, radius) => Some(ColliderBuilder::ball(*radius).translation(to_rapier(*center))),
        Shape::Capsule(a, b, radius) => Some(ColliderBuilder::capsule_from_endpoints(to_rapier(*a), to_rapier(*b), *radius)),
        Shape::Hull(points) => ColliderBuilder::convex_hull(&vectors(points)),
        Shape::Mesh(triangles) => {
            let indices = (0..triangles.len() as u32).map(|i| [3 * i, 3 * i + 1, 3 * i + 2]).collect();
            let flags = TriMeshFlags::MERGE_DUPLICATE_VERTICES | TriMeshFlags::FIX_INTERNAL_EDGES_TWO_SIDED | TriMeshFlags::DELETE_DEGENERATE_TRIANGLES;
            ColliderBuilder::trimesh_with_flags(vectors(triangles.as_flattened()), indices, flags).ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_whole_and_the_rest_stays_owed() {
        assert_eq!(steps_owed(STEP), (1, 0.0));
        assert_eq!(steps_owed(STEP * 0.6), (0, STEP * 0.6));
        let (steps, owed) = steps_owed(STEP * 2.6);
        assert!(steps == 2 && (owed - STEP * 0.6).abs() < 1e-6, "{steps} {owed}");
        assert_eq!(steps_owed(2.0), (MAX_STEPS, 0.0));
    }

    #[test]
    fn a_falling_body_moves_between_steps() {
        let mut world = World::new([0.0, 0.0, -950.0].into());
        let body = world.add_body(&crate_box([0.0, 0.0, 500.0].into()));
        for _ in 0..6 {
            world.step(STEP);
        }
        let mut last = world.pose(body).unwrap().0.z;
        for _ in 0..12 {
            world.step(STEP / 4.0);
            let now = world.pose(body).unwrap().0.z;
            assert!(now < last - 0.1, "{now} after {last}");
            last = now;
        }
    }

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
        world.add_static_mesh(&vertices, vec![[0, 1, 2], [0, 2, 3]]).unwrap();
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
    fn a_box_on_a_rising_mover_rises_with_it() {
        let mut world = World::new([0.0, 0.0, -950.0].into());
        let lift = world.add_mover(&[Shape::Box([64.0, 64.0, 8.0].into())], [0.0; 3].into(), [0.0, 0.0, 0.0, 1.0]).unwrap();
        let body = world.add_body(&crate_box([0.0, 0.0, 24.0].into()));
        for _ in 0..60 {
            world.step(1.0 / 60.0);
        }
        for frame in 1..=60 {
            world.move_mover(lift, [0.0, 0.0, frame as f32 * 2.0].into(), [0.0, 0.0, 0.0, 1.0]);
            world.step(1.0 / 60.0);
        }
        let (location, _) = world.pose(body).unwrap();
        assert!((location[2] - 144.0).abs() < 4.0 && location[0].abs() < 1.0, "{location:?}");
        assert!((world.pose(lift).unwrap().0[2] - 120.0).abs() < 0.01);
    }

    #[test]
    fn a_spun_hull_turns_about_its_axis() {
        let mut world = World::new([0.0; 3].into());
        let corners = vec![[-8.0, -8.0, -8.0], [8.0, -8.0, -8.0], [8.0, 8.0, -8.0], [-8.0, 8.0, -8.0], [-8.0, -8.0, 8.0], [8.0, -8.0, 8.0], [8.0, 8.0, 8.0], [-8.0, 8.0, 8.0]];
        let body = world.add_body(&BodyParams { shapes: vec![Shape::Hull(corners)], ..crate_box([0.0; 3].into()) });
        world.set_spin(body, [0.0, 0.0, std::f32::consts::PI].into());
        world.step(0.5);
        world.step(STEP);
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
