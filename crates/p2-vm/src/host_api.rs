use p2_package::{Loader, Package};
use p2_script::{Objects, Source};
use crate::{
    ClassId, Error, LoaderRef, ObjectId, PropertyBlock, PropertyName, Slot, Value, Vm, interp, native_id, natives,
    props,
};

impl<L: LoaderRef> Vm<L> {
    pub fn property_slot(&self, class: ClassId, name: &str) -> Option<Slot> {
        self.store.property_index(class, name).map(Slot)
    }

    /// For a reader that asks the same names in the same order every time: later runs read the i-th name asked by the i-th
    /// recorded slot, without hashing, after checking it is that name. The values are those of [`ObjectStore::get`].
    pub fn read_ordered<R>(&self, object: ObjectId, reader: &'static str, f: impl FnOnce(&dyn Fn(&str) -> Option<Value>) -> R) -> R {
        let Some(class) = self.store.object(object).class else {
            return f(&|name: &str| self.store.get(object, name));
        };
        let recorded = self.script.slot_lists.lock().ok().and_then(|lists| lists.get(&(class, reader)).cloned());
        let ordered = props::Ordered::new(recorded.as_deref());
        let result =
            f(&|name: &str| ordered.slot(name, |name| self.property_slot(class, name)).and_then(|slot| self.store.get(object, slot)));
        if let Some(list) = ordered.into_recorded()
            && let Ok(mut lists) = self.script.slot_lists.lock()
        {
            lists.insert((class, reader), list);
        }
        result
    }

    #[inline]
    pub(crate) fn members<const N: usize>(&self, object: ObjectId, property: impl PropertyName) -> Option<[Value; N]> {
        self.store.members::<N>(object, property)
    }

    pub fn property_block(&self, object: ObjectId) -> &PropertyBlock {
        &self.store.object(object).mem
    }

    pub fn export_class(&self, path: &str, wanted: &str) -> Option<Option<String>> {
        let package = path.split('.').next()?;
        self.loader.loaded_id(package)?;
        let by_class = (!wanted.is_empty())
            .then(|| {
                std::iter::once(wanted)
                    .chain(interp::intrinsic_subclasses(wanted))
                    .find_map(|class| self.loader.loaded_export_by_path_of_class(path, class))
            })
            .flatten();
        Some(by_class.or_else(|| self.loader.loaded_export_by_path(path)).map(|(id, export)| {
            let pkg = &self.loader.package(id).package;
            pkg.object_name(pkg.exports[export].class_index).unwrap_or("Class").to_owned()
        }))
    }

    pub fn call_native(&mut self, name: &str, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        natives::call(self, name, native_id::Native::from_name(name), this, args)
    }

    pub fn texture_size(&mut self, texture: ObjectId) -> [i32; 2] {
        if let Some(&size) = self.media.texture_sizes.get(&texture) {
            return size;
        }
        let phase = self.profile_start();
        let path = self.store.object(texture).path.clone();
        let size_of = |loader: &Loader, package: &Package, data: &[u8], export: usize| {
            let texture = p2_assets::read_texture(&mut Objects::new(loader), Source { package, data }, export).ok()?;
            texture.mips.first().map(|mip| [mip.width as i32, mip.height as i32])
        };
        let size = match self.export_of(texture) {
            Some((package, export)) => {
                let loaded = self.loader.package(package);
                size_of(&self.loader, &loaded.package, &loaded.data, export)
            }
            None => self.side_export(&path).and_then(|(side, export)| size_of(&self.loader, &side.package, &side.data, export)),
        }
        .unwrap_or([0, 0]);
        self.media.texture_sizes.insert(texture, size);
        self.profile_phase("load: texture size", phase);
        size
    }

    pub fn sound_file(&mut self, path: &str) -> Option<p2_assets::Sound> {
        if let Some((package, export)) = self.loader.loaded_export_by_path(path) {
            let loaded = self.loader.package(package);
            return p2_assets::read_sound(&loaded.package, &loaded.data, &loaded.package.exports[export]).ok();
        }
        let (side, export) = self.side_export(path)?;
        p2_assets::read_sound(&side.package, &side.data, &side.package.exports[export]).ok()
    }

    pub fn sound_duration(&mut self, sound: ObjectId) -> Option<f32> {
        if let Some(&duration) = self.media.sound_durations.get(&sound) {
            return duration;
        }
        let phase = self.profile_start();
        let path = self.store.object(sound).path.clone();
        let duration = |pkg: &Package, data: &[u8], export: usize| p2_assets::read_sound_duration(pkg, data, &pkg.exports[export]).ok()?;
        let found = match self.export_of(sound) {
            Some((package, export)) => {
                let loaded = self.loader.package(package);
                duration(&loaded.package, &loaded.data, export)
            }
            None => self.side_sound_duration(&path),
        };
        self.media.sound_durations.insert(sound, found);
        self.profile_phase("load: sound duration", phase);
        found
    }

    pub fn class_package(&self, name: &str) -> Option<String> {
        self.loader.package_names().filter_map(|package| self.loader.loaded_id(package)).find_map(|id| {
            let loaded = self.loader.package(id);
            let pkg = &loaded.package;
            pkg.exports
                .iter()
                .any(|e| e.class_index == 0 && pkg.name(e.object_name).eq_ignore_ascii_case(name))
                .then(|| loaded.name.clone())
        })
    }
}
