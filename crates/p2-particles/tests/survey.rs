mod common;

use common::game_dir;
use std::collections::BTreeMap;

use p2_assets::{read_palette, read_texture, texture_rgba};
use p2_package::{Loader, Package, PackageId, Resolved, TagValue};
use p2_script::{Objects, Source};

fn features(pkg: &Package, object: &p2_package::ObjectData) -> Vec<String> {
    let get = |name: &str| object.property(pkg, name);
    let set = |name: &str| matches!(get(name), Some(TagValue::Bool(true)));
    let mut out = Vec::new();
    for flag in [
        "UseCollision",
        "UseCollisionPlanes",
        "UseMaxCollisions",
        "UseSpawnedVelocityScale",
        "DampRotation",
        "UseActorForces",
        "UseColorScale",
        "FadeIn",
        "FadeOut",
        "UseSizeScale",
        "SpinParticles",
        "BlendBetweenSubdivisions",
        "UseRandomSubdivision",
        "UseSubdivisionScale",
        "UniformSize",
        "DisableFogging",
        "AcceptsProjectors",
        "ZWrite",
        "UseMeshBlendMode",
        "UseParticleColor",
        "RenderTwoSided",
        "UseBranching",
    ] {
        if set(flag) {
            out.push(flag.to_owned());
        }
    }
    for off in ["AlphaTest", "ZTest", "AutomaticInitialSpawning", "RespawnDeadParticles"] {
        if matches!(get(off), Some(TagValue::Bool(false))) {
            out.push(format!("!{off}"));
        }
    }
    if matches!(get("SpawnFromOtherEmitter"), Some(&TagValue::Int(i)) if i >= 0) {
        out.push("SpawnFromOtherEmitter".to_owned());
    }
    for (name, labels) in [
        ("DrawStyle", &["Regular", "AlphaBlend", "Modulated", "Translucent", "AlphaModulate", "Darken", "Brighten"][..]),
        ("UseDirectionAs", &["None", "Up", "Right", "Forward", "Normal", "UpAndNormal", "RightAndNormal"][..]),
        ("CoordinateSystem", &["Independent", "Relative", "Absolute"][..]),
        ("UseRotationFrom", &["None", "Actor", "Offset", "Normal"][..]),
        ("StartLocationShape", &["Box", "Sphere", "Polar"][..]),
        ("GetVelocityDirectionFrom", &["None", "StartPositionAndOwner", "OwnerAndStartPosition", "AddRadial"][..]),
        ("CollisionSound", &["None", "LinearGlobal", "LinearLocal", "Random"][..]),
        ("LocationShapeExtend", &["None", "Line", "Circle"][..]),
    ] {
        if let Some(&TagValue::Byte(v)) = get(name) {
            out.push(format!("{name}={}", labels.get(usize::from(v)).copied().unwrap_or("?")));
        }
    }
    for name in ["AddLocationFromOtherEmitter", "AddVelocityFromOtherEmitter"] {
        if matches!(get(name), Some(&TagValue::Int(i)) if i >= 0) {
            out.push(name.to_owned());
        }
    }
    for name in ["WarmupTicksPerSecond", "MinSquaredVelocity", "AlphaRef"] {
        if matches!(get(name), Some(&TagValue::Float(v)) if v != 0.0) || matches!(get(name), Some(&TagValue::Int(v)) if v != 0) {
            out.push(name.to_owned());
        }
    }
    if matches!(get("StaticMesh"), Some(&TagValue::Object(o)) if o != 0) {
        out.push("StaticMesh".to_owned());
    }
    out
}

fn texture_alpha_share(loader: &mut Loader, package: PackageId, reference: i32) -> Option<(String, f32)> {
    let Ok(Some(Resolved { package, export: Some(export) })) = loader.resolve(package, reference) else { return None };
    let (texture, palette_ref) = {
        let loaded = loader.package(package);
        let export_entry = &loaded.package.exports[export];
        if loaded.package.object_name(export_entry.class_index) != Some("Texture") {
            return Some((loaded.package.object_name(export_entry.class_index).unwrap_or("?").to_owned(), f32::NAN));
        }
        let source = Source { package: &loaded.package, data: &loaded.data };
        let texture = read_texture(&mut Objects::new(&*loader), source, export).ok()?;
        let palette = texture.palette;
        (texture, palette)
    };
    let palette = palette_ref.and_then(|reference| match loader.resolve(package, reference).ok()?? {
        Resolved { package, export: Some(export) } => {
            let loaded = loader.package(package);
            read_palette(&loaded.package, &loaded.data, &loaded.package.exports[export]).ok()
        }
        _ => None,
    });
    let (_, _, texels) = texture_rgba(&texture, palette.as_deref())?;
    let (mut light, mut faded) = (0.0f64, 0.0f64);
    for [r, g, b, a] in texels {
        let c = f64::from(u16::from(r) + u16::from(g) + u16::from(b));
        light += c;
        faded += c * f64::from(255 - a) / 255.0;
    }
    Some((format!("{:?}", texture.format), if light == 0.0 { 0.0 } else { (faded / light) as f32 }))
}

#[test]
#[ignore = "survey: prints only"]
fn emitter_feature_survey() {
    let Some(dir) = game_dir() else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    let mut names: Vec<String> = loader
        .package_names()
        .filter(|name| loader.find(name).and_then(|p| p.extension()).is_some_and(|e| e.eq_ignore_ascii_case("u") || e.eq_ignore_ascii_case("fuk")))
        .map(str::to_owned)
        .collect();
    names.sort();
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut textures: Vec<(PackageId, i32, u8, String)> = Vec::new();
    for name in &names {
        let Ok(id) = loader.load(name) else { continue };
        let loaded = loader.package(id);
        let pkg = &loaded.package;
        for export in &pkg.exports {
            let class = pkg.object_name(export.class_index).unwrap_or("");
            if export.class_index == 0 || !class.ends_with("Emitter") || class == "Emitter" || class.ends_with("P2Emitter") {
                continue;
            }
            let Ok(object) = pkg.read_object(&loaded.data, export) else { continue };
            if object.property(pkg, "Emitters").is_some() || object.property(pkg, "Location").is_some() {
                continue;
            }
            *classes.entry(class.to_owned()).or_default() += 1;
            for feature in features(pkg, &object) {
                *counts.entry(feature).or_default() += 1;
            }
            let style = match object.property(pkg, "DrawStyle") {
                Some(&TagValue::Byte(v)) => v,
                _ => 3,
            };
            if let Some(&TagValue::Object(texture)) = object.property(pkg, "Texture") {
                textures.push((id, texture, style, pkg.name(export.object_name).to_owned()));
            }
        }
    }
    eprintln!("classes: {classes:#?}");
    eprintln!("features: {counts:#?}");
    let mut formats: BTreeMap<String, usize> = BTreeMap::new();
    let (mut with_alpha, mut additive) = (0, 0);
    for (package, reference, style, name) in textures {
        if style != 3 && style != 6 {
            continue;
        }
        let Some((format, share)) = texture_alpha_share(&mut loader, package, reference) else { continue };
        additive += 1;
        *formats.entry(format.clone()).or_default() += 1;
        if share > 0.05 {
            with_alpha += 1;
            if with_alpha <= 15 {
                eprintln!("additive emitter {name}: {format} texture, {:.0}% of its colour under alpha < 255", share * 100.0);
            }
        }
    }
    eprintln!("additive emitters with a texture: {additive}, formats {formats:?}; {with_alpha} with > 5% of their colour under alpha < 255");
    assert!(classes.get("SpriteEmitter").copied().unwrap_or(0) > 0);
}
