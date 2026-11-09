mod common;

use common::game_dir;
use std::collections::BTreeMap;

use p2_package::{Loader, Package};
use p2_particles::{DrawStyle, Emitter, EmitterProperties, EmitterSettings, PropertyValue, RangeVector};
use p2_vm::{ObjectId, Value, Vm};

#[test]
fn emitter_classes_in_maps() {
    let Some(dir) = game_dir() else { return };
    let mut totals: BTreeMap<String, usize> = BTreeMap::new();
    for entry in std::fs::read_dir(dir.join("Maps")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| !e.eq_ignore_ascii_case("fuk")) {
            continue;
        }
        let data = std::fs::read(&path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        for export in &pkg.exports {
            let class = pkg.object_name(export.class_index).unwrap_or("");
            if class.ends_with("Emitter") {
                *totals.entry(class.to_owned()).or_default() += 1;
            }
        }
    }
    assert!(totals.get("SpriteEmitter").copied().unwrap_or(0) > 0);
}

fn property(value: &Value) -> PropertyValue {
    match value {
        &Value::Byte(v) => PropertyValue::Int(i32::from(v)),
        &Value::Int(v) => PropertyValue::Int(v),
        &Value::Bool(v) => PropertyValue::Bool(v),
        &Value::Float(v) => PropertyValue::Float(v),
        Value::Struct(members) => PropertyValue::Struct(members.iter().map(property).collect()),
        &Value::Vector(v) => PropertyValue::Struct(<[f32; 3]>::from(v).map(PropertyValue::Float).to_vec()),
        &Value::Rotator(r) => PropertyValue::Struct(<[i32; 3]>::from(r).map(PropertyValue::Int).to_vec()),
        Value::Array(items) => PropertyValue::Array(items.iter().map(property).collect()),
        _ => PropertyValue::Int(0),
    }
}

fn vector(vm: &Vm<&Loader>, object: ObjectId, name: &str) -> [f32; 3] {
    match vm.store.get(object, name) {
        Some(Value::Vector(v)) => <[f32; 3]>::from(v),
        _ => [0.0; 3],
    }
}

fn rotator(vm: &Vm<&Loader>, object: ObjectId) -> [i32; 3] {
    match vm.store.get(object, "Rotation") {
        Some(Value::Rotator(r)) => <[i32; 3]>::from(r),
        _ => [0; 3],
    }
}

fn scale_bounds(p: &EmitterProperties) -> (f32, f32) {
    if !p.use_size_scale {
        (1.0, 1.0)
    } else if p.use_regular_size_scale {
        (0.0, 1.0)
    } else {
        p.size_scale.iter().fold((1.0f32, 1.0f32), |(lo, hi), key| (lo.min(key.size), hi.max(key.size)))
    }
}

fn simulate_map(map: &str) {
    let Some(dir) = game_dir() else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    let package = loader.load(map).unwrap();
    let mut vm = Vm::new(&loader);
    for export in 0..loader.package(package).package.exports.len() {
        vm.load_object(package, export).unwrap();
    }
    let actors: Vec<ObjectId> =
        vm.object_ids().filter(|&id| matches!(vm.store.get(id, "Emitters"), Some(Value::Array(e)) if !e.is_empty())).collect();
    let (mut emitter_count, mut triggered, mut size_checks, mut white_checks, mut live_total) = (0, 0, 0, 0, 0);
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    for (seed, &actor) in actors.iter().enumerate() {
        let vm = &vm;
        let lookup = |object: ObjectId| move |name: &str| vm.store.get(object, name).as_ref().map(property);
        let settings = EmitterSettings::from_lookup(lookup(actor));
        let Some(Value::Array(entries)) = vm.store.get(actor, "Emitters") else { continue };
        let mut props = Vec::new();
        for entry in entries {
            let Value::Object(Some(emitter)) = entry else { continue };
            *classes.entry(vm.store.name(vm.store.object(emitter).class_name).to_owned()).or_default() += 1;
            props.push(EmitterProperties::from_lookup(lookup(emitter)));
        }
        emitter_count += props.len();
        let mut emitter = Emitter::new(settings, props, seed as u32 + 1);
        for e in emitter.emitters_mut() {
            triggered += usize::from(e.props().disabled);
            e.set_disabled(false);
        }
        let (location, rotation) = (vector(vm, actor, "Location"), rotator(vm, actor));
        for _ in 0..100 {
            emitter.tick(0.05, location.into(), rotation.into());
            for (index, e) in emitter.emitters().iter().enumerate() {
                let p = e.props();
                let live = e.live_particles();
                assert!(live <= usize::try_from(p.max_particles).unwrap_or(0), "{map}: {live} > {}", p.max_particles);
                let (lo, hi) = scale_bounds(p);
                let x = p.start_size_range.x;
                let (min, max) = (x.min.min(x.max) * lo, x.min.max(x.max) * hi);
                let plain = !p.use_color_scale
                    && !p.fade_in
                    && !p.fade_out
                    && p.color_multiplier_range == RangeVector::splat(1.0, 1.0);
                for sprite in emitter.sprites(index, p2_particles::View::default()) {
                    let size = sprite.size[0];
                    assert!(size >= min - 1e-3 && size <= max + 1e-3, "{map}: size {size} outside {min}..{max}");
                    size_checks += 1;
                    if plain && p.draw_style != DrawStyle::Modulated {
                        assert_eq!(sprite.color, [255; 4], "{map}");
                        white_checks += 1;
                    }
                }
            }
        }
        live_total += emitter.emitters().iter().map(p2_particles::ParticleEmitter::live_particles).sum::<usize>();
    }
    eprintln!(
        "{map}: {} emitter actors, {emitter_count} emitters {classes:?} ({triggered} disabled until triggered); {size_checks} sprite size checks, \
         {white_checks} colour checks; {live_total} particles alive after 5 s",
        actors.len()
    );
    assert!(live_total > 0);
}

#[test]
fn church_emitters_simulate() {
    simulate_map("Church");
}

#[test]
fn police_emitters_simulate() {
    simulate_map("police");
}


#[test]
fn shatter_glass_shards_bounce_and_lie_on_the_floor() {
    use p2_particles::{CollisionHit, Owner, View};
    let Some(dir) = game_dir() else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    let mut vm = Vm::new(&loader);
    let class = vm.find_class("FX", "ShatterGlass").unwrap();
    let actor = vm.new_object(class, None);
    let vm = &vm;
    let lookup = |object: ObjectId| move |name: &str| vm.store.get(object, name).as_ref().map(property);
    let Some(Value::Array(entries)) = vm.store.get(actor, "Emitters") else { panic!("no Emitters") };
    let props: Vec<EmitterProperties> =
        entries.iter().filter_map(|e| if let &Value::Object(Some(e)) = e { Some(EmitterProperties::from_lookup(lookup(e))) } else { None }).collect();
    assert_eq!(props.len(), 2);
    assert!(props[0].use_collision && props[0].spawn_from_other_emitter == Some(1), "{:?}", props[0]);
    assert_eq!(props[1].use_direction_as, p2_particles::DirectionUsage::Normal);
    let mut emitter = Emitter::new(EmitterSettings::from_lookup(lookup(actor)), props, 1);
    let floor_z = -300.0;
    let mut floor = |start: p2_math::Vector, end: p2_math::Vector| {
        (start[2] >= floor_z && end[2] < floor_z).then(|| {
            let t = (start[2] - floor_z) / (start[2] - end[2]);
            CollisionHit { location: [0, 1, 2].map(|i| start[i] + (end[i] - start[i]) * t).into(), normal: [0.0, 0.0, 1.0].into() }
        })
    };
    let owner = Owner::default();
    let view = View { location: [-500.0, 0.0, 0.0].into(), ..View::default() };
    let mut lowest = f32::MAX;
    for _ in 0..360 {
        emitter.tick_colliding(1.0 / 60.0, owner, &mut floor);
        for sprite in emitter.sprites(0, view) {
            lowest = lowest.min(sprite.location[2]);
        }
    }
    assert!(lowest >= floor_z, "a falling shard passed the floor: {lowest}");
    assert_eq!(emitter.emitters()[0].live_particles(), 0, "falling shards left after 6 s");
    let lying: Vec<_> = emitter.sprites(1, view).collect();
    assert!(!lying.is_empty());
    for sprite in &lying {
        assert!((sprite.location[2] - floor_z).abs() < 0.1, "{sprite:?}");
        assert!(sprite.up[2].abs() < 1e-4 && sprite.right[2].abs() < 1e-4, "not flat: {sprite:?}");
    }
}
