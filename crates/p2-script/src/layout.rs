use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

use p2_package::{Loader, Package, import_path, tag};

use crate::{Error, PropertyKind, ScriptObject, read_child, read_export};

/// Struct and array nesting limit; deeper means a reference cycle.
const MAX_DEPTH: usize = 64;

const CPF_NATIVE: u32 = 0x1000;
const CPF_TRANSIENT: u32 = 0x2000;

#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub package: &'a Package,
    pub data: &'a [u8],
}

pub trait Resolve {
    fn resolve_import(&self, from: &Package, import: usize) -> Option<(Source<'_>, usize)>;
}

impl Resolve for Loader {
    fn resolve_import(&self, from: &Package, import: usize) -> Option<(Source<'_>, usize)> {
        let (id, export) = match self.id_of(from) {
            Some(id) => self.loaded_export(id, -(import as i32) - 1)?,
            None => self.loaded_export_by_path_of_class(&import_path(from, import), from.name(from.imports.get(import)?.class_name))?,
        };
        let loaded = self.package(id);
        Some((Source { package: &loaded.package, data: &loaded.data }, export))
    }
}

#[derive(Debug)]
pub struct StructLayout {
    pub name: String,
    /// Own properties in children-chain order, then the super struct's, recursively
    /// (the order of the engine's field iterator).
    pub properties: Vec<FieldLayout>,
    /// The struct or class it extends, an object reference of its package; 0 = none.
    pub super_field: i32,
    pub defaults_offset: Option<usize>,
    first_by_name: HashMap<u64, u32>,
}

#[derive(Debug, Clone)]
pub struct FieldLayout {
    pub name: Arc<str>,
    pub array_dim: u32,
    pub flags: u32,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub enum Type {
    Byte,
    Int,
    Bool,
    Float,
    Object,
    Class,
    Name,
    Str,
    String,
    Array(Box<Self>),
    Struct(Arc<StructLayout>),
    Map,
    FixedArray(Box<Self>, u32),
    Delegate,
}

impl Type {
    /// Tag type id the engine compares a tag against; `None` for delegates, whose id depends on a runtime
    /// name index (unverified).
    pub fn id(&self) -> Option<u8> {
        Some(match self {
            Self::Byte => tag::BYTE,
            Self::Int => tag::INT,
            Self::Bool => tag::BOOL,
            Self::Float => tag::FLOAT,
            Self::Object | Self::Class => tag::OBJECT,
            Self::Name => tag::NAME,
            Self::String => tag::STRING,
            Self::Array(_) => tag::ARRAY,
            Self::Struct(_) => tag::STRUCT,
            Self::Str => tag::STR,
            Self::Map => tag::MAP,
            Self::FixedArray(..) => tag::FIXED_ARRAY,
            Self::Delegate => return None,
        })
    }
}

impl FieldLayout {
    pub fn is_serialized(&self) -> bool {
        self.flags & (CPF_NATIVE | CPF_TRANSIENT) == 0
    }
}

impl StructLayout {
    pub fn new(name: String, properties: Vec<FieldLayout>, super_field: i32, defaults_offset: Option<usize>) -> Self {
        let mut first_by_name = HashMap::with_capacity(properties.len());
        for (index, property) in properties.iter().enumerate() {
            first_by_name.entry(name_hash(&property.name)).or_insert(index as u32);
        }
        Self { name, properties, super_field, defaults_offset, first_by_name }
    }

    /// First property named `name` (case-insensitive). Equal names have equal hashes, so no entry means no
    /// such property, and an entry whose name matches is the first of that name.
    pub fn find(&self, name: &str) -> Option<&FieldLayout> {
        let first = &self.properties[*self.first_by_name.get(&name_hash(name))? as usize];
        if first.name.eq_ignore_ascii_case(name) {
            return Some(first);
        }
        // Another name with the same hash came first.
        self.properties.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }
}

fn name_hash(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte.to_ascii_lowercase())).wrapping_mul(0x0100_0000_01b3))
}

pub struct Layouts<D> {
    resolver: D,
    cache: HashMap<(usize, usize), Arc<StructLayout>>,
    depth: usize,
}

impl<D> Layouts<D>
where
    D: Deref + Clone,
    D::Target: Resolve,
{
    pub fn new(resolver: D) -> Self {
        Self { resolver, cache: HashMap::new(), depth: 0 }
    }

    pub fn layout(&mut self, src: Source<'_>, export: usize) -> Result<Arc<StructLayout>, Error> {
        let resolver = self.resolver.clone();
        self.layout_in(&*resolver, src, export)
    }

    pub fn class_layout(&mut self, src: Source<'_>, export: usize) -> Result<Arc<StructLayout>, Error> {
        let resolver = self.resolver.clone();
        let index = src.package.exports.get(export).map_or(0, |e| e.class_index);
        let (class_src, class_export) = reference(&*resolver, src, index)?;
        self.layout_in(&*resolver, class_src, class_export)
    }

    fn layout_in<'s>(&mut self, resolver: &'s D::Target, src: Source<'s>, export: usize) -> Result<Arc<StructLayout>, Error> {
        let key = (std::ptr::from_ref(src.package) as usize, export);
        if let Some(layout) = self.cache.get(&key) {
            return Ok(Arc::clone(layout));
        }
        self.enter(src, export)?;
        let layout = self.build(resolver, src, export);
        self.depth -= 1;
        let layout = Arc::new(layout?);
        self.cache.insert(key, Arc::clone(&layout));
        Ok(layout)
    }

    fn enter(&mut self, src: Source<'_>, export: usize) -> Result<(), Error> {
        if self.depth >= MAX_DEPTH {
            let offset = src.package.exports.get(export).map_or(0, |e| e.serial_offset);
            return Err(Error::TooDeep { limit: MAX_DEPTH, offset });
        }
        self.depth += 1;
        Ok(())
    }

    fn build<'s>(&mut self, resolver: &'s D::Target, src: Source<'s>, export: usize) -> Result<StructLayout, Error> {
        let object = read_export(src.package, src.data, export)?;
        let Some(base) = object.as_ref().and_then(ScriptObject::as_struct) else {
            return Err(Error::WrongKind { export, expected: "struct" });
        };
        let name = src.package.name(src.package.exports[export].object_name).to_owned();
        let defaults_offset = match &object {
            Some(ScriptObject::Class(class)) => Some(class.defaults_offset),
            _ => None,
        };
        let mut properties = Vec::new();
        let mut child = base.children;
        let max_steps = src.package.exports.len() + src.package.imports.len();
        let mut steps = 0;
        while child != 0 {
            if steps >= max_steps {
                let offset = src.package.exports[export].serial_offset;
                return Err(p2_package::Error::Invalid { what: "children chain", offset }.into());
            }
            steps += 1;
            let (child_src, child_export) = reference(resolver, src, child)?;
            let (next, property) = read_child(child_src.package, child_src.data, child_export)?;
            if let Some(property) = property {
                properties.push(FieldLayout {
                    name: Arc::from(child_src.package.name(child_src.package.exports[child_export].object_name)),
                    array_dim: u32::try_from(property.array_dim).unwrap_or(0),
                    flags: property.flags,
                    ty: self.ty(resolver, child_src, property.kind)?,
                });
            }
            child = next;
        }
        if base.field.super_field != 0 {
            let (super_src, super_export) = reference(resolver, src, base.field.super_field)?;
            let parent = self.layout_in(resolver, super_src, super_export)?;
            properties.extend(parent.properties.iter().cloned());
        }
        Ok(StructLayout::new(name, properties, base.field.super_field, defaults_offset))
    }

    fn ty<'s>(&mut self, resolver: &'s D::Target, src: Source<'s>, kind: PropertyKind) -> Result<Type, Error> {
        Ok(match kind {
            PropertyKind::Byte { .. } => Type::Byte,
            PropertyKind::Int => Type::Int,
            PropertyKind::Bool => Type::Bool,
            PropertyKind::Float => Type::Float,
            PropertyKind::Object { .. } => Type::Object,
            PropertyKind::Class { .. } => Type::Class,
            PropertyKind::Name => Type::Name,
            PropertyKind::Str => Type::Str,
            PropertyKind::String => Type::String,
            PropertyKind::Array { inner } => Type::Array(Box::new(self.inner_ty(resolver, src, inner)?)),
            PropertyKind::Struct { struct_ } => {
                let (struct_src, struct_export) = reference(resolver, src, struct_)?;
                Type::Struct(self.layout_in(resolver, struct_src, struct_export)?)
            }
            PropertyKind::Map { .. } => Type::Map,
            PropertyKind::FixedArray { inner, count } => {
                Type::FixedArray(Box::new(self.inner_ty(resolver, src, inner)?), u32::try_from(count).unwrap_or(0))
            }
            PropertyKind::Delegate { .. } => Type::Delegate,
        })
    }

    fn inner_ty<'s>(&mut self, resolver: &'s D::Target, src: Source<'s>, inner: i32) -> Result<Type, Error> {
        let (inner_src, inner_export) = reference(resolver, src, inner)?;
        let Some(ScriptObject::Property(property)) = read_export(inner_src.package, inner_src.data, inner_export)? else {
            return Err(Error::WrongKind { export: inner_export, expected: "property" });
        };
        // An inner property naming itself recurses here without passing through `layout_in`.
        self.enter(inner_src, inner_export)?;
        let ty = self.ty(resolver, inner_src, property.kind);
        self.depth -= 1;
        ty
    }

}

pub(crate) fn reference<'s, R: Resolve + ?Sized>(resolver: &'s R, src: Source<'s>, index: i32) -> Result<(Source<'s>, usize), Error> {
    match index {
        1.. => Ok((src, index as usize - 1)),
        0 => Err(Error::Unresolved { index }),
        _ => resolver.resolve_import(src.package, index.unsigned_abs() as usize - 1).ok_or(Error::Unresolved { index }),
    }
}
