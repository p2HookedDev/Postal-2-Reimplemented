//! `UnrealScript` definitions in `.u` packages (version 118, licensee 2). Object references (`i32`)
//! and names (`usize`) are relative to the package the definition was read from.

pub mod bytecode;
pub mod disasm;
pub mod flags;
pub mod layout;
pub mod object;
pub mod value;

use p2_package::{Export, Package, Reader};

use flags::{CPF_COMMENT_STRING, CPF_NET, FUNC_NET};

pub use bytecode::{Callee, Expr, Statement, cast_name, decode_script, decode_script_linked};
pub use disasm::{NativeName, disassemble, native_names};
pub use layout::{FieldLayout, LayoutCache, Layouts, Resolve, Source, StructLayout, Type};
pub use object::{Layer, ObjectProperties, Objects};
pub use value::{Properties, PropertyValue, SkipReason, SkippedTag, StoredValue, read_properties};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Package(#[from] p2_package::Error),
    #[error("unknown bytecode token {token:#04x} at offset {offset:#x}")]
    BadToken { token: u8, offset: usize },
    #[error("script walked to {walked} of declared size {declared} (offset {offset:#x})")]
    ScriptSize { walked: u32, declared: u32, offset: usize },
    #[error("{left} bytes left at the end of export data (offset {offset:#x})")]
    Trailing { left: usize, offset: usize },
    #[error("object reference {index} does not resolve")]
    Unresolved { index: i32 },
    #[error("export {export} is not a {expected}")]
    WrongKind { export: usize, expected: &'static str },
    #[error("nesting deeper than {limit} at offset {offset:#x}")]
    TooDeep { limit: usize, offset: usize },
    #[error("unsupported property type {0} in a value")]
    Unsupported(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub super_field: i32,
    pub next: i32,
}

#[derive(Debug, Clone)]
pub struct Struct {
    pub field: Field,
    pub script_text: i32,
    pub children: i32,
    pub friendly_name: usize,
    pub line: i32,
    pub text_pos: i32,
    /// Bytecode size in memory (object references and names count 4 bytes, not their disk size).
    pub script_size: u32,
    /// Bytecode as stored on disk (object references and names as compact indices).
    pub script: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub base: Struct,
    pub native: u16,
    pub operator_precedence: u8,
    pub flags: u32,
    pub rep_offset: Option<u16>,
}

#[derive(Debug, Clone)]
pub struct State {
    pub base: Struct,
    pub probe_mask: u64,
    pub ignore_mask: u64,
    pub label_table_offset: u16,
    pub flags: u32,
}

#[derive(Debug, Clone)]
pub struct Class {
    pub base: State,
    pub flags: u32,
    pub guid: [u8; 16],
    pub dependencies: Vec<Dependency>,
    pub package_imports: Vec<usize>,
    pub within: i32,
    pub config_name: usize,
    pub hide_categories: Vec<usize>,
    pub defaults_offset: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct Dependency {
    pub class: i32,
    pub deep: i32,
    pub script_text_crc: u32,
}

#[derive(Debug, Clone)]
pub struct Property {
    pub field: Field,
    pub array_dim: i32,
    pub flags: u32,
    pub category: usize,
    pub rep_offset: Option<u16>,
    pub comment: Option<String>,
    pub kind: PropertyKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyKind {
    /// `enum_` is 0 for a plain byte.
    Byte { enum_: i32 },
    Int,
    Bool,
    Float,
    Object { class: i32 },
    Class { class: i32, meta_class: i32 },
    Name,
    Str,
    String,
    Array { inner: i32 },
    Struct { struct_: i32 },
    Map { key: i32, value: i32 },
    FixedArray { inner: i32, count: i32 },
    Delegate { function: i32 },
}

#[derive(Debug, Clone)]
pub struct Enum {
    pub field: Field,
    pub names: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct Const {
    pub field: Field,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct TextBuffer {
    pub pos: i32,
    pub top: i32,
    pub text: String,
}

#[derive(Debug, Clone)]
pub enum ScriptObject {
    Class(Class),
    Struct(Struct),
    State(State),
    Function(Function),
    Property(Property),
    Enum(Enum),
    Const(Const),
    TextBuffer(TextBuffer),
}

impl ScriptObject {
    pub fn as_struct(&self) -> Option<&Struct> {
        match self {
            Self::Class(c) => Some(&c.base.base),
            Self::State(s) => Some(&s.base),
            Self::Function(f) => Some(&f.base),
            Self::Struct(s) => Some(s),
            _ => None,
        }
    }

    pub fn field(&self) -> Option<Field> {
        match self {
            Self::Property(p) => Some(p.field),
            Self::Enum(e) => Some(e.field),
            Self::Const(c) => Some(c.field),
            Self::TextBuffer(_) => None,
            _ => self.as_struct().map(|s| s.field),
        }
    }
}

pub fn class_name<'p>(pkg: &'p Package, export: &Export) -> &'p str {
    match export.class_index {
        0 => "Class",
        index => pkg.object_name(index).unwrap_or(""),
    }
}

/// `Ok(None)` for any class but the `Core` definition classes, matched by name only (no other package defines classes
/// with these names). Everything but class default properties must end exactly at the export's end.
pub fn read_export(pkg: &Package, file: &[u8], index: usize) -> Result<Option<ScriptObject>, Error> {
    let Some(export) = pkg.exports.get(index) else {
        return Err(Error::WrongKind { export: index, expected: "export" });
    };
    let Some(definition) = Definition::from_class_name(class_name(pkg, export)) else {
        return Ok(None);
    };
    let mut r = pkg.native_reader(file, export)?;
    let object = match definition {
        Definition::Class => ScriptObject::Class(read_class(pkg, &mut r)?),
        Definition::Struct => ScriptObject::Struct(read_struct(pkg, &mut r)?),
        Definition::State => ScriptObject::State(read_state(pkg, &mut r)?),
        Definition::Function => ScriptObject::Function(read_function(pkg, &mut r)?),
        Definition::Enum => ScriptObject::Enum(Enum { field: read_field(&mut r)?, names: read_names(pkg, &mut r)? }),
        Definition::Const => ScriptObject::Const(Const { field: read_field(&mut r)?, value: r.fstring()? }),
        Definition::TextBuffer => ScriptObject::TextBuffer(TextBuffer { pos: r.i32()?, top: r.i32()?, text: r.fstring()? }),
        Definition::Property(class) => ScriptObject::Property(read_property(pkg, &mut r, class)?),
    };
    if !matches!(object, ScriptObject::Class(_)) && !r.at_end() {
        let offset = r.pos();
        return Err(Error::Trailing { left: r.rest().len(), offset });
    }
    Ok(Some(object))
}

/// `Field::next` of definition export `index` and, if it is a property, the property: what a struct's children chain
/// needs. Functions, states and classes stop after their `UField` header, so their bytecode is not decoded.
pub(crate) fn read_child(pkg: &Package, file: &[u8], index: usize) -> Result<(i32, Option<Property>), Error> {
    let Some(export) = pkg.exports.get(index) else {
        return Err(Error::WrongKind { export: index, expected: "export" });
    };
    let definition = Definition::from_class_name(class_name(pkg, export));
    if matches!(definition, Some(Definition::Class | Definition::Struct | Definition::State | Definition::Function)) {
        let mut r = pkg.native_reader(file, export)?;
        return Ok((read_field(&mut r)?.next, None));
    }
    match read_export(pkg, file, index)? {
        Some(ScriptObject::Property(property)) => Ok((property.field.next, Some(property))),
        object => Ok((object.as_ref().and_then(ScriptObject::field).map_or(0, |f| f.next), None)),
    }
}

#[derive(Clone, Copy)]
enum Definition {
    Class,
    Struct,
    State,
    Function,
    Enum,
    Const,
    TextBuffer,
    Property(PropertyClass),
}

#[derive(Clone, Copy)]
enum PropertyClass {
    Byte,
    Int,
    Bool,
    Float,
    Object,
    Class,
    Name,
    Str,
    String,
    Array,
    Struct,
    Map,
    FixedArray,
    Delegate,
}

const DEFINITIONS: [(&str, Definition); 21] = [
    ("Class", Definition::Class),
    ("Struct", Definition::Struct),
    ("State", Definition::State),
    ("Function", Definition::Function),
    ("Enum", Definition::Enum),
    ("Const", Definition::Const),
    ("TextBuffer", Definition::TextBuffer),
    ("ByteProperty", Definition::Property(PropertyClass::Byte)),
    ("IntProperty", Definition::Property(PropertyClass::Int)),
    ("BoolProperty", Definition::Property(PropertyClass::Bool)),
    ("FloatProperty", Definition::Property(PropertyClass::Float)),
    ("ObjectProperty", Definition::Property(PropertyClass::Object)),
    ("ClassProperty", Definition::Property(PropertyClass::Class)),
    ("NameProperty", Definition::Property(PropertyClass::Name)),
    ("StrProperty", Definition::Property(PropertyClass::Str)),
    ("StringProperty", Definition::Property(PropertyClass::String)),
    ("ArrayProperty", Definition::Property(PropertyClass::Array)),
    ("StructProperty", Definition::Property(PropertyClass::Struct)),
    ("MapProperty", Definition::Property(PropertyClass::Map)),
    ("FixedArrayProperty", Definition::Property(PropertyClass::FixedArray)),
    ("DelegateProperty", Definition::Property(PropertyClass::Delegate)),
];

impl Definition {
    fn from_class_name(name: &str) -> Option<Self> {
        DEFINITIONS.iter().find(|(class, _)| class.eq_ignore_ascii_case(name)).map(|&(_, definition)| definition)
    }
}

fn read_field(r: &mut Reader) -> Result<Field, Error> {
    Ok(Field { super_field: r.compact()?, next: r.compact()? })
}

fn read_struct(pkg: &Package, r: &mut Reader) -> Result<Struct, Error> {
    let field = read_field(r)?;
    let script_text = r.compact()?;
    let children = r.compact()?;
    let friendly_name = pkg.read_name(r)?;
    let line = r.i32()?;
    let text_pos = r.i32()?;
    let script_size = r.u32()?;
    let start = r.pos();
    bytecode::skip_script(pkg, r, script_size)?;
    let end = r.pos();
    r.seek(start)?;
    let script = r.bytes(end - start)?.to_vec();
    Ok(Struct { field, script_text, children, friendly_name, line, text_pos, script_size, script })
}

fn read_function(pkg: &Package, r: &mut Reader) -> Result<Function, Error> {
    let base = read_struct(pkg, r)?;
    let native = r.u16()?;
    let operator_precedence = r.u8()?;
    let flags = r.u32()?;
    let rep_offset = if flags & FUNC_NET != 0 { Some(r.u16()?) } else { None };
    Ok(Function { base, native, operator_precedence, flags, rep_offset })
}

fn read_state(pkg: &Package, r: &mut Reader) -> Result<State, Error> {
    let base = read_struct(pkg, r)?;
    Ok(State {
        base,
        probe_mask: u64::from_le_bytes(r.array()?),
        ignore_mask: u64::from_le_bytes(r.array()?),
        label_table_offset: r.u16()?,
        flags: r.u32()?,
    })
}

fn read_class(pkg: &Package, r: &mut Reader) -> Result<Class, Error> {
    let base = read_state(pkg, r)?;
    let flags = r.u32()?;
    let guid = r.array()?;
    let count = r.count()?;
    let dependencies = (0..count)
        .map(|_| Ok(Dependency { class: r.compact()?, deep: r.i32()?, script_text_crc: r.u32()? }))
        .collect::<Result<Vec<_>, Error>>()?;
    let package_imports = read_names(pkg, r)?;
    let within = r.compact()?;
    let config_name = pkg.read_name(r)?;
    let hide_categories = read_names(pkg, r)?;
    let defaults_offset = r.pos();
    Ok(Class {
        base,
        flags,
        guid,
        dependencies,
        package_imports,
        within,
        config_name,
        hide_categories,
        defaults_offset,
    })
}

fn read_property(pkg: &Package, r: &mut Reader, class: PropertyClass) -> Result<Property, Error> {
    let field = read_field(r)?;
    let array_dim = r.i32()?;
    let flags = r.u32()?;
    let category = pkg.read_name(r)?;
    let rep_offset = if flags & CPF_NET != 0 { Some(r.u16()?) } else { None };
    let comment = if flags & CPF_COMMENT_STRING != 0 { Some(r.fstring()?) } else { None };
    let kind = match class {
        PropertyClass::Byte => PropertyKind::Byte { enum_: r.compact()? },
        PropertyClass::Int => PropertyKind::Int,
        PropertyClass::Bool => PropertyKind::Bool,
        PropertyClass::Float => PropertyKind::Float,
        PropertyClass::Object => PropertyKind::Object { class: r.compact()? },
        PropertyClass::Class => PropertyKind::Class { class: r.compact()?, meta_class: r.compact()? },
        PropertyClass::Name => PropertyKind::Name,
        PropertyClass::Str => PropertyKind::Str,
        PropertyClass::String => PropertyKind::String,
        PropertyClass::Array => PropertyKind::Array { inner: r.compact()? },
        PropertyClass::Struct => PropertyKind::Struct { struct_: r.compact()? },
        PropertyClass::Map => PropertyKind::Map { key: r.compact()?, value: r.compact()? },
        PropertyClass::FixedArray => PropertyKind::FixedArray { inner: r.compact()?, count: r.i32()? },
        PropertyClass::Delegate => PropertyKind::Delegate { function: r.compact()? },
    };
    Ok(Property { field, array_dim, flags, category, rep_offset, comment, kind })
}

fn read_names(pkg: &Package, r: &mut Reader) -> Result<Vec<usize>, Error> {
    let count = r.count()?;
    (0..count).map(|_| pkg.read_name(r).map_err(Error::from)).collect()
}
