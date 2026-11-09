mod common;

use common::game_dir;

use bevy::math::{Mat4, Vec3};
use p2_assets::{SkeletalMesh, read_skeletal_mesh};
use p2_bevy_assets::{bone_matrices, rotator_matrix, to_bevy, unreal_mesh_matrix, unreal_mesh_transform};
use p2_engine::actors::skeletal_actor;
use p2_engine::animation::attachment_frame;
use p2_package::Loader;
use p2_vm::Vm;

#[test]
fn church_pawns_stand_on_floors() {
    let Some(dir) = game_dir() else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    let map = loader.load("Church").unwrap();
    let mut actors = Vec::new();
    {
        let mut vm = Vm::new(&loader);
        for index in 0..loader.package(map).package.exports.len() {
            if let Ok(object) = vm.load_object(map, index)
                && let Some(actor) = skeletal_actor(&vm, object)
            {
                let Some(p2_vm::Value::Float(height)) = vm.store.get(object, "CollisionHeight") else { continue };
                actors.push((actor, height));
            }
        }
    }
    let (mut checked, mut on_floor) = (0, 0);
    for (actor, collision_height) in &actors {
        let Ok(Some((package, export))) = loader.export_by_path_of_class(&actor.mesh, "SkeletalMesh") else { continue };
        let loaded = loader.package(package);
        let Ok(mesh) = read_skeletal_mesh(&loaded.package, &loaded.data, &loaded.package.exports[export]) else { continue };
        let transform =
            unreal_mesh_transform(actor.location, actor.rotation, actor.scale, actor.pre_pivot, mesh.scale, mesh.origin, mesh.rot_origin);
        let heights: Vec<f32> = mesh.positions.iter().map(|&p| transform.transform_point(Vec3::from(to_bevy(p))).y).collect();
        let low = heights.iter().copied().fold(f32::MAX, f32::min);
        checked += 1;
        let bottom = actor.location[2] - collision_height;
        let ok = (low - bottom).abs() < 15.0;
        on_floor += usize::from(ok);
    }
    assert!(checked > 50 && on_floor * 10 >= checked * 9, "{on_floor} of {checked}");
}

#[test]
fn church_heads_sit_on_bodies() {
    let Some(dir) = game_dir() else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    let map = loader.load("Church").unwrap();
    let mut actors = Vec::new();
    {
        let mut vm = Vm::new(&loader);
        let game = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
        assert!(vm.begin_play(map, game, "").unwrap().is_empty());
        for object in vm.object_ids() {
            if let Some(actor) = skeletal_actor(&vm, object) {
                actors.push(actor);
            }
        }
    }
    let mut mesh = |path: &str| -> Option<(SkeletalMesh, Vec<String>)> {
        let (package, export) = loader.export_by_path_of_class(path, "SkeletalMesh").ok()??;
        let loaded = loader.package(package);
        let mesh = read_skeletal_mesh(&loaded.package, &loaded.data, &loaded.package.exports[export]).ok()?;
        let names = mesh.bones.iter().map(|b| loaded.package.name(b.name).to_owned()).collect();
        Some((mesh, names))
    };
    let (mut heads, mut seated) = (0, 0);
    for head in actors.iter().filter(|a| a.attached_to.is_some()) {
        let Some((base, bone)) = &head.attached_to else { continue };
        let Some(body) = actors.iter().find(|a| a.object == *base) else { continue };
        let (Some((body_mesh, bones)), Some((head_mesh, _))) = (mesh(&body.mesh), mesh(&head.mesh)) else { continue };
        let parents: Vec<usize> = body_mesh.bones.iter().map(|b| b.parent).collect();
        let local: Vec<_> = body_mesh.bones.iter().map(|b| (b.orientation, b.position)).collect();
        let pose = bone_matrices(&parents, &local);
        let Some(bone) = bones.iter().position(|n| n.eq_ignore_ascii_case(bone)) else { continue };
        let body_matrix = unreal_mesh_matrix(
            body.location,
            rotator_matrix(body.rotation),
            body.scale,
            body.pre_pivot,
            body_mesh.scale,
            body_mesh.origin,
            body_mesh.rot_origin,
        );
        let frame = attachment_frame(body_matrix, pose[bone], head.relative_location, head.relative_rotation);
        let (_, rotation, position) = frame.to_scale_rotation_translation();
        let head_matrix = unreal_mesh_matrix(
            position.to_array().into(),
            Mat4::from_quat(rotation),
            head.scale,
            head.pre_pivot,
            head_mesh.scale,
            head_mesh.origin,
            head_mesh.rot_origin,
        );
        let z = |m: Mat4, points: &[[f32; 3]]| -> (f32, f32) {
            points.iter().map(|&p| m.transform_point3(Vec3::from(p)).z).fold((f32::MAX, f32::MIN), |(lo, hi), z| (lo.min(z), hi.max(z)))
        };
        let (_, body_top) = z(body_matrix, &body_mesh.positions);
        let (head_low, head_high) = z(head_matrix, &head_mesh.positions);
        heads += 1;
        let ok = (head_low - body_top).abs() < 30.0 && (10.0..60.0).contains(&(head_high - head_low));
        seated += usize::from(ok);
    }
    assert!(heads > 50 && seated * 10 >= heads * 9, "{seated} of {heads}");
}
