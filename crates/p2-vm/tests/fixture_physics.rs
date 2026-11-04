mod common;

use std::sync::Arc;

use common::load_fixture;
use p2_math::Vector;
use p2_package::Loader;
use p2_test_package::token::{INSTANCE_VARIABLE, LET, LOCAL_VARIABLE, NOTHING, RETURN};
use p2_test_package::{CPF_PARM, DefaultValue, PackageBuilder, PropertyType, Script};
use p2_vm::{ObjectId, Value, Vm};

const PHYS_WALKING: u8 = 1;
const PHYS_FALLING: u8 = 2;
const PROBE_HIT_WALL: u32 = 9;
const PROBE_LANDED: u32 = 11;
const PROBE_TICK: u32 = 36;
const ONE_SUBSTEP: f32 = 1.0 / 32.0;
const GRAVITY_Z: f32 = -1500.0;
const MOVE_TEST_ADJUST: f32 = 2.0;
const CYLINDER_PULL_BACK: f32 = 0.001;
const START: Vector = Vector::new(0.0, 0.0, 1000.0);
const PAWN_RADIUS: f32 = 10.0;
const PAWN_HEIGHT: f32 = 20.0;

fn actor_package() -> (PackageBuilder, i32, i32) {
    let mut package = PackageBuilder::new("Fixture");
    let vector = package.struct_type("Vector");
    for member in ["X", "Y", "Z"] {
        package.property(vector, member, PropertyType::Float, 0);
    }
    let rotator = package.struct_type("Rotator");
    for member in ["Pitch", "Yaw", "Roll"] {
        package.property(rotator, member, PropertyType::Int, 0);
    }
    let actor = package.class("Actor", None);
    for name in ["Location", "Velocity", "Acceleration"] {
        package.property(actor, name, PropertyType::Struct(vector), 0);
    }
    package.property(actor, "Rotation", PropertyType::Struct(rotator), 0);
    package.property(actor, "Physics", PropertyType::Byte, 0);
    package.property(actor, "Base", PropertyType::Object, 0);
    for name in ["AirControl", "AccelRate", "GroundSpeed", "CollisionRadius", "CollisionHeight"] {
        package.property(actor, name, PropertyType::Float, 0);
    }
    for name in ["bJustTeleported", "bCollideActors", "bBlockActors", "bBlockNonZeroExtentTraces", "bAlwaysTick"] {
        package.property(actor, name, PropertyType::Bool, 0);
    }
    (package, actor, vector)
}

fn collides(package: &mut PackageBuilder, class: i32, radius: f32, height: f32) {
    package.default_value(class, "CollisionRadius", DefaultValue::Float(radius));
    package.default_value(class, "CollisionHeight", DefaultValue::Float(height));
    for flag in ["bCollideActors", "bBlockActors", "bBlockNonZeroExtentTraces"] {
        package.default_value(class, flag, DefaultValue::Bool(true));
    }
}

fn pawn_and_blocker(dir: &str, pawn_at: Vector, blocker_at: Vector, [radius, height]: [f32; 2]) -> (Vm<Arc<Loader>>, ObjectId, ObjectId) {
    let (mut package, actor, vector) = actor_package();
    let blocker = package.class("Blocker", Some(actor));
    collides(&mut package, blocker, radius, height);
    let pawn = package.class("Pawn", Some(actor));
    collides(&mut package, pawn, PAWN_RADIUS, PAWN_HEIGHT);
    package.default_value(pawn, "Physics", DefaultValue::Byte(PHYS_FALLING));
    let landed_normal = package.property(pawn, "LandedNormal", PropertyType::Struct(vector), 0);
    let wall_normal = package.property(pawn, "WallNormal", PropertyType::Struct(vector), 0);
    let wall_actor = package.property(pawn, "WallActor", PropertyType::Object, 0);
    package.probe(pawn, PROBE_LANDED);
    package.probe(pawn, PROBE_HIT_WALL);

    let landed = package.function(pawn, "Landed", 0);
    let normal = package.property(landed, "HitNormal", PropertyType::Struct(vector), CPF_PARM);
    let keep_normal = Script::new().token(LET).token(INSTANCE_VARIABLE).object(landed_normal).token(LOCAL_VARIABLE).object(normal);
    package.script(landed, keep_normal.token(RETURN).token(NOTHING));

    let hit_wall = package.function(pawn, "HitWall", 0);
    let normal = package.property(hit_wall, "HitNormal", PropertyType::Struct(vector), CPF_PARM);
    let wall = package.property(hit_wall, "Wall", PropertyType::Object, CPF_PARM);
    let keep_normal = Script::new().token(LET).token(INSTANCE_VARIABLE).object(wall_normal).token(LOCAL_VARIABLE).object(normal);
    let keep_wall = keep_normal.token(LET).token(INSTANCE_VARIABLE).object(wall_actor).token(LOCAL_VARIABLE).object(wall);
    package.script(hit_wall, keep_wall.token(RETURN).token(NOTHING));

    let mut vm = load_fixture(&package, dir);
    let class = vm.find_class("Fixture", "Blocker").unwrap();
    let blocker = vm.spawn(class, None, [Some(Value::Vector(blocker_at)), None]).unwrap().unwrap();
    let class = vm.find_class("Fixture", "Pawn").unwrap();
    let pawn = vm.spawn(class, None, [Some(Value::Vector(pawn_at)), None]).unwrap().unwrap();
    (vm, pawn, blocker)
}

fn falling_after(dir: &str, class: &str, velocity: Vector, acceleration: Vector, delta: f32) -> (Vm<Arc<Loader>>, ObjectId) {
    let (mut package, actor, _) = actor_package();
    let pawn = package.class("Pawn", Some(actor));
    for falls in [actor, pawn] {
        package.default_value(falls, "Physics", DefaultValue::Byte(PHYS_FALLING));
    }
    let mut vm = load_fixture(&package, dir);
    let class = vm.find_class("Fixture", class).unwrap();
    let object = vm.spawn(class, None, [Some(Value::Vector(START)), None]).unwrap().unwrap();
    assert!(vm.set(object, "Velocity", Value::Vector(velocity)));
    assert!(vm.set(object, "Acceleration", Value::Vector(acceleration)));
    vm.tick_actor(object, delta).unwrap();
    (vm, object)
}

fn level_and_tickers(dir: &str, dilation: f32) -> (Vm<Arc<Loader>>, ObjectId, ObjectId, ObjectId) {
    let (mut package, actor, _) = actor_package();
    let level = package.class("LevelInfo", Some(actor));
    for name in ["TimeDilation", "TimeSeconds", "TimeSecondsAlways"] {
        package.property(level, name, PropertyType::Float, 0);
    }
    package.property(level, "Pauser", PropertyType::Object, 0);
    let ticker = package.class("Ticker", Some(actor));
    let last_delta = package.property(ticker, "LastDelta", PropertyType::Float, 0);
    package.probe(ticker, PROBE_TICK);
    let tick = package.function(ticker, "Tick", 0);
    let delta = package.property(tick, "DeltaTime", PropertyType::Float, CPF_PARM);
    let keep_delta = Script::new().token(LET).token(INSTANCE_VARIABLE).object(last_delta).token(LOCAL_VARIABLE).object(delta);
    package.script(tick, keep_delta.token(RETURN).token(NOTHING));

    let mut vm = load_fixture(&package, dir);
    let class = vm.find_class("Fixture", "LevelInfo").unwrap();
    let level = vm.new_object(class, None);
    assert!(vm.set(level, "TimeDilation", Value::Float(dilation)));
    vm.set_level(level);
    let class = vm.find_class("Fixture", "Ticker").unwrap();
    let first = vm.spawn(class, None, [None, None]).unwrap().unwrap();
    let second = vm.spawn(class, None, [None, None]).unwrap().unwrap();
    (vm, level, first, second)
}

fn vector_of(vm: &Vm<Arc<Loader>>, object: ObjectId, property: &str) -> Vector {
    match vm.get(object, property) {
        Some(Value::Vector(value)) => value,
        other => panic!("{property}: {other:?}"),
    }
}

fn assert_near(actual: Vector, expected: [f32; 3], tolerance: f32) {
    let close = <[f32; 3]>::from(actual).iter().zip(expected).all(|(a, e)| (a - e).abs() < tolerance);
    assert!(close, "{actual:?} is not {expected:?}");
}

#[test]
fn falling_pawn_lands_on_a_blocking_actor_and_walks() {
    const FLOOR_Z: f32 = 100.0;
    const FLOOR_RADIUS: f32 = 100.0;
    const FLOOR_HEIGHT: f32 = 10.0;
    const FALL_SPEED: f32 = -1000.0;
    let contact_z = FLOOR_Z + FLOOR_HEIGHT + PAWN_HEIGHT;
    let start = Vector::new(0.0, 0.0, contact_z + 20.0);
    let floor_at = Vector::new(0.0, 0.0, FLOOR_Z);
    let (mut vm, pawn, floor) = pawn_and_blocker("physics-landing", start, floor_at, [FLOOR_RADIUS, FLOOR_HEIGHT]);
    assert!(vm.set(pawn, "Velocity", Value::Vector(Vector::new(0.0, 0.0, FALL_SPEED))));

    vm.tick_actor(pawn, ONE_SUBSTEP).unwrap();

    let half_step = FALL_SPEED + GRAVITY_Z * 0.5 * ONE_SUBSTEP;
    let test_length = -half_step * ONE_SUBSTEP + MOVE_TEST_ADJUST;
    let rest_z = contact_z + MOVE_TEST_ADJUST + test_length * CYLINDER_PULL_BACK;
    assert_near(vm.location(pawn), [0.0, 0.0, rest_z], 1e-3);
    assert_near(vm.velocity(pawn), [0.0, 0.0, half_step], 0.05);
    assert_eq!(vm.get(pawn, "Physics"), Some(Value::Byte(PHYS_WALKING)));
    assert_near(vector_of(&vm, pawn, "LandedNormal"), [0.0, 0.0, 1.0], 1e-6);
    assert_eq!(vm.get(pawn, "Base"), Some(Value::Object(Some(floor))));

    let landed_at = vm.location(pawn);
    vm.tick_actor(pawn, ONE_SUBSTEP).unwrap();
    assert_eq!(vm.location(pawn), landed_at);
}

#[test]
fn falling_pawn_stops_at_a_blocking_actors_side_and_gets_hit_wall() {
    const WALL_X: f32 = 100.0;
    const WALL_RADIUS: f32 = 20.0;
    const WALL_HEIGHT: f32 = 200.0;
    const START_X: f32 = 60.0;
    const START_Z: f32 = 500.0;
    const SPEED_X: f32 = 640.0;
    let contact_x = WALL_X - WALL_RADIUS - PAWN_RADIUS;
    let start = Vector::new(START_X, 0.0, START_Z);
    let wall_at = Vector::new(WALL_X, 0.0, START_Z);
    let (mut vm, pawn, wall) = pawn_and_blocker("physics-wall", start, wall_at, [WALL_RADIUS, WALL_HEIGHT]);
    assert!(vm.set(pawn, "Velocity", Value::Vector(Vector::new(SPEED_X, 0.0, 0.0))));

    vm.tick_actor(pawn, ONE_SUBSTEP).unwrap();

    let half_step_z = GRAVITY_Z * 0.5 * ONE_SUBSTEP;
    let (move_x, move_z) = (SPEED_X * ONE_SUBSTEP, half_step_z * ONE_SUBSTEP);
    let direction_x = move_x / move_x.hypot(move_z);
    let test_x = move_x + direction_x * MOVE_TEST_ADJUST;
    let rest_x = contact_x - direction_x * MOVE_TEST_ADJUST - test_x * CYLINDER_PULL_BACK;
    assert_near(vm.location(pawn), [rest_x, 0.0, START_Z + move_z], 1e-3);
    assert_near(vm.velocity(pawn), [(rest_x - START_X) / ONE_SUBSTEP, 0.0, 2.0 * half_step_z], 0.01);
    assert_eq!(vm.get(pawn, "Physics"), Some(Value::Byte(PHYS_FALLING)));
    assert_near(vector_of(&vm, pawn, "WallNormal"), [-1.0, 0.0, 0.0], 1e-6);
    assert_eq!(vm.get(pawn, "WallActor"), Some(Value::Object(Some(wall))));

    for _ in 0..8 {
        vm.tick_actor(pawn, ONE_SUBSTEP).unwrap();
        assert!(vm.location(pawn)[0] < contact_x, "{:?} went through the wall", vm.location(pawn));
    }
}

#[test]
fn falling_actor_takes_its_acceleration_and_gravity_over_half_a_step() {
    let velocity = Vector::new(100.0, 0.0, 0.0);
    let acceleration = Vector::new(320.0, 0.0, 0.0);
    let (vm, actor) = falling_after("physics-actor-fall", "Actor", velocity, acceleration, ONE_SUBSTEP);
    assert_near(vm.location(actor), [3.281_25, 0.0, 999.267_6], 1e-3);
    assert_near(vm.velocity(actor), [110.0, 0.0, -46.875], 1e-3);
    assert_near(vector_of(&vm, actor, "Acceleration"), [320.0, 0.0, 0.0], 1e-6);
}

#[test]
fn falling_actor_stops_at_terminal_velocity_after_the_move() {
    let velocity = Vector::new(0.0, 0.0, -3000.0);
    let (vm, actor) = falling_after("physics-actor-terminal", "Actor", velocity, Vector::ZERO, ONE_SUBSTEP);
    assert_near(vm.location(actor), [0.0, 0.0, 905.517_6], 1e-3);
    assert_near(vm.velocity(actor), [0.0, 0.0, -2500.0], 1e-3);
}

#[test]
fn delta_of_two_substeps_adds_up_to_free_fall() {
    let velocity = Vector::new(100.0, 0.0, 0.0);
    let (vm, pawn) = falling_after("physics-two-substeps", "Pawn", velocity, Vector::ZERO, 2.0 * ONE_SUBSTEP);
    assert_near(vm.location(pawn), [6.25, 0.0, 997.070_3], 1e-3);
    assert_near(vm.velocity(pawn), [100.0, 0.0, -93.75], 1e-3);
}

#[test]
fn pawn_and_actor_clamp_to_terminal_velocity_in_every_substep() {
    const DELTA: f32 = 0.125;
    let velocity = Vector::new(0.0, 0.0, -2490.0);
    for (dir, class) in [("physics-substeps-pawn", "Pawn"), ("physics-substeps-actor", "Actor")] {
        let (vm, falling) = falling_after(dir, class, velocity, Vector::ZERO, DELTA);
        assert_near(vm.location(falling), [0.0, 0.0, 684.015_6], 1e-2);
        assert_near(vm.velocity(falling), [0.0, 0.0, -2500.0], 1e-3);
    }
}

#[test]
fn tick_level_scales_the_delta_by_time_dilation() {
    let (mut vm, level, first, second) = level_and_tickers("physics-level-dilation", 0.5);
    let errors = vm.tick_level(0.25);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(vm.get(level, "TimeSeconds"), Some(Value::Float(0.125)));
    assert_eq!(vm.get(level, "TimeSecondsAlways"), Some(Value::Float(0.125)));
    assert_eq!(vm.get(first, "LastDelta"), Some(Value::Float(0.125)));
    assert_eq!(vm.get(second, "LastDelta"), Some(Value::Float(0.125)));

    let errors = vm.tick_level(0.25);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(vm.get(level, "TimeSeconds"), Some(Value::Float(0.25)));
}

#[test]
fn tick_level_advances_time_by_the_dilated_delta_before_the_tick_clamp() {
    let (mut vm, level, first, _) = level_and_tickers("physics-level-clamp", 2.0);
    let errors = vm.tick_level(0.25);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(vm.get(level, "TimeSeconds"), Some(Value::Float(0.5)));
    assert_eq!(vm.get(first, "LastDelta"), Some(Value::Float(0.4)));
}

#[test]
fn pauser_stops_time_and_ticks_except_always_tick_actors() {
    let (mut vm, level, always, other) = level_and_tickers("physics-level-pause", 1.0);
    assert!(vm.set(always, "bAlwaysTick", Value::Bool(true)));
    assert!(vm.set(level, "Pauser", Value::Object(Some(other))));
    assert!(vm.paused());

    let errors = vm.tick_level(0.25);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(vm.get(level, "TimeSeconds"), Some(Value::Float(0.0)));
    assert_eq!(vm.get(level, "TimeSecondsAlways"), Some(Value::Float(0.25)));
    assert_eq!(vm.get(always, "LastDelta"), Some(Value::Float(0.25)));
    assert_eq!(vm.get(other, "LastDelta"), Some(Value::Float(0.0)));

    assert!(vm.set(level, "Pauser", Value::Object(None)));
    assert!(!vm.paused());
    let errors = vm.tick_level(0.25);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(vm.get(level, "TimeSeconds"), Some(Value::Float(0.25)));
    assert_eq!(vm.get(level, "TimeSecondsAlways"), Some(Value::Float(0.5)));
    assert_eq!(vm.get(other, "LastDelta"), Some(Value::Float(0.25)));
}
