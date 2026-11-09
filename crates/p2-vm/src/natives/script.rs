use p2_math::Vector;
use crate::native_id::Native;
use crate::{Error, LoaderRef, NameId, ObjectId, Prop, Value, Vm};
use super::{bool_arg, float_arg, int_arg, is_struct_value, name_arg, object_arg, str_arg};

pub(super) fn text_native(native: Native, args: &mut [Value]) -> Result<Option<Value>, Error> {
    let value = match native {
        Native::SplitString => {
            let (text, delim) = (str_arg(args, 0), str_arg(args, 1));
            let pieces: Vec<Value> = if delim.is_empty() { vec![Value::Str(text.to_owned())] } else { text.split(delim).map(|p| Value::Str(p.to_owned())).collect() };
            let count = pieces.len() as i32;
            if let Some(slot) = args.get_mut(2) {
                *slot = Value::Array(pieces);
            }
            Value::Int(count)
        }
        Native::SplitStringSingle => {
            let (text, delim) = (str_arg(args, 0), str_arg(args, 1));
            match (!delim.is_empty()).then(|| text.split_once(delim)).flatten() {
                Some((head, rest)) => {
                    let (head, rest) = (head.to_owned(), rest.to_owned());
                    args[0] = Value::Str(rest);
                    if let Some(slot) = args.get_mut(2) {
                        *slot = Value::Str(head);
                    }
                    Value::Bool(true)
                }
                None => Value::Bool(false),
            }
        }
        Native::Concat_StrStr => Value::Str([str_arg(args, 0), str_arg(args, 1)].concat()),
        Native::At_StrStr => Value::Str(format!("{} {}", str_arg(args, 0), str_arg(args, 1))),
        Native::EqualEqual_StrStr => Value::Bool(str_arg(args, 0) == str_arg(args, 1)),
        Native::NotEqual_StrStr => Value::Bool(str_arg(args, 0) != str_arg(args, 1)),
        Native::Less_StrStr => Value::Bool(str_arg(args, 0) < str_arg(args, 1)),
        Native::Greater_StrStr => Value::Bool(str_arg(args, 0) > str_arg(args, 1)),
        Native::LessEqual_StrStr => Value::Bool(str_arg(args, 0) <= str_arg(args, 1)),
        Native::GreaterEqual_StrStr => Value::Bool(str_arg(args, 0) >= str_arg(args, 1)),
        Native::ComplementEqual_StrStr => Value::Bool(str_arg(args, 0).eq_ignore_ascii_case(str_arg(args, 1))),
        Native::Len => Value::Int(str_arg(args, 0).chars().count() as i32),
        Native::Caps => Value::Str(str_arg(args, 0).to_uppercase()),
        Native::Locs => Value::Str(str_arg(args, 0).to_lowercase()),
        Native::Left => Value::Str(str_arg(args, 0).chars().take(int_arg(args, 1).max(0) as usize).collect()),
        Native::Right => {
            let text = str_arg(args, 0);
            let count = text.chars().count();
            Value::Str(text.chars().skip(count.saturating_sub(int_arg(args, 1).max(0) as usize)).collect())
        }
        Native::Mid => {
            let count = if args.len() > 2 && int_arg(args, 2) > 0 { int_arg(args, 2) as usize } else { usize::MAX };
            Value::Str(str_arg(args, 0).chars().skip(int_arg(args, 1).max(0) as usize).take(count).collect())
        }
        Native::Asc => Value::Int(str_arg(args, 0).chars().next().map_or(0, |c| c as i32)),
        Native::Chr => Value::Str(char::from_u32(int_arg(args, 0).max(0) as u32).map(String::from).unwrap_or_default()),
        Native::InStr => Value::Int(str_arg(args, 0).find(str_arg(args, 1)).map_or(-1, |at| str_arg(args, 0)[..at].chars().count() as i32)),
        _ => return Err(Error::UnknownNative(format!("{native:?}"))),
    };
    Ok(Some(value))
}

pub(super) fn random_native<L: LoaderRef>(vm: &mut Vm<L>, native: Native, args: &[Value]) -> Result<Option<Value>, Error> {
    let value = match native {
        // Draws yaw first, then pitch, then roll when asked, as retail.
        Native::RotRand => {
            let mut turn = || (vm.rand() * 2) % 0xffff;
            let (yaw, pitch) = (turn(), turn());
            Value::Rotator([pitch, yaw, if bool_arg(args, 0) { turn() } else { 0 }].into())
        }
        Native::FRand => Value::Float(vm.random()),
        // `rand() % Max`, so never above 32767; 0 for Max <= 0.
        Native::Rand => Value::Int(if int_arg(args, 0) > 0 { vm.rand() % int_arg(args, 0) } else { 0 }),
        // Size squared summed Y, X, Z, with the square root in double precision, as retail.
        Native::VRand => loop {
            let v = Vector::from([0; 3].map(|_| vm.random() * 2.0 - 1.0));
            let size_squared = v.y * v.y + v.x * v.x + v.z * v.z;
            if size_squared <= 1.0 {
                break Value::Vector(v * (1.0 / (f64::from(size_squared).sqrt() as f32)));
            }
        },
        _ => return Err(Error::UnknownNative(format!("{native:?}"))),
    };
    Ok(Some(value))
}

pub(super) fn reflect_native<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>, args: &[Value]) -> Result<Option<Value>, Error> {
    let value = match native {
        Native::GetEnum => Value::Name(match object_arg(args, 0) {
            Some(e) => vm.enum_name(e, int_arg(args, 1)),
            None => NameId::NONE,
        }),
        Native::GetPropertyText => Value::Str(this.map_or_else(String::new, |this| vm.property_text(this, str_arg(args, 0)))),
        Native::SetPropertyText => {
            if let Some(this) = this {
                vm.set_property_text(this, str_arg(args, 0), str_arg(args, 1));
            }
            return Ok(None);
        }
        Native::FindObject => {
            let class = object_arg(args, 1).map(|class| vm.store.name(vm.store.object(class).name).to_owned()).unwrap_or_default();
            Value::Object(vm.find_object_by_path(str_arg(args, 0), &class))
        }
        Native::DynamicLoadObject => {
            let path = match str_arg(args, 0).to_owned() {
                name if name.len() > 4 && name.get(..4).is_some_and(|prefix| prefix.eq_ignore_ascii_case("ini:")) => vm.ini_name(&name[4..]).unwrap_or(name),
                name => name,
            };
            let class_name = match args.get(1) {
                Some(&Value::Object(Some(class))) => vm.store.name(vm.store.object(class).name).to_owned(),
                _ => String::new(),
            };
            if class_name.eq_ignore_ascii_case("Class") {
                let path = match path.contains('.') {
                    true => path,
                    false => vm.class_package(&path).map_or(path.clone(), |package| format!("{package}.{path}")),
                };
                let stub = vm.stub(&path, "Class");
                match vm.class_of_object(stub)? {
                    Some(_) => Value::Object(Some(stub)),
                    None => {
                        vm.diagnostics.log.push(format!("DynamicLoadObject: class {path} not found"));
                        Value::Object(None)
                    }
                }
            } else if path.contains('.') {
                // An object of a loaded package exists or not (then None, like a failed load); one in a package not
                // in the install is assumed to exist, as the requested class.
                vm.load_package(path.split('.').next().unwrap_or_default());
                match vm.export_class(&path, &class_name) {
                    Some(Some(class)) => Value::Object(Some(vm.stub(&path, &class))),
                    Some(None) => {
                        if !bool_arg(args, 2) {
                            vm.diagnostics.log.push(format!("DynamicLoadObject: {path} not found"));
                        }
                        Value::Object(None)
                    }
                    None => Value::Object(Some(vm.stub(&path, &class_name))),
                }
            } else {
                Value::Object(None)
            }
        }
        Native::IsA => {
            let class = name_arg(args, 0);
            Value::Bool(this.is_some_and(|this| vm.store.is_a(this, class)))
        }
        Native::ClassIsChildOf => {
            let class = |vm: &mut Vm<L>, n: usize| match args.get(n) {
                Some(&Value::Object(Some(object))) => vm.class_of_object(object),
                _ => Ok(None),
            };
            let (test, parent) = (class(vm, 0)?, class(vm, 1)?);
            Value::Bool(match (test, parent) {
                (Some(test), Some(parent)) => vm.class_is_a(test, vm.store.class(parent).name),
                _ => false,
            })
        }
        _ => return Err(Error::UnknownNative(format!("{native:?}"))),
    };
    Ok(Some(value))
}

pub(super) fn state_native<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>, args: &[Value]) -> Result<Option<Value>, Error> {
    let value = match native {
        Native::GotoState => {
            if let Some(this) = this {
                // An omitted state is the current one (`GotoState(, '')`): the code goes to the label in that state.
                let state = if matches!(args.first(), Some(Value::Map)) { vm.state_name(this) } else { name_arg(args, 0) };
                vm.goto_state(this, state, name_arg(args, 1))?;
            }
            return Ok(None);
        }
        Native::GetStateName => Value::Name(this.map_or(NameId::NONE, |this| vm.state_name(this))),
        Native::IsInState => Value::Bool(match this {
            Some(this) => vm.is_in_state(this, name_arg(args, 0))?,
            None => false,
        }),
        Native::Sleep => {
            vm.script.latent.sleep = Some(float_arg(args, 0));
            return Ok(None);
        }
        Native::FinishInterpolation => {
            if let Some(this) = this {
                vm.finish_interpolation(this);
            }
            return Ok(None);
        }
        Native::SetTimer => {
            if let Some(this) = this {
                vm.store.set(this, Prop::TimerRate, Value::Float(float_arg(args, 0)));
                vm.store.set(this, Prop::TimerCounter, Value::Float(0.0));
                vm.store.set(this, Prop::bTimerLoop, Value::Bool(bool_arg(args, 1)));
            }
            return Ok(None);
        }
        Native::Disable | Native::Enable => {
            let (Some(this), Some(&Value::Name(probe))) = (this, args.first()) else { return Ok(None) };
            let Some(bit) = crate::states::probe_bit(vm.store.name(probe)) else { return Ok(None) };
            let disabled = vm.script.states.probes_disabled.entry(this).or_insert(0);
            if native == Native::Disable {
                *disabled |= 1 << bit;
            } else {
                *disabled &= !(1 << bit);
            }
            return Ok(None);
        }
        Native::Initialize if this.is_some_and(|this| vm.store.is_a(this, "Interaction")) => {
            if let Some(this) = this {
                vm.event(this, "Initialized", Vec::new())?;
            }
            return Ok(None);
        }
        _ => return Err(Error::UnknownNative(format!("{native:?}"))),
    };
    Ok(Some(value))
}

pub(super) fn lifetime_native<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>, args: &[Value]) -> Result<Option<Value>, Error> {
    let value = match native {
        Native::Destroy => Value::Bool(match this {
            Some(this) => vm.destroy(this)?,
            None => false,
        }),
        Native::Spawn => {
            let class = match args.first() {
                Some(&Value::Object(Some(class))) => vm.class_of_object(class)?,
                _ => None,
            };
            let Some(class) = class else { return Ok(Some(Value::Object(None))) };
            let owner = match args.get(1) {
                Some(&Value::Object(owner)) => owner,
                _ => None,
            };
            let placement = [(3, Prop::Location), (4, Prop::Rotation)].map(|(index, property)| match args.get(index) {
                Some(value) if is_struct_value(value) => Some(value.clone()),
                _ => this.and_then(|this| vm.store.get(this, property)),
            });
            let instigator = this.and_then(|this| vm.store.object_of(this, Prop::Instigator));
            let actor = vm.spawn_actor(class, owner, instigator, placement)?;
            if let (Some(actor), Some(tag @ Value::Name(_))) = (actor, args.get(2)) {
                vm.store.set(actor, Prop::Tag, tag.clone());
            }
            Value::Object(actor)
        }
        _ => return Err(Error::UnknownNative(format!("{native:?}"))),
    };
    Ok(Some(value))
}
