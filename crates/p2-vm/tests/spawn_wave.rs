mod common;

use std::time::{Duration, Instant};

use common::{DT, Dude, game_dir, load_suburbs, object_of};

use p2_vm::{ClassId, FunctionProfiling, Value};

const WAVE: usize = 40;

#[test]
fn spawn_wave_splits_script_from_host() {
    let Some(dir) = game_dir() else { return };
    let loader = load_suburbs(&dir);
    let mut dude = Dude::playing(&loader, &dir);
    dude.vm.prewarm();
    dude.frames(30, &[]);

    let mut classes: Vec<(ClassId, usize)> = Vec::new();
    let bystanders: Vec<_> = dude
        .vm
        .actors()
        .filter(|&a| a != dude.pawn && dude.vm.store.is_a(a, "P2MoCapPawn") && !dude.vm.store.is_deleted(a) && object_of(&dude.vm, a, "Controller").is_some())
        .collect();
    for actor in bystanders {
        let Some(class) = dude.vm.store.object(actor).class else { continue };
        match classes.iter_mut().find(|(c, _)| *c == class) {
            Some((_, count)) => *count += 1,
            None => classes.push((class, 1)),
        }
    }
    let (class, placed) = classes.into_iter().max_by_key(|&(_, count)| count).expect("a bystander class on the map");
    let class_name = dude.vm.store.name(dude.vm.store.class(class).name).to_owned();

    let me = dude.vector(dude.pawn, "Location");
    let at = |index: usize| {
        let angle = index as f32 * std::f32::consts::TAU / WAVE as f32;
        let radius = 300.0 + 150.0 * (index % 3) as f32;
        Value::Vector(p2_math::Vector::new(me[0] + radius * angle.cos(), me[1] + radius * angle.sin(), me[2]))
    };

    let started = Instant::now();
    let warm = dude.vm.spawn(class, None, [Some(at(WAVE)), None]).unwrap();
    eprintln!("{class_name} ({placed} placed): first spawn {:?}, spawned {}", started.elapsed(), warm.is_some());

    dude.vm.profile_functions(FunctionProfiling::On);
    dude.vm.take_work_counts();
    dude.vm.diagnostics.tick_profile = Some(Default::default());
    let mut each = Vec::with_capacity(WAVE);
    let mut spawned = 0;
    let started = Instant::now();
    for index in 0..WAVE {
        let one = Instant::now();
        spawned += usize::from(dude.vm.spawn(class, None, [Some(at(index)), None]).unwrap().is_some());
        each.push(one.elapsed());
    }
    let wave = started.elapsed();
    let tick = Instant::now();
    dude.vm.tick_level(DT);
    let tick = tick.elapsed();

    each.sort();
    eprintln!(
        "wave of {WAVE}: {spawned} spawned in {wave:?} ({:?} a spawn, median {:?}, slowest {:?}); the next tick {tick:?}",
        wave / WAVE as u32,
        each[WAVE / 2],
        each[WAVE - 1]
    );
    let functions = dude.vm.take_function_profile();
    let (script, native) = functions.iter().fold((Duration::ZERO, Duration::ZERO), |(script, native), (_, time)| {
        if time.native { (script, native + time.own) } else { (script + time.own, native) }
    });
    eprintln!("own time over the wave and its tick: script {script:?}, natives {native:?}, host rest {:?}", (wave + tick).saturating_sub(script + native));
    for (name, time) in functions.iter().take(30) {
        let kind = if time.native { "native" } else { "script" };
        eprintln!("  {name} ({kind}): {} calls, own {:?}, total {:?}", time.calls, time.own, time.total);
    }
    let mut phases: Vec<(String, Duration)> = dude.vm.diagnostics.tick_profile.take().unwrap_or_default().into_iter().collect();
    phases.sort_by_key(|(_, time)| std::cmp::Reverse(*time));
    let phases: Vec<String> = phases.iter().take(16).map(|(name, time)| format!("{name} {time:?}")).collect();
    eprintln!("phases: {}", phases.join(", "));
    let mut work = dude.vm.take_work_counts();
    work.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    let work: Vec<String> = work.iter().take(16).map(|(name, count)| format!("{name} {count}")).collect();
    eprintln!("work: {}", work.join(", "));
    assert!(spawned > 0, "no {class_name} fit around the player");
}
