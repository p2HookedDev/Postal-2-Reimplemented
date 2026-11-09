use std::collections::HashMap;
use std::fmt::Write;

use p2_package::Package;

use crate::bytecode::{Callee, Expr, Statement, cast_name};
use crate::flags::{FUNC_OPERATOR, FUNC_PRE_OPERATOR};
use crate::{Function, ScriptObject, read_export};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeName {
    pub name: String,
    pub operator: bool,
    pub pre_operator: bool,
}

pub fn native_names(pkg: &Package, file: &[u8]) -> HashMap<u16, NativeName> {
    let mut natives = HashMap::new();
    for index in 0..pkg.exports.len() {
        let Ok(Some(ScriptObject::Function(Function { base, native, flags, .. }))) = read_export(pkg, file, index) else {
            continue;
        };
        if native != 0 {
            let name = NativeName {
                name: pkg.name(base.friendly_name).to_owned(),
                operator: flags & FUNC_OPERATOR != 0,
                pre_operator: flags & FUNC_PRE_OPERATOR != 0,
            };
            natives.insert(native, name);
        }
    }
    natives
}

pub fn disassemble(pkg: &Package, statements: &[Statement], natives: &HashMap<u16, NativeName>) -> String {
    let printer = Printer { pkg, natives };
    let mut text = String::new();
    for statement in statements {
        let _ = writeln!(text, "{:04x}: {}", statement.offset, printer.expr(&statement.expr));
    }
    text
}

struct Printer<'a> {
    pkg: &'a Package,
    natives: &'a HashMap<u16, NativeName>,
}

impl Printer<'_> {
    fn object(&self, reference: i32) -> &str {
        match reference {
            0 => "None",
            _ => self.pkg.object_name(reference).unwrap_or("<invalid>"),
        }
    }

    fn name(&self, name: usize) -> &str {
        self.pkg.name(name)
    }

    fn args(&self, args: &[Expr]) -> String {
        args.iter().map(|arg| self.expr(arg)).collect::<Vec<_>>().join(", ")
    }

    fn call(&self, callee: Callee, args: &[Expr]) -> String {
        match callee {
            Callee::Native(index) => match self.natives.get(&index) {
                Some(native) if native.operator && native.pre_operator && args.len() == 1 => {
                    format!("{}{}", native.name, self.expr(&args[0]))
                }
                Some(native) if native.operator && args.len() == 1 => format!("{}{}", self.expr(&args[0]), native.name),
                Some(native) if native.operator && args.len() == 2 => {
                    format!("({} {} {})", self.expr(&args[0]), native.name, self.expr(&args[1]))
                }
                Some(native) => format!("{}({})", native.name, self.args(args)),
                None => format!("native{index}({})", self.args(args)),
            },
            Callee::Virtual(name) | Callee::Delegate { name, .. } => format!("{}({})", self.name(name), self.args(args)),
            Callee::Final(function) => format!("{}({})", self.object(function), self.args(args)),
            Callee::Global(name) => format!("Global.{}({})", self.name(name), self.args(args)),
        }
    }

    fn expr(&self, expr: &Expr) -> String {
        match expr {
            Expr::Linked { expr, .. } => self.expr(expr),
            Expr::LocalVariable(r) | Expr::InstanceVariable(r) | Expr::NativeParm(r) => self.object(*r).to_owned(),
            Expr::DefaultVariable(r) => format!("Default.{}", self.object(*r)),
            Expr::Return(value) => match **value {
                Expr::Nothing => "return".to_owned(),
                ref value => format!("return {}", self.expr(value)),
            },
            Expr::Switch { value, .. } => format!("switch ({})", self.expr(value)),
            Expr::Jump(target) => format!("goto {target:04x}"),
            Expr::JumpIfNot { target, condition } => format!("if (!{}) goto {target:04x}", self.expr(condition)),
            Expr::Stop => "stop".to_owned(),
            Expr::Assert { line, condition } => format!("assert({}) // line {line}", self.expr(condition)),
            Expr::Case { value: Some(value), next } => format!("case {}: // next {next:04x}", self.expr(value)),
            Expr::Case { value: None, .. } => "default:".to_owned(),
            Expr::Nothing => "nothing".to_owned(),
            Expr::LabelTable(labels) => {
                let labels: Vec<String> = labels.iter().map(|(name, code)| format!("{}={code:04x}", self.name(*name))).collect();
                format!("labels {}", labels.join(" "))
            }
            Expr::GotoLabel(label) => format!("goto {}", self.expr(label)),
            Expr::Let { target, value } | Expr::LetBool { target, value } | Expr::LetDelegate { target, value } => {
                format!("{} = {}", self.expr(target), self.expr(value))
            }
            Expr::DynArrayElement { index, array } | Expr::ArrayElement { index, array } => {
                format!("{}[{}]", self.expr(array), self.expr(index))
            }
            Expr::New { outer, name, flags, class } => {
                format!("new({}, {}, {}) {}", self.expr(outer), self.expr(name), self.expr(flags), self.expr(class))
            }
            Expr::ClassContext { object, member, .. } => format!("{}.static.{}", self.expr(object), self.expr(member)),
            Expr::Context { object, member, .. } => format!("{}.{}", self.expr(object), self.expr(member)),
            Expr::MetaCast { class, value } => format!("class<{}>({})", self.object(*class), self.expr(value)),
            Expr::DynamicCast { class, value } => format!("{}({})", self.object(*class), self.expr(value)),
            Expr::EndFunctionParms => ")".to_owned(),
            Expr::SelfObject => "self".to_owned(),
            Expr::Skip { value, .. } | Expr::BoolVariable(value) | Expr::EatString(value) => self.expr(value),
            Expr::Call { callee, args } => self.call(*callee, args),
            Expr::IntConst(v) => v.to_string(),
            Expr::FloatConst(v) => format!("{v:?}"),
            Expr::StringConst(v) | Expr::UnicodeStringConst(v) => format!("{v:?}"),
            Expr::ObjectConst(r) => format!("'{}'", self.object(*r)),
            Expr::NameConst(name) => format!("'{}'", self.name(*name)),
            Expr::RotationConst([pitch, yaw, roll]) => format!("rot({pitch}, {yaw}, {roll})"),
            Expr::VectorConst([x, y, z]) => format!("vect({x:?}, {y:?}, {z:?})"),
            Expr::ByteConst(v) | Expr::IntConstByte(v) => v.to_string(),
            Expr::IntZero => "0".to_owned(),
            Expr::IntOne => "1".to_owned(),
            Expr::True => "true".to_owned(),
            Expr::False => "false".to_owned(),
            Expr::NoObject => "None".to_owned(),
            Expr::Iterator { iterator, end } => format!("foreach {} // end {end:04x}", self.expr(iterator)),
            Expr::IteratorPop => "iterator pop".to_owned(),
            Expr::IteratorNext => "iterator next".to_owned(),
            Expr::StructCmpEq { left, right, .. } => format!("({} == {})", self.expr(left), self.expr(right)),
            Expr::StructCmpNe { left, right, .. } => format!("({} != {})", self.expr(left), self.expr(right)),
            Expr::StructMember { member, value } => format!("{}.{}", self.expr(value), self.object(*member)),
            Expr::DynArrayLength(array) => format!("{}.Length", self.expr(array)),
            Expr::PrimitiveCast { kind, value } => match cast_name(*kind).and_then(|name| name.split("To").last()) {
                Some(target) => format!("{}({})", target.to_ascii_lowercase(), self.expr(value)),
                None => format!("cast{kind:#04x}({})", self.expr(value)),
            },
            Expr::DynArrayInsert { array, index, count } => {
                format!("{}.Insert({}, {})", self.expr(array), self.expr(index), self.expr(count))
            }
            Expr::DynArrayRemove { array, index, count } => {
                format!("{}.Remove({}, {})", self.expr(array), self.expr(index), self.expr(count))
            }
            Expr::DebugInfo { line, opcode, .. } => format!("// debug line {line} {opcode}"),
            Expr::DelegateProperty(name) => self.name(*name).to_owned(),
        }
    }
}
