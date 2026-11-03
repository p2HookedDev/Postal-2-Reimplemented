//! An object's properties as loaded: its class's defaults, then its saved tags over them. The two
//! index different packages, so every value is found together with the package it indexes.

use std::collections::HashMap;
use std::sync::Arc;

use p2_package::Package;

use crate::layout::{Layouts, Resolve, Source, reference};
use crate::value::{Properties, StoredValue, read_properties};
use crate::Error;

#[derive(Debug, Clone)]
pub struct Layer<'a> {
    pub package: &'a Package,
    pub properties: Properties,
}

/// Config values are not included.
#[derive(Debug, Clone)]
pub struct ObjectProperties<'a> {
    pub defaults: Arc<[Arc<Layer<'a>>]>,
    pub saved: Layer<'a>,
    pub native_offset: usize,
}

impl<'a> ObjectProperties<'a> {
    /// Element `index` of property `name` (case-insensitive) from the last layer that sets it, with that layer's
    /// package. A tagged struct (all but `Vector`, `Rotator`, `Color`) loads member by member over the value
    /// below it: read its members with [`ObjectProperties::member`].
    pub fn get(&self, name: &str, index: u32) -> Option<(&'a Package, &StoredValue)> {
        self.layers().rev().find_map(|layer| find(&layer.properties, name, index).map(|value| (layer.package, value)))
    }

    pub fn member(&self, name: &str, index: u32, member: &str) -> Option<(&'a Package, &StoredValue)> {
        self.layers().rev().find_map(|layer| match find(&layer.properties, name, index)? {
            StoredValue::Struct(members) => find(members, member, 0).map(|value| (layer.package, value)),
            _ => None,
        })
    }

    /// Bool property `name` (element 0). Here and below a value no layer sets reads as zero, like the engine's
    /// zeroed memory.
    pub fn bool(&self, name: &str) -> bool {
        matches!(self.get(name, 0), Some((_, StoredValue::Bool(true))))
    }

    pub fn byte(&self, name: &str) -> u8 {
        match self.get(name, 0) {
            Some((_, &StoredValue::Byte(value))) => value,
            _ => 0,
        }
    }

    pub fn int(&self, name: &str) -> i32 {
        match self.get(name, 0) {
            Some((_, &StoredValue::Int(value))) => value,
            _ => 0,
        }
    }

    pub fn float(&self, name: &str) -> f32 {
        match self.get(name, 0) {
            Some((_, &StoredValue::Float(value))) => value,
            _ => 0.0,
        }
    }

    pub fn object(&self, name: &str) -> Option<(&'a Package, i32)> {
        match self.get(name, 0) {
            Some((package, &StoredValue::Object(reference))) if reference != 0 => Some((package, reference)),
            _ => None,
        }
    }

    /// Object reference `name` unless `None`, when it indexes `package` (a class default would index the class's).
    pub fn object_in(&self, name: &str, package: &Package) -> Option<i32> {
        self.object(name).filter(|&(found, _)| std::ptr::eq(found, package)).map(|(_, reference)| reference)
    }

    pub fn vector(&self, name: &str) -> [f32; 3] {
        ["X", "Y", "Z"].map(|member| match self.member(name, 0, member) {
            Some((_, &StoredValue::Float(value))) => value,
            _ => 0.0,
        })
    }

    pub fn rotator(&self, name: &str) -> [i32; 3] {
        ["Pitch", "Yaw", "Roll"].map(|member| match self.member(name, 0, member) {
            Some((_, &StoredValue::Int(value))) => value,
            _ => 0,
        })
    }

    pub fn color(&self, name: &str) -> [u8; 4] {
        ["R", "G", "B", "A"].map(|member| match self.member(name, 0, member) {
            Some((_, &StoredValue::Byte(value))) => value,
            _ => 0,
        })
    }

    fn layers(&self) -> impl DoubleEndedIterator<Item = &Layer<'a>> {
        self.defaults.iter().map(|layer| &**layer).chain(std::iter::once(&self.saved))
    }
}

fn find<'v>(properties: &'v Properties, name: &str, index: u32) -> Option<&'v StoredValue> {
    properties.values.iter().find(|p| p.array_index == index && p.name.eq_ignore_ascii_case(name)).map(|p| &p.value)
}

pub struct Objects<'a, R: ?Sized> {
    resolver: &'a R,
    layouts: Layouts<&'a R>,
    defaults: HashMap<(usize, usize), Arc<[Arc<Layer<'a>>]>>,
}

impl<'a, R: Resolve + ?Sized> Objects<'a, R> {
    pub fn new(resolver: &'a R) -> Self {
        Self { resolver, layouts: Layouts::new(resolver), defaults: HashMap::new() }
    }

    /// Properties of object `export` of `src` (not of a class: classes save no tags of their own).
    pub fn read(&mut self, src: Source<'a>, export: usize) -> Result<ObjectProperties<'a>, Error> {
        let Some(entry) = src.package.exports.get(export) else {
            return Err(Error::WrongKind { export, expected: "export" });
        };
        let (class_src, class_export) = reference(self.resolver, src, entry.class_index)?;
        let defaults = self.defaults(class_src, class_export)?;
        let layout = self.layouts.layout(class_src, class_export)?;
        let mut r = src.package.export_reader(src.data, entry)?;
        let properties = read_properties(src.package, &mut r, &layout)?;
        Ok(ObjectProperties { defaults, saved: Layer { package: src.package, properties }, native_offset: r.pos() })
    }

    fn defaults(&mut self, src: Source<'a>, export: usize) -> Result<Arc<[Arc<Layer<'a>>]>, Error> {
        let key = (std::ptr::from_ref(src.package) as usize, export);
        if let Some(layers) = self.defaults.get(&key) {
            return Ok(Arc::clone(layers));
        }
        // The layout first: it fails on a superclass cycle before the recursion below.
        let layout = self.layouts.layout(src, export)?;
        let Some(defaults_offset) = layout.defaults_offset else {
            return Err(Error::WrongKind { export, expected: "class" });
        };
        let mut layers = match layout.super_field {
            0 => Vec::new(),
            index => {
                let (super_src, super_export) = reference(self.resolver, src, index)?;
                self.defaults(super_src, super_export)?.to_vec()
            }
        };
        let mut r = src.package.export_reader(src.data, &src.package.exports[export])?;
        r.seek(defaults_offset)?;
        layers.push(Arc::new(Layer { package: src.package, properties: read_properties(src.package, &mut r, &layout)? }));
        let layers: Arc<[Arc<Layer<'a>>]> = layers.into();
        self.defaults.insert(key, Arc::clone(&layers));
        Ok(layers)
    }
}

#[cfg(test)]
mod tests {
    use p2_package::Summary;

    use super::*;
    use crate::PropertyValue;

    fn package() -> Package {
        let summary = Summary {
            version: 128,
            licensee: 0,
            package_flags: 0,
            name_count: 0,
            name_offset: 0,
            export_count: 0,
            export_offset: 0,
            import_count: 0,
            import_offset: 0,
            guid: [0; 16],
            generations: Vec::new(),
        };
        Package { summary, names: Vec::new(), imports: Vec::new(), exports: Vec::new() }
    }

    fn tags(values: &[(&str, u32, StoredValue)]) -> Properties {
        let values = values
            .iter()
            .map(|(name, array_index, value)| PropertyValue { name: Arc::from(*name), array_index: *array_index, value: value.clone() })
            .collect();
        Properties { values, skipped: Vec::new() }
    }

    #[test]
    fn saved_tags_override_defaults_by_element_and_struct_member() {
        let (class_pkg, object_pkg) = (package(), package());
        let range = |min: f32, max: Option<f32>| {
            let mut members = vec![("Min", 0, StoredValue::Float(min))];
            members.extend(max.map(|max| ("Max", 0, StoredValue::Float(max))));
            StoredValue::Struct(tags(&members))
        };
        let root = Layer { package: &class_pkg, properties: tags(&[("Format", 0, StoredValue::Byte(3)), ("Skins", 1, StoredValue::Object(-4))]) };
        let class = Layer { package: &class_pkg, properties: tags(&[("bMasked", 0, StoredValue::Bool(true)), ("Range", 0, range(1.0, Some(2.0)))]) };
        let saved = Layer {
            package: &object_pkg,
            properties: tags(&[("format", 0, StoredValue::Byte(5)), ("Skins", 0, StoredValue::Object(7)), ("Range", 0, range(9.0, None))]),
        };
        let object = ObjectProperties { defaults: vec![Arc::new(root), Arc::new(class)].into(), saved, native_offset: 0 };

        let at = |found: Option<(&Package, &StoredValue)>| found.map(|(pkg, value)| (std::ptr::eq(pkg, &object_pkg), value.clone()));
        assert_eq!(at(object.get("Format", 0)), Some((true, StoredValue::Byte(5))));
        assert_eq!(at(object.get("bMasked", 0)), Some((false, StoredValue::Bool(true))));
        assert_eq!(at(object.get("Skins", 0)), Some((true, StoredValue::Object(7))));
        assert_eq!(at(object.get("Skins", 1)), Some((false, StoredValue::Object(-4))));
        assert_eq!(at(object.get("Missing", 0)), None);
        assert_eq!(at(object.member("Range", 0, "Min")), Some((true, StoredValue::Float(9.0))));
        assert_eq!(at(object.member("Range", 0, "Max")), Some((false, StoredValue::Float(2.0))));
        assert_eq!((object.byte("Format"), object.bool("bMasked"), object.float("Missing")), (5, true, 0.0));
        assert!(object.object("Skins").is_some_and(|(pkg, reference)| std::ptr::eq(pkg, &object_pkg) && reference == 7));
    }
}
