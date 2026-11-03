mod common;

use common::{game_dir, ini, loader, object_of, system_ini};

use p2_math::Rotator;
use p2_package::Loader;
use p2_vm::{HostCommand, NameId, ObjectId, ParticleCommand, PlayedSound, Value, Vm};

const ANIM_NATIVES: [&str; 17] = [
    "PlayAnim", "PlayAnimAt", "LoopAnim", "TweenAnim", "IsAnimating", "FinishAnim", "HasAnim", "StopAnimating",
    "FreezeAnimAt", "IsTweening", "AnimIsInGroup", "LinkSkelAnim", "GetDefaultAnim", "AnimBlendParams",
    "AnimBlendToAlpha", "GetAnimParams", "EnableChannelNotify",
];

fn load_animations(loader: &mut Loader) {
    loader.load_imported(&["MeshAnimation", "SkeletalMesh"]).unwrap();
}

fn sequence_names(loader: &Loader, vm: &Vm<&Loader>, animation: ObjectId) -> Vec<String> {
    let (id, export) = loader.loaded_export_by_path(&vm.object(animation).path).expect("animation export");
    let loaded = loader.package(id);
    let pkg = &loaded.package;
    let animation = p2_assets::read_mesh_animation(pkg, &loaded.data, &pkg.exports[export]).unwrap();
    animation.sequences.iter().map(|s| pkg.name(s.name).to_owned()).collect()
}

#[test]
fn actor_defaults() {
    let Some(loader) = loader() else { return };
    let mut vm = Vm::new(&loader);
    let actor = vm.find_class("Engine", "Actor").unwrap();

    assert_eq!(vm.default_value(actor, "DrawType"), Some(Value::Byte(1)));
    assert_eq!(vm.default_value(actor, "DrawScale"), Some(Value::Float(1.0)));
    assert_eq!(vm.default_value(actor, "CollisionRadius"), Some(Value::Float(22.0)));
    assert_eq!(vm.default_value(actor, "bMovable"), Some(Value::Bool(true)));
    assert_eq!(
        vm.default_value(actor, "DrawScale3D"),
        Some(Value::Vector(p2_math::Vector::new(1.0, 1.0, 1.0)))
    );
    let Some(Value::Object(Some(texture))) = vm.default_value(actor, "Texture") else { panic!("Texture") };
    assert_eq!(vm.name(vm.object(texture).name), "S_Actor");
    let Some(Value::Object(Some(message))) = vm.default_value(actor, "MessageClass") else { panic!("MessageClass") };
    assert_eq!(vm.object(message).path, "Engine.LocalMessage");
    assert_eq!(vm.name(vm.object(message).class_name), "Class");
    assert_eq!(vm.default_value(actor, "bHidden"), Some(Value::Bool(false)));
}

#[test]
fn pawn_inherits_actor_defaults() {
    let Some(loader) = loader() else { return };
    let mut vm = Vm::new(&loader);
    let pawn = vm.find_class("Engine", "Pawn").unwrap();
    let actor = vm.class(pawn).super_class.map(|id| vm.class(id).name);
    assert_eq!(actor.map(|name| vm.name(name)), Some("Actor"));
    assert_eq!(vm.default_value(pawn, "DrawType"), Some(Value::Byte(2)));
    assert_eq!(vm.default_value(pawn, "bMovable"), Some(Value::Bool(true)));
    let actor = vm.find_class("Engine", "Actor").unwrap();
    assert_eq!(vm.default_value(actor, "SoundRadius"), Some(Value::Float(64.0)));
    assert_eq!(vm.default_value(pawn, "SoundRadius"), Some(Value::Float(9.0)));
}

#[test]
fn every_class_loads() {
    let Some(loader) = loader() else { return };
    let mut vm = Vm::new(&loader);
    let mut failures = Vec::new();
    let mut classes = 0;
    for name in loader.package_names().filter(|name| loader.loaded_id(name).is_some()) {
        let Some(id) = loader.loaded_id(name) else { continue };
        let pkg = &loader.package(id).package;
        for export in pkg.exports.iter().filter(|e| e.class_index == 0) {
            let class = pkg.name(export.object_name);
            match vm.find_class(name, class) {
                Ok(_) => classes += 1,
                Err(e) => failures.push(format!("{name}.{class}: {e}")),
            }
        }
    }
    assert!(classes > 2000);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn new_object_from_defaults() {
    let Some(loader) = loader() else { return };
    let mut vm = Vm::new(&loader);
    let actor = vm.find_class("Engine", "Actor").unwrap();
    let first = vm.new_object(actor, None);
    let second = vm.new_object(actor, None);
    assert_eq!(vm.name(vm.object(first).name), "Actor0");
    assert_eq!(vm.name(vm.object(second).name), "Actor1");
    assert_eq!(vm.get(first, "DrawScale"), Some(Value::Float(1.0)));
    assert!(vm.set(first, "DrawScale", Value::Float(2.0)));
    assert_eq!(vm.get(first, "DrawScale"), Some(Value::Float(2.0)));
    assert_eq!(vm.get(second, "DrawScale"), Some(Value::Float(1.0)));
    assert_eq!(vm.default_value(actor, "DrawScale"), Some(Value::Float(1.0)));
    assert!(!vm.set(first, "NoSuchProperty", Value::Int(0)));
}

#[test]
fn church_actors_load() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    let mut vm = Vm::new(&loader);
    let exports = loader.package(map).package.exports.len();
    let mut failures = Vec::new();
    let mut mesh_actors = 0;
    for export in 0..exports {
        let object = match vm.load_object(map, export) {
            Ok(object) => object,
            Err(e) => {
                failures.push(format!("{export}: {e}"));
                continue;
            }
        };
        let drawn = vm.get(object, "DrawType") == Some(Value::Byte(8))
            && vm.get(object, "bHidden") == Some(Value::Bool(false))
            && matches!(vm.get(object, "StaticMesh"), Some(Value::Object(Some(_))));
        mesh_actors += usize::from(drawn);
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    assert_eq!(mesh_actors, 2122);
}
#[test]
fn run_damage_type_is_of_type() {
    let Some(loader) = loader() else { return };
    let mut vm = Vm::new(&loader);
    let class = vm.find_class("Engine", "DamageType").unwrap();
    let function = vm.find_function(class, "IsOfType").unwrap().unwrap();
    for (description, expected) in [(1, true), (0, true), (2, false), (3, false)] {
        let result = vm.call(&function, None, class, vec![Value::Int(description)]).unwrap();
        assert_eq!(result, Some(Value::Bool(expected)), "IsOfType({description})");
    }
}

#[test]
fn run_ammunition_add_ammo() {
    let Some(loader) = loader() else { return };
    let mut vm = Vm::new(&loader);
    let class = vm.find_class("Engine", "Ammunition").unwrap();
    let ammo = vm.new_object(class, None);
    vm.set(ammo, "AmmoAmount", Value::Int(5));
    vm.set(ammo, "MaxAmmo", Value::Int(10));
    let function = vm.find_function(class, "AddAmmo").unwrap().unwrap();
    assert_eq!(vm.call(&function, Some(ammo), class, vec![Value::Int(3)]).unwrap(), Some(Value::Bool(true)));
    assert_eq!(vm.get(ammo, "AmmoAmount"), Some(Value::Int(8)));
    vm.call(&function, Some(ammo), class, vec![Value::Int(10)]).unwrap();
    assert_eq!(vm.get(ammo, "AmmoAmount"), Some(Value::Int(10)));
}
#[test]
fn run_game_info_options() {
    let Some(loader) = loader() else { return };
    let mut vm = Vm::new(&loader);
    let class = vm.find_class("Engine", "GameInfo").unwrap();
    let game = vm.new_object(class, None);
    let options = Value::Str("?Name=Dude?Class=Postal2Game.Dude?Difficulty=3".to_owned());
    let parse = vm.find_function(class, "ParseOption").unwrap().unwrap();
    let option = |vm: &mut Vm<&Loader>, key: &str| {
        vm.call(&parse, Some(game), class, vec![options.clone(), Value::Str(key.to_owned())]).unwrap()
    };
    assert_eq!(option(&mut vm, "class"), Some(Value::Str("Postal2Game.Dude".to_owned())));
    assert_eq!(option(&mut vm, "Name"), Some(Value::Str("Dude".to_owned())));
    assert_eq!(option(&mut vm, "Missing"), Some(Value::Str(String::new())));

    let int_option = vm.find_function(class, "GetIntOption").unwrap().unwrap();
    let args = vec![options.clone(), Value::Str("Difficulty".to_owned()), Value::Int(1)];
    assert_eq!(vm.call(&int_option, Some(game), class, args).unwrap(), Some(Value::Int(3)));
    assert_eq!(vm.diagnostics.log, ["Difficulty 3"]);
    let args = vec![options, Value::Str("Missing".to_owned()), Value::Int(7)];
    assert_eq!(vm.call(&int_option, Some(game), class, args).unwrap(), Some(Value::Int(7)));
}
#[test]
fn run_trigger_is_relevant() {
    let Some(loader) = loader() else { return };
    let mut vm = Vm::new(&loader);
    let trigger_class = vm.find_class("Engine", "Trigger").unwrap();
    let projectile_class = vm.find_class("Engine", "Projectile").unwrap();
    let trigger = vm.new_object(trigger_class, None);
    let other = vm.new_object(trigger_class, None);
    let projectile = vm.new_object(projectile_class, None);
    vm.set(trigger, "bInitiallyActive", Value::Bool(true));
    vm.set(trigger, "DamageThreshold", Value::Float(10.0));
    vm.set(projectile, "Damage", Value::Float(50.0));
    let function = vm.find_function(trigger_class, "IsRelevant").unwrap().unwrap();
    let relevant = |vm: &mut Vm<&Loader>, trigger_type: u8, actor| {
        vm.set(trigger, "TriggerType", Value::Byte(trigger_type));
        vm.call(&function, Some(trigger), trigger_class, vec![Value::Object(Some(actor))]).unwrap()
    };
    assert_eq!(relevant(&mut vm, 3, other), Some(Value::Bool(true)));
    assert_eq!(relevant(&mut vm, 5, other), Some(Value::Bool(false)));
    assert_eq!(relevant(&mut vm, 0, other), Some(Value::Bool(false)));
    assert_eq!(relevant(&mut vm, 4, projectile), Some(Value::Bool(true)));
    assert_eq!(relevant(&mut vm, 4, other), Some(Value::Bool(false)));
    vm.set(projectile, "Damage", Value::Float(5.0));
    assert_eq!(relevant(&mut vm, 4, projectile), Some(Value::Bool(false)));
}

#[test]
fn run_warp_zone_marker_find_trigger_actor() {
    let Some(loader) = loader() else { return };
    let mut vm = Vm::new(&loader);
    let marker_class = vm.find_class("Engine", "WarpZoneMarker").unwrap();
    let zone_class = vm.find_class("Engine", "WarpZoneInfo").unwrap();
    let trigger_class = vm.find_class("Engine", "ZoneTrigger").unwrap();
    let marker = vm.new_object(marker_class, None);
    let zone = vm.new_object(zone_class, None);
    let (wrong, right) = (vm.new_object(trigger_class, None), vm.new_object(trigger_class, None));
    let (zone_tag, other_tag) = (vm.intern("Zone1"), vm.intern("Elsewhere"));
    vm.set(zone, "ZoneTag", Value::Name(zone_tag));
    vm.set(marker, "markedWarpZone", Value::Object(Some(zone)));
    vm.set(wrong, "Event", Value::Name(other_tag));
    vm.set(right, "Event", Value::Name(zone_tag));
    let function = vm.find_function(marker_class, "FindTriggerActor").unwrap().unwrap();
    vm.call(&function, Some(marker), marker_class, Vec::new()).unwrap();
    assert_eq!(vm.get(marker, "TriggerActor"), Some(Value::Object(Some(right))));
    vm.set(marker, "TriggerActor", Value::Object(None));
    vm.set(right, "Event", Value::Name(other_tag));
    vm.call(&function, Some(marker), marker_class, Vec::new()).unwrap();
    assert_eq!(vm.get(marker, "TriggerActor"), Some(Value::Object(None)));
}
#[test]
fn run_pawn_dying_state() {
    let Some(loader) = loader() else { return };
    let mut vm = Vm::new(&loader);
    let class = vm.find_class("Engine", "Pawn").unwrap();
    let pawn = vm.new_object(class, None);
    let dying = vm.intern("Dying");
    vm.goto_state(pawn, dying, NameId::NONE).unwrap();
    assert_eq!(vm.state_name(pawn), dying);
    assert_eq!(vm.get(pawn, "bInvulnerableBody"), Some(Value::Bool(true)));
    assert_eq!(vm.get(pawn, "Physics"), Some(Value::Byte(2)), "PHYS_Falling");
    assert_eq!(vm.get(pawn, "TimerRate"), Some(Value::Float(12.0)));

    vm.tick(pawn, 0.0).unwrap();
    vm.tick(pawn, 0.1).unwrap();
    assert_eq!(vm.get(pawn, "bInvulnerableBody"), Some(Value::Bool(true)));
    vm.tick(pawn, 0.15).unwrap();
    assert_eq!(vm.get(pawn, "bInvulnerableBody"), Some(Value::Bool(false)));

    let died = vm.find_object_function(pawn, "Died").unwrap().unwrap();
    let pawn_died = vm.find_function(class, "Died").unwrap().unwrap();
    assert_ne!(died.export, pawn_died.export);
}
#[test]
fn spawn_tick_destroy() {
    let Some(loader) = loader() else { return };
    let mut vm = Vm::new(&loader);
    let level_class = vm.find_class("Engine", "LevelInfo").unwrap();
    let game_class = vm.find_class("Engine", "GameInfo").unwrap();
    let mutator_class = vm.find_class("Engine", "Mutator").unwrap();
    let (level, game, mutator) =
        (vm.new_object(level_class, None), vm.new_object(game_class, None), vm.new_object(mutator_class, None));
    vm.set(game, "BaseMutator", Value::Object(Some(mutator)));
    vm.set(level, "Game", Value::Object(Some(game)));
    vm.set_level(level);

    let trigger_class = vm.find_class("Engine", "Trigger").unwrap();
    let trigger = vm.spawn(trigger_class, None, [None, None]).unwrap().expect("spawned");
    assert_eq!(vm.get(trigger, "Level"), Some(Value::Object(Some(level))));
    assert_eq!(vm.get(trigger, "bScriptInitialized"), Some(Value::Bool(true)));
    let normal = vm.intern("NormalTrigger");
    assert_eq!(vm.state_name(trigger), normal);
    let Some(Value::Object(Some(class))) = vm.get(trigger, "Class") else { panic!("Class") };
    assert_eq!(vm.object(class).path, "Engine.Trigger");

    vm.set(trigger, "TimerRate", Value::Float(1.0));
    vm.tick_actors(0.6).unwrap();
    assert_eq!(vm.get(trigger, "TimerRate"), Some(Value::Float(1.0)));
    vm.tick_actors(0.6).unwrap();
    assert_eq!(vm.get(trigger, "TimerRate"), Some(Value::Float(0.0)));

    vm.destroy(trigger).unwrap();
    assert!(vm.is_deleted(trigger));
    assert!(!vm.actors().any(|actor| actor == trigger), "a destroyed actor leaves the actor list");
    assert_eq!(vm.state_name(trigger), NameId::NONE);
}
#[test]
fn church_begin_play() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("Engine", "GameInfo").unwrap();
    let errors = vm.begin_play(map, game_class, "").unwrap();
    let mut grouped = std::collections::BTreeMap::new();
    for (_, event, error) in &errors {
        *grouped.entry(format!("{event}: {error}")).or_insert(0) += 1;
    }
    assert!(errors.is_empty(), "{grouped:#?}");
    let Some(Value::Object(Some(game))) = vm.get(vm.level().unwrap(), "Game") else { panic!("Game") };
    assert!(matches!(vm.get(game, "BaseMutator"), Some(Value::Object(Some(_)))));
}
#[test]
fn church_begin_play_postal2_game() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    let errors = vm.begin_play(map, game_class, "").unwrap();
    let mut grouped = std::collections::BTreeMap::new();
    for (_, event, error) in &errors {
        *grouped.entry(format!("{event}: {error}")).or_insert(0) += 1;
    }
    assert!(errors.is_empty(), "{grouped:#?}");
}
#[test]
fn church_ticks_a_minute() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    load_animations(&mut loader);
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    let time = |vm: &Vm<&Loader>| match vm.get(vm.level().unwrap(), "TimeSeconds") {
        Some(Value::Float(time)) => time,
        _ => panic!("TimeSeconds"),
    };
    let start = time(&vm);
    let mut grouped = std::collections::BTreeMap::new();
    for _ in 0..1200 {
        for (_, error) in vm.tick_level(0.05) {
            *grouped.entry(format!("{error}")).or_insert(0) += 1;
        }
    }
    let elapsed = time(&vm) - start;
    assert!((elapsed - 60.0).abs() < 0.01);
    assert!(grouped.is_empty(), "{grouped:#?}");
    let missing: Vec<_> = ANIM_NATIVES.iter().filter(|n| vm.diagnostics.missing_natives.contains_key(**n)).collect();
    assert!(missing.is_empty(), "{missing:?}");
}

#[test]
fn church_pawns_animate() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    load_animations(&mut loader);
    let mut vm = Vm::new(&loader);
    let game = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game, "").unwrap().is_empty());
    let pawns: Vec<ObjectId> = vm.object_ids().filter(|&o| vm.is_a(o, "Pawn") && !vm.is_deleted(o)).collect();
    let channels = |vm: &Vm<&Loader>| -> Vec<_> {
        pawns.iter().map(|&p| vm.anim_channel(p, 0).map(|(c, a)| (c.sequence, c.frame, c.looping, a))).collect()
    };
    let (mut advanced, mut wrapped) = (0, 0);
    let mut previous = channels(&vm);
    for _ in 0..100 {
        assert!(vm.tick_level(0.05).is_empty());
        let current = channels(&vm);
        for (before, after) in previous.iter().zip(&current) {
            if let (Some((s0, f0, _, _)), Some((s1, f1, looping, _))) = (before, after)
                && s0 == s1
                && *s1 != NameId::NONE
            {
                advanced += usize::from(f1 > f0);
                wrapped += usize::from(*looping && f1 < f0);
            }
        }
        previous = current;
    }
    let mut playing = std::collections::BTreeMap::new();
    for (sequence, _, _, animation) in previous.iter().flatten().filter(|c| c.0 != NameId::NONE) {
        let animation = animation.expect("animation of a playing channel");
        let name = vm.name(*sequence).to_owned();
        assert!(sequence_names(&loader, &vm, animation).iter().any(|s| s.eq_ignore_ascii_case(&name)), "{name}");
        *playing.entry(name).or_insert(0) += 1;
    }
    let count: usize = playing.values().sum();
    assert!(count * 2 >= pawns.len() && count > 10);
    assert!(advanced > 100 && wrapped > 0);
}

#[test]
fn finish_anim_waits_for_the_sequence() {
    let Some(mut loader) = loader() else { return };
    load_animations(&mut loader);
    let mut vm = Vm::new(&loader);
    let class = vm.find_class("Inventory", "HandsWeapon").unwrap();
    let hands = vm.new_object(class, None);
    vm.set(hands, "DrawType", Value::Byte(2));
    let Some(Value::Object(Some(mesh))) = vm.get(hands, "Mesh") else { panic!("Mesh") };
    let get_default_anim = vm.find_function(class, "GetDefaultAnim").unwrap().unwrap();
    let Some(Value::Object(Some(animation))) = vm.call(&get_default_anim, Some(hands), class, vec![Value::Object(Some(mesh))]).unwrap()
    else {
        panic!("no default animation for {}", vm.object(mesh).path)
    };
    let (id, export) = loader.loaded_export_by_path(&vm.object(animation).path).unwrap();
    let pkg = &loader.package(id).package;
    let anim = p2_assets::read_mesh_animation(pkg, &loader.package(id).data, &pkg.exports[export]).unwrap();
    let sequence = anim.sequences.iter().find(|s| s.num_frames > 10 && s.rate > 0.0).unwrap();
    let name = vm.intern(pkg.name(sequence.name));
    vm.set(hands, "CurrentUpAnim", Value::Name(name));
    let Some(Value::Float(speed)) = vm.get(hands, "WeaponSpeedShoot1") else { panic!("WeaponSpeedShoot1") };
    let frames = sequence.num_frames as f32;
    let seconds = (1.0 - 1.0 / frames - 0.001) * frames / (sequence.rate * speed);

    let state = vm.intern("BringUpItem");
    vm.goto_state(hands, state, NameId::NONE).unwrap();
    let mut ticks = 0;
    while vm.state_name(hands) == state && ticks < 1000 {
        vm.tick_actor(hands, 0.05).unwrap();
        ticks += 1;
    }
    assert_eq!(vm.name(vm.state_name(hands)), "NormalFire");
    let expected = (seconds / 0.05).ceil() as i32 + 2;
    assert!((ticks - expected).abs() <= 1, "{ticks} ticks, expected {expected}");
}
#[test]
#[ignore = "survey: prints only; slow, every map"]
fn survey_all_maps() {
    let Some(dir) = game_dir() else { return };
    let mut errors: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut missing: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut maps = 0;
    for entry in std::fs::read_dir(dir.join("Maps")).unwrap() {
        let path = entry.unwrap().path();
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("fuk")) {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let Some(mut loader) = loader() else { return };
        let Ok(map) = loader.load(&name) else { continue };
        let mut vm = Vm::new(&loader);
        let game = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
        maps += 1;
        let mut record = |error: String| {
            let key: String = error.chars().filter(|c| !c.is_ascii_digit()).take(90).collect();
            *errors.entry(key).or_insert(0) += 1;
        };
        match vm.begin_play(map, game, "") {
            Ok(start) => start.into_iter().for_each(|(_, event, e)| record(format!("{event}: {e}"))),
            Err(e) => record(format!("begin_play: {e}")),
        }
        for _ in 0..100 {
            vm.tick_level(0.05).into_iter().for_each(|(_, e)| record(format!("tick: {e}")));
        }
        for (native, count) in &vm.diagnostics.missing_natives {
            *missing.entry(native.clone()).or_insert(0) += count;
        }
    }
    let mut missing: Vec<_> = missing.into_iter().collect();
    missing.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    eprintln!("{maps} maps; errors {errors:#?}");
    eprintln!("missing natives (most called): {:#?}", missing.iter().take(40).collect::<Vec<_>>());
}
#[test]
fn church_pawns_get_heads() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    let mut vm = Vm::new(&loader);
    let game = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game, "").unwrap().is_empty());
    let heads: Vec<_> = vm
        .object_ids()
        .filter(|&o| vm.is_a(o, "Head") && matches!(vm.get(o, "Base"), Some(Value::Object(Some(_)))))
        .collect();
    let with_mesh = heads.iter().filter(|&&h| matches!(vm.get(h, "Mesh"), Some(Value::Object(Some(_))))).count();
    assert!(heads.len() > 50 && with_mesh * 10 >= heads.len() * 9);
}
#[test]
fn church_player_joins() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    let (player, pawn) = vm.login("").unwrap().expect("no player controller");
    let pawn = pawn.expect("no pawn");
    let start = vm.object_ids().find(|&o| vm.is_a(o, "PlayerStart")).unwrap();
    assert_eq!(vm.get(pawn, "Location"), vm.get(start, "Location"));
    assert_eq!(vm.get(pawn, "Controller"), Some(Value::Object(Some(player))));
    let dudes = vm.object_ids().filter(|&o| vm.is_a(o, "AWPostalDude") && !vm.is_deleted(o)).count();
    assert_eq!(dudes, 1);
}

#[test]
fn church_plays_sounds() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    loader.load_imports(map).unwrap();
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    for _ in 0..1200 {
        assert!(vm.tick_level(0.05).is_empty());
    }
    assert!(!vm.diagnostics.missing_natives.contains_key("PlaySound"));
    assert!(!vm.diagnostics.missing_natives.contains_key("GetSoundDuration"));
    let played = take_sounds(&mut vm);
    assert!(!played.is_empty());
    drop(vm);
    for sound in &played {
        if loader.loaded_export_by_path(&sound.sound).is_none() {
            let package = sound.sound.split('.').next().unwrap_or_default();
            loader.load(package).unwrap();
        }
        assert!(loader.loaded_export_by_path(&sound.sound).is_some(), "{} not found", sound.sound);
        assert!(sound.radius > 0.0 && sound.pitch > 0.0, "{sound:?}");
    }
}

#[test]
fn player_touches_a_trigger() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    let (_, pawn) = vm.login("").unwrap().unwrap();
    let pawn = pawn.unwrap();
    let triggers: Vec<_> = vm
        .object_ids()
        .filter(|&o| vm.is_a(o, "Trigger") && vm.get(o, "bCollideActors") == Some(Value::Bool(true)))
        .collect();
    assert!(!triggers.is_empty());
    let mut fired = 0;
    for trigger in triggers {
        let at = ["X", "Y", "Z"].map(|m| match vm.get_path(trigger, &["Location", m]) {
            Some(Value::Float(v)) => v,
            _ => 0.0,
        });
        vm.call_native("SetLocation", Some(pawn), &mut [Value::Vector(at.into())]).unwrap();
        if vm.get(trigger, "bCollideActors") == Some(Value::Bool(false)) {
            fired += 1;
        }
        let touching = |o| match vm.get(o, "Touching") {
            Some(Value::Array(list)) => list.contains(&Value::Object(Some(if o == pawn { trigger } else { pawn }))),
            _ => false,
        };
        assert_eq!(touching(pawn), touching(trigger), "lists agree");
    }
    assert!(fired > 0);
}

fn take_sounds(vm: &mut Vm<&Loader>) -> Vec<PlayedSound> {
    vm.take_commands(|command| matches!(command, HostCommand::Sound(_))).filter_map(|command| if let HostCommand::Sound(sound) = command { Some(sound) } else { None }).collect()
}

fn take_particles(vm: &mut Vm<&Loader>) -> Vec<ParticleCommand> {
    vm.take_commands(|command| matches!(command, HostCommand::Particle(_))).filter_map(|command| if let HostCommand::Particle(particle) = command { Some(particle) } else { None }).collect()
}

fn take_travel(vm: &mut Vm<&Loader>) -> Option<String> {
    vm.take_commands(|command| matches!(command, HostCommand::Travel(_))).filter_map(|command| if let HostCommand::Travel(url) = command { Some(url) } else { None }).last()
}

fn vector_of(vm: &Vm<&Loader>, object: ObjectId, property: &str, index: Option<usize>) -> [f32; 3] {
    let value = match (vm.get(object, property), index) {
        (Some(Value::Array(elements)), Some(index)) => elements.get(index).cloned(),
        (value, None) => value,
        _ => None,
    };
    let Some(Value::Vector(v)) = value else { panic!("{property} is not a vector") };
    v.into()
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn ticks_to_reach(vm: &mut Vm<&Loader>, mover: ObjectId, target: [f32; 3], limit: usize) -> usize {
    for tick in 1..=limit {
        assert!(vm.tick_level(0.05).is_empty());
        if distance(vector_of(vm, mover, "Location", None), target) < 0.01
            && vm.get(mover, "bInterpolating") == Some(Value::Bool(false))
        {
            return tick;
        }
    }
    panic!("{} never reached {target:?}", vm.object(mover).path);
}

#[test]
fn church_mover_moves_when_triggered() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    let (_, pawn) = vm.login("").unwrap().unwrap();
    let mover = vm.object_ids().find(|&o| vm.object(o).path == "Church.Mover5").expect("Church.Mover5");
    let trigger = vm.object_ids().find(|&o| vm.object(o).path == "Church.TriggerSuper0").expect("Church.TriggerSuper0");
    assert_eq!(vm.name(vm.state_name(mover)), "TriggerToggle");
    let base = vector_of(&vm, mover, "BasePos", None);
    let [closed, open] = [0, 1].map(|key| {
        let offset = vector_of(&vm, mover, "KeyPos", Some(key));
        [0, 1, 2].map(|i| base[i] + offset[i])
    });
    assert!(distance(vector_of(&vm, mover, "Location", None), closed) < 0.01);

    vm.event(mover, "Trigger", vec![Value::Object(Some(trigger)), Value::Object(pawn)]).unwrap();
    for _ in 0..10 {
        assert!(vm.tick_level(0.05).is_empty());
    }
    let halfway = [0, 1, 2].map(|i| (closed[i] + open[i]) / 2.0);
    assert!(distance(vector_of(&vm, mover, "Location", None), halfway) < 1.0, "{:?}", vector_of(&vm, mover, "Location", None));
    assert_eq!(vm.get(mover, "bInterpolating"), Some(Value::Bool(true)));
    let ticks = 10 + ticks_to_reach(&mut vm, mover, open, 40);
    assert!((20..=21).contains(&ticks), "{ticks} ticks");
    assert_eq!(vm.get(mover, "KeyNum"), Some(Value::Byte(1)));
    assert_eq!(vm.get(mover, "bClosed"), Some(Value::Bool(false)));

    assert!(vm.tick_level(0.05).is_empty());
    vm.event(mover, "Trigger", vec![Value::Object(Some(trigger)), Value::Object(pawn)]).unwrap();
    let ticks = ticks_to_reach(&mut vm, mover, closed, 40);
    assert!((20..=21).contains(&ticks), "{ticks} ticks");
    assert!(vm.tick_level(0.05).is_empty());
    assert_eq!(vm.get(mover, "bClosed"), Some(Value::Bool(true)));
    assert_eq!(vm.get(mover, "Velocity"), Some(Value::Vector(p2_math::Vector::ZERO)));
}

#[test]
fn police_movers_open_and_close() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("police").unwrap();
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    let (_, pawn) = vm.login("").unwrap().unwrap();
    let pawn = pawn.expect("no pawn");
    let find = |vm: &Vm<&Loader>, path: &str| vm.object_ids().find(|&o| vm.object(o).path == path).expect(path);

    let door = find(&vm, "police.Mover4");
    assert_eq!(vm.name(vm.state_name(door)), "TriggerOpenTimed");
    let base = vector_of(&vm, door, "BasePos", None);
    let key = |vm: &Vm<&Loader>, k| {
        let offset = vector_of(vm, door, "KeyPos", Some(k));
        [0, 1, 2].map(|i| base[i] + offset[i])
    };
    let (closed, open) = (key(&vm, 0), key(&vm, 2));
    let Some(Value::Float(stay)) = vm.get(door, "StayOpenTime") else { panic!("StayOpenTime") };
    vm.event(door, "Trigger", vec![Value::Object(Some(pawn)), Value::Object(Some(pawn))]).unwrap();
    let opened = ticks_to_reach(&mut vm, door, open, 100);
    assert_eq!(vm.get(door, "KeyNum"), Some(Value::Byte(2)));
    let closed_after = ticks_to_reach(&mut vm, door, closed, 400);
    assert!((40..=43).contains(&opened), "{opened} ticks");
    let expected = (stay / 0.05) as usize + 40;
    assert!(closed_after.abs_diff(expected) <= 3, "{closed_after} ticks, expected {expected}");
    assert_eq!(vm.get(door, "KeyNum"), Some(Value::Byte(0)));

    let trigger = find(&vm, "police.Trigger5");
    let lockers = [find(&vm, "police.Mover10"), find(&vm, "police.Mover11")];
    vm.call_native("SetLocation", Some(pawn), &mut [Value::Vector(vector_of(&vm, trigger, "Location", None).into())]).unwrap();
    for _ in 0..30 {
        assert!(vm.tick_level(0.05).is_empty());
    }
    for locker in lockers {
        let base = vector_of(&vm, locker, "BasePos", None);
        let offset = vector_of(&vm, locker, "KeyPos", Some(1));
        let open = [0, 1, 2].map(|i| base[i] + offset[i]);
        assert!(distance(vector_of(&vm, locker, "Location", None), open) < 0.01, "{} not open", vm.object(locker).path);
    }
}

#[test]
fn config_values_reach_class_defaults() {
    let Some(loader) = loader() else { return };
    let Some(dir) = game_dir() else { return };
    let system = ini(&dir, "Postal2.ini");
    let mut vm = Vm::new(&loader);
    vm.add_config("System", system.clone());
    vm.add_config("User", ini(&dir, "User.ini"));
    let expected_gore: i32 = system.get("Engine.GameInfo", "GoreLevel").unwrap().parse().unwrap();
    let expected_difficulty: f32 = system.get("Engine.GameInfo", "GameDifficulty").unwrap().parse().unwrap();
    for (package, class) in [("Engine", "GameInfo"), ("GameTypes", "AWPGameInfo")] {
        let class = vm.find_class(package, class).unwrap();
        assert_eq!(vm.default_value(class, "GoreLevel"), Some(Value::Int(expected_gore)));
        assert_eq!(vm.default_value(class, "GameDifficulty"), Some(Value::Float(expected_difficulty)));
    }
    let client = system.get("Engine.Engine", "ViewportManager").unwrap();
    let fullscreen = system.get(client, "StartupFullscreen").unwrap();
    assert_eq!(vm.get_command("ini:Engine.Engine.ViewportManager StartupFullscreen").unwrap(), fullscreen);
    let gri = vm.find_class("Engine", "GameReplicationInfo").unwrap();
    let server = system.get("Engine.GameReplicationInfo", "ServerName").unwrap().to_owned();
    assert_eq!(vm.default_value(gri, "ServerName"), Some(Value::Str(server)));
}

#[test]
fn church_hud_draws() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    loader.load_imports(map).unwrap();
    loader.load_imported(&["Texture"]).unwrap();
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    let (player, _) = vm.login("").unwrap().unwrap();
    for _ in 0..20 {
        assert!(vm.tick_level(0.05).is_empty());
    }
    let items = vm.render_hud(player, 1024.0, 768.0).unwrap();
    let title = items.iter().filter(|i| matches!(i, p2_vm::CanvasItem::Text { text, .. } if text == "Church and Cemetery")).count();
    assert_eq!(title, 2, "{items:?}");
    for item in &items {
        if let p2_vm::CanvasItem::Tile { size, .. } = item {
            assert!(size.iter().all(|s| s.is_finite() && *s > 0.0), "{item:?}");
        }
    }
}

fn float_of(vm: &Vm<&Loader>, object: ObjectId, path: &[&str]) -> f32 {
    match vm.get_path(object, path) {
        Some(Value::Float(value)) => value,
        other => panic!("{path:?} is {other:?}"),
    }
}

fn int_of(vm: &Vm<&Loader>, object: ObjectId, path: &[&str]) -> i32 {
    match vm.get_path(object, path) {
        Some(Value::Int(value)) => value,
        other => panic!("{path:?} is {other:?}"),
    }
}

#[test]
fn player_input_moves_the_pawn() {
    let Some(mut loader) = loader() else { return };
    let Some(dir) = game_dir() else { return };
    let user = ini(&dir, "User.ini");
    let map = loader.load("Church").unwrap();
    let mut vm = Vm::new(&loader);
    vm.add_config("System", ini(&dir, "Postal2.ini"));
    vm.add_config("User", user.clone());
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    let (player, pawn) = vm.login("").unwrap().unwrap();
    let pawn = pawn.expect("no pawn");
    assert_eq!(vm.player(), Some(player));
    let Some(Value::Object(Some(input))) = vm.get(player, "PlayerInput") else { panic!("no PlayerInput") };
    assert_eq!(vm.get(input, "Outer"), Some(Value::Object(Some(player))));
    for _ in 0..40 {
        assert!(vm.tick_level(0.05).is_empty());
    }
    assert_eq!(vm.name(vm.state_name(player)), "PlayerWalking");

    let bindings = user.section("Engine.Input");
    let key = |alias: &str| {
        bindings.iter().find(|(k, v)| v.eq_ignore_ascii_case(alias) && !k.starts_with("Aliases")).map(|(k, _)| k.clone()).unwrap()
    };
    let speed = |alias: &str| -> f32 {
        let (_, command) = bindings.iter().find(|(k, v)| k.starts_with("Aliases") && v.contains(&format!("\"{alias}\""))).unwrap();
        let at = command.find("Speed=").unwrap() + "Speed=".len();
        command[at..].split(['"', ' ']).next().unwrap().parse().unwrap()
    };
    let tick = |vm: &mut Vm<&Loader>, keys: &[&str], mouse: [f32; 2]| {
        vm.player_input(keys, mouse).unwrap();
        let errors = vm.tick_level(0.05);
        assert!(errors.is_empty(), "{:?}", errors.iter().map(|(a, e)| format!("{a}: {e}")).collect::<Vec<_>>());
    };
    let axes = |vm: &Vm<&Loader>| {
        let angle = |member| int_of(vm, pawn, &["Rotation", member]) as f32 * std::f32::consts::TAU / 65536.0;
        let (pitch, yaw) = (angle("Pitch"), angle("Yaw"));
        ([pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin()], [-yaw.sin(), yaw.cos()])
    };
    let acceleration = |vm: &Vm<&Loader>| vector_of(vm, pawn, "Acceleration", None);

    let yaw = |vm: &Vm<&Loader>| int_of(vm, player, &["Rotation", "Yaw"]);
    let before = yaw(&vm);
    tick(&mut vm, &[], [100.0, 0.0]);
    let sensitivity = float_of(&vm, input, &["MouseSensitivity"]);
    let fov = float_of(&vm, player, &["DesiredFOV"]);
    let binding = user.get("Engine.Input", "MouseX").unwrap();
    let mouse_speed: f32 = binding[binding.find("Speed=").unwrap() + "Speed=".len()..].split_whitespace().next().unwrap().parse().unwrap();
    let expected = 32.0 * 100.0 * 0.01 * mouse_speed * 20.0 * sensitivity * fov * 0.01111;
    let turned = (yaw(&vm) - before) as f32;
    assert!((turned - expected).abs() <= 1.0 && turned > 1000.0, "{turned} != {expected}");
    assert_eq!(int_of(&vm, pawn, &["Rotation", "Yaw"]), yaw(&vm), "the pawn faces the view");

    for (alias, axis) in [("MoveForward", 0), ("MoveBackward", 0), ("StrafeLeft", 1), ("StrafeRight", 1)] {
        let key = key(alias);
        for _ in 0..5 {
            tick(&mut vm, &[&key], [0.0, 0.0]);
        }
        let (x, y) = axes(&vm);
        let direction = if axis == 0 { x } else { y };
        let expected = [direction[0] * 20.0 * speed(alias), direction[1] * 20.0 * speed(alias), 0.0];
        let got = acceleration(&vm);
        assert!(distance(got, expected) < 1.0, "{alias}: {got:?} != {expected:?}");
        tick(&mut vm, &[], [0.0, 0.0]);
        assert!(distance(acceleration(&vm), [0.0; 3]) < 1e-3);
    }

    let duck = key("Duck");
    tick(&mut vm, &[&duck], [0.0, 0.0]);
    assert_eq!(vm.get(player, "bDuck"), Some(Value::Byte(1)));
    tick(&mut vm, &[], [0.0, 0.0]);
    assert_eq!(vm.get(player, "bDuck"), Some(Value::Byte(0)));
    let jump = "Space";
    assert!(user.get("Engine.Input", jump).unwrap().starts_with("Jump"));
    tick(&mut vm, &[jump], [0.0, 0.0]);
    assert!(vector_of(&vm, pawn, "Velocity", None)[2] > 0.0);
    let forward = key("MoveForward");
    for frame in 0..400 {
        let keys: &[&str] = if frame % 40 < 30 { &[&forward] } else { &[] };
        tick(&mut vm, keys, [(frame % 7) as f32 - 3.0, (frame % 5) as f32 - 2.0]);
    }
}

#[test]
fn startup_client_interactions() {
    let Some(mut loader) = loader() else { return };
    let Some(dir) = game_dir() else { return };
    let map = loader.load("Startup").unwrap();
    let mut vm = Vm::new(&loader);
    vm.add_config("System", ini(&dir, "Postal2.ini"));
    vm.add_config("User", ini(&dir, "User.ini"));
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    vm.init_client().unwrap();
    let viewport = vm.viewport().expect("no viewport");
    let Some(Value::Object(Some(master))) = vm.get(viewport, "InteractionMaster") else { panic!("no master") };
    let class_of = |vm: &Vm<&Loader>, property: &str| match vm.get(master, property) {
        Some(Value::Object(Some(o))) => vm.name(vm.object(o).class_name).to_owned(),
        other => format!("{other:?}"),
    };
    assert_eq!(class_of(&vm, "Console"), "FPSConsoleExt");
    assert_eq!(class_of(&vm, "BaseMenu"), "ShellRootWindow");
    let (player, _) = vm.login("").unwrap().unwrap();
    assert_eq!(vm.get(player, "Player"), Some(Value::Object(Some(viewport))));
    for frame in 0..200 {
        let errors = vm.tick_level(0.05);
        assert!(errors.is_empty(), "frame {frame}: {:?}", errors.iter().map(|(a, e)| format!("{a}: {e}")).collect::<Vec<_>>());
        vm.render_hud(player, 1024.0, 768.0).unwrap();
    }
    let scenes: Vec<_> = vm.object_ids().filter(|&o| vm.is_a(o, "SceneManager")).collect();
    let started: Vec<String> = scenes.iter().filter(|&&s| vm.get(s, "bIsRunning") == Some(Value::Bool(true)) || vm.get(s, "bIsSceneStarted") == Some(Value::Bool(true))).map(|&s| format!("{:?}", vm.get(s, "Tag"))).collect();
    assert!(!started.is_empty() && vm.get(player, "bInterpolating") == Some(Value::Bool(true)), "intro not running: {started:?}");
    for frame in 0..40 {
        let keys: &[&str] = if frame % 20 == 0 { &["Escape"] } else { &[] };
        vm.player_input(keys, [3.0, -2.0]).unwrap();
        let errors = vm.tick_level(0.05);
        assert!(errors.is_empty(), "{:?}", errors.iter().map(|(a, e)| format!("{a}: {e}")).collect::<Vec<_>>());
        vm.render_hud(player, 1024.0, 768.0).unwrap();
    }
    let Some(Value::Object(Some(menu))) = vm.get(master, "BaseMenu") else { panic!("no base menu") };
    vm.event(menu, "ShowMenu", Vec::new()).unwrap();
    assert!(vm.name(vm.state_name(menu)).ends_with("MenuShowingVirgin"), "in {}", vm.name(vm.state_name(menu)));
    let Some(Value::Object(Some(main))) = vm.get(menu, "MyMenu") else { panic!("no MyMenu") };
    assert_eq!(vm.name(vm.object(main).class_name), "MenuMain");
    assert!(vm.shows_mouse(), "the menu wants the system cursor");
    vm.set_mouse_position(512.0, 384.0);
    vm.typed("Dude 1").unwrap();
    let mut items = Vec::new();
    for _ in 0..20 {
        vm.client_tick(0.05).unwrap();
        items = vm.render_hud(player, 1024.0, 768.0).unwrap();
    }
    let texts: Vec<&str> = items.iter().filter_map(|i| if let p2_vm::CanvasItem::Text { text, .. } = i { Some(text.as_str()) } else { None }).collect();
    let open_menu = |vm: &Vm<&Loader>| match vm.get(menu, "MyMenu") {
        Some(Value::Object(Some(m))) => m,
        _ => panic!("no menu open"),
    };
    let click = |vm: &mut Vm<&Loader>, window: ObjectId, field: &str| {
        let Some(Value::Object(Some(choice))) = vm.get(window, field) else { panic!("no {field}") };
        vm.event(window, "Notify", vec![Value::Object(Some(choice)), Value::Byte(2)]).unwrap();
    };
    let class_of = |vm: &Vm<&Loader>, o: ObjectId| vm.name(vm.object(o).class_name).to_owned();
    click(&mut vm, main, "NewChoice");
    let modes = open_menu(&vm);
    assert_eq!(class_of(&vm, modes), "MenuGameMode");
    click(&mut vm, modes, "P2Choice");
    let start = open_menu(&vm);
    assert_eq!(class_of(&vm, start), "MenuStart_P2");
    click(&mut vm, start, "StartMF");
    let keys = open_menu(&vm);
    assert_eq!(class_of(&vm, keys), "MenuImageKeys");
    let image = vm.render_hud(player, 1024.0, 768.0).unwrap().into_iter().find_map(|i| match i {
        p2_vm::CanvasItem::Tile { material, uv, .. } if material.ends_with("Vital_Keys") => Some(uv),
        _ => None,
    });
    assert!(image.is_some_and(|uv| uv[2] > 0.0 && uv[3] > 0.0), "keys image {image:?}");
    click(&mut vm, keys, "StartChoice");
    assert!(vm.diagnostics.log.iter().any(|l| l.contains("SendPlayerTo")), "start sends the player");
    let row = |label: &str| {
        items.iter().find_map(|i| match i {
            p2_vm::CanvasItem::Text { text, position, .. } if text == label => Some(position[1]),
            _ => None,
        })
    };
    let rows: Vec<f32> = ["New Game", "Load Game", "Options", "Exit"].iter().filter_map(|label| row(label)).collect();
    assert!(rows.len() == 4 && rows.windows(2).all(|w| w[1] - w[0] > 20.0), "rows {rows:?}");
    assert!(texts.contains(&"Exit") && texts.contains(&"Options"), "{texts:?}");
    let ask = |vm: &mut Vm<&Loader>, command: &str| match vm.call_native("ConsoleCommand", Some(player), &mut [Value::Str(command.to_owned())]).unwrap() {
        Some(Value::Str(answer)) => answer,
        other => panic!("{command}: {other:?}"),
    };
    assert_eq!(ask(&mut vm, "BINDING2KEYSTR \"Fire\" 0"), "LeftMouse");
    assert_eq!(ask(&mut vm, "KEYNO LeftMouse"), "1");
    assert_eq!(ask(&mut vm, "KEYNAME 1"), "LeftMouse");
    assert!(!ask(&mut vm, "MOVEBIND 0").is_empty());
    let game_info = vm.find_class("Postal2Game", "P2GameInfo").unwrap();
    assert_eq!(vm.default_value(game_info, "bLocalizedDialog"), Some(Value::Bool(false)));
    let exit_font = items.iter().find_map(|i| match i {
        p2_vm::CanvasItem::Text { text, font, .. } if text == "Exit" => Some(font.clone()),
        _ => None,
    });
    assert!(exit_font.as_ref().is_some_and(Option::is_some), "Exit font {exit_font:?}");
    let logo = items.iter().find_map(|i| match i {
        p2_vm::CanvasItem::Tile { material, size, .. } if material.ends_with("postal2underlined") => Some(*size),
        _ => None,
    });
    assert!(logo.is_some_and(|[w, h]| w > 0.0 && h > 0.0), "logo {logo:?}");
}

#[test]
fn church_player_walks_by_vm_physics() {
    let Some(mut loader) = loader() else { return };
    let Some(dir) = game_dir() else { return };
    let user = ini(&dir, "User.ini");
    let map = loader.load("Church").unwrap();
    let mut vm = Vm::new(&loader);
    vm.add_config("System", ini(&dir, "Postal2.ini"));
    vm.add_config("User", user.clone());
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    vm.init_client().unwrap();
    let (_, pawn) = vm.login("").unwrap().unwrap();
    let pawn = pawn.expect("no pawn");
    vm.options.player_physics = true;
    let forward = user
        .section("Engine.Input")
        .iter()
        .find(|(k, v)| v.eq_ignore_ascii_case("MoveForward") && !k.starts_with("Aliases"))
        .map(|(k, _)| k.clone())
        .unwrap();
    let tick = |vm: &mut Vm<&Loader>, keys: &[&str]| {
        vm.player_input(keys, [0.0, 0.0]).unwrap();
        let errors = vm.tick_level(0.05);
        assert!(errors.is_empty(), "{:?}", errors.iter().map(|(a, e)| format!("{a}: {e}")).collect::<Vec<_>>());
    };
    for _ in 0..40 {
        tick(&mut vm, &[]);
    }
    let Rotator { yaw: yaw_before, .. } = vm.player_view().unwrap().expect("no view").rotation;
    vm.player_input(&[], [200.0, 0.0]).unwrap();
    tick(&mut vm, &[]);
    let Rotator { yaw: yaw_after, .. } = vm.player_view().unwrap().expect("no view").rotation;
    assert_ne!(yaw_before, yaw_after, "mouse look");
    let start = vector_of(&vm, pawn, "Location", None);
    let p2_vm::PlayerView { actor: view_actor, location: camera, .. } = vm.player_view().unwrap().expect("no view");
    assert_eq!(view_actor, Some(pawn));
    assert!((camera[0] - start[0]).hypot(camera[1] - start[1]) < 1.0 && camera[2] > start[2], "camera {camera:?} at pawn {start:?}");
    for _ in 0..40 {
        tick(&mut vm, &[&forward]);
    }
    let walked = vector_of(&vm, pawn, "Location", None);
    let flat = (walked[0] - start[0]).hypot(walked[1] - start[1]);
    assert!(flat > 100.0, "walked {flat}");
    assert!((walked[2] - start[2]).abs() < 40.0, "height {} -> {}", start[2], walked[2]);
    tick(&mut vm, &["Space"]);
    assert!(vector_of(&vm, pawn, "Velocity", None)[2] > 0.0);
    for _ in 0..40 {
        tick(&mut vm, &[]);
    }
    assert_eq!(vm.get(pawn, "Physics"), Some(Value::Byte(1)), "walking again after the jump");
    let standing = (vm.get(pawn, "CollisionHeight"), vector_of(&vm, pawn, "Location", None)[2]);
    for _ in 0..10 {
        tick(&mut vm, &["Ctrl"]);
    }
    assert_eq!(vm.get(pawn, "bIsCrouched"), Some(Value::Bool(true)), "crouched");
    assert_eq!(vm.get(pawn, "CollisionHeight"), vm.get(pawn, "CrouchHeight"));
    for _ in 0..10 {
        tick(&mut vm, &[]);
    }
    assert_eq!(vm.get(pawn, "bIsCrouched"), Some(Value::Bool(false)), "stood up");
    assert_eq!(vm.get(pawn, "CollisionHeight"), standing.0);
    assert!((vector_of(&vm, pawn, "Location", None)[2] - standing.1).abs() < 3.0);
}

#[test]
fn church_traces_hit_the_level_and_pawns() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    loader.load_imports(map).unwrap();
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    let start = vm.object_ids().find(|&o| vm.is_a(o, "PlayerStart")).unwrap();
    let at = vector_of(&vm, start, "Location", None);
    let hit = vm.trace_world(at.into(), [at[0], at[1], at[2] - 1000.0].into(), [0.0; 3].into()).expect("floor below the start");
    assert!(hit.normal[2] > 0.7 && at[2] - hit.location[2] < 200.0);
    let pawns: Vec<_> = vm.object_ids().filter(|&o| vm.is_a(o, "Pawn") && !vm.is_deleted(o)).collect();
    let mut tested = 0;
    for pawn in pawns {
        let p = vector_of(&vm, pawn, "Location", None);
        for from in [[p[0] - 300.0, p[1], p[2]], [p[0] + 300.0, p[1], p[2]], [p[0], p[1] - 300.0, p[2]], [p[0], p[1] + 300.0, p[2]]] {
            if vm.trace_world(from.into(), p.into(), [0.0; 3].into()).is_some() {
                continue;
            }
            let vector = |v: [f32; 3]| Value::Vector(v.into());
            let mut args = vec![Value::Map, Value::Map, vector(p), vector(from), Value::Bool(true), Value::Map, Value::Map];
            let hit = vm.call_native("Trace", None, &mut args).unwrap();
            assert!(matches!(hit, Some(Value::Object(Some(o))) if vm.is_a(o, "Actor") && !vm.is_a(o, "LevelInfo")), "{hit:?}");
            tested += 1;
            break;
        }
    }
    assert!(tested > 10);
}

#[test]
fn server_travel_reaches_the_host() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    vm.login("").unwrap().unwrap();
    for _ in 0..20 {
        assert!(vm.tick_level(0.05).is_empty());
    }
    let level = vm.level().unwrap();
    vm.event(level, "ServerTravel", vec![Value::Str("Bridge".to_owned()), Value::Bool(false)]).unwrap();
    for _ in 0..5 {
        assert!(vm.tick_level(0.05).is_empty());
    }
    let travel = take_travel(&mut vm);
    assert!(travel.as_deref().is_some_and(|url| url.starts_with("Bridge")), "{travel:?}");
}

#[test]
fn hurt_radius_reaches_nearby_actors() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    let pawn = vm.object_ids().find(|&o| vm.is_a(o, "Pawn") && !vm.is_deleted(o) && vm.object(o).path.contains("Bystander")).unwrap();
    let at = vector_of(&vm, pawn, "Location", None);
    let vector = |v: [f32; 3]| Value::Vector(v.into());
    let exploded = Value::Object(Some(vm.stub("BaseFX.ExplodedDamage", "Class")));
    let blast = |vm: &mut Vm<&Loader>, centre: [f32; 3]| {
        vm.diagnostics.trace = Some(Vec::new());
        let mut args = vec![Value::Float(50.0), Value::Float(200.0), exploded.clone(), Value::Float(1000.0), vector(centre)];
        vm.call_native("HurtRadius", vm.level(), &mut args).unwrap();
        vm.diagnostics.trace.take().unwrap().iter().filter(|f| *f == "TakeDamage").count()
    };
    let far = blast(&mut vm, [at[0], at[1], at[2] + 100_000.0]);
    let near = blast(&mut vm, [at[0] + 30.0, at[1], at[2]]);
    assert_eq!(far, 0);
    assert!(near > 0);
}

fn church_with_ai(loader: &mut Loader) -> Vm<&Loader> {
    let map = loader.load("Church").unwrap();
    loader.load_imports(map).unwrap();
    load_animations(loader);
    let dir = game_dir().unwrap();
    let mut vm = Vm::new(&*loader);
    vm.add_config("System", ini(&dir, "Postal2.ini"));
    vm.add_config("User", ini(&dir, "User.ini"));
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    vm.login("").unwrap().unwrap();
    vm
}

fn route_costs(edges: &std::collections::HashMap<ObjectId, Vec<(ObjectId, i32)>>, start: ObjectId) -> std::collections::HashMap<ObjectId, i32> {
    let mut costs = std::collections::HashMap::from([(start, 0)]);
    let mut done = std::collections::HashSet::new();
    while let Some((node, cost)) = costs.iter().filter(|(n, _)| !done.contains(*n)).min_by_key(|&(_, &c)| c).map(|(&n, &c)| (n, c)) {
        done.insert(node);
        for &(end, edge) in edges.get(&node).into_iter().flatten() {
            if costs.get(&end).is_none_or(|&c| cost + edge < c) {
                costs.insert(end, cost + edge);
            }
        }
    }
    costs
}

#[test]
fn church_find_path_toward_takes_a_cheapest_route() {
    let Some(mut loader) = loader() else { return };
    let mut vm = church_with_ai(&mut loader);
    let mut navs = Vec::new();
    let mut current = object_of(&vm, vm.level().unwrap(), "NavigationPointList");
    while let Some(nav) = current {
        navs.push(nav);
        current = object_of(&vm, nav, "nextNavigationPoint");
    }
    let controller = vm
        .object_ids()
        .find(|&o| vm.is_a(o, "Controller") && !vm.is_a(o, "PlayerController") && object_of(&vm, o, "Pawn").is_some_and(|p| vm.is_a(p, "Bystander") || vm.is_a(p, "Pawn")))
        .unwrap();
    let pawn = object_of(&vm, controller, "Pawn").unwrap();
    let flag = |vm: &Vm<&Loader>, o: ObjectId, name: &str| vm.get(o, name) == Some(Value::Bool(true));
    let crouch = flag(&vm, pawn, "bCanCrouch");
    let size = |name: &str, crouched: &str| float_of(&vm, pawn, &[if crouch { crouched } else { name }]) as i32;
    let (radius, height) = (size("CollisionRadius", "CrouchRadius"), size("CollisionHeight", "CrouchHeight"));
    let moves = [("bCanWalk", 1), ("bCanFly", 2), ("bCanSwim", 4), ("bCanJump", 8), ("bCanClimbLadders", 64)]
        .iter()
        .filter(|(name, _)| flag(&vm, pawn, name))
        .fold(256, |flags, (_, bit)| flags | bit)
        | [("bCanOpenDoors", 16), ("bCanDoSpecial", 32), ("bIsPlayer", 128)].iter().filter(|(n, _)| flag(&vm, controller, n)).fold(0, |f, (_, b)| f | b);
    let max_fall = float_of(&vm, pawn, &["MaxFallSpeed"]) as i32;
    let mut edges: std::collections::HashMap<ObjectId, Vec<(ObjectId, i32)>> = std::collections::HashMap::new();
    let mut specs = 0;
    for &nav in &navs {
        let Some(Value::Array(list)) = vm.get(nav, "PathList") else { continue };
        for spec in list {
            let Value::Object(Some(spec)) = spec else { panic!("{spec:?}") };
            specs += 1;
            assert_eq!(object_of(&vm, spec, "Start"), Some(nav));
            let end = object_of(&vm, spec, "End").unwrap();
            assert!(vm.is_a(end, "NavigationPoint"));
            let int = |name: &str| int_of(&vm, spec, &[name]);
            let fits = int("CollisionRadius") >= radius && int("CollisionHeight") >= height && int("reachFlags") & moves == int("reachFlags") && int("MaxLandingVelocity") <= max_fall;
            if fits && !flag(&vm, end, "bBlocked") && !flag(&vm, nav, "bBlocked") {
                edges.entry(nav).or_default().push((end, int("Distance") + int_of(&vm, end, &["ExtraCost"])));
            }
        }
    }
    assert!(navs.len() > 100 && specs > navs.len());

    let hops = |start: ObjectId| {
        let mut depth = std::collections::HashMap::from([(start, 0)]);
        let mut queue = std::collections::VecDeque::from([start]);
        while let Some(node) = queue.pop_front() {
            for &(end, _) in edges.get(&node).into_iter().flatten() {
                if !depth.contains_key(&end) {
                    depth.insert(end, depth[&node] + 1);
                    queue.push_back(end);
                }
            }
        }
        depth
    };
    let (start, goal) = navs
        .iter()
        .filter(|&&n| vm.is_a(n, "PathNode"))
        .find_map(|&n| hops(n).into_iter().filter(|&(_, d)| d >= 3).max_by_key(|&(_, d)| d).map(|(goal, _)| (n, goal)))
        .unwrap();
    let at = vector_of(&vm, start, "Location", None);
    let lift = float_of(&vm, pawn, &["CollisionHeight"]) - float_of(&vm, start, &["CollisionHeight"]);
    vm.set_location(pawn, [at[0], at[1], at[2] + lift].into()).unwrap();
    vm.set(pawn, "Anchor", Value::Object(Some(start)));
    let mut args = vec![Value::Object(Some(goal)), Value::Map];
    let Some(Value::Object(Some(next))) = vm.call_native("FindPathToward", Some(controller), &mut args).unwrap() else { panic!("no path") };
    let from_start = route_costs(&edges, start);
    let from_next = route_costs(&edges, next);
    let step = edges[&start].iter().filter(|&&(end, _)| end == next).map(|&(_, cost)| cost).min();
    assert_eq!(step.zip(from_next.get(&goal)).map(|(a, b)| a + b), Some(from_start[&goal]));
    assert_eq!(object_of(&vm, controller, "RouteGoal"), Some(goal));
}

#[test]
fn church_ai_pawns_walk() {
    let Some(mut loader) = loader() else { return };
    let mut vm = church_with_ai(&mut loader);
    let player_pawn = vm.player().and_then(|p| object_of(&vm, p, "Pawn"));
    let pawns: Vec<ObjectId> = vm.object_ids().filter(|&o| vm.is_a(o, "Pawn") && !vm.is_deleted(o) && Some(o) != player_pawn).collect();
    let starts: Vec<[f32; 3]> = pawns.iter().map(|&p| vector_of(&vm, p, "Location", None)).collect();
    let mut grouped = std::collections::BTreeMap::new();
    for _ in 0..1200 {
        for &pawn in &pawns {
            if !vm.is_deleted(pawn) {
                vm.mark_rendered(pawn).unwrap();
            }
        }
        for (_, error) in vm.tick_level(0.05) {
            *grouped.entry(format!("{error}")).or_insert(0) += 1;
        }
    }
    assert!(grouped.is_empty(), "{grouped:#?}");
    let mut walked = 0;
    for (&pawn, &start) in pawns.iter().zip(&starts) {
        if vm.is_deleted(pawn) || vm.get(pawn, "Physics") != Some(Value::Byte(1)) {
            continue;
        }
        let at = vector_of(&vm, pawn, "Location", None);
        let height = float_of(&vm, pawn, &["CollisionHeight"]);
        let floor = vm.trace_world(at.into(), [at[0], at[1], at[2] - 1000.0].into(), [0.0; 3].into()).map(|hit| at[2] - height - hit.location[2]);
        let moved = (at[0] - start[0]).hypot(at[1] - start[1]);
        assert!(floor.is_some_and(|f| (-1.0..40.0).contains(&f)), "{} is not on the ground", vm.object(pawn).path);
        walked += usize::from(moved > 500.0);
    }
    assert!(walked >= 3);
}
#[test]
fn get_game_map_steps_through_multiplayer_maps() {
    let Some(loader) = loader() else { return };
    let mut vm = Vm::new(&loader);
    let mut game_map = |code: &str, previous: &str, dir: i32| {
        let mut args = [Value::Str(code.into()), Value::Str(previous.into()), Value::Int(dir)];
        match vm.call_native("GetGameMap", None, &mut args).unwrap() {
            Some(Value::Str(map)) => map,
            other => panic!("GetGameMap returned {other:?}"),
        }
    };
    assert_eq!(game_map("DGT", "", 0), "MPDGT-Asylum.fuk");
    assert_eq!(game_map("DGT", "MPDGT-Asylum.fuk", 1), "MPDGT-Boneyard.fuk");
    assert_eq!(game_map("DGT", "MPDGT-Trainyard.fuk", 1), "MPDGT-Asylum.fuk");
    assert_eq!(game_map("DGT", "MPDGT-Asylum.fuk", -1), "MPDGT-Trainyard.fuk");
    assert_eq!(game_map("S", "MPS-Intifada.fuk", 1), "MPS-Ponderous.fuk");
}

#[test]
fn church_fire_emitters() {
    let Some(mut loader) = loader() else { return };
    let mut vm = church_with_ai(&mut loader);
    let player_pawn = vm.player().and_then(|p| object_of(&vm, p, "Pawn"));
    let pawn = vm
        .object_ids()
        .find(|&o| vm.is_a(o, "FPSPawn") && !vm.is_deleted(o) && Some(o) != player_pawn)
        .expect("a pawn");
    let at = vector_of(&vm, pawn, "Location", None);
    let location = Value::Vector(at.into());

    let woof_class = vm.find_class("FX", "SizeableWoof").unwrap();
    let woof = vm.spawn(woof_class, None, [Some(location.clone()), None]).unwrap().expect("woof");
    let Some(Value::Array(emitters)) = vm.get(woof, "Emitters") else { panic!("Emitters") };
    let Value::Object(Some(emitter)) = emitters[0] else { panic!("Emitters[0]") };
    let set_size = vm.find_function(woof_class, "SetSize").unwrap().unwrap();
    vm.call(&set_size, Some(woof), woof_class, vec![Value::Float(100.0)]).unwrap();
    assert!(!vm.diagnostics.missing_natives.contains_key("SetMaxParticles"), "{:?}", vm.diagnostics.missing_natives);
    let Some(Value::Int(max)) = vm.get(emitter, "MaxParticles") else { panic!("MaxParticles") };
    assert_ne!(max, 15, "the default");
    assert_eq!(vm.get(emitter, "InitialParticlesPerSecond"), Some(Value::Float(4.0 * max as f32)));
    assert!(take_particles(&mut vm).contains(&ParticleCommand::Reset(emitter, ())));

    let vec = |v: [f32; 3]| Value::Vector(v.into());
    vm.call_native("SetProjectionNormal", Some(emitter), &mut [vec([0.0, 0.0, 2.0])]).unwrap();
    assert_eq!(vector_of(&vm, emitter, "ProjectionNormal", None), [0.0, 0.0, 2.0]);
    assert_eq!(vector_of(&vm, emitter, "RealProjectionNormal", None), [0.0, 0.0, 1.0]);
    let line_start = vector_of(&vm, emitter, "LineStart", None);
    vm.call_native("SpawnParticleLength", Some(emitter), &mut [vec([0.0; 3]), vec([100.0, 0.0, 0.0]), Value::Float(30.0)]).unwrap();
    assert_eq!(take_particles(&mut vm).iter().filter(|command| matches!(command, ParticleCommand::Spawn { .. })).count(), 3);
    assert_eq!(vector_of(&vm, emitter, "StartLocationOffset", None), [60.0, 0.0, 0.0]);
    assert_eq!(vector_of(&vm, emitter, "LineStart", None), [line_start[0] + 90.0, line_start[1], line_start[2]]);
    assert_eq!(vector_of(&vm, emitter, "LineEnd", None), [100.0, 0.0, 0.0]);

    let emitter_class = vm.find_class("BaseFX", "P2Emitter").unwrap();
    let fire = vm.spawn(emitter_class, None, [Some(location), None]).unwrap().expect("fire");
    let streak = vm.find_class("FX", "FireStreak").unwrap();
    let damage_type = vm.default_value(streak, "MyDamageType").unwrap();
    let health = vm.get(pawn, "Health");
    let mut args = [
        Value::Float(20.0),
        Value::Float(500.0),
        damage_type,
        vec(at),
        vec([at[0] + 50.0, at[1], at[2]]),
        Value::Float(10.0),
        Value::Float(10.0),
    ];
    vm.call_native("FireHurtRadius", Some(fire), &mut args).unwrap();
    assert_ne!(vm.get(pawn, "Health"), health, "the pawn took damage");
}

#[test]
fn startup_runs_matinee_scenes() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Startup").unwrap();
    load_animations(&mut loader);
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    let scenes: Vec<ObjectId> = vm.object_ids().filter(|&o| vm.is_a(o, "SceneManager")).collect();
    assert_eq!(scenes.len(), 5);
    for &scene in &scenes {
        let total = float_of(&vm, scene, &["TotalSceneTime"]);
        assert!(total > 0.0, "{} has no length", vm.object(scene).path);
    }
    let look_target = vm.object_ids().find(|&o| vm.is_a(o, "LookTarget")).expect("no LookTarget");
    let (player, pawn) = vm.login("").unwrap().expect("no player controller");
    let first = scenes.iter().copied().find(|&s| matches!(vm.get(s, "Tag"), Some(Value::Name(n)) if vm.name(n) == "StartFirstTime"));
    let first = first.expect("no StartFirstTime scene");
    let trigger = vm.intern("StartFirstTime");
    vm.event(first, "TriggerEvent", vec![Value::Name(trigger), Value::Object(Some(player)), Value::Object(pawn)]).unwrap();
    let mut started = Vec::new();
    let mut grouped = std::collections::BTreeMap::new();
    let (mut travelled, mut last, mut on_path, mut looking, mut titles, mut fading) = (0.0, None, 0, 0, 0, 0);
    for _ in 0..600 {
        for (_, error) in vm.tick_level(0.05) {
            *grouped.entry(format!("{error}")).or_insert(0) += 1;
        }
        let Some(scene) = scenes.iter().copied().find(|&s| vm.get(s, "bIsSceneStarted") == Some(Value::Bool(true))) else { continue };
        assert_eq!(object_of(&vm, scene, "Viewer"), Some(player));
        if started.last() != Some(&scene) {
            started.push(scene);
        }
        let at = vector_of(&vm, player, "Location", None);
        if let Some(last) = last {
            travelled += distance(at, last);
        }
        last = Some(at);
        let action = object_of(&vm, scene, "CurrentAction").expect("no current action");
        let Some(Value::Array(samples)) = vm.get(action, "SampleLocations") else { panic!("no samples") };
        let samples: Vec<[f32; 3]> = (0..samples.len()).map(|i| vector_of(&vm, action, "SampleLocations", Some(i))).collect();
        let step = samples.windows(2).map(|w| distance(w[0], w[1])).fold(0.0, f32::max);
        let nearest = samples.iter().map(|&s| distance(s, at)).fold(f32::MAX, f32::min);
        on_path += usize::from(nearest <= step + 0.1);
        let target = vector_of(&vm, look_target, "Location", None);
        let yaw = (target[1] - at[1]).atan2(target[0] - at[0]) * 65535.0 / std::f32::consts::TAU;
        let rotation = int_of(&vm, player, &["Rotation", "Yaw"]) as f32;
        let off = (rotation - yaw).rem_euclid(65536.0);
        looking += usize::from(off.min(65536.0 - off) < 3.0);
        titles += usize::from(matches!(vm.get(player, "CameraEffects"), Some(Value::Array(effects)) if !effects.is_empty()));
        fading += usize::from(float_of(&vm, player, &["FlashScale", "X"]) < 0.99);
    }
    let order: Vec<&str> = started.iter().map(|&s| vm.object(s).path.as_str()).collect();
    assert!(grouped.is_empty(), "{grouped:#?}");
    assert_eq!(order, ["Startup.SceneManager2", "Startup.SceneManager3", "Startup.SceneManager1", "Startup.SceneManager0"]);
    assert!(travelled > 1000.0);
    assert!(on_path >= 595, "{on_path}");
    assert!(looking >= 595, "{looking}");
    assert!(titles > 50 && fading > 20, "{titles} {fading}");
    assert_eq!(vm.get(player, "Pawn"), Some(Value::Object(None)));
}
#[test]
fn church_karma_props_rest_and_get_pushed() {
    let Some(mut loader) = loader() else { return };
    let map = loader.load("Church").unwrap();
    loader.load_imports(map).unwrap();
    let mut vm = Vm::new(&loader);
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    assert!(vm.tick_level(0.05).is_empty());
    let mut props: Vec<ObjectId> = vm.karma_actors().collect();
    props.sort_by_key(|&o| vm.object(o).path.clone());
    assert!(props.len() > 40, "{} bodies", props.len());
    let start: Vec<[f32; 3]> = props.iter().map(|&p| vector_of(&vm, p, "Location", None)).collect();
    for _ in 0..20 {
        assert!(vm.tick_level(0.05).is_empty());
    }
    let still = props.iter().zip(&start).filter(|&(&p, s)| vector_of(&vm, p, "Location", None) == *s).count();
    let enabled = props.iter().filter(|&&p| object_of(&vm, p, "KParams").is_some_and(|k| vm.get(k, "KStartEnabled") == Some(Value::Bool(true)))).count();
    assert!(still + enabled >= props.len());
    for &prop in &props {
        if let Some(params) = object_of(&vm, prop, "KParams") {
            vm.set(params, "KImpactThreshold", Value::Float(10.0));
        }
        vm.call_native("KWake", Some(prop), &mut []).unwrap();
    }
    take_sounds(&mut vm);
    for _ in 0..100 {
        assert!(vm.tick_level(0.05).is_empty());
    }
    assert!(take_sounds(&mut vm).into_iter().any(|s| s.actor.is_some_and(|a| props.contains(&a))));
    let moved: Vec<(String, [f32; 3])> = props
        .iter()
        .zip(&start)
        .map(|(&p, s)| {
            let at = vector_of(&vm, p, "Location", None);
            (vm.object(p).path.clone(), [0, 1, 2].map(|i| at[i] - s[i]))
        })
        .filter(|(_, d)| d.iter().any(|c| c.abs() > 4.0))
        .collect();
    let fell: Vec<_> = props
        .iter()
        .zip(&start)
        .filter_map(|(&p, &s)| {
            let floor = vm.trace_world(s.into(), [s[0], s[1], s[2] - 2000.0].into(), [0.0; 3].into())?.location[2];
            let z = vector_of(&vm, p, "Location", None)[2];
            (z < floor - 8.0).then(|| (vm.object(p).path.clone(), floor, z))
        })
        .collect();
    assert!(fell.is_empty(), "below the floor: {fell:?}");
    assert!(moved.len() * 10 < props.len(), "{} moved", moved.len());

    let vector = |v: [f32; 3]| Value::Vector(v.into());
    let plain: Vec<ObjectId> = props.iter().copied().filter(|&p| vm.name(vm.object(p).class_name) == "KActor").collect();
    let (shot, blasted) = (plain[0], plain[plain.len() / 2]);
    let at = vector_of(&vm, shot, "Location", None);
    let damage = vm.stub("BaseFX.ExplodedDamage", "Class");
    let args = vec![Value::Int(10), Value::Object(None), vector(at), vector([0.0, 0.0, 30_000.0]), Value::Object(Some(damage))];
    vm.event(shot, "TakeDamage", args).unwrap();
    let at = vector_of(&vm, blasted, "Location", None);
    let mut args = vec![Value::Float(50.0), Value::Float(200.0), Value::Object(Some(damage)), Value::Float(30_000.0), vector([at[0], at[1], at[2] - 50.0])];
    vm.call_native("HurtRadius", vm.level(), &mut args).unwrap();
    let before = [shot, blasted].map(|p| vector_of(&vm, p, "Location", None));
    for _ in 0..10 {
        assert!(vm.tick_level(0.05).is_empty());
    }
    for (i, &p) in [shot, blasted].iter().enumerate() {
        assert!(distance(vector_of(&vm, p, "Location", None), before[i]) > 10.0, "{} not pushed", vm.object(p).path);
    }
}

#[test]
fn new_game_travels_in_the_same_vm() {
    let Some(mut loader) = loader() else { return };
    let Some(dir) = game_dir() else { return };
    let map = loader.load("Startup").unwrap();
    // Declared before the VM: it borrows them for its whole life (its compiled closures make `Vm<&Loader>`
    // invariant in the borrow).
    let (next_loader, monday);
    let mut vm = Vm::new(&loader);
    vm.add_config("System", ini(&dir, "Postal2.ini"));
    vm.add_config("User", ini(&dir, "User.ini"));
    let game_class = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    assert!(vm.begin_play(map, game_class, "").unwrap().is_empty());
    vm.init_client().unwrap();
    let viewport = vm.viewport().unwrap();
    let Some(Value::Object(Some(master))) = vm.get(viewport, "InteractionMaster") else { panic!("no master") };
    let Some(Value::Object(Some(menu))) = vm.get(master, "BaseMenu") else { panic!("no base menu") };
    let (player, _) = vm.login("").unwrap().unwrap();
    vm.tick_level(0.05);
    vm.event(menu, "ShowMenu", Vec::new()).unwrap();
    let click = |vm: &mut Vm<&Loader>, field: &str| {
        let Some(Value::Object(Some(window))) = vm.get(menu, "MyMenu") else { panic!("no menu for {field}") };
        let Some(Value::Object(Some(choice))) = vm.get(window, field) else { panic!("no {field}") };
        vm.event(window, "Notify", vec![Value::Object(Some(choice)), Value::Byte(2)]).unwrap();
    };
    for field in ["NewChoice", "P2Choice", "StartMF", "StartChoice"] {
        click(&mut vm, field);
    }
    let mut url = None;
    for frame in 0..1200 {
        let errors = vm.tick_level(0.05);
        assert!(errors.is_empty(), "frame {frame}: {:?}", errors.iter().map(|(a, e)| format!("{a}: {e}")).collect::<Vec<_>>());
        vm.render_hud(player, 1024.0, 768.0).unwrap();
        url = take_travel(&mut vm);
        if url.is_some() {
            break;
        }
    }
    let url = url.unwrap_or_else(|| panic!("no travel; log tail {:?}", vm.diagnostics.log.iter().rev().take(10).collect::<Vec<_>>()));
    let name = url.split(['?', '#']).next().unwrap().trim_end_matches(".fuk").to_owned();
    let options = url.find(['?', '#']).map_or("", |i| &url[i..]).to_owned();
    vm.leave_level().unwrap();
    assert!(vm.player().is_none() && vm.level().is_none());
    next_loader = {
        let mut next = loader.clone();
        let id = next.load(&name).unwrap();
        next.load_imports(id).unwrap();
        (next, id)
    };
    vm.set_loader(&next_loader.0);
    let next_game = vm.url_game_class(&url).expect("no ?Game= class");
    assert_eq!(vm.name(vm.class(next_game).name), "GameSinglePlayer");
    vm.begin_play(next_loader.1, next_game, &options).unwrap();
    let (player, pawn) = vm.login(&options).unwrap().unwrap();
    let pawn = pawn.expect("no pawn after travel");
    assert_eq!(vm.viewport(), Some(viewport));
    assert_eq!(vm.get(viewport, "InteractionMaster"), Some(Value::Object(Some(master))));
    assert_eq!(vm.get(player, "Player"), Some(Value::Object(Some(viewport))));
    let mut inventory = Vec::new();
    let mut next = vm.get(pawn, "Inventory");
    while let Some(Value::Object(Some(item))) = next {
        inventory.push(vm.name(vm.object(item).class_name).to_owned());
        next = vm.get(item, "Inventory");
        assert!(inventory.len() < 100);
    }
    assert!(inventory.iter().any(|class| class == "HandsWeapon"), "inventory {inventory:?}");
    assert!(vm.diagnostics.log.iter().any(|l| l.contains("Continuing existing game")), "the game state did not travel");
    let Some(Value::Object(Some(game))) = vm.get(vm.level().unwrap(), "Game") else { panic!("no game") };
    assert!(matches!(vm.get(game, "TheGameState"), Some(Value::Object(Some(_)))));
    assert!(vm.diagnostics.log.iter().any(|l| l.contains("P2Screen.PostTravel()")));
    for frame in 0..40 {
        let errors = vm.tick_level(0.05);
        assert!(errors.is_empty(), "after travel, frame {frame}: {:?}", errors.iter().map(|(a, e)| format!("{a}: {e}")).collect::<Vec<_>>());
        vm.render_hud(player, 1024.0, 768.0).unwrap();
    }
    for _ in 0..180 {
        for pawn in vm.actors().filter(|&a| vm.is_a(a, "Pawn") && !vm.is_deleted(a)).collect::<Vec<_>>() {
            vm.mark_rendered(pawn).unwrap();
        }
        vm.tick_level(1.0 / 60.0);
        vm.render_hud(player, 1920.0, 1080.0).unwrap();
    }
    let url = match vm.default_value(next_game, "StartFirstDayURL") {
        Some(Value::Str(url)) => format!("{url}?Game=GameTypes.GameSinglePlayer"),
        other => panic!("no first day {other:?}"),
    };
    let name = url.split('?').next().unwrap().trim_end_matches(".fuk").to_owned();
    vm.leave_level().unwrap();
    monday = {
        let mut next = next_loader.0.clone();
        let id = next.load(&name).unwrap();
        next.load_imports(id).unwrap();
        (next, id)
    };
    vm.set_loader(&monday.0);
    let game = vm.url_game_class(&url).unwrap();
    let errors = vm.begin_play(monday.1, game, "?Game=GameTypes.GameSinglePlayer").unwrap();
    assert!(errors.is_empty(), "{name}: {:?}", errors.iter().take(5).map(|(a, ev, e)| format!("{a} {ev}: {e}")).collect::<Vec<_>>());
    let (player, _) = vm.login("?Game=GameTypes.GameSinglePlayer").unwrap().unwrap();
    for frame in 0..100 {
        let errors = vm.tick_level(0.05);
        assert!(errors.is_empty(), "{name} frame {frame}: {:?}", errors.iter().map(|(a, e)| format!("{a}: {e}")).collect::<Vec<_>>());
        vm.render_hud(player, 1024.0, 768.0).unwrap();
    }
    let removed = vm.actors().filter(|&a| vm.is_deleted(a) && vm.object(a).path.to_ascii_lowercase().starts_with("suburbs-3.")).count();
    assert!(removed > 0, "no actors removed for the day");
    assert!(vm.actors().all(|a| !["startup.", "intro."].iter().any(|p| vm.object(a).path.to_ascii_lowercase().starts_with(p))), "old actors");
}

#[test]
fn suburbs_player_stays_on_the_ground() {
    let Some(mut loader) = loader() else { return };
    let Some(dir) = game_dir() else { return };
    let map = loader.load("Suburbs-3").unwrap();
    loader.load_imports(map).unwrap();
    let mut vm = Vm::new(&loader);
    vm.add_config("System", ini(&dir, "Postal2.ini"));
    vm.add_config("User", ini(&dir, "User.ini"));
    let game = vm.find_class("GameTypes", "GameSinglePlayer").unwrap();
    vm.begin_play(map, game, "").unwrap();
    vm.init_client().unwrap();
    let (_, pawn) = vm.login("").unwrap().unwrap();
    let pawn = pawn.unwrap();
    vm.options.player_physics = true;
    for _ in 0..40 {
        vm.player_input(&[], [0.0, 0.0]).unwrap();
        vm.tick_level(0.05);
    }
    let height = match vm.get(pawn, "CollisionHeight") { Some(Value::Float(h)) => h, _ => 0.0 };
    let mut bad = Vec::new();
    for frame in 0..1200 {
        let keys: &[&str] = if frame % 300 < 280 { &["W"] } else { &[] };
        vm.player_input(keys, [if frame % 120 < 30 { 25.0 } else { 0.0 }, 0.0]).unwrap();
        vm.tick_level(1.0 / 20.0);
        let at = vector_of(&vm, pawn, "Location", None);
        let ground = vm.trace_world(at.into(), [at[0], at[1], at[2] - 3000.0].into(), [0.0; 3].into());
        let feet = at[2] - height;
        if ground.is_none_or(|g| feet < g.location[2] - 8.0) {
            bad.push((frame, at, ground.map(|g| g.location[2]), vm.get(pawn, "Physics")));
        }
    }
    assert!(bad.is_empty(), "{} frames below ground; first {:?}", bad.len(), bad.iter().take(8).collect::<Vec<_>>());
}

#[test]
fn monday_wheel_and_click_pick_a_weapon() {
    let Some(mut loader) = loader() else { return };
    let Some(dir) = game_dir() else { return };
    let map = loader.load("Suburbs-3").unwrap();
    loader.load_imports(map).unwrap();
    let mut vm = Vm::new(&loader);
    vm.add_config("System", ini(&dir, "Postal2.ini"));
    vm.add_config("User", ini(&dir, "User.ini"));
    let game = vm.find_class("GameTypes", "GameSinglePlayer").unwrap();
    vm.begin_play(map, game, "").unwrap();
    vm.init_client().unwrap();
    let (controller, _) = vm.login("").unwrap().unwrap();
    for _ in 0..40 {
        vm.player_input(&[], [0.0, 0.0]).unwrap();
        vm.tick_level(0.05);
    }
    vm.render_hud(controller, 1600.0, 900.0).unwrap();
    let selector = object_of(&vm, controller, "WeaponSelector").expect("weapon selector");
    let selected = |vm: &Vm<&Loader>| int_of(vm, selector, &["SelectedGroupIndex"]);
    let frame = |vm: &mut Vm<&Loader>, keys: &[&str]| {
        vm.player_input(keys, [0.0, 0.0]).unwrap();
        vm.tick_level(0.05);
    };
    frame(&mut vm, &[]);
    frame(&mut vm, &["MouseWheelUp"]);
    frame(&mut vm, &[]);
    let pawn = object_of(&vm, controller, "Pawn");
    assert!(
        selected(&vm) >= 0,
        "the wheel opens the selector; weapon {:?}, groups {:?}, log {:?}",
        pawn.and_then(|p| object_of(&vm, p, "Weapon")).map(|w| vm.object(w).path.clone()),
        vm.get(selector, "WeaponGroups").map(|g| format!("{g:?}").chars().take(200).collect::<String>()),
        vm.diagnostics.log.iter().rev().take(10).collect::<Vec<_>>()
    );
    frame(&mut vm, &["LeftMouse"]);
    frame(&mut vm, &[]);
    assert_eq!(selected(&vm), -1, "the click picks the weapon and closes the selector; log {:?}", vm.diagnostics.log.iter().rev().take(8).collect::<Vec<_>>());
    let res = vm.call_native("ConsoleCommand", Some(controller), &mut [Value::Str("GETCURRENTRES".to_owned()), Value::Bool(false)]).unwrap();
    assert_eq!(res, Some(Value::Str("1600x900".to_owned())));
}

#[test]
#[ignore = "benchmark"]
fn monday_frame_profile() {
    let Some(mut loader) = loader() else { return };
    let Some(dir) = game_dir() else { return };
    let map = loader.load("Suburbs-3").unwrap();
    loader.load_imports(map).unwrap();
    loader.load_imported(&["MeshAnimation", "SkeletalMesh", "Texture"]).unwrap();
    let mut vm = Vm::new(&loader);
    vm.add_config("System", ini(&dir, "Postal2.ini"));
    vm.add_config("User", ini(&dir, "User.ini"));
    let game_class = vm.find_class("GameTypes", "GameSinglePlayer").unwrap();
    vm.begin_play(map, game_class, "").unwrap();
    vm.init_client().unwrap();
    let (player, _) = vm.login("").unwrap().unwrap();
    let drawn: Vec<ObjectId> = vm.actors().filter(|&a| vm.get(a, "DrawType").is_some_and(|d| d != Value::Byte(0))).collect();
    let mut profile = std::collections::HashMap::<String, std::time::Duration>::new();
    let mut frames = 0u32;
    for second in 0..25 {
        vm.diagnostics.tick_profile = Some(Default::default());
        for _ in 0..60 {
            vm.player_input(&[], [0.0, 0.0]).unwrap();
            for pawn in vm.actors().filter(|&a| vm.is_a(a, "Pawn") && !vm.is_deleted(a)).collect::<Vec<_>>() {
                vm.mark_rendered(pawn).unwrap();
            }
            let started = std::time::Instant::now();
            vm.tick_level(1.0 / 60.0);
            let hud = std::time::Instant::now();
            vm.render_hud(player, 1920.0, 1080.0).unwrap();
            *vm.diagnostics.tick_profile.as_mut().unwrap().entry("render_hud".into()).or_default() += hud.elapsed();
            *vm.diagnostics.tick_profile.as_mut().unwrap().entry("frame (tick_level + render_hud)".into()).or_default() += started.elapsed();
            let reads = std::time::Instant::now();
            let mut sink = 0.0f32;
            for &actor in &drawn {
                sink += <[f32; 3]>::from(vm.location(actor)).iter().sum::<f32>() + <[i32; 3]>::from(vm.rotation(actor)).iter().sum::<i32>() as f32;
                let flags = [vm.is_deleted(actor), vm.get(actor, p2_vm::Prop::bHidden) == Some(Value::Bool(true))];
                sink += flags.iter().filter(|&&f| f).count() as f32;
                let reads = [vm.get(actor, p2_vm::Prop::Base), vm.get(actor, p2_vm::Prop::AttachmentBone), vm.get(actor, p2_vm::Prop::Emitters)];
                sink += reads.iter().filter(|v| v.is_some()).count() as f32;
            }
            std::hint::black_box(sink);
            *vm.diagnostics.tick_profile.as_mut().unwrap().entry("host-like actor reads".into()).or_default() += reads.elapsed();
        }
        if second >= 5 {
            frames += 60;
            for (part, time) in vm.diagnostics.tick_profile.take().unwrap() {
                *profile.entry(part).or_default() += time;
            }
        }
    }
    let pawns = vm.actors().filter(|&a| vm.is_a(a, "Pawn") && !vm.is_deleted(a)).count();
    let mut v: Vec<_> = profile.iter().collect();
    v.sort_by_key(|(_, d)| std::cmp::Reverse(**d));
    eprintln!("{pawns} pawns, {:?}", v.iter().take(24).map(|(c, d)| format!("{c} {:?}", **d / frames)).collect::<Vec<_>>());
    let mut plain = std::time::Duration::ZERO;
    for _ in 0..3 {
        for _ in 0..60 {
            vm.player_input(&[], [0.0, 0.0]).unwrap();
            for pawn in vm.actors().filter(|&a| vm.is_a(a, "Pawn") && !vm.is_deleted(a)).collect::<Vec<_>>() {
                vm.mark_rendered(pawn).unwrap();
            }
            let started = std::time::Instant::now();
            vm.tick_level(1.0 / 60.0);
            vm.render_hud(player, 1920.0, 1080.0).unwrap();
            plain += started.elapsed();
        }
    }
    eprintln!("frame without profiling: {:?}", plain / 180);
    for by_function in [false, true] {
        vm.take_work_counts();
        vm.profile_functions(by_function);
        let frames = if by_function { 120 } else { 60 };
        for _ in 0..frames {
            vm.player_input(&[], [0.0, 0.0]).unwrap();
            for pawn in vm.actors().filter(|&a| vm.is_a(a, "Pawn") && !vm.is_deleted(a)).collect::<Vec<_>>() {
                vm.mark_rendered(pawn).unwrap();
            }
            vm.diagnostics.count_work = !by_function;
            vm.tick_level(1.0 / 60.0);
            vm.render_hud(player, 1920.0, 1080.0).unwrap();
            vm.diagnostics.count_work = false;
        }
        if !by_function {
            let counts: Vec<String> = vm.take_work_counts().into_iter().map(|(work, n)| format!("{work} {}", n / 60)).collect();
            eprintln!("work per frame: {counts:?}");
            let names: Vec<String> = vm.take_property_names().into_iter().take(25).map(|(name, n)| format!("{name} {}", n / 60)).collect();
            eprintln!("properties by name per frame: {names:?}");
        }
    }
    let functions = vm.take_function_profile();
    let own = |native: bool| functions.iter().filter(|(_, t)| t.native == native).map(|(_, t)| t.own).sum::<std::time::Duration>() / 120;
    let calls = |native: bool| functions.iter().filter(|(_, t)| t.native == native).map(|(_, t)| t.calls).sum::<u64>() / 120;
    eprintln!("per frame: script {:?} in {} calls, natives {:?} in {} calls", own(false), calls(false), own(true), calls(true));
    for (function, time) in functions.into_iter().take(40) {
        let kind = if time.native { " (native)" } else { "" };
        eprintln!("  {function}{kind}: {} calls, own {:?}, total {:?} per frame", time.calls / 120, time.own / 120, time.total / 120);
    }
}

#[test]
fn monday_is_deterministic() {
    let Some(mut loader) = loader() else { return };
    let Some(dir) = game_dir() else { return };
    let map = loader.load("Suburbs-3").unwrap();
    loader.load_imports(map).unwrap();
    loader.load_imported(&["MeshAnimation", "SkeletalMesh"]).unwrap();
    let run = || {
        let mut vm = Vm::new(&loader);
        vm.add_config("System", ini(&dir, "Postal2.ini"));
        vm.add_config("User", ini(&dir, "User.ini"));
        let game_class = vm.find_class("GameTypes", "GameSinglePlayer").unwrap();
        vm.begin_play(map, game_class, "").unwrap();
        vm.init_client().unwrap();
        vm.login("").unwrap().unwrap();
        vm.options.player_physics = true;
        for frame in 0..240 {
            vm.player_input(if frame % 80 < 60 { &["W"] } else { &[] }, [if frame % 40 < 10 { 20.0 } else { 0.0 }, 0.0]).unwrap();
            for pawn in vm.actors().filter(|&a| vm.is_a(a, "Pawn") && !vm.is_deleted(a)).collect::<Vec<_>>() {
                vm.mark_rendered(pawn).unwrap();
            }
            vm.tick_level(1.0 / 60.0);
        }
        let level = vm.level().unwrap();
        for field in ["Year", "Month", "Day", "DayOfWeek", "Hour", "Minute", "Second", "Millisecond"] {
            vm.set(level, field, Value::Int(0));
        }
        for object in vm.object_ids().filter(|&o| vm.is_a(o, "GameState") || vm.is_a(o, "GameInfo")).collect::<Vec<_>>() {
            for field in ["TimeStart", "TimeStop", "PreTravelTime", "PostTravelTime"] {
                vm.set(object, field, Value::Struct(vec![Value::Float(0.0); 3]));
            }
        }
        let state: Vec<String> = vm
            .object_ids()
            .filter(|&o| !vm.is_a(o, "SlotInfoMgr"))
            .map(|o| {
                let object = vm.object(o);
                let channels: Vec<_> = (0..16).filter_map(|c| vm.anim_channel(o, c).map(|(channel, _)| channel.clone())).collect();
                format!("{} {:?} {:?} {:?}", object.path, vm.name(vm.state_name(o)), vm.properties(o), channels)
            })
            .collect();
        (state, vm.diagnostics.log.clone())
    };
    let (first, first_log) = run();
    let (second, second_log) = run();
    assert_eq!(first.len(), second.len(), "object counts differ");
    let differing: Vec<_> = first.iter().zip(&second).filter(|(a, b)| a != b).map(|(a, _)| a.split(' ').next().unwrap_or_default().to_owned()).collect();
    assert!(differing.is_empty(), "{} objects differ, first {:?}", differing.len(), differing.iter().take(8).collect::<Vec<_>>());
    assert_eq!(first_log, second_log);
}

fn movement_anim(vm: &Vm<&Loader>, pawn: ObjectId, index: usize) -> NameId {
    match vm.get(pawn, "MovementAnims") {
        Some(Value::Array(names)) => match names.get(index) {
            Some(&Value::Name(name)) => name,
            _ => NameId::NONE,
        },
        _ => NameId::NONE,
    }
}

fn alpha_of(vm: &Vm<&Loader>, pawn: ObjectId, channel: usize) -> f32 {
    vm.anim_channel(pawn, channel).map_or(0.0, |(c, _)| c.blend_alpha)
}

#[test]
fn suburbs_pawns_animate_their_movement() {
    let Some(mut loader) = loader() else { return };
    let Some(dir) = game_dir() else { return };
    let map = loader.load("Suburbs-3").unwrap();
    loader.load_imports(map).unwrap();
    load_animations(&mut loader);
    let mut vm = Vm::new(&loader);
    vm.add_config("System", ini(&dir, "Postal2.ini"));
    vm.add_config("User", ini(&dir, "User.ini"));
    let game = vm.find_class("GameTypes", "GameSinglePlayer").unwrap();
    vm.begin_play(map, game, "").unwrap();
    vm.init_client().unwrap();
    vm.login("").unwrap().unwrap();
    let people = |vm: &Vm<&Loader>| -> Vec<ObjectId> {
        vm.actors()
            .filter(|&a| vm.is_a(a, "P2MoCapPawn") && !vm.is_deleted(a) && vm.get(a, "bPhysicsAnimUpdate") == Some(Value::Bool(true)))
            .filter(|&a| vm.get(a, "Controller").is_some_and(|c| !matches!(c, Value::Object(None))))
            .collect()
    };
    let frame = |vm: &mut Vm<&Loader>| {
        vm.player_input(&[], [0.0, 0.0]).unwrap();
        for pawn in vm.actors().filter(|&a| vm.is_a(a, "Pawn") && !vm.is_deleted(a)).collect::<Vec<_>>() {
            vm.mark_rendered(pawn).unwrap();
        }
        vm.tick_level(0.05);
    };
    let accelerating = |vm: &Vm<&Loader>, p: ObjectId| vector_of(vm, p, "Acceleration", None) != [0.0; 3];
    let mut still = std::collections::HashMap::new();
    for _ in 0..200 {
        frame(&mut vm);
        for pawn in people(&vm) {
            let ticks = still.entry(pawn).or_insert(0);
            *ticks = if accelerating(&vm, pawn) { 0 } else { *ticks + 1 };
        }
    }
    let speed = |vm: &Vm<&Loader>, p: ObjectId| distance(vector_of(vm, p, "Velocity", None), [0.0; 3]);
    let movers: Vec<ObjectId> = people(&vm).into_iter().filter(|&p| accelerating(&vm, p) && speed(&vm, p) > 10.0).collect();
    let standing: Vec<ObjectId> = people(&vm).into_iter().filter(|p| still.get(p).is_some_and(|&t| t >= 40)).collect();
    assert!(movers.len() >= 5, "too few walking people");
    assert!(!standing.is_empty(), "nobody standing");

    let dilation = float_of(&vm, vm.level().unwrap(), &["TimeDilation"]);
    let before: Vec<_> = movers.iter().map(|&p| vm.anim_channel(p, 4).map(|(c, _)| (c.frame, c.rate))).collect();
    for &pawn in &movers {
        let (channel, alpha) = (4..8).map(|c| (c, alpha_of(&vm, pawn, c))).max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        let (state, animation) = vm.anim_channel(pawn, channel).unwrap();
        let wanted = movement_anim(&vm, pawn, channel - 4);
        assert!(alpha > 0.0 && state.sequence == wanted, "{}: channel {channel} alpha {alpha} plays {} not {}", vm.object(pawn).path, vm.name(state.sequence), vm.name(wanted));
        let (id, export) = loader.loaded_export_by_path(&vm.object(animation.unwrap()).path).unwrap();
        let pkg = &loader.package(id).package;
        let anim = p2_assets::read_mesh_animation(pkg, &loader.package(id).data, &pkg.exports[export]).unwrap();
        let sequence = |name: NameId| anim.sequences.iter().find(|s| pkg.name(s.name).eq_ignore_ascii_case(vm.name(name)));
        let frames = sequence(state.sequence).unwrap().num_frames as f32;
        let rates: Vec<f32> = (0..4).filter_map(|i| sequence(movement_anim(&vm, pawn, i))).map(|s| if s.rate == 0.0 { 30.0 } else { s.rate }).collect();
        let scale = float_of(&vm, pawn, &["GroundSpeed"]) / float_of(&vm, pawn, &["BaseMovementRate"]);
        let own_rate = state.rate * frames / scale;
        let (low, high) = rates.iter().fold((f32::MAX, f32::MIN), |(l, h), &r| (l.min(r), h.max(r)));
        assert!(own_rate >= low * 0.99 && own_rate <= high * 1.01, "{}: {own_rate} fps / scale, sequences {rates:?}", vm.object(pawn).path);
    }
    frame(&mut vm);
    let mut advanced = 0;
    for (&pawn, before) in movers.iter().zip(&before) {
        let (Some((frame0, rate)), Some((after, _))) = (before, vm.anim_channel(pawn, 4)) else { continue };
        if accelerating(&vm, pawn) && *rate > 0.0 {
            let moved = (after.frame - frame0).rem_euclid(1.0);
            let expected = (rate * 0.05 * dilation).rem_euclid(1.0);
            assert!((moved - expected).abs() < 1e-3, "{}: moved {moved}, expected {expected}", vm.object(pawn).path);
            advanced += 1;
        }
    }
    assert!(advanced >= 3);

    for &pawn in &standing {
        for channel in 4..12 {
            assert_eq!(alpha_of(&vm, pawn, channel), 0.0, "{} channel {channel}", vm.object(pawn).path);
        }
    }

    let pawn = standing.iter().copied().find(|&p| vm.get(p, "Physics") == Some(Value::Byte(1))).expect("a walking-physics standing person");
    for (direction, channel) in [("back", 5), ("left", 6), ("right", 7)] {
        for _ in 0..10 {
            let yaw = int_of(&vm, pawn, &["Rotation", "Yaw"]) as f32 * std::f32::consts::PI / 32768.0;
            let look = [yaw.cos(), yaw.sin(), 0.0];
            let push = match direction {
                "back" => [-look[0], -look[1], 0.0],
                "left" => [look[1], -look[0], 0.0],
                _ => [-look[1], look[0], 0.0],
            };
            vm.set(pawn, "Acceleration", Value::Vector(push.map(|c| c * 100.0).into()));
            if let Some(rotation) = vm.get(pawn, "Rotation") {
                vm.set(pawn, "DesiredRotation", rotation);
            }
            vm.mark_rendered(pawn).unwrap();
            vm.tick_actor(pawn, 0.05).unwrap();
        }
        let (state, _) = vm.anim_channel(pawn, channel).unwrap();
        let wanted = movement_anim(&vm, pawn, channel - 4);
        assert_eq!(state.sequence, wanted);
        assert!(alpha_of(&vm, pawn, channel) > 0.99, "{direction}: channel {channel} not blended in");
        let opposite = [5, 4, 7, 6][channel - 4];
        assert!(alpha_of(&vm, pawn, opposite) < 0.01, "{direction}: channel {opposite} still blended in");
        if direction == "back" {
            assert!(alpha_of(&vm, pawn, 6).max(alpha_of(&vm, pawn, 7)) < 0.01, "back: a strafe channel blended in");
        }
    }
}

#[test]
fn console_cheat_gives_weapons() {
    let Some(mut loader) = loader() else { return };
    let Some(dir) = game_dir() else { return };
    let map = loader.load("Suburbs-3").unwrap();
    loader.load_imports(map).unwrap();
    let mut vm = Vm::new(&loader);
    vm.add_config("System", system_ini(&dir));
    vm.add_config("User", ini(&dir, "User.ini"));
    let game = vm.find_class("GameTypes", "GameSinglePlayer").unwrap();
    vm.begin_play(map, game, "").unwrap();
    vm.init_client().unwrap();
    let (controller, pawn) = vm.login("").unwrap().unwrap();
    let pawn = pawn.unwrap();
    vm.tick_level(0.05);
    let count = |vm: &Vm<&Loader>| {
        let mut seen = Vec::new();
        let mut next = object_of(vm, pawn, "Inventory");
        while let Some(item) = next.filter(|item| seen.len() < 500 && !seen.contains(item)) {
            seen.push(item);
            next = object_of(vm, item, "Inventory");
        }
        seen.iter().filter(|&&item| vm.is_a(item, "Weapon")).count()
    };
    let before = count(&vm);
    vm.call_native("ConsoleCommand", Some(controller), &mut [Value::Str("PackNHeat".to_owned()), Value::Bool(false)]).unwrap();
    assert!(count(&vm) > before + 10, "{} weapons before, {} after; log {:?}", before, count(&vm), vm.diagnostics.log.iter().rev().take(8).collect::<Vec<_>>());
}