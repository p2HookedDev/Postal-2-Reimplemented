//! Ragdolls: a body per part, joints between parts. A ragdoll's parts collide
//! with everything except pairs told not to and jointed pairs. The engine's contact callback drops ragdoll–Karma-actor
//! and ragdoll–ragdoll contacts; ours keeps them so corpses stack and push props (our choice).

use rapier3d::prelude::*;

use crate::{BodyId, Shape, World, body_collider, body_pair, collider, to_rapier};

#[derive(Debug, Clone, PartialEq)]
pub struct PartParams {
    pub shapes: Vec<Shape>,
    pub location: p2_math::Vector,
    pub rotation: [f32; 4],
    pub mass: f32,
    pub center_of_mass: p2_math::Vector,
    pub inertia: p2_math::Vector,
    pub velocity: p2_math::Vector,
}

/// How a joint limits the child's rotation relative to the parent, about the joint frame's axes: X the primary axis,
/// Y the orthogonal one, Z their cross product.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JointLimit {
    Hinge(Option<[f32; 2]>),
    Cone { swing: Option<[f32; 2]>, twist: Option<f32> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct JointParams {
    pub child: usize,
    pub parent: usize,
    pub child_anchor: p2_math::Vector,
    pub child_axes: [p2_math::Vector; 2],
    pub parent_anchor: p2_math::Vector,
    pub parent_axes: [p2_math::Vector; 2],
    pub limit: JointLimit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RagdollParams {
    pub parts: Vec<PartParams>,
    pub joints: Vec<JointParams>,
    pub linear_damping: f32,
    pub angular_damping: f32,
    pub friction: f32,
    pub restitution: f32,
    pub asleep: bool,
}

const SLEEP_SPEED: f32 = 1.5;
const SLEEP_ANGULAR_SPEED: f32 = 1.0;
const SLEEP_TIME: f32 = 0.5;
/// The child / parent mass ratio a joint may keep, and the most a parent's inertia is raised toward the sum of its
/// children's (Jolt's `Ragdoll::Stabilize`, after Strunk's "Stop my Constraints from Blowing Up!"; our own code).
/// Karma solves a step's joints exactly (one LCP per partition); Rapier's iterative solver lets parts of very different
/// mass drift apart (the shipped parts range from 0.0015 to 0.43). Even masses hold the joints with no extra iterations.
const MASS_RATIO: [f32; 2] = [0.8, 1.2];
const MAX_INERTIA_INCREASE: f32 = 2.0;

/// Each part's mass and principal inertia evened out so an iterative solver holds the joints: a child weighs
/// 0.8-1.2 times its parent (each jointed tree keeps its total mass, inertia scales with mass), then a parent's
/// inertia is raised toward its children's sum, at most doubled. Script impulses act on these masses unscaled:
/// the body's momentum, so its overall flight, stays retail's.
fn stabilize(parts: &[PartParams], joints: &[JointParams]) -> Vec<(f32, p2_math::Vector)> {
    let n = parts.len();
    let mut parent = vec![None; n];
    for joint in joints.iter().filter(|j| j.child < n && j.parent < n) {
        parent[joint.child].get_or_insert(joint.parent);
    }
    let walks: Vec<(usize, usize, f32)> = (0..n)
        .map(|mut i| {
            let (mut depth, mut ratio) = (0, 1.0);
            while let Some(p) = parent[i].filter(|_| depth < n) {
                let own = parts[i].mass / parts[p].mass;
                ratio *= if own.is_nan() { 1.0 } else { own.clamp(MASS_RATIO[0], MASS_RATIO[1]) };
                (i, depth) = (p, depth + 1);
            }
            (i, depth, ratio)
        })
        .collect();
    let (mut total, mut ratios) = (vec![0.0; n], vec![0.0; n]);
    for (part, &(root, _, ratio)) in parts.iter().zip(&walks) {
        total[root] += part.mass;
        ratios[root] += ratio;
    }
    let mut out: Vec<(f32, p2_math::Vector)> = parts
        .iter()
        .zip(&walks)
        .map(|(part, &(root, _, ratio))| {
            let mass = ratio * total[root] / ratios[root];
            let scale = mass / part.mass;
            let scale = if scale.is_finite() { scale } else { 1.0 };
            (mass, part.inertia * scale)
        })
        .collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(walks[i].1));
    let largest = |inertia: p2_math::Vector| <[f32; 3]>::from(inertia).into_iter().fold(0.0, f32::max);
    for &p in &order {
        let children: f32 = (0..n).filter(|&c| parent[c] == Some(p)).map(|c| largest(out[c].1)).sum();
        let own = largest(out[p].1);
        if own > 0.0 && children > own {
            let raise = (children / own).min(MAX_INERTIA_INCREASE);
            out[p].1 *= raise;
        }
    }
    out
}

pub fn shapes_inertia(shapes: &[Shape], mass: f32) -> Option<p2_math::Vector> {
    let props = shapes.iter().filter_map(collider).map(|builder| builder.build().mass_properties()).reduce(|a, b| a + b)?;
    let scale = mass / props.mass();
    scale.is_finite().then(|| p2_math::Vector::from((props.principal_inertia() * scale).to_array()))
}

impl World {
    pub fn add_ragdoll(&mut self, params: &RagdollParams) -> Vec<BodyId> {
        let masses = stabilize(&params.parts, &params.joints);
        let mut activation = RigidBodyActivation::active();
        activation.normalized_linear_threshold = SLEEP_SPEED / self.parameters.length_unit;
        activation.angular_threshold = SLEEP_ANGULAR_SPEED;
        activation.time_until_sleep = SLEEP_TIME;
        let bodies: Vec<BodyId> = params.parts.iter().zip(masses).map(|(part, (mass, inertia))| self.ragdoll_body(part, mass, inertia, params, activation)).collect();
        for joint in &params.joints {
            self.ragdoll_joint(joint, &bodies);
        }
        self.filter_self_collision(&params.joints, &bodies);
        if params.asleep {
            for id in &bodies {
                if let Some(body) = self.bodies.get_mut(id.0) {
                    body.sleep();
                }
            }
        }
        bodies
    }

    fn ragdoll_body(&mut self, part: &PartParams, mass: f32, inertia: p2_math::Vector, params: &RagdollParams, activation: RigidBodyActivation) -> BodyId {
        let pose = Pose::from_parts(to_rapier(part.location), Rotation::from_array(part.rotation).normalize());
        let mass = MassProperties::new(to_rapier(part.center_of_mass), mass, to_rapier(inertia));
        let body = RigidBodyBuilder::dynamic()
            .pose(pose)
            .linvel(to_rapier(part.velocity))
            .additional_mass_properties(mass)
            .linear_damping(params.linear_damping)
            .angular_damping(params.angular_damping)
            .ccd_enabled(true)
            .build();
        let handle = self.bodies.insert(body);
        if let Some(body) = self.bodies.get_mut(handle) {
            *body.activation_mut() = activation;
        }
        for shape in &part.shapes {
            let Some(builder) = collider(shape) else { continue };
            let collider = body_collider(builder.density(0.0), params.friction, params.restitution).build();
            self.colliders.insert_with_parent(collider, handle, &mut self.bodies);
        }
        if let Some(body) = self.bodies.get_mut(handle) {
            body.recompute_mass_properties_from_colliders(&self.colliders);
        }
        BodyId(handle)
    }

    fn ragdoll_joint(&mut self, joint: &JointParams, bodies: &[BodyId]) {
        let (Some(child), Some(parent)) = (bodies.get(joint.child), bodies.get(joint.parent)) else { return };
        let frame = |anchor: p2_math::Vector, [primary, orthogonal]: [p2_math::Vector; 2]| {
            Pose::from_parts(to_rapier(anchor), axes_rotation(to_rapier(primary), to_rapier(orthogonal)))
        };
        let mut builder = GenericJointBuilder::new(JointAxesMask::LIN_AXES)
            .local_frame1(frame(joint.parent_anchor, joint.parent_axes))
            .local_frame2(frame(joint.child_anchor, joint.child_axes))
            .contacts_enabled(false);
        match joint.limit {
            JointLimit::Hinge(limits) => {
                builder = builder.locked_axes(JointAxesMask::LIN_AXES | JointAxesMask::ANG_Y | JointAxesMask::ANG_Z);
                if let Some(limits) = limits {
                    builder = builder.limits(JointAxis::AngX, limits);
                }
            }
            JointLimit::Cone { swing, twist } => {
                if let Some([y, z]) = swing {
                    builder = builder.limits(JointAxis::AngY, [-y, y]).limits(JointAxis::AngZ, [-z, z]);
                }
                if let Some(twist) = twist {
                    builder = builder.limits(JointAxis::AngX, [-twist, twist]);
                }
            }
        }
        self.impulse_joints.insert(parent.0, child.0, builder.build(), true);
    }

    /// Self-collision: jointed parts never collide
    /// (Jolt's `DisableParentChildCollisions`; a jointed pair colliding fights its joint and flings the corpse);
    /// other parts at most two joints apart (siblings too) only not when they already touch where the ragdoll
    /// starts. The `.ka` file's no-collision table, which turns off nearly every pair, is not used.
    fn filter_self_collision(&mut self, joints: &[JointParams], bodies: &[BodyId]) {
        let n = bodies.len();
        let mut near = vec![vec![false; n]; n];
        for joint in joints {
            if joint.child < n && joint.parent < n {
                near[joint.child][joint.parent] = true;
                near[joint.parent][joint.child] = true;
            }
        }
        let jointed = near.clone();
        for (i, row) in near.iter_mut().enumerate() {
            for j in 0..n {
                row[j] = row[j] || (0..n).any(|k| jointed[i][k] && jointed[k][j]);
            }
        }
        for (i, a) in bodies.iter().enumerate() {
            for (j, b) in bodies.iter().enumerate().skip(i + 1) {
                if jointed[i][j] || (near[i][j] && self.overlapping(*a, *b)) {
                    self.hooks.disabled.insert(body_pair(a.0, b.0));
                }
            }
        }
    }

    pub fn remove_ragdoll(&mut self, bodies: &[BodyId]) {
        self.hooks.disabled.retain(|&(a, b)| !bodies.contains(&BodyId(a)) && !bodies.contains(&BodyId(b)));
        for &id in bodies {
            self.take_out_body(id);
        }
    }

    pub fn wake_ragdoll(&mut self, bodies: &[BodyId]) {
        for id in bodies {
            if let Some(body) = self.bodies.get_mut(id.0) {
                body.wake_up(true);
            }
        }
    }

    fn overlapping(&self, a: BodyId, b: BodyId) -> bool {
        let colliders = |id: BodyId| self.bodies.get(id.0).map(|body| body.colliders().to_vec()).unwrap_or_default();
        let (first, second) = (colliders(a), colliders(b));
        first.iter().filter_map(|&h| self.colliders.get(h)).any(|c1| {
            second.iter().filter_map(|&h| self.colliders.get(h)).any(|c2| {
                rapier3d::parry::query::intersection_test(c1.position(), c1.shape(), c2.position(), c2.shape())
                    .is_ok_and(|hit| hit.intersecting)
            })
        })
    }
}

fn axes_rotation(primary: Vector, orthogonal: Vector) -> Rotation {
    let x = primary.try_normalize().unwrap_or(Vector::X);
    let y = (orthogonal - x * orthogonal.dot(x)).try_normalize().unwrap_or_else(|| x.any_orthonormal_vector());
    Rotation::from_mat3(&Mat3::from_cols(x, y, x.cross(y)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inertia_from_shapes_is_the_shapes_at_the_parts_mass() {
        let inertia = shapes_inertia(&[Shape::Sphere(p2_math::Vector::ZERO, 10.0)], 2.0).expect("a sphere has inertia");
        for axis in <[f32; 3]>::from(inertia) {
            assert!((axis - 0.4 * 2.0 * 100.0).abs() < 1e-3, "{inertia:?}");
        }
        assert_eq!(shapes_inertia(&[], 2.0), None);
    }

    fn floor(world: &mut World) {
        let vertices = [[-2000.0, -2000.0, 0.0], [2000.0, -2000.0, 0.0], [2000.0, 2000.0, 0.0], [-2000.0, 2000.0, 0.0]];
        world.add_static_mesh(&vertices, vec![[0, 1, 2], [0, 2, 3]], false).unwrap();
    }

    fn limb(location: p2_math::Vector, velocity: p2_math::Vector) -> PartParams {
        PartParams {
            shapes: vec![Shape::Capsule([0.0; 3].into(), [40.0, 0.0, 0.0].into(), 5.0)],
            location,
            rotation: [0.0, 0.0, 0.0, 1.0],
            mass: 0.07,
            center_of_mass: [20.0, 0.0, 0.0].into(),
            inertia: [2.0, 10.0, 10.0].into(),
            velocity,
        }
    }

    fn chain(height: f32, limit: JointLimit, velocity: p2_math::Vector) -> RagdollParams {
        let parts = (0..3).map(|i| limb([i as f32 * 40.0, 0.0, height].into(), velocity)).collect();
        let joints = (1..3)
            .map(|i| JointParams {
                child: i,
                parent: i - 1,
                child_anchor: [0.0; 3].into(),
                child_axes: [[1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into()],
                parent_anchor: [40.0, 0.0, 0.0].into(),
                parent_axes: [[1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into()],
                limit,
            })
            .collect();
        RagdollParams {
            parts,
            joints,
            linear_damping: 0.2,
            angular_damping: 0.2,
            friction: 0.8,
            restitution: 0.0,
            asleep: false,
        }
    }

    fn origin(world: &World, id: BodyId) -> p2_math::Vector {
        world.pose(id).unwrap().0
    }

    #[test]
    fn flung_corpse_lands_on_one_sided_ground() {
        for spin in [200.0_f32, 600.0, 1000.0] {
            let mut world = World::new([0.0, 0.0, -1500.0].into());
            let vertices = [[-4000.0, -4000.0, 0.0], [4000.0, -4000.0, 0.0], [4000.0, 4000.0, 0.0], [-4000.0, 4000.0, 0.0]];
            world.add_static_mesh(&vertices, vec![[0, 1, 2], [0, 2, 3]], true).unwrap();
            let cone = JointLimit::Cone { swing: Some([1.55, 1.55]), twist: Some(0.1) };
            let mut params = chain(250.0, cone, [0.0; 3].into());
            params.parts = (0..6)
                .map(|i| {
                    let side = if i % 2 == 0 { spin } else { -spin };
                    let mut part = limb([i as f32 * 40.0, 0.0, 250.0].into(), [side, side * 0.5, -1000.0].into());
                    part.inertia = shapes_inertia(&part.shapes, part.mass).unwrap();
                    part
                })
                .collect();
            params.joints = (1..6)
                .map(|i| JointParams { child: i, parent: i - 1, ..params.joints[0].clone() })
                .collect();
            let bodies = world.add_ragdoll(&params);
            for _ in 0..120 {
                world.step(1.0 / 60.0);
            }
            for &b in &bodies {
                let z = origin(&world, b).z;
                assert!(z > -2.0, "spin {spin}: part at height {z}");
            }
        }
    }

    #[test]
    fn chain_falls_holds_together_and_sleeps() {
        let mut world = World::new([0.0, 0.0, -950.0].into());
        floor(&mut world);
        let cone = JointLimit::Cone { swing: Some([0.7, 0.7]), twist: Some(0.3) };
        let bodies = world.add_ragdoll(&chain(150.0, cone, [0.0; 3].into()));
        let mut slept = None;
        for frame in 0..300 {
            world.step(1.0 / 30.0);
            if slept.is_none() && bodies.iter().all(|&b| world.is_sleeping(b)) {
                slept = Some(frame);
            }
        }
        for pair in bodies.windows(2) {
            let (a, b) = (world.pose(pair[0]).unwrap(), origin(&world, pair[1]));
            let end = rotate(a.1, [40.0, 0.0, 0.0].into());
            let gap = (0..3).map(|i| (a.0[i] + end[i] - b[i]).powi(2)).sum::<f32>().sqrt();
            assert!(gap < 1.0, "joint gap {gap}");
        }
        for &b in &bodies {
            let z = origin(&world, b).z;
            assert!((3.0..12.0).contains(&z), "part at height {z}");
        }
        assert!(slept.is_some_and(|f| f < 240), "slept at frame {slept:?}");
    }

    #[test]
    fn hinge_bends_one_way_within_its_limits() {
        let hinge = JointLimit::Hinge(Some([0.0, 0.5]));
        let mut params = chain(500.0, hinge, [0.0; 3].into());
        params.parts.truncate(2);
        params.joints.truncate(1);
        params.linear_damping = 0.0;
        params.angular_damping = 0.0;
        for (spin, expected) in [(4.0, 0.5), (-4.0, 0.0)] {
            let mut world = World::new([0.0; 3].into());
            let bodies = world.add_ragdoll(&params);
            if let Some(body) = world.bodies.get_mut(bodies[1].0) {
                body.set_angvel(Vector::new(spin, 0.0, 0.0), true);
            }
            for _ in 0..30 {
                world.step(1.0 / 30.0);
            }
            let relative = Rotation::from_array(world.pose(bodies[0]).unwrap().1).inverse() * Rotation::from_array(world.pose(bodies[1]).unwrap().1);
            let (axis, angle) = relative.to_axis_angle();
            let signed = if axis.x < 0.0 { -angle } else { angle };
            assert!((signed - expected).abs() < 0.1, "spin {spin}: angle {signed}, expected {expected}");
        }
    }

    #[test]
    fn ragdolls_stack_on_each_other_and_carry_props() {
        let mut world = World::new([0.0, 0.0, -950.0].into());
        floor(&mut world);
        let cone = JointLimit::Cone { swing: Some([0.7, 0.7]), twist: None };
        let mut low = chain(5.0, cone, [0.0; 3].into());
        for (i, part) in low.parts.iter_mut().enumerate() {
            part.location = [60.0, i as f32 * 40.0 - 60.0, 5.0].into();
            part.rotation = [0.0, 0.0, std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2];
        }
        world.add_ragdoll(&low);
        let high = world.add_ragdoll(&chain(60.0, cone, [0.0; 3].into()));
        let prop = world.add_body(&crate::BodyParams {
            shapes: vec![Shape::Box([40.0, 40.0, 5.0].into())],
            location: [60.0, 0.0, 150.0].into(),
            rotation: [0.0, 0.0, 0.0, 1.0],
            mass: 0.1,
            linear_damping: 0.0,
            angular_damping: 0.0,
            friction: 0.8,
            restitution: 0.0,
            asleep: false,
            stay_upright: false,
            allow_yaw: false,
        });
        for _ in 0..180 {
            world.step(1.0 / 60.0);
        }
        let middle = world.center_of_mass(high[1]).unwrap()[2];
        assert!(middle > 10.0, "middle part at {middle}");
        let plate = origin(&world, prop)[2];
        assert!(plate > 12.0, "prop at {plate}");
    }

    #[test]
    fn stabilize_evens_masses_and_keeps_the_total() {
        let params = uneven_chain(0.0);
        let stable = stabilize(&params.parts, &params.joints);
        let total: f32 = stable.iter().map(|s| s.0).sum();
        assert!((total - UNEVEN.iter().map(|u| u.0).sum::<f32>()).abs() < 1e-5, "total {total}");
        for joint in &params.joints {
            let ratio = stable[joint.child].0 / stable[joint.parent].0;
            assert!((0.8 - 1e-5..=1.2 + 1e-5).contains(&ratio), "ratio {ratio}");
        }
    }

    const UNEVEN: [(f32, f32); 4] = [(0.43, 435.0), (0.005, 0.05), (0.43, 435.0), (0.0027, 0.03)];

    fn uneven_chain(height: f32) -> RagdollParams {
        let cone = JointLimit::Cone { swing: Some([0.7, 0.7]), twist: Some(0.3) };
        let mut params = chain(height, cone, [0.0; 3].into());
        params.parts.push(limb([120.0, 0.0, height].into(), [0.0; 3].into()));
        params.joints.push(JointParams { child: 3, parent: 2, ..params.joints[0].clone() });
        for (part, (mass, inertia)) in params.parts.iter_mut().zip(UNEVEN) {
            part.mass = mass;
            part.inertia = [inertia; 3].into();
        }
        params
    }

    fn max_gap(world: &World, bodies: &[BodyId]) -> f32 {
        bodies
            .windows(2)
            .map(|pair| {
                let (a, b) = (world.pose(pair[0]).unwrap(), origin(world, pair[1]));
                let end = rotate(a.1, [40.0, 0.0, 0.0].into());
                (0..3).map(|i| (a.0[i] + end[i] - b[i]).powi(2)).sum::<f32>().sqrt()
            })
            .fold(0.0, f32::max)
    }

    #[test]
    fn impulse_on_a_new_ragdoll_changes_its_velocity_at_once() {
        let mut world = World::new([0.0; 3].into());
        let bodies = world.add_ragdoll(&uneven_chain(0.0));
        world.apply_impulse(bodies[0], [0.0, 43.0, 0.0].into(), None);
        let v = world.velocity(bodies[0]).unwrap();
        let expected = 43.0 / world.mass(bodies[0]).unwrap();
        assert!((v[1] - expected).abs() < 0.1, "velocity {v:?}, expected {expected}");
    }

    #[test]
    fn light_part_impulse_keeps_joints_together() {
        let mut world = World::new([0.0; 3].into());
        let bodies = world.add_ragdoll(&uneven_chain(0.0));
        world.apply_impulse(bodies[1], [0.0, 50.0, 0.0].into(), None);
        let mut gap = 0.0f32;
        for _ in 0..60 {
            world.step(1.0 / 60.0);
            gap = gap.max(max_gap(&world, &bodies));
            for &b in &bodies {
                world.limit_speed(b, 1000.0);
            }
        }
        let momentum: f32 = bodies.iter().map(|&b| world.velocity(b).unwrap()[1] * world.mass(b).unwrap()).sum();
        assert!(gap < 2.0, "joint gap {gap}");
        assert!((momentum - 50.0 * (-0.2f32).exp()).abs() < 1.0, "momentum {momentum}");
    }

    #[test]
    fn uneven_chain_falls_at_gravity_and_rests_together() {
        let mut world = World::new([0.0, 0.0, -950.0].into());
        floor(&mut world);
        let mut params = uneven_chain(300.0);
        params.linear_damping = 0.0;
        let bodies = world.add_ragdoll(&params);
        for _ in 0..30 {
            world.step(1.0 / 60.0);
        }
        let drop = 300.0 - origin(&world, bodies[0])[2];
        assert!((drop - 118.75).abs() < 3.0, "fell {drop}");
        let mut gap = 0.0f32;
        for _ in 0..240 {
            world.step(1.0 / 60.0);
            gap = gap.max(max_gap(&world, &bodies));
            for &b in &bodies {
                world.limit_speed(b, 1000.0);
            }
        }
        assert!(gap < 2.0, "joint gap {gap}");
        for &b in &bodies {
            let z = origin(&world, b)[2];
            assert!((3.0..12.0).contains(&z), "part at height {z}");
        }
    }

    fn rotate(q: [f32; 4], v: p2_math::Vector) -> p2_math::Vector {
        (Rotation::from_array(q) * Vector::from_array(v.into())).to_array().into()
    }
}
