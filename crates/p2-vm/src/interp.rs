use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use p2_package::PackageId;
use p2_script::flags::FUNC_NATIVE;
use p2_script::{Expr, Statement, StructLayout};

use p2_math::{Rotator, Vector};
use rustc_hash::FxHashMap;

use crate::memory::{Heap, LayoutId, Mem, MemLayouts, Shape};
use crate::{ClassId, Error, NameId, ObjectId, LoaderRef, Value, Vm, natives};

pub(crate) mod compile;
mod call;
mod eval;
mod exec;
mod lookup;
mod place;
mod run;

const MAX_SCRIPT_DEPTH: u32 = 1000;
const RECURSION_CHAIN: usize = 24;
const RUNAWAY: usize = 1_000_000;

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
    pub(crate) hook: Option<crate::script_hooks::ScriptHook>,
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
    pub(crate) locals: Mem,
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
        (PrimCast::ToStr | PrimCast::VectorToStr, Value::Name(n)) => Value::Str(vm.store.name(*n).to_owned()),
        (PrimCast::ToStr | PrimCast::VectorToStr, Value::Object(o)) => Value::Str(o.map_or("None".to_owned(), |o| vm.store.object(o).path.clone())),
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

