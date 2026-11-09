mod common;

use common::load_fixture;
use p2_test_package::token::{INSTANCE_VARIABLE, INT_CONST, LET, LOCAL_VARIABLE, NOTHING, RETURN, STOP};
use p2_test_package::{CPF_OUT_PARM, CPF_PARM, CPF_RETURN_PARM, PackageBuilder, Script};
use p2_vm::{NameId, Value};

#[test]
fn class_with_a_parent_loads_its_defaults() {
    let mut package = PackageBuilder::new("Fixture");
    let base = package.class("Base", None);
    package.int_property(base, "Count", 0);
    package.int_property(base, "Limit", 0);
    package.default_int(base, "Count", 7);
    package.default_int(base, "Limit", 2);
    let derived = package.class("Derived", Some(base));
    package.int_property(derived, "Extra", 0);
    package.default_int(derived, "Count", 9);
    package.default_int(derived, "Extra", 3);

    let mut vm = load_fixture(&package, "defaults");
    let class = vm.find_class("Fixture", "Derived").unwrap();
    let object = vm.new_object(class, None);
    assert_eq!(vm.store.get(object, "Count"), Some(Value::Int(9)));
    assert_eq!(vm.store.get(object, "Limit"), Some(Value::Int(2)));
    assert_eq!(vm.store.get(object, "Extra"), Some(Value::Int(3)));

    assert!(vm.store.set(object, "Count", Value::Int(11)));
    assert_eq!(vm.store.get(object, "Count"), Some(Value::Int(11)));
    assert_eq!(vm.store.default_value(class, "Count"), Some(Value::Int(9)));

    let base = vm.find_class("Fixture", "Base").unwrap();
    let object = vm.new_object(base, None);
    assert_eq!(vm.store.get(object, "Count"), Some(Value::Int(7)));
    assert_eq!(vm.store.get(object, "Extra"), None);
}

#[test]
fn functions_return_a_constant_and_set_a_property() {
    let mut package = PackageBuilder::new("Fixture");
    let class = package.class("Counter", None);
    let count = package.int_property(class, "Count", 0);
    let answer = package.function(class, "Answer", 0);
    package.int_property(answer, "ReturnValue", CPF_PARM | CPF_OUT_PARM | CPF_RETURN_PARM);
    package.script(answer, Script::new().token(RETURN).token(INT_CONST).int(42).token(RETURN).token(NOTHING));
    let set_count = package.function(class, "SetCount", 0);
    let new_count = package.int_property(set_count, "NewCount", CPF_PARM);
    let assign = Script::new().token(LET).token(INSTANCE_VARIABLE).object(count).token(LOCAL_VARIABLE).object(new_count);
    package.script(set_count, assign.token(RETURN).token(NOTHING));

    let mut vm = load_fixture(&package, "functions");
    let class = vm.find_class("Fixture", "Counter").unwrap();
    let object = vm.new_object(class, None);
    let function = vm.find_function(class, "Answer").unwrap().unwrap();
    assert_eq!(vm.call(&function, Some(object), class, []).unwrap(), Some(Value::Int(42)));

    vm.event(object, "SetCount", [Value::Int(5)]).unwrap();
    assert_eq!(vm.store.get(object, "Count"), Some(Value::Int(5)));
}

#[test]
fn state_code_runs_on_tick_until_it_stops() {
    let mut package = PackageBuilder::new("Fixture");
    let class = package.class("Counter", None);
    let count = package.int_property(class, "Count", 0);
    package.default_int(class, "Count", 1);
    let state = package.state(class, "Active");
    let begin = package.name("Begin");
    let code = Script::new().token(LET).token(INSTANCE_VARIABLE).object(count).token(INT_CONST).int(6).token(STOP);
    package.script(state, code.label_table(&[(begin, 0)]));

    let mut vm = load_fixture(&package, "state-code");
    let class = vm.find_class("Fixture", "Counter").unwrap();
    let object = vm.new_object(class, None);
    let active = vm.intern("Active");
    vm.goto_state(object, active, NameId::NONE).unwrap();
    assert_eq!(vm.store.get(object, "Count"), Some(Value::Int(1)));

    vm.tick(object, 0.1).unwrap();
    assert_eq!(vm.store.get(object, "Count"), Some(Value::Int(6)));

    assert!(vm.store.set(object, "Count", Value::Int(8)));
    vm.tick(object, 0.1).unwrap();
    assert_eq!(vm.store.get(object, "Count"), Some(Value::Int(8)));
}
