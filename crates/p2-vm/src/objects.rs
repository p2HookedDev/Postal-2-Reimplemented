use std::sync::Arc;
use p2_package::PackageId;
use p2_script::read_properties;
use crate::{ClassId, Error, LoaderRef, Object, ObjectId, Value, Vm, class_export_index, editor_only, interp, memory};

impl<L: LoaderRef> Vm<L> {
    pub fn find_class(&mut self, package: &str, name: &str) -> Result<ClassId, Error> {
        let id = self.loader.loaded_id(package).ok_or_else(|| Error::PackageNotLoaded(package.to_owned()))?;
        if !self.store.class_exports.contains_key(&id) {
            let phase = self.profile_start();
            let pkg = &self.loader.package(id).package;
            let index = class_export_index(pkg.exports.iter().map(|e| (e.class_index, pkg.name(e.object_name))));
            self.store.class_exports.insert(id, index);
            self.profile_phase("load: class index", phase);
        }
        let export = self
            .store
            .class_exports
            .get(&id)
            .and_then(|index| index.get(&name.to_ascii_lowercase()).copied())
            .ok_or_else(|| Error::ClassNotFound(format!("{package}.{name}")))?;
        self.load_class(id, export)
    }

    pub fn redirect_class(&mut self, from: (&str, &str), to: (&str, &str)) -> bool {
        let Ok(to) = self.find_class(to.0, to.1) else { return false };
        let Ok(from) = self.find_class(from.0, from.1) else { return false };
        self.store.class_redirects.push((from, to));
        true
    }

    pub(crate) fn redirected(&self, class: ClassId) -> ClassId {
        self.store.class_redirects.iter().find(|(from, _)| *from == class).map_or(class, |&(_, to)| to)
    }

    pub fn new_object(&mut self, class: ClassId, name: Option<&str>) -> ObjectId {
        let (name, key) = match name {
            Some(name) => (name.to_owned(), format!("transient.{name}").to_ascii_lowercase()),
            None => loop {
                let counter = self.store.name_counters.entry(class).or_insert(0);
                let candidate = format!("{}{counter}", self.store.names[self.store.classes[class.0 as usize].name.0 as usize]);
                *counter += 1;
                let key = format!("transient.{candidate}").to_ascii_lowercase();
                if !self.store.object_ids.contains_key(&key) {
                    break (candidate, key);
                }
            },
        };
        let object = Object {
            name: self.intern(&name),
            class_name: self.store.class(class).name,
            path: format!("Transient.{name}"),
            class: Some(class),
            mem: self.store.class(class).defaults.clone(),
        };
        let id = ObjectId(self.store.objects.len() as u32);
        self.store.objects.push(object);
        self.store.object_ids.insert(key, id);
        self.set_header(id, class);
        id
    }

    /// Copies a template, as retail makes an instanced subobject; `template` itself for a stub,
    /// which has no class to make one of.
    pub(crate) fn duplicate_object(&mut self, template: ObjectId) -> ObjectId {
        let Some(class) = self.store.object(template).class else { return template };
        let copy = self.new_object(class, None);
        let mem = self.store.object(template).mem.clone();
        self.store.objects[copy.0 as usize].mem = mem;
        self.set_header(copy, class);
        copy
    }

    /// Written at `Object`'s own slots, the last of their name: a subclass may declare a `Name` of its own
    /// (`ACTION_IfGameState`'s `String Name`, `DAY_A`), which a lookup by name finds first.
    fn set_header(&mut self, id: ObjectId, class: ClassId) {
        if self.store.class(class).is_actor() {
            self.store.actor_ids.insert(id);
            self.store.dynamic_ids.added(id);
        }
        let name = self.store.object(id).name;
        if let Some(slot) = self.store.class(class).name_slot {
            self.store.set_property_at(id, slot, Value::Name(name));
        }
        let class_object = match self.store.class(class).class_object {
            Some(class_object) => class_object,
            None => {
                let class_path = format!("{}.{}", self.loader.package(self.store.class(class).package).name, self.store.name(self.store.class(class).name));
                let class_object = self.stub(&class_path, "Class");
                self.store.classes[class.0 as usize].class_object = Some(class_object);
                class_object
            }
        };
        if let Some(slot) = self.store.class(class).class_slot {
            self.store.set_property_at(id, slot, Value::Object(Some(class_object)));
        }
    }

    /// The key of the object at `path` of class `class_name` in the object table: its lower-case path, unless another
    /// object of a different class already holds that path and the package has exports of both classes under it
    /// (`ED_Weapons.ED_MP5_NEW` is a `SkeletalMesh` and a `MeshAnimation`); then the class is part of the key.
    pub(crate) fn object_key(&self, path: &str, class_name: &str) -> String {
        let key = path.to_ascii_lowercase();
        let Some(&existing) = self.store.object_ids.get(&key) else { return key };
        let existing = self.store.name(self.store.object(existing).class_name);
        if class_name.is_empty() || existing.eq_ignore_ascii_case(class_name) {
            return key;
        }
        let ours = self.loader.loaded_export_by_path_of_class(path, class_name);
        let theirs = self.loader.loaded_export_by_path_of_class(path, existing);
        match (ours, theirs) {
            (Some(ours), Some(theirs)) if ours != theirs => format!("{key}|{}", class_name.to_ascii_lowercase()),
            _ => key,
        }
    }

    pub(crate) fn export_of(&self, object: ObjectId) -> Option<(PackageId, usize)> {
        let object = self.store.object(object);
        let class = self.store.name(object.class_name);
        std::iter::once(class)
            .chain(interp::intrinsic_subclasses(class))
            .find_map(|class| self.loader.loaded_export_by_path_of_class(&object.path, class))
            .or_else(|| self.loader.loaded_export_by_path(&object.path))
    }

    pub fn stub(&mut self, path: &str, class_name: &str) -> ObjectId {
        let key = self.object_key(path, class_name);
        if let Some(&id) = self.store.object_ids.get(&key) {
            return id;
        }
        let name = path.rsplit('.').next().unwrap_or(path);
        let object = Object {
            name: self.intern(name),
            class_name: self.intern(class_name),
            path: path.to_owned(),
            class: None,
            mem: memory::Mem::default(),
        };
        let id = ObjectId(self.store.objects.len() as u32);
        self.store.objects.push(object);
        self.store.object_ids.insert(key, id);
        id
    }

    /// Assets stay stubs until a script uses them. A failed load leaves the stub (calls return zero, property reads read
    /// zero).
    pub(crate) fn load_stub(&mut self, object: ObjectId) {
        if self.store.object(object).class.is_none()
            && let Some((package, export)) = self.export_of(object)
            && self.load_object(package, export).is_err()
        {
            self.diagnostics.log.push(format!("Could not load {}", self.store.object(object).path));
        }
    }

    pub fn load_object(&mut self, package: PackageId, export: usize) -> Result<ObjectId, Error> {
        let loader = self.loader.clone();
        let loaded = loader.package(package);
        let pkg = &loaded.package;
        let entry = pkg.exports.get(export).ok_or(Error::BadExport(export))?;
        let id = self.object_ref(pkg, package, export as i32 + 1).ok_or(Error::BadExport(export))?;
        let Some((class_package, class_export)) = self.loader.loaded_export(package, entry.class_index) else {
            return Ok(id);
        };
        if self.store.object(id).class.is_some() || editor_only(pkg, export) {
            return Ok(id);
        }
        let class = self.load_class(class_package, class_export)?;
        let layout = Arc::clone(&self.store.class(class).layout);
        let mut properties = self.store.class(class).defaults.clone();
        let mut r = pkg.export_reader(&loaded.data, entry)?;
        let tags = read_properties(pkg, &mut r, &layout)?;
        self.apply(pkg, package, &layout, self.store.class(class).mem, &mut properties, tags.values);
        let object = &mut self.store.objects[id.0 as usize];
        object.class = Some(class);
        object.mem = properties;
        self.set_header(id, class);
        Ok(id)
    }
}
