mod common;

use common::{add, game_dir, load, scale, start_single_player, vector};
use std::path::Path;

use p2_package::{Loader, PackageId};
use p2_vm::{ObjectId, Value, Vm};

const DT: f32 = 1.0 / 60.0;
const GRAVITY: f32 = 1500.0;
const PHYS_WALKING: Value = Value::Byte(1);
const PHYS_FALLING: Value = Value::Byte(2);

struct Player<'a> {
    vm: Vm<&'a Loader>,
    pawn: ObjectId,
    controller: ObjectId,
}

impl<'a> Player<'a> {
    fn start(loader: &'a Loader, dir: &Path, id: PackageId) -> Self {
        let (vm, controller, pawn) = start_single_player(dir, loader, id);
        let mut player = Self { vm, pawn: pawn.expect("no pawn"), controller };
        let others: Vec<ObjectId> = player.vm.actors().filter(|&a| a != player.pawn && player.vm.is_a(a, "Pawn")).collect();
        for other in others {
            player.vm.set(other, "bCollideActors", Value::Bool(false));
            player.vm.set(other, "bBlockActors", Value::Bool(false));
        }
        player.frames(90, &[]);
        player
    }

    fn frames(&mut self, count: usize, keys: &[&str]) {
        for _ in 0..count {
            self.frame(DT, keys, [0.0, 0.0]);
        }
    }

    fn frame(&mut self, dt: f32, keys: &[&str], mouse: [f32; 2]) {
        self.vm.player_input(keys, mouse).unwrap();
        let errors = self.vm.tick_level(dt);
        assert!(errors.is_empty(), "{:?}", errors.iter().map(|(a, e)| format!("{a}: {e}")).collect::<Vec<_>>());
    }

    fn vector(&self, property: &str) -> [f32; 3] {
        vector(&self.vm, self.pawn, property)
    }

    fn float(&self, property: &str) -> f32 {
        match self.vm.get(self.pawn, property) {
            Some(Value::Float(v)) => v,
            other => panic!("{property}: {other:?}"),
        }
    }

    fn physics(&self) -> Value {
        self.vm.get(self.pawn, "Physics").unwrap()
    }

    fn extent(&self) -> [f32; 3] {
        let (r, h) = (self.float("CollisionRadius"), self.float("CollisionHeight"));
        [r, r, h]
    }

    fn face(&mut self, yaw: i32) {
        let rotation = Value::Rotator(p2_math::Rotator::new(0, yaw, 0));
        self.vm.set(self.controller, "Rotation", rotation.clone());
        self.vm.set(self.pawn, "Rotation", rotation);
    }

    fn place(&mut self, at: [f32; 3]) {
        self.vm.set_location(self.pawn, at.into()).unwrap();
        self.vm.set(self.pawn, "Velocity", Value::Vector(p2_math::Vector::ZERO));
        self.frames(30, &[]);
    }

    fn blocking_actor(&self, at: [f32; 3], direction: [f32; 3], length: f32) -> Option<ObjectId> {
        let [radius, _, height] = self.extent();
        let flag = |a: ObjectId, name: &str| self.vm.get(a, name) == Some(Value::Bool(true));
        let size = |a: ObjectId, name: &str| match self.vm.get(a, name) {
            Some(Value::Float(v)) => v,
            _ => 0.0,
        };
        self.vm.actors().filter(|&a| a != self.pawn && !self.vm.is_deleted(a) && flag(a, "bCollideActors") && flag(a, "bBlockActors")).find(|&a| {
            let centre = vector(&self.vm, a, "Location");
            let offset = add(centre, scale(at, -1.0));
            let along = dot(offset, direction).clamp(0.0, length);
            let closest = add(at, scale(direction, along));
            let reach = radius + size(a, "CollisionRadius") + 4.0;
            flat(add(centre, scale(closest, -1.0))) < reach && (centre[2] - at[2]).abs() < height + size(a, "CollisionHeight") + 40.0
        })
    }

    fn floor_distance(&self) -> Option<f32> {
        let at = self.vector("Location");
        self.vm.trace_world(at.into(), [at[0], at[1], at[2] - 37.0].into(), self.extent().into()).map(|hit| hit.time * 37.0)
    }

    fn floor(&self, at: [f32; 3]) -> Option<(f32, [f32; 3])> {
        let hit = self.vm.trace_world(at.into(), [at[0], at[1], at[2] - 400.0].into(), [0.0; 3].into())?;
        (hit.time > 0.0).then_some((hit.location[2], hit.normal.into()))
    }

    fn clear_run(&self, direction: [f32; 3], length: f32) -> bool {
        self.clear_run_from(self.vector("Location"), direction, length)
    }

    fn clear_run_from(&self, at: [f32; 3], direction: [f32; 3], length: f32) -> bool {
        if self.blocking_actor(at, direction, length).is_some() {
            return false;
        }
        let Some((ground, _)) = self.floor(at) else { return false };
        let low = ground + self.extent()[2] + 1.5;
        for lift in [[at[0], at[1], at[2] + 4.0], [at[0], at[1], low]] {
            if self.vm.trace_world(lift.into(), add(lift, scale(direction, length)).into(), self.extent().into()).is_some() {
                return false;
            }
        }
        (1..=(length / 32.0) as usize).all(|i| {
            let p = add(at, scale(direction, i as f32 * 32.0));
            self.floor(p).is_some_and(|(z, n)| (z - ground).abs() < 1.0 && n[2] > 0.99)
        })
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn flat(v: [f32; 3]) -> f32 {
    v[0].hypot(v[1])
}

fn heading(yaw: i32) -> [f32; 3] {
    let angle = yaw as f32 * std::f32::consts::TAU / 65536.0;
    [angle.cos(), angle.sin(), 0.0]
}

fn yaw_of(d: [f32; 3]) -> i32 {
    (d[1].atan2(d[0]) * 65536.0 / std::f32::consts::TAU).round() as i32
}

fn engine_accelerate(velocity: [f32; 3], dir: [f32; 3], accel_rate: f32, friction: f32, max_speed: f32) -> [f32; 3] {
    let speed = dot(velocity, velocity).sqrt();
    let mut v = add(velocity, scale(add(velocity, scale(dir, -speed)), -DT * friction));
    v = add(v, scale(dir, accel_rate * DT));
    let size = dot(v, v).sqrt();
    if size > max_speed { scale(v, max_speed / size) } else { v }
}

fn engine_brake(velocity: [f32; 3], friction: f32) -> [f32; 3] {
    let v = scale(velocity, 1.0 - 2.0 * DT * friction);
    if dot(v, v) < 100.0 { [0.0; 3] } else { v }
}

#[test]
fn church_run_stop_walk_crouch_jump() {
    let Some(dir) = game_dir() else { return };
    let (loader, id) = load(&dir, "Church");
    let mut player = Player::start(&loader, &dir, id);
    assert_eq!(player.physics(), PHYS_WALKING);
    let (ground_speed, accel_rate, friction) = (player.float("GroundSpeed"), player.float("AccelRate"), player.float("WalkFriction"));
    let jump_z = player.float("JumpZ");
    let yaw = (0..32).map(|i| i * 2048).find(|&yaw| player.clear_run(heading(yaw), 1000.0)).expect("no open direction");
    let direction = heading(yaw);
    player.face(yaw);
    let start = player.vector("Location");
    let height = start[2];

    let mut expected = [0.0; 3];
    for frame in 0..60 {
        player.frames(1, &["W"]);
        expected = engine_accelerate(expected, direction, accel_rate, friction, ground_speed);
        let velocity = player.vector("Velocity");
        assert!((flat(velocity) - flat(expected)).abs() < 0.5, "frame {frame}: speed {} expected {}", flat(velocity), flat(expected));
        assert!(dot(velocity, direction) > 0.99 * flat(velocity), "frame {frame}: off course {velocity:?}");
    }
    assert!((flat(player.vector("Velocity")) - ground_speed).abs() < 0.5);
    assert!((player.vector("Location")[2] - height).abs() < 0.5, "height changed on flat floor");

    let before = player.vector("Location");
    let (mut v, mut distance, mut frames) = (player.vector("Velocity"), 0.0, 0);
    while v != [0.0; 3] {
        v = engine_brake(v, friction);
        distance += flat(v) * DT;
        frames += 1;
    }
    player.frames(frames, &[]);
    let stopped = player.vector("Location");
    let slid = flat(add(stopped, scale(before, -1.0)));
    assert_eq!(player.vector("Velocity"), [0.0; 3], "still moving after {frames} frames");
    assert!((slid - distance).abs() < 0.5, "stopping distance {slid}, engine {distance}");

    player.face(yaw.wrapping_add(32768));
    player.frames(40, &["Shift", "W"]);
    let walk = flat(player.vector("Velocity"));
    assert!((walk - ground_speed * player.float("WalkingPct")).abs() < 0.5, "walking speed {walk}");
    player.frames(30, &[]);

    player.face(yaw);
    player.frames(40, &["Ctrl", "W"]);
    let crouched = flat(player.vector("Velocity"));
    assert_eq!(player.vm.get(player.pawn, "bIsCrouched"), Some(Value::Bool(true)));
    assert!((crouched - ground_speed * player.float("WalkingPct")).abs() < 0.5, "crouched speed {crouched}");
    player.frames(60, &[]);
    assert_eq!(player.vm.get(player.pawn, "bIsCrouched"), Some(Value::Bool(false)));

    player.place(start);
    let at = player.vector("Location");
    assert!(player.vm.trace_world(at.into(), [at[0], at[1], at[2] + 150.0].into(), player.extent().into()).is_none(), "no room to jump");
    let z0 = at[2];
    player.frames(1, &["Space"]);
    assert_eq!(player.physics(), PHYS_FALLING);
    let (mut top, mut air) = (z0, 1);
    while player.physics() == PHYS_FALLING && air < 200 {
        player.frames(1, &[]);
        top = top.max(player.vector("Location")[2]);
        air += 1;
    }
    player.frames(2, &[]);
    let (rise, frames) = (jump_z * jump_z / (2.0 * GRAVITY), 2.0 * jump_z / GRAVITY / DT);
    assert!((top - z0 - rise).abs() < 1.0, "jump height {}", top - z0);
    assert!((air as f32 - frames).abs() <= 1.5, "air time {air} frames");
    assert!((player.vector("Location")[2] - z0).abs() < 0.5, "landed at {} from {z0}", player.vector("Location")[2]);

    player.place(start);
    player.face(yaw);
    player.frames(30, &["W"]);
    let take_off = flat(player.vector("Velocity"));
    let jump_from = player.vector("Location");
    player.frames(1, &["W", "Space"]);
    let mut fastest: f32 = 0.0;
    let mut air = 1;
    while player.physics() == PHYS_FALLING && air < 200 {
        player.frames(1, &["W"]);
        fastest = fastest.max(flat(player.vector("Velocity")));
        air += 1;
    }
    let jumped = flat(add(player.vector("Location"), scale(jump_from, -1.0)));
    assert!(fastest <= take_off + 0.5, "sped up in the air: {fastest} > {take_off}");
    assert!((jumped - take_off * air as f32 * DT).abs() < take_off * 2.0 * DT, "jump length {jumped}");
}

#[test]
fn church_view_bob_and_eye_height() {
    let Some(dir) = game_dir() else { return };
    let (loader, id) = load(&dir, "Church");
    let mut player = Player::start(&loader, &dir, id);
    assert_eq!(player.vm.get(player.pawn, "bUpdateEyeheight"), Some(Value::Bool(true)));
    let yaw = (0..32).map(|i| i * 2048).find(|&yaw| player.clear_run(heading(yaw), 800.0)).expect("no open direction");
    player.face(yaw);
    let base = player.float("BaseEyeHeight");
    let (mut side, mut up) = (Vec::new(), Vec::new());
    let right = heading(yaw.wrapping_add(16384));
    for _ in 0..60 {
        player.frames(1, &["W"]);
        let bob = player.vector("WalkBob");
        side.push(dot(bob, right));
        up.push(bob[2]);
        let eye = player.float("EyeHeight");
        assert!((eye - base).abs() < 1.0, "EyeHeight {eye}, BaseEyeHeight {base}");
    }
    let range = |v: &[f32]| v.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    let ((side_lo, side_hi), (up_lo, up_hi)) = (range(&side[20..]), range(&up[20..]));
    let crossings = |v: &[f32]| v.windows(2).filter(|w| w[0].signum() != w[1].signum()).count();
    assert!(side_lo < -1.0 && side_hi > 1.0 && crossings(&side[20..]) >= 2, "no sideways bob");
    assert!(up_lo < -1.0 && up_hi > 1.0 && crossings(&up[20..]) >= 4, "no up and down bob");
    player.frames(60, &[]);
    let bob = player.vector("WalkBob");
    assert!(bob.iter().all(|c| c.abs() < 0.5), "still bobbing: {bob:?}");
}

#[derive(Debug, Clone, Copy)]
struct ViewSample {
    dt: f32,
    camera: [f32; 3],
    location: [f32; 3],
    eye_height: f32,
    bob: [f32; 3],
    physics: u8,
    just_landed: bool,
    sweep: Option<(f32, f32)>,
}

impl Player<'_> {
    fn view_frame(&mut self, dt: f32, keys: &[&str], mouse: [f32; 2]) -> ViewSample {
        self.frame(dt, keys, mouse);
        let camera = self.vm.player_view().unwrap().expect("no view").location;
        let physics = match self.physics() {
            Value::Byte(b) => b,
            _ => 0,
        };
        let location = self.vector("Location");
        ViewSample {
            dt,
            camera: camera.into(),
            location,
            eye_height: self.float("EyeHeight"),
            bob: self.vector("WalkBob"),
            physics,
            just_landed: self.vm.get(self.pawn, "bJustLanded") == Some(Value::Bool(true)),
            sweep: self.vm.move_check(self.pawn, location.into(), [0.0, 0.0, -37.0].into()).map(|(t, n, _)| (t * 37.0, n[2])),
        }
    }
}

fn game_dt(frame: usize) -> f32 {
    const PATTERN: [f32; 7] = [0.0125, 0.0108, 0.0139, 0.0117, 0.0131, 0.0102, 0.0136];
    PATTERN[frame % PATTERN.len()]
}

fn view_path_stats(samples: &[ViewSample], range: std::ops::Range<usize>) -> ([f32; 2], f32, f32, usize) {
    let mut speed = [f32::MAX, 0.0f32];
    let (mut low, mut high, mut accel) = (f32::MAX, f32::MIN, 0.0f32);
    for i in range.clone() {
        let (s, before, earlier) = (&samples[i], &samples[i - 1], &samples[i - 2]);
        let v = flat(add(s.location, scale(before.location, -1.0))) / s.dt;
        speed = [speed[0].min(v), speed[1].max(v)];
        let height = s.camera[2] - s.bob[2];
        (low, high) = (low.min(height), high.max(height));
        let velocity = |a: &ViewSample, b: &ViewSample| scale(add(b.camera, scale(a.camera, -1.0)), 1.0 / b.dt);
        let change = add(velocity(before, s), scale(velocity(earlier, before), -1.0));
        accel = accel.max(dot(change, change).sqrt() * 2.0 / (s.dt + before.dt));
    }
    let steep = range.filter(|&i| samples[i].physics == 1 && samples[i].sweep.is_some_and(|(_, z)| z < 0.7)).count();
    (speed, high - low, accel, steep)
}

#[test]
fn suburbs_view_path_while_walking() {
    let Some(dir) = game_dir() else { return };
    let (loader, id) = load(&dir, "Suburbs-3");
    let mut player = Player::start(&loader, &dir, id);
    let home = player.vector("Location");
    let ground_speed = player.float("GroundSpeed");
    let yaw = (0..32).map(|i| i * 2048).find(|&yaw| player.clear_run(heading(yaw), 900.0)).expect("no open flat direction");
    let runs: [(&str, fn(usize) -> f32, [f32; 2], i32, usize, bool); 7] = [
        ("60 Hz straight", |_| DT, [0.0, 0.0], 0, 80, true),
        ("game dt straight", game_dt, [0.0, 0.0], 0, 80, true),
        ("60 Hz turning", |_| DT, [3.0, 0.0], 0, 80, true),
        ("game dt turning", game_dt, [3.0, 0.0], 0, 80, true),
        ("game dt other ground 1", game_dt, [0.0, 0.0], 8192, 300, false),
        ("game dt other ground 2", game_dt, [0.0, 0.0], 16384, 300, false),
        ("game dt other ground 3", game_dt, [0.0, 0.0], -16384, 300, false),
    ];
    for (name, dt, mouse, turn, frames, open) in runs {
        player.place(home);
        player.face(yaw.wrapping_add(turn));
        let samples: Vec<ViewSample> = (0..frames).map(|i| player.view_frame(dt(i), &["W"], mouse)).collect();
        let (speed, height, accel, steep) = view_path_stats(&samples, 25..frames);
        eprintln!("{name}: speed {:.2}..{:.2}, camera height less bob changes by {height:.4}, largest acceleration {accel:.0}, {steep} frames on steep floor", speed[0], speed[1]);
        if open {
            assert!(speed[1] - ground_speed < 0.5 && ground_speed - speed[0] < 0.5, "{name}: speed {speed:?}");
            assert!(height < 0.01, "{name}: camera height less bob changed by {height}");
            assert!(accel < 1300.0, "{name}: camera acceleration {accel}");
            assert!(samples[25..].iter().all(|s| (s.eye_height - player.float("BaseEyeHeight")).abs() < 0.01 && !s.just_landed));
        }
    }
}

#[test]
fn church_slide_along_wall() {
    let Some(dir) = game_dir() else { return };
    let (loader, id) = load(&dir, "Church");
    let mut player = Player::start(&loader, &dir, id);
    let (ground_speed, accel_rate, friction) = (player.float("GroundSpeed"), player.float("AccelRate"), player.float("WalkFriction"));
    let extent = player.extent();
    let home = player.vector("Location");
    let candidates = (-30..=30).flat_map(|i| (-30..=30).map(move |j| (i, j)));
    let wall = candidates.flat_map(|(i, j)| (0..8).map(move |k| (i, j, k * 8192))).find_map(|(i, j, yaw)| {
        let at = [home[0] + i as f32 * 64.0, home[1] + j as f32 * 64.0, home[2]];
        let (ground, _) = player.floor([at[0], at[1], at[2] + 40.0]).filter(|(_, n)| n[2] > 0.99)?;
        let at = [at[0], at[1], ground + extent[2] + 6.0];
        let lift = [at[0], at[1], at[2] + 4.0];
        let d = heading(yaw);
        let hit = player.vm.trace_world(lift.into(), add(lift, scale(d, 400.0)).into(), extent.into())?;
        let distance = hit.time * 400.0;
        let along = [hit.normal[1], -hit.normal[0], 0.0];
        let wall_end = add(at, scale(d, distance - 10.0));
        let length = distance + 300.0;
        let wall_goes_on = (0..=(length / 32.0) as usize).all(|k| {
            let from = add(wall_end, scale(along, k as f32 * 32.0));
            player.vm.trace_world(from.into(), add(from, scale(d, 20.0)).into(), extent.into()).is_some_and(|h| dot(h.normal.into(), hit.normal.into()) > 0.999)
        });
        let usable = hit.normal[2].abs() < 0.01
            && distance > 150.0
            && player.vm.trace_world([at[0], at[1], at[2] + 30.0].into(), at.into(), extent.into()).is_none()
            && player.clear_run_from(at, d, distance - 10.0)
            && wall_goes_on
            && player.clear_run_from(wall_end, along, length);
        usable.then_some((at, hit.normal))
    });
    let Some((at, normal)) = wall else { panic!("no wall found near the start") };
    player.place(at);
    let into = scale(normal.into(), -1.0);
    let along = [-into[1], into[0], 0.0];
    let direction = scale(add(into, along), std::f32::consts::FRAC_1_SQRT_2);
    player.face(yaw_of(direction));
    let plane = |p: [f32; 3]| dot(p, normal.into());
    let mut contact = None;
    let mut speeds = Vec::new();
    let mut expected = [0.0; 3];
    for frame in 0..120 {
        let before = player.vector("Location");
        player.frames(1, &["W"]);
        let now = player.vector("Location");
        let velocity = player.vector("Velocity");
        expected = engine_accelerate(expected, direction, accel_rate, friction, ground_speed);
        if contact.is_some() && dot(expected, normal.into()) < 0.0 {
            expected = add(expected, scale(normal.into(), -dot(expected, normal.into())));
        }
        if contact.is_none() && plane(now) - plane(before) > -1e-3 && frame > 0 {
            contact = Some(frame);
            expected = add(expected, scale(normal.into(), -dot(expected, normal.into())));
        }
        if contact.is_some() {
            assert!(dot(velocity, normal.into()) < 1.0, "frame {frame}: bounced off the wall, velocity {velocity:?}");
            speeds.push((dot(velocity, along), dot(expected, along)));
        }
        if player.physics() != PHYS_WALKING {
            break;
        }
    }
    assert!(contact.is_some(), "never reached the wall");
    let settled: Vec<_> = speeds.iter().skip(12).take(10).collect();
    assert!(!settled.is_empty(), "too little sliding to measure");
    for &&(ours, engine) in &settled {
        assert!((ours - engine).abs() < 0.05 * engine.max(1.0), "along-wall speed {ours}, engine {engine}");
    }
}

#[test]
fn church_step_up_and_walk_off() {
    let Some(dir) = game_dir() else { return };
    let (loader, id) = load(&dir, "Church");
    let mut player = Player::start(&loader, &dir, id);
    let extent = player.extent();
    let home = player.vector("Location");
    let probe_z = home[2] + 200.0;
    let floor = |x: f32, y: f32| player.floor([x, y, probe_z]).filter(|(_, n)| n[2] > 0.99).map(|(z, _)| z);
    let (mut step, mut drop) = (None, None);
    'search: for i in -60..=60 {
        for j in -60..=60 {
            let (x, y) = (home[0] + i as f32 * 48.0, home[1] + j as f32 * 48.0);
            let Some(low) = floor(x, y) else { continue };
            for yaw in (0..8).map(|k| k * 8192) {
                let d = heading(yaw);
                let side = |s: f32| floor(x + d[0] * s, y + d[1] * s);
                let stand = [x - d[0] * 80.0, y - d[1] * 80.0, low + extent[2] + 6.0];
                let flat_behind = (1..=4).all(|k| floor(x - d[0] * 20.0 * k as f32, y - d[1] * 20.0 * k as f32) == Some(low));
                if !flat_behind
                    || player.vm.trace_world([stand[0], stand[1], stand[2] + 30.0].into(), stand.into(), extent.into()).is_some()
                    || player.blocking_actor(stand, d, 200.0).is_some()
                {
                    continue;
                }
                match (side(40.0), side(120.0)) {
                    (Some(a), Some(b)) if step.is_none() && (a - b).abs() < 0.5 && (8.0..34.0).contains(&(a - low)) => {
                        let raised = [stand[0], stand[1], a + extent[2] + 4.0];
                        let clear_up = player.vm.trace_world(stand.into(), raised.into(), extent.into()).is_none();
                        if clear_up && player.vm.trace_world(raised.into(), add(raised, scale(d, 160.0)).into(), extent.into()).is_none() {
                            step = Some((stand, yaw, a - low));
                        }
                    }
                    (Some(a), _) if drop.is_none() && low - a > 100.0 => {
                        let lift = [stand[0], stand[1], stand[2] + 4.0];
                        if player.vm.trace_world(lift.into(), add(lift, scale(d, 160.0)).into(), extent.into()).is_none() {
                            drop = Some((stand, yaw, low - a));
                        }
                    }
                    _ => {}
                }
                if step.is_some() && drop.is_some() {
                    break 'search;
                }
            }
        }
    }
    let (stand, yaw, rise) = step.expect("no step found near the start");
    player.place(stand);
    player.face(yaw);
    let z0 = player.vector("Location")[2];
    let mut fell = false;
    let mut frames = 0;
    let eyes = |player: &Player| player.vector("Location")[2] + player.float("EyeHeight");
    let (mut body_step, mut eye_step) = (0.0f32, 0.0f32);
    let (mut last_body, mut last_eyes) = (z0, eyes(&player));
    while dot(add(player.vector("Location"), scale(stand, -1.0)), heading(yaw)) < 180.0 && frames < 60 {
        player.frames(1, &["W"]);
        fell |= player.physics() != PHYS_WALKING;
        frames += 1;
        let (body, eye) = (player.vector("Location")[2], eyes(&player));
        body_step = body_step.max(body - last_body);
        eye_step = eye_step.max(eye - last_eyes);
        (last_body, last_eyes) = (body, eye);
    }
    assert!(body_step > 5.0 && eye_step < body_step * 0.7, "eyes not smoothed: body {body_step}, eyes {eye_step}");
    let at = player.vector("Location");
    let low = player.floor(stand).map_or(f32::MIN, |(z, _)| z);
    let under = player.floor(at).map_or(f32::MIN, |(z, _)| z);
    let gap = player.floor_distance();
    assert!(frames < 60, "stuck at the step");
    assert!(!fell, "stepping up left walking");
    assert!((under - low - rise).abs() < 0.5, "not on the step: floor {under}, step {}", low + rise);
    assert!(gap.is_some_and(|g| (1.85..2.45).contains(&g)), "floor distance {gap:?} on the step");

    let (stand, yaw, _) = drop.expect("no drop found near the start");

    player.place(stand);
    player.face(yaw);
    let mut frames = 0;
    while player.physics() == PHYS_WALKING && frames < 60 {
        player.frames(1, &["W"]);
        frames += 1;
    }
    assert_eq!(player.physics(), PHYS_FALLING, "never fell off the ledge");
    let top = player.vector("Location")[2];
    let mut air = 0;
    while player.physics() == PHYS_FALLING && air < 240 {
        player.frames(1, &[]);
        air += 1;
    }
    let landed = player.vector("Location");
    let fallen = top - landed[2];
    let free_fall = (2.0 * fallen / GRAVITY).sqrt() / DT;
    assert_eq!(player.physics(), PHYS_WALKING, "not walking again after the fall");
    assert!(fallen > 30.0, "fell only {fallen}");
    assert!(air as f32 >= free_fall - 2.0, "fell faster than gravity allows");

    let gap = player.floor_distance();
    assert!(gap.is_some_and(|g| (1.85..2.45).contains(&g)), "floor distance {gap:?} after landing");
}

#[test]
fn church_movers_block_with_their_mesh() {
    let Some(dir) = game_dir() else { return };
    let (loader, id) = load(&dir, "Church");
    let mut player = Player::start(&loader, &dir, id);
    let extent = player.extent();
    let movers: Vec<ObjectId> = player.vm.actors().filter(|&a| player.vm.is_a(a, "Mover") && !player.vm.is_deleted(a)).collect();
    let mut passes = Vec::new();
    for &mover in &movers {
        let centre = vector(&player.vm, mover, "Location");
        for yaw in (0..8).map(|k| k * 8192) {
            let d = heading(yaw);
            let side = [-d[1], d[0], 0.0];
            let start = add(add(centre, scale(side, 120.0)), scale(d, -300.0));
            let Some((ground, n)) = player.floor([start[0], start[1], centre[2] + 100.0]) else { continue };
            let start = [start[0], start[1], ground + extent[2] + 6.0];
            if n[2] < 0.99 || player.vm.trace_world([start[0], start[1], start[2] + 20.0].into(), start.into(), extent.into()).is_some() || !player.clear_run_from(start, d, 600.0) {
                continue;
            }
            if player.vm.actors().any(|a| a != mover && a != player.pawn && player.blocking_actor(start, d, 600.0) == Some(a)) {
                continue;
            }
            player.place(start);
            player.face(yaw);
            let mut closest = f32::MAX;
            for _ in 0..80 {
                player.frames(1, &["W"]);
                let at = player.vector("Location");
                closest = closest.min(flat(add(at, scale(centre, -1.0))));
            }
            let moved = flat(add(player.vector("Location"), scale(start, -1.0)));
            passes.push((player.vm.object(mover).path.clone(), yaw, closest, moved));
            if passes.len() >= 6 {
                break;
            }
        }
        if passes.len() >= 6 {
            break;
        }
    }
    assert!(!passes.is_empty(), "no mover to run past");
    assert!(passes.iter().any(|&(_, _, closest, moved)| closest < 150.0 && moved > 400.0), "every run was stopped near a mover");
}
