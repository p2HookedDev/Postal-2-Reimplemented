//! Tagged properties decoded by declaration: each tag is matched by name to a declared property and
//! read by that property's type. The tag's Size is used only to skip tags the engine skips.

use std::sync::Arc;

use p2_package::tag::{ARRAY as TAG_ARRAY, BYTE as TAG_BYTE, STRING as TAG_STRING};
use p2_package::{Package, PropertyTag, Reader};

use crate::Error;
use crate::layout::{FieldLayout, StructLayout, Type};

/// Structs serialized as raw fields instead of tagged properties (version >= 118).
const BINARY_STRUCTS: [&str; 3] = ["Vector", "Rotator", "Color"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Properties {
    pub values: Vec<PropertyValue>,
    pub skipped: Vec<SkippedTag>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PropertyValue {
    pub name: Arc<str>,
    /// Static array index. For a dynamic array saved one tag per element (the engine
    /// accepts this), the element index, with [`PropertyValue::value`] holding the element.
    pub array_index: u32,
    pub value: StoredValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedTag {
    pub name: usize,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Unknown,
    TypeMismatch,
    ArrayIndex,
    StructMismatch,
    NotSerialized,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StoredValue {
    Byte(u8),
    Int(i32),
    Bool(bool),
    Float(f32),
    Object(i32),
    Name(usize),
    Str(String),
    Array(Vec<Self>),
    Struct(Properties),
    Delegate { object: i32, function: usize },
    Map,
}

pub fn read_properties(pkg: &Package, r: &mut Reader, layout: &StructLayout) -> Result<Properties, Error> {
    let mut out = Properties::default();
    while let Some(tag) = pkg.read_tag(r)? {
        let skip = match layout.find(pkg.name(tag.name)) {
            None => Some(SkipReason::Unknown),
            Some(field) => read_tagged(pkg, r, &tag, field, &mut out.values)?,
        };
        if let Some(reason) = skip {
            r.bytes(tag.size)?;
            out.skipped.push(SkippedTag { name: tag.name, reason });
        }
    }
    Ok(out)
}

fn read_tagged(
    pkg: &Package,
    r: &mut Reader,
    tag: &PropertyTag,
    field: &FieldLayout,
    values: &mut Vec<PropertyValue>,
) -> Result<Option<SkipReason>, Error> {
    let mut push = |array_index, value| {
        values.push(PropertyValue { name: field.name.clone(), array_index, value });
    };

    if tag.kind == TAG_STRING && matches!(field.ty, Type::Str) {
        let limit = if tag.size == 0 { 254 } else { tag.size };
        let mut text = String::new();
        for _ in 0..limit {
            match r.u8()? {
                0 => break,
                b => text.push(char::from(b)),
            }
        }
        push(tag.array_index, StoredValue::Str(text));
        return Ok(None);
    }
    if let Type::Array(inner) = &field.ty
        && tag.kind != TAG_ARRAY
        && inner.id() == Some(tag.kind)
    {
        push(tag.array_index, read_tag_value(pkg, r, tag, inner)?);
        return Ok(None);
    }
    if tag.kind == TAG_BYTE && matches!(field.ty, Type::Float) {
        push(tag.array_index, StoredValue::Float(f32::from(r.u8()?)));
        return Ok(None);
    }
    if field.ty.id().is_some_and(|id| id != tag.kind) {
        return Ok(Some(SkipReason::TypeMismatch));
    }
    if tag.array_index >= field.array_dim {
        return Ok(Some(SkipReason::ArrayIndex));
    }
    if let (Type::Struct(layout), Some(name)) = (&field.ty, tag.struct_name)
        && !pkg.name(name).eq_ignore_ascii_case(&layout.name)
    {
        return Ok(Some(SkipReason::StructMismatch));
    }
    if !field.is_serialized() {
        return Ok(Some(SkipReason::NotSerialized));
    }
    push(tag.array_index, read_tag_value(pkg, r, tag, &field.ty)?);
    Ok(None)
}

fn read_tag_value(pkg: &Package, r: &mut Reader, tag: &PropertyTag, ty: &Type) -> Result<StoredValue, Error> {
    match ty {
        Type::Bool => Ok(StoredValue::Bool(tag.bool_value)),
        _ => read_item(pkg, r, ty),
    }
}

fn read_item(pkg: &Package, r: &mut Reader, ty: &Type) -> Result<StoredValue, Error> {
    Ok(match ty {
        Type::Byte => StoredValue::Byte(r.u8()?),
        Type::Int => StoredValue::Int(r.i32()?),
        Type::Bool => StoredValue::Bool(r.u8()? != 0),
        Type::Float => StoredValue::Float(r.f32()?),
        Type::Object | Type::Class => StoredValue::Object(r.compact()?),
        Type::Name => StoredValue::Name(pkg.read_name(r)?),
        Type::Str => StoredValue::Str(r.fstring()?),
        Type::String => return Err(Error::Unsupported("StringProperty")),
        Type::Array(inner) => {
            let offset = r.pos();
            let count = r.count()?;
            check_elements(inner, i64::try_from(count).unwrap_or(i64::MAX), offset)?;
            StoredValue::Array((0..count).map(|_| read_item(pkg, r, inner)).collect::<Result<_, _>>()?)
        }
        Type::FixedArray(inner, count) => {
            check_elements(inner, i64::from(*count), r.pos())?;
            StoredValue::Array((0..*count).map(|_| read_item(pkg, r, inner)).collect::<Result<_, _>>()?)
        }
        Type::Struct(layout) if is_binary(layout) => {
            StoredValue::Struct(read_binary(pkg, r, layout)?)
        }
        Type::Struct(layout) => StoredValue::Struct(read_properties(pkg, r, layout)?),
        Type::Map => StoredValue::Map,
        Type::Delegate => StoredValue::Delegate { object: r.compact()?, function: pkg.read_name(r)? },
    })
}

fn read_binary(pkg: &Package, r: &mut Reader, layout: &StructLayout) -> Result<Properties, Error> {
    let mut values = Vec::new();
    for field in layout.properties.iter().filter(|f| f.is_serialized()) {
        // One value of a type that takes no bytes is harmless; a declared count of them is not.
        check_elements(&field.ty, i64::from(field.array_dim) - 1, r.pos())?;
        for array_index in 0..field.array_dim {
            values.push(PropertyValue { name: field.name.clone(), array_index, value: read_item(pkg, r, &field.ty)? });
        }
    }
    Ok(Properties { values, skipped: Vec::new() })
}

fn is_binary(layout: &StructLayout) -> bool {
    BINARY_STRUCTS.iter().any(|s| s.eq_ignore_ascii_case(&layout.name))
}

/// Rejects `count` elements of `inner` when an element takes no bytes: nothing but the declaration would then limit
/// how many values a few damaged bytes ask for.
fn check_elements(inner: &Type, count: i64, offset: usize) -> Result<(), Error> {
    if count > 0 && reads_nothing(inner) {
        return Err(p2_package::Error::Invalid { what: "array of values that take no bytes", offset }.into());
    }
    Ok(())
}

fn reads_nothing(ty: &Type) -> bool {
    match ty {
        Type::Map => true,
        Type::FixedArray(inner, count) => *count == 0 || reads_nothing(inner),
        Type::Struct(layout) if is_binary(layout) => {
            layout.properties.iter().filter(|f| f.is_serialized()).all(|f| f.array_dim == 0 || reads_nothing(&f.ty))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_that_take_no_bytes() {
        assert!(reads_nothing(&Type::Map));
        assert!(reads_nothing(&Type::FixedArray(Box::new(Type::Map), 3)));
        assert!(reads_nothing(&Type::FixedArray(Box::new(Type::Byte), 0)));
        assert!(!reads_nothing(&Type::Array(Box::new(Type::Map))));
        assert!(!reads_nothing(&Type::FixedArray(Box::new(Type::Byte), 3)));
        assert!(check_elements(&Type::Map, 1 << 30, 0).is_err());
        assert!(check_elements(&Type::Map, 0, 0).is_ok());
        assert!(check_elements(&Type::Byte, 1 << 30, 0).is_ok());
    }
}
