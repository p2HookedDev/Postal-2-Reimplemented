use std::sync::Arc;
use p2_script::{Callee, Expr};
use crate::{ClassId, Error, ObjectId, LoaderRef, Value, Vm};
use p2_script::flags::CPF_OUT_PARM;
use super::{
    Frame, Function, Place, SITE_FUNCTION_MASK, Target, is_place, linked, set_link, site_key, truthy,
};

impl<L: LoaderRef> Vm<L> {
    pub(super) fn eval_call(
        &mut self,
        frame: &mut Frame,
        callee: Callee,
        args: &[Expr],
        context: Option<ObjectId>,
        link: Option<u32>,
    ) -> Result<Option<Value>, Error> {
        if self.call_on_stub(frame, callee, context) {
            return Ok(None);
        }
        let class = self.context_class(context)?.unwrap_or(frame.class);
        let function = self.call_target(frame, callee, context, class, link)?;
        self.call_resolved(frame, &function, args, context, class)
    }

    /// A stub loads its export first, as the engine has every object loaded (`MaterialSwitch.SetCurrent`); one without an
    /// export or a script class cannot run: true (counted), and the call gives zero, like Accessed None.
    pub(super) fn call_on_stub(&mut self, frame: &Frame, callee: Callee, context: Option<ObjectId>) -> bool {
        if let (Callee::Virtual(name), Some(target)) = (callee, context)
            && self.store.object(target).class.is_none()
            && self.store.object(target).class_name != self.script.lookups.class_names.class
            && {
                self.load_stub(target);
                self.store.object(target).class.is_none()
            }
        {
            let key = (self.store.object(target).class_name, frame.function.package, name);
            if !self.script.lookups.stub_calls.contains_key(&key) {
                let text = format!("call on stub: {}.{}", self.store.name(key.0), self.loader.package(key.1).package.name(name));
                self.script.lookups.stub_calls.insert(key, text);
            }
            let text = &self.script.lookups.stub_calls[&key];
            match self.diagnostics.missing_natives.get_mut(text) {
                Some(count) => *count += 1,
                None => {
                    self.diagnostics.missing_natives.insert(text.clone(), 1);
                }
            }
            return true;
        }
        false
    }

    fn call_resolved(
        &mut self,
        frame: &mut Frame,
        function: &Arc<Function>,
        args: &[Expr],
        context: Option<ObjectId>,
        class: ClassId,
    ) -> Result<Option<Value>, Error> {
        // Arguments belong to the calling frame, whatever object the call goes to.
        let this = frame.this;
        if let [first, Expr::Skip { value: second, .. }] = args {
            let first = truthy(&self.eval(frame, first, this)?);
            match function.native_id {
                Some(crate::native_id::Native::AndAnd_BoolBool) if !first => return Ok(Some(Value::Bool(false))),
                Some(crate::native_id::Native::OrOr_BoolBool) if first => return Ok(Some(Value::Bool(true))),
                _ => {}
            }
            let second = self.eval(frame, second, this)?;
            if function.is_logical() && self.in_place_ok() {
                return Ok(Some(Value::Bool(truthy(&second))));
            }
            return self.call(function, context, class, [Value::Bool(first), second]);
        }

        let is_out = |param: usize| function.layout.properties[param].flags & CPF_OUT_PARM != 0;
        if let Some(op) = function.fast
            && args.len() == function.params.len()
            && args.len() <= 3
            && self.in_place_ok()
        {
            let mut values = [Value::Map, Value::Map, Value::Map];
            let mut out = None;
            for (position, arg) in args.iter().enumerate() {
                values[position] = match arg {
                    Expr::Nothing => Value::Map,
                    arg if position == 0 && is_out(function.params[0]) && is_place(arg) => match self.place(frame, arg, this)? {
                        Target::Place(place) => {
                            if self.readable(frame, &place) {
                                out = Some(place);
                            }
                            self.read_place(frame, &place)
                        }
                        Target::Constant(value) => value,
                        Target::Nowhere => Value::Int(0),
                    },
                    arg => self.eval(frame, arg, this)?,
                };
            }
            let value = crate::operators::operate(op, &mut values[..args.len()]).unwrap_or(Value::Int(0));
            if let Some(place) = out {
                self.write_place(frame, &place, std::mem::replace(&mut values[0], Value::Map));
            }
            return Ok(Some(value));
        }

        if !function.is_native() {
            return self.call_script(frame, function, context, class, args.len(), |vm, frame, position, out| {
                Ok(match &args[position] {
                    Expr::Nothing => None,
                    arg if out && is_place(arg) => {
                        let target = vm.place(frame, arg, this)?;
                        Some(vm.out_arg(frame, target))
                    }
                    arg => Some((vm.eval(frame, arg, this)?, None)),
                })
            });
        }

        // Omitted optional arguments are `Value::Map`, so natives can apply defaults (`PlayAnim`'s rate 1).
        let mut values = self.take_args();
        let mut outs: Vec<(usize, Place)> = Vec::new();
        for (position, (arg, &param)) in args.iter().zip(&function.params).enumerate() {
            let out = is_out(param);
            values.push(match arg {
                Expr::Nothing => Value::Map,
                arg if out && is_place(arg) => match self.place(frame, arg, this)? {
                    Target::Place(place) => {
                        if self.readable(frame, &place) {
                            outs.push((position, place));
                        }
                        self.read_place(frame, &place)
                    }
                    Target::Constant(value) => value,
                    Target::Nowhere => Value::Int(0),
                },
                arg => self.eval(frame, arg, this)?,
            });
        }
        self.run_native(frame, function, context, class, values, &outs)
    }

    pub(super) fn run_native(
        &mut self,
        frame: &mut Frame,
        function: &Arc<Function>,
        context: Option<ObjectId>,
        class: ClassId,
        values: Vec<Value>,
        outs: &[(usize, Place)],
    ) -> Result<Option<Value>, Error> {
        let (value, mut params) = self.run(function, context, class, values)?;
        for &(position, place) in outs.iter().rev() {
            if let Some(value) = params.get_mut(position) {
                self.write_place(frame, &place, std::mem::replace(value, Value::Map));
            }
        }
        self.give_args(params);
        Ok(value)
    }

    /// `out` arguments are written back after the call, last first. `arg(vm, frame, position, out)` gives
    /// a value (`None`: omitted, keeps its zero value) and, for an `out` parameter, the place to write to.
    pub(super) fn call_script(
        &mut self,
        frame: &mut Frame,
        function: &Arc<Function>,
        context: Option<ObjectId>,
        class: ClassId,
        count: usize,
        mut arg: impl FnMut(&mut Self, &mut Frame, usize, bool) -> Result<Option<(Value, Option<Place>)>, Error>,
    ) -> Result<Option<Value>, Error> {
        let mut locals = self.take_locals(function.mem);
        let mut outs: Vec<(usize, Place)> = Vec::new();
        for (position, &param) in function.params.iter().take(count).enumerate() {
            let out = function.layout.properties[param].flags & CPF_OUT_PARM != 0;
            let Some((value, place)) = arg(self, frame, position, out)? else { continue };
            if let Some(place) = place {
                outs.push((param, place));
            }
            let field = self.store.memory.get(function.mem).fields[param];
            self.store.memory.write_field(&field, &mut locals, value);
        }
        let (value, locals) = self.run_locals(function, context, class, locals)?;
        for &(param, place) in outs.iter().rev() {
            let field = self.store.memory.get(function.mem).fields[param];
            let final_value = self.store.memory.read_field(&field, &locals);
            self.write_place(frame, &place, final_value);
        }
        self.give_locals(locals);
        Ok(value)
    }

    pub(super) fn out_arg(&self, frame: &Frame, target: Target) -> (Value, Option<Place>) {
        match target {
            Target::Place(place) => (self.read_place(frame, &place), self.readable(frame, &place).then_some(place)),
            Target::Constant(value) => (value, None),
            Target::Nowhere => (Value::Int(0), None),
        }
    }

    pub(super) fn call_target(
        &mut self,
        frame: &Frame,
        callee: Callee,
        context: Option<ObjectId>,
        class: ClassId,
        link: Option<u32>,
    ) -> Result<Arc<Function>, Error> {
        let package = frame.function.package;
        match callee {
            Callee::Native(index) => {
                if let Some(function) = linked(frame, link).and_then(|index| self.table_function(index)) {
                    return Ok(function);
                }
                let (package, export) = self.native_function(index).ok_or(Error::UnknownNative(format!("index {index}")))?;
                let function = self.load_function(package, export)?;
                set_link(frame, link, u64::from(function.index));
                Ok(function)
            }
            Callee::Virtual(name) | Callee::Global(name) => {
                // Virtual calls see the target's state; `Global.` calls only its class, as do calls on a class object
                // (`class'X'.static.F()`: `class` is the class it stands for).
                let receiver = match (callee, context) {
                    (Callee::Virtual(_), Some(target)) if self.store.object(target).class.is_some() => Some(target),
                    _ => None,
                };
                let state = receiver.map_or(0, |target| self.object_state_key(target));
                let key = site_key(class, state);
                if let Some(key) = key
                    && let Some(value) = linked(frame, link)
                    && value & !SITE_FUNCTION_MASK == key
                    && let Some(function) = self.table_function(value & SITE_FUNCTION_MASK)
                {
                    return Ok(function);
                }
                let name = self.package_name(package, name);
                let found = match receiver {
                    Some(target) => self.find_object_function_id(target, name)?,
                    None => self.find_function_id(class, name)?,
                };
                let function = found.ok_or_else(|| Error::Unsupported(format!("no function {}", self.store.name(name))))?;
                if let Some(key) = key
                    && u64::from(function.index) < SITE_FUNCTION_MASK
                {
                    set_link(frame, link, key | u64::from(function.index));
                }
                Ok(function)
            }
            Callee::Final(reference) => {
                if let Some(function) = linked(frame, link).and_then(|index| self.table_function(index)) {
                    return Ok(function);
                }
                let (package, export) = self
                    .loader
                    .loaded_export(package, reference)
                    .ok_or(Error::Unsupported(format!("function reference {reference}")))?;
                let function = self.load_function(package, export)?;
                set_link(frame, link, u64::from(function.index));
                Ok(function)
            }
            Callee::Delegate { .. } => Err(Error::Unsupported("delegate call".to_owned())),
        }
    }

    fn table_function(&self, index: u64) -> Option<Arc<Function>> {
        self.script.lookups.function_table.get(usize::try_from(index).ok()?).cloned()
    }
}
