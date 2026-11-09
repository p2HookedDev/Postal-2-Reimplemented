use p2_math::{Rotator, Vector};
use crate::interp::{float, int, truthy};
use crate::native_id::{Native, NativeGroup};
use crate::{Error, LoaderRef, NameId, ObjectId, Prop, Value, Vm};
use crate::natives::actor::mesh_native;
use crate::natives::actor::placement_native;
use crate::natives::actor::sense_native;
use crate::natives::media::presentation_native;
use crate::natives::media::sound_native;
use crate::natives::platform::platform_native;
use crate::natives::script::lifetime_native;
use crate::natives::script::random_native;
use crate::natives::script::reflect_native;
use crate::natives::script::state_native;
use crate::natives::script::text_native;

mod actor;
mod media;
mod platform;
mod script;

pub const AUDIO_DEFAULT_RADIUS: f32 = 80.0;

pub(crate) fn call<L: LoaderRef>(vm: &mut Vm<L>, name: &str, native: Option<Native>, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
    let Some(native) = native else { return Err(Error::UnknownNative(name.to_owned())) };
    if let Some(value) = crate::operators::operate(native, args) {
        return Ok(Some(value));
    }
    match native.group() {
        NativeGroup::Operator => Err(Error::UnknownNative(name.to_owned())),
        NativeGroup::Text => text_native(native, args),
        NativeGroup::Random => random_native(vm, native, args),
        NativeGroup::Reflect => reflect_native(vm, native, this, args),
        NativeGroup::State => state_native(vm, native, this, args),
        NativeGroup::Lifetime => lifetime_native(vm, native, this, args),
        NativeGroup::Placement => placement_native(vm, native, this, args),
        NativeGroup::Sound => sound_native(vm, native, this, args),
        NativeGroup::Platform => platform_native(vm, native, this, args),
        NativeGroup::Sense => sense_native(vm, native, this, args),
        NativeGroup::Mesh => mesh_native(vm, native, this, args),
        NativeGroup::Presentation => presentation_native(vm, native, this, args),
        NativeGroup::Canvas => vm.canvas_native(native, this, args),
        NativeGroup::Trace => vm.trace_native(native, this, args),
        NativeGroup::Ai => vm.ai_native(native, this, args),
        NativeGroup::Move => vm.move_native(native, this, args),
        NativeGroup::Nav => vm.nav_native(native, this, args),
        NativeGroup::Karma => vm.karma_native(native, this, args),
        NativeGroup::Game => vm.game_native(native, this, args),
        NativeGroup::Util => vm.util_native(native, this, args),
        NativeGroup::HurtRadius => vm.hurt_radius_native(native, this, args),
        NativeGroup::Anim => vm.anim_native(native, this, args),
        NativeGroup::Actor => vm.actor_native(native, this, args),
        NativeGroup::Bone => this.map_or(Ok(None), |this| vm.bone_native(native, this, args)),
        NativeGroup::Lod => this.map_or(Ok(None), |this| vm.lod_native(native, this, args)),
        NativeGroup::KarmaState => this.map_or(Ok(None), |this| vm.karma_state_native(native, this, args)),
        NativeGroup::Movie => this.map_or(Ok(None), |this| vm.movie_native(native, this, args)),
    }
}

pub(crate) fn iterator_outs(name: &str) -> &'static [usize] {
    match name {
        "TraceActors" => &[2, 3],
        _ => &[],
    }
}

/// An actor the loop body spawns, added at the list's end, is reached.
/// For `ChildActors` and `BasedActors` this is (unverified).
pub(crate) fn walks_actor_list(name: &str) -> Option<&'static str> {
    ["AllActors", "DynamicActors", "ChildActors", "BasedActors", "RadiusActors", "VisibleActors"].into_iter().find(|&walk| walk == name)
}

pub(crate) fn iterate<L: LoaderRef>(vm: &mut Vm<L>, name: &str, this: Option<ObjectId>, args: &[Value]) -> Result<Vec<Value>, Error> {
    let class = match args.first() {
        Some(&Value::Object(Some(class))) => vm.store.object(class).name,
        _ => vm.script.lookups.class_names.object,
    };
    let all_objects = name == "AllObjects";
    let tested = std::cell::RefCell::new(vm.script.lookups.class_tests.remove(&class).unwrap_or_default());
    let of_class = |vm: &Vm<L>, candidates: &mut dyn Iterator<Item = ObjectId>, keep: &dyn Fn(ObjectId) -> bool| -> Vec<Value> {
        let mut tested = tested.borrow_mut();
        candidates
            .filter(|&object| {
                let of_class = vm.store.object(object).class.is_some_and(|c| *tested.entry(c).or_insert_with(|| vm.store.class_has_name(c, class)));
                of_class && !vm.store.is_deleted(object) && keep(object)
            })
            .map(|object| Value::Object(Some(object)))
            .collect()
    };
    let objects = |vm: &Vm<L>, keep: &dyn Fn(ObjectId) -> bool| -> Vec<Value> {
        if all_objects { of_class(vm, &mut vm.object_ids(), keep) } else { of_class(vm, &mut vm.actors(), keep) }
    };
    let radius = args.get(1).map_or(0.0, float);
    let centre = |vm: &Vm<L>| match args.get(2) {
        Some(location) if is_struct_value(location) => vec3(Some(location)),
        _ => vec3(this.and_then(|this| vm.store.get(this, Prop::Location))),
    };
    let within = |vm: &Vm<L>, centre: Vector, object: ObjectId| {
        let at = vm.store.vector(object, Prop::Location);
        (0..3).map(|i| (at[i] - centre[i]).powi(2)).sum::<f32>() <= radius * radius
    };
    let items = match name {
        "AllActors" | "DynamicActors" | "AllObjects" => {
            let tag = match args.get(1) {
                Some(&Value::Name(tag)) if tag != NameId::NONE => Some(tag),
                _ => None,
            };
            let dynamic = (name == "DynamicActors").then(|| vm.dynamic_actors());
            let keep = |object: ObjectId| tag.is_none_or(|tag| vm.store.name_of(object, Prop::Tag) == tag);
            match dynamic {
                Some(dynamic) => of_class(vm, &mut dynamic.into_iter(), &keep),
                None => objects(vm, &keep),
            }
        }
        "TouchingActors" => {
            this.map_or_else(Vec::new, |this| vm.store.objects_of(this, Prop::Touching).flatten().filter(|&o| vm.store.is_a(o, class)).map(|o| Value::Object(Some(o))).collect())
        }
        "ChildActors" => objects(vm, &|object| this.is_some() && vm.store.get(object, Prop::Owner) == Some(Value::Object(this))),
        "BasedActors" => objects(vm, &|object| this.is_some() && vm.store.get(object, Prop::Base) == Some(Value::Object(this))),
        // Centre strictly within `Radius` + the actor's own `CollisionRadius`
        // (a match of radius 5 finds a gas puddle of radius 100 around it).
        "RadiusActors" => {
            let centre = centre(vm);
            objects(vm, &|object| {
                let radius = radius + vm.store.float_of(object, Prop::CollisionRadius);
                let at = vm.store.vec3_of(object, Prop::Location);
                (0..3).map(|i| (at[i] - centre[i]).powi(2)).sum::<f32>() < radius * radius
            })
        }
        "CollidingActors" | "VisibleCollidingActors" => {
            let at = centre(vm);
            let near = vm.colliders_near([at[0], at[1]], [at[0], at[1]], radius);
            let visible = name == "VisibleCollidingActors";
            let ignore_hidden = visible && args.get(3).is_some_and(truthy);
            let mut items = of_class(vm, &mut near.into_iter().map(|e| e.actor), &|object| {
                vm.store.flag_of(object, Prop::bCollideActors) && !(ignore_hidden && vm.store.flag_of(object, Prop::bHidden)) && within(vm, at, object)
            });
            if visible {
                vm.keep_line_of_sight(this, at, &mut items);
            }
            items
        }
        "VisibleActors" => {
            let at = centre(vm);
            let mut items = objects(vm, &|object| {
                let offset = vm.store.vector(object, Prop::Location) - at;
                !vm.store.flag_of(object, Prop::bHidden) && (radius == 0.0 || offset.dot(offset) < radius * radius)
            });
            vm.keep_line_of_sight(this, at, &mut items);
            items
        }
        "TraceActors" => vm.trace_actors_items(this, args),
        _ => {
            vm.script.lookups.class_tests.insert(class, tested.into_inner());
            return Err(Error::UnknownNative(name.to_owned()));
        }
    };
    vm.script.lookups.class_tests.insert(class, tested.into_inner());
    Ok(items)
}

pub(crate) fn is_struct_value(value: &Value) -> bool {
    matches!(value, Value::Struct(_) | Value::Vector(_) | Value::Rotator(_))
}

pub(crate) fn vec3(value: Option<impl std::borrow::Borrow<Value>>) -> Vector {
    match value.as_ref().map(std::borrow::Borrow::borrow) {
        Some(&Value::Vector(v)) => v,
        Some(&Value::Rotator(r)) => Vector::new(r.pitch as f32, r.yaw as f32, r.roll as f32),
        Some(Value::Struct(members)) => [0, 1, 2].map(|i| members.get(i).map_or(0.0, float)).into(),
        _ => Vector::ZERO,
    }
}

pub(crate) fn object_arg(args: &[Value], n: usize) -> Option<ObjectId> {
    match args.get(n) {
        Some(&Value::Object(object)) => object,
        _ => None,
    }
}

pub(crate) fn given(args: &[Value], n: usize) -> bool {
    !matches!(args.get(n), None | Some(Value::Map))
}

pub(crate) fn optional_float(args: &[Value], n: usize, default: f32) -> f32 {
    if given(args, n) { float_arg(args, n) } else { default }
}

pub(crate) fn bool_arg(args: &[Value], n: usize) -> bool {
    args.get(n).is_some_and(truthy)
}

pub(crate) fn int_arg(args: &[Value], n: usize) -> i32 {
    args.get(n).map_or(0, int)
}

pub(crate) fn float_arg(args: &[Value], n: usize) -> f32 {
    args.get(n).map_or(0.0, float)
}

pub(crate) fn str_arg(args: &[Value], n: usize) -> &str {
    match args.get(n) {
        Some(Value::Str(text)) => text.as_str(),
        _ => "",
    }
}

pub(crate) fn name_arg(args: &[Value], n: usize) -> NameId {
    match args.get(n) {
        Some(&Value::Name(name)) => name,
        _ => NameId::NONE,
    }
}

pub(crate) fn text_arg(args: &[Value], n: usize) -> String {
    match args.get(n) {
        Some(Value::Str(text)) => text.clone(),
        _ => String::new(),
    }
}

pub(crate) fn vector_arg(args: &[Value], n: usize) -> Option<Vector> {
    match args.get(n) {
        Some(value @ (Value::Struct(_) | Value::Vector(_) | Value::Rotator(_))) => Some(vec3(Some(value))),
        _ => None,
    }
}

pub(crate) fn rot3(value: Option<impl std::borrow::Borrow<Value>>) -> Rotator {
    match value.as_ref().map(std::borrow::Borrow::borrow) {
        Some(&Value::Rotator(r)) => r,
        other => {
            let v = vec3(other);
            Rotator::new(v.x as i32, v.y as i32, v.z as i32)
        }
    }
}

pub(crate) fn system_time() -> [i32; 8] {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let small = |value: u32| value as i32;
    [
        now.year(),
        small(now.month()),
        small(now.weekday().num_days_from_sunday()),
        small(now.day()),
        small(now.hour()),
        small(now.minute()),
        small(now.second()),
        small((now.nanosecond() / 1_000_000).min(999)),
    ]
}

/// Runs before actors begin play and at the start of every level tick (unverified).
pub(crate) fn update_time<L: LoaderRef>(vm: &mut Vm<L>, level: ObjectId) {
    let properties = [Prop::Year, Prop::Month, Prop::DayOfWeek, Prop::Day, Prop::Hour, Prop::Minute, Prop::Second, Prop::Millisecond];
    for (property, value) in properties.into_iter().zip(system_time()) {
        vm.store.set(level, property, Value::Int(value));
    }
}

/// As the engine counts: the day of the month and every month up to and including the current one (30 days for April,
/// June, September, November; February 29 when the years since 2000 divide by 4, else 28; others 31), and 366 or 365
/// days for each year since 2000 alike.
fn milliseconds_since_2000<L: LoaderRef>(vm: &mut Vm<L>, level: Option<ObjectId>) -> [f32; 3] {
    let [year, month, _, day, hour, minute, second, millisecond] = system_time();
    if let Some(level) = level {
        update_time(vm, level);
    }
    const DAY: f64 = 86_400.0;
    let years = year - 2000;
    let mut total = f64::from(day) * DAY + f64::from(hour) * 3600.0 + f64::from(minute) * 60.0 + f64::from(second) + f64::from(millisecond) / 1000.0;
    for m in 1..=month {
        total += DAY * match m {
            4 | 6 | 9 | 11 => 30.0,
            2 if years % 4 == 0 => 29.0,
            2 => 28.0,
            _ => 31.0,
        };
    }
    for y in 1..=years {
        total += DAY * if y % 4 == 0 { 366.0 } else { 365.0 };
    }
    let whole_days = (total / DAY) as i32;
    let rest = total - f64::from(whole_days) * DAY;
    let seconds = rest as i32;
    [whole_days as f32, seconds as f32, ((rest - f64::from(seconds)) * 1000.0) as i32 as f32]
}

pub(crate) fn orthogonal_rotation(x: Vector, y: Vector, z: Vector) -> Rotator {
    let units = |angle: f32| (angle * 32768.0 / std::f32::consts::PI) as i32;
    let (pitch, yaw) = (units(x[2].atan2((x[0] * x[0] + x[1] * x[1]).sqrt())), units(x[1].atan2(x[0])));
    let unrolled = Rotator::new(pitch, yaw, 0).axes();
    let roll = units(z.dot(unrolled[1]).atan2(y.dot(unrolled[1])));
    [pitch, yaw, roll].into()
}

#[cfg(test)]
mod route_tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use p2_package::Loader;

    use super::*;

    const UNROUTED: [&str; 3] = ["AtEqual_StrStr", "ConcatEqual_StrStr", "Initialize"];

    #[test]
    fn every_native_keeps_its_route() {
        let empty = std::env::temp_dir();
        let loader = Loader::new(&empty, &[]).unwrap();
        let mut unrouted = Vec::new();
        for &native in Native::ALL {
            let name = format!("{native:?}");
            let mut vm = Vm::new(&loader);
            let mut args = vec![Value::Int(0); 8];
            let result = catch_unwind(AssertUnwindSafe(|| call(&mut vm, &name, Some(native), None, &mut args)));
            if matches!(result, Ok(Err(Error::UnknownNative(_)))) {
                unrouted.push(name);
            }
        }
        unrouted.sort();
        assert_eq!(unrouted, UNROUTED);
    }
}
