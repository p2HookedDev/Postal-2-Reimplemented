use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use p2_package::PackageId;
use p2_script::{Callee, Expr, ScriptObject, Source, Statement, StructLayout, class_name, decode_script_linked, read_export};

use p2_math::{Rotator, Vector};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::Prop;
use crate::memory::{Array, Heap, LayoutId, Mem, MemLayouts, Shape};
use crate::{ClassId, Error, NameId, ObjectId, LoaderRef, Value, Vm, natives, zero_slot};

pub(crate) mod compile;

const MAX_SCRIPT_DEPTH: u32 = 1000;
const RECURSION_CHAIN: usize = 24;
const CPF_PARM: u32 = 0x80;
const CPF_RETURN_PARM: u32 = 0x400;
const CPF_OUT_PARM: u32 = 0x100;
const RUNAWAY: usize = 1_000_000;
const FUNC_LATENT: u32 = 0x08;
const FUNC_NATIVE: u32 = 0x400;
const FUNC_SINGULAR: u32 = 0x20;

#[derive(Debug)]
pub struct Function {
    pub object_name: String,
    pub package: PackageId,
    pub export: usize,
    pub layout: Arc<StructLayout>,
    pub(crate) mem: LayoutId,
    pub statements: Vec<Statement>,
    pub native: u16,
    pub flags: u32,
    pub params: Vec<usize>,
    pub return_value: Option<usize>,
    pub(crate) fast: Option<crate::native_id::Native>,
    pub(crate) native_id: Option<crate::native_id::Native>,
    pub(crate) take_damage: bool,
    /// What each `Expr::Linked` reference of `statements` resolved to, filled on its first run ([`UNLINKED`] until then): a
    /// local's slot, a member index, a `NameId`, or a packed `member_link`, `site_key` or `cast_link`. Links name this VM's
    /// classes and function table: a `Function` belongs to the VM that decoded it.
    pub(crate) links: Box<[AtomicU64]>,
    pub(crate) index: u32,
    pub(crate) labels: FxHashMap<NameId, u32>,
}

const UNLINKED: u64 = u64::MAX;
const NO_MEMBER: u64 = u32::MAX as u64;

fn linked(frame: &Frame, link: Option<u32>) -> Option<u64> {
    let value = frame.function.links.get(link? as usize)?.load(Ordering::Relaxed);
    (value != UNLINKED).then_some(value)
}

fn set_link(frame: &Frame, link: Option<u32>, value: u64) {
    if let Some(entry) = link.and_then(|link| frame.function.links.get(link as usize)) {
        entry.store(value, Ordering::Relaxed);
    }
}

fn member_link(class: ClassId, index: Option<usize>) -> u64 {
    (u64::from(class.0) << 32) | index.map_or(NO_MEMBER, |index| index as u64)
}

fn member_of_link(value: u64, class: ClassId) -> Option<Option<usize>> {
    if (value >> 32) as u32 != class.0 {
        return None;
    }
    let index = value & NO_MEMBER;
    Some((index != NO_MEMBER).then_some(index as usize))
}

/// Bits of a virtual / global call site's link for the function index (the low ones), the receiver's state key
/// (next) and its class (the high ones). No part is ever all ones, so a link is never [`UNLINKED`].
const SITE_FUNCTION_BITS: u32 = 24;
const SITE_STATE_BITS: u32 = 20;
const SITE_FUNCTION_MASK: u64 = (1 << SITE_FUNCTION_BITS) - 1;

fn site_key(class: ClassId, state: u32) -> Option<u64> {
    let fits = u64::from(class.0) < (1 << (64 - SITE_FUNCTION_BITS - SITE_STATE_BITS)) - 1 && u64::from(state) < (1 << SITE_STATE_BITS) - 1;
    fits.then_some((u64::from(class.0) << (SITE_FUNCTION_BITS + SITE_STATE_BITS)) | (u64::from(state) << SITE_FUNCTION_BITS))
}

fn cast_link(class: ClassId, keeps: bool) -> u64 {
    (u64::from(class.0) << 1) | u64::from(keeps)
}

fn cast_of_link(value: u64, class: ClassId) -> Option<bool> {
    (value >> 1 == u64::from(class.0)).then_some(value & 1 != 0)
}

fn is_native_declaration(native: u16, flags: u32) -> bool {
    native != 0 || flags & FUNC_NATIVE != 0
}

impl Function {
    pub(crate) fn is_native(&self) -> bool {
        is_native_declaration(self.native, self.flags)
    }

    fn is_logical(&self) -> bool {
        matches!(self.native_id, Some(crate::native_id::Native::AndAnd_BoolBool | crate::native_id::Native::OrOr_BoolBool))
    }
}

const FRAME_POOL: usize = 256;
const ARG_POOL: usize = 256;

pub(crate) enum StepResult {
    Next,
    Goto(usize),
    End,
}

enum Flow {
    Next,
    /// To a code offset.
    Jump(u16),
    /// To a statement index.
    Goto(usize),
    Return(Option<Value>),
}

pub(crate) struct Frame {
    function: Arc<Function>,
    this: Option<ObjectId>,
    class: ClassId,
    locals: Mem,
    switch: Option<Value>,
    iterators: Vec<Iteration>,
}

struct Iteration {
    items: Vec<Value>,
    next: usize,
    statement: usize,
    outs: &'static [usize],
    body: usize,
    walk: Option<ListWalk>,
}

struct ListWalk {
    name: &'static str,
    target: Option<ObjectId>,
    args: Vec<Value>,
    first_new: u32,
}

const MAX_HOPS: usize = 4;

#[derive(Debug, Clone, Copy)]
enum Root {
    Local,
    Instance(ObjectId),
    Default(ClassId),
}

#[derive(Debug, Clone, Copy)]
struct Hop {
    heap: u32,
    index: u32,
    elem: LayoutId,
}

#[derive(Debug, Clone, Copy)]
struct Place {
    root: Root,
    hops: [Hop; MAX_HOPS],
    depth: u8,
    word: u32,
    heap: u32,
    shape: Shape,
    dim: u32,
    grows: bool,
}

impl Place {
    fn new(root: Root, field: &crate::memory::Field) -> Self {
        let hop = Hop { heap: 0, index: 0, elem: LayoutId::NONE };
        Self { root, hops: [hop; MAX_HOPS], depth: 0, word: field.word, heap: field.heap, shape: field.shape, dim: field.dim, grows: false }
    }
}

enum Target {
    Place(Place),
    /// A variable of None ("Accessed None") or out of bounds: reads give zero, writes are dropped.
    Nowhere,
    Constant(Value),
}

fn block<'m>(mut mem: &'m Mem, place: &Place) -> Option<&'m Mem> {
    for hop in &place.hops[..place.depth as usize] {
        let Some(Heap::Array(array)) = mem.heap.get(hop.heap as usize) else { return None };
        if hop.index >= array.len {
            return None;
        }
        mem = &array.mem;
    }
    Some(mem)
}

/// `None`: out of bounds, the write is dropped. Only `A[i] = x` grows the last dynamic array.
/// `A[i].m = x` out of range is logged and lost: `DoorMover.FitTheEffect` relies on it.
fn block_mut<'m>(memory: &MemLayouts, mut mem: &'m mut Mem, place: &Place) -> Option<&'m mut Mem> {
    let last = place.depth as usize;
    for (n, hop) in place.hops[..last].iter().enumerate() {
        let Some(Heap::Array(array)) = mem.heap.get_mut(hop.heap as usize) else { return None };
        if hop.index >= array.len {
            if !(place.grows && n + 1 == last) {
                return None;
            }
            memory.resize(hop.elem, array, hop.index as usize + 1);
        }
        mem = &mut array.mem;
    }
    Some(mem)
}

impl<L: LoaderRef> Vm<L> {
    pub fn find_function(&mut self, class: ClassId, name: &str) -> Result<Option<Arc<Function>>, Error> {
        let name = self.intern(name);
        self.find_function_id(class, name)
    }

    pub fn find_function_id(&mut self, class: ClassId, name: NameId) -> Result<Option<Arc<Function>>, Error> {
        if let Some(found) = self.script.lookups.functions.get(&(class, None, name)) {
            return Ok(found.clone());
        }
        let text = self.name(name).to_owned();
        let found = self.search_function(class, &text)?;
        self.script.lookups.functions.insert((class, None, name), found.clone());
        Ok(found)
    }

    fn search_function(&mut self, class: ClassId, name: &str) -> Result<Option<Arc<Function>>, Error> {
        self.count(crate::profiling::Work::FunctionSearch);
        let mut current = Some(class);
        while let Some(class) = current {
            let (package, export) = (self.class(class).package, self.class(class).export);
            if let Some((package, export)) = self.find_child(package, export, name, false)? {
                return self.load_function(package, export).map(Some);
            }
            current = self.class(class).super_class;
        }
        Ok(None)
    }

    pub(crate) fn find_child(
        &mut self,
        package: PackageId,
        export: usize,
        name: &str,
        state: bool,
    ) -> Result<Option<(PackageId, usize)>, Error> {
        Ok(self.children_of(package, export)?.get(&(name.to_ascii_lowercase(), state)).copied())
    }

    pub(crate) fn children_of(&mut self, package: PackageId, export: usize) -> Result<&FxHashMap<(String, bool), (PackageId, usize)>, Error> {
        if !self.script.children.contains_key(&(package, export)) {
            let phase = self.profile_start();
            let mut children = FxHashMap::default();
            let loaded = self.loader.package(package);
            let definition = read_export(&loaded.package, &loaded.data, export)?;
            let mut child = definition.as_ref().and_then(ScriptObject::as_struct).map_or(0, |s| s.children);
            // Bounded: a malformed `next` chain may loop.
            for _ in 0..=loaded.package.imports.len() + loaded.package.exports.len() {
                let Some((child_package, child_export)) = self.loader.loaded_export(package, child) else { break };
                let child_pkg = self.loader.package(child_package);
                let object = read_export(&child_pkg.package, &child_pkg.data, child_export)?;
                let is_state = match &object {
                    Some(ScriptObject::Function(_)) => Some(false),
                    Some(ScriptObject::State(_)) => Some(true),
                    _ => None,
                };
                if let Some(is_state) = is_state {
                    let child_name = child_pkg.package.name(child_pkg.package.exports[child_export].object_name);
                    children.entry((child_name.to_ascii_lowercase(), is_state)).or_insert((child_package, child_export));
                }
                child = object.and_then(|o| o.field()).map_or(0, |f| f.next);
            }
            self.script.children.insert((package, export), children);
            self.profile_phase("load: functions of a class", phase);
        }
        Ok(&self.script.children[&(package, export)])
    }

    pub fn load_function(&mut self, package: PackageId, export: usize) -> Result<Arc<Function>, Error> {
        if let Some(function) = self.script.functions.get(&(package, export)) {
            return Ok(Arc::clone(function));
        }
        let phase = self.profile_start();
        let loaded = self.loader.package(package);
        let pkg = &loaded.package;
        let (base, native, flags) = match read_export(pkg, &loaded.data, export)? {
            Some(ScriptObject::Function(f)) => (f.base, f.native, f.flags),
            Some(ScriptObject::State(s)) => (s.base, 0, 0),
            _ => return Err(Error::BadExport(export)),
        };
        let layout = self.layouts.layout(Source { package: pkg, data: &loaded.data }, export)?;
        let (statements, links) = decode_script_linked(pkg, &base.script, base.script_size)?;
        // The layout repeats the overridden function's properties after the function's own: first names win.
        let mut seen = FxHashSet::default();
        let (mut params, mut return_value) = (Vec::new(), None);
        for (index, field) in layout.properties.iter().enumerate() {
            if !seen.insert(field.name.to_ascii_lowercase()) {
                continue;
            }
            if field.flags & CPF_RETURN_PARM != 0 {
                return_value = Some(index);
            } else if field.flags & CPF_PARM != 0 {
                params.push(index);
            }
        }
        let object_name = pkg.name(pkg.exports[export].object_name).to_owned();
        let native_id = if is_native_declaration(native, flags) { crate::native_id::Native::from_name(&object_name) } else { None };
        let mem = self.store.memory.of(&layout);
        let take_damage = object_name.eq_ignore_ascii_case("TakeDamage") && params.len() >= 5;
        let mut labels = FxHashMap::default();
        for statement in &statements {
            if let Expr::LabelTable(table) = &statement.expr {
                for &(name, offset) in table {
                    if let Ok(offset) = u32::try_from(offset) {
                        labels.entry(self.package_name(package, name)).or_insert(offset);
                    }
                }
            }
        }
        let function = Arc::new(Function {
            object_name,
            package,
            export,
            layout,
            mem,
            statements,
            native,
            flags,
            params,
            return_value,
            native_id,
            fast: native_id.filter(|native| native.is_fast()),
            take_damage,
            links: (0..links).map(|_| AtomicU64::new(UNLINKED)).collect(),
            index: u32::try_from(self.script.lookups.function_table.len()).unwrap_or(u32::MAX),
            labels,
        });
        self.script.functions.insert((package, export), Arc::clone(&function));
        self.script.lookups.function_table.push(Arc::clone(&function));
        self.profile_phase("load: function", phase);
        Ok(function)
    }

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

    fn take_locals(&mut self, layout: LayoutId) -> Mem {
        let mut mem = self.script.frame_pool.pop().unwrap_or_else(|| {
            self.count(crate::profiling::Work::FrameAlloc);
            Mem::default()
        });
        self.store.memory.reset(layout, &mut mem);
        mem
    }

    fn give_locals(&mut self, mem: Mem) {
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

    fn run_locals(&mut self, function: &Arc<Function>, this: Option<ObjectId>, class: ClassId, locals: Mem) -> Result<(Option<Value>, Mem), Error> {
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
            chain.push(format!("{}.{}", self.name(self.class(class).name), function.object_name));
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
        let class = self.object(object).class.ok_or(Error::Unsupported("state code of a stub".to_owned()))?;
        let locals = self.store.memory.zero(code.mem);
        Ok(Frame { function: Arc::clone(code), this: Some(object), class, locals, switch: None, iterators: Vec::new() })
    }

    pub(crate) fn exec_statement(&mut self, frame: &mut Frame, compiled: &compile::Code<L>, pc: usize) -> Result<StepResult, Error> {
        let flow = compiled.run(self, frame, pc)?;
        Ok(match flow {
            Flow::Next => StepResult::Next,
            Flow::Goto(index) => StepResult::Goto(index),
            Flow::Jump(offset) => StepResult::Goto(
                frame
                    .function
                    .statements
                    .binary_search_by_key(&u32::from(offset), |s| s.offset)
                    .map_err(|_| Error::BadJump(offset))?,
            ),
            Flow::Return(_) => StepResult::End,
        })
    }

    fn exec(&mut self, frame: &mut Frame, expr: &Expr, pc: usize) -> Result<Flow, Error> {
        self.count(crate::profiling::Work::Statement);
        Ok(match expr {
            Expr::Iterator { iterator, end } => self.exec_iterator(frame, iterator, *end, pc)?,
            Expr::IteratorNext => match frame.iterators.pop() {
                Some(mut iteration) => {
                    let body = Flow::Goto(iteration.body);
                    let flow = self.advance(frame, &mut iteration, body, Flow::Next)?;
                    frame.iterators.push(iteration);
                    flow
                }
                None => Flow::Next,
            },
            Expr::GotoLabel(label) => {
                let Value::Name(label) = self.eval(frame, label, frame.this)? else { return Ok(Flow::Return(None)) };
                match frame.function.labels.get(&label) {
                    Some(&offset) => Flow::Jump(offset as u16),
                    None => Flow::Return(None),
                }
            }
            Expr::Assert { line, condition } => {
                if !truthy(&self.eval(frame, condition, frame.this)?) {
                    return Err(Error::AssertFailed { function: frame.function.object_name.clone(), line: *line });
                }
                Flow::Next
            }
            Expr::IteratorPop => {
                frame.iterators.pop();
                Flow::Next
            }
            Expr::DynArrayInsert { array, index, count } | Expr::DynArrayRemove { array, index, count } => {
                self.exec_array_change(frame, array, index, count, matches!(expr, Expr::DynArrayInsert { .. }))?;
                Flow::Next
            }
            Expr::Let { target, value } | Expr::LetBool { target, value } | Expr::LetDelegate { target, value } => {
                let value = self.eval(frame, value, frame.this)?;
                self.assign(frame, target, value)?;
                Flow::Next
            }
            other => {
                self.eval(frame, other, frame.this)?;
                Flow::Next
            }
        })
    }

    fn exec_iterator(&mut self, frame: &mut Frame, iterator: &Expr, end: u16, pc: usize) -> Result<Flow, Error> {
        let (call, target) = match iterator {
            Expr::Context { object, member, .. } => match self.eval(frame, object, frame.this)? {
                Value::Object(Some(object)) => (&**member, Some(object)),
                _ => return Ok(Flow::Jump(end)),
            },
            call => (call, frame.this),
        };
        let (call, link) = unlink(call);
        let Expr::Call { callee, args } = call else {
            return Err(Error::Unsupported("iterator that is not a call".to_owned()));
        };
        let function = match callee {
            Callee::Native(_) | Callee::Final(_) => self.call_target(frame, *callee, None, frame.class, link)?,
            _ => return Err(Error::Unsupported("iterator called by name".to_owned())),
        };
        let mut values = Vec::new();
        for (position, arg) in args.iter().enumerate() {
            if position != 1 {
                values.push(if matches!(arg, Expr::Nothing) { Value::Map } else { self.eval(frame, arg, frame.this)? });
            }
        }
        let first_new = self.store.objects.len() as u32;
        let items = match natives::iterate(self, &function.object_name, target, &values) {
            Ok(items) => items,
            Err(Error::UnknownNative(name)) => {
                *self.diagnostics.missing_natives.entry(name).or_insert(0) += 1;
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        if args.get(1).is_none() {
            return Err(Error::Unsupported("iterator without a variable".to_owned()));
        }
        let outs = natives::iterator_outs(&function.object_name);
        let walk = natives::walks_actor_list(&function.object_name).map(|name| {
            let mut values = values;
            // An omitted `Loc` is read once, here: the loop body may move the caller.
            if matches!(name, "RadiusActors" | "VisibleActors")
                && !values.get(2).is_some_and(natives::is_struct_value)
                && let Some(target) = target
            {
                values.truncate(2);
                values.push(Value::Vector(self.vector(target, Prop::Location)));
            }
            ListWalk { name, target, args: values, first_new }
        });
        let mut iteration = Iteration { items, next: 0, statement: pc, outs, body: pc + 1, walk };
        let flow = self.advance(frame, &mut iteration, Flow::Next, Flow::Jump(end))?;
        frame.iterators.push(iteration);
        Ok(flow)
    }

    fn exec_array_change(&mut self, frame: &mut Frame, array: &Expr, index: &Expr, count: &Expr, insert: bool) -> Result<(), Error> {
        let at = int(&self.eval(frame, index, frame.this)?).max(0) as usize;
        let count = int(&self.eval(frame, count, frame.this)?).max(0) as usize;
        if let Target::Place(place) = self.place(frame, array, frame.this)?
            && let Some((memory, items, elem)) = self.array_mut(frame, &place)
        {
            if insert {
                memory.insert(elem, items, at, count);
            } else {
                memory.remove(elem, items, at, count);
            }
        }
        Ok(())
    }

    fn walk_spawned(&mut self, iteration: &mut Iteration) -> Result<bool, Error> {
        let Some(walk) = &mut iteration.walk else { return Ok(false) };
        let (first_new, made) = (walk.first_new, self.store.objects.len() as u32);
        if made == first_new {
            return Ok(false);
        }
        walk.first_new = made;
        let items = natives::iterate(self, walk.name, walk.target, &walk.args)?;
        iteration.items.extend(items.into_iter().filter(|item| matches!(item, &Value::Object(Some(o)) if o.0 >= first_new)));
        Ok(iteration.next < iteration.items.len())
    }

    /// An actor destroyed since the iterator ran is passed over; retail tests this at each step.
    fn advance(&mut self, frame: &mut Frame, iteration: &mut Iteration, more: Flow, done: Flow) -> Result<Flow, Error> {
        let item = loop {
            let Some(item) = iteration.items.get(iteration.next).cloned() else {
                if self.walk_spawned(iteration)? {
                    continue;
                }
                return Ok(done);
            };
            iteration.next += 1;
            if !matches!(item, Value::Object(Some(object)) if self.is_deleted(object)) {
                break item;
            }
        };
        if matches!(item, Value::Map) {
            return Ok(more);
        }
        let function = Arc::clone(&frame.function);
        let args = iterator_args(&function.statements[iteration.statement].expr);
        match item {
            Value::Struct(parts) if !iteration.outs.is_empty() => {
                let targets = std::iter::once(1).chain(iteration.outs.iter().copied()).filter_map(|position| args.get(position));
                for (target, value) in targets.zip(parts) {
                    self.assign(frame, target, value)?;
                }
            }
            item => {
                if let Some(target) = args.get(1) {
                    self.assign(frame, target, item)?;
                }
            }
        }
        Ok(more)
    }

    fn eval(&mut self, frame: &mut Frame, expr: &Expr, context: Option<ObjectId>) -> Result<Value, Error> {
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

    fn name_const(&mut self, frame: &Frame, link: Option<u32>, name: usize) -> NameId {
        if let Some(id) = linked(frame, link) {
            return NameId(id as u32);
        }
        let id = self.package_name(frame.function.package, name);
        set_link(frame, link, u64::from(id.0));
        id
    }

    /// Found once per reference until the level is left (`leave_level` drops the map's objects). Only found objects are
    /// kept: a reference whose object failed to load gives its stub next time.
    fn object_const(&mut self, package: PackageId, reference: i32) -> Value {
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
                Value::Name(name) if name != NameId::NONE => Some(self.name(name).to_owned()),
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
                self.set(object, Prop::Outer, Value::Object(outer));
                Value::Object(Some(object))
            }
            None => Value::Object(None),
        })
    }

    fn in_place_ok(&self) -> bool {
        self.diagnostics.trace.is_none() && self.diagnostics.function_profile.is_none() && self.script.script_depth < MAX_SCRIPT_DEPTH
    }

    fn eval_call(
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
    fn call_on_stub(&mut self, frame: &Frame, callee: Callee, context: Option<ObjectId>) -> bool {
        if let (Callee::Virtual(name), Some(target)) = (callee, context)
            && self.object(target).class.is_none()
            && self.object(target).class_name != self.script.lookups.class_names.class
            && {
                self.load_stub(target);
                self.object(target).class.is_none()
            }
        {
            let key = (self.object(target).class_name, frame.function.package, name);
            if !self.script.lookups.stub_calls.contains_key(&key) {
                let text = format!("call on stub: {}.{}", self.name(key.0), self.loader.package(key.1).package.name(name));
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

    fn run_native(
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
    fn call_script(
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

    fn out_arg(&self, frame: &Frame, target: Target) -> (Value, Option<Place>) {
        match target {
            Target::Place(place) => (self.read_place(frame, &place), self.readable(frame, &place).then_some(place)),
            Target::Constant(value) => (value, None),
            Target::Nowhere => (Value::Int(0), None),
        }
    }

    fn call_target(
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
                    (Callee::Virtual(_), Some(target)) if self.object(target).class.is_some() => Some(target),
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
                let function = found.ok_or_else(|| Error::Unsupported(format!("no function {}", self.name(name))))?;
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

    fn meta_cast(&mut self, frame: &Frame, link: Option<u32>, class: i32, value: Value) -> Result<Value, Error> {
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

    fn dynamic_cast(&mut self, frame: &Frame, link: Option<u32>, class: i32, value: Value) -> Value {
        let package = frame.function.package;
        let Value::Object(Some(object)) = value else { return Value::Object(None) };
        let key = self.object(object).class.map(|of| (of, package, class, false));
        if let Some(keeps) = key.and_then(|key| self.known_cast(frame, link, key)) {
            return Value::Object(keeps.then_some(object));
        }
        let target = self.cast_target(package, class);
        let class_name = self.name(target);
        // `Viewport` is C++-only: the VM's one `Player`, the local one, stands for it (scripts find the local
        // player by `ViewPort(Player) != None`, e.g. the ragdoll pool in `P2MoCapPawn.GetKarmaSkeleton`).
        let viewport = class_name.eq_ignore_ascii_case("Viewport") && self.is_a(object, "Player");
        let keeps = self.is_a(object, target) || self.stub_is_a(object, class_name) || viewport;
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

    fn assign(&mut self, frame: &mut Frame, target: &Expr, value: Value) -> Result<(), Error> {
        if let Expr::DynArrayLength(array) = target {
            if let Target::Place(place) = self.place(frame, array, frame.this)?
                && let Some((memory, items, elem)) = self.array_mut(frame, &place)
            {
                memory.resize(elem, items, int(&value).max(0) as usize);
            }
            return Ok(());
        }
        match self.place(frame, target, frame.this)? {
            Target::Place(place) => self.write_place(frame, &place, value),
            Target::Nowhere | Target::Constant(_) => {}
        }
        Ok(())
    }

    fn root<'s>(&'s self, frame: &'s Frame, root: Root) -> Option<&'s Mem> {
        Some(match root {
            Root::Local => &frame.locals,
            Root::Instance(object) => &self.store.objects.get(object.0 as usize)?.mem,
            Root::Default(class) => &self.store.classes.get(class.0 as usize)?.defaults,
        })
    }

    fn readable(&self, frame: &Frame, place: &Place) -> bool {
        self.root(frame, place.root).and_then(|mem| block(mem, place)).is_some()
    }

    fn read_place(&self, frame: &Frame, place: &Place) -> Value {
        let empty = Mem::default();
        let (mem, word, heap) = match self.root(frame, place.root).and_then(|mem| block(mem, place)) {
            Some(mem) => (mem, place.word as usize, place.heap as usize),
            None => (&empty, 0, 0),
        };
        self.store.memory.read(place.shape, place.dim, mem, word, heap)
    }

    fn write_place(&mut self, frame: &mut Frame, place: &Place, value: Value) {
        let memory = &self.store.memory;
        let mem = match place.root {
            Root::Local => &mut frame.locals,
            Root::Instance(object) => match self.store.objects.get_mut(object.0 as usize) {
                Some(object) => &mut object.mem,
                None => return,
            },
            Root::Default(class) => match self.store.classes.get_mut(class.0 as usize) {
                Some(class) => &mut class.defaults,
                None => return,
            },
        };
        if let Some(mem) = block_mut(memory, mem, place) {
            memory.write(place.shape, place.dim, mem, place.word as usize, place.heap as usize, value);
        }
    }

    fn array_mut<'s>(&'s mut self, frame: &'s mut Frame, place: &Place) -> Option<(&'s MemLayouts, &'s mut Array, LayoutId)> {
        let (Shape::Array(elem), 1) = (place.shape, place.dim) else { return None };
        let memory = &self.store.memory;
        let mem = match place.root {
            Root::Local => &mut frame.locals,
            Root::Instance(object) => &mut self.store.objects.get_mut(object.0 as usize)?.mem,
            Root::Default(class) => &mut self.store.classes.get_mut(class.0 as usize)?.defaults,
        };
        match block_mut(memory, mem, place)?.heap.get_mut(place.heap as usize)? {
            Heap::Array(array) => Some((memory, array, elem)),
            Heap::Str(_) => None,
        }
    }

    fn array_len(&self, frame: &Frame, place: &Place) -> usize {
        let (Shape::Array(_), 1) = (place.shape, place.dim) else { return 0 };
        match self.root(frame, place.root).and_then(|mem| block(mem, place)).and_then(|mem| mem.heap.get(place.heap as usize)) {
            Some(Heap::Array(array)) => array.len as usize,
            _ => 0,
        }
    }

    /// A static array's index clamps into range, as retail; a dynamic array's is a hop into its elements.
    fn element(&self, mut place: Place, index: i32) -> Target {
        match place.shape {
            _ if place.dim > 1 => {
                let index = index.clamp(0, place.dim as i32 - 1) as u32;
                let (words, heaps) = self.store.memory.size(place.shape);
                place.word += index * words;
                place.heap += index * heaps;
                place.dim = 1;
                Target::Place(place)
            }
            Shape::Array(elem) if (place.depth as usize) < MAX_HOPS && index >= 0 => {
                place.hops[place.depth as usize] = Hop { heap: place.heap, index: index as u32, elem };
                place.depth += 1;
                let (words, heaps) = self.store.memory.element_size(elem);
                let field = self.store.memory.get(elem).fields[0];
                place.word = index as u32 * words;
                place.heap = index as u32 * heaps;
                place.shape = field.shape;
                place.dim = field.dim;
                place.grows = true;
                Target::Place(place)
            }
            _ => Target::Nowhere,
        }
    }

    fn place(&mut self, frame: &mut Frame, expr: &Expr, context: Option<ObjectId>) -> Result<Target, Error> {
        let package = frame.function.package;
        let property_name = |vm: &Self, reference: i32| {
            vm.loader.package(package).package.object_name(reference).unwrap_or("").to_owned()
        };
        let (expr, link) = unlink(expr);
        Ok(match expr {
            Expr::BoolVariable(inner) => self.place(frame, inner, context)?,
            Expr::Context { object, member, .. } | Expr::ClassContext { object, member, .. } => {
                match self.eval(frame, object, context)? {
                    Value::Object(Some(object)) => self.place(frame, member, Some(object))?,
                    _ => Target::Nowhere,
                }
            }
            Expr::LocalVariable(reference) => {
                let index = match linked(frame, link) {
                    Some(index) => index as usize,
                    None => {
                        let name = property_name(self, *reference);
                        let index = frame.function.layout.properties.iter().position(|p| p.name.eq_ignore_ascii_case(&name));
                        let index = index.ok_or(Error::Unsupported(format!("local {name}")))?;
                        set_link(frame, link, index as u64);
                        index
                    }
                };
                self.local_target(frame, index)
            }
            Expr::InstanceVariable(reference) => {
                let object = context.ok_or_else(|| Error::Unsupported(format!("{} without an object", property_name(self, *reference))))?;
                if let Some(class) = self.object(object).class {
                    let index = match linked(frame, link).and_then(|value| member_of_link(value, class)) {
                        Some(index) => index,
                        None => {
                            let key = (class, package, *reference);
                            let index = match self.script.lookups.members.get(&key) {
                                Some(&index) => index,
                                None => {
                                    let index = self.reference_property(class, package, *reference);
                                    self.script.lookups.members.insert(key, index);
                                    index
                                }
                            };
                            set_link(frame, link, member_link(class, index));
                            index
                        }
                    };
                    let index = index.ok_or_else(|| Error::Unsupported(format!("property {}", property_name(self, *reference))))?;
                    return Ok(self.instance_target(object, class, index));
                }
                Target::Constant(self.stub_member(object, package, *reference))
            }
            Expr::DefaultVariable(reference) => {
                let class = self.context_class(context)?.unwrap_or(frame.class);
                let cached = linked(frame, link).and_then(|value| member_of_link(value, class));
                let index = match cached {
                    Some(index) => index,
                    None => {
                        let key = (class, package, *reference);
                        let index = match self.script.lookups.members.get(&key) {
                            Some(&index) => index,
                            None => {
                                let index = self.reference_property(class, package, *reference);
                                self.script.lookups.members.insert(key, index);
                                index
                            }
                        };
                        set_link(frame, link, member_link(class, index));
                        index
                    }
                };
                let index = index.ok_or_else(|| Error::Unsupported(format!("default {}", property_name(self, *reference))))?;
                match self.store.memory.get(self.class(class).mem).fields.get(index) {
                    Some(field) => Target::Place(Place::new(Root::Default(class), field)),
                    None => Target::Nowhere,
                }
            }
            Expr::ArrayElement { index, array } | Expr::DynArrayElement { index, array } => {
                let element = int(&self.eval(frame, index, frame.this)?);
                match self.place(frame, array, context)? {
                    Target::Place(place) => self.element(place, element),
                    _ => Target::Nowhere,
                }
            }
            Expr::StructMember { member, value } => {
                let index = self.linked_member_index(frame, link, *member)?;
                let target = self.place(frame, value, context)?;
                self.struct_member_target(target, index)
            }
            other => return Err(Error::Unsupported(format!("assign to {other:?}").chars().take(60).collect())),
        })
    }

    fn local_target(&self, frame: &Frame, index: usize) -> Target {
        match self.store.memory.get(frame.function.mem).fields.get(index) {
            Some(field) => Target::Place(Place::new(Root::Local, field)),
            None => Target::Nowhere,
        }
    }

    fn instance_target(&self, object: ObjectId, class: ClassId, index: usize) -> Target {
        match self.store.memory.get(self.class(class).mem).fields.get(index) {
            Some(field) => Target::Place(Place::new(Root::Instance(object), field)),
            None => Target::Nowhere,
        }
    }

    fn struct_member_target(&self, target: Target, index: usize) -> Target {
        match target {
            Target::Place(mut place) => match (place.shape, place.dim) {
                (Shape::Struct(id), 1) => match self.store.memory.get(id).fields.get(index) {
                    Some(field) => {
                        place.word += field.word;
                        place.heap += field.heap;
                        place.shape = field.shape;
                        place.dim = field.dim;
                        place.grows = false;
                        Target::Place(place)
                    }
                    None => Target::Nowhere,
                },
                _ => Target::Nowhere,
            },
            _ => Target::Nowhere,
        }
    }

    fn linked_member_index(&mut self, frame: &Frame, link: Option<u32>, member: i32) -> Result<usize, Error> {
        if let Some(index) = linked(frame, link) {
            return Ok(index as usize);
        }
        let index = self.find_member_index(frame.function.package, member)?;
        set_link(frame, link, index as u64);
        Ok(index)
    }

    fn find_member_index(&mut self, package: PackageId, member: i32) -> Result<usize, Error> {
        let missing = || Error::Unsupported(format!("struct member {member}"));
        let (member_package, member_export) = self.loader.loaded_export(package, member).ok_or_else(missing)?;
        let loaded = self.loader.package(member_package);
        let entry = &loaded.package.exports[member_export];
        let name = loaded.package.name(entry.object_name);
        let struct_export = usize::try_from(entry.package_index - 1).map_err(|_| missing())?;
        let layout = self.layouts.layout(Source { package: &loaded.package, data: &loaded.data }, struct_export)?;
        layout.properties.iter().position(|p| p.name.eq_ignore_ascii_case(name)).ok_or_else(missing)
    }

    fn context_class(&mut self, context: Option<ObjectId>) -> Result<Option<ClassId>, Error> {
        let Some(object) = context else { return Ok(None) };
        if let Some(class) = self.object(object).class {
            return Ok(Some(class));
        }
        if self.object(object).class_name == self.script.lookups.class_names.class {
            return self.class_of_object(object);
        }
        Ok(None)
    }

    #[inline]
    pub fn is_a(&self, object: ObjectId, class_name: impl crate::ClassName) -> bool {
        self.store.is_a(object, class_name)
    }

    pub fn stub_is_a(&self, object: ObjectId, class_name: &str) -> bool {
        let object = self.object(object);
        if object.class.is_some() {
            return false;
        }
        let mut current = self.name(object.class_name);
        let mut loaded = self.store.class_by_name.get(&object.class_name).copied();
        for _ in 0..16 {
            if current.eq_ignore_ascii_case(class_name) {
                return true;
            }
            if let Some(class) = loaded {
                return self.class_is_a(class, class_name);
            }
            match INTRINSIC_SUPER.iter().find(|(class, _)| class.eq_ignore_ascii_case(current)) {
                Some(&(_, parent)) => {
                    current = parent;
                    loaded = self.store.name_ids.get(&parent.to_ascii_lowercase()).and_then(|name| self.store.class_by_name.get(name)).copied();
                }
                None => return false,
            }
        }
        false
    }

    pub fn class_is_a(&self, class: ClassId, class_name: impl crate::ClassName) -> bool {
        class_name.is_base_of(&self.store, class)
    }

    #[inline]
    pub(crate) fn class_has_name(&self, class: ClassId, name: NameId) -> bool {
        self.store.class_has_name(class, name)
    }

    fn cast_target(&mut self, package: PackageId, class: i32) -> NameId {
        if let Some(&id) = self.script.lookups.cast_targets.get(&(package, class)) {
            return id;
        }
        let loader = self.loader.clone();
        let id = self.intern(loader.package(package).package.object_name(class).unwrap_or(""));
        self.script.lookups.cast_targets.insert((package, class), id);
        id
    }

    pub(crate) fn package_name(&mut self, package: PackageId, index: usize) -> NameId {
        if let Some(&id) = self.script.lookups.names.get(&(package, index)) {
            return id;
        }
        let text = self.loader.package(package).package.name(index).to_owned();
        let id = self.intern(&text);
        self.script.lookups.names.insert((package, index), id);
        id
    }

    fn reference_property(&self, class: ClassId, package: PackageId, reference: i32) -> Option<usize> {
        self.property_index(class, self.loader.package(package).package.object_name(reference).unwrap_or(""))
    }

    fn stub_member(&mut self, object: ObjectId, package: PackageId, reference: i32) -> Value {
        let name = self.loader.package(package).package.object_name(reference).unwrap_or("");
        if name.eq_ignore_ascii_case("Name") {
            return Value::Name(self.object(object).name);
        }
        match ["USize", "UClamp", "VSize", "VClamp"].iter().position(|member| name.eq_ignore_ascii_case(member)) {
            Some(member) => Value::Int(self.texture_size(object)[member / 2]),
            None => Value::Int(0),
        }
    }

    fn native_function(&mut self, index: u16) -> Option<(PackageId, usize)> {
        if self.script.lookups.native_declarations.is_empty() {
            for id in self.loader.package_names().filter_map(|name| self.loader.loaded_id(name)) {
                let loaded = self.loader.package(id);
                if !loaded.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("u")) {
                    continue;
                }
                for export in 0..loaded.package.exports.len() {
                    if class_name(&loaded.package, &loaded.package.exports[export]) == "Function"
                        && let Ok(Some(ScriptObject::Function(f))) = read_export(&loaded.package, &loaded.data, export)
                        && f.native != 0
                    {
                        self.script.lookups.native_declarations.insert(f.native, (id, export));
                    }
                }
            }
        }
        self.script.lookups.native_declarations.get(&index).copied()
    }
}

pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Byte(v) => *v != 0,
        Value::Int(v) => *v != 0,
        Value::Float(v) => *v != 0.0,
        Value::Object(o) => o.is_some(),
        _ => false,
    }
}

pub(crate) fn int(value: &Value) -> i32 {
    match value {
        Value::Byte(v) => i32::from(*v),
        Value::Int(v) => *v,
        Value::Bool(v) => i32::from(*v),
        Value::Float(v) => *v as i32,
        _ => 0,
    }
}

pub(crate) fn float(value: &Value) -> f32 {
    match value {
        Value::Float(v) => *v,
        other => int(other) as f32,
    }
}

fn is_place(expr: &Expr) -> bool {
    match expr {
        Expr::Linked { expr, .. } => is_place(expr),
        Expr::LocalVariable(_) | Expr::InstanceVariable(_) | Expr::DefaultVariable(_) => true,
        Expr::ArrayElement { array, .. } | Expr::DynArrayElement { array, .. } => is_place(array),
        Expr::StructMember { value, .. } | Expr::BoolVariable(value) => is_place(value),
        Expr::Context { member, .. } | Expr::ClassContext { member, .. } => is_place(member),
        _ => false,
    }
}

fn iterator_args(expr: &Expr) -> &[Expr] {
    let Expr::Iterator { iterator, .. } = expr else { return &[] };
    let call = match &**iterator {
        Expr::Context { member, .. } => &**member,
        call => call,
    };
    match unlink(call).0 {
        Expr::Call { args, .. } => args.as_slice(),
        _ => &[],
    }
}

fn unlink(expr: &Expr) -> (&Expr, Option<u32>) {
    match expr {
        Expr::Linked { link, expr } => (expr, Some(*link)),
        _ => (expr, None),
    }
}

/// Superclasses of C++-only engine classes; no script class holds them (unverified).
const INTRINSIC_SUPER: [(&str, &str); 6] = [
    ("SkeletalMesh", "LodMesh"),
    ("VertMesh", "LodMesh"),
    ("LodMesh", "Mesh"),
    ("Mesh", "Primitive"),
    ("StaticMesh", "Primitive"),
    ("Model", "Primitive"),
];

fn is_zero(value: &Value) -> bool {
    match value {
        Value::Byte(v) => *v == 0,
        Value::Int(v) => *v == 0,
        Value::Bool(v) => !*v,
        Value::Float(v) => *v == 0.0,
        Value::Object(object) => object.is_none(),
        Value::Name(name) => *name == NameId::NONE,
        Value::Str(text) => text.is_empty(),
        Value::Array(items) | Value::Struct(items) => items.iter().all(is_zero),
        Value::Vector(v) => *v == Vector::ZERO,
        Value::Rotator(r) => r.pitch == 0 && r.yaw == 0 && r.roll == 0,
        Value::Delegate { object, function } => object.is_none() && *function == NameId::NONE,
        Value::Map => false,
    }
}

/// The typeless zero `Value::Int(0)` from an "Accessed None" equals the zero of the other side's type.
fn same_value(left: &Value, right: &Value) -> bool {
    let typeless = Value::Int(0);
    left == right || (*left == typeless && is_zero(right)) || (*right == typeless && is_zero(left))
}

fn case_matches(switch: &Value, case: &Value) -> bool {
    match (switch, case) {
        (Value::Str(a), Value::Str(b)) => a.eq_ignore_ascii_case(b),
        (&Value::Byte(byte), Value::Byte(_) | Value::Int(_)) => int(case) as u8 == byte,
        (Value::Int(_), Value::Byte(_) | Value::Int(_)) => int(switch) == int(case),
        _ => same_value(switch, case),
    }
}

pub(crate) fn intrinsic_subclasses(class: &str) -> Vec<&'static str> {
    let below = |sub: &str| {
        let mut current = sub;
        for _ in 0..8 {
            let Some(&(_, parent)) = INTRINSIC_SUPER.iter().find(|(c, _)| *c == current) else { return false };
            if parent.eq_ignore_ascii_case(class) {
                return true;
            }
            current = parent;
        }
        false
    };
    INTRINSIC_SUPER.iter().map(|&(sub, _)| sub).filter(|sub| below(sub)).collect()
}

#[derive(Debug, Clone, Copy)]
pub(super) enum PrimCast {
    ToInt,
    ToByte,
    ToFloat,
    ToBool,
    ToStr,
    ToStrOr(&'static str),
    VectorToStr,
    RotatorToVector,
    VectorToRotator,
    StringToVector,
    StringToRotator,
    Unknown(u8),
}

const PRIM_CASTS: [PrimCast; 33] = [
    PrimCast::RotatorToVector,
    PrimCast::ToInt,
    PrimCast::ToBool,
    PrimCast::ToFloat,
    PrimCast::ToByte,
    PrimCast::ToBool,
    PrimCast::ToFloat,
    PrimCast::ToByte,
    PrimCast::ToInt,
    PrimCast::ToFloat,
    PrimCast::ToByte,
    PrimCast::ToInt,
    PrimCast::ToBool,
    PrimCast::Unknown(0x46),
    PrimCast::ToBool,
    PrimCast::ToBool,
    PrimCast::ToByte,
    PrimCast::ToInt,
    PrimCast::ToBool,
    PrimCast::ToFloat,
    PrimCast::StringToVector,
    PrimCast::StringToRotator,
    PrimCast::ToBool,
    PrimCast::VectorToRotator,
    PrimCast::ToBool,
    PrimCast::ToStr,
    PrimCast::ToStr,
    PrimCast::ToStrOr("False"),
    PrimCast::ToStrOr("0.00"),
    PrimCast::ToStrOr("None"),
    PrimCast::ToStrOr("None"),
    PrimCast::VectorToStr,
    PrimCast::ToStrOr("0,0,0"),
];

impl PrimCast {
    pub(super) fn of(kind: u8) -> Self {
        kind.checked_sub(0x39).and_then(|index| PRIM_CASTS.get(usize::from(index))).copied().unwrap_or(Self::Unknown(kind))
    }
}

fn cast<L: LoaderRef>(vm: &Vm<L>, to: PrimCast, value: Value) -> Result<Value, Error> {
    let to = match to {
        PrimCast::ToStrOr(zero) if value == Value::Int(0) => return Ok(Value::Str(zero.to_owned())),
        PrimCast::VectorToStr if value == Value::Int(0) => return Ok(Value::Str("0.00,0.00,0.00".to_owned())),
        PrimCast::ToStrOr(_) => PrimCast::ToStr,
        to => to,
    };
    Ok(match (to, &value) {
        (PrimCast::RotatorToVector, _) => Value::Vector(natives::rot3(Some(&value)).vector()),
        (PrimCast::VectorToRotator, _) => Value::Rotator(natives::vec3(Some(&value)).rotation()),
        (PrimCast::ToBool, Value::Struct(_) | Value::Vector(_) | Value::Rotator(_)) => Value::Bool(natives::vec3(Some(&value)) != [0.0; 3].into()),
        (PrimCast::VectorToStr, Value::Struct(_) | Value::Vector(_) | Value::Rotator(_)) => {
            let Vector { x, y, z } = natives::vec3(Some(&value));
            Value::Str(format!("{x:.2},{y:.2},{z:.2}"))
        }
        (PrimCast::ToStr, Value::Struct(_) | Value::Vector(_) | Value::Rotator(_)) => {
            let Rotator { pitch: p, yaw: y, roll: r } = natives::rot3(Some(&value));
            Value::Str(format!("{p},{y},{r}"))
        }
        (PrimCast::StringToVector | PrimCast::StringToRotator, Value::Str(s)) => {
            let parts: Vec<f32> = s.split(',').map(|p| p.trim().parse().unwrap_or(0.0)).collect();
            let c = [0, 1, 2].map(|i| parts.get(i).copied().unwrap_or(0.0));
            if matches!(to, PrimCast::StringToVector) { Value::Vector(c.into()) } else { Value::Rotator(c.map(|c| c as i32).into()) }
        }
        (PrimCast::ToInt, Value::Str(s)) => Value::Int(s.trim().parse::<f64>().map_or(0, |v| v as i32)),
        (PrimCast::ToByte, Value::Str(s)) => Value::Byte(s.trim().parse::<f64>().map_or(0, |v| v as i32) as u8),
        (PrimCast::ToFloat, Value::Str(s)) => Value::Float(s.trim().parse().unwrap_or(0.0)),
        (PrimCast::ToInt, _) => Value::Int(int(&value)),
        (PrimCast::ToByte, _) => Value::Byte(int(&value) as u8),
        (PrimCast::ToFloat, _) => Value::Float(float(&value)),
        (PrimCast::ToBool, Value::Str(s)) => Value::Bool(s.eq_ignore_ascii_case("true") || s.parse::<i32>().is_ok_and(|v| v != 0)),
        (PrimCast::ToBool, Value::Name(n)) => Value::Bool(*n != NameId::NONE),
        (PrimCast::ToBool, _) => Value::Bool(truthy(&value)),
        (PrimCast::ToStr | PrimCast::VectorToStr, Value::Float(v)) => Value::Str(format!("{v:.2}")),
        (PrimCast::ToStr | PrimCast::VectorToStr, Value::Bool(v)) => Value::Str(if *v { "True" } else { "False" }.to_owned()),
        (PrimCast::ToStr | PrimCast::VectorToStr, Value::Name(n)) => Value::Str(vm.name(*n).to_owned()),
        (PrimCast::ToStr | PrimCast::VectorToStr, Value::Object(o)) => Value::Str(o.map_or("None".to_owned(), |o| vm.object(o).path.clone())),
        (PrimCast::ToStr | PrimCast::ToStrOr(_) | PrimCast::VectorToStr, _) => Value::Str(int(&value).to_string()),
        (PrimCast::StringToVector | PrimCast::StringToRotator, _) => return Err(Error::Unsupported(format!("cast {to:?}"))),
        (PrimCast::Unknown(kind), _) => return Err(Error::Unsupported(format!("cast {kind:#x}"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typeless_zero_equals_any_zero() {
        let zero = Value::Int(0);
        assert!(same_value(&zero, &Value::Name(NameId::NONE)));
        assert!(same_value(&Value::Struct(vec![Value::Float(0.0), Value::Object(None)]), &zero));
        assert!(!same_value(&zero, &Value::Float(1.0)));
        assert!(case_matches(&zero, &Value::Name(NameId::NONE)));
    }

    #[test]
    fn prim_casts_follow_the_cast_names() {
        for kind in 0..=u8::MAX {
            let cast = PrimCast::of(kind);
            let Some(name) = p2_script::cast_name(kind) else {
                assert!(matches!(cast, PrimCast::Unknown(unknown) if unknown == kind), "{kind:#x}");
                continue;
            };
            let to = match cast {
                PrimCast::ToInt => "Int",
                PrimCast::ToByte => "Byte",
                PrimCast::ToFloat => "Float",
                PrimCast::ToBool => "Bool",
                PrimCast::ToStr | PrimCast::ToStrOr(_) | PrimCast::VectorToStr => "String",
                PrimCast::RotatorToVector | PrimCast::StringToVector => "Vector",
                PrimCast::VectorToRotator | PrimCast::StringToRotator => "Rotator",
                PrimCast::Unknown(_) => "",
            };
            assert_eq!(name.split_once("To").map(|(_, to)| to), Some(to), "{name}");
            assert_eq!(matches!(cast, PrimCast::VectorToStr), name == "VectorToString", "{name}");
            assert_eq!(matches!(cast, PrimCast::RotatorToVector), name == "RotatorToVector", "{name}");
            assert_eq!(matches!(cast, PrimCast::VectorToRotator), name == "VectorToRotator", "{name}");
        }
    }

    #[test]
    fn call_site_keys_tell_classes_and_states_apart() {
        let (pawn, head) = (ClassId(7), ClassId(8));
        let keys = [site_key(pawn, 0), site_key(pawn, 1), site_key(head, 0), site_key(head, 1)];
        for (n, key) in keys.iter().enumerate() {
            let key = key.unwrap();
            assert_eq!(key & SITE_FUNCTION_MASK, 0);
            assert_ne!(key | (SITE_FUNCTION_MASK - 1), UNLINKED);
            assert!(keys[n + 1..].iter().all(|other| other.unwrap() != key));
        }
        assert_eq!(site_key(ClassId(u32::MAX), 0), None);
        assert_eq!(site_key(pawn, u32::MAX), None);
    }

    #[test]
    fn class_redirect_needs_its_target() {
        let loader = p2_package::Loader::new(&std::env::temp_dir(), &[]).unwrap();
        let mut vm = Vm::new(&loader);
        assert!(!vm.redirect_class(("Shell", "MenuOptions"), ("P2Modern", "ModernMenuOptions")));
        assert_eq!(vm.redirected(ClassId(3)), ClassId(3));
        vm.store.class_redirects.push((ClassId(3), ClassId(9)));
        assert_eq!(vm.redirected(ClassId(3)), ClassId(9));
        assert_eq!(vm.redirected(ClassId(4)), ClassId(4));
    }

    #[test]
    fn cast_link_holds_its_class_only() {
        let (pawn, head) = (ClassId(7), ClassId(8));
        for keeps in [false, true] {
            let value = cast_link(pawn, keeps);
            assert_ne!(value, UNLINKED);
            assert_eq!(cast_of_link(value, pawn), Some(keeps));
            assert_eq!(cast_of_link(value, head), None);
        }
        assert_eq!(cast_of_link(UNLINKED, ClassId(u32::MAX)), None);
    }

    #[test]
    fn member_link_holds_its_class_only() {
        let (pawn, head) = (ClassId(7), ClassId(8));
        let value = member_link(pawn, Some(42));
        assert_ne!(value, UNLINKED);
        assert_eq!(member_of_link(value, pawn), Some(Some(42)));
        assert_eq!(member_of_link(value, head), None);
        assert_eq!(member_of_link(member_link(head, None), head), Some(None));
        assert_eq!(member_of_link(member_link(head, Some(0)), head), Some(Some(0)));
    }
}

