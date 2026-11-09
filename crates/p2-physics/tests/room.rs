use p2_physics::{BodyId, BodyParams, JointLimit, JointParams, PartParams, RagdollParams, Shape, World};

const GRAVITY: [f32; 3] = [0.0, 0.0, -950.0];
const HALF: [f32; 3] = [300.0, 300.0, 150.0];
const MAX_SPEED: f32 = 1000.0;
const STEP: f32 = 1.0 / 60.0;

fn add_room(world: &mut World) {
    let [x, y, z] = HALF;
    let walls: [[[f32; 3]; 4]; 6] = [
        [[-x, -y, -z], [x, -y, -z], [x, y, -z], [-x, y, -z]],
        [[-x, -y, z], [-x, y, z], [x, y, z], [x, -y, z]],
        [[-x, -y, -z], [-x, y, -z], [-x, y, z], [-x, -y, z]],
        [[x, -y, -z], [x, -y, z], [x, y, z], [x, y, -z]],
        [[-x, -y, -z], [-x, -y, z], [x, -y, z], [x, -y, -z]],
        [[-x, y, -z], [x, y, -z], [x, y, z], [-x, y, z]],
    ];
    let vertices: Vec<[f32; 3]> = walls.iter().flatten().copied().collect();
    let triangles = (0..6u32).flat_map(|w| [[w * 4, w * 4 + 1, w * 4 + 2], [w * 4, w * 4 + 2, w * 4 + 3]]).collect();
    world.add_static_mesh(&vertices, triangles).unwrap();
}

fn add_slab(world: &mut World, near: f32, far: f32) {
    let (y, z) = (HALF[1], HALF[2]);
    let vertices = [[near, -y, -z], [near, -y, z], [near, y, z], [near, y, -z], [far, -y, -z], [far, y, -z], [far, y, z], [far, -y, z]];
    world.add_static_mesh(&vertices, vec![[0, 1, 2], [0, 2, 3], [4, 5, 6], [4, 6, 7]]).unwrap();
}

fn outside(p: p2_math::Vector) -> f32 {
    (0..3).map(|i| p[i].abs() - HALF[i]).fold(f32::MIN, f32::max)
}

fn corpse(at: [f32; 3]) -> RagdollParams {
    const BONES: [(usize, [f32; 3], f32, f32); 10] = [
        (0, [0.0; 3], 12.0, 9.0),
        (0, [12.0, 0.0, 0.0], 18.0, 8.0),
        (1, [18.0, 0.0, 0.0], 0.0, 6.0),
        (1, [15.0, 10.0, 0.0], 13.0, 3.5),
        (3, [13.0, 0.0, 0.0], 13.0, 3.0),
        (1, [15.0, -10.0, 0.0], 13.0, 3.5),
        (5, [13.0, 0.0, 0.0], 13.0, 3.0),
        (0, [0.0, 6.0, 0.0], -20.0, 5.0),
        (7, [-20.0, 0.0, 0.0], -20.0, 4.0),
        (0, [0.0, -6.0, 0.0], -20.0, 5.0),
    ];
    let mut origins = [[0.0f32; 3]; 10];
    let mut parts = Vec::new();
    let mut joints = Vec::new();
    for (i, &(parent, offset, length, radius)) in BONES.iter().enumerate() {
        origins[i] = [0, 1, 2].map(|k| origins[parent][k] + offset[k]);
        let shape = if length == 0.0 { Shape::Sphere([0.0; 3].into(), radius) } else { Shape::Capsule([0.0; 3].into(), [length, 0.0, 0.0].into(), radius) };
        parts.push(PartParams {
            shapes: vec![shape.clone()],
            location: [0, 1, 2].map(|k| at[k] + origins[i][k]).into(),
            rotation: [0.0, 0.0, 0.0, 1.0],
            mass: 0.05,
            center_of_mass: [length / 2.0, 0.0, 0.0].into(),
            inertia: p2_physics::shapes_inertia(&[shape], 0.05).unwrap(),
            velocity: [0.0; 3].into(),
        });
        if i > 0 {
            joints.push(JointParams {
                child: i,
                parent,
                child_anchor: [0.0; 3].into(),
                child_axes: [[1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into()],
                parent_anchor: offset.into(),
                parent_axes: [[1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into()],
                limit: JointLimit::Cone { swing: Some([1.3, 1.3]), twist: Some(0.4) },
            });
        }
    }
    RagdollParams { parts, joints, linear_damping: 0.2, angular_damping: 0.2, friction: 0.3, restitution: 0.0, asleep: false }
}

struct Lcg(u32);

impl Lcg {
    fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12345);
        ((self.0 >> 16) & 0x7fff) as f32 / 16383.5 - 1.0
    }
}

fn step(world: &mut World, bodies: &[BodyId], frame: f32) {
    world.step(frame);
    for &id in bodies {
        world.limit_speed(id, MAX_SPEED);
    }
}

fn centres(world: &World, bodies: &[BodyId]) -> Vec<p2_math::Vector> {
    bodies.iter().map(|&b| world.center_of_mass(b).unwrap()).collect()
}

fn deepest(world: &World, bodies: &[BodyId]) -> f32 {
    centres(world, bodies).into_iter().map(outside).fold(f32::MIN, f32::max)
}

fn length(v: p2_math::Vector) -> f32 {
    v.dot(v).sqrt()
}

#[test]
fn limbs_started_through_a_wall_come_back() {
    for frame in [STEP, 0.05] {
        let mut world = World::new(GRAVITY.into());
        add_room(&mut world);
        let bodies = world.add_ragdoll(&corpse([HALF[0] - 25.0, 0.0, -HALF[2] + 30.0]));
        for _ in 0..(3.0 / frame) as u32 {
            step(&mut world, &bodies, frame);
        }
        let depth = deepest(&world, &bodies);
        assert!(depth < 0.0, "frame {frame}: a part is {depth} outside");
    }
}

#[test]
fn limbs_started_through_a_thin_wall_come_back_without_a_fling() {
    for thickness in [64.0, 0.0] {
        let (near, far) = (100.0 - thickness, 100.0);
        let mut world = World::new(GRAVITY.into());
        add_room(&mut world);
        add_slab(&mut world, near, far);
        let bodies = world.add_ragdoll(&corpse([near - 20.0, 0.0, -HALF[2] + 30.0]));
        let mut top = 0.0f32;
        for _ in 0..240 {
            step(&mut world, &bodies, STEP);
            top = bodies.iter().map(|&b| length(world.velocity(b).unwrap())).fold(top, f32::max);
        }
        let past = centres(&world, &bodies).into_iter().map(|p| p[0] - near).fold(f32::MIN, f32::max);
        assert!(past < 0.0, "thickness {thickness}: a part is {past} past the wall");
        assert!(top < 600.0, "thickness {thickness}: top speed {top}");
    }
}

#[test]
fn hard_kicks_in_a_corner_stay_in_the_room() {
    for seed in 1..=20 {
        let mut rng = Lcg(seed * 7919);
        let mut world = World::new(GRAVITY.into());
        add_room(&mut world);
        let bodies = world.add_ragdoll(&corpse([HALF[0] - 60.0, HALF[1] - 40.0, -HALF[2] + 20.0]));
        for frame in 0..240 {
            if frame % 12 == 0 && frame < 120 {
                let body = bodies[(rng.unit().abs() * 9.0) as usize];
                world.apply_impulse(body, [300.0, 300.0 * rng.unit().abs(), -300.0 * rng.unit().abs()].into(), None);
            }
            step(&mut world, &bodies, STEP);
        }
        let depth = deepest(&world, &bodies);
        assert!(depth < 0.0, "seed {seed}: a part is {depth} outside");
    }
}

#[test]
fn a_corpse_fired_at_a_thin_wall_stays_on_its_side() {
    for frame in [STEP, 0.05] {
        let (near, far) = (92.0, 100.0);
        let mut world = World::new(GRAVITY.into());
        add_room(&mut world);
        add_slab(&mut world, near, far);
        let bodies = world.add_ragdoll(&corpse([near - 60.0, 0.0, 0.0]));
        for &b in &bodies {
            world.set_velocity(b, [20000.0, 0.0, 0.0].into());
        }
        for _ in 0..(1.0 / frame) as u32 {
            world.step(frame);
        }
        let past = centres(&world, &bodies).into_iter().map(|p| p[0] - near).fold(f32::MIN, f32::max);
        assert!(past < 0.0, "frame {frame}: a part is {past} past the wall");
    }
}

fn settled_pile(world: &mut World, count: usize) -> Vec<BodyId> {
    add_room(world);
    let mut all = Vec::new();
    for i in 0..count {
        all.extend(world.add_ragdoll(&corpse([(i % 3) as f32 * 8.0, (i / 3) as f32 * 8.0, -HALF[2] + 30.0 + i as f32 * 25.0])));
    }
    for _ in 0..1200 {
        world.step(STEP);
    }
    all
}

#[test]
fn a_pile_of_corpses_goes_to_sleep() {
    for count in [2, 3, 5, 8] {
        let mut world = World::new(GRAVITY.into());
        let all = settled_pile(&mut world, count);
        let awake = all.iter().filter(|&&b| !world.is_sleeping(b)).count();
        assert_eq!(awake, 0, "{count} corpses: {awake} of {} parts awake", all.len());
    }
}

#[test]
fn a_kick_moves_a_sleeping_pile_at_full_strength() {
    let mut world = World::new(GRAVITY.into());
    let all = settled_pile(&mut world, 8);
    let before = centres(&world, &all);
    world.apply_impulse(all[0], [0.0, 0.0, 60.0].into(), None);
    world.step(STEP);
    assert!(!world.is_sleeping(all[0]));
    for _ in 0..10 {
        world.step(STEP);
    }
    let moved = centres(&world, &all).into_iter().zip(before).map(|(now, then)| length(now - then)).fold(0.0, f32::max);
    assert!(moved > 10.0, "the most any part moved is {moved}");
}

fn q_axis(axis: [f32; 3], angle: f32) -> [f32; 4] {
    let (s, c) = (angle / 2.0).sin_cos();
    [axis[0] * s, axis[1] * s, axis[2] * s, c]
}

fn kneeling(sink: f32) -> RagdollParams {
    let floor = -HALF[2];
    let upright = q_axis([0.0, 1.0, 0.0], -std::f32::consts::FRAC_PI_2);
    let flat = [0.0, 0.0, 0.0, 1.0];
    let (knee, hip) = (floor - sink, floor - sink + 36.0);
    let bones: [(usize, [f32; 3], [f32; 3], [f32; 4], f32, f32); 11] = [
        (0, [0.0; 3], [0.0, 0.0, hip], upright, 20.0, 12.0),
        (0, [20.0, 0.0, 0.0], [0.0, 0.0, hip + 20.0], upright, 30.0, 12.0),
        (1, [30.0, 0.0, 0.0], [0.0, 0.0, hip + 50.0], upright, 0.0, 9.0),
        (1, [26.0, 16.0, 0.0], [0.0, 16.0, hip + 46.0], upright, 22.0, 5.0),
        (3, [22.0, 0.0, 0.0], [0.0, 16.0, hip + 68.0], upright, 22.0, 4.0),
        (1, [26.0, -16.0, 0.0], [0.0, -16.0, hip + 46.0], upright, 22.0, 5.0),
        (5, [22.0, 0.0, 0.0], [0.0, -16.0, hip + 68.0], upright, 22.0, 4.0),
        (0, [0.0, 9.0, 0.0], [0.0, 9.0, hip], upright, -36.0, 7.0),
        (7, [-36.0, 0.0, 0.0], [0.0, 9.0, knee], flat, -36.0, 5.0),
        (0, [0.0, -9.0, 0.0], [0.0, -9.0, hip], upright, -36.0, 7.0),
        (9, [-36.0, 0.0, 0.0], [0.0, -9.0, knee], flat, -36.0, 5.0),
    ];
    let mut parts = Vec::new();
    let mut joints = Vec::new();
    for (i, &(parent, anchor, location, rotation, length, radius)) in bones.iter().enumerate() {
        let shape = if length == 0.0 { Shape::Sphere([0.0; 3].into(), radius) } else { Shape::Capsule([0.0; 3].into(), [length, 0.0, 0.0].into(), radius) };
        parts.push(PartParams {
            shapes: vec![shape.clone()],
            location: location.into(),
            rotation,
            mass: 0.05,
            center_of_mass: [length / 2.0, 0.0, 0.0].into(),
            inertia: p2_physics::shapes_inertia(&[shape], 0.05).unwrap(),
            velocity: [0.0; 3].into(),
        });
        if i > 0 {
            joints.push(JointParams {
                child: i,
                parent,
                child_anchor: [0.0; 3].into(),
                child_axes: [[1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into()],
                parent_anchor: anchor.into(),
                parent_axes: [[1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into()],
                limit: JointLimit::Cone { swing: Some([1.8, 1.8]), twist: Some(0.4) },
            });
        }
    }
    RagdollParams { parts, joints, linear_damping: 0.2, angular_damping: 0.2, friction: 0.3, restitution: 0.0, asleep: false }
}

#[test]
fn a_kneeling_corpse_is_not_thrown() {
    for sink in [-6.0, -2.0, 0.0, 2.0, 4.0, 6.0, 8.0, 12.0, 20.0] {
        let mut world = World::new(GRAVITY.into());
        add_room(&mut world);
        let bodies = world.add_ragdoll(&kneeling(sink));
        let start = centres(&world, &bodies);
        let (mut top, mut rise) = (0.0f32, 0.0f32);
        for _ in 0..240 {
            step(&mut world, &bodies, STEP);
            top = bodies.iter().map(|&b| length(world.velocity(b).unwrap())).fold(top, f32::max);
            rise = centres(&world, &bodies).iter().zip(&start).map(|(now, then)| now[2] - then[2]).fold(rise, f32::max);
        }
        let lowest = centres(&world, &bodies).iter().map(|p| p[2] + HALF[2]).fold(f32::MAX, f32::min);
        assert!(top < 600.0, "sink {sink}: top speed {top}");
        assert!(rise < 60.0, "sink {sink}: a part rose {rise}");
        assert!(lowest > 0.0, "sink {sink}: a part is {lowest} above the floor");
    }
}

#[test]
fn a_prop_with_its_origin_under_the_floor_stays_on_it() {
    let floor = -HALF[2];
    let mut world = World::new(GRAVITY.into());
    add_room(&mut world);
    let corners: Vec<[f32; 3]> = (0..8).map(|i| [if i & 1 == 0 { -20.0 } else { 20.0 }, if i & 2 == 0 { -20.0 } else { 20.0 }, if i & 4 == 0 { 10.0 } else { 50.0 }]).collect();
    let prop = world.add_body(&BodyParams {
        shapes: vec![Shape::Hull(corners)],
        location: [0.0, 0.0, floor - 9.0].into(),
        rotation: [0.0, 0.0, 0.0, 1.0],
        mass: 1.0,
        linear_damping: 0.1,
        angular_damping: 0.1,
        friction: 0.7,
        restitution: 0.0,
        asleep: false,
        stay_upright: false,
        allow_yaw: false,
    });
    for _ in 0..180 {
        world.step(STEP);
    }
    let origin = world.pose(prop).unwrap().0[2] - floor;
    assert!((-14.0..0.0).contains(&origin), "the origin ended {origin} from the floor");
}

#[test]
fn a_ball_dropped_into_an_open_cabin_mover_lands_inside() {
    let (r, h) = (100.0, 120.0);
    let quad = |q: [[f32; 3]; 4]| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]];
    let triangles: Vec<[[f32; 3]; 3]> = [
        [[-r, -r, 0.0], [r, -r, 0.0], [r, r, 0.0], [-r, r, 0.0]],
        [[-r, -r, 0.0], [-r, r, 0.0], [-r, r, h], [-r, -r, h]],
        [[r, -r, 0.0], [r, -r, h], [r, r, h], [r, r, 0.0]],
        [[-r, -r, 0.0], [-r, -r, h], [r, -r, h], [r, -r, 0.0]],
        [[-r, r, 0.0], [r, r, 0.0], [r, r, h], [-r, r, h]],
    ]
    .into_iter()
    .flat_map(quad)
    .collect();
    let mut world = World::new(GRAVITY.into());
    world.add_mover(&[Shape::Mesh(triangles)], [0.0; 3].into(), [0.0, 0.0, 0.0, 1.0]).unwrap();
    let ball = world.add_body(&BodyParams {
        shapes: vec![Shape::Sphere([0.0; 3].into(), 10.0)],
        location: [0.0, 0.0, 200.0].into(),
        rotation: [0.0, 0.0, 0.0, 1.0],
        mass: 1.0,
        linear_damping: 0.1,
        angular_damping: 0.1,
        friction: 0.7,
        restitution: 0.0,
        asleep: false,
        stay_upright: false,
        allow_yaw: false,
    });
    for _ in 0..120 {
        world.step(STEP);
    }
    let z = world.pose(ball).unwrap().0[2];
    assert!((5.0..20.0).contains(&z), "the ball rests at {z}, not on the cabin floor");
}
