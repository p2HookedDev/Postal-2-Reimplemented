use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use p2_package::PackageId;
use p2_script::{Expr, ScriptObject, Source, class_name, decode_script_linked, read_export};
use rustc_hash::{FxHashMap, FxHashSet};
use crate::{ClassId, Error, NameId, ObjectId, LoaderRef, Value, Vm};
use p2_script::flags::{CPF_PARM, CPF_RETURN_PARM};
use super::{Function, INTRINSIC_SUPER, UNLINKED, is_native_declaration};

impl<L: LoaderRef> Vm<L> {
    pub fn find_function(&mut self, class: ClassId, name: &str) -> Result<Option<Arc<Function>>, Error> {
        let name = self.intern(name);
        self.find_function_id(class, name)
    }

    pub fn find_function_id(&mut self, class: ClassId, name: NameId) -> Result<Option<Arc<Function>>, Error> {
        if let Some(found) = self.script.lookups.functions.get(&(class, None, name)) {
            return Ok(found.clone());
        }
        let text = self.store.name(name).to_owned();
        let found = self.search_function(class, &text)?;
        self.script.lookups.functions.insert((class, None, name), found.clone());
        Ok(found)
    }

    fn search_function(&mut self, class: ClassId, name: &str) -> Result<Option<Arc<Function>>, Error> {
        self.count(crate::profiling::Work::FunctionSearch);
        let mut current = Some(class);
        while let Some(class) = current {
            let (package, export) = (self.store.class(class).package, self.store.class(class).export);
            if let Some((package, export)) = self.find_child(package, export, name, false)? {
                return self.load_function(package, export).map(Some);
            }
            current = self.store.class(class).super_class;
        }
        Ok(None)
    }

    pub(crate) fn find_child(
        &mut self,
        package: PackageId,
        export: usize,
        name: &str,
        state: bool,
    ) -> Result<Option<(PackageId, usize)>, Error> {
        Ok(self.children_of(package, export)?.get(&(name.to_ascii_lowercase(), state)).copied())
    }

    pub(crate) fn children_of(&mut self, package: PackageId, export: usize) -> Result<&FxHashMap<(String, bool), (PackageId, usize)>, Error> {
        if !self.script.children.contains_key(&(package, export)) {
            let phase = self.profile_start();
            let mut children = FxHashMap::default();
            let loaded = self.loader.package(package);
            let definition = read_export(&loaded.package, &loaded.data, export)?;
            let mut child = definition.as_ref().and_then(ScriptObject::as_struct).map_or(0, |s| s.children);
            // Bounded: a malformed `next` chain may loop.
            for _ in 0..=loaded.package.imports.len() + loaded.package.exports.len() {
                let Some((child_package, child_export)) = self.loader.loaded_export(package, child) else { break };
                let child_pkg = self.loader.package(child_package);
                let object = read_export(&child_pkg.package, &child_pkg.data, child_export)?;
                let is_state = match &object {
                    Some(ScriptObject::Function(_)) => Some(false),
                    Some(ScriptObject::State(_)) => Some(true),
                    _ => None,
                };
                if let Some(is_state) = is_state {
                    let child_name = child_pkg.package.name(child_pkg.package.exports[child_export].object_name);
                    children.entry((child_name.to_ascii_lowercase(), is_state)).or_insert((child_package, child_export));
                }
                child = object.and_then(|o| o.field()).map_or(0, |f| f.next);
            }
            self.script.children.insert((package, export), children);
            self.profile_phase("load: functions of a class", phase);
        }
        Ok(&self.script.children[&(package, export)])
    }

    pub fn load_function(&mut self, package: PackageId, export: usize) -> Result<Arc<Function>, Error> {
        if let Some(function) = self.script.functions.get(&(package, export)) {
            return Ok(Arc::clone(function));
        }
        let phase = self.profile_start();
        let loaded = self.loader.package(package);
        let pkg = &loaded.package;
        let (base, native, flags) = match read_export(pkg, &loaded.data, export)? {
            Some(ScriptObject::Function(f)) => (f.base, f.native, f.flags),
            Some(ScriptObject::State(s)) => (s.base, 0, 0),
            _ => return Err(Error::BadExport(export)),
        };
        let layout = self.layouts.layout(Source { package: pkg, data: &loaded.data }, export)?;
        let (statements, links) = decode_script_linked(pkg, &base.script, base.script_size)?;
        // The layout repeats the overridden function's properties after the function's own: first names win.
        let mut seen = FxHashSet::default();
        let (mut params, mut return_value) = (Vec::new(), None);
        for (index, field) in layout.properties.iter().enumerate() {
            if !seen.insert(field.name.to_ascii_lowercase()) {
                continue;
            }
            if field.flags & CPF_RETURN_PARM != 0 {
                return_value = Some(index);
            } else if field.flags & CPF_PARM != 0 {
                params.push(index);
            }
        }
        let object_name = pkg.name(pkg.exports[export].object_name).to_owned();
        let native_id = if is_native_declaration(native, flags) { crate::native_id::Native::from_name(&object_name) } else { None };
        let mem = self.store.memory.of(&layout);
        let hook = crate::script_hooks::ScriptHook::of(&object_name, params.len());
        let mut labels = FxHashMap::default();
        for statement in &statements {
            if let Expr::LabelTable(table) = &statement.expr {
                for &(name, offset) in table {
                    if let Ok(offset) = u32::try_from(offset) {
                        labels.entry(self.package_name(package, name)).or_insert(offset);
                    }
                }
            }
        }
        let function = Arc::new(Function {
            object_name,
            package,
            export,
            layout,
            mem,
            statements,
            native,
            flags,
            params,
            return_value,
            native_id,
            fast: native_id.filter(|native| native.is_fast()),
            hook,
            links: (0..links).map(|_| AtomicU64::new(UNLINKED)).collect(),
            index: u32::try_from(self.script.lookups.function_table.len()).unwrap_or(u32::MAX),
            labels,
        });
        self.script.functions.insert((package, export), Arc::clone(&function));
        self.script.lookups.function_table.push(Arc::clone(&function));
        self.profile_phase("load: function", phase);
        Ok(function)
    }

    pub(super) fn context_class(&mut self, context: Option<ObjectId>) -> Result<Option<ClassId>, Error> {
        let Some(object) = context else { return Ok(None) };
        if let Some(class) = self.store.object(object).class {
            return Ok(Some(class));
        }
        if self.store.object(object).class_name == self.script.lookups.class_names.class {
            return self.class_of_object(object);
        }
        Ok(None)
    }

    pub fn stub_is_a(&self, object: ObjectId, class_name: &str) -> bool {
        let object = self.store.object(object);
        if object.class.is_some() {
            return false;
        }
        let mut current = self.store.name(object.class_name);
        let mut loaded = self.store.class_by_name.get(&object.class_name).copied();
        for _ in 0..16 {
            if current.eq_ignore_ascii_case(class_name) {
                return true;
            }
            if let Some(class) = loaded {
                return self.class_is_a(class, class_name);
            }
            match INTRINSIC_SUPER.iter().find(|(class, _)| class.eq_ignore_ascii_case(current)) {
                Some(&(_, parent)) => {
                    current = parent;
                    loaded = self.store.name_ids.get(&parent.to_ascii_lowercase()).and_then(|name| self.store.class_by_name.get(name)).copied();
                }
                None => return false,
            }
        }
        false
    }

    pub fn class_is_a(&self, class: ClassId, class_name: impl crate::ClassName) -> bool {
        class_name.is_base_of(&self.store, class)
    }

    pub(super) fn cast_target(&mut self, package: PackageId, class: i32) -> NameId {
        if let Some(&id) = self.script.lookups.cast_targets.get(&(package, class)) {
            return id;
        }
        let loader = self.loader.clone();
        let id = self.intern(loader.package(package).package.object_name(class).unwrap_or(""));
        self.script.lookups.cast_targets.insert((package, class), id);
        id
    }

    pub(crate) fn package_name(&mut self, package: PackageId, index: usize) -> NameId {
        if let Some(&id) = self.script.lookups.names.get(&(package, index)) {
            return id;
        }
        let text = self.loader.package(package).package.name(index).to_owned();
        let id = self.intern(&text);
        self.script.lookups.names.insert((package, index), id);
        id
    }

    pub(super) fn reference_property(&self, class: ClassId, package: PackageId, reference: i32) -> Option<usize> {
        self.store.property_index(class, self.loader.package(package).package.object_name(reference).unwrap_or(""))
    }

    pub(super) fn stub_member(&mut self, object: ObjectId, package: PackageId, reference: i32) -> Value {
        let name = self.loader.package(package).package.object_name(reference).unwrap_or("");
        if name.eq_ignore_ascii_case("Name") {
            return Value::Name(self.store.object(object).name);
        }
        match ["USize", "UClamp", "VSize", "VClamp"].iter().position(|member| name.eq_ignore_ascii_case(member)) {
            Some(member) => Value::Int(self.texture_size(object)[member / 2]),
            None => Value::Int(0),
        }
    }

    pub(super) fn native_function(&mut self, index: u16) -> Option<(PackageId, usize)> {
        if self.script.lookups.native_declarations.is_empty() {
            for id in self.loader.package_names().filter_map(|name| self.loader.loaded_id(name)) {
                let loaded = self.loader.package(id);
                if !loaded.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("u")) {
                    continue;
                }
                for export in 0..loaded.package.exports.len() {
                    if class_name(&loaded.package, &loaded.package.exports[export]) == "Function"
                        && let Ok(Some(ScriptObject::Function(f))) = read_export(&loaded.package, &loaded.data, export)
                        && f.native != 0
                    {
                        self.script.lookups.native_declarations.insert(f.native, (id, export));
                    }
                }
            }
        }
        self.script.lookups.native_declarations.get(&index).copied()
    }
}
