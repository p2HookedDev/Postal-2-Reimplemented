use std::sync::Arc;
use p2_package::{Package, PackageId, Reader};
use rustc_hash::FxHashMap;
use p2_script::{ScriptObject, Source, StructLayout, Type, read_export, read_properties};
use crate::{
    Class, ClassId, Error, LoaderRef, MAX_CLASS_DEPTH, Object, ObjectId, Value, Vm, editor_only, lower_lookup,
    lowercase_index, memory, props, set_element, zero_slot,
};

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn load_class(&mut self, package: PackageId, export: usize) -> Result<ClassId, Error> {
        self.load_class_at(package, export, 0)
    }

    fn load_class_at(&mut self, package: PackageId, export: usize, depth: u32) -> Result<ClassId, Error> {
        if let Some(&id) = self.store.class_ids.get(&(package, export)) {
            return Ok(id);
        }
        let loader = self.loader.clone();
        let loaded = loader.package(package);
        if depth >= MAX_CLASS_DEPTH {
            return Err(Error::ClassNotFound(loaded.package.name(loaded.package.exports[export].object_name).to_owned()));
        }
        let source = Source { package: &loaded.package, data: &loaded.data };
        let Some(ScriptObject::Class(class)) = read_export(source.package, source.data, export)? else {
            return Err(Error::ClassNotFound(loaded.package.name(loaded.package.exports[export].object_name).to_owned()));
        };
        let super_class = match self.loader.loaded_export(package, class.base.base.field.super_field) {
            Some((super_package, super_export)) => Some(self.load_class_at(super_package, super_export, depth + 1)?),
            None => None,
        };
        let phase = self.profile_start();
        let layout = self.layouts.layout(source, export)?;

        // The layout lists the class's own properties first, then the superclass's (whose defaults apply). In memory
        // the superclass's come first, at the superclass's offsets, as in the engine.
        let base = super_class.map(|id| (self.store.class(id).mem, self.store.class(id).layout.properties.len()));
        let mem_layout = self.store.memory.of_class(&layout, base);
        let mut defaults = self.store.memory.zero(mem_layout);
        if let Some(id) = super_class {
            let inherited = &self.store.class(id).defaults;
            defaults.words[..inherited.words.len()].copy_from_slice(&inherited.words);
            defaults.heap[..inherited.heap.len()].clone_from_slice(&inherited.heap);
        }
        let entry = &source.package.exports[export];
        let end = (entry.serial_offset + entry.serial_size).min(source.data.len());
        let mut r = Reader::new(&source.data[..end]);
        r.seek(class.defaults_offset)?;
        let tags = read_properties(source.package, &mut r, &layout)?;
        self.apply(source.package, package, &layout, mem_layout, &mut defaults, tags.values);
        let class_path = format!("{}.{}", loaded.name, source.package.name(entry.object_name));
        let config_file = match source.package.name(class.config_name) {
            "" | "None" => "System".to_owned(),
            name => name.to_owned(),
        };
        self.load_config(class.flags, &config_file, &class_path, super_class, &layout, mem_layout, &mut defaults);

        let name = self.intern(source.package.name(entry.object_name));
        // The default object's native `Class` is the class (`Default.Class` in static functions, e.g.
        // `P2GameInfoSingle.GetStartURL`).
        let class_object = self.stub(&class_path, "Class");
        let id = ClassId(self.store.classes.len() as u32);
        let mut index = FxHashMap::default();
        for (i, field) in layout.properties.iter().enumerate() {
            index.entry(field.name.to_string()).or_insert(i);
        }
        let lower = lowercase_index(&layout);
        let slot_of = |p: &str| index.get(p).copied().or_else(|| lower_lookup(&lower, p));
        let fields = &self.store.memory.get(mem_layout).fields;
        let resolved = props::Resolved::new(slot_of, fields, self.store.name(name), super_class.map_or(0, |s| self.store.class(s).resolved.kinds()));
        let lower_index = std::sync::OnceLock::from(lower);
        let mem = mem_layout;
        let last_named = |name: &str| layout.properties.iter().rposition(|p| p.name.eq_ignore_ascii_case(name));
        let (name_slot, class_slot) = (last_named("Name"), last_named("Class"));
        self.store.classes.push(Class {
            name,
            package,
            export,
            super_class,
            layout,
            mem,
            defaults,
            index,
            lower_index,
            resolved,
            class_object: Some(class_object),
            name_slot,
            class_slot,
        });
        self.store.class_ids.insert((package, export), id);
        self.store.class_by_name.entry(name).or_insert(id);
        if let Some(index) = self.store.property_index(id, "Class") {
            let field = self.store.memory.get(mem).fields[index];
            self.store.memory.write_field(&field, &mut self.store.classes[id.0 as usize].defaults, Value::Object(Some(class_object)));
        }
        self.profile_phase("load: class", phase);
        Ok(id)
    }

    pub(crate) fn apply(
        &mut self,
        pkg: &Package,
        package: PackageId,
        layout: &StructLayout,
        mem_layout: memory::LayoutId,
        mem: &mut memory::Mem,
        tags: Vec<p2_script::PropertyValue>,
    ) {
        let indices = Arc::clone(self.script.lookups.tag_indices.entry(mem_layout).or_insert_with(|| Arc::new(lowercase_index(layout))));
        for tag in tags {
            let Some(index) = lower_lookup(&indices, &tag.name) else {
                continue;
            };
            let value = self.convert(pkg, package, &layout.properties[index].ty, tag.value);
            let field = self.store.memory.get(mem_layout).fields[index];
            self.store.memory.write_element(&field, mem, tag.array_index as usize, value);
        }
    }

    fn convert(&mut self, pkg: &Package, package: PackageId, ty: &Type, value: p2_script::StoredValue) -> Value {
        use p2_script::StoredValue as V;
        if let Type::Array(inner) = ty
            && !matches!(value, V::Array(_))
        {
            return self.convert(pkg, package, inner, value);
        }
        match value {
            V::Byte(v) => Value::Byte(v),
            V::Int(v) => Value::Int(v),
            V::Bool(v) => Value::Bool(v),
            V::Float(v) => Value::Float(v),
            V::Object(reference) if reference > 0 && editor_only(pkg, reference as usize - 1) => Value::Object(None),
            V::Object(reference) => Value::Object(self.object_ref(pkg, package, reference)),
            V::Name(name) => Value::Name(self.intern(pkg.name(name))),
            V::Str(v) => Value::Str(v),
            V::Array(values) => {
                let inner = match ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => inner.as_ref(),
                    other => other,
                };
                Value::Array(values.into_iter().map(|v| self.convert(pkg, package, inner, v)).collect())
            }
            V::Struct(members) => {
                let Type::Struct(layout) = ty else { return Value::Struct(Vec::new()) };
                let mut slots: Vec<Value> = layout.properties.iter().map(|f| zero_slot(&f.ty, f.array_dim)).collect();
                for member in members.values {
                    let Some(index) = layout.properties.iter().position(|p| p.name.eq_ignore_ascii_case(&member.name))
                    else {
                        continue;
                    };
                    let field = &layout.properties[index];
                    let value = self.convert(pkg, package, &field.ty, member.value);
                    set_element(&mut slots[index], &field.ty, field.array_dim, member.array_index, value);
                }
                Value::of_struct(layout, slots)
            }
            V::Delegate { object, function } => {
                Value::Delegate { object: self.object_ref(pkg, package, object), function: self.intern(pkg.name(function)) }
            }
            V::Map => Value::Map,
        }
    }

    pub(crate) fn object_ref(&mut self, pkg: &Package, package: PackageId, reference: i32) -> Option<ObjectId> {
        if reference == 0 {
            return None;
        }
        let mut parts = Vec::new();
        let mut current = reference;
        for _ in 0..=pkg.imports.len() + pkg.exports.len() {
            if current == 0 {
                break;
            }
            parts.push(pkg.object_name(current)?);
            current = match current {
                ..0 => pkg.imports.get(current.unsigned_abs() as usize - 1)?.package_index,
                _ => pkg.exports.get(current as usize - 1)?.package_index,
            };
        }
        let loader = self.loader.clone();
        if reference > 0 {
            parts.push(&loader.package(package).name);
        }
        parts.reverse();
        let path = parts.join(".");
        let class_name = match reference {
            ..0 => pkg.name(pkg.imports.get(reference.unsigned_abs() as usize - 1)?.class_name),
            _ => match pkg.exports.get(reference as usize - 1)?.class_index {
                0 => "Class",
                class => pkg.object_name(class)?,
            },
        };
        let key = self.object_key(&path, class_name);
        if let Some(&id) = self.store.object_ids.get(&key) {
            return Some(id);
        }
        let object = Object::new(self.intern(parts.last()?), self.intern(class_name), path, None, memory::Mem::default());
        let id = ObjectId(self.store.objects.len() as u32);
        self.store.objects.push(object);
        self.store.object_ids.insert(key, id);
        if let Some(found) = loader.resolve_loaded(package, reference)
            && let Some(export) = found.export
            && self.loads_on_reference(found.package, export)
        {
            return self.load_object(found.package, export).ok();
        }
        Some(id)
    }

    fn loads_on_reference(&mut self, package: PackageId, export: usize) -> bool {
        let class_index = self.loader.package(package).package.exports[export].class_index;
        let Some((class_package, class_export)) = self.loader.loaded_export(package, class_index) else { return false };
        let Ok(class) = self.load_class(class_package, class_export) else { return false };
        !["Actor", "Material", "Sound", "Font", "Palette", "MeshAnimation"].iter().any(|skip| self.class_is_a(class, skip))
    }
}
