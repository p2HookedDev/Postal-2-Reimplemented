use std::sync::Arc;
use p2_script::{Callee, Expr};
use crate::Prop;
use crate::{Error, LoaderRef, Value, Vm, natives};
use super::{Flow, Frame, Iteration, ListWalk, StepResult, Target, compile, int, iterator_args, truthy, unlink};

impl<L: LoaderRef> Vm<L> {
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

    pub(super) fn exec(&mut self, frame: &mut Frame, expr: &Expr, pc: usize) -> Result<Flow, Error> {
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
                values.push(Value::Vector(self.store.vector(target, Prop::Location)));
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
            if !matches!(item, Value::Object(Some(object)) if self.store.is_deleted(object)) {
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
}
