use p2_physics::{BodyId, JointLimit, JointParams, PartParams, RagdollParams, Shape, World};

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
    world.add_static_mesh(&vertices, triangles, true).unwrap();
}

fn add_slab(world: &mut World, near: f32, far: f32, one_sided: bool) {
    let (y, z) = (HALF[1], HALF[2]);
    let vertices = [[near, -y, -z], [near, -y, z], [near, y, z], [near, y, -z], [far, -y, -z], [far, y, -z], [far, y, z], [far, -y, z]];
    world.add_static_mesh(&vertices, vec![[0, 1, 2], [0, 2, 3], [4, 5, 6], [4, 6, 7]], one_sided).unwrap();
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
    RagdollParams { parts, joints, linear_damping: 0.2, angular_damping: 0.2, friction: 0.3, restitution: 0.0, asleep: false, safe_point: at.into() }
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
    for (one_sided, thickness) in [(true, 8.0), (true, 0.0), (false, 8.0), (false, 0.0)] {
        let (near, far) = (100.0 - thickness, 100.0);
        let mut world = World::new(GRAVITY.into());
        add_room(&mut world);
        add_slab(&mut world, near, far, one_sided);
        let bodies = world.add_ragdoll(&corpse([near - 20.0, 0.0, -HALF[2] + 30.0]));
        let mut top = 0.0f32;
        for _ in 0..240 {
            step(&mut world, &bodies, STEP);
            top = bodies.iter().map(|&b| length(world.velocity(b).unwrap())).fold(top, f32::max);
        }
        let past = centres(&world, &bodies).into_iter().map(|p| p[0] - near).fold(f32::MIN, f32::max);
        assert!(past < 0.0, "one sided {one_sided}, thickness {thickness}: a part is {past} past the wall");
        assert!(top < 600.0, "one sided {one_sided}, thickness {thickness}: top speed {top}");
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
        add_slab(&mut world, near, far, true);
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
