use crate::reader::Reader;
use crate::{Error, Export, Package, name_ref};

const RF_HAS_STACK: u32 = 0x0200_0000;

pub mod tag {
    pub const BYTE: u8 = 1;
    pub const INT: u8 = 2;
    pub const BOOL: u8 = 3;
    pub const FLOAT: u8 = 4;
    pub const OBJECT: u8 = 5;
    pub const NAME: u8 = 6;
    pub const STRING: u8 = 7;
    pub const CLASS: u8 = 8;
    pub const ARRAY: u8 = 9;
    pub const STRUCT: u8 = 10;
    pub const STR: u8 = 13;
    pub const MAP: u8 = 14;
    pub const FIXED_ARRAY: u8 = 15;
}

use tag::{BOOL, STRUCT};

#[derive(Debug, Clone)]
pub struct Property {
    pub name: usize,
    pub array_index: u32,
    pub value: TagValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TagValue {
    Byte(u8),
    Int(i32),
    Bool(bool),
    Float(f32),
    Object(i32),
    Name(usize),
    Str(String),
    Struct { name: usize, data: Vec<u8> },
    Other { kind: u8, data: Vec<u8> },
}

#[derive(Debug, Clone, Copy)]
pub struct PropertyTag {
    pub name: usize,
    pub kind: u8,
    pub struct_name: Option<usize>,
    /// Value size in bytes as saved. Unreliable for some nested values.
    pub size: usize,
    pub array_index: u32,
    pub bool_value: bool,
}

#[derive(Debug, Clone)]
pub struct ObjectData {
    pub properties: Vec<Property>,
    pub native_offset: usize,
}

impl ObjectData {
    pub fn property<'a>(&'a self, pkg: &Package, name: &str) -> Option<&'a TagValue> {
        self.properties
            .iter()
            .find(|p| p.array_index == 0 && pkg.name(p.name).eq_ignore_ascii_case(name))
            .map(|p| &p.value)
    }
}

impl Package {
    pub fn read_object(&self, file: &[u8], export: &Export) -> Result<ObjectData, Error> {
        self.read_native(file, export).map(|(object, _)| object)
    }

    pub fn read_native<'a>(&self, file: &'a [u8], export: &Export) -> Result<(ObjectData, Reader<'a>), Error> {
        let mut r = self.export_reader(file, export)?;

        // Objects of class `Class` (class index 0) have no tagged properties here.
        let properties = if export.class_index != 0 { self.read_properties(&mut r)? } else { Vec::new() };
        Ok((ObjectData { properties, native_offset: r.pos() }, r))
    }

    pub fn native_reader<'a>(&self, file: &'a [u8], export: &Export) -> Result<Reader<'a>, Error> {
        let mut r = self.export_reader(file, export)?;
        if export.class_index != 0 {
            while let Some(property) = self.read_tag(&mut r)? {
                if property.kind != BOOL {
                    r.bytes(property.size)?;
                }
            }
        }
        Ok(r)
    }

    pub fn read_properties(&self, r: &mut Reader) -> Result<Vec<Property>, Error> {
        let mut properties = Vec::new();
        while let Some(property) = self.read_property(r)? {
            properties.push(property);
        }
        Ok(properties)
    }

    pub fn export_reader<'a>(&self, file: &'a [u8], export: &Export) -> Result<Reader<'a>, Error> {
        let end = export.serial_offset.saturating_add(export.serial_size);
        let data = file
            .get(..end)
            .ok_or(Error::Eof { offset: export.serial_offset, needed: export.serial_size })?;
        let mut r = Reader::new(data);
        r.seek(export.serial_offset)?;

        if export.object_flags & RF_HAS_STACK != 0 {
            let node = r.compact()?;
            r.compact()?; // StateNode
            r.bytes(8)?; // ProbeMask
            r.i32()?; // LatentAction
            if node != 0 {
                r.compact()?; // Code offset into Node's script, -1 = none
            }
        }
        Ok(r)
    }

    pub fn read_name(&self, r: &mut Reader) -> Result<usize, Error> {
        name_ref(r, self.names.len())
    }

    pub fn read_tag(&self, r: &mut Reader) -> Result<Option<PropertyTag>, Error> {
        let name = self.read_name(r)?;
        if self.name(name).eq_ignore_ascii_case("None") {
            return Ok(None);
        }
        // Bits 0-3 type, 4-6 size code, 7 array index follows (for bools: the value).
        let info = r.u8()?;
        let kind = info & 0x0f;
        let struct_name = if kind == STRUCT { Some(self.read_name(r)?) } else { None };
        let size = match (info >> 4) & 0x07 {
            0 => 1,
            1 => 2,
            2 => 4,
            3 => 12,
            4 => 16,
            5 => usize::from(r.u8()?),
            6 => usize::from(r.u16()?),
            _ => r.u32()? as usize,
        };
        let has_array_index = info & 0x80 != 0 && kind != BOOL;
        let array_index = if has_array_index { read_array_index(r)? } else { 0 };
        let bool_value = kind == BOOL && info & 0x80 != 0;
        Ok(Some(PropertyTag { name, kind, struct_name, size, array_index, bool_value }))
    }

    fn read_property(&self, r: &mut Reader) -> Result<Option<Property>, Error> {
        let Some(PropertyTag { name, kind, struct_name, size, array_index, bool_value }) = self.read_tag(r)? else {
            return Ok(None);
        };

        // A bool's value lives in the info byte; the engine reads no value bytes for it.
        if kind == BOOL {
            return Ok(Some(Property { name, array_index, value: TagValue::Bool(bool_value) }));
        }

        let offset = r.pos();
        let mut v = r.sub(size)?;
        let value = match kind {
            tag::BYTE => TagValue::Byte(v.u8()?),
            tag::INT => TagValue::Int(v.i32()?),
            tag::FLOAT => TagValue::Float(v.f32()?),
            tag::OBJECT | tag::CLASS => TagValue::Object(v.compact()?),
            tag::NAME => TagValue::Name(self.read_name(&mut v)?),
            tag::STR => TagValue::Str(v.fstring()?),
            STRUCT => TagValue::Struct { name: struct_name.unwrap_or_default(), data: v.rest().to_vec() },
            _ => TagValue::Other { kind, data: v.rest().to_vec() },
        };
        if !v.at_end() {
            return Err(Error::Invalid { what: "property size", offset });
        }
        Ok(Some(Property { name, array_index, value }))
    }
}

/// Array index: 1 byte (< 0x80), 2 bytes (`10xxxxxx`), or 4 bytes (`11xxxxxx`), big-endian.
fn read_array_index(r: &mut Reader) -> Result<u32, Error> {
    let first = u32::from(r.u8()?);
    Ok(if first & 0x80 == 0 {
        first
    } else if first & 0xc0 == 0x80 {
        (first & 0x7f) << 8 | u32::from(r.u8()?)
    } else {
        let [a, b, c] = r.array()?;
        (first & 0x3f) << 24 | u32::from_be_bytes([0, a, b, c])
    })
}
