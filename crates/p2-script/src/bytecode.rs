//! Script bytecode decoded into expression trees. The script size counts bytes in memory (references
//! and names take 4 each; compact on disk), and jump and case offsets are in-memory offsets.

use std::marker::PhantomData;

use p2_package::{Package, Reader};

use crate::Error;

mod tokens;

const END_FUNCTION_PARMS: u8 = 0x16;
const DEBUG_INFO: u8 = 0x42;
/// Debug info version that the engine treats as part of the preceding call.
const DEBUG_INFO_VERSION: i32 = 100;
/// Deepest expression nesting decoded. A nesting token takes one byte, so a damaged script could otherwise recurse once
/// per byte and overflow the stack. Well above source code, which adds a level per term of a `$` or `||` chain (tens of
/// levels; the deepest retail expression is unverified), and within the game's 64 MB stacks even in a debug build.
const MAX_EXPR_DEPTH: usize = 512;

/// `EX_PrimitiveCast` kinds from 0x39, in retail's table order (0x46 is unused).
const CASTS: [&str; 33] = [
    "RotatorToVector",
    "ByteToInt",
    "ByteToBool",
    "ByteToFloat",
    "IntToByte",
    "IntToBool",
    "IntToFloat",
    "BoolToByte",
    "BoolToInt",
    "BoolToFloat",
    "FloatToByte",
    "FloatToInt",
    "FloatToBool",
    "",
    "ObjectToBool",
    "NameToBool",
    "StringToByte",
    "StringToInt",
    "StringToBool",
    "StringToFloat",
    "StringToVector",
    "StringToRotator",
    "VectorToBool",
    "VectorToRotator",
    "RotatorToBool",
    "ByteToString",
    "IntToString",
    "BoolToString",
    "FloatToString",
    "ObjectToString",
    "NameToString",
    "VectorToString",
    "RotatorToString",
];

pub fn cast_name(kind: u8) -> Option<&'static str> {
    CASTS.get(usize::from(kind.checked_sub(0x39)?)).copied().filter(|name| !name.is_empty())
}

/// One top-level expression and its in-memory offset (what jumps and cases refer to).
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub offset: u32,
    pub expr: Expr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callee {
    Virtual(usize),
    Final(i32),
    Global(usize),
    Delegate { property: i32, name: usize },
    Native(u16),
}

/// A bytecode expression. Object references and names index the script's package tables.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    LocalVariable(i32),
    InstanceVariable(i32),
    DefaultVariable(i32),
    Return(Box<Self>),
    Switch { size: u8, value: Box<Self> },
    Jump(u16),
    JumpIfNot { target: u16, condition: Box<Self> },
    Stop,
    Assert { line: u16, condition: Box<Self> },
    Case { next: u16, value: Option<Box<Self>> },
    Nothing,
    LabelTable(Vec<(usize, i32)>),
    GotoLabel(Box<Self>),
    EatString(Box<Self>),
    Let { target: Box<Self>, value: Box<Self> },
    DynArrayElement { index: Box<Self>, array: Box<Self> },
    New { outer: Box<Self>, name: Box<Self>, flags: Box<Self>, class: Box<Self> },
    /// `skip`: bytes to skip when the object is `None`; `size`: result size.
    ClassContext { object: Box<Self>, skip: u16, size: u8, member: Box<Self> },
    MetaCast { class: i32, value: Box<Self> },
    LetBool { target: Box<Self>, value: Box<Self> },
    EndFunctionParms,
    SelfObject,
    Skip { size: u16, value: Box<Self> },
    Context { object: Box<Self>, skip: u16, size: u8, member: Box<Self> },
    ArrayElement { index: Box<Self>, array: Box<Self> },
    /// Parameters without the closing `EX_EndFunctionParms`; debug info attached to the call is dropped.
    Call { callee: Callee, args: Vec<Self> },
    IntConst(i32),
    FloatConst(f32),
    StringConst(String),
    ObjectConst(i32),
    NameConst(usize),
    RotationConst([i32; 3]),
    VectorConst([f32; 3]),
    ByteConst(u8),
    IntZero,
    IntOne,
    True,
    False,
    NativeParm(i32),
    NoObject,
    IntConstByte(u8),
    BoolVariable(Box<Self>),
    DynamicCast { class: i32, value: Box<Self> },
    Iterator { iterator: Box<Self>, end: u16 },
    IteratorPop,
    IteratorNext,
    StructCmpEq { structure: i32, left: Box<Self>, right: Box<Self> },
    StructCmpNe { structure: i32, left: Box<Self>, right: Box<Self> },
    UnicodeStringConst(String),
    StructMember { member: i32, value: Box<Self> },
    DynArrayLength(Box<Self>),
    PrimitiveCast { kind: u8, value: Box<Self> },
    DynArrayInsert { array: Box<Self>, index: Box<Self>, count: Box<Self> },
    DynArrayRemove { array: Box<Self>, index: Box<Self>, count: Box<Self> },
    DebugInfo { version: i32, line: i32, position: i32, opcode: String },
    DelegateProperty(usize),
    LetDelegate { target: Box<Self>, value: Box<Self> },
    /// A reference an interpreter resolves once: `expr` as decoded (a local, instance or default
    /// variable, struct member, name constant, call other than a delegate call, or dynamic / meta cast), `link` its
    /// slot in the function's link table. Only [`decode_script_linked`] produces it.
    Linked { link: u32, expr: Box<Self> },
}

pub fn decode_script(pkg: &Package, script: &[u8], size: u32) -> Result<Vec<Statement>, Error> {
    decode_tree(pkg, script, size, None).map(|(statements, _)| statements)
}

pub fn decode_script_linked(pkg: &Package, script: &[u8], size: u32) -> Result<(Vec<Statement>, u32), Error> {
    decode_tree(pkg, script, size, Some(0))
}

pub(crate) fn skip_script(pkg: &Package, r: &mut Reader, size: u32) -> Result<(), Error> {
    decode::<Walk>(pkg, r, size, None, |_, ()| {}).map(drop)
}

fn decode_tree(pkg: &Package, script: &[u8], size: u32, links: Option<u32>) -> Result<(Vec<Statement>, u32), Error> {
    let mut statements = Vec::new();
    let links = decode::<Tree>(pkg, &mut Reader::new(script), size, links, |offset, expr| statements.push(Statement { offset, expr }))?;
    Ok((statements, links))
}

fn decode<B: Build>(pkg: &Package, r: &mut Reader, size: u32, links: Option<u32>, mut statement: impl FnMut(u32, B::Node)) -> Result<u32, Error> {
    let start = r.pos();
    let mut decoder = Decoder { pkg, r, code: 0, size, links, depth: 0, build: PhantomData::<B> };
    while decoder.code < size {
        let offset = decoder.code;
        statement(offset, decoder.child()?);
    }
    if decoder.code != size {
        return Err(Error::ScriptSize { walked: decoder.code, declared: size, offset: start });
    }
    Ok(decoder.links.unwrap_or(0))
}

/// What decoding makes of an expression: the tree, or nothing for a walk that only measures the script.
trait Build {
    type Node;
    fn node(make: impl FnOnce() -> Expr) -> Self::Node;
    fn expr(node: Self::Node) -> Expr;

    fn boxed(node: Self::Node) -> Box<Expr> {
        Box::new(Self::expr(node))
    }
}

struct Tree;
struct Walk;

impl Build for Tree {
    type Node = Expr;

    fn node(make: impl FnOnce() -> Expr) -> Expr {
        make()
    }

    fn expr(node: Expr) -> Expr {
        node
    }
}

impl Build for Walk {
    type Node = ();

    fn node(_: impl FnOnce() -> Expr) {}

    /// Never called: a walk makes no expressions.
    fn expr((): ()) -> Expr {
        Expr::Nothing
    }
}

struct Decoder<'p, 'r, 'a, B> {
    pkg: &'p Package,
    r: &'r mut Reader<'a>,
    code: u32,
    size: u32,
    links: Option<u32>,
    depth: usize,
    build: PhantomData<B>,
}

impl<'a, B: Build> Decoder<'_, '_, 'a, B> {
    fn link(&mut self, make: impl FnOnce() -> Expr) -> B::Node {
        match &mut self.links {
            Some(next) => {
                let link = *next;
                *next += 1;
                B::node(|| Expr::Linked { link, expr: Box::new(make()) })
            }
            None => B::node(make),
        }
    }

    fn byte(&mut self) -> Result<u8, Error> {
        self.code += 1;
        Ok(self.r.u8()?)
    }

    fn word(&mut self) -> Result<u16, Error> {
        self.code += 2;
        Ok(self.r.u16()?)
    }

    fn int(&mut self) -> Result<i32, Error> {
        self.code += 4;
        Ok(self.r.i32()?)
    }

    fn float(&mut self) -> Result<f32, Error> {
        self.code += 4;
        Ok(self.r.f32()?)
    }

    /// Object reference: compact index on disk, pointer in memory.
    fn object(&mut self) -> Result<i32, Error> {
        self.code += 4;
        Ok(self.r.compact()?)
    }

    /// Name: compact index on disk, 4 bytes in memory. Returns the name-table index.
    fn name(&mut self) -> Result<usize, Error> {
        self.code += 4;
        Ok(self.pkg.read_name(self.r)?)
    }

    fn child(&mut self) -> Result<B::Node, Error> {
        Ok(self.expr()?.1)
    }

    fn read_since(&mut self, start: usize) -> Result<&'a [u8], Error> {
        let end = self.r.pos();
        self.r.seek(start)?;
        Ok(self.r.bytes(end - start)?)
    }

    /// Latin-1 text up to a 0 byte, without it.
    fn c_string(&mut self) -> Result<&'a [u8], Error> {
        let start = self.r.pos();
        while self.byte()? != 0 {}
        let text = self.read_since(start)?;
        Ok(text.strip_suffix(&[0]).unwrap_or(text))
    }

    fn call(&mut self, callee: Callee) -> Result<B::Node, Error> {
        let is_delegate = matches!(callee, Callee::Delegate { .. });
        let mut args = Vec::new();
        loop {
            match self.expr()? {
                (END_FUNCTION_PARMS, _) => break,
                (_, arg) => args.push(arg),
            }
        }
        if !is_delegate && self.code < self.size {
            // Peek: a debug info token of version 100 belongs to this call.
            let pos = self.r.pos();
            let version = if self.r.u8()? == DEBUG_INFO { self.r.i32()? } else { -1 };
            self.r.seek(pos)?;
            if version == DEBUG_INFO_VERSION {
                self.expr()?;
            }
        }
        let call = move || Expr::Call { callee, args: args.into_iter().map(B::expr).collect() };
        Ok(if is_delegate { B::node(call) } else { self.link(call) })
    }

    /// The expression's first token, and what was made of it.
    fn expr(&mut self) -> Result<(u8, B::Node), Error> {
        if self.depth >= MAX_EXPR_DEPTH {
            return Err(Error::TooDeep { limit: MAX_EXPR_DEPTH, offset: self.r.pos() });
        }
        self.depth += 1;
        let expr = self.token_expr();
        self.depth -= 1;
        expr
    }
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_package() -> Package {
        let mut data = Vec::new();
        data.extend(p2_package::PACKAGE_TAG.to_le_bytes());
        data.extend(118_u16.to_le_bytes());
        data.extend(2_u16.to_le_bytes());
        for value in [0, 1, 56, 0, 66, 0, 66_u32] {
            data.extend(value.to_le_bytes());
        }
        data.extend([0; 16]);
        data.extend(0_u32.to_le_bytes());
        data.extend(b"\x05None\0");
        data.extend(0_u32.to_le_bytes());
        Package::parse(&data).unwrap()
    }

    const SCRIPT: &[u8] = &[
        0x0f, 0x00, 0x40, 0x01, 0x1d, 1, 0, 0, 0, // Let: object reference as 2-byte compact index
        0x04, 0x0b,
        0x70, 0x25, 0x16,
        0x0c, 0x00, 0, 0, 0, 0,
    ];

    #[test]
    fn script_counts_memory_size() {
        let pkg = tiny_package();
        let mut r = Reader::new(SCRIPT);
        skip_script(&pkg, &mut r, 25).unwrap();
        assert!(r.at_end());

        let mut r = Reader::new(SCRIPT);
        assert!(matches!(skip_script(&pkg, &mut r, 24), Err(Error::ScriptSize { walked: 25, .. })));
    }

    #[test]
    fn decodes_statements() {
        let pkg = tiny_package();
        let statements = decode_script(&pkg, SCRIPT, 25).unwrap();
        let offsets: Vec<u32> = statements.iter().map(|s| s.offset).collect();
        assert_eq!(offsets, [0, 11, 13, 16]);
        assert_eq!(
            statements[0].expr,
            Expr::Let { target: Box::new(Expr::LocalVariable(64)), value: Box::new(Expr::IntConst(1)) }
        );
        assert_eq!(statements[1].expr, Expr::Return(Box::new(Expr::Nothing)));
        assert_eq!(statements[2].expr, Expr::Call { callee: Callee::Native(0x70), args: vec![Expr::IntZero] });
        assert_eq!(statements[3].expr, Expr::LabelTable(Vec::new()));
    }

    #[test]
    fn bool_variable_wraps_its_variable() {
        let pkg = tiny_package();
        let script = [0x14, 0x2d, 0x01, 0x01, 0x27];
        let statements = decode_script(&pkg, &script, 8).unwrap();
        assert_eq!(
            statements,
            [Statement {
                offset: 0,
                expr: Expr::LetBool {
                    target: Box::new(Expr::BoolVariable(Box::new(Expr::InstanceVariable(1)))),
                    value: Box::new(Expr::True),
                },
            }]
        );
    }

    #[test]
    fn linked_decode_numbers_references() {
        let pkg = tiny_package();
        let (statements, links) = decode_script_linked(&pkg, SCRIPT, 25).unwrap();
        assert_eq!(links, 2);
        let local = Box::new(Expr::Linked { link: 0, expr: Box::new(Expr::LocalVariable(64)) });
        assert_eq!(statements[0].expr, Expr::Let { target: local, value: Box::new(Expr::IntConst(1)) });
        let call = Expr::Call { callee: Callee::Native(0x70), args: vec![Expr::IntZero] };
        assert_eq!(statements[2].expr, Expr::Linked { link: 1, expr: Box::new(call) });
        let offsets: Vec<u32> = statements.iter().map(|s| s.offset).collect();
        assert_eq!(offsets, [0, 11, 13, 16]);

        let (statements, links) = decode_script_linked(&pkg, &[0x14, 0x2d, 0x01, 0x01, 0x27], 8).unwrap();
        assert_eq!(links, 1);
        let Expr::LetBool { target, .. } = &statements[0].expr else { panic!("not a LetBool") };
        let Expr::BoolVariable(inner) = &**target else { panic!("not a BoolVariable") };
        assert_eq!(**inner, Expr::Linked { link: 0, expr: Box::new(Expr::InstanceVariable(1)) });
    }

    #[test]
    fn linked_decode_numbers_call_sites_and_casts() {
        let pkg = tiny_package();
        let linked = |script: &[u8], size| decode_script_linked(&pkg, script, size).unwrap();
        let (statements, links) = linked(&[0x1b, 0x00, 0x16], 6);
        assert_eq!(links, 1);
        let call = Expr::Call { callee: Callee::Virtual(0), args: Vec::new() };
        assert_eq!(statements[0].expr, Expr::Linked { link: 0, expr: Box::new(call) });
        let (statements, links) = linked(&[0x2e, 0x00, 0x2a], 6);
        assert_eq!(links, 1);
        let cast = Expr::DynamicCast { class: 0, value: Box::new(Expr::NoObject) };
        assert_eq!(statements[0].expr, Expr::Linked { link: 0, expr: Box::new(cast) });
        let (statements, links) = linked(&[0x43, 0x00, 0x00, 0x16], 10);
        assert_eq!(links, 0);
        assert_eq!(statements[0].expr, Expr::Call { callee: Callee::Delegate { property: 0, name: 0 }, args: Vec::new() });
        let plain = decode_script(&pkg, &[0x1b, 0x00, 0x16], 6).unwrap();
        assert_eq!(plain[0].expr, Expr::Call { callee: Callee::Virtual(0), args: Vec::new() });
    }

    #[test]
    fn nesting_is_limited() {
        let decode = || {
            let pkg = tiny_package();
            let nested = |levels: usize| [vec![0x04_u8; levels], vec![0x0b_u8]].concat();
            let fits = nested(MAX_EXPR_DEPTH - 1);
            assert!(decode_script(&pkg, &fits, fits.len() as u32).is_ok());
            let too_deep = nested(MAX_EXPR_DEPTH + 10);
            let result = decode_script(&pkg, &too_deep, too_deep.len() as u32);
            assert!(matches!(result, Err(Error::TooDeep { offset: MAX_EXPR_DEPTH, .. })));
        };
        std::thread::Builder::new().stack_size(64 << 20).spawn(decode).unwrap().join().unwrap();
    }

    #[test]
    fn unknown_token() {
        let pkg = tiny_package();
        let mut r = Reader::new(&[0x03]);
        assert!(matches!(skip_script(&pkg, &mut r, 1), Err(Error::BadToken { token: 3, offset: 0 })));
    }
}
