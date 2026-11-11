mod common;

use common::game_dir;
use std::collections::HashMap;

use p2_engine::actors::{emitter_actor, location_rotation};
use p2_engine::emitter_changes::{EmitterValues, apply_command};
use p2_package::Loader;
use p2_particles::{DirectionUsage, Emitter, EmitterKind};
use p2_vm::{HostCommand, ObjectId, ParticleCommand, Value, Vm};

const DT: f32 = 1.0 / 30.0;

#[derive(Default)]
struct Host {
    emitters: HashMap<ObjectId, (Emitter, Vec<ObjectId>)>,
    values: EmitterValues,
    seen: usize,
}

impl Host {
    fn frame(&mut self, vm: &mut Vm<&Loader>) {
        let new: Vec<ObjectId> = vm.object_ids().skip(self.seen).collect();
        self.seen += new.len();
        for actor in new {
            if let Some(e) = emitter_actor(vm, actor) {
                let props = e.emitters.into_iter().map(|(p, _)| p).collect();
                self.emitters.insert(actor, (Emitter::new(e.settings, props, 1), e.objects));
            }
        }
        self.emitters.retain(|&actor, _| !vm.store.is_deleted(actor));
        let watched: Vec<ObjectId> = self.emitters.keys().copied().collect();
        let mark = vm.store.stamp();
        vm.store.next_stamp();
        for (object, props) in self.values.changes(vm, &watched, mark) {
            if let Some((sim, index)) = self.emitter_of(object) {
                sim.set_properties(index, props);
            }
        }
        let commands: Vec<_> = vm
            .take_commands(|command| matches!(command, HostCommand::Particle(_)))
            .filter_map(|command| if let HostCommand::Particle(particle) = command { Some(particle) } else { None })
            .collect();
        for command in commands {
            let (ParticleCommand::Reset(object, ())
            | ParticleCommand::Spawn { emitter: object, .. }
            | ParticleCommand::CapTime { emitter: object, .. }
            | ParticleCommand::KeepAlive(object)) = command
            else {
                continue;
            };
            let command = command.with_properties(|emitter| p2_engine::actors::emitter_properties(vm, emitter));
            if let Some((sim, index)) = self.emitter_of(object) {
                apply_command(sim, index, command);
            }
        }
        for (&actor, (sim, objects)) in &mut self.emitters {
            for (index, &object) in objects.iter().enumerate() {
                let disabled = vm.store.get(object, "Disabled") == Some(Value::Bool(true));
                if let Some(e) = sim.emitters_mut().get_mut(index)
                    && e.props().disabled != disabled
                {
                    e.set_disabled(disabled);
                }
            }
            let (location, rotation) = location_rotation(vm, actor);
            sim.tick(DT, location, rotation);
        }
    }

    fn emitter_of(&mut self, object: ObjectId) -> Option<(&mut Emitter, usize)> {
        self.emitters.values_mut().find_map(|(sim, objects)| objects.iter().position(|&o| o == object).map(|i| (sim, i)))
    }
}

fn float(vm: &Vm<&Loader>, object: ObjectId, property: &str) -> f32 {
    match vm.store.get(object, property) {
        Some(Value::Float(v)) => v,
        other => panic!("{property}: {other:?}"),
    }
}

fn vector(vm: &Vm<&Loader>, object: ObjectId, property: &str) -> [f32; 3] {
    match vm.store.get(object, property) {
        Some(Value::Vector(v)) => <[f32; 3]>::from(v),
        other => panic!("{property}: {other:?}"),
    }
}

fn load_monday(dir: &std::path::Path) -> (Loader, p2_package::PackageId) {
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    let id = loader.load("Suburbs-3").unwrap();
    loader.load_imports(id).unwrap();
    loader.load_imported(&["MeshAnimation", "SkeletalMesh"]).unwrap();
    (loader, id)
}

fn frame(vm: &mut Vm<&Loader>, host: &mut Host, controller: ObjectId, keys: &[&str]) {
    vm.player_input(keys, [0.0, 0.0]).unwrap();
    assert!(vm.tick_level(DT).is_empty());
    vm.render_hud(controller, 1024.0, 768.0).unwrap();
    host.frame(vm);
}

fn start<'a>(loader: &'a Loader, dir: &std::path::Path, id: p2_package::PackageId) -> (Vm<&'a Loader>, ObjectId, Host) {
    let ini = |name: &str| p2_package::Ini::read(dir.join("System").join(name)).unwrap();
    let mut vm = Vm::new(loader);
    vm.add_config("System", ini("Postal2.ini"));
    vm.add_config("User", ini("User.ini"));
    let game = vm.find_class("GameTypes", "GameSinglePlayer").unwrap();
    vm.begin_play(id, game, "").unwrap();
    vm.init_client().unwrap();
    let (controller, _) = vm.login("").unwrap().unwrap();
    vm.options.player_physics = true;
    let mut host = Host::default();
    for _ in 0..300 {
        frame(&mut vm, &mut host, controller, &[]);
        if vm.store.name(vm.state_name(controller)) != "PlayerPrepSave" {
            break;
        }
    }
    (vm, controller, host)
}

fn stream_at(loader: &Loader, dir: &std::path::Path, id: p2_package::PackageId, yaw: i32) -> Vec<p2_particles::StripPoint> {
    let (mut vm, controller, mut host) = start(loader, dir, id);
    vm.store.set(controller, "Rotation", Value::Rotator(p2_math::Rotator::new(65536 - 3000, yaw, 0)));
    frame(&mut vm, &mut host, controller, &["R"]);
    for _ in 0..90 {
        frame(&mut vm, &mut host, controller, &[]);
    }
    for _ in 0..45 {
        frame(&mut vm, &mut host, controller, &["LeftMouse"]);
    }
    let feeder = vm.actors().find(|&a| vm.store.is_a(a, "UrinePourFeeder") && !vm.store.is_deleted(a)).expect("no urine stream");
    let (sim, _) = &host.emitters[&feeder];
    sim.strip(0)
}

#[test]
fn urine_stream_goes_where_the_dude_looks() {
    let Some(dir) = game_dir() else { return };
    let (loader, id) = load_monday(&dir);
    for yaw in [0, 10000, 16384, 32768, 45000] {
        let strip = stream_at(&loader, &dir, id, yaw);
        assert!(strip.len() >= 4, "yaw {yaw}: {strip:?}");
        let angle = yaw as f32 * std::f32::consts::TAU / 65536.0;
        let ahead = [angle.cos(), angle.sin()];
        let first = strip[0].location;
        let far = strip.last().map_or(first, |p| p.location);
        let out = [far[0] - first[0], far[1] - first[1]];
        let length = out[0].hypot(out[1]);
        let along = out[0] * ahead[0] + out[1] * ahead[1];
        assert!(along > 0.9 * length && length > 30.0, "yaw {yaw}: the stream goes {out:?}, the dude looks along {ahead:?}");
        assert!(far[2] < first[2], "yaw {yaw}: the stream rises");
    }
}

#[test]
fn urine_stream_puddle_and_trail_as_drawn() {
    let Some(dir) = game_dir() else { return };
    let (loader, id) = load_monday(&dir);
    let (mut vm, controller, mut host) = start(&loader, &dir, id);
    frame(&mut vm, &mut host, controller, &["R"]);
    for _ in 0..90 {
        frame(&mut vm, &mut host, controller, &[]);
    }
    vm.store.set(controller, "Rotation", Value::Rotator(p2_math::Rotator::new(65536 - 3000, 0, 0)));
    for _ in 0..90 {
        frame(&mut vm, &mut host, controller, &["LeftMouse"]);
    }
    let actors_of = |vm: &Vm<&Loader>, class: &str| -> Vec<ObjectId> { vm.actors().filter(|&a| vm.store.is_a(a, class) && !vm.store.is_deleted(a)).collect() };
    let feeder = *actors_of(&vm, "UrinePourFeeder").first().expect("no urine stream");
    let (nozzle, hit) = (vector(&vm, feeder, "Location"), vector(&vm, feeder, "LastHitLocation"));
    let (sim, _) = &host.emitters[&feeder];
    assert_eq!(sim.emitters()[0].props().kind, EmitterKind::Strip);
    let strip = sim.strip(0);
    let far = strip.last().expect("an empty stream").location;
    assert!(strip.len() >= 6, "{strip:?}");
    let distance = |a: [f32; 3], b: [f32; 3]| (a[0] - b[0]).hypot(a[1] - b[1]);
    assert!(distance(strip[0].location.into(), nozzle) < 30.0, "the stream starts at {:?}, the feeder is at {nozzle:?}", strip[0].location);
    let (out, aim) = ([far[0] - nozzle[0], far[1] - nozzle[1]], [hit[0] - nozzle[0], hit[1] - nozzle[1]]);
    assert!(out[0] * aim[0] + out[1] * aim[1] > 0.0, "the stream goes {out:?}, the hit is {aim:?}");
    assert!(far[2] < nozzle[2] - 20.0, "the stream does not fall: {strip:?}");
    assert!(distance(far.into(), nozzle) > 0.5 * distance(hit, nozzle), "the stream ends at {far:?}, short of {hit:?}");

    let puddle = *actors_of(&vm, "UrinePuddle").first().expect("no puddle");
    let radius = float(&vm, puddle, "UseColRadius");
    let (sim, _) = &host.emitters[&puddle];
    assert_eq!(sim.emitters()[0].props().use_direction_as, DirectionUsage::Normal);
    let sprites: Vec<_> = sim.sprites(0, p2_particles::View::default()).collect();
    assert!(radius > 25.0 && !sprites.is_empty(), "radius {radius}, {} sprites", sprites.len());
    assert!(sprites.iter().any(|s| (s.size[0] - radius).abs() < 2.0), "puddle sprites {sprites:?} for radius {radius}");

    let trail_sprites: Vec<_> = actors_of(&vm, "UrineTrail").iter().flat_map(|t| host.emitters[t].0.sprites(0, p2_particles::View::default()).collect::<Vec<_>>()).collect();
    assert!(!trail_sprites.is_empty(), "no trail drawn");
    assert!(trail_sprites.iter().all(|s| (s.location[2] - hit[2]).abs() < 15.0), "trail off the floor: {trail_sprites:?}");
}
