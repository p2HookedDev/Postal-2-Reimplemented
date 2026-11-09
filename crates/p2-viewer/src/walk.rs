//! The viewer's walk mode (`G`): a simple body for looking around a map at eye height. Not the game's player: the game
//! moves its pawn by the VM's physics. Unreal units and coordinates.

use p2_collision::{Hit, Sides, box_check, mesh_line_check, terrain_line_check};
use p2_engine::map::StaticCollision;
use p2_math::Vector;

const HALF_HEIGHT: f32 = 72.0;
const RADIUS: f32 = 28.0;
pub const EYE_HEIGHT: f32 = 60.0;
const SPEED: f32 = 440.0;
const GRAVITY: f32 = 950.0;
const JUMP_SPEED: f32 = 400.0;
const STEP: f32 = 35.0;
const MAX_FALL_SPEED: f32 = 4000.0;
const MIN_FLOOR_NORMAL: f32 = 0.7;

#[derive(Debug, Clone, Copy, Default)]
pub struct Body {
    pub center: Vector,
    pub fall_speed: f32,
    pub grounded: bool,
}

/// The bottom is `STEP` raised, so ledges up to `STEP` pass underneath: a box against BSP, a knee-height line against
/// terrain and meshes.
fn sweep(world: &StaticCollision, start: Vector, end: Vector) -> Option<Hit> {
    let raise = Vector::new(0.0, 0.0, STEP / 2.0);
    let extent = Vector::new(RADIUS, RADIUS, HALF_HEIGHT - STEP / 2.0);
    let bsp = box_check(&world.model, start + raise, end + raise, extent);
    let knee = Vector::new(0.0, 0.0, STEP - HALF_HEIGHT);
    let (from, to) = (start + knee, end + knee);
    let terrain = world.terrains.iter().filter_map(|t| terrain_line_check(t, from, to, Sides::Both));
    let mesh = mesh_line_check(&world.meshes, from, to);
    bsp.into_iter().chain(terrain).chain(mesh).min_by(|a, b| a.time.total_cmp(&b.time))
}

/// `wish`: horizontal direction (X, Y; length up to 1); `delta`: seconds.
pub fn step(world: &StaticCollision, body: &mut Body, wish: [f32; 2], speed: f32, jump: bool, delta: f32) {
    let mut motion = Vector::new(wish[0] * SPEED * speed * delta, wish[1] * SPEED * speed * delta, 0.0);
    for _ in 0..2 {
        if motion.x == 0.0 && motion.y == 0.0 {
            break;
        }
        match sweep(world, body.center, body.center + motion) {
            None => {
                body.center += motion;
                break;
            }
            Some(hit) => {
                let used = (hit.time - 0.01).max(0.0);
                body.center += motion * used;
                let rest = motion * (1.0 - used);
                let into = rest.x * hit.normal.x + rest.y * hit.normal.y;
                motion = Vector::new(rest.x - hit.normal.x * into, rest.y - hit.normal.y * into, 0.0);
            }
        }
    }
    if jump && body.grounded {
        body.fall_speed = -JUMP_SPEED;
        body.grounded = false;
    }
    body.fall_speed = (body.fall_speed + GRAVITY * delta).min(MAX_FALL_SPEED);
    let reach = if body.grounded { STEP } else { 0.0 };
    let fall = body.fall_speed * delta;
    let Vector { x, y, z } = body.center;
    let from = Vector::new(x, y, z + reach);
    let to = Vector::new(x, y, z - HALF_HEIGHT - fall.max(0.0) - reach);
    match (body.fall_speed >= 0.0).then(|| world.first_hit(from, to)).flatten().filter(|hit| hit.normal.z > MIN_FLOOR_NORMAL) {
        Some(floor) => {
            body.center.z = floor.location.z + HALF_HEIGHT;
            body.fall_speed = 0.0;
            body.grounded = true;
        }
        None => {
            if fall < 0.0 {
                let head = Vector::new(x, y, z + HALF_HEIGHT);
                if world.first_hit(head, Vector::new(x, y, head.z - fall)).is_some() {
                    body.fall_speed = 0.0;
                    return;
                }
            }
            body.center.z -= fall;
            body.grounded = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use p2_assets::{read_actor_location, read_level, read_model, read_terrain, read_texture, texture_heights};
    use p2_collision::line_check;
    use p2_package::{LoadedPackage, Loader};
    use p2_script::{Layouts, Objects, Source};

    use super::*;

    #[test]
    fn walk_on_church() {
        let Some(dir) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else {
            eprintln!("P2_GAME_DIR unset, skipping");
            return;
        };
        let mut loader = Loader::open(&dir.join("System")).unwrap();
        loader.load_scripts().unwrap();
        let map = loader.load("Church").unwrap();
        let LoadedPackage { package: pkg, data, .. } = loader.package(map);
        let mut layouts = Layouts::new(&loader);
        let mut terrains = Vec::new();
        for (index, export) in pkg.exports.iter().enumerate() {
            if pkg.object_name(export.class_index) == Some("TerrainInfo") {
                let layout = layouts.class_layout(Source { package: pkg, data }, index).unwrap();
                let mut terrain = read_terrain(pkg, data, export, &layout).unwrap();
                let source = Source { package: pkg, data };
                let heightmap = read_texture(&mut Objects::new(&loader), source, terrain.heightmap as usize - 1).unwrap();
                terrain.vertices = terrain.vertices_from_heights(&texture_heights(&heightmap).unwrap().2);
                terrains.push(terrain);
            }
        }
        let level = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("Level")).unwrap();
        let level = read_level(pkg, data, level).unwrap();
        let model = read_model(pkg, data, &pkg.exports[level.model as usize - 1]).unwrap();
        let start = (0..pkg.exports.len())
            .find(|&i| pkg.object_name(pkg.exports[i].class_index) == Some("PlayerStart"))
            .map(|i| <[f32; 3]>::from(read_actor_location(&mut Objects::new(&loader), Source { package: pkg, data }, i).unwrap()))
            .unwrap();
        let world = StaticCollision { model, terrains, meshes: Vec::new() };

        let mut body = Body { center: start.into(), ..Body::default() };
        for _ in 0..60 {
            step(&world, &mut body, [0.0, 0.0], 1.0, false, 1.0 / 30.0);
        }
        assert!(body.grounded, "{body:?}");
        let settled = body.center[2];
        assert!((settled - (start[2] - 8.0)).abs() < 2.0, "{body:?}");

        let mut legs = Vec::new();
        for wish in [[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]] {
            let before = body.center;
            for _ in 0..30 {
                step(&world, &mut body, wish, 1.0, false, 1.0 / 30.0);
                assert!(body.center[2] > start[2] - 500.0, "fell through: {body:?}");
            }
            for _ in 0..30 {
                step(&world, &mut body, [0.0, 0.0], 1.0, false, 1.0 / 30.0);
            }
            assert!(body.grounded, "{wish:?}: {body:?}");
            legs.push(((body.center[0] - before[0]).powi(2) + (body.center[1] - before[1]).powi(2)).sqrt());
        }
        eprintln!("settled {settled}; legs walked {legs:?}");
        assert!(legs.iter().any(|&leg| (leg - 440.0).abs() < 5.0), "{legs:?}");

        let model = &world.model;
        for angle in (0..8).map(|i| i as f32 * std::f32::consts::FRAC_PI_4) {
            let wish = [angle.cos(), angle.sin()];
            for _ in 0..300 {
                step(&world, &mut body, wish, 1.0, false, 1.0 / 30.0);
            }
            let c = body.center;
            let reach = c + Vector::new(wish[0] * (RADIUS - 2.0), wish[1] * (RADIUS - 2.0), 0.0);
            assert!(line_check(model, c, reach).is_none(), "too close to a wall at {angle}: {body:?}");
        }
    }
}
