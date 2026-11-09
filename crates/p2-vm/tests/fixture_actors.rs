mod common;

use common::load_fixture;
use p2_math::{Rotator, Vector};
use p2_test_package::token::{CONTEXT, INSTANCE_VARIABLE, INT_CONST, LET, LOCAL_VARIABLE, NOTHING, RETURN};
use p2_script::flags::CPF_PARM;
use p2_test_package::{DefaultValue, PackageBuilder, PropertyType, Script};
use p2_vm::Value;

const PHYS_FALLING: u8 = 2;
const PROBE_TICK: u32 = 36;
const START: Vector = Vector::new(0.0, 0.0, 1000.0);
const ONE_SUBSTEP: f32 = 1.0 / 32.0;

fn declare_structs(package: &mut PackageBuilder) -> (i32, i32) {
    let vector = package.struct_type("Vector");
    for member in ["X", "Y", "Z"] {
        package.property(vector, member, PropertyType::Float, 0);
    }
    let rotator = package.struct_type("Rotator");
    for member in ["Pitch", "Yaw", "Roll"] {
        package.property(rotator, member, PropertyType::Int, 0);
    }
    (vector, rotator)
}

fn actor_package() -> (PackageBuilder, i32) {
    let mut package = PackageBuilder::new("Fixture");
    let (vector, rotator) = declare_structs(&mut package);
    let actor = package.class("Actor", None);
    for name in ["Location", "Velocity", "Acceleration"] {
        package.property(actor, name, PropertyType::Struct(vector), 0);
    }
    package.property(actor, "Rotation", PropertyType::Struct(rotator), 0);
    package.property(actor, "Physics", PropertyType::Byte, 0);
    for name in ["AirControl", "AccelRate", "GroundSpeed", "CollisionRadius", "CollisionHeight"] {
        package.property(actor, name, PropertyType::Float, 0);
    }
    package.property(actor, "bJustTeleported", PropertyType::Bool, 0);
    (package, actor)
}

fn fall_one_substep(dir: &str, velocity: Vector) -> (Vector, Vector) {
    let (mut package, actor) = actor_package();
    let pawn = package.class("Pawn", Some(actor));
    package.default_value(pawn, "Physics", DefaultValue::Byte(PHYS_FALLING));
    let mut vm = load_fixture(&package, dir);
    let class = vm.find_class("Fixture", "Pawn").unwrap();
    let pawn = vm.spawn(class, None, [Some(Value::Vector(START)), None]).unwrap().unwrap();
    assert!(vm.store.set(pawn, "Velocity", Value::Vector(velocity)));
    vm.tick_actor(pawn, ONE_SUBSTEP).unwrap();
    (vm.store.location(pawn), vm.velocity(pawn))
}

fn assert_near(actual: Vector, expected: [f32; 3]) {
    let close = <[f32; 3]>::from(actual).iter().zip(expected).all(|(a, e)| (a - e).abs() < 1e-3);
    assert!(close, "{actual:?} is not {expected:?}");
}

#[test]
fn defaults_of_every_property_type_load() {
    let mut package = PackageBuilder::new("Fixture");
    let (vector, rotator) = declare_structs(&mut package);
    let other = package.class("Other", None);
    let holder = package.class("Holder", None);
    package.property(holder, "Speed", PropertyType::Float, 0);
    package.property(holder, "Mode", PropertyType::Byte, 0);
    package.property(holder, "bOn", PropertyType::Bool, 0);
    package.property(holder, "bOff", PropertyType::Bool, 0);
    package.property(holder, "Label", PropertyType::Name, 0);
    package.property(holder, "Target", PropertyType::Object, 0);
    package.property(holder, "Spot", PropertyType::Struct(vector), 0);
    package.property(holder, "Turn", PropertyType::Struct(rotator), 0);
    let label = package.name("Hello");
    package.default_value(holder, "Speed", DefaultValue::Float(1.5));
    package.default_value(holder, "Mode", DefaultValue::Byte(9));
    package.default_value(holder, "bOn", DefaultValue::Bool(true));
    package.default_value(holder, "bOff", DefaultValue::Bool(false));
    package.default_value(holder, "Label", DefaultValue::Name(label));
    package.default_value(holder, "Target", DefaultValue::Object(other));
    package.default_value(holder, "Spot", DefaultValue::Vector([1.0, 2.0, 3.0]));
    package.default_value(holder, "Turn", DefaultValue::Rotator([4, 5, 6]));

    let mut vm = load_fixture(&package, "actors-defaults");
    let class = vm.find_class("Fixture", "Holder").unwrap();
    let object = vm.new_object(class, None);
    let hello = vm.intern("Hello");
    assert_eq!(vm.store.get(object, "Speed"), Some(Value::Float(1.5)));
    assert_eq!(vm.store.get(object, "Mode"), Some(Value::Byte(9)));
    assert_eq!(vm.store.get(object, "bOn"), Some(Value::Bool(true)));
    assert_eq!(vm.store.get(object, "bOff"), Some(Value::Bool(false)));
    assert_eq!(vm.store.get(object, "Label"), Some(Value::Name(hello)));
    assert_eq!(vm.store.get(object, "Spot"), Some(Value::Vector(Vector::new(1.0, 2.0, 3.0))));
    assert_eq!(vm.store.get(object, "Turn"), Some(Value::Rotator(Rotator::new(4, 5, 6))));
    let Some(Value::Object(Some(target))) = vm.store.get(object, "Target") else { panic!("no object default") };
    assert_eq!(vm.store.object(target).path, "Fixture.Other");
}

#[test]
fn falling_pawn_gains_gravity_and_keeps_its_horizontal_speed() {
    let (location, velocity) = fall_one_substep("actors-fall", Vector::new(100.0, 0.0, 0.0));
    assert_near(location, [3.125, 0.0, 999.267_6]);
    assert_near(velocity, [100.0, 0.0, -46.875]);
}

#[test]
fn falling_pawn_stops_at_terminal_velocity() {
    let (location, velocity) = fall_one_substep("actors-terminal", Vector::new(0.0, 0.0, -3000.0));
    assert_near(location, [0.0, 0.0, 905.517_6]);
    assert_near(velocity, [0.0, 0.0, -2500.0]);
}

#[test]
fn tick_level_runs_tick_events_in_spawn_order_with_the_clamped_delta() {
    let (mut package, actor) = actor_package();
    let ticker = package.class("Ticker", Some(actor));
    let partner = package.property(ticker, "Partner", PropertyType::Object, 0);
    let mark = package.property(ticker, "Mark", PropertyType::Int, 0);
    let seen = package.property(ticker, "Seen", PropertyType::Int, 0);
    let last_delta = package.property(ticker, "LastDelta", PropertyType::Float, 0);
    package.probe(ticker, PROBE_TICK);
    let tick = package.function(ticker, "Tick", 0);
    let delta = package.property(tick, "DeltaTime", PropertyType::Float, CPF_PARM);
    let see_partner = Script::new().token(LET).token(INSTANCE_VARIABLE).object(seen).token(CONTEXT).token(INSTANCE_VARIABLE).object(partner);
    let context_skip_and_size = see_partner.token(0).token(0).token(0);
    let set_mark = context_skip_and_size.token(INSTANCE_VARIABLE).object(mark).token(LET).token(INSTANCE_VARIABLE).object(mark).token(INT_CONST).int(1);
    let keep_delta = set_mark.token(LET).token(INSTANCE_VARIABLE).object(last_delta).token(LOCAL_VARIABLE).object(delta);
    package.script(tick, keep_delta.token(RETURN).token(NOTHING));

    let mut vm = load_fixture(&package, "actors-tick-level");
    let class = vm.find_class("Fixture", "Ticker").unwrap();
    let first = vm.spawn(class, None, [None, None]).unwrap().unwrap();
    let second = vm.spawn(class, None, [None, None]).unwrap().unwrap();
    assert!(vm.store.set(first, "Partner", Value::Object(Some(second))));
    assert!(vm.store.set(second, "Partner", Value::Object(Some(first))));

    let errors = vm.tick_level(1.0);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(vm.store.get(first, "Seen"), Some(Value::Int(0)));
    assert_eq!(vm.store.get(second, "Seen"), Some(Value::Int(1)));
    assert_eq!(vm.store.get(first, "LastDelta"), Some(Value::Float(0.4)));
    assert_eq!(vm.store.get(second, "LastDelta"), Some(Value::Float(0.4)));
}
