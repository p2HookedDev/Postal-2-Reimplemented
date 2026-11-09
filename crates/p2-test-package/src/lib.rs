//! Writes real `.u` package bytes (version 118, licensee 2) from declarations in test code, so a test can run the normal
//! load path without the game install. Object references follow the package format: an export is `index + 1`, an import
//! is `-index - 1`, and 0 is none.

use std::path::PathBuf;

use p2_package::PACKAGE_TAG;
use p2_package::flags::RF_PUBLIC;

pub mod token {
    pub const LOCAL_VARIABLE: u8 = 0x00;
    pub const INSTANCE_VARIABLE: u8 = 0x01;
    pub const RETURN: u8 = 0x04;
    pub const STOP: u8 = 0x08;
    pub const NOTHING: u8 = 0x0b;
    pub const LET: u8 = 0x0f;
    pub const CONTEXT: u8 = 0x19;
    pub const INT_CONST: u8 = 0x1d;
}

const VERSION: u16 = 118;
const LICENSEE: u16 = 2;
const SUMMARY_SIZE: usize = 64;
const NONE: i32 = 0;
const NO_LABEL: i32 = 0xffff;

/// Bytecode under assembly. A reference takes 4 bytes of the script's size but a compact index on disk, so both are counted.
#[derive(Default)]
pub struct Script {
    bytes: Vec<u8>,
    size: u32,
    label_table: u16,
}

impl Script {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn token(mut self, token: u8) -> Self {
        self.bytes.push(token);
        self.size += 1;
        self
    }

    pub fn int(mut self, value: i32) -> Self {
        self.bytes.extend(value.to_le_bytes());
        self.size += 4;
        self
    }

    pub fn object(mut self, reference: i32) -> Self {
        compact(&mut self.bytes, reference);
        self.size += 4;
        self
    }

    /// Ends state code. `labels` holds a name index from [`PackageBuilder::name`] and that label's offset in the script's size.
    pub fn label_table(mut self, labels: &[(usize, u32)]) -> Self {
        self.label_table = self.size as u16;
        self.bytes.push(0x0c);
        self.size += 1;
        for &(name, offset) in labels {
            compact(&mut self.bytes, name as i32);
            self.bytes.extend(offset.to_le_bytes());
            self.size += 8;
        }
        compact(&mut self.bytes, NONE);
        self.bytes.extend(NO_LABEL.to_le_bytes());
        self.size += 8;
        self
    }
}

pub enum PropertyType {
    Int,
    Float,
    Byte,
    Bool,
    Name,
    Object,
    Struct(i32),
}

/// `Name` holds an index from [`PackageBuilder::name`], `Object` an object reference.
pub enum DefaultValue {
    Int(i32),
    Float(f32),
    Byte(u8),
    Bool(bool),
    Name(usize),
    Object(i32),
    Vector([f32; 3]),
    Rotator([i32; 3]),
}

enum Kind {
    Class,
    Function { flags: u32 },
    State,
    Struct,
    Property { flags: u32, reference: Option<i32> },
}

struct Declared {
    class: i32,
    super_field: i32,
    outer: i32,
    name: usize,
    children: Vec<i32>,
    script: Script,
    defaults: Vec<(usize, Vec<u8>)>,
    probe_mask: u64,
    kind: Kind,
}

struct Import {
    class_package: usize,
    class_name: usize,
    outer: i32,
    name: usize,
}

pub struct PackageBuilder {
    file_name: String,
    names: Vec<String>,
    imports: Vec<Import>,
    exports: Vec<Declared>,
}

impl PackageBuilder {
    pub fn new(name: &str) -> Self {
        let mut package = Self { file_name: name.to_owned(), names: vec!["None".to_owned()], imports: Vec::new(), exports: Vec::new() };
        let core = package.name("Core");
        let class_name = package.name("Package");
        package.imports.push(Import { class_package: core, class_name, outer: 0, name: core });
        package
    }

    /// Index of `text` in the name table, added if new.
    pub fn name(&mut self, text: &str) -> usize {
        if let Some(index) = self.names.iter().position(|name| name.eq_ignore_ascii_case(text)) {
            return index;
        }
        self.names.push(text.to_owned());
        self.names.len() - 1
    }

    pub fn class(&mut self, name: &str, parent: Option<i32>) -> i32 {
        self.declare(0, parent.unwrap_or(0), 0, name, Kind::Class)
    }

    /// `owner` is a class, or a function for a parameter or return value.
    pub fn int_property(&mut self, owner: i32, name: &str, flags: u32) -> i32 {
        self.property(owner, name, PropertyType::Int, flags)
    }

    pub fn property(&mut self, owner: i32, name: &str, ty: PropertyType, flags: u32) -> i32 {
        let (class, reference) = match ty {
            PropertyType::Int => ("IntProperty", None),
            PropertyType::Float => ("FloatProperty", None),
            PropertyType::Byte => ("ByteProperty", Some(NONE)),
            PropertyType::Bool => ("BoolProperty", None),
            PropertyType::Name => ("NameProperty", None),
            PropertyType::Object => ("ObjectProperty", Some(NONE)),
            PropertyType::Struct(reference) => ("StructProperty", Some(reference)),
        };
        let class = self.core_class(class);
        self.declare(class, 0, owner, name, Kind::Property { flags, reference })
    }

    /// Give it members with [`Self::property`]. `Vector` and `Rotator` are read as binary, in declaration order.
    pub fn struct_type(&mut self, name: &str) -> i32 {
        let class = self.core_class("Struct");
        self.declare(class, 0, 0, name, Kind::Struct)
    }

    /// Marks `class` as handling the probe `bit`, as a compiled class does for an event it declares.
    pub fn probe(&mut self, class: i32, bit: u32) {
        self.exports[class as usize - 1].probe_mask |= 1 << bit;
    }

    pub fn default_int(&mut self, class: i32, property: &str, value: i32) {
        self.default_value(class, property, DefaultValue::Int(value));
    }

    pub fn default_value(&mut self, class: i32, property: &str, value: DefaultValue) {
        let name = self.name(property);
        let mut tag = Vec::new();
        match value {
            DefaultValue::Int(value) => {
                tag.push(0x22);
                tag.extend(value.to_le_bytes());
            }
            DefaultValue::Float(value) => {
                tag.push(0x24);
                tag.extend(value.to_le_bytes());
            }
            DefaultValue::Byte(value) => tag.extend([0x01, value]),
            DefaultValue::Bool(value) => tag.extend([if value { 0xd3 } else { 0x53 }, 0]),
            DefaultValue::Name(value) => sized_tag(&mut tag, 0x56, value as i32),
            DefaultValue::Object(value) => sized_tag(&mut tag, 0x55, value),
            DefaultValue::Vector(values) => {
                tag.push(0x3a);
                compact(&mut tag, self.name("Vector") as i32);
                tag.extend(values.into_iter().flat_map(f32::to_le_bytes));
            }
            DefaultValue::Rotator(values) => {
                tag.push(0x3a);
                compact(&mut tag, self.name("Rotator") as i32);
                tag.extend(values.into_iter().flat_map(i32::to_le_bytes));
            }
        }
        self.exports[class as usize - 1].defaults.push((name, tag));
    }

    pub fn function(&mut self, owner: i32, name: &str, flags: u32) -> i32 {
        let class = self.core_class("Function");
        self.declare(class, 0, owner, name, Kind::Function { flags })
    }

    pub fn state(&mut self, owner: i32, name: &str) -> i32 {
        let class = self.core_class("State");
        self.declare(class, 0, owner, name, Kind::State)
    }

    pub fn script(&mut self, owner: i32, script: Script) {
        self.exports[owner as usize - 1].script = script;
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut next = vec![0; self.exports.len()];
        for export in &self.exports {
            for pair in export.children.windows(2) {
                next[pair[0] as usize - 1] = pair[1];
            }
        }
        let mut out = vec![0; SUMMARY_SIZE];
        for name in &self.names {
            fstring(&mut out, name);
            out.extend(0_u32.to_le_bytes());
        }
        let mut serials = Vec::with_capacity(self.exports.len());
        for (export, &next) in self.exports.iter().zip(&next) {
            let start = out.len();
            write_export(export, next, &mut out);
            serials.push((out.len() - start, start));
        }
        let import_offset = out.len();
        for import in &self.imports {
            compact(&mut out, import.class_package as i32);
            compact(&mut out, import.class_name as i32);
            out.extend(import.outer.to_le_bytes());
            compact(&mut out, import.name as i32);
        }
        let export_offset = out.len();
        for (export, &(size, offset)) in self.exports.iter().zip(&serials) {
            compact(&mut out, export.class);
            compact(&mut out, export.super_field);
            out.extend(export.outer.to_le_bytes());
            compact(&mut out, export.name as i32);
            out.extend(RF_PUBLIC.to_le_bytes());
            compact(&mut out, size as i32);
            compact(&mut out, offset as i32);
        }
        out[..SUMMARY_SIZE].copy_from_slice(&self.summary(import_offset, export_offset));
        out
    }

    /// Writes `<package name>.u` into a directory named after `dir` under the system temp dir, for
    /// `Loader::new(path, &["*.u".into()])`. The file is rewritten on each run, so give each test its own `dir`.
    pub fn write_to_temp(&self, dir: &str) -> std::io::Result<PathBuf> {
        let dir = std::env::temp_dir().join(format!("p2-test-package-{dir}"));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(format!("{}.u", self.file_name)), self.bytes())?;
        Ok(dir)
    }

    fn declare(&mut self, class: i32, super_field: i32, outer: i32, name: &str, kind: Kind) -> i32 {
        let name = self.name(name);
        self.exports.push(Declared { class, super_field, outer, name, children: Vec::new(), script: Script::new(), defaults: Vec::new(), probe_mask: 0, kind });
        let reference = self.exports.len() as i32;
        if outer != 0 {
            self.exports[outer as usize - 1].children.push(reference);
        }
        reference
    }

    fn core_class(&mut self, class: &str) -> i32 {
        let name = self.name(class);
        let index = self.imports.iter().position(|import| import.name == name).unwrap_or_else(|| {
            let class_package = self.name("Core");
            let class_name = self.name("Class");
            self.imports.push(Import { class_package, class_name, outer: -1, name });
            self.imports.len() - 1
        });
        -(index as i32) - 1
    }

    fn summary(&self, import_offset: usize, export_offset: usize) -> Vec<u8> {
        let (name_count, export_count) = (self.names.len() as i32, self.exports.len() as i32);
        let mut out = Vec::with_capacity(SUMMARY_SIZE);
        out.extend(PACKAGE_TAG.to_le_bytes());
        out.extend(VERSION.to_le_bytes());
        out.extend(LICENSEE.to_le_bytes());
        let tables = [0, name_count, SUMMARY_SIZE as i32, export_count, export_offset as i32, self.imports.len() as i32, import_offset as i32];
        for value in tables {
            out.extend(value.to_le_bytes());
        }
        out.extend([0; 16]);
        for value in [1, export_count, name_count] {
            out.extend(value.to_le_bytes());
        }
        out
    }

    /// A dynamic array of `element`, as an `array<...>` declaration compiles to.
    pub fn array_property(&mut self, owner: i32, name: &str, element: PropertyType, flags: u32) -> i32 {
        let class = self.core_class("ArrayProperty");
        let array = self.declare(class, 0, owner, name, Kind::Property { flags, reference: Some(NONE) });
        let inner = self.property(array, name, element, 0);
        self.exports[array as usize - 1].kind = Kind::Property { flags, reference: Some(inner) };
        array
    }
}

fn write_export(export: &Declared, next: i32, out: &mut Vec<u8>) {
    if !matches!(export.kind, Kind::Class) {
        compact(out, NONE);
    }
    compact(out, export.super_field);
    compact(out, next);
    match export.kind {
        Kind::Property { flags, reference } => {
            out.extend(1_i32.to_le_bytes());
            out.extend(flags.to_le_bytes());
            compact(out, NONE);
            if let Some(reference) = reference {
                compact(out, reference);
            }
        }
        Kind::Struct => write_struct(export, out),
        Kind::Function { flags } => {
            write_struct(export, out);
            out.extend([0; 3]);
            out.extend(flags.to_le_bytes());
        }
        Kind::State => {
            write_struct(export, out);
            write_state(export, out);
        }
        Kind::Class => {
            write_struct(export, out);
            write_state(export, out);
            write_class(export, out);
        }
    }
}

fn write_struct(export: &Declared, out: &mut Vec<u8>) {
    compact(out, 0);
    compact(out, export.children.first().copied().unwrap_or(0));
    compact(out, export.name as i32);
    out.extend(0_i32.to_le_bytes());
    out.extend(0_i32.to_le_bytes());
    out.extend(export.script.size.to_le_bytes());
    out.extend_from_slice(&export.script.bytes);
}

fn write_state(export: &Declared, out: &mut Vec<u8>) {
    let ignore_mask = if export.probe_mask == 0 { 0 } else { u64::MAX };
    out.extend(export.probe_mask.to_le_bytes());
    out.extend(ignore_mask.to_le_bytes());
    out.extend(export.script.label_table.to_le_bytes());
    out.extend(0_u32.to_le_bytes());
}

fn write_class(export: &Declared, out: &mut Vec<u8>) {
    out.extend(0_u32.to_le_bytes());
    out.extend([0; 16]);
    compact(out, 0);
    compact(out, 0);
    compact(out, 0);
    compact(out, NONE);
    compact(out, 0);
    for (name, tag) in &export.defaults {
        compact(out, *name as i32);
        out.extend_from_slice(tag);
    }
    compact(out, NONE);
}

fn compact(out: &mut Vec<u8>, value: i32) {
    let magnitude = value.unsigned_abs();
    let mut rest = magnitude >> 6;
    let sign = if value < 0 { 0x80 } else { 0 };
    let more = if rest > 0 { 0x40 } else { 0 };
    out.push(sign | more | (magnitude & 0x3f) as u8);
    while rest > 0 {
        let more = if rest >= 0x80 { 0x80 } else { 0 };
        out.push(more | (rest & 0x7f) as u8);
        rest >>= 7;
    }
}

fn sized_tag(out: &mut Vec<u8>, info: u8, value: i32) {
    let mut bytes = Vec::new();
    compact(&mut bytes, value);
    out.extend([info, bytes.len() as u8]);
    out.append(&mut bytes);
}

fn fstring(out: &mut Vec<u8>, text: &str) {
    compact(out, text.len() as i32 + 1);
    out.extend_from_slice(text.as_bytes());
    out.push(0);
}

#[cfg(test)]
mod tests {
    use p2_package::{Package, Reader};
    use p2_script::{PropertyKind, ScriptObject, read_export};

    use super::*;

    #[test]
    fn written_compact_reads_back() {
        for value in [0, 5, -1, 63, 64, -64, 8191, 8192, i32::MAX, i32::MIN] {
            let mut bytes = Vec::new();
            compact(&mut bytes, value);
            let mut r = Reader::new(&bytes);
            assert_eq!(r.compact().unwrap(), value);
            assert!(r.at_end());
        }
    }

    #[test]
    fn written_package_parses() {
        let mut builder = PackageBuilder::new("Fixture");
        let base = builder.class("Base", None);
        let derived = builder.class("Derived", Some(base));
        let count = builder.int_property(derived, "Count", 0);
        let function = builder.function(derived, "Run", 0);
        builder.script(function, Script::new().token(token::LET).token(token::INSTANCE_VARIABLE).object(count).token(token::INT_CONST).int(1));
        builder.default_int(derived, "Count", 3);
        let bytes = builder.bytes();

        let package = Package::parse(&bytes).unwrap();
        assert_eq!(package.summary.version, VERSION);
        assert_eq!(package.object_name(function), Some("Run"));
        assert_eq!(package.object_name(package.exports[function as usize - 1].class_index), Some("Function"));

        let Some(ScriptObject::Class(class)) = read_export(&package, &bytes, derived as usize - 1).unwrap() else { panic!("not a class") };
        assert_eq!(class.base.base.field.super_field, base);
        assert_eq!(class.base.base.children, count);
        let Some(ScriptObject::Property(property)) = read_export(&package, &bytes, count as usize - 1).unwrap() else { panic!("not a property") };
        assert_eq!((property.field.next, property.kind), (function, PropertyKind::Int));
        let Some(ScriptObject::Function(run)) = read_export(&package, &bytes, function as usize - 1).unwrap() else { panic!("not a function") };
        assert_eq!(run.base.script_size, 11);
        assert_eq!(run.base.script.len(), 8);
    }
}
