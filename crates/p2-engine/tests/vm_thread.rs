mod common;

use common::game_dir;
use std::any::TypeId;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use bevy::MinimalPlugins;
use bevy::app::{App, PostUpdate, Update};
use bevy::asset::{AssetApp, AssetPlugin};
use bevy::audio::AudioSource;
use bevy::camera::Camera;
use bevy::camera::visibility::{Visibility, VisibleEntities};
use bevy::ecs::prelude::{Entity, IntoScheduleConfigs, Local, Query, ResMut, Resource, With, Without};
use bevy::image::Image;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::pbr::StandardMaterial;
use bevy::time::TimeUpdateStrategy;
use bevy::transform::components::Transform;
use p2_engine::animation::AnimationPlugin;
use p2_engine::map::{Fluids, Hud, MaterialSwitches, Particles, Projectors, SceneTargets, Session, SpawnedActors};
use p2_engine::mesh_view::{CanvasDrawn, FollowActor, MeshView, MeshViewPlugin, SceneView};
use p2_engine::play::{FrameTimes, Play, PlayPlugin, PlaySystems, PlayerInput};
use p2_vm::ObjectId;

#[derive(Resource)]
struct Holder {
    session: Session,
    frame: u32,
    url: Option<String>,
    loading: bool,
}

#[derive(Resource)]
struct Steer(Vec<&'static str>, [f32; 2]);

#[derive(Resource)]
struct OtherWork(f64);

fn input_at(frame: u32) -> (Vec<&'static str>, [f32; 2]) {
    let mut held = Vec::new();
    if (60..240).contains(&frame) {
        held.push("W");
    }
    if frame % 50 == 25 {
        held.push("Space");
    }
    let mouse = [if frame % 80 < 40 { 6.0 } else { -4.0 }, if frame % 30 < 15 { 1.0 } else { -1.0 }];
    (held, mouse)
}

fn monday(root: &Path, threaded: bool, script: fn(u32) -> (Vec<&'static str>, [f32; 2])) -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::transform::TransformPlugin, MeshViewPlugin, PlayPlugin, AnimationPlugin))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .init_asset::<SkinnedMeshInverseBindposes>()
        .init_asset::<AudioSource>()
        .init_resource::<MeshView>()
        .init_resource::<Fluids>()
        .init_resource::<MaterialSwitches>()
        .init_resource::<Projectors>()
        .init_resource::<Particles>()
        .init_resource::<SpawnedActors>()
        .init_resource::<Hud>()
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(1.0 / 60.0)))
        .insert_resource(Holder { session: Session::new(root.to_path_buf()), frame: 0, url: None, loading: false })
        .insert_resource(OtherWork(0.0))
        .register_required_components::<Mesh3d, Visibility>()
        .add_systems(PostUpdate, |mut cameras: Query<&mut VisibleEntities, With<Camera>>, models: Query<Entity, With<FollowActor>>| {
            for mut seen in &mut cameras {
                let meshes = seen.get_mut(TypeId::of::<Mesh3d>());
                meshes.clear();
                meshes.extend(&models);
                meshes.sort_unstable();
            }
        })
        .add_systems(
            PostUpdate,
            (|mut holder: ResMut<Holder>, mut t: SceneTargets| {
                if holder.loading {
                    if let Some(result) = holder.session.finish_load(&mut t) {
                        result.unwrap_or_else(|error| panic!("{error}"));
                        holder.loading = false;
                        holder.frame += 1;
                    }
                    return;
                }
                if holder.frame == 0 {
                    let map = Path::new("Maps").join("Suburbs-3.fuk").display().to_string();
                    if let Some(url) = holder.url.take() {
                        holder.session.begin_load(&map, Some(url), &mut t).unwrap_or_else(|error| panic!("{error}"));
                        holder.loading = true;
                        return;
                    }
                    if let Err(error) = holder.session.load_map(&map, &mut t) {
                        panic!("{error}");
                    }
                } else {
                    let items = t.play.world().map(|world| world.hud_items().to_vec()).unwrap_or_default();
                    t.play.set_canvas_size([1024.0, 768.0]);
                    holder.session.present(&items, &mut t);
                }
                holder.frame += 1;
            })
            .in_set(PlaySystems::Present),
        )
        .add_systems(PostUpdate, (|work: bevy::ecs::prelude::Res<OtherWork>| {
            let started = Instant::now();
            while started.elapsed().as_secs_f64() * 1000.0 < work.0 {
                std::hint::spin_loop();
            }
        })
        .after(PlaySystems::Present))
        .add_systems(
            Update,
            (move |mut input: ResMut<PlayerInput>, mut frame: Local<u32>, steer: Option<bevy::ecs::prelude::Res<Steer>>| {
                (input.held, input.mouse) = match steer {
                    Some(steer) => (steer.0.clone(), steer.1),
                    None => script(*frame),
                };
                input.playing = true;
                *frame += 1;
            })
            .in_set(PlaySystems::Input),
        );
    app.world_mut().resource_mut::<Play>().threaded = threaded;
    app.world_mut().spawn(Camera::default());
    app
}

#[derive(Debug, Clone, PartialEq)]
struct Shown {
    view: [f32; 5],
    models: (usize, f64),
    weapons: BTreeMap<String, [f32; 7]>,
}

fn shown(app: &mut App) -> Shown {
    let world = app.world_mut();
    let mut views = world.query::<&SceneView>();
    let view = views.iter(world).next().map_or([0.0; 5], |view| {
        let (yaw, pitch, _) = view.rotation.to_euler(bevy::math::EulerRot::YXZ);
        [view.position.x, view.position.y, view.position.z, yaw, pitch]
    });
    let mut models = world.query_filtered::<(&FollowActor, &Transform), With<CanvasDrawn>>();
    let weapons = models
        .iter(world)
        .map(|(follow, t)| {
            let (p, r) = (t.translation, t.rotation);
            (format!("{:?}", follow.actor), [p.x, p.y, p.z, r.x, r.y, r.z, r.w])
        })
        .collect();
    let mut others = world.query_filtered::<&Transform, (With<FollowActor>, Without<CanvasDrawn>)>();
    let models = others.iter(world).fold((0, 0.0), |(n, sum), t| (n + 1, sum + f64::from(t.translation.x + t.translation.y + t.translation.z)));
    Shown { view, models, weapons }
}

type State = Vec<(String, Vec<String>)>;

fn vm_state(app: &mut App) -> State {
    let mut vm = app.world_mut().resource_mut::<Play>().take_vm().expect("a played VM");
    for id in vm.object_ids() {
        if vm.store.get(id, "LastRenderTime").is_some() {
            vm.store.set(id, "LastRenderTime", p2_vm::Value::Float(0.0));
        }
        if matches!(vm.store.get(id, "AttachmentBone"), Some(p2_vm::Value::Name(bone)) if vm.store.name(bone) != "None") {
            vm.store.set(id, "Location", p2_vm::Value::Vector([0.0; 3].into()));
            vm.store.set(id, "Rotation", p2_vm::Value::Rotator([0; 3].into()));
        }
    }
    vm.object_ids().map(|id| (vm.store.object(id).path.clone(), vm.store.properties(id).iter().map(|p| format!("{p:?}")).collect())).collect()
}
fn run(app: &mut App, frames: usize) -> Vec<Shown> {
    app.update();
    (0..frames)
        .map(|_| {
            app.update();
            shown(app)
        })
        .collect()
}

fn differences(a: &State, b: &State) -> HashSet<(String, usize)> {
    assert_eq!(a.len(), b.len(), "object counts");
    let mut found = HashSet::new();
    for ((path, x), (_, y)) in a.iter().zip(b) {
        assert_eq!(x.len(), y.len(), "{path}");
        found.extend(x.iter().zip(y).enumerate().filter(|(_, (p, q))| p != q).map(|(i, _)| (path.clone(), i)));
    }
    found
}
#[test]
fn threaded_frames_match_inline_frames_one_frame_later() {
    let Some(root) = game_dir() else { return };
    let frames = 240;
    let (mut inline, mut again, mut threaded) = (monday(&root, false, input_at), monday(&root, false, input_at), monday(&root, true, input_at));
    let (shown_inline, shown_threaded) = (run(&mut inline, frames), run(&mut threaded, frames));
    run(&mut again, frames);
    let (state_inline, state_again, state_threaded) = (vm_state(&mut inline), vm_state(&mut again), vm_state(&mut threaded));
    // The VM is deterministic for the same input, but for what scripts read from the system clock (`GetMillisecondsFrom`:
    // the level's time of day, times the game state records): two inline runs differ only there, and the threaded run
    // from the inline one nowhere else.
    let clock = differences(&state_inline, &state_again);
    assert!(clock.len() < 16, "{clock:?}");
    let threaded_only: Vec<_> = differences(&state_inline, &state_threaded).difference(&clock).cloned().collect();
    assert!(threaded_only.is_empty(), "threaded run differs: {threaded_only:?}");
    let weapon_frames = shown_inline.iter().filter(|s| !s.weapons.is_empty()).count();
    assert!(weapon_frames > frames / 2, "the HUD draws the player's weapon");
    assert_eq!(shown_threaded[0], shown_inline[0]);
    assert_eq!(shown_threaded[1].view, shown_inline[0].view);
    for k in 2..frames {
        assert_eq!(shown_threaded[k], shown_inline[k - 1], "frame {k}");
    }
    assert_ne!(shown_inline[10].view, shown_inline[frames - 1].view);
}

#[test]
fn input_reaches_the_view_one_frame_later_threaded() {
    let Some(root) = game_dir() else { return };
    for (threaded, delay) in [(false, 0), (true, 1)] {
        let mut app = monday(&root, threaded, |frame| (Vec::new(), if frame == 200 { [200.0, 0.0] } else { [0.0; 2] }));
        let shown = run(&mut app, 260);
        let turned = shown.windows(2).position(|pair| pair[0].view[3] != pair[1].view[3]).map(|i| i + 1);
        assert_eq!(turned, Some(199 + delay), "threaded: {threaded}");
    }
}

fn vsync_dt(frame: usize) -> f32 {
    if frame.is_multiple_of(2) { 3.0 / 240.0 } else { 4.0 / 240.0 }
}

fn busy_dt(frame: usize) -> f32 {
    const PATTERN: [f32; 7] = [0.0125, 0.0108, 0.0139, 0.0117, 0.0131, 0.0102, 0.0136];
    PATTERN[frame % PATTERN.len()]
}

fn view_yaw(app: &mut App) -> f32 {
    let world = app.world_mut();
    let forward = world.query::<&SceneView>().iter(world).next().map_or(bevy::math::Vec3::X, |view| view.rotation * -bevy::math::Vec3::Z);
    forward.z.atan2(forward.x) * 65536.0 / std::f32::consts::TAU
}

fn walk(root: &Path, smooth: bool, toward: f32, dt: &[f32]) -> Vec<(f32, bevy::math::Vec3)> {
    let mut app = monday(root, true, |_| (Vec::new(), [0.0; 2]));
    app.world_mut().resource_mut::<Play>().smooth_step = smooth;
    app.insert_resource(Steer(Vec::new(), [0.0; 2]));
    run(&mut app, 60);
    for _ in 0..60 {
        let error = (toward - view_yaw(&mut app) + 32768.0).rem_euclid(65536.0) - 32768.0;
        app.world_mut().resource_mut::<Steer>().1 = [(error / 80.0).clamp(-300.0, 300.0), 0.0];
        app.update();
    }
    app.world_mut().resource_mut::<Steer>().1 = [0.0; 2];
    run(&mut app, 5);
    let error = (toward - view_yaw(&mut app) + 32768.0).rem_euclid(65536.0) - 32768.0;
    assert!(error.abs() < 300.0, "looking along {}, not {toward}", view_yaw(&mut app));
    app.world_mut().resource_mut::<Steer>().0 = vec!["W"];
    dt.iter()
        .map(|&step| {
            app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(step)));
            app.update();
            let world = app.world_mut();
            let position = world.query::<&SceneView>().iter(world).next().map(|view| view.position).unwrap_or_default();
            (step, position)
        })
        .collect()
}

fn mismatch(path: &[(f32, bevy::math::Vec3)], range: std::ops::Range<usize>, lag: usize) -> (f32, f32) {
    let flat = |v: bevy::math::Vec3| bevy::math::Vec3::new(v.x, 0.0, v.z);
    let direction = flat(path[range.end - 1].1 - path[range.start - 1].1).normalize();
    let advances: Vec<f32> = range.clone().map(|k| flat(path[k].1 - path[k - 1].1).dot(direction)).collect();
    let mean_advance = advances.iter().sum::<f32>() / advances.len() as f32;
    let mean_dt = range.clone().map(|k| path[k + lag].0).sum::<f32>() / advances.len() as f32;
    let shares = advances.iter().map(|a| a / mean_advance);
    let worst = shares.clone().zip(range).map(|(share, k)| (share - path[k + lag].0 / mean_dt).abs()).fold(0.0, f32::max);
    (worst, shares.map(|share| (share - 1.0).abs()).fold(0.0, f32::max))
}

/// Which frame time a picture stays up for is not known when its step is taken. With each frame's own time as its step,
/// the view advances by the time of the frame before it; averaged (`StepClock`) it advances evenly, whatever the lag.
#[test]
fn walking_moves_evenly_over_uneven_frame_times() {
    let Some(root) = game_dir() else { return };
    let toward = 22528.0;
    let dt: Vec<f32> = (0..50).map(vsync_dt).chain((0..50).map(busy_dt)).collect();
    let windows = [("vsync", 25..46), ("busy", 64..96)];
    for smooth in [false, true] {
        let path = walk(&root, smooth, toward, &dt);
        for (name, range) in windows.clone() {
            let deviation = {
                let mean = range.clone().map(|k| dt[k]).sum::<f32>() / range.len() as f32;
                range.clone().map(|k| (dt[k] / mean - 1.0).abs()).fold(0.0, f32::max)
            };
            let by_lag: Vec<(f32, f32)> = (0..=3).map(|lag| mismatch(&path, range.clone(), lag)).collect();
            let offs: Vec<String> = by_lag.iter().map(|(off, _)| format!("{off:.3}")).collect();
            if smooth {
                assert!(by_lag[0].1 < 0.02, "{name}: uneven advances {:.3}", by_lag[0].1);
                for (lag, &(off, _)) in by_lag.iter().enumerate() {
                    assert!(off < deviation + 0.03, "{name}: lag {lag}: {off:.3} of a frame (frame times {deviation:.3})");
                }
            } else if name == "vsync" {
                assert!(by_lag[0].0 > 1.8 * deviation && by_lag[2].0 > 1.8 * deviation, "{name}: {offs:?} (frame times {deviation:.3})");
            }
        }
    }
}

fn object_of(vm: &p2_vm::Vm<std::sync::Arc<p2_package::Loader>>, object: ObjectId, property: &str) -> Option<ObjectId> {
    match vm.store.get(object, property) {
        Some(p2_vm::Value::Object(found)) => found,
        _ => None,
    }
}

#[test]
#[ignore = "benchmark"]
fn monday_spawn_cost() {
    let Some(root) = game_dir() else { return };
    let mut app = monday(&root, true, |_| (Vec::new(), [0.0; 2]));
    app.world_mut().resource_mut::<Holder>().url = Some("Suburbs-3?Game=GameTypes.GameSinglePlayer".to_owned());
    while app.world().resource::<Holder>().frame == 0 {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
    run(&mut app, 60);
    let vm = |app: &mut App, f: &mut dyn FnMut(&mut p2_vm::Vm<std::sync::Arc<p2_package::Loader>>)| {
        app.world_mut().resource_mut::<Play>().world_mut().and_then(|world| world.with_vm(|vm| f(vm))).expect("a played VM");
    };
    let mut classes: Vec<p2_vm::ClassId> = Vec::new();
    vm(&mut app, &mut |vm| {
        let draw = |vm: &p2_vm::Vm<_>, a: ObjectId| match vm.store.get(a, "DrawType") {
            Some(p2_vm::Value::Byte(d)) => d,
            _ => 0,
        };
        let movable = |vm: &p2_vm::Vm<_>, a: ObjectId| vm.store.get(a, "bStatic") != Some(p2_vm::Value::Bool(true)) && !vm.store.is_deleted(a);
        for kind in [8u8, 2] {
            let mut found: Vec<p2_vm::ClassId> = Vec::new();
            for actor in vm.actors().filter(|&a| draw(vm, a) == kind && movable(vm, a) && !vm.store.is_a(a, "Pawn")).collect::<Vec<_>>() {
                let class = vm.store.object(actor).class.unwrap();
                if !found.contains(&class) && found.len() < 4 {
                    found.push(class);
                }
            }
            classes.extend(found);
        }
        let emitters: Vec<p2_vm::ClassId> = vm.actors().filter(|&a| vm.store.is_a(a, "Emitter") && movable(vm, a)).filter_map(|a| vm.store.object(a).class).collect();
        for class in emitters {
            if !classes.contains(&class) && classes.len() < 12 {
                classes.push(class);
            }
        }
        eprintln!("spawning {:?}", classes.iter().map(|&c| vm.store.name(vm.store.class(c).name).to_owned()).collect::<Vec<_>>());
    });
    app.world_mut().resource_mut::<FrameTimes>().take_slowest();
    let ms = |time: Duration| time.as_secs_f64() * 1000.0;
    for round in 0..5 {
        vm(&mut app, &mut |vm| {
            let pawn = object_of(vm, vm.player().unwrap(), "Pawn").unwrap();
            let at = vm.store.location(pawn);
            for (i, &class) in classes.iter().enumerate() {
                let spot = [at[0] + 80.0 * (i % 4) as f32 + 60.0, at[1] + 80.0 * (i / 4) as f32 - 80.0, at[2] + 200.0];
                let place = p2_vm::Value::Vector(spot.into());
                let _ = vm.spawn(class, None, [Some(place), None]);
            }
        });
        app.world_mut().resource_mut::<FrameTimes>().totals.clear();
        let mut slowest: Option<p2_engine::play::FrameParts> = None;
        for _ in 0..10 {
            app.update();
            if let Some(parts) = app.world_mut().resource_mut::<FrameTimes>().take_slowest()
                && slowest.as_ref().is_none_or(|s| parts.length > s.length)
            {
                slowest = Some(parts);
            }
        }
        let spawned = app.world().resource::<FrameTimes>().totals.get("spawned").copied().unwrap_or_default();
        let slowest = slowest.unwrap_or_default();
        let mut main = slowest.main.clone();
        main.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
        let main: Vec<String> = main.iter().take(5).map(|(n, t)| format!("{n} {:.2}", ms(*t))).collect();
        eprintln!("round {round}: spawned {:.2} ms in all; slowest frame {:.2} ms: {}", ms(spawned), ms(slowest.length), main.join(", "));
    }
}
#[test]
#[ignore = "benchmark"]
fn monday_main_thread_time() {
    let Some(root) = game_dir() else { return };
    let (rounds, block) = (10, 30);
    for other in [0.0, 4.0, 8.0] {
        let mut apps = [monday(&root, false, input_at), monday(&root, true, input_at)];
        let mut totals: [(Duration, BTreeMap<&'static str, Duration>); 2] = Default::default();
        for app in &mut apps {
            app.world_mut().resource_mut::<OtherWork>().0 = other;
            run(app, 30);
        }
        let world = apps[0].world_mut();
        let followed = world.query::<&FollowActor>().iter(world).count();
        let animated = world.resource::<p2_engine::animation::Animated>().0.len();
        for _ in 0..rounds {
            for (app, (wall, parts)) in apps.iter_mut().zip(&mut totals) {
                let mut times = app.world_mut().resource_mut::<FrameTimes>();
                times.totals.clear();
                times.vm_totals.clear();
                let started = Instant::now();
                for _ in 0..block {
                    app.update();
                }
                *wall += started.elapsed();
                let times = app.world().resource::<FrameTimes>();
                for (name, time) in times.totals.iter().chain(&times.vm_totals) {
                    *parts.entry(name).or_default() += *time;
                }
            }
        }
        let frames = f64::from(rounds * block);
        for (threaded, (wall, parts)) in [false, true].into_iter().zip(&totals) {
            let ms = |time: &Duration| time.as_secs_f64() * 1000.0 / frames;
            let per_frame = ms(wall);
            let part = |name: &str| parts.get(name).map_or(0.0, ms);
            let vm: f64 = parts.iter().filter(|(n, _)| (n.starts_with("vm ") && **n != "vm wait") || **n == "hud scripts").map(|(_, t)| ms(t)).sum();
            let wait = part("vm wait");
            let listed: Vec<String> = parts.iter().map(|(n, t)| format!("{n} {:.2}", ms(t))).collect();
            eprintln!(
                "other work {other} ms, {} ({followed} models, {animated} animated): frame {per_frame:.2} ms, main thread busy {:.2} ms \
                 (waiting for the VM {wait:.2}), VM work {vm:.2} ms; input sampled to frame shown {:.1} ms; parts: {}",
                if threaded { "threaded" } else { "inline" },
                per_frame - wait,
                per_frame * if threaded { 2.0 } else { 1.0 },
                listed.join(", ")
            );
        }
    }
}