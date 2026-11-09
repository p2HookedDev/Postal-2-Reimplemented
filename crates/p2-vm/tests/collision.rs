mod common;

use common::{add, game_dir, load, scale, start_single_player, vector};

use p2_assets::{read_model, read_static_mesh};
use p2_collision::{MeshCollision, box_overlaps_triangle, model_triangles, static_mesh_collision};
use p2_package::Loader;
use p2_vm::{Encroachment, ObjectId, Value, Vm};

const STEP: f32 = 4.0;

fn flag(vm: &Vm<&Loader>, object: ObjectId, name: &str) -> bool {
    vm.get(object, name) == Some(Value::Bool(true))
}

fn float(vm: &Vm<&Loader>, object: ObjectId, name: &str) -> f32 {
    match vm.get(object, name) {
        Some(Value::Float(v)) => v,
        _ => 0.0,
    }
}

fn object(vm: &Vm<&Loader>, object: ObjectId, name: &str) -> Option<ObjectId> {
    match vm.get(object, name) {
        Some(Value::Object(found)) => found,
        _ => None,
    }
}

fn rotator(vm: &Vm<&Loader>, object: ObjectId, name: &str) -> [i32; 3] {
    match vm.get(object, name) {
        Some(Value::Rotator(r)) => <[i32; 3]>::from(r),
        _ => [0; 3],
    }
}

enum Shape {
    Triangles(MeshCollision),
    Cylinder(f32, f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Kind {
    WorldMesh,
    WorldMeshModel,
    Volume,
    Mover,
    OtherMesh,
    Cylinder,
}

struct Blocker {
    actor: ObjectId,
    kind: Kind,
    shape: Shape,
    min: [f32; 3],
    max: [f32; 3],
}

fn model_local(vm: &Vm<&Loader>, loader: &Loader, model: ObjectId) -> Option<Vec<[[f32; 3]; 3]>> {
    let (package, export) = loader.loaded_export_by_path_of_class(&vm.object(model).path, "Model")?;
    let source = loader.package(package);
    read_model(&source.package, &source.data, &source.package.exports[export]).ok().map(|m| model_triangles(&m))
}

fn reference_shape(vm: &Vm<&Loader>, loader: &Loader, actor: ObjectId) -> (Shape, bool) {
    let cylinder = Shape::Cylinder(float(vm, actor, "CollisionRadius"), float(vm, actor, "CollisionHeight"));
    let scale = vector(vm, actor, "DrawScale3D").map(|s| s * float(vm, actor, "DrawScale"));
    let place = |local| MeshCollision::placed(local, vector(vm, actor, "Location").into(), rotator(vm, actor, "Rotation").into(), scale.into(), vector(vm, actor, "PrePivot").into());
    if vm.is_a(actor, "Brush") {
        return match object(vm, actor, "Brush").and_then(|b| model_local(vm, loader, b)) {
            Some(local) => (Shape::Triangles(place(local)), false),
            None => (cylinder, false),
        };
    }
    if object(vm, actor, "Mesh").is_some() || flag(vm, actor, "bUseCylinderCollision") {
        return (cylinder, false);
    }
    let Some(mesh) = object(vm, actor, "StaticMesh") else { return (cylinder, false) };
    let Some((package, export)) = loader.loaded_export_by_path_of_class(&vm.object(mesh).path, "StaticMesh") else { return (cylinder, false) };
    let source = loader.package(package);
    let Ok(read) = read_static_mesh(&source.package, &source.data, &source.package.exports[export]) else { return (cylinder, false) };
    let model = loader.resolve_loaded(package, read.collision_model).and_then(|found| {
        let source = loader.package(found.package);
        read_model(&source.package, &source.data, &source.package.exports[found.export?]).ok()
    });
    let simple = model.is_some() && read.use_simple_box_collision;
    (Shape::Triangles(place(static_mesh_collision(&read, model.as_ref(), false).0)), simple)
}

fn is_encroacher(vm: &Vm<&Loader>, actor: ObjectId) -> bool {
    let physics = match vm.get(actor, "Physics") {
        Some(Value::Byte(p)) => p,
        _ => 0,
    };
    (vm.is_a(actor, "Mover") && object(vm, actor, "StaticMesh").is_some()) || physics == 14 || (physics == 13 && !vm.is_a(actor, "KConstraint"))
}

fn blockers(vm: &Vm<&Loader>, loader: &Loader, pawn: ObjectId) -> (Vec<Blocker>, usize) {
    let mut found = Vec::new();
    let mut class_blockers = 0;
    let actors: Vec<ObjectId> = vm.actors().filter(|&a| a != pawn && !vm.is_deleted(a) && !vm.is_a(a, "Pawn")).collect();
    for actor in actors {
        if !blocks_player(vm, pawn, actor) {
            continue;
        }
        if vm.is_a(actor, "BlockingVolume") && flag(vm, actor, "bClassBlocker") {
            class_blockers += 1;
            continue;
        }
        let Some(blocker) = blocker_of(vm, loader, actor) else { continue };
        found.push(blocker);
    }
    (found, class_blockers)
}

fn blocks_player(vm: &Vm<&Loader>, pawn: ObjectId, actor: ObjectId) -> bool {
    if vm.is_deleted(actor) || !flag(vm, actor, "bCollideActors") || !flag(vm, actor, "bBlockNonZeroExtentTraces") {
        return false;
    }
    let brush = vm.is_a(actor, "Brush") && object(vm, actor, "Brush").is_some();
    if flag(vm, actor, "bWorldGeometry") {
        flag(vm, pawn, "bCollideWorld")
    } else if brush || is_encroacher(vm, actor) {
        flag(vm, pawn, "bCollideWorld") && flag(vm, actor, "bBlockPlayers")
    } else {
        flag(vm, actor, "bBlockPlayers") && flag(vm, pawn, "bBlockActors")
    }
}

fn blocker_of(vm: &Vm<&Loader>, loader: &Loader, actor: ObjectId) -> Option<Blocker> {
    let brush = vm.is_a(actor, "Brush") && object(vm, actor, "Brush").is_some();
    let (shape, simple) = reference_shape(vm, loader, actor);
    let (min, max) = match &shape {
        Shape::Triangles(mesh) => mesh.bounds(),
        &Shape::Cylinder(r, h) => {
            let at = vector(vm, actor, "Location");
            ([at[0] - r, at[1] - r, at[2] - h].into(), [at[0] + r, at[1] + r, at[2] + h].into())
        }
    };
    if (0..3).any(|i| min[i] > max[i]) || matches!(shape, Shape::Cylinder(r, h) if r <= 0.0 || h <= 0.0) {
        return None;
    }
    let kind = match (&shape, brush) {
        (_, true) => Kind::Volume,
        (Shape::Cylinder(..), _) => Kind::Cylinder,
        _ if is_encroacher(vm, actor) => Kind::Mover,
        _ if flag(vm, actor, "bWorldGeometry") && simple => Kind::WorldMeshModel,
        _ if flag(vm, actor, "bWorldGeometry") => Kind::WorldMesh,
        _ => Kind::OtherMesh,
    };
    Some(Blocker { actor, kind, shape, min: min.into(), max: max.into() })
}

fn overlaps(blocker: &Blocker, triangles: &[[[f32; 3]; 3]], at: [f32; 3], extent: [f32; 3], location: [f32; 3]) -> bool {
    match blocker.shape {
        Shape::Triangles(_) => triangles.iter().any(|&t| box_overlaps_triangle(at.into(), extent.into(), t)),
        Shape::Cylinder(r, h) => (at[0] - location[0]).hypot(at[1] - location[1]) < r + extent[0] && (at[2] - location[2]).abs() < h + extent[2],
    }
}

fn near_triangles(blocker: &Blocker, start: [f32; 3], end: [f32; 3], extent: [f32; 3]) -> Vec<[[f32; 3]; 3]> {
    let Shape::Triangles(mesh) = &blocker.shape else { return Vec::new() };
    let low = [0, 1, 2].map(|i| start[i].min(end[i]) - extent[i]);
    let high = [0, 1, 2].map(|i| start[i].max(end[i]) + extent[i]);
    let inside = |t: &[[f32; 3]; 3]| (0..3).all(|i| t.iter().any(|p| p[i] >= low[i]) && t.iter().any(|p| p[i] <= high[i]));
    mesh.triangles().iter().filter(|t| inside(t)).copied().collect()
}

fn first_overlap(blocker: &Blocker, start: [f32; 3], d: [f32; 3], length: f32, extent: [f32; 3], location: [f32; 3]) -> Option<f32> {
    let triangles = near_triangles(blocker, start, add(start, scale(d, length)), extent);
    if matches!(blocker.shape, Shape::Triangles(_)) && triangles.is_empty() {
        return None;
    }
    let steps = (length / STEP) as usize;
    let hit = (0..=steps).find(|&i| overlaps(blocker, &triangles, add(start, scale(d, i as f32 * STEP)), extent, location))?;
    (hit > 0).then_some(hit as f32 * STEP)
}

struct Case {
    blocker: usize,
    start: [f32; 3],
    d: [f32; 3],
    reach: f32,
}

#[derive(Clone, Copy)]
struct Player {
    controller: ObjectId,
    pawn: ObjectId,
    radius: f32,
    height: f32,
}

struct SweepSurvey {
    blocking: usize,
    through: usize,
}

struct WalkSurvey {
    walks: usize,
    entered: usize,
}

fn blocking_differences(vm: &Vm<&Loader>, loader: &Loader, map: &str) {
    let mut differences = std::collections::BTreeMap::<&str, usize>::new();
    for actor in vm.actors().filter(|&a| !vm.is_a(a, "Pawn")).collect::<Vec<_>>() {
        let static_mesh = object(vm, actor, "StaticMesh").is_some() && object(vm, actor, "Mesh").is_none() && !vm.is_a(actor, "Brush");
        let colliding = flag(vm, actor, "bCollideActors") && flag(vm, actor, "bBlockNonZeroExtentTraces");
        if static_mesh && colliding && flag(vm, actor, "bWorldGeometry") && !flag(vm, actor, "bBlockActors") {
            *differences.entry("world mesh without bBlockActors (blocks)").or_default() += 1;
        }
        if static_mesh && colliding && !flag(vm, actor, "bWorldGeometry") && flag(vm, actor, "bStatic") && flag(vm, actor, "bBlockActors") && !flag(vm, actor, "bBlockPlayers") {
            *differences.entry("static non-world mesh, bBlockActors but not bBlockPlayers (lets players through)").or_default() += 1;
        }
        if static_mesh && flag(vm, actor, "bCollideActors") && !flag(vm, actor, "bBlockNonZeroExtentTraces") {
            *differences.entry("mesh without bBlockNonZeroExtentTraces (no box collision)").or_default() += 1;
            if flag(vm, actor, "bBlockActors") && flag(vm, actor, "bStatic") {
                *differences.entry("  of them bStatic, bBlockActors (the VM's old rule blocked with the drawn mesh)").or_default() += 1;
            }
        }
        if static_mesh && colliding && flag(vm, actor, "bUseCylinderCollision") {
            *differences.entry("mesh with bUseCylinderCollision").or_default() += 1;
        }
        if vm.is_a(actor, "BlockingVolume") {
            *differences.entry("BlockingVolume").or_default() += 1;
        }
        if static_mesh && colliding && let (Shape::Triangles(mesh), simple) = reference_shape(vm, loader, actor) {
            if mesh.triangles().is_empty() {
                *differences.entry("mesh without collision triangles or model (collides with nothing)").or_default() += 1;
            } else if simple {
                *differences.entry("mesh using its collision model for boxes").or_default() += 1;
            }
        }
    }
    eprintln!("{map}: differences {differences:#?}");
}

fn sweep_survey(map: &str, vm: &mut Vm<&Loader>, player: Player, blockers: &[Blocker]) -> (Vec<Case>, SweepSurvey) {
    let Player { pawn, radius, height, .. } = player;
    let extent = [radius, radius, height];
    let mut cases = Vec::new();
    let mut passed = std::collections::BTreeMap::<Kind, Vec<(ObjectId, f32, Option<f32>)>>::new();
    let mut skipped_large = 0;
    for (index, blocker) in blockers.iter().enumerate() {
        let (min, max) = (blocker.min, blocker.max);
        if max[0] - min[0] > 4000.0 || max[1] - min[1] > 4000.0 {
            skipped_large += 1;
            continue;
        }
        let centre = [0, 1, 2].map(|i| (min[i] + max[i]) / 2.0);
        let location = vector(vm, blocker.actor, "Location");
        for d in [[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -1.0, 0.0]] {
            let axis = if d[0] != 0.0 { 0 } else { 1 };
            let half = (max[axis] - min[axis]) / 2.0 + radius + 16.0;
            for lift in [2.0, 42.0] {
                let z = min[2] + height + lift;
                if z - height > max[2] {
                    continue;
                }
                let start = [centre[0] - d[0] * half, centre[1] - d[1] * half, z];
                let Some(reach) = first_overlap(blocker, start, d, 2.0 * half, extent, location) else { continue };
                let check = vm.move_check(pawn, start.into(), scale(d, 2.0 * half).into());
                let made = check.map(|(time, _, _)| time * 2.0 * half);
                if made.is_none_or(|made| made > reach + STEP + 2.0) {
                    passed.entry(blocker.kind).or_default().push((blocker.actor, reach, made));
                }
                cases.push(Case { blocker: index, start, d, reach });
            }
        }
    }
    let through: usize = passed.values().map(Vec::len).sum();
    eprintln!("{map}: {} blocking sweeps ({skipped_large} blockers over 4000 units skipped), {through} passed through", cases.len());
    for (kind, list) in &passed {
        let actors: std::collections::BTreeSet<ObjectId> = list.iter().map(|p| p.0).collect();
        let examples: Vec<String> = list.iter().take(4).map(|(a, reach, made)| format!("{} (at {reach}, made {made:?})", vm.object(*a).path)).collect();
        eprintln!("  {kind:?}: {} sweeps, {} actors; {examples:?}", list.len(), actors.len());
    }
    let survey = SweepSurvey { blocking: cases.len(), through };
    (cases, survey)
}

fn walk_survey(map: &str, vm: &mut Vm<&Loader>, loader: &Loader, player: Player, blockers: &[Blocker], cases: &[Case]) -> WalkSurvey {
    let Player { controller, pawn, radius, height } = player;
    let mut walks = 0;
    let mut entered = Vec::new();
    let mut excused = std::collections::BTreeMap::<&str, usize>::new();
    let mut stopped = 0;
    let wall_extent = [radius, radius, height - (18.0 + 1.0)];
    for case in cases.iter().step_by((cases.len() / 120).max(1)) {
        let blocker = &blockers[case.blocker];
        let location = vector(vm, blocker.actor, "Location");
        let top = [case.start[0], case.start[1], case.start[2] + 100.0];
        let Some(floor) = vm.trace_world(top.into(), [top[0], top[1], top[2] - 600.0].into(), [0.0; 3].into()) else { continue };
        if floor.normal[2] < 0.99 || floor.time <= 0.0 {
            continue;
        }
        let at = [case.start[0], case.start[1], floor.location[2] + height + 3.0];
        let raised = [at[0], at[1], at[2] + 18.0 + 1.0];
        let Some(reach) = first_overlap(blocker, raised, case.d, case.reach + 64.0, wall_extent, location) else { continue };
        walks += 1;
        vm.place_far_moved(pawn, at.into(), Encroachment::Ignored).unwrap();
        vm.set(pawn, "Velocity", Value::Vector(p2_math::Vector::ZERO));
        let yaw = (case.d[1].atan2(case.d[0]) * 65536.0 / std::f32::consts::TAU).round() as i32;
        let rotation = Value::Rotator(p2_math::Rotator::new(0, yaw, 0));
        vm.set(controller, "Rotation", rotation.clone());
        vm.set(pawn, "Rotation", rotation);
        let frames = (((reach + 150.0) / 450.0 + 0.5) * 60.0) as usize;
        let shrunk = [radius - 2.0, radius - 2.0, height - 2.0];
        let placement = |vm: &Vm<&Loader>| (vector(vm, blocker.actor, "Location"), rotator(vm, blocker.actor, "Rotation"));
        let placed = placement(vm);
        let mut furthest = 0.0_f32;
        for frame in 0..frames {
            vm.player_input(&["W"], [0.0, 0.0]).unwrap();
            vm.tick_level(1.0 / 60.0);
            let now = vector(vm, pawn, "Location");
            furthest = furthest.max((now[0] - at[0]) * case.d[0] + (now[1] - at[1]) * case.d[1]);
            let moved;
            let current = if flag(vm, blocker.actor, "bStatic") {
                blocker
            } else {
                let Some(fresh) = blocker_of(vm, loader, blocker.actor) else { continue };
                moved = fresh;
                &moved
            };
            let triangles = near_triangles(current, now, now, shrunk);
            if !overlaps(current, &triangles, now, shrunk, vector(vm, blocker.actor, "Location")) {
                continue;
            }
            let why = if !blocks_player(vm, pawn, blocker.actor) {
                "stopped blocking"
            } else if placement(vm) != placed {
                "moved into the pawn"
            } else {
                entered.push(format!("{} {:?} frame {frame} at {now:?} (start {at:?}, wall at {reach})", vm.object(blocker.actor).path, blocker.kind));
                "entered"
            };
            *excused.entry(why).or_default() += 1;
            break;
        }
        stopped += usize::from(furthest < reach + STEP + 4.0);
    }
    eprintln!("{map}: {walks} walks into blockers, {stopped} stopped in front, overlaps {excused:?}: {entered:#?}");
    WalkSurvey { walks, entered: entered.len() }
}

fn survey(map: &str) -> Option<(SweepSurvey, WalkSurvey)> {
    let dir = game_dir()?;
    let (loader, id) = load(&dir, map);
    let (mut vm, controller, pawn) = start_single_player(&dir, &loader, id);
    let pawn = pawn.expect("no pawn");
    let others: Vec<ObjectId> = vm.actors().filter(|&a| a != pawn && vm.is_a(a, "Pawn")).collect();
    for other in others {
        vm.set(other, "bCollideActors", Value::Bool(false));
        vm.set(other, "bBlockActors", Value::Bool(false));
    }
    let player = Player { controller, pawn, radius: float(&vm, pawn, "CollisionRadius"), height: float(&vm, pawn, "CollisionHeight") };
    blocking_differences(&vm, &loader, map);

    let (blockers, class_blockers) = blockers(&vm, &loader, pawn);
    let mut kinds = std::collections::BTreeMap::<Kind, usize>::new();
    for b in &blockers {
        *kinds.entry(b.kind).or_default() += 1;
    }
    eprintln!("{map}: {} blockers {kinds:?}, {class_blockers} class-blocking volumes skipped", blockers.len());

    let (cases, sweeps) = sweep_survey(map, &mut vm, player, &blockers);
    let walks = walk_survey(map, &mut vm, &loader, player, &blockers, &cases);
    Some((sweeps, walks))
}

#[test]
fn suburbs_blockers_stop_the_player() {
    let Some((SweepSurvey { blocking: sweeps, through }, WalkSurvey { walks, entered })) = survey("Suburbs-3") else { return };
    assert!(sweeps > 1000 && walks > 10, "{sweeps} sweeps, {walks} walks");
    assert!(through * 200 <= sweeps && entered == 0, "{through} of {sweeps} sweeps passed through, {entered} of {walks} walks entered");
}

#[test]
fn church_blockers_stop_the_player() {
    let Some((SweepSurvey { blocking: sweeps, through }, WalkSurvey { walks, entered })) = survey("Church") else { return };
    assert!(sweeps > 200 && walks > 10, "{sweeps} sweeps, {walks} walks");
    assert!(through * 200 <= sweeps && entered == 0, "{through} of {sweeps} sweeps passed through, {entered} of {walks} walks entered");
}
