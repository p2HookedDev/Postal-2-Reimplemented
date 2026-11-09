//! Script bytecode decoded into expression trees. The script size counts bytes in memory (references
//! and names take 4 each; compact on disk), and jump and case offsets are in-memory offsets.

use std::marker::PhantomData;

use p2_package::{Package, Reader};

use crate::Error;

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

    #[expect(clippy::too_many_lines, reason = "one match over every expression kind; the link step grew it")]
    fn token_expr(&mut self) -> Result<(u8, B::Node), Error> {
        let offset = self.r.pos();
        let token = self.byte()?;
        let node = match token {
            0x04..=0x0e => self.flow_expr(token, offset)?,
            0x1d..=0x24 | 0x2c | 0x34 => self.const_expr(token, offset)?,
            0x70..=0xff => self.call(Callee::Native(u16::from(token)))?,
            0x60..=0x6f => {
                let index = u16::from(token - 0x60) << 8 | u16::from(self.byte()?);
                self.call(Callee::Native(index))?
            }
            0x00 => {
                let index = self.object()?;
                self.link(|| Expr::LocalVariable(index))
            }
            0x01 => {
                let index = self.object()?;
                self.link(|| Expr::InstanceVariable(index))
            }
            0x02 => {
                let index = self.object()?;
                self.link(|| Expr::DefaultVariable(index))
            }
            0x0f => {
                let (target, value) = (self.child()?, self.child()?);
                B::node(|| Expr::Let { target: B::boxed(target), value: B::boxed(value) })
            }
            0x10 => {
                let (index, array) = (self.child()?, self.child()?);
                B::node(|| Expr::DynArrayElement { index: B::boxed(index), array: B::boxed(array) })
            }
            0x11 => {
                let (outer, name, flags, class) = (self.child()?, self.child()?, self.child()?, self.child()?);
                B::node(|| Expr::New { outer: B::boxed(outer), name: B::boxed(name), flags: B::boxed(flags), class: B::boxed(class) })
            }
            0x12 => {
                let (object, skip, size, member) = (self.child()?, self.word()?, self.byte()?, self.child()?);
                B::node(|| Expr::ClassContext { object: B::boxed(object), skip, size, member: B::boxed(member) })
            }
            0x13 => {
                let (class, value) = (self.object()?, self.child()?);
                self.link(|| Expr::MetaCast { class, value: B::boxed(value) })
            }
            0x14 => {
                let (target, value) = (self.child()?, self.child()?);
                B::node(|| Expr::LetBool { target: B::boxed(target), value: B::boxed(value) })
            }
            END_FUNCTION_PARMS => B::node(|| Expr::EndFunctionParms),
            0x17 => B::node(|| Expr::SelfObject),
            0x18 => {
                let (size, value) = (self.word()?, self.child()?);
                B::node(|| Expr::Skip { size, value: B::boxed(value) })
            }
            0x19 => {
                let (object, skip, size, member) = (self.child()?, self.word()?, self.byte()?, self.child()?);
                B::node(|| Expr::Context { object: B::boxed(object), skip, size, member: B::boxed(member) })
            }
            0x1a => {
                let (index, array) = (self.child()?, self.child()?);
                B::node(|| Expr::ArrayElement { index: B::boxed(index), array: B::boxed(array) })
            }
            0x1b => {
                let name = self.name()?;
                self.call(Callee::Virtual(name))?
            }
            0x1c => {
                let function = self.object()?;
                self.call(Callee::Final(function))?
            }
            0x25 => B::node(|| Expr::IntZero),
            0x26 => B::node(|| Expr::IntOne),
            0x27 => B::node(|| Expr::True),
            0x28 => B::node(|| Expr::False),
            0x29 => {
                let property = self.object()?;
                B::node(|| Expr::NativeParm(property))
            }
            0x2a => B::node(|| Expr::NoObject),
            // A bool variable token runs the next expression (the variable).
            0x2d => {
                let variable = self.child()?;
                B::node(|| Expr::BoolVariable(B::boxed(variable)))
            }
            0x2e => {
                let (class, value) = (self.object()?, self.child()?);
                self.link(|| Expr::DynamicCast { class, value: B::boxed(value) })
            }
            0x2f => {
                let (iterator, end) = (self.child()?, self.word()?);
                B::node(|| Expr::Iterator { iterator: B::boxed(iterator), end })
            }
            0x30 => B::node(|| Expr::IteratorPop),
            0x31 => B::node(|| Expr::IteratorNext),
            0x32 => {
                let (structure, left, right) = (self.object()?, self.child()?, self.child()?);
                B::node(|| Expr::StructCmpEq { structure, left: B::boxed(left), right: B::boxed(right) })
            }
            0x33 => {
                let (structure, left, right) = (self.object()?, self.child()?, self.child()?);
                B::node(|| Expr::StructCmpNe { structure, left: B::boxed(left), right: B::boxed(right) })
            }
            0x36 => {
                let (member, value) = (self.object()?, self.child()?);
                self.link(|| Expr::StructMember { member, value: B::boxed(value) })
            }
            0x37 => {
                let array = self.child()?;
                B::node(|| Expr::DynArrayLength(B::boxed(array)))
            }
            0x38 => {
                let name = self.name()?;
                self.call(Callee::Global(name))?
            }
            0x39 => {
                let (kind, value) = (self.byte()?, self.child()?);
                B::node(|| Expr::PrimitiveCast { kind, value: B::boxed(value) })
            }
            0x40 => {
                let (array, index, count) = (self.child()?, self.child()?, self.child()?);
                B::node(|| Expr::DynArrayInsert { array: B::boxed(array), index: B::boxed(index), count: B::boxed(count) })
            }
            0x41 => {
                let (array, index, count) = (self.child()?, self.child()?, self.child()?);
                B::node(|| Expr::DynArrayRemove { array: B::boxed(array), index: B::boxed(index), count: B::boxed(count) })
            }
            DEBUG_INFO => {
                let (version, line, position, opcode) = (self.int()?, self.int()?, self.int()?, self.c_string()?);
                B::node(|| Expr::DebugInfo { version, line, position, opcode: latin1(opcode) })
            }
            // A delegate call: property, name, then call parameters. The serializer reads these linearly and
            // attaches no debug info.
            0x43 => {
                let property = self.object()?;
                let name = self.name()?;
                self.call(Callee::Delegate { property, name })?
            }
            0x44 => {
                let name = self.name()?;
                B::node(|| Expr::DelegateProperty(name))
            }
            0x45 => {
                let (target, value) = (self.child()?, self.child()?);
                B::node(|| Expr::LetDelegate { target: B::boxed(target), value: B::boxed(value) })
            }
            _ => return Err(Error::BadToken { token, offset }),
        };
        Ok((token, node))
    }

    fn flow_expr(&mut self, token: u8, offset: usize) -> Result<B::Node, Error> {
        Ok(match token {
            0x04 => {
                let value = self.child()?;
                B::node(|| Expr::Return(B::boxed(value)))
            }
            0x05 => {
                let (size, value) = (self.byte()?, self.child()?);
                B::node(|| Expr::Switch { size, value: B::boxed(value) })
            }
            0x06 => {
                let target = self.word()?;
                B::node(|| Expr::Jump(target))
            }
            0x07 => {
                let (target, condition) = (self.word()?, self.child()?);
                B::node(|| Expr::JumpIfNot { target, condition: B::boxed(condition) })
            }
            0x08 => B::node(|| Expr::Stop),
            0x09 => {
                let (line, condition) = (self.word()?, self.child()?);
                B::node(|| Expr::Assert { line, condition: B::boxed(condition) })
            }
            0x0a => {
                let next = self.word()?;
                let value = if next == 0xffff { None } else { Some(self.child()?) };
                B::node(|| Expr::Case { next, value: value.map(B::boxed) })
            }
            0x0b => B::node(|| Expr::Nothing),
            0x0c => {
                let mut labels = Vec::new();
                loop {
                    let name = self.name()?;
                    let code = self.int()?;
                    if self.pkg.name(name).eq_ignore_ascii_case("None") {
                        break B::node(|| Expr::LabelTable(labels));
                    }
                    labels.push((name, code));
                }
            }
            0x0d => {
                let value = self.child()?;
                B::node(|| Expr::GotoLabel(B::boxed(value)))
            }
            0x0e => {
                let value = self.child()?;
                B::node(|| Expr::EatString(B::boxed(value)))
            }
            _ => return Err(Error::BadToken { token, offset }),
        })
    }

    fn const_expr(&mut self, token: u8, offset: usize) -> Result<B::Node, Error> {
        Ok(match token {
            0x1d => {
                let value = self.int()?;
                B::node(|| Expr::IntConst(value))
            }
            0x1e => {
                let value = self.float()?;
                B::node(|| Expr::FloatConst(value))
            }
            0x1f => {
                let text = self.c_string()?;
                B::node(|| Expr::StringConst(latin1(text)))
            }
            0x20 => {
                let object = self.object()?;
                B::node(|| Expr::ObjectConst(object))
            }
            0x21 => {
                let name = self.name()?;
                self.link(|| Expr::NameConst(name))
            }
            0x22 => {
                let value = [self.int()?, self.int()?, self.int()?];
                B::node(|| Expr::RotationConst(value))
            }
            0x23 => {
                let value = [self.float()?, self.float()?, self.float()?];
                B::node(|| Expr::VectorConst(value))
            }
            0x24 => {
                let value = self.byte()?;
                B::node(|| Expr::ByteConst(value))
            }
            0x2c => {
                let value = self.byte()?;
                B::node(|| Expr::IntConstByte(value))
            }
            // Words until one whose high byte is 0 (the engine tests the last byte read); a final 0 is no character.
            0x34 => {
                let start = self.r.pos();
                while self.word()? >> 8 != 0 {}
                let bytes = self.read_since(start)?;
                B::node(|| {
                    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&unit| u16::from_le_bytes(unit)).collect();
                    let text = match units.split_last() {
                        Some((&0, text)) => text,
                        _ => units.as_slice(),
                    };
                    Expr::UnicodeStringConst(String::from_utf16_lossy(text))
                })
            }
            _ => return Err(Error::BadToken { token, offset }),
        })
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
