//! Dumps VM state after a few hundred frames. Run `--ignored`; `P2_EQUIV_OUT` is the dump directory,
//! `P2_EQUIV_BASELINE` one with dumps of an earlier build.

mod common;

use common::{game_dir, start_single_player};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use p2_package::Loader;
use p2_script::Type;
use p2_vm::{FunctionProfiling, HostCommand, ObjectId, Value, Vm};

fn load(dir: &Path, map: &str) -> (Loader, p2_package::PackageId) {
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    let id = loader.load(map).unwrap();
    loader.load_imports(id).unwrap();
    loader.load_imported(&["MeshAnimation", "SkeletalMesh", "Texture"]).unwrap();
    (loader, id)
}

fn input(frame: usize) -> (Vec<&'static str>, [f32; 2]) {
    let mut keys = Vec::new();
    if frame % 90 < 60 {
        keys.push("W");
    }
    if (120..140).contains(&(frame % 200)) {
        keys.push("D");
    }
    match frame {
        100 => keys.push("MouseWheelUp"),
        103 | 180..=200 | 260..=264 => keys.push("LeftMouse"),
        _ => {}
    }
    (keys, [if frame % 45 < 8 { 20.0 } else { 0.0 }, if frame % 70 < 5 { 6.0 } else { 0.0 }])
}

fn frame(vm: &mut Vm<&Loader>, player: ObjectId, frame: usize, out: Option<&mut String>) {
    let (keys, mouse) = input(frame);
    vm.player_input(&keys, mouse).unwrap();
    for pawn in vm.actors().filter(|&a| vm.store.is_a(a, "Pawn") && !vm.store.is_deleted(a)).collect::<Vec<_>>() {
        vm.mark_rendered(pawn).unwrap();
    }
    let errors = vm.tick_level(1.0 / 30.0);
    let view = vm.player_view().unwrap();
    let items = vm.render_hud(player, 1024.0, 768.0).unwrap();
    let sounds: Vec<_> = vm.take_commands(|command| matches!(command, HostCommand::Sound(_))).filter_map(|command| if let HostCommand::Sound(sound) = command { Some(sound) } else { None }).collect();
    if let Some(out) = out {
        writeln!(out, "frame {frame}: errors {:?}", errors.iter().map(|(a, e)| format!("{a}: {e}")).collect::<Vec<_>>()).unwrap();
        writeln!(out, "  view {view:?}").unwrap();
        writeln!(out, "  sounds {sounds:?}").unwrap();
        writeln!(out, "  hud {} items", items.len()).unwrap();
        if frame.is_multiple_of(50) {
            for item in &items {
                writeln!(out, "    {item:?}").unwrap();
            }
        }
    }
}

fn slot(vm: &Vm<&Loader>, object: ObjectId, index: usize) -> Option<Value> {
    vm.store.property_at(object, index)
}

fn int(value: &Value) -> i32 {
    match value {
        Value::Byte(v) => i32::from(*v),
        Value::Int(v) => *v,
        Value::Bool(v) => i32::from(*v),
        Value::Float(v) => *v as i32,
        _ => 0,
    }
}

fn typed(vm: &Vm<&Loader>, ty: &Type, dim: u32, value: Option<&Value>, out: &mut String) {
    if dim > 1 {
        out.push('<');
        for i in 0..dim as usize {
            let element = match value {
                Some(Value::Array(items)) => items.get(i),
                _ => None,
            };
            typed(vm, ty, 1, element, out);
            out.push(',');
        }
        out.push('>');
        return;
    }
    match ty {
        Type::Byte => write!(out, "{}", value.map_or(0, int) as u8).unwrap(),
        Type::Int => write!(out, "{}", value.map_or(0, int)).unwrap(),
        Type::Bool => out.push_str(if value.is_some_and(|v| int(v) != 0 || matches!(v, Value::Object(Some(_)))) { "T" } else { "F" }),
        Type::Float => {
            let f = match value {
                Some(Value::Float(f)) => *f,
                Some(other) => int(other) as f32,
                None => 0.0,
            };
            write!(out, "{f:?}").unwrap();
        }
        Type::Object | Type::Class => match value {
            Some(Value::Object(Some(o))) => out.push_str(&vm.store.object(*o).path),
            _ => out.push_str("None"),
        },
        Type::Name => match value {
            Some(Value::Name(n)) => out.push_str(vm.store.name(*n)),
            _ => out.push_str("None"),
        },
        Type::Str | Type::String => match value {
            Some(Value::Str(s)) => write!(out, "{s:?}").unwrap(),
            _ => out.push_str("\"\""),
        },
        Type::Array(inner) | Type::FixedArray(inner, _) => {
            out.push('[');
            if let Some(Value::Array(items)) = value {
                for item in items {
                    typed(vm, inner, 1, Some(item), out);
                    out.push(',');
                }
            }
            out.push(']');
        }
        Type::Struct(layout) => {
            out.push('{');
            let inline = match value {
                Some(&Value::Vector(v)) => <[f32; 3]>::from(v).map(Value::Float).to_vec(),
                Some(&Value::Rotator(r)) => <[i32; 3]>::from(r).map(Value::Int).to_vec(),
                _ => Vec::new(),
            };
            for (i, member) in layout.properties.iter().enumerate() {
                let value = match value {
                    Some(Value::Struct(members)) => members.get(i),
                    _ => inline.get(i),
                };
                write!(out, "{}=", member.name).unwrap();
                typed(vm, &member.ty, member.array_dim, value, out);
                out.push(';');
            }
            out.push('}');
        }
        Type::Delegate => match value {
            Some(Value::Delegate { object, function }) => {
                write!(out, "{}.{}", object.map_or("None", |o| vm.store.object(o).path.as_str()), vm.store.name(*function)).unwrap();
            }
            _ => out.push_str("None.None"),
        },
        Type::Map => out.push_str("map"),
    }
}

fn dump(vm: &mut Vm<&Loader>, out: &mut String) {
    let level = vm.level().unwrap();
    for field in ["Year", "Month", "Day", "DayOfWeek", "Hour", "Minute", "Second", "Millisecond"] {
        vm.store.set(level, field, Value::Int(0));
    }
    for object in vm.object_ids().filter(|&o| vm.store.is_a(o, "GameState") || vm.store.is_a(o, "GameInfo")).collect::<Vec<_>>() {
        for field in ["TimeStart", "TimeStop", "PreTravelTime", "PostTravelTime"] {
            vm.store.set(object, field, Value::Struct(vec![Value::Float(0.0); 3]));
        }
    }
    for o in vm.object_ids() {
        let object = vm.store.object(o);
        write!(out, "{} {} state={}", object.path, vm.store.name(object.class_name), vm.store.name(vm.state_name(o))).unwrap();
        if let Some(class) = object.class {
            let layout = std::sync::Arc::clone(&vm.store.class(class).layout);
            for (i, field) in layout.properties.iter().enumerate() {
                write!(out, " {}=", field.name).unwrap();
                typed(vm, &field.ty, field.array_dim, slot(vm, o, i).as_ref(), out);
            }
        }
        for channel in 0..16 {
            if let Some((c, mesh)) = vm.anim_channel(o, channel) {
                write!(out, " ch{channel}={c:?}/{mesh:?}").unwrap();
            }
        }
        out.push('\n');
    }
    writeln!(out, "log:").unwrap();
    for line in &vm.diagnostics.log {
        writeln!(out, "  {line}").unwrap();
    }
    writeln!(out, "missing natives {:?}", vm.diagnostics.missing_natives).unwrap();
}

fn run(dir: &Path, map: &str, frames: usize) -> String {
    let (loader, id) = load(dir, map);
    let (mut vm, player, _) = start_single_player(dir, &loader, id);
    let mut out = String::new();
    let started = std::time::Instant::now();
    for f in 0..frames {
        frame(&mut vm, player, f, Some(&mut out));
    }
    eprintln!("{map}: {frames} frames in {:?}", started.elapsed());
    dump(&mut vm, &mut out);
    out
}

fn mask_name_ids(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("NameId(") {
        let (head, tail) = rest.split_at(at + "NameId(".len());
        out.push_str(head);
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && tail[digits..].starts_with(')') {
            out.push('#');
            rest = &tail[digits..];
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

#[test]
fn name_numbers_are_masked() {
    assert_eq!(mask_name_ids("a NameId(12320), b NameId(x) NameId(0)"), "a NameId(#), b NameId(x) NameId(#)");
}

/// A save slot records the real date and time of the run (`Time="2026-11-11 : 12:53"`, `lTime=1794394392`): two runs
/// a minute apart differ there and nowhere else.
fn mask_clock(text: &str) -> String {
    mask_field(&mask_field(text, "Time=\"", |c| c == '"'), "lTime=", |c| !c.is_ascii_digit())
}

fn mask_field(text: &str, key: &str, ends: fn(char) -> bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(key) {
        let after = at + key.len();
        out.push_str(&rest[..after]);
        out.push('#');
        let tail = &rest[after..];
        rest = &tail[tail.find(ends).unwrap_or(tail.len())..];
    }
    out.push_str(rest);
    out
}

#[test]
fn the_clock_of_a_save_slot_is_masked() {
    let slot = |time: &str, stamp: u32| format!("{{Name=\"Monday\";Day=0;Time=\"{time}\";lTime={stamp};LoadScreen=\"a\";}}");
    assert_eq!(mask_clock(&slot("2026-11-11 : 12:53", 1794394392)), mask_clock(&slot("2026-11-11 : 12:52", 1794394346)));
    assert_eq!(mask_clock("Day=3;LoadScreen=\"a\""), "Day=3;LoadScreen=\"a\"");
    assert_ne!(mask_clock(&slot("x", 1)), mask_clock(&slot("x", 1)).replace("Day=0", "Day=1"));
}

fn check(name: &str, text: &str) {
    if let Some(dir) = std::env::var_os("P2_EQUIV_OUT").map(PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.txt")), text).unwrap();
    }
    let Some(baseline) = std::env::var_os("P2_EQUIV_BASELINE").map(PathBuf::from) else {
        eprintln!("{name}: nothing compared, P2_EQUIV_BASELINE is not set");
        return;
    };
    let expected = std::fs::read_to_string(baseline.join(format!("{name}.txt"))).unwrap();
    let (text, expected) = (mask_clock(&mask_name_ids(text)), mask_clock(&mask_name_ids(&expected)));
    let (ours, theirs): (Vec<_>, Vec<_>) = (text.lines().collect(), expected.lines().collect());
    let differing: Vec<_> = ours.iter().zip(&theirs).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
    for &i in differing.iter().take(6) {
        let (a, b) = (ours[i], theirs[i]);
        let at = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
        let around = |s: &str| s.chars().skip(at.saturating_sub(200)).take(500).collect::<String>();
        eprintln!("{name} line {i} differs at {at}:\n  now:      {}\n  baseline: {}", around(a), around(b));
    }
    assert_eq!(ours.len(), theirs.len(), "{name}: line counts differ");
    assert!(differing.is_empty(), "{name}: {} lines differ", differing.len());
    eprintln!("{name}: {} lines identical", ours.len());
}

#[test]
#[ignore = "equivalence check: run with P2_GAME_DIR and --ignored"]
fn equivalence_church() {
    let Some(dir) = game_dir() else { return };
    check("church", &run(&dir, "Church", 300));
}

#[test]
#[ignore = "equivalence check: run with P2_GAME_DIR and --ignored"]
fn equivalence_monday() {
    let Some(dir) = game_dir() else { return };
    check("monday", &run(&dir, "Suburbs-3", 300));
}

#[test]
#[ignore = "benchmark: run with P2_GAME_DIR and --ignored"]
fn equivalence_bench() {
    let Some(dir) = game_dir() else { return };
    let (loader, id) = load(&dir, "Suburbs-3");
    let (mut vm, player, _) = start_single_player(&dir, &loader, id);
    for f in 0..60 {
        frame(&mut vm, player, f, None);
    }
    let mut times = Vec::new();
    for f in 60..300 {
        let started = std::time::Instant::now();
        frame(&mut vm, player, f, None);
        times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    eprintln!("BENCH frame ms: mean {mean:.3}, median {:.3}, p10 {:.3}, p90 {:.3}", times[times.len() / 2], times[times.len() / 10], times[times.len() * 9 / 10]);
    vm.take_work_counts();
    vm.diagnostics.count_work = true;
    for f in 300..360 {
        frame(&mut vm, player, f, None);
    }
    vm.diagnostics.count_work = false;
    let counts: Vec<String> = vm.take_work_counts().into_iter().map(|(work, n)| format!("{work} {}", n / 60)).collect();
    eprintln!("BENCH work per frame: {counts:?}");
    let actors: Vec<ObjectId> = vm.actors().filter(|&a| !vm.store.is_deleted(a)).collect();
    let reads = |what: &str, read: &dyn Fn(ObjectId) -> f32| {
        let started = std::time::Instant::now();
        let mut sink = 0.0;
        for _ in 0..200 {
            for &a in &actors {
                sink += read(a);
            }
        }
        std::hint::black_box(sink);
        eprintln!("BENCH read {what}: {:.1} ns", started.elapsed().as_secs_f64() * 1e9 / (200 * actors.len()) as f64);
    };
    fn number(value: Option<impl std::borrow::Borrow<Value>>) -> f32 {
        match value.as_ref().map(std::borrow::Borrow::borrow) {
            Some(Value::Float(f)) => *f,
            Some(Value::Bool(b)) => f32::from(u8::from(*b)),
            _ => 0.0,
        }
    }
    reads("bool (get by Prop)", &|a| number(vm.store.get(a, p2_vm::Prop::bHidden)));
    reads("is_deleted", &|a| f32::from(u8::from(vm.store.is_deleted(a))));
    reads("location", &|a| vm.store.location(a)[0]);
    reads("rotation", &|a| vm.store.rotation(a)[1] as f32);
    reads("float by name", &|a| number(vm.store.get(a, "CollisionRadius")));
    if let Some(function) = vm.find_object_function(player, "PlayerCalcView").unwrap() {
        let class = vm.store.object(player).class.unwrap();
        let started = std::time::Instant::now();
        for _ in 0..2000 {
            let args = vec![Value::Object(None), Value::Vector(p2_math::Vector::ZERO), Value::Rotator(p2_math::Rotator::ZERO)];
            std::hint::black_box(vm.call(&function, Some(player), class, args).unwrap());
        }
        eprintln!("BENCH call PlayerCalcView: {:.2} us", started.elapsed().as_secs_f64() * 1e6 / 2000.0);
    }
    vm.diagnostics.tick_profile = Some(Default::default());
    for f in 360..420 {
        frame(&mut vm, player, f, None);
    }
    let mut phases: Vec<_> = vm.diagnostics.tick_profile.take().unwrap_or_default().into_iter().collect();
    phases.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
    eprintln!("BENCH phases per frame: {:?}", phases.iter().take(30).map(|(c, d)| format!("{c} {:?}", *d / 60)).collect::<Vec<_>>());
    vm.profile_functions(FunctionProfiling::On);
    for f in 420..480 {
        frame(&mut vm, player, f, None);
    }
    let functions = vm.take_function_profile();
    let own = |native: bool| functions.iter().filter(|(_, t)| t.native == native).map(|(_, t)| t.own).sum::<std::time::Duration>() / 60;
    let calls = |native: bool| functions.iter().filter(|(_, t)| t.native == native).map(|(_, t)| t.calls).sum::<u64>() / 60;
    eprintln!("BENCH per frame: script {:?} in {} calls, natives {:?} in {} calls", own(false), calls(false), own(true), calls(true));
    let mut by_calls = functions.clone();
    by_calls.sort_by_key(|(_, t)| std::cmp::Reverse(t.calls));
    let top: Vec<String> = by_calls.iter().take(40).map(|(f, t)| format!("{f}{} {}", if t.native { "*" } else { "" }, t.calls / 60)).collect();
    eprintln!("BENCH most called per frame (* native): {top:?}");
}
