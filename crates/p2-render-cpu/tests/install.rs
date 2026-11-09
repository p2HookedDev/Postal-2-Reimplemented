use std::path::PathBuf;

use p2_assets::{read_mesh_actor, read_static_mesh, read_static_mesh_instance};
use p2_package::{LoadedPackage, Loader, Package};
use p2_render_cpu::{MeshLight, MeshPlacement, needs_relight, static_mesh_lighting};
use p2_script::{Objects, Source};

fn game_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("P2_GAME_DIR").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("P2_GAME_DIR unset, skipping");
    }
    dir
}

fn saved_light(pkg: &Package, data: &[u8], reference: i32) -> Option<MeshLight> {
    use p2_package::TagValue;
    let export = pkg.exports.get(usize::try_from(reference - 1).ok()?)?;
    if pkg.object_name(export.class_index) != Some("Light") {
        return None;
    }
    let object = pkg.read_object(data, export).ok()?;
    let mut light = MeshLight::default();
    for property in &object.properties {
        match (pkg.name(property.name), &property.value) {
            ("Location", TagValue::Struct { data, .. }) => {
                light.location = [0, 4, 8].map(|at| f32::from_le_bytes(data[at..at + 4].try_into().unwrap())).into();
            }
            ("Rotation", TagValue::Struct { data, .. }) => {
                light.rotation = [0, 4, 8].map(|at| i32::from_le_bytes(data[at..at + 4].try_into().unwrap())).into();
            }
            ("LightType", &TagValue::Byte(v)) => light.light_type = p2_render_cpu::LightType(v),
            ("LightEffect", &TagValue::Byte(v)) => light.effect = p2_render_cpu::LightEffect(v),
            ("LightBrightness", &TagValue::Float(v)) => light.brightness = v,
            ("LightHue", &TagValue::Byte(v)) => light.hue = v,
            ("LightSaturation", &TagValue::Byte(v)) => light.saturation = v,
            ("LightRadius", &TagValue::Float(v)) => light.radius = v,
            ("LightCone", &TagValue::Byte(v)) => light.cone = v,
            ("bDynamicLight", &TagValue::Bool(v)) => light.dynamic = v,
            _ => {}
        }
    }
    Some(light)
}

#[test]
fn static_mesh_lighting_matches_saved() {
    let Some(dir) = game_dir() else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    for map_name in ["Suburbs-3", "Church"] {
        let map = loader.load(map_name).unwrap();
        let (mut instances, mut vertices, mut close) = (0, 0, 0);
        let exports = loader.package(map).package.exports.len();
        for index in 0..exports {
            let (mesh, placement, instance, lights, level_brightness) = {
                let LoadedPackage { package: pkg, data, .. } = loader.package(map);
                let export = &pkg.exports[index];
                if export.serial_size == 0 || pkg.object_name(export.class_index) == Some("SoundThing") {
                    continue;
                }
                let Ok(Some(actor)) = read_mesh_actor(&mut Objects::new(&loader), Source { package: pkg, data }, index) else { continue };
                if actor.instance <= 0 {
                    continue;
                }
                let instance = read_static_mesh_instance(pkg, data, &pkg.exports[actor.instance as usize - 1]).unwrap();
                let lights: Vec<_> = instance.lights.iter().map(|l| saved_light(pkg, data, l.actor)).collect();
                if instance.lights.is_empty() || !instance.lights.iter().all(|l| l.applied) || lights.iter().any(|l| l.is_none_or(|l| l.dynamic)) {
                    continue;
                }
                let level = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("LevelInfo")).unwrap();
                let level = pkg.read_object(data, level).unwrap();
                let brightness = match level.property(pkg, "Brightness") {
                    Some(&p2_package::TagValue::Float(b)) => b,
                    _ => 1.0,
                };
                let mesh = (loader.id_of(actor.mesh_package).unwrap(), actor.static_mesh);
                let placement =
                    MeshPlacement { location: actor.location, rotation: actor.rotation, scale: actor.scale, pre_pivot: actor.pre_pivot };
                (mesh, placement, instance, lights, brightness)
            };
            let Some(resolved) = loader.resolve(mesh.0, mesh.1).ok().flatten() else { continue };
            let LoadedPackage { package: pkg, data, .. } = loader.package(resolved.package);
            let mesh = read_static_mesh(pkg, data, &pkg.exports[resolved.export.unwrap()]).unwrap();
            if instance.colors.len() != mesh.positions.len() {
                continue;
            }
            assert!(!needs_relight(&instance, &lights));
            let computed = static_mesh_lighting(&mesh, &instance, &lights, &placement.world_vertices(&mesh), level_brightness);
            instances += 1;
            for (a, b) in computed.iter().zip(&instance.colors) {
                vertices += 1;
                let diff = (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap();
                close += usize::from(diff <= 2);
            }
        }
        assert!(instances > 0);
        assert!(close as f64 >= vertices as f64 * 0.95, "{map_name}: {close} of {vertices}");
    }
}