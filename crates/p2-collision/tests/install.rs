use std::path::PathBuf;

use p2_assets::{
    read_actor_location, read_level, read_mesh_actor, read_model, read_static_mesh, read_terrain, read_texture, texture_heights,
};
use p2_collision::{
    MeshCollision, Sides, box_check, line_check, mesh_line_check, point_check, point_zone, terrain_line_check,
};
use p2_math::Vector;
use p2_package::{LoadedPackage, Loader, Package, Resolved};
use p2_script::{Layouts, Objects, Source};

fn game_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("P2_GAME_DIR").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("P2_GAME_DIR unset, skipping");
    }
    dir
}

/// Maps whose player starts the level BSP puts in solid space (open question).
const SOLID_START_MAPS: [&str; 4] = ["MPS-Ponderous", "ngt-underhub", "underhub", "VincesHouse"];

fn script_loader(dir: &std::path::Path) -> Loader {
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    loader
}

#[test]
fn player_starts_stand_on_floors() {
    let Some(dir) = game_dir() else { return };
    let mut floors = 0;
    let mut failures = Vec::new();
    let loader = script_loader(&dir);
    for entry in std::fs::read_dir(dir.join("Maps")).unwrap() {
        let path = entry.unwrap().path();
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("fuk")) {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let data = std::fs::read(&path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        let Some(level) = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("Level")) else {
            continue;
        };
        let level = read_level(&pkg, &data, level).unwrap();
        let model = read_model(&pkg, &data, &pkg.exports[level.model as usize - 1]).unwrap();
        assert!(point_check(&model, Vector::new(1e6, 1e6, 1e6), Vector::ZERO).is_some(), "{name}");
        let mut objects = Objects::new(&loader);
        for index in (0..pkg.exports.len()).filter(|&i| pkg.object_name(pkg.exports[i].class_index) == Some("PlayerStart")) {
            let start = read_actor_location(&mut objects, Source { package: &pkg, data: &data }, index).unwrap();
            if usize::from(point_zone(&model, start)) >= model.zones.len().max(1) {
                failures.push(format!("{name}: start {start:?} in zone outside the zone list"));
            }
            if point_check(&model, start, Vector::ZERO).is_some() {
                if !SOLID_START_MAPS.contains(&name.as_str()) {
                    failures.push(format!("{name}: start {start:?} in solid space"));
                }
                continue;
            }
            let below = start - Vector::new(0.0, 0.0, 100_000.0);
            let Some(hit) = line_check(&model, start, below) else {
                failures.push(format!("{name}: nothing below {start:?}"));
                continue;
            };
            let inside = Vector::new(start.x, start.y, hit.location.z - 1.0);
            if point_check(&model, inside, Vector::ZERO).is_none() {
                failures.push(format!("{name}: empty below the hit under {start:?}"));
            }
            if hit.time * 100_000.0 < 300.0 && hit.normal.z > 0.7 {
                floors += 1;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(floors >= 180, "{floors} floors");
}

fn maps_with_starts(dir: &std::path::Path) -> Vec<(String, p2_assets::Model, Vec<Vector>)> {
    let mut maps = Vec::new();
    let loader = script_loader(dir);
    for entry in std::fs::read_dir(dir.join("Maps")).unwrap() {
        let path = entry.unwrap().path();
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("fuk")) {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let data = std::fs::read(&path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        let Some(level) = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("Level")) else {
            continue;
        };
        let level = read_level(&pkg, &data, level).unwrap();
        let model = read_model(&pkg, &data, &pkg.exports[level.model as usize - 1]).unwrap();
        let mut objects = Objects::new(&loader);
        let starts = (0..pkg.exports.len())
            .filter(|&i| pkg.object_name(pkg.exports[i].class_index) == Some("PlayerStart"))
            .map(|i| read_actor_location(&mut objects, Source { package: &pkg, data: &data }, i).unwrap())
            .collect();
        maps.push((name, model, starts));
    }
    maps
}

#[test]
fn tiny_box_matches_line_check() {
    let Some(dir) = game_dir() else { return };
    let directions: [Vector; 9] = [
        Vector::new(0.0, 0.0, -1.0),
        Vector::new(1.0, 0.0, 0.0),
        Vector::new(-1.0, 0.0, 0.0),
        Vector::new(0.0, 1.0, 0.0),
        Vector::new(0.0, -1.0, 0.0),
        Vector::new(0.6, 0.8, 0.0),
        Vector::new(-0.8, 0.6, 0.0),
        Vector::new(0.48, -0.64, -0.6),
        Vector::new(-0.64, -0.48, 0.6),
    ];
    let (mut hits, mut mismatches) = (0, Vec::new());
    for (name, model, starts) in maps_with_starts(&dir) {
        for &start in starts.iter().filter(|&&s| point_check(&model, s, Vector::ZERO).is_none()) {
            for d in directions {
                let end = start + d * 3000.0;
                let line = line_check(&model, start, end);
                let boxed = box_check(&model, start, end, Vector::new(0.5, 0.5, 0.5));
                match (line, boxed) {
                    (Some(line), Some(boxed)) => {
                        hits += 1;
                        let gap = (line.time - boxed.time) * 3000.0;
                        if !(-0.5..=8.0).contains(&gap) {
                            mismatches.push(format!("{name} {start:?} {d:?}: gap {gap}"));
                        }
                    }
                    (None, None) => {}
                    (line, boxed) => mismatches.push(format!("{name} {start:?} {d:?}: line {line:?}, box {boxed:?}")),
                }
            }
        }
    }
    assert!(hits > 1000, "{hits}");
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Starts sit about 84 units above their floor, not at pawn centre height.
#[test]
fn pawn_box_lands_and_stops_at_walls() {
    let Some(dir) = game_dir() else { return };
    const PAWN: Vector = Vector::new(25.0, 25.0, 44.0);
    let (mut landed, mut overlapping, mut stopped) = (0, 0, 0);
    let mut failures = Vec::new();
    for (name, model, starts) in maps_with_starts(&dir) {
        for &start in &starts {
            if point_check(&model, start, Vector::ZERO).is_some() {
                assert!(SOLID_START_MAPS.contains(&name.as_str()), "{name}");
                continue;
            }
            if point_check(&model, start, PAWN).is_some() {
                overlapping += 1;
            }
            let below = start - Vector::new(0.0, 0.0, 2000.0);
            if let Some(floor) = line_check(&model, start, below)
                && floor.normal.z > 0.999
                && floor.time * 2000.0 < 300.0
            {
                let hit = box_check(&model, start, below, PAWN).expect("box lands");
                let (drop, expected) = (hit.time * 2000.0, floor.time * 2000.0 - PAWN.z - 4.0);
                if (drop - expected).abs() < 0.5 && hit.normal.z > 0.999 {
                    landed += 1;
                } else if drop > expected + 0.5 || hit.normal.z <= 0.0 {
                    failures.push(format!("{name} {start:?}: dropped {drop}, expected {expected}, {hit:?}"));
                }
            }
            for d in [Vector::new(1.0, 0.0, 0.0), Vector::new(-1.0, 0.0, 0.0), Vector::new(0.0, 1.0, 0.0), Vector::new(0.0, -1.0, 0.0)] {
                let end = start + d * 3000.0;
                let Some(line) = line_check(&model, start, end) else { continue };
                let distance = line.time * 3000.0;
                if line.normal.z.abs() > 0.01 || !(100.0..3000.0).contains(&distance) {
                    continue;
                }
                match box_check(&model, start, end, PAWN) {
                    Some(hit) if hit.time * 3000.0 <= distance - 25.0 => stopped += 1,
                    hit => failures.push(format!("{name} {start:?} {d:?}: wall at {distance}, box {hit:?}")),
                }
            }
        }
    }
    assert!(landed >= 170, "{landed}");
    assert!(stopped >= 500, "{stopped}");
    assert!(overlapping == 0);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn church_start_stands_on_terrain() {
    let Some(dir) = game_dir() else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    let map = loader.load("Church").unwrap();
    let LoadedPackage { package: pkg, data, .. } = loader.package(map);
    let mut layouts = Layouts::new(&loader);
    let source = Source { package: pkg, data };
    let mut terrains = Vec::new();
    for (index, export) in pkg.exports.iter().enumerate() {
        if pkg.object_name(export.class_index) == Some("TerrainInfo") {
            let layout = layouts.class_layout(source, index).unwrap();
            let mut terrain = read_terrain(pkg, data, export, &layout).unwrap();
            let heightmap = read_texture(&mut Objects::new(&loader), source, terrain.heightmap as usize - 1).unwrap();
            let (_, _, heights) = texture_heights(&heightmap).unwrap();
            terrain.vertices = terrain.vertices_from_heights(&heights);
            terrains.push(terrain);
        }
    }
    let start = (0..pkg.exports.len())
        .find(|&i| pkg.object_name(pkg.exports[i].class_index) == Some("PlayerStart"))
        .map(|i| read_actor_location(&mut Objects::new(&loader), source, i).unwrap())
        .unwrap();
    let below = start - Vector::new(0.0, 0.0, 2000.0);
    let hit = terrains.iter().find_map(|terrain| terrain_line_check(terrain, start, below, Sides::Both)).expect("terrain below");
    let distance = hit.time * 2000.0;
    assert!(distance > 0.0 && distance < 300.0);
    assert!(hit.normal.z > 0.7);
}
#[test]
fn church_static_meshes_collide() {
    let Some(dir) = game_dir() else { return };
    let mut loader = script_loader(&dir);
    let map = loader.load("Church").unwrap();
    let mut hits = 0;
    let actors: Vec<_> = {
        let LoadedPackage { package: pkg, data, .. } = loader.package(map);
        let mut objects = Objects::new(&loader);
        (0..pkg.exports.len())
            .filter_map(|i| read_mesh_actor(&mut objects, Source { package: pkg, data }, i).ok().flatten())
            .map(|a| (loader.id_of(a.mesh_package).unwrap(), a.static_mesh, (a.location, a.rotation, a.scale, a.pre_pivot)))
            .collect()
    };
    for &(mesh_package, static_mesh, (location, rotation, scale, pre_pivot)) in actors.iter().take(200) {
        let Some(Resolved { package, export: Some(export) }) = loader.resolve(mesh_package, static_mesh).ok().flatten() else { continue };
        let loaded = loader.package(package);
        let Ok(mesh) = read_static_mesh(&loaded.package, &loaded.data, &loaded.package.exports[export]) else { continue };
        let collision = MeshCollision::new(&mesh, location, rotation, scale, pre_pivot);
        let Some(target) = first_flat_triangle_center(&collision) else { continue };
        let above = target + Vector::new(0.0, 0.0, 5000.0);
        let below = target - Vector::new(0.0, 0.0, 1.0);
        let hit = mesh_line_check(std::slice::from_ref(&collision), above, below).expect("hit");
        assert!(hit.location.z >= target.z - 0.5, "{:?} below {target:?}", hit.location);
        hits += 1;
    }
    assert!(hits > 100);
}
fn first_flat_triangle_center(mesh: &MeshCollision) -> Option<Vector> {
    mesh.triangles().iter().find_map(|&[a, b, c]| {
        let (u, v) = ([0, 1, 2].map(|i| b[i] - a[i]), [0, 1, 2].map(|i| c[i] - a[i]));
        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        (length > 1e-3 && (n[2] / length).abs() > 0.5).then(|| [0, 1, 2].map(|i| (a[i] + b[i] + c[i]) / 3.0).into())
    })
}