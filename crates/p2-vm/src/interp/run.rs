use std::sync::Arc;
use crate::memory::{LayoutId, Mem};
use crate::{ClassId, Error, ObjectId, LoaderRef, Value, Vm, natives, zero_slot};
use super::{
    ARG_POOL, CPF_OUT_PARM, FRAME_POOL, FUNC_LATENT, FUNC_SINGULAR, Flow, Frame, Function, MAX_SCRIPT_DEPTH,
    RECURSION_CHAIN, RUNAWAY,
};

impl<L: LoaderRef> Vm<L> {
    pub fn call(
        &mut self,
        function: &Arc<Function>,
        this: Option<ObjectId>,
        class: ClassId,
        args: impl IntoIterator<Item = Value>,
    ) -> Result<Option<Value>, Error> {
        if function.is_native() {
            let mut values = self.take_args();
            values.extend(args);
            let (value, params) = self.run(function, this, class, values)?;
            self.give_args(params);
            return Ok(value);
        }
        let mut locals = self.take_locals(function.mem);
        for (&index, value) in function.params.iter().zip(args) {
            let field = self.store.memory.get(function.mem).fields[index];
            self.store.memory.write_field(&field, &mut locals, value);
        }
        let (value, locals) = self.run_locals(function, this, class, locals)?;
        self.give_locals(locals);
        Ok(value)
    }

    /// Also returns the parameters' final values. More than [`MAX_SCRIPT_DEPTH`] nested calls fail,
    /// as in retail, instead of overflowing the stack.
    pub(crate) fn run(
        &mut self,
        function: &Arc<Function>,
        this: Option<ObjectId>,
        class: ClassId,
        args: Vec<Value>,
    ) -> Result<(Option<Value>, Vec<Value>), Error> {
        self.run_with(function, this, class, args, |_, values, k, _| values.get(k).cloned().unwrap_or(Value::Object(None)), Self::run_values)
    }

    pub(super) fn take_locals(&mut self, layout: LayoutId) -> Mem {
        let mut mem = self.script.frame_pool.pop().unwrap_or_else(|| {
            self.count(crate::profiling::Work::FrameAlloc);
            Mem::default()
        });
        self.store.memory.reset(layout, &mut mem);
        mem
    }

    pub(super) fn give_locals(&mut self, mem: Mem) {
        if self.script.frame_pool.len() < FRAME_POOL {
            self.script.frame_pool.push(mem);
        }
    }

    pub(crate) fn take_args(&mut self) -> Vec<Value> {
        self.script.arg_pool.pop().unwrap_or_else(|| {
            self.count(crate::profiling::Work::ArgVector);
            Vec::new()
        })
    }

    pub(crate) fn give_args(&mut self, mut args: Vec<Value>) {
        if self.script.arg_pool.len() < ARG_POOL {
            args.clear();
            self.script.arg_pool.push(args);
        }
    }

    pub(super) fn run_locals(&mut self, function: &Arc<Function>, this: Option<ObjectId>, class: ClassId, locals: Mem) -> Result<(Option<Value>, Mem), Error> {
        self.run_with(
            function,
            this,
            class,
            locals,
            |vm, locals, _, index| vm.store.memory.read_field(&vm.store.memory.get(function.mem).fields[index], locals),
            |vm, function, this, class, locals| {
                vm.trace_call(function);
                vm.count(crate::profiling::Work::ScriptCall);
                vm.interpret(function, this, class, locals)
            },
        )
    }

    fn run_with<A>(
        &mut self,
        function: &Arc<Function>,
        this: Option<ObjectId>,
        class: ClassId,
        args: A,
        hit_arg: impl Fn(&Self, &A, usize, usize) -> Value,
        run: impl FnOnce(&mut Self, &Arc<Function>, Option<ObjectId>, ClassId, A) -> Result<(Option<Value>, A), Error>,
    ) -> Result<(Option<Value>, A), Error> {
        if self.script.script_depth >= MAX_SCRIPT_DEPTH {
            return Err(Error::Recursion { calls: MAX_SCRIPT_DEPTH, chain: vec![function.object_name.clone()] });
        }
        // `singular`: an object already inside a singular function skips the call, giving its zero value.
        let singular = function.flags & FUNC_SINGULAR != 0 && this.is_some();
        if singular && let Some(object) = this
            && !self.script.in_singular.insert(object)
        {
            let value = function.return_value.map(|index| {
                let field = &function.layout.properties[index];
                zero_slot(&field.ty, field.array_dim)
            });
            return Ok((value, args));
        }
        if function.take_damage && let Some(victim) = this {
            let mut values = [Value::Map, Value::Map, Value::Map, Value::Map, Value::Map];
            let mut count = 0;
            for (k, &index) in function.params.iter().take(5).enumerate() {
                values[k] = hit_arg(&*self, &args, k, index);
                count = k + 1;
            }
            self.hit_seen(victim, &values[..count]);
        }
        self.script.script_depth += 1;
        let started = self.function_enter();
        let mut result = run(self, function, this, class, args);
        self.function_exit(function, started);
        self.script.script_depth -= 1;
        if singular && let Some(object) = this {
            self.script.in_singular.remove(&object);
        }
        if let Err(Error::Recursion { chain, .. }) = &mut result
            && chain.len() <= RECURSION_CHAIN
        {
            chain.push(format!("{}.{}", self.store.name(self.store.class(class).name), function.object_name));
        }
        result
    }

    fn trace_call(&mut self, function: &Function) {
        if let Some(trace) = &mut self.diagnostics.trace {
            trace.push(function.object_name.clone());
        }
    }

    fn run_values(&mut self, function: &Arc<Function>, this: Option<ObjectId>, class: ClassId, mut args: Vec<Value>) -> Result<(Option<Value>, Vec<Value>), Error> {
        self.trace_call(function);
        if let Some(op) = function.fast {
            return Ok((Some(crate::operators::operate(op, &mut args).unwrap_or(Value::Int(0))), args));
        }
        if function.is_native() {
            self.count(crate::profiling::Work::NativeCall);
            let value = match natives::call(self, &function.object_name, function.native_id, this, &mut args) {
                Ok(value) => value,
                Err(Error::UnknownNative(name)) => {
                    *self.diagnostics.missing_natives.entry(name).or_insert(0) += 1;
                    if function.flags & FUNC_LATENT != 0 {
                        self.script.latent.sleep = Some(0.0);
                    }
                    function.return_value.map(|index| {
                        let field = &function.layout.properties[index];
                        zero_slot(&field.ty, field.array_dim)
                    })
                }
                Err(e) => return Err(e),
            };
            return Ok((value, args));
        }
        self.count(crate::profiling::Work::ScriptCall);
        let mut locals = self.take_locals(function.mem);
        let fields = &self.store.memory.get(function.mem).fields;
        for (&index, value) in function.params.iter().zip(args.drain(..)) {
            self.store.memory.write_field(&fields[index], &mut locals, value);
        }
        let (value, locals) = self.interpret(function, this, class, locals)?;
        let fields = &self.store.memory.get(function.mem).fields;
        args.extend(function.params.iter().map(|&index| match function.layout.properties[index].flags & CPF_OUT_PARM {
            0 => Value::Map,
            _ => self.store.memory.read_field(&fields[index], &locals),
        }));
        self.give_locals(locals);
        Ok((value, args))
    }

    fn interpret(
        &mut self,
        function: &Arc<Function>,
        this: Option<ObjectId>,
        class: ClassId,
        locals: Mem,
    ) -> Result<(Option<Value>, Mem), Error> {
        let mut frame =
            Frame { function: Arc::clone(function), this, class, locals, switch: None, iterators: Vec::new() };
        let (mut pc, mut steps) = (0, 0);
        let compiled = self.compiled(function);
        while let Some(statement) = function.statements.get(pc) {
            steps += 1;
            if steps > RUNAWAY {
                return Err(Error::Unsupported(format!("runaway loop in {}", frame.function.object_name)));
            }
            if let Some(trace) = &mut self.diagnostics.trace {
                trace.push(format!("@{:04x}", statement.offset));
            }
            let flow = compiled.run(self, &mut frame, pc)?;
            match flow {
                Flow::Next => pc += 1,
                Flow::Goto(index) => pc = index,
                Flow::Jump(offset) => {
                    pc = frame
                        .function
                        .statements
                        .binary_search_by_key(&u32::from(offset), |s| s.offset)
                        .map_err(|_| Error::BadJump(offset))?;
                }
                Flow::Return(value) => {
                    let value = value.or_else(|| self.return_slot(&frame));
                    return Ok((value, frame.locals));
                }
            }
        }
        Ok((self.return_slot(&frame), frame.locals))
    }

    fn return_slot(&self, frame: &Frame) -> Option<Value> {
        let index = frame.function.return_value?;
        let field = self.store.memory.get(frame.function.mem).fields.get(index)?;
        Some(self.store.memory.read_field(field, &frame.locals))
    }

    pub(crate) fn state_frame(&mut self, code: &Arc<Function>, object: ObjectId) -> Result<Frame, Error> {
        let class = self.store.object(object).class.ok_or(Error::Unsupported("state code of a stub".to_owned()))?;
        let locals = self.store.memory.zero(code.mem);
        Ok(Frame { function: Arc::clone(code), this: Some(object), class, locals, switch: None, iterators: Vec::new() })
    }
}
