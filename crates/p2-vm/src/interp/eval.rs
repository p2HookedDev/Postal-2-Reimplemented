use p2_package::PackageId;
use p2_script::Expr;
use p2_math::{Rotator, Vector};
use crate::Prop;
use crate::{ClassId, Error, NameId, ObjectId, LoaderRef, Value, Vm};
use super::{
    Frame, MAX_SCRIPT_DEPTH, PrimCast, Target, cast, cast_link, cast_of_link, is_place, linked, same_value, set_link,
    unlink,
};

impl<L: LoaderRef> Vm<L> {
    pub(super) fn eval(&mut self, frame: &mut Frame, expr: &Expr, context: Option<ObjectId>) -> Result<Value, Error> {
        let package = frame.function.package;
        let whole = expr;
        let (expr, link) = unlink(expr);
        Ok(match expr {
            Expr::IntConst(v) => Value::Int(*v),
            Expr::FloatConst(v) => Value::Float(*v),
            Expr::ByteConst(v) => Value::Byte(*v),
            Expr::IntConstByte(v) => Value::Int(i32::from(*v)),
            Expr::IntZero => Value::Int(0),
            Expr::IntOne => Value::Int(1),
            Expr::True => Value::Bool(true),
            Expr::False => Value::Bool(false),
            Expr::StringConst(v) | Expr::UnicodeStringConst(v) => Value::Str(v.clone()),
            Expr::NameConst(name) => Value::Name(self.name_const(frame, link, *name)),
            Expr::VectorConst(v) => Value::Vector(Vector::new(v[0], v[1], v[2])),
            Expr::RotationConst(v) => Value::Rotator(Rotator::new(v[0], v[1], v[2])),
            Expr::NoObject => Value::Object(None),
            Expr::SelfObject => Value::Object(frame.this),
            Expr::ObjectConst(reference) => self.object_const(package, *reference),
            Expr::BoolVariable(inner) | Expr::Skip { value: inner, .. } | Expr::EatString(inner) => {
                self.eval(frame, inner, context)?
            }
            Expr::StructMember { member, value } if !is_place(value) => {
                let index = self.linked_member_index(frame, link, *member)?;
                match self.eval(frame, value, context)? {
                    Value::Struct(members) => members.get(index).cloned().unwrap_or(Value::Int(0)),
                    Value::Vector(v) => <[f32; 3]>::from(v).get(index).map_or(Value::Int(0), |&c| Value::Float(c)),
                    Value::Rotator(r) => <[i32; 3]>::from(r).get(index).map_or(Value::Int(0), |&c| Value::Int(c)),
                    _ => Value::Int(0),
                }
            }
            Expr::LocalVariable(_)
            | Expr::InstanceVariable(_)
            | Expr::DefaultVariable(_)
            | Expr::ArrayElement { .. }
            | Expr::DynArrayElement { .. }
            | Expr::StructMember { .. } => match self.place(frame, whole, context)? {
                Target::Place(place) => self.read_place(frame, &place),
                Target::Constant(value) => value,
                Target::Nowhere => Value::Int(0),
            },
            Expr::Context { object, member, .. } | Expr::ClassContext { object, member, .. } => {
                match self.eval(frame, object, context)? {
                    Value::Object(Some(object)) => self.eval(frame, member, Some(object))?,
                    // "Accessed None": the engine logs it and yields a zero value.
                    _ => Value::Int(0),
                }
            }
            Expr::DynArrayLength(array) if is_place(array) => match self.place(frame, array, context)? {
                Target::Place(place) => Value::Int(self.array_len(frame, &place) as i32),
                Target::Constant(Value::Array(elements)) => Value::Int(elements.len() as i32),
                _ => Value::Int(0),
            },
            Expr::DynArrayLength(array) => match self.eval(frame, array, context)? {
                Value::Array(elements) => Value::Int(elements.len() as i32),
                _ => Value::Int(0),
            },
            Expr::PrimitiveCast { kind, value } => {
                let value = self.eval(frame, value, context)?;
                cast(self, PrimCast::of(*kind), value)?
            }
            Expr::MetaCast { class, value } => {
                let value = self.eval(frame, value, context)?;
                self.meta_cast(frame, link, *class, value)?
            }
            Expr::New { outer, name, class, .. } => self.eval_new(frame, outer, name, class, context)?,
            Expr::DynamicCast { class, value } => {
                let value = self.eval(frame, value, context)?;
                self.dynamic_cast(frame, link, *class, value)
            }
            Expr::StructCmpEq { left, right, .. } => {
                let left = self.eval(frame, left, context)?;
                Value::Bool(same_value(&left, &self.eval(frame, right, context)?))
            }
            Expr::StructCmpNe { left, right, .. } => {
                let left = self.eval(frame, left, context)?;
                Value::Bool(!same_value(&left, &self.eval(frame, right, context)?))
            }
            Expr::Call { callee, args } => self.eval_call(frame, *callee, args, context, link)?.unwrap_or(Value::Int(0)),
            other => return Err(Error::Unsupported(format!("{other:?}").chars().take(60).collect())),
        })
    }

    pub(super) fn name_const(&mut self, frame: &Frame, link: Option<u32>, name: usize) -> NameId {
        if let Some(id) = linked(frame, link) {
            return NameId(id as u32);
        }
        let id = self.package_name(frame.function.package, name);
        set_link(frame, link, u64::from(id.0));
        id
    }

    /// Found once per reference until the level is left (`leave_level` drops the map's objects). Only found objects are
    /// kept: a reference whose object failed to load gives its stub next time.
    pub(super) fn object_const(&mut self, package: PackageId, reference: i32) -> Value {
        if let Some(&object) = self.script.lookups.object_consts.get(&(package, reference)) {
            return Value::Object(Some(object));
        }
        let loader = self.loader.clone();
        let object = self.object_ref(&loader.package(package).package, package, reference);
        if let Some(object) = object {
            self.script.lookups.object_consts.insert((package, reference), object);
        }
        Value::Object(object)
    }

    fn eval_new(&mut self, frame: &mut Frame, outer: &Expr, name: &Expr, class: &Expr, context: Option<ObjectId>) -> Result<Value, Error> {
        let outer = match outer {
            Expr::Nothing => None,
            outer => match self.eval(frame, outer, context)? {
                Value::Object(outer) => outer,
                _ => None,
            },
        };
        let name = match name {
            Expr::Nothing => None,
            name => match self.eval(frame, name, context)? {
                Value::Name(name) if name != NameId::NONE => Some(self.store.name(name).to_owned()),
                Value::Str(name) if !name.is_empty() => Some(name),
                _ => None,
            },
        };
        let class = match self.eval(frame, class, context)? {
            Value::Object(Some(object)) => self.class_of_object(object)?,
            _ => None,
        };
        Ok(match class {
            Some(class) => {
                let object = self.new_object(self.redirected(class), name.as_deref());
                self.store.set(object, Prop::Outer, Value::Object(outer));
                Value::Object(Some(object))
            }
            None => Value::Object(None),
        })
    }

    pub(super) fn in_place_ok(&self) -> bool {
        self.diagnostics.trace.is_none() && self.diagnostics.function_profile.is_none() && self.script.script_depth < MAX_SCRIPT_DEPTH
    }

    pub(super) fn meta_cast(&mut self, frame: &Frame, link: Option<u32>, class: i32, value: Value) -> Result<Value, Error> {
        let package = frame.function.package;
        Ok(match value {
            Value::Object(Some(object)) => match self.class_of_object(object)? {
                Some(of) => {
                    let key = (of, package, class, true);
                    let keeps = match self.known_cast(frame, link, key) {
                        Some(keeps) => keeps,
                        None => {
                            let target = self.cast_target(package, class);
                            let keeps = self.class_is_a(of, target);
                            self.script.lookups.casts.insert(key, keeps);
                            keeps
                        }
                    };
                    Value::Object(keeps.then_some(object))
                }
                None => Value::Object(None),
            },
            _ => Value::Object(None),
        })
    }

    pub(super) fn dynamic_cast(&mut self, frame: &Frame, link: Option<u32>, class: i32, value: Value) -> Value {
        let package = frame.function.package;
        let Value::Object(Some(object)) = value else { return Value::Object(None) };
        let key = self.store.object(object).class.map(|of| (of, package, class, false));
        if let Some(keeps) = key.and_then(|key| self.known_cast(frame, link, key)) {
            return Value::Object(keeps.then_some(object));
        }
        let target = self.cast_target(package, class);
        let class_name = self.store.name(target);
        // `Viewport` is C++-only: the VM's one `Player`, the local one, stands for it (scripts find the local
        // player by `ViewPort(Player) != None`, e.g. the ragdoll pool in `P2MoCapPawn.GetKarmaSkeleton`).
        let viewport = class_name.eq_ignore_ascii_case("Viewport") && self.store.is_a(object, "Player");
        let keeps = self.store.is_a(object, target) || self.stub_is_a(object, class_name) || viewport;
        if let Some(key) = key {
            self.script.lookups.casts.insert(key, keeps);
        }
        Value::Object(keeps.then_some(object))
    }

    fn known_cast(&self, frame: &Frame, link: Option<u32>, key: (ClassId, PackageId, i32, bool)) -> Option<bool> {
        if let Some(keeps) = linked(frame, link).and_then(|value| cast_of_link(value, key.0)) {
            return Some(keeps);
        }
        let keeps = *self.script.lookups.casts.get(&key)?;
        set_link(frame, link, cast_link(key.0, keeps));
        Some(keeps)
    }
}
