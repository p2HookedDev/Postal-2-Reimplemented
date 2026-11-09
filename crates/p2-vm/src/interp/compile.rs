//! Closures hold indices into their [`Code`]'s tables, never other closures, so they are `'static`.

use std::sync::Arc;

use p2_math::{Rotator, Vector};
use p2_script::{Callee, Expr, Statement};

use super::{
    CPF_OUT_PARM, Flow, Frame, Function, Place, PrimCast, Target, case_matches, cast, int, is_place, linked, member_of_link,
    truthy, unlink,
};
use crate::profiling::Work;
use crate::native_id::Native;
use crate::{ClassId, Error, LoaderRef, ObjectId, Value, Vm};

type ExprFn<L> = Box<dyn Fn(&Code<L>, &mut Vm<L>, &mut Frame, Option<ObjectId>) -> Result<Value, Error> + Send + Sync>;
type PlaceFn<L> = Box<dyn Fn(&Code<L>, &mut Vm<L>, &mut Frame, Option<ObjectId>) -> Result<Target, Error> + Send + Sync>;
type StmtFn<L> = Box<dyn Fn(&Code<L>, &mut Vm<L>, &mut Frame) -> Result<Flow, Error> + Send + Sync>;

pub(crate) struct Code<L> {
    exprs: Vec<ExprFn<L>>,
    places: Vec<PlaceFn<L>>,
    statements: Vec<StmtFn<L>>,
}

fn expr_fn<L, F>(f: F) -> ExprFn<L>
where
    F: Fn(&Code<L>, &mut Vm<L>, &mut Frame, Option<ObjectId>) -> Result<Value, Error> + Send + Sync + 'static,
{
    Box::new(f)
}

fn place_fn<L, F>(f: F) -> PlaceFn<L>
where
    F: Fn(&Code<L>, &mut Vm<L>, &mut Frame, Option<ObjectId>) -> Result<Target, Error> + Send + Sync + 'static,
{
    Box::new(f)
}

fn stmt_fn<L, F>(f: F) -> StmtFn<L>
where
    F: Fn(&Code<L>, &mut Vm<L>, &mut Frame) -> Result<Flow, Error> + Send + Sync + 'static,
{
    Box::new(f)
}

fn constant<L: LoaderRef>(value: Value) -> ExprFn<L> {
    expr_fn(move |_, _, _, _| Ok(value.clone()))
}

fn eval_fallback<L: LoaderRef>(whole: &Expr) -> ExprFn<L> {
    let whole = whole.clone();
    expr_fn(move |_, vm, frame, context| vm.eval(frame, &whole, context))
}

fn place_fallback<L: LoaderRef>(whole: &Expr) -> PlaceFn<L> {
    let whole = whole.clone();
    place_fn(move |_, vm, frame, context| vm.place(frame, &whole, context))
}

struct Arg {
    value: Option<usize>,
    place: Option<usize>,
}

struct Site {
    callee: Callee,
    link: Option<u32>,
    args: Vec<Arg>,
    whole: Expr,
    operator: bool,
    logical: bool,
    script: bool,
}

/// Found when compiled. An offset that is no statement stays a [`Flow::Jump`], which fails when taken.
#[derive(Clone, Copy)]
struct JumpTo {
    offset: u16,
    index: Option<usize>,
}

impl JumpTo {
    fn new(statements: &[Statement], offset: u16) -> Self {
        Self { offset, index: statements.binary_search_by_key(&u32::from(offset), |s| s.offset).ok() }
    }

    fn flow(self) -> Flow {
        self.index.map_or(Flow::Jump(self.offset), Flow::Goto)
    }
}

fn plain<L: LoaderRef>(flow: fn() -> Flow) -> StmtFn<L> {
    stmt_fn(move |_, vm, _| {
        vm.count(Work::Statement);
        Ok(flow())
    })
}

impl<L: LoaderRef> Code<L> {
    fn of(statements: &[Statement]) -> Self {
        let mut code = Self { exprs: Vec::new(), places: Vec::new(), statements: Vec::new() };
        let compiled: Vec<StmtFn<L>> = statements.iter().enumerate().map(|(pc, statement)| code.statement(&statement.expr, pc, statements)).collect();
        code.statements = compiled;
        code
    }

    pub(super) fn run(&self, vm: &mut Vm<L>, frame: &mut Frame, pc: usize) -> Result<Flow, Error> {
        (self.statements[pc])(self, vm, frame)
    }

    fn eval(&self, index: usize, vm: &mut Vm<L>, frame: &mut Frame, context: Option<ObjectId>) -> Result<Value, Error> {
        (self.exprs[index])(self, vm, frame, context)
    }

    fn place_at(&self, index: usize, vm: &mut Vm<L>, frame: &mut Frame, context: Option<ObjectId>) -> Result<Target, Error> {
        (self.places[index])(self, vm, frame, context)
    }

    fn read_at(&self, index: usize, vm: &mut Vm<L>, frame: &mut Frame, context: Option<ObjectId>) -> Result<Value, Error> {
        Ok(match self.place_at(index, vm, frame, context)? {
            Target::Place(place) => vm.read_place(frame, &place),
            Target::Constant(value) => value,
            Target::Nowhere => Value::Int(0),
        })
    }

    fn statement(&mut self, expr: &Expr, pc: usize, statements: &[Statement]) -> StmtFn<L> {
        match expr {
            Expr::Let { target, value } | Expr::LetBool { target, value } | Expr::LetDelegate { target, value }
                if !matches!(**target, Expr::DynArrayLength(_)) =>
            {
                self.assignment(target, value)
            }
            Expr::JumpIfNot { target, condition } => {
                let (target, condition) = (JumpTo::new(statements, *target), self.expr(condition));
                stmt_fn(move |code, vm, frame| {
                    vm.count(Work::Statement);
                    let this = frame.this;
                    Ok(if truthy(&code.eval(condition, vm, frame, this)?) { Flow::Next } else { target.flow() })
                })
            }
            Expr::Jump(target) => {
                let target = JumpTo::new(statements, *target);
                stmt_fn(move |_, vm, _| {
                    vm.count(Work::Statement);
                    Ok(target.flow())
                })
            }
            Expr::Return(value) if !matches!(**value, Expr::Nothing) => {
                let value = self.expr(value);
                stmt_fn(move |code, vm, frame| {
                    vm.count(Work::Statement);
                    let this = frame.this;
                    Ok(Flow::Return(Some(code.eval(value, vm, frame, this)?)))
                })
            }
            Expr::Switch { value, .. } => {
                let value = self.expr(value);
                stmt_fn(move |code, vm, frame| {
                    vm.count(Work::Statement);
                    let this = frame.this;
                    frame.switch = Some(code.eval(value, vm, frame, this)?);
                    Ok(Flow::Next)
                })
            }
            Expr::Case { next, value } => self.case(JumpTo::new(statements, *next), value.as_deref()),
            Expr::Iterator { iterator, end } => {
                let (iterator, end) = (iterator.clone(), JumpTo::new(statements, *end));
                stmt_fn(move |_, vm, frame| {
                    vm.count(Work::Statement);
                    vm.exec_iterator(frame, &iterator, end.flow(), pc)
                })
            }
            Expr::Return(_) | Expr::Stop => plain(|| Flow::Return(None)),
            Expr::Nothing | Expr::DebugInfo { .. } | Expr::LabelTable(_) => plain(|| Flow::Next),
            Expr::Call { .. } | Expr::Context { .. } | Expr::ClassContext { .. } | Expr::Linked { .. } => {
                let value = self.expr(expr);
                stmt_fn(move |code, vm, frame| {
                    vm.count(Work::Statement);
                    let this = frame.this;
                    code.eval(value, vm, frame, this)?;
                    Ok(Flow::Next)
                })
            }
            _ => {
                let expr = expr.clone();
                stmt_fn(move |_, vm, frame| vm.exec(frame, &expr, pc))
            }
        }
    }

    fn case(&mut self, next: JumpTo, value: Option<&Expr>) -> StmtFn<L> {
        let value = value.map(|value| self.expr(value));
        stmt_fn(move |code, vm, frame| {
            vm.count(Work::Statement);
            Ok(match (frame.switch.take(), value) {
                (Some(switch), Some(value)) => {
                    let this = frame.this;
                    if case_matches(&switch, &code.eval(value, vm, frame, this)?) {
                        Flow::Next
                    } else {
                        frame.switch = Some(switch);
                        next.flow()
                    }
                }
                _ => Flow::Next,
            })
        })
    }

    fn assignment(&mut self, target: &Expr, value: &Expr) -> StmtFn<L> {
        let (value, target) = (self.expr(value), self.place(target));
        stmt_fn(move |code, vm, frame| {
            vm.count(Work::Statement);
            let this = frame.this;
            let value = code.eval(value, vm, frame, this)?;
            if let Target::Place(place) = code.place_at(target, vm, frame, this)? {
                vm.write_place(frame, &place, value);
            }
            Ok(Flow::Next)
        })
    }

    fn expr(&mut self, whole: &Expr) -> usize {
        let (expr, link) = unlink(whole);
        let compiled = match expr {
            Expr::IntConst(v) => constant(Value::Int(*v)),
            Expr::FloatConst(v) => constant(Value::Float(*v)),
            Expr::ByteConst(v) => constant(Value::Byte(*v)),
            Expr::IntConstByte(v) => constant(Value::Int(i32::from(*v))),
            Expr::IntZero => constant(Value::Int(0)),
            Expr::IntOne => constant(Value::Int(1)),
            Expr::True => constant(Value::Bool(true)),
            Expr::False => constant(Value::Bool(false)),
            Expr::StringConst(v) | Expr::UnicodeStringConst(v) => constant(Value::Str(v.clone())),
            Expr::VectorConst(v) => constant(Value::Vector(Vector::new(v[0], v[1], v[2]))),
            Expr::RotationConst(v) => constant(Value::Rotator(Rotator::new(v[0], v[1], v[2]))),
            Expr::NoObject => constant(Value::Object(None)),
            Expr::SelfObject => expr_fn(|_, _, frame, _| Ok(Value::Object(frame.this))),
            Expr::BoolVariable(inner) | Expr::Skip { value: inner, .. } | Expr::EatString(inner) => return self.expr(inner),
            Expr::StructMember { value, .. } if !is_place(value) => eval_fallback(whole),
            Expr::NameConst(name) => {
                let name = *name;
                expr_fn(move |_, vm, frame, _| Ok(Value::Name(vm.name_const(frame, link, name))))
            }
            Expr::ObjectConst(reference) => {
                let reference = *reference;
                expr_fn(move |_, vm, frame, _| Ok(vm.object_const(frame.function.package, reference)))
            }
            Expr::LocalVariable(_) => {
                let place = self.place(whole);
                expr_fn(move |code, vm, frame, context| match linked(frame, link) {
                    Some(index) => Ok(vm.read_local(frame, index as usize)),
                    None => code.read_at(place, vm, frame, context),
                })
            }
            Expr::InstanceVariable(_) => {
                let place = self.place(whole);
                expr_fn(move |code, vm, frame, context| {
                    if let Some(object) = context
                        && let Some(class) = vm.store.object(object).class
                        && let Some(Some(index)) = linked(frame, link).and_then(|value| member_of_link(value, class))
                    {
                        return Ok(vm.read_instance(object, class, index));
                    }
                    code.read_at(place, vm, frame, context)
                })
            }
            Expr::DefaultVariable(_) | Expr::ArrayElement { .. } | Expr::DynArrayElement { .. } | Expr::StructMember { .. } => {
                let place = self.place(whole);
                expr_fn(move |code, vm, frame, context| code.read_at(place, vm, frame, context))
            }
            Expr::DynArrayLength(array) if is_place(array) => {
                let array = self.place(array);
                expr_fn(move |code, vm, frame, context| {
                    Ok(match code.place_at(array, vm, frame, context)? {
                        Target::Place(place) => Value::Int(vm.array_len(frame, &place) as i32),
                        Target::Constant(Value::Array(elements)) => Value::Int(elements.len() as i32),
                        _ => Value::Int(0),
                    })
                })
            }
            Expr::StructCmpEq { left, right, .. } => {
                let (left, right) = (self.expr(left), self.expr(right));
                expr_fn(move |code, vm, frame, context| Ok(Value::Bool(super::same_value(&code.eval(left, vm, frame, context)?, &code.eval(right, vm, frame, context)?))))
            }
            Expr::StructCmpNe { left, right, .. } => {
                let (left, right) = (self.expr(left), self.expr(right));
                expr_fn(move |code, vm, frame, context| Ok(Value::Bool(!super::same_value(&code.eval(left, vm, frame, context)?, &code.eval(right, vm, frame, context)?))))
            }
            Expr::Context { object, member, .. } | Expr::ClassContext { object, member, .. } => {
                let (object, member) = (self.expr(object), self.expr(member));
                expr_fn(move |code, vm, frame, context| match code.eval(object, vm, frame, context)? {
                    Value::Object(Some(object)) => code.eval(member, vm, frame, Some(object)),
                    _ => Ok(Value::Int(0)),
                })
            }
            Expr::PrimitiveCast { .. } | Expr::MetaCast { .. } | Expr::DynamicCast { .. } => self.cast_expr(whole),
            Expr::Call { callee, args } => self.call(whole, *callee, args, link),
            _ => eval_fallback(whole),
        };
        self.exprs.push(compiled);
        self.exprs.len() - 1
    }

    fn cast_expr(&mut self, whole: &Expr) -> ExprFn<L> {
        let (expr, link) = unlink(whole);
        match expr {
            Expr::PrimitiveCast { kind, value } => {
                let (to, value) = (PrimCast::of(*kind), self.expr(value));
                expr_fn(move |code, vm, frame, context| {
                    let value = code.eval(value, vm, frame, context)?;
                    cast(vm, to, value)
                })
            }
            Expr::MetaCast { class, value } => {
                let (class, value) = (*class, self.expr(value));
                expr_fn(move |code, vm, frame, context| {
                    let value = code.eval(value, vm, frame, context)?;
                    vm.meta_cast(frame, link, class, value)
                })
            }
            Expr::DynamicCast { class, value } => {
                let (class, value) = (*class, self.expr(value));
                expr_fn(move |code, vm, frame, context| {
                    let value = code.eval(value, vm, frame, context)?;
                    Ok(vm.dynamic_cast(frame, link, class, value))
                })
            }
            _ => eval_fallback(whole),
        }
    }

    fn place(&mut self, whole: &Expr) -> usize {
        let (expr, link) = unlink(whole);
        let compiled = match expr {
            Expr::BoolVariable(inner) => return self.place(inner),
            Expr::LocalVariable(_) => {
                let whole = whole.clone();
                place_fn(move |_, vm, frame, context| match linked(frame, link) {
                    Some(index) => Ok(vm.local_target(frame, index as usize)),
                    None => vm.place(frame, &whole, context),
                })
            }
            Expr::InstanceVariable(_) => {
                let whole = whole.clone();
                place_fn(move |_, vm, frame, context| {
                    if let Some(object) = context
                        && let Some(class) = vm.store.object(object).class
                        && let Some(Some(index)) = linked(frame, link).and_then(|value| member_of_link(value, class))
                    {
                        return Ok(vm.instance_target(object, class, index));
                    }
                    vm.place(frame, &whole, context)
                })
            }
            Expr::ArrayElement { index, array } | Expr::DynArrayElement { index, array } => {
                let (index, array) = (self.expr(index), self.place(array));
                place_fn(move |code, vm, frame, context| {
                    let this = frame.this;
                    let element = int(&code.eval(index, vm, frame, this)?);
                    Ok(match code.place_at(array, vm, frame, context)? {
                        Target::Place(place) => vm.element(place, element),
                        _ => Target::Nowhere,
                    })
                })
            }
            Expr::StructMember { member, value } => {
                let (member, value) = (*member, self.place(value));
                place_fn(move |code, vm, frame, context| {
                    let index = vm.linked_member_index(frame, link, member)?;
                    let target = code.place_at(value, vm, frame, context)?;
                    Ok(vm.struct_member_target(target, index))
                })
            }
            Expr::Context { object, member, .. } | Expr::ClassContext { object, member, .. } => {
                let (object, member) = (self.expr(object), self.place(member));
                place_fn(move |code, vm, frame, context| match code.eval(object, vm, frame, context)? {
                    Value::Object(Some(object)) => code.place_at(member, vm, frame, Some(object)),
                    _ => Ok(Target::Nowhere),
                })
            }
            _ => place_fallback(whole),
        };
        self.places.push(compiled);
        self.places.len() - 1
    }

    fn call(&mut self, whole: &Expr, callee: Callee, args: &[Expr], link: Option<u32>) -> ExprFn<L> {
        let skip = matches!(args, [_, Expr::Skip { .. }]);
        let direct = matches!(callee, Callee::Native(_) | Callee::Final(_)) && link.is_some();
        let operator = direct && args.len() <= 3 && !args.iter().any(|arg| matches!(arg, Expr::Nothing)) && !skip;
        let logical = direct && skip && !matches!(args[0], Expr::Nothing);
        let script = !skip
            && match callee {
                Callee::Virtual(_) | Callee::Global(_) | Callee::Final(_) => true,
                Callee::Native(_) => link.is_some(),
                Callee::Delegate { .. } => false,
            };
        if !operator && !logical && !script {
            return eval_fallback(whole);
        }
        let args_compiled = args
            .iter()
            .map(|arg| Arg {
                value: (!matches!(arg, Expr::Nothing)).then(|| self.expr(arg)),
                place: (script && is_place(arg)).then(|| self.place(arg)),
            })
            .collect();
        let site = Site { callee, link, args: args_compiled, whole: whole.clone(), operator, logical, script };
        expr_fn(move |code, vm, frame, context| code.run_call(&site, vm, frame, context))
    }

    fn run_call(&self, site: &Site, vm: &mut Vm<L>, frame: &mut Frame, context: Option<ObjectId>) -> Result<Value, Error> {
        if site.logical
            && let Some(op) = vm.logical_op(frame, site.link)
        {
            return self.run_logical(site, op, vm, frame, context);
        }
        if site.operator
            && let Some((op, first_out)) = vm.direct_op(frame, site.link, site.args.len())
        {
            vm.context_class(context)?;
            return self.run_in_place(site, op, first_out, vm, frame);
        }
        if !site.script {
            return vm.eval(frame, &site.whole, context);
        }
        if vm.call_on_stub(frame, site.callee, context) {
            return Ok(Value::Int(0));
        }
        let class = vm.context_class(context)?.unwrap_or(frame.class);
        let function = vm.call_target(frame, site.callee, context, class, site.link)?;
        let value = if function.is_native() {
            self.call_native(site, &function, vm, frame, context, class)?
        } else {
            let this = frame.this;
            vm.call_script(frame, &function, context, class, site.args.len(), |vm, frame, position, out| {
                self.arg(&site.args[position], vm, frame, this, out)
            })?
        };
        Ok(value.unwrap_or(Value::Int(0)))
    }

    fn run_logical(&self, site: &Site, op: Native, vm: &mut Vm<L>, frame: &mut Frame, context: Option<ObjectId>) -> Result<Value, Error> {
        vm.context_class(context)?;
        let this = frame.this;
        let (Some(first), Some(second)) = (site.args[0].value, site.args[1].value) else {
            return vm.eval(frame, &site.whole, context);
        };
        let first = truthy(&self.eval(first, vm, frame, this)?);
        if first == (op == Native::OrOr_BoolBool) {
            return Ok(Value::Bool(first));
        }
        Ok(Value::Bool(truthy(&self.eval(second, vm, frame, this)?)))
    }

    fn run_in_place(&self, site: &Site, op: Native, first_out: bool, vm: &mut Vm<L>, frame: &mut Frame) -> Result<Value, Error> {
        let this = frame.this;
        let mut values = [Value::Map, Value::Map, Value::Map];
        let mut out = None;
        for (position, (value, arg)) in values.iter_mut().zip(&site.args).enumerate() {
            if position == 0
                && first_out
                && let Some(place) = arg.place
            {
                let target = self.place_at(place, vm, frame, this)?;
                (*value, out) = vm.out_arg(frame, target);
            } else if let Some(operand) = arg.value {
                *value = self.eval(operand, vm, frame, this)?;
            }
        }
        let result = crate::operators::operate(op, &mut values[..site.args.len()]).unwrap_or(Value::Int(0));
        if let Some(place) = out {
            vm.write_place(frame, &place, std::mem::replace(&mut values[0], Value::Map));
        }
        Ok(result)
    }

    fn call_native(
        &self,
        site: &Site,
        function: &Arc<Function>,
        vm: &mut Vm<L>,
        frame: &mut Frame,
        context: Option<ObjectId>,
        class: ClassId,
    ) -> Result<Option<Value>, Error> {
        if let Some(op) = function.fast
            && site.args.len() == function.params.len()
            && site.args.len() <= 3
            && vm.in_place_ok()
        {
            return Ok(Some(self.run_in_place(site, op, first_is_out(function), vm, frame)?));
        }
        let this = frame.this;
        let mut values = vm.take_args();
        let mut outs: Vec<(usize, Place)> = Vec::new();
        for (position, (arg, &param)) in site.args.iter().zip(&function.params).enumerate() {
            let out = function.layout.properties[param].flags & CPF_OUT_PARM != 0;
            values.push(match (arg.value, arg.place) {
                (None, _) => Value::Map,
                (Some(_), Some(place)) if out => {
                    let target = self.place_at(place, vm, frame, this)?;
                    let (value, place) = vm.out_arg(frame, target);
                    if let Some(place) = place {
                        outs.push((position, place));
                    }
                    value
                }
                (Some(value), _) => self.eval(value, vm, frame, this)?,
            });
        }
        vm.run_native(frame, function, context, class, values, &outs)
    }

    fn arg(&self, arg: &Arg, vm: &mut Vm<L>, frame: &mut Frame, this: Option<ObjectId>, out: bool) -> Result<Option<(Value, Option<Place>)>, Error> {
        let Some(value) = arg.value else { return Ok(None) };
        if out && let Some(place) = arg.place {
            let target = self.place_at(place, vm, frame, this)?;
            return Ok(Some(vm.out_arg(frame, target)));
        }
        Ok(Some((self.eval(value, vm, frame, this)?, None)))
    }
}

fn first_is_out(function: &Function) -> bool {
    function.params.first().is_some_and(|&param| function.layout.properties[param].flags & CPF_OUT_PARM != 0)
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn compiled(&mut self, function: &Function) -> Arc<Code<L>> {
        let index = function.index as usize;
        if self.compiled.len() <= index {
            self.compiled.resize_with(index + 1, || None);
        }
        Arc::clone(self.compiled[index].get_or_insert_with(|| Arc::new(Code::of(&function.statements))))
    }

    fn direct_op(&self, frame: &Frame, link: Option<u32>, count: usize) -> Option<(Native, bool)> {
        if !self.in_place_ok() {
            return None;
        }
        let function = self.script.lookups.function_table.get(usize::try_from(linked(frame, link)?).ok()?)?;
        let op = function.fast?;
        (function.params.len() == count).then(|| (op, first_is_out(function)))
    }

    fn logical_op(&self, frame: &Frame, link: Option<u32>) -> Option<Native> {
        if !self.in_place_ok() {
            return None;
        }
        let function = self.script.lookups.function_table.get(usize::try_from(linked(frame, link)?).ok()?)?;
        function.native_id.filter(|_| function.is_logical())
    }

    fn read_local(&self, frame: &Frame, index: usize) -> Value {
        let memory = &self.store.memory;
        memory.get(frame.function.mem).fields.get(index).map_or(Value::Int(0), |field| memory.read_field(field, &frame.locals))
    }

    fn read_instance(&self, object: ObjectId, class: ClassId, index: usize) -> Value {
        let memory = &self.store.memory;
        memory.get(self.store.class(class).mem).fields.get(index).map_or(Value::Int(0), |field| memory.read_field(field, &self.store.object(object).mem))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statement(expr: Expr) -> Statement {
        Statement { offset: 0, expr }
    }

    #[test]
    fn one_closure_per_statement_and_shared_tables() {
        let statements = [
            statement(Expr::Let { target: Box::new(Expr::LocalVariable(1)), value: Box::new(Expr::IntOne) }),
            statement(Expr::JumpIfNot { target: 0, condition: Box::new(Expr::True) }),
            statement(Expr::Switch { size: 4, value: Box::new(Expr::IntZero) }),
            statement(Expr::Return(Box::new(Expr::Nothing))),
        ];
        let code = Code::<&crate::Loader>::of(&statements);
        assert_eq!(code.statements.len(), statements.len());
        assert_eq!(code.exprs.len(), 3);
        assert_eq!(code.places.len(), 1);
    }

    #[test]
    fn case_compiles_its_value_and_default_none() {
        let statements = [
            statement(Expr::Case { next: 8, value: Some(Box::new(Expr::IntOne)) }),
            statement(Expr::Case { next: 0xFFFF, value: None }),
        ];
        let code = Code::<&crate::Loader>::of(&statements);
        assert_eq!(code.statements.len(), 2);
        assert_eq!(code.exprs.len(), 1);
    }

    #[test]
    fn casts_compile_their_value() {
        let mut code = Code::<&crate::Loader>::of(&[]);
        code.expr(&Expr::DynamicCast { class: 3, value: Box::new(Expr::SelfObject) });
        code.expr(&Expr::Linked { link: 0, expr: Box::new(Expr::MetaCast { class: 3, value: Box::new(Expr::NoObject) }) });
        code.expr(&Expr::PrimitiveCast { kind: 0x39, value: Box::new(Expr::IntOne) });
        assert_eq!(code.exprs.len(), 6, "three casts and their three values");
    }

    #[test]
    fn script_calls_compile_values_and_variable_places() {
        let call = Expr::Call {
            callee: Callee::Virtual(5),
            args: vec![Expr::LocalVariable(0), Expr::Nothing, Expr::IntOne],
        };
        let mut code = Code::<&crate::Loader>::of(&[]);
        code.expr(&Expr::Linked { link: 0, expr: Box::new(call) });
        assert_eq!(code.exprs.len(), 3);
        assert_eq!(code.places.len(), 2);
    }

    #[test]
    fn logical_operators_compile_both_operands_and_delegates_run_the_tree_walk() {
        let and = Expr::Call {
            callee: Callee::Final(7),
            args: vec![Expr::True, Expr::Skip { size: 1, value: Box::new(Expr::False) }],
        };
        let delegate = Expr::Call { callee: Callee::Delegate { property: 1, name: 2 }, args: vec![Expr::IntOne] };
        let mut code = Code::<&crate::Loader>::of(&[]);
        code.expr(&Expr::Linked { link: 0, expr: Box::new(and) });
        code.expr(&delegate);
        assert_eq!(code.exprs.len(), 4);
        assert!(code.places.is_empty());
    }

    #[test]
    fn wrappers_compile_to_their_inner_node() {
        let mut code = Code::<&crate::Loader>::of(&[]);
        let inner = code.expr(&Expr::BoolVariable(Box::new(Expr::IntOne)));
        assert_eq!(inner, 0);
        assert_eq!(code.exprs.len(), 1);
    }

    #[test]
    fn operator_calls_compile_their_operands_only_when_linked() {
        let call = Expr::Call { callee: Callee::Native(0x90), args: vec![Expr::IntOne, Expr::IntZero] };
        let mut code = Code::<&crate::Loader>::of(&[]);
        code.expr(&call);
        assert_eq!(code.exprs.len(), 1, "an unlinked call runs the tree walk");
        code.expr(&Expr::Linked { link: 0, expr: Box::new(call) });
        assert_eq!(code.exprs.len(), 4, "a linked native call compiles its two operands");
    }
}
