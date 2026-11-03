use p2_assets::Terrain;
use p2_physics::{BodyId, BodyParams, JointLimit, JointParams, PartParams, RagdollParams, Shape, World};

const GRAVITY: [f32; 3] = [0.0, 0.0, -950.0];
const SIZE: usize = 64;

fn terrain() -> Terrain {
    let (width, height) = (SIZE + 1, SIZE + 1);
    let scale = [64.0, 64.0, 64.0];
    let mut terrain = Terrain {
        width,
        height,
        heightmap: 0,
        scale: scale.into(),
        location: [3000.0, -1500.0, 200.0].into(),
        vertices: Vec::new(),
        vertex_colors: Vec::new(),
        quad_visibility: Vec::new(),
        edge_turn: vec![0; (width * height).div_ceil(32)],
        layers: Vec::new(),
        sector_size: 16,
        inverted: false,
        deco_layers: Vec::new(),
    };
    for quad in 0..width * height {
        if (quad % width + quad / width) % 2 == 0 {
            terrain.edge_turn[quad / 32] |= 1 << (quad % 32);
        }
    }
    let rise = 64.0 * 30f32.to_radians().tan() * 256.0 / scale[2];
    let heights: Vec<u16> = (0..width * height)
        .map(|i| {
            let (x, y) = ((i % width) as f32, (i / width) as f32);
            (32767.0 + (x - (SIZE / 2) as f32) * rise + 80.0 * (y * 0.9).sin()) as u16
        })
        .collect();
    terrain.vertices = terrain.vertices_from_heights(&heights);
    terrain
}

fn add_terrain(world: &mut World, terrain: &Terrain) {
    let triangles = terrain.karma_triangles().into_iter().map(|t| t.map(|i| i as u32)).collect();
    world.add_static_mesh(&terrain.vertices, triangles, true).unwrap();
}

fn surface(terrain: &Terrain, x: f32, y: f32) -> f32 {
    let cell = |p: f32, axis: usize| (p - terrain.location[axis]) / terrain.scale[axis] + (SIZE / 2) as f32;
    let (cx, cy) = (cell(x, 0), cell(y, 1));
    assert!((0.0..SIZE as f32).contains(&cx) && (0.0..SIZE as f32).contains(&cy), "off the terrain at {x} {y}");
    let (qx, qy) = (cx.floor() as usize, cy.floor() as usize);
    for [a, b, c] in terrain.quad_triangles(qx, qy) {
        let [a, b, c] = [a, b, c].map(|i| terrain.vertices[i]);
        let det = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
        let u = ((b[1] - c[1]) * (x - c[0]) + (c[0] - b[0]) * (y - c[1])) / det;
        let v = ((c[1] - a[1]) * (x - c[0]) + (a[0] - c[0]) * (y - c[1])) / det;
        let w = 1.0 - u - v;
        if u >= -1e-4 && v >= -1e-4 && w >= -1e-4 {
            return u * a[2] + v * b[2] + w * c[2];
        }
    }
    f32::NAN
}

fn above(terrain: &Terrain, cells: [f32; 2], up: f32) -> [f32; 3] {
    let (x, y) = (terrain.location[0] + cells[0] * 64.0 + 32.0, terrain.location[1] + cells[1] * 64.0 + 32.0);
    [x, y, surface(terrain, x, y) + up]
}

fn part(location: [f32; 3], length: f32, radius: f32, velocity: [f32; 3]) -> PartParams {
    let shape = if length > 0.0 { Shape::Capsule([0.0; 3].into(), [length, 0.0, 0.0].into(), radius) } else { Shape::Sphere([0.0; 3].into(), radius) };
    PartParams {
        shapes: vec![shape],
        location: location.into(),
        rotation: [0.0, 0.0, 0.0, 1.0],
        mass: 0.05,
        center_of_mass: [length / 2.0, 0.0, 0.0].into(),
        inertia: [0.5, 2.0, 2.0].into(),
        velocity: velocity.into(),
    }
}

const CHAIN: [(f32, f32); 4] = [(30.0, 10.0), (25.0, 4.0), (25.0, 3.5), (0.0, 3.0)];

fn chain(start: [f32; 3], velocity: [f32; 3]) -> RagdollParams {
    let mut x = start[0];
    let mut parts = Vec::new();
    for (length, radius) in CHAIN {
        parts.push(part([x, start[1], start[2]], length, radius, velocity));
        x += length;
    }
    let joints = (1..parts.len())
        .map(|i| JointParams {
            child: i,
            parent: i - 1,
            child_anchor: [0.0; 3].into(),
            child_axes: [[1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into()],
            parent_anchor: [CHAIN[i - 1].0, 0.0, 0.0].into(),
            parent_axes: [[1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into()],
            limit: JointLimit::Cone { swing: Some([1.2, 1.2]), twist: Some(0.5) },
        })
        .collect();
    RagdollParams {
        parts,
        joints,
        linear_damping: 0.2,
        angular_damping: 0.2,
        friction: 0.7,
        restitution: 0.3,
        asleep: false,
        safe_point: start.into(),
    }
}

fn centres(world: &World, bodies: &[BodyId]) -> Vec<([f32; 3], f32)> {
    let mut out = Vec::new();
    for (&id, (length, radius)) in bodies.iter().zip(CHAIN) {
        let (origin, [qx, qy, qz, qw]) = world.pose(id).unwrap();
        let axis = [1.0 - 2.0 * (qy * qy + qz * qz), 2.0 * (qx * qy + qz * qw), 2.0 * (qx * qz - qy * qw)];
        for t in [0.0, 0.5, 1.0] {
            out.push(([0, 1, 2].map(|i| origin[i] + axis[i] * length * t), radius));
        }
    }
    out
}

fn clearance(world: &World, bodies: &[BodyId], terrain: &Terrain) -> f32 {
    centres(world, bodies).into_iter().map(|(p, r)| (p[2] - surface(terrain, p[0], p[1])) / r).fold(f32::MAX, f32::min)
}

fn run(world: &mut World, bodies: &[BodyId], terrain: &Terrain, seconds: f32, frame: f32) -> f32 {
    let mut lowest = f32::MAX;
    for _ in 0..(seconds / frame) as u32 {
        world.step(frame);
        for &id in bodies {
            world.limit_speed(id, 1000.0);
        }
        lowest = lowest.min(clearance(world, bodies, terrain));
    }
    lowest
}

#[test]
fn surface_matches_the_vertices() {
    let terrain = terrain();
    for (i, v) in terrain.vertices.iter().enumerate().filter(|(i, _)| i % (SIZE + 1) < SIZE && i / (SIZE + 1) < SIZE) {
        assert!((surface(&terrain, v[0], v[1]) - v[2]).abs() < 0.01, "vertex {i}");
    }
}

#[test]
fn karma_triangles_face_up() {
    let terrain = terrain();
    for [a, b, c] in terrain.karma_triangles().into_iter().map(|t| t.map(|i| terrain.vertices[i])) {
        let (u, v) = ([0, 1, 2].map(|i| b[i] - a[i]), [0, 1, 2].map(|i| c[i] - b[i]));
        assert!(u[0] * v[1] - u[1] * v[0] > 0.0, "{a:?} {b:?} {c:?}");
    }
}

#[test]
fn chain_dropped_onto_sloped_terrain_rests_on_it() {
    let terrain = terrain();
    for (cells, velocity) in [([-4.0, 2.0], [0.0, 0.0, 0.0]), ([3.0, -5.0], [600.0, -300.0, -1000.0]), ([0.0, 0.0], [-800.0, 0.0, -600.0])] {
        let mut world = World::new(GRAVITY.into());
        add_terrain(&mut world, &terrain);
        let bodies = world.add_ragdoll(&chain(above(&terrain, cells, 300.0), velocity));
        let lowest = run(&mut world, &bodies, &terrain, 6.0, 1.0 / 30.0);
        let rest = clearance(&world, &bodies, &terrain);
        assert!(lowest > 0.8, "{cells:?} {velocity:?}: a part sank to {lowest} of its radius");
        assert!(rest > 0.9, "{cells:?} {velocity:?}: rests at {rest} of a radius");
    }
}

#[test]
fn chain_started_partly_under_the_terrain_comes_out() {
    let terrain = terrain();
    for frame in [1.0 / 30.0, 1.0 / 100.0] {
        let mut world = World::new(GRAVITY.into());
        add_terrain(&mut world, &terrain);
        let bodies = world.add_ragdoll(&chain(above(&terrain, [-2.0, 1.0], 12.0), [0.0; 3]));
        let before = clearance(&world, &bodies, &terrain);
        assert!(before < -10.0, "the hand starts under the surface: {before}");
        run(&mut world, &bodies, &terrain, 2.0, frame);
        let lowest = run(&mut world, &bodies, &terrain, 4.0, frame);
        let rest = clearance(&world, &bodies, &terrain);
        assert!(lowest > 0.8, "{frame}: after 2 s a part was at {lowest} of its radius");
        assert!(rest > 0.9, "{frame}: rests at {rest} of a radius");
    }
}

#[test]
fn fast_small_sphere_does_not_tunnel() {
    let terrain = terrain();
    let radius = 3.0;
    for velocity in [[0.0, 0.0, -1000.0], [700.0, 0.0, -700.0], [-700.0, 300.0, -700.0]] {
        let mut world = World::new(GRAVITY.into());
        add_terrain(&mut world, &terrain);
        let start = above(&terrain, [0.0, 0.0], 150.0);
        let body = world.add_body(&BodyParams {
            shapes: vec![Shape::Sphere([0.0; 3].into(), radius)],
            location: start.into(),
            rotation: [0.0, 0.0, 0.0, 1.0],
            mass: 0.05,
            linear_damping: 0.0,
            angular_damping: 0.0,
            friction: 0.7,
            restitution: 0.3,
            asleep: false,
            stay_upright: false,
            allow_yaw: false,
        });
        world.set_velocity(body, velocity.into());
        let mut lowest = f32::MAX;
        for _ in 0..60 {
            world.step(1.0 / 30.0);
            world.limit_speed(body, 1000.0);
            let p = world.pose(body).unwrap().0;
            lowest = lowest.min((p[2] - surface(&terrain, p[0], p[1])) / radius);
        }
        assert!(lowest > 0.8, "{velocity:?}: sank to {lowest} of its radius");
    }
}
