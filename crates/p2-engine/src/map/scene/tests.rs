use std::path::PathBuf;

use p2_assets::{Blending, MeshActor, read_mesh_actor, read_model, read_static_mesh_instance, read_texture, texture_rgba};
use p2_script::{Objects, Source};
use p2_render_cpu::{MeshLight, instance_fits};
use p2_package::TagValue;

use super::*;
use super::projectors::{ALPHA_TEST_TAG, PROJECTOR_TAG, TAGGED_TAG};
use p2_render::lighting::actor_light::mesh_sphere;

fn mesh_placement(loader: &Loader, actor: &MeshActor) -> ((PackageId, i32), MeshPlacement) {
    let mesh = (loader.id_of(actor.mesh_package).unwrap(), actor.static_mesh);
    (mesh, MeshPlacement { location: actor.location, rotation: actor.rotation, scale: actor.scale, pre_pivot: actor.pre_pivot })
}

#[test]
fn splat_surface_lies_inside_the_decal_box() {
    let local = projector_box([100.0, 200.0, 50.0].into(), [-16384, 0, 0].into(), 64.0, 32.0).to_matrix().inverse();
    let surface = local.transform_point3(Vec3::new(100.0, 50.0, 200.0));
    assert!(surface.z.abs() < 0.5 - 1.0 / 34.0 + 1e-4, "surface at local z {}", surface.z);
    assert!((local.transform_point3(Vec3::new(100.0, 50.0 - 32.0, 200.0)).z + 0.5).abs() < 1e-4);
    assert!((local.transform_point3(Vec3::new(132.0, 50.0, 200.0)).y.abs() - 0.5).abs() < 1e-4);
}

#[test]
#[ignore = "survey: prints only"]
fn splat_textures_survey() {
    let Some(root) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
    let mut library = Library::new(p2_host::game_dir::config_folder());
    let map = Path::new("Maps").join("Suburbs-3.fuk").display().to_string();
    let (game, _) = library.open(&root, &map).unwrap();
    for (path, blending) in [
        ("nathans.Skins.bloodsplatblend1", 1),
        ("nathans.Skins.bloodsplatblend5", 1),
        ("nathans.Skins.blooddripblend1", 1),
        ("nathans.skins.pistolhitblend", 2),
        ("nathans.skins.shotgunhitblend1", 2),
    ] {
        let Some((package, export)) = game.export_by_path(path) else {
            eprintln!("{path}: not found");
            continue;
        };
        let texture = game.material_texture_rgba(package, export_reference(export)).expect("texture");
        let data = texture.image.data.clone().unwrap_or_default();
        let size = texture.image.texture_descriptor.size;
        let texels = (size.width * size.height) as usize;
        let mean = |data: &[u8], filter: &dyn Fn(&[u8]) -> bool| {
            let picked: Vec<&[u8]> = data[..texels * 4].chunks(4).filter(|t| filter(t)).collect();
            let n = picked.len().max(1) as f32;
            let m = [0, 1, 2, 3].map(|c| picked.iter().map(|t| f32::from(t[c])).sum::<f32>() / n);
            (picked.len(), m.map(|v| v.round()))
        };
        eprintln!(
            "{path}: {:?} {}x{}, state {:?}; texels alpha < 128 {:?}, alpha >= 128 {:?}; all {:?}; projector blending {blending}",
            texture.image.texture_descriptor.format,
            size.width,
            size.height,
            texture.state.blending,
            mean(&data, &|t| t[3] < 128),
            mean(&data, &|t| t[3] >= 128),
            mean(&data, &|_| true),
        );
    }
}

fn saved_light(pkg: &Package, data: &[u8], reference: i32) -> Option<MeshLight> {
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
            ("LightType", &TagValue::Byte(v)) => light.light_type = p2_render_cpu::LightType::from_byte(v),
            ("LightPeriod", &TagValue::Byte(v)) => light.period = v,
            ("LightPhase", &TagValue::Byte(v)) => light.phase = v,
            ("LightEffect", &TagValue::Byte(v)) => light.effect = p2_render_cpu::LightEffect::from_byte(v),
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

fn saved_ambient(pkg: &Package, data: &[u8], export: &p2_package::Export) -> [f32; 3] {
    let object = pkg.read_object(data, export).unwrap();
    let byte = |name, default| match object.property(pkg, name) {
        Some(&TagValue::Byte(v)) => v,
        _ => default,
    };
    p2_render_cpu::hsv_color(byte("AmbientHue", 0), byte("AmbientSaturation", 255), byte("AmbientBrightness", 0))
}

#[test]
fn suburbs3_static_meshes_not_black() {
    let Some(dir) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    let map = loader.load("Suburbs-3").unwrap();
    let (environment, model) = {
        let LoadedPackage { package: pkg, data, .. } = loader.package(map);
        let level = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("Level")).unwrap();
        let level = read_level(pkg, data, level).unwrap();
        let model = read_model(pkg, data, &pkg.exports[level.model as usize - 1]).unwrap();
        let info = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("LevelInfo")).unwrap();
        let zone_ambients = model
            .zones
            .iter()
            .map(|zone| saved_ambient(pkg, data, pkg.exports.get(usize::try_from(zone.actor - 1).ok().unwrap_or(usize::MAX)).unwrap_or(info)))
            .collect();
        let lights = (1..=pkg.exports.len() as i32).filter_map(|r| saved_light(pkg, data, r).map(|l| (r, l))).collect();
        (LightEnvironment::new(lights, zone_ambients, 1.0), model)
    };
    let (mut lit, mut relit, mut black_before, mut black_after, mut dark_zone) = (0, 0, 0, 0, 0);
    let mean = |colors: &[[u8; 4]], scale: f32| {
        colors.iter().map(|c| c[..3].iter().map(|&b| f32::from(b) * scale).sum::<f32>() / 3.0).sum::<f32>() / colors.len().max(1) as f32
    };
    let exports = loader.package(map).package.exports.len();
    for index in 0..exports {
        let (mesh, placement, instance) = {
            let LoadedPackage { package: pkg, data, .. } = loader.package(map);
            let export = &pkg.exports[index];
            if export.serial_size == 0 || pkg.object_name(export.class_index) == Some("SoundThing") {
                continue;
            }
            let Ok(Some(actor)) = read_mesh_actor(&mut Objects::new(&loader), Source { package: pkg, data }, index) else { continue };
            if actor.instance <= 0 || actor.unlit {
                continue;
            }
            let instance = read_static_mesh_instance(pkg, data, &pkg.exports[actor.instance as usize - 1]).unwrap();
            let (mesh, placement) = mesh_placement(&loader, &actor);
            (mesh, placement, instance)
        };
        let Some(resolved) = loader.resolve(mesh.0, mesh.1).ok().flatten() else { continue };
        let LoadedPackage { package: pkg, data, .. } = loader.package(resolved.package);
        let mesh = read_static_mesh(pkg, data, &pkg.exports[resolved.export.unwrap()]).unwrap();
        let lighting = MeshLighting { placement, zone: p2_collision::point_zone(&model, placement.location), glow: 0 };
        if instance.colors.len() != mesh.positions.len() {
            continue;
        }
        let colors = environment.mesh_colors(&model, &mesh, mesh_sphere(&mesh), Some(&instance), &lighting);
        lit += 1;
        let lights: Vec<_> = instance.lights.iter().map(|l| environment.lights.get(&l.actor).copied()).collect();
        relit += usize::from(p2_render_cpu::needs_relight(&instance, &lights));
        black_before += usize::from(mean(&instance.colors, 1.0) < 32.0);
        if mean(&colors, 2.0) < 32.0 {
            black_after += 1;
            let zone = environment.zone_ambients.get(usize::from(lighting.zone)).copied().unwrap_or_default();
            dark_zone += usize::from(zone.iter().all(|&c| c * 255.0 < 1.0));
        }
    }
    eprintln!(
        "{lit} lit actors, {relit} relit; near black: {black_before} before, {black_after} after ({dark_zone} in zones without ambient)"
    );
    assert!(lit > 1000 && relit > 50, "{lit} {relit}");
    assert!(black_after * 5 < black_before, "{black_after} of {black_before}");
    assert_eq!(dark_zone, black_after, "near-black actors in a zone with ambient light");
}

#[test]
fn suburbs3_brightness_balance() {
    let Some(dir) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    let map = loader.load("Suburbs-3").unwrap();
    let shown = |c: &[u8]| c[..3].iter().map(|&b| (2.0 * f64::from(b)).min(255.0)).sum::<f64>() / 3.0;
    let mean_shown = |colors: &[[u8; 4]]| colors.iter().map(|c| shown(c)).sum::<f64>() / colors.len().max(1) as f64;
    let (model, environment, exports, bsp, terrain) = {
        let LoadedPackage { package: pkg, data, .. } = loader.package(map);
        let level = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("Level")).unwrap();
        let level = read_level(pkg, data, level).unwrap();
        let model = read_model(pkg, data, &pkg.exports[level.model as usize - 1]).unwrap();
        let atlases: Vec<_> = model.light_map_textures.iter().map(|a| a.texture().and_then(|t| texture_rgba(&t, None))).collect();
        let mut texels = Vec::new();
        for polygon in model.polygons() {
            let Some((atlas, uvs)) = model.light_map_uvs(&polygon) else { continue };
            let Some(Some((w, h, atlas))) = atlases.get(atlas) else { continue };
            let (w, h) = (*w as usize, *h as usize);
            texels.extend(uvs.iter().map(|&[u, v]| atlas[((v * h as f32) as usize).min(h - 1) * w + ((u * w as f32) as usize).min(w - 1)]));
        }
        let mut layouts = p2_script::Layouts::new(&loader);
        let terrain: Vec<[u8; 4]> = (pkg.exports.iter().enumerate())
            .filter(|(_, export)| pkg.object_name(export.class_index) == Some("TerrainInfo"))
            .flat_map(|(index, export)| {
                let layout = layouts.class_layout(Source { package: pkg, data }, index).unwrap();
                read_terrain(pkg, data, export, &layout).unwrap().vertex_colors
            })
            .collect();
        let info = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("LevelInfo")).unwrap();
        let zone_ambients = (model.zones.iter())
            .map(|zone| saved_ambient(pkg, data, pkg.exports.get(usize::try_from(zone.actor - 1).ok().unwrap_or(usize::MAX)).unwrap_or(info)))
            .collect();
        let lights = (1..=pkg.exports.len() as i32).filter_map(|r| saved_light(pkg, data, r).map(|l| (r, l))).collect();
        let environment = LightEnvironment::new(lights, zone_ambients, 1.0);
        (model, environment, pkg.exports.len(), mean_shown(&texels), mean_shown(&terrain))
    };
    let (mut lit, mut lit_sum, mut ambient_only, mut ambient_sum) = (0, 0.0, 0, 0.0);
    for index in 0..exports {
        let (mesh, placement, instance) = {
            let LoadedPackage { package: pkg, data, .. } = loader.package(map);
            let export = &pkg.exports[index];
            if export.serial_size == 0 || pkg.object_name(export.class_index) == Some("SoundThing") {
                continue;
            }
            let Ok(Some(actor)) = read_mesh_actor(&mut Objects::new(&loader), Source { package: pkg, data }, index) else { continue };
            if actor.unlit {
                continue;
            }
            let instance = (actor.instance > 0)
                .then(|| read_static_mesh_instance(pkg, data, &pkg.exports[actor.instance as usize - 1]).unwrap());
            let (mesh, placement) = mesh_placement(&loader, &actor);
            (mesh, placement, instance)
        };
        let Some(resolved) = loader.resolve(mesh.0, mesh.1).ok().flatten() else { continue };
        let LoadedPackage { package: pkg, data, .. } = loader.package(resolved.package);
        let mesh = read_static_mesh(pkg, data, &pkg.exports[resolved.export.unwrap()]).unwrap();
        let lighting = MeshLighting { placement, zone: p2_collision::point_zone(&model, placement.location), glow: 0 };
        let drawn = mean_shown(&environment.mesh_colors(&model, &mesh, mesh_sphere(&mesh), instance.as_ref(), &lighting));
        if instance.is_some_and(|i| i.colors.len() == mesh.positions.len()) {
            (lit, lit_sum) = (lit + 1, lit_sum + drawn);
        } else {
            (ambient_only, ambient_sum) = (ambient_only + 1, ambient_sum + drawn);
        }
    }
    let (statics, ambient) = (lit_sum / f64::from(lit), ambient_sum / f64::from(ambient_only));
    eprintln!(
        "shown at 2x: BSP {bsp:.1}, terrain {terrain:.1}, {lit} lit static meshes {statics:.1}, {ambient_only} with ambient only {ambient:.1}"
    );
    assert!(lit > 1000 && ambient_only > 100, "{lit} {ambient_only}");
    assert!(bsp < statics && statics < terrain, "BSP {bsp}, static meshes {statics}, terrain {terrain}");
}

#[test]
fn props_without_baked_light_not_black() {
    let Some(dir) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    for map_name in ["Suburbs-3", "Church"] {
        let map = loader.load(map_name).unwrap();
        let (environment, model) = {
            let LoadedPackage { package: pkg, data, .. } = loader.package(map);
            let level = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("Level")).unwrap();
            let level = read_level(pkg, data, level).unwrap();
            let model = read_model(pkg, data, &pkg.exports[level.model as usize - 1]).unwrap();
            let info = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("LevelInfo")).unwrap();
            let zone_ambients = (model.zones.iter())
                .map(|zone| saved_ambient(pkg, data, pkg.exports.get(usize::try_from(zone.actor - 1).ok().unwrap_or(usize::MAX)).unwrap_or(info)))
                .collect();
            let lights = (1..=pkg.exports.len() as i32).filter_map(|r| saved_light(pkg, data, r).map(|l| (r, l))).collect();
            (LightEnvironment::new(lights, zone_ambients, 1.0), model)
        };
        let shown = |colors: &[[u8; 4]]| {
            colors.iter().map(|c| c[..3].iter().map(|&b| (2.0 * f32::from(b)).min(255.0)).sum::<f32>() / 3.0).sum::<f32>()
                / colors.len().max(1) as f32
        };
        let exports = loader.package(map).package.exports.len();
        let (mut statics, mut props) = (Vec::new(), Vec::new());
        for index in 0..exports {
            let (mesh, placement, instance, class) = {
                let LoadedPackage { package: pkg, data, .. } = loader.package(map);
                let export = &pkg.exports[index];
                if export.serial_size == 0 || pkg.object_name(export.class_index) == Some("SoundThing") {
                    continue;
                }
                let Ok(Some(actor)) = read_mesh_actor(&mut Objects::new(&loader), Source { package: pkg, data }, index) else { continue };
                if actor.unlit {
                    continue;
                }
                let instance = (actor.instance > 0).then(|| read_static_mesh_instance(pkg, data, &pkg.exports[actor.instance as usize - 1]).unwrap());
                let (mesh, placement) = mesh_placement(&loader, &actor);
                (mesh, placement, instance, pkg.object_name(export.class_index).unwrap_or("?").to_owned())
            };
            let Some(resolved) = loader.resolve(mesh.0, mesh.1).ok().flatten() else { continue };
            let LoadedPackage { package: pkg, data, .. } = loader.package(resolved.package);
            let mesh = read_static_mesh(pkg, data, &pkg.exports[resolved.export.unwrap()]).unwrap();
            let lighting = MeshLighting { placement, zone: p2_collision::point_zone(&model, placement.location), glow: 0 };
            let after = shown(&environment.mesh_colors(&model, &mesh, mesh_sphere(&mesh), instance.as_ref(), &lighting));
            if instance.as_ref().is_some_and(|instance| instance_fits(&mesh, instance)) {
                statics.push((placement.location, after));
                continue;
            }
            let ambient = environment.ambient(lighting.zone, 0).map(|c| (c * 255.0).floor().min(255.0));
            let before = ambient.iter().map(|&c| (2.0 * c).min(255.0)).sum::<f32>() / 3.0;
            props.push((class, placement.location, before, after));
        }
        let near = |at: [f32; 3]| {
            let around: Vec<f32> = statics
                .iter()
                .filter(|(p, _)| (0..3).map(|i| (p[i] - at[i]).powi(2)).sum::<f32>() < 1000.0f32.powi(2))
                .map(|&(_, b)| b)
                .collect();
            (!around.is_empty()).then(|| around.iter().sum::<f32>() / around.len() as f32)
        };
        let black_before = props.iter().filter(|p| p.2 < 32.0).count();
        let black_after = props.iter().filter(|p| p.3 < 32.0).count();
        let dark: Vec<_> = props.iter().filter(|p| p.3 < 32.0 && near(p.1.into()).is_some_and(|b| b >= 64.0)).collect();
        if !dark.is_empty() {
            eprintln!("  dark among lit: {dark:?}");
        }
        let dark_among_lit = dark.len();
        let (mut prop_sum, mut static_sum, mut paired) = (0.0, 0.0, 0);
        for p in &props {
            if let Some(b) = near(p.1.into()) {
                (prop_sum, static_sum, paired) = (prop_sum + p.3, static_sum + b, paired + 1);
            }
        }
        let mut classes: HashMap<&str, usize> = HashMap::new();
        for p in &props {
            *classes.entry(p.0.as_str()).or_default() += 1;
        }
        let mean = |f: &dyn Fn(&(String, Vector, f32, f32)) -> f32| props.iter().map(f).sum::<f32>() / props.len().max(1) as f32;
        eprintln!(
            "{map_name}: {} props without baked light ({classes:?}); mean shown {:.1} before, {:.1} after; near black {black_before} before, {black_after} after, {dark_among_lit} among lit static meshes; {paired} with static meshes within 1000: props {:.1}, static meshes {:.1}",
            props.len(),
            mean(&|p| p.2),
            mean(&|p| p.3),
            prop_sum / paired.max(1) as f32,
            static_sum / paired.max(1) as f32,
        );
        assert!(!props.is_empty());
        assert!(black_after <= black_before, "{black_after} > {black_before}");
        assert!(dark_among_lit * 100 <= props.len(), "{dark_among_lit} props near black among lit static meshes");
    }
}

#[test]
fn window_alpha_from_opacity_mask() {
    let Some(root) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
    let mut library = Library::new(p2_host::game_dir::config_folder());
    let map = Path::new("Maps").join("Suburbs-3.fuk").display().to_string();
    let (game, _) = library.open(&root, &map).unwrap();
    let (package, export) = game.export_by_path("P2R_Tex_D.window_house_innertrans").expect("window shader");
    let drawn = game.material_texture(package, export_reference(export)).expect("window texture");
    assert_eq!(drawn.state.blending, Some(Blending::Alpha));
    let decoded = |game: &mut Game, path: &str| {
        let (package, export) = game.export_by_path(path).unwrap();
        let (package, data) = game.data(package);
        let source = Source { package, data };
        texture_rgba(&read_texture(&mut game.objects(), source, export).unwrap(), None).unwrap()
    };
    let (width, height, diffuse) = decoded(game, "STV-3.window-house");
    let (_, _, mask) = decoded(game, "P2R_Tex_D.window_house_innermask");
    assert_eq!(drawn.image.texture_descriptor.size.width, width);
    let data = drawn.image.data.as_ref().unwrap();
    let texels = (width * height) as usize;
    let drawn: Vec<&[u8]> = data[..texels * 4].chunks(4).collect();
    let differs = diffuse.iter().zip(&mask).filter(|(d, m)| d[3] != m[3]).count();
    eprintln!("{differs} of {texels} texels: the diffuse's own alpha differs from the mask's");
    assert!(drawn.iter().zip(&mask).all(|(d, m)| d[3] == m[3]));
    assert!(drawn.iter().zip(&diffuse).all(|(d, c)| d[..3] == c[..3]));
}
#[test]
fn projector_tag_takes_the_innermost_final_blend() {
    let projector = |material_blending| ProjectorActor {
        texture: String::new(),
        location: Vector::default(),
        rotation: Rotator::default(),
        draw_scale: 1.0,
        max_trace: 100.0,
        fov: 0,
        blending: ProjectorBlending::Add,
        material_blending,
        targets: crate::actors::ProjectTargets(0),
        tagged: false,
        follows: None,
        life: 0.0,
    };
    let splat = Some(FinalBlend { blending: p2_assets::FrameBufferBlending::AlphaBlend, alpha_ref: Some(10) });
    assert_eq!(projector_tag(&projector(ProjectorBlending::None), splat), PROJECTOR_TAG | ALPHA_TEST_TAG | 10 << 22 | 2);
    assert_eq!(projector_tag(&projector(ProjectorBlending::Modulate), splat), PROJECTOR_TAG | 1 << 8 | 4);
    assert_eq!(projector_tag(&projector(ProjectorBlending::None), None), PROJECTOR_TAG | 4);
    assert_eq!(projector_tag(&ProjectorActor { tagged: true, ..projector(ProjectorBlending::None) }, None), PROJECTOR_TAG | TAGGED_TAG | 4);
}

#[test]
fn textureless_invisible_shader_hides_its_section() {
    let Some(root) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
    let mut library = Library::new(p2_host::game_dir::config_folder());
    let map = Path::new("Maps").join("Suburbs-3.fuk").display().to_string();
    let (game, _) = library.open(&root, &map).unwrap();
    let (package, export) = game.export_by_path("P2R_Tex_D.Weapons.fake").expect("protest sign's invisible shader");
    let drawn = game.material_texture(package, export_reference(export)).expect("invisible section");
    assert_eq!(drawn.state.blending, Some(Blending::Invisible));
}
