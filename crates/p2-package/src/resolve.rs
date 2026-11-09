use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::caseless::{Caseless, Text, caseless_cmp};
use crate::flags::RF_PUBLIC;
use crate::{Error, Export, Ini, Package, split_path};

const MAX_IMPORT_DEPTH: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{path}: {source}")]
    Parse { path: PathBuf, source: Error },
    #[error("package {0} not found in the search paths")]
    PackageNotFound(String),
    #[error("{class} {path} not found")]
    ObjectNotFound { class: String, path: String },
    #[error("{class} {path} is not public")]
    Private { class: String, path: String },
    #[error("malformed import {0}")]
    BadImport(String),
    #[error("object reference {0} out of range")]
    BadReference(i32),
}

/// Export indices sorted by name, any case; every call takes the package it was built from.
#[derive(Debug)]
pub struct ExportsByName(Box<[usize]>);

impl ExportsByName {
    pub fn new(package: &Package) -> Self {
        let names: Vec<_> = package.exports.iter().map(|export| package.name(export.object_name)).collect();
        let mut order: Vec<usize> = (0..package.exports.len()).collect();
        order.sort_by(|&a, &b| caseless_cmp(names[a], names[b]));
        Self(order.into_boxed_slice())
    }

    /// The exports named `name`, ascending.
    pub fn get(&self, package: &Package, name: &str) -> &[usize] {
        let order_of = |index: &usize| caseless_cmp(package.name(package.exports[*index].object_name), name);
        let start = self.0.partition_point(|index| order_of(index).is_lt());
        let len = self.0[start..].partition_point(|index| order_of(index).is_eq());
        &self.0[start..start + len]
    }

    pub fn export_by_path(&self, package: &Package, path: &str) -> Option<usize> {
        let (outers, name) = split_path(path);
        package.export_among(self.get(package, name).iter().copied(), outers)
    }
}

pub fn map_file(path: &Path) -> std::io::Result<memmap2::Mmap> {
    let file = std::fs::File::open(path)?;
    #[expect(unsafe_code, reason = "package files memory-mapped")]
    // SAFETY: the map is read-only and the engine never writes game files; another process rewriting or
    // truncating one during play would fault, which we accept.
    unsafe { memmap2::Mmap::map(&file) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PackageId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub package: PackageId,
    pub export: Option<usize>,
}

#[derive(Debug)]
pub struct LoadedPackage {
    pub name: String,
    pub path: PathBuf,
    pub package: Package,
    pub data: memmap2::Mmap,
    exports_by_name: ExportsByName,
}

/// Finds packages by name in the search paths, loads them on demand, and resolves imports. A clone shares the loaded
/// packages and copies the resolved-import cache: a clone that loads more packages keeps every [`PackageId`] of the original.
#[derive(Debug, Default, Clone)]
pub struct Loader {
    files: FxHashMap<Caseless, PathBuf>,
    packages: Vec<Arc<LoadedPackage>>,
    ids: FxHashMap<Caseless, PackageId>,
    ids_by_address: FxHashMap<usize, PackageId>,
    /// Successes only: a package's imports never change and ids only grow, while a failure may resolve after a later load.
    resolved_imports: Vec<Box<[OnceLock<Resolved>]>>,
}

pub fn search_paths(ini: &str) -> Vec<String> {
    Ini::parse(ini).get_all("Core.System", "Paths").map(str::to_owned).collect()
}

impl Loader {
    pub fn open(system_dir: &Path) -> Result<Self, ResolveError> {
        Self::open_with(system_dir, &[])
    }

    /// [`Loader::open`] with `extra` search paths after the ini's (e.g. `Packages\*.u`, our own packages
    /// outside the install). The install wins a name clash: a file outside it never replaces a retail package.
    pub fn open_with(system_dir: &Path, extra: &[String]) -> Result<Self, ResolveError> {
        let ini = ["Postal2.ini", "Default.ini"]
            .iter()
            .map(|name| system_dir.join(name))
            .find(|path| path.is_file())
            .unwrap_or_else(|| system_dir.join("Default.ini"));
        let text = std::fs::read(&ini).map_err(|source| ResolveError::Io { path: ini, source })?;
        let mut paths = search_paths(&String::from_utf8_lossy(&text));
        paths.extend_from_slice(extra);
        Self::new(system_dir, &paths)
    }

    pub fn new(system_dir: &Path, search_paths: &[String]) -> Result<Self, ResolveError> {
        let mut files = FxHashMap::default();
        for pattern in search_paths {
            let Some((dir, ext)) = pattern.split_once('*') else { continue };
            let dir = system_dir.join(dir.replace('\\', "/"));
            let ext = ext.to_ascii_lowercase();
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(ResolveError::Io { path: dir, source }),
            };
            for entry in entries {
                let entry = entry.map_err(|source| ResolveError::Io { path: dir.clone(), source })?;
                let Some(file_name) = entry.file_name().to_str().map(str::to_ascii_lowercase) else { continue };
                if let Some(name) = file_name.strip_suffix(&ext) {
                    files.entry(Caseless(name.into())).or_insert_with(|| entry.path());
                }
            }
        }
        Ok(Self { files, ..Self::default() })
    }

    pub fn package_names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(|key| &*key.0)
    }

    pub fn find(&self, name: &str) -> Option<&Path> {
        self.files.get(&name as &dyn Text).map(PathBuf::as_path)
    }

    pub fn load(&mut self, name: &str) -> Result<PackageId, ResolveError> {
        if let Some(id) = self.loaded_id(name) {
            return Ok(id);
        }
        let (key, path) = self.files.get_key_value(&name as &dyn Text).ok_or_else(|| ResolveError::PackageNotFound(name.to_owned()))?;
        let (key, path) = (key.clone(), path.clone());
        let data = map_file(&path).map_err(|source| ResolveError::Io { path: path.clone(), source })?;
        let package = Package::parse(&data).map_err(|source| ResolveError::Parse { path: path.clone(), source })?;

        let exports_by_name = ExportsByName::new(&package);
        let name = path.file_stem().map_or_else(|| name.to_owned(), |s| s.to_string_lossy().into_owned());
        Ok(self.push(key, Arc::new(LoadedPackage { name, path, package, data, exports_by_name })))
    }

    fn push(&mut self, key: Caseless, loaded: Arc<LoadedPackage>) -> PackageId {
        let id = PackageId(self.packages.len());
        self.ids_by_address.insert(std::ptr::from_ref(&loaded.package) as usize, id);
        self.resolved_imports.push(loaded.package.imports.iter().map(|_| OnceLock::new()).collect());
        self.packages.push(loaded);
        self.ids.insert(key, id);
        id
    }

    pub fn load_scripts(&mut self) -> Result<(), ResolveError> {
        let is_script = |path: &PathBuf| path.extension().is_some_and(|e| e.eq_ignore_ascii_case("u"));
        let names: Vec<Caseless> = self.files.iter().filter(|(_, path)| is_script(path)).map(|(n, _)| n.clone()).collect();
        for name in names {
            self.load(&name.0)?;
        }
        Ok(())
    }

    pub fn package(&self, id: PackageId) -> &LoadedPackage {
        &self.packages[id.0]
    }

    pub fn resolve(&mut self, id: PackageId, reference: i32) -> Result<Option<Resolved>, ResolveError> {
        match reference {
            0 => Ok(None),
            1.. if (reference as usize) <= self.package(id).package.exports.len() => {
                Ok(Some(Resolved { package: id, export: Some(reference as usize - 1) }))
            }
            ..0 => self.resolve_import(id, reference.unsigned_abs() as usize - 1).map(Some),
            _ => Err(ResolveError::BadReference(reference)),
        }
    }

    pub fn resolve_import(&mut self, id: PackageId, index: usize) -> Result<Resolved, ResolveError> {
        if let Some(&resolved) = self.resolved_imports[id.0].get(index).and_then(OnceLock::get) {
            return Ok(resolved);
        }
        let top = self.top_package(id, index)?;
        let loaded = Arc::clone(&self.packages[id.0]);
        self.load(loaded.package.name(top))?;
        self.resolve_loaded_import(id, index, 0)
    }

    pub fn resolve_loaded(&self, id: PackageId, reference: i32) -> Option<Resolved> {
        match reference {
            1.. if (reference as usize) <= self.package(id).package.exports.len() => {
                Some(Resolved { package: id, export: Some(reference as usize - 1) })
            }
            ..=-1 => self.resolve_loaded_import(id, reference.unsigned_abs() as usize - 1, 0).ok(),
            _ => None,
        }
    }

    pub fn export_by_path(&mut self, path: &str) -> Result<Option<(PackageId, usize)>, ResolveError> {
        self.load_path_package(path)?;
        Ok(self.find_loaded_export(path, None))
    }

    /// A package may hold objects of different classes under one name (`ED_Weapons.ED_Chainsaw_NEW` is a
    /// mesh and its `MeshAnimation`), so the class tells them apart.
    pub fn export_by_path_of_class(&mut self, path: &str, class: &str) -> Result<Option<(PackageId, usize)>, ResolveError> {
        self.load_path_package(path)?;
        Ok(self.find_loaded_export(path, Some(class)))
    }

    fn load_path_package(&mut self, path: &str) -> Result<(), ResolveError> {
        if let Some((package, _)) = path.split_once('.') {
            self.load(package)?;
        }
        Ok(())
    }

    pub fn loaded_export_by_path(&self, path: &str) -> Option<(PackageId, usize)> {
        self.find_loaded_export(path, None)
    }

    pub fn loaded_export_by_path_of_class(&self, path: &str, class: &str) -> Option<(PackageId, usize)> {
        self.find_loaded_export(path, Some(class))
    }

    fn find_loaded_export(&self, path: &str, class: Option<&str>) -> Option<(PackageId, usize)> {
        let (package, rest) = path.split_once('.')?;
        let id = self.loaded_id(package)?;
        let loaded = self.package(id);
        let pkg = &loaded.package;
        let (outers, name) = split_path(rest);
        let of_class = |index: &usize| {
            class.is_none_or(|class| pkg.object_name(pkg.exports[*index].class_index).unwrap_or("Class").eq_ignore_ascii_case(class))
        };
        let candidates = loaded.exports_by_name.get(pkg, name).iter().copied().filter(of_class);
        pkg.export_among(candidates, outers).map(|index| (id, index))
    }

    pub fn load_imports(&mut self, id: PackageId) -> Result<(), ResolveError> {
        self.load_imports_with(id, |_, _| {})
    }

    pub fn load_imports_with(&mut self, id: PackageId, mut progress: impl FnMut(usize, usize)) -> Result<(), ResolveError> {
        let mut pending = vec![id];
        let mut reached: FxHashSet<PackageId> = [id].into_iter().collect();
        let mut done = 0;
        while let Some(id) = pending.pop() {
            done += 1;
            progress(done, done + pending.len());
            let pkg = &self.package(id).package;
            let names: Vec<String> = pkg
                .imports
                .iter()
                .filter(|import| import.package_index == 0)
                .map(|import| pkg.name(import.object_name).to_owned())
                .filter(|name| self.find(name).is_some())
                .collect();
            for name in names {
                let next = self.load(&name)?;
                if reached.insert(next) {
                    pending.push(next);
                }
            }
        }
        Ok(())
    }

    pub fn load_imported(&mut self, classes: &[&str]) -> Result<usize, ResolveError> {
        let (mut scanned, mut count) = (0, 0);
        while scanned < self.packages.len() {
            let mut wanted: Vec<String> = Vec::new();
            for index in scanned..self.packages.len() {
                let pkg = &self.packages[index].package;
                for (i, import) in pkg.imports.iter().enumerate() {
                    if classes.iter().any(|c| pkg.name(import.class_name).eq_ignore_ascii_case(c))
                        && let Ok(name) = self.top_package(PackageId(index), i).map(|top| pkg.name(top))
                        && self.loaded_id(name).is_none()
                        && self.find(name).is_some()
                        && !wanted.iter().any(|known| known.eq_ignore_ascii_case(name))
                    {
                        wanted.push(name.to_owned());
                    }
                }
            }
            scanned = self.packages.len();
            for name in wanted {
                self.load(&name)?;
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn adopt(&mut self, other: &Self) {
        let mut added: Vec<(&Caseless, PackageId)> = other.ids.iter().filter(|(key, _)| !self.ids.contains_key(*key)).map(|(k, &id)| (k, id)).collect();
        added.sort_by_key(|(_, id)| id.0);
        for (key, id) in added {
            self.push(key.clone(), Arc::clone(&other.packages[id.0]));
        }
    }

    pub fn loaded_id(&self, name: &str) -> Option<PackageId> {
        self.ids.get(&name as &dyn Text).copied()
    }

    pub fn id_of(&self, package: &Package) -> Option<PackageId> {
        self.ids_by_address.get(&(std::ptr::from_ref(package) as usize)).copied()
    }

    pub fn loaded_export(&self, id: PackageId, reference: i32) -> Option<(PackageId, usize)> {
        let resolved = self.resolve_loaded(id, reference)?;
        Some((resolved.package, resolved.export?))
    }

    /// The name index of the package at the top of import `index`'s outer chain.
    fn top_package(&self, id: PackageId, index: usize) -> Result<usize, ResolveError> {
        let pkg = &self.package(id).package;
        let mut current = index;
        for _ in 0..=pkg.imports.len() {
            let import = pkg.imports.get(current).ok_or(ResolveError::BadReference(-(current as i32) - 1))?;
            match import.package_index {
                0 => return Ok(import.object_name),
                n @ ..0 => current = n.unsigned_abs() as usize - 1,
                _ => break,
            }
        }
        Err(ResolveError::BadImport(import_path(pkg, index)))
    }

    fn resolve_loaded_import(&self, id: PackageId, index: usize, depth: usize) -> Result<Resolved, ResolveError> {
        let cached = self.resolved_imports[id.0].get(index).ok_or(ResolveError::BadReference(-(index as i32) - 1))?;
        if let Some(&resolved) = cached.get() {
            return Ok(resolved);
        }
        let resolved = self.resolve_uncached_import(id, index, depth)?;
        Ok(*cached.get_or_init(|| resolved))
    }

    fn resolve_uncached_import(&self, id: PackageId, index: usize, depth: usize) -> Result<Resolved, ResolveError> {
        let pkg = &self.package(id).package;
        let import = &pkg.imports[index];
        if depth > MAX_IMPORT_DEPTH {
            return Err(ResolveError::BadImport(import_path(pkg, index)));
        }
        let object_name = pkg.name(import.object_name);
        let class_name = pkg.name(import.class_name);
        let class_package = pkg.name(import.class_package);

        let outer = match import.package_index {
            0 if class_name.eq_ignore_ascii_case("Package") && class_package.eq_ignore_ascii_case("Core") => {
                let package = self.loaded_id(object_name).ok_or_else(|| ResolveError::PackageNotFound(object_name.to_owned()))?;
                return Ok(Resolved { package, export: None });
            }
            n @ ..0 => self.resolve_loaded_import(id, n.unsigned_abs() as usize - 1, depth + 1)?,
            _ => return Err(ResolveError::BadImport(import_path(pkg, index))),
        };

        let source = self.package(outer.package);
        let found = source.find_export(object_name, class_name, class_package, outer.export);
        let class = || format!("{class_package}.{class_name}");
        let path = || import_path(&self.package(id).package, index);
        match found {
            Some(export) if source.package.exports[export].object_flags & RF_PUBLIC != 0 => {
                Ok(Resolved { package: outer.package, export: Some(export) })
            }
            Some(_) => Err(ResolveError::Private { class: class(), path: path() }),
            None => Err(ResolveError::ObjectNotFound { class: class(), path: path() }),
        }
    }
}

impl LoadedPackage {
    fn find_export(&self, name: &str, class_name: &str, class_package: &str, outer: Option<usize>) -> Option<usize> {
        self.exports_by_name.get(&self.package, name).iter().rev().copied().find(|&index| {
            let export = &self.package.exports[index];
            let (export_class, export_class_package) = self.export_class(export);
            let outer_matches = match outer {
                None => export.package_index == 0,
                // The engine also accepts a top-level export for a nested import.
                Some(outer) => export.package_index == 0 || export.package_index == outer as i32 + 1,
            };
            outer_matches
                && export_class.eq_ignore_ascii_case(class_name)
                && export_class_package.eq_ignore_ascii_case(class_package)
        })
    }

    fn export_class(&self, export: &Export) -> (&str, &str) {
        let pkg = &self.package;
        match export.class_index {
            0 => ("Class", "Core"),
            1.. => (pkg.object_name(export.class_index).unwrap_or(""), &self.name),
            n => {
                let class_package = pkg
                    .imports
                    .get(n.unsigned_abs() as usize - 1)
                    .and_then(|import| pkg.object_name(import.package_index))
                    .unwrap_or("");
                (pkg.object_name(n).unwrap_or(""), class_package)
            }
        }
    }
}

pub fn import_path(pkg: &Package, index: usize) -> String {
    let mut parts = Vec::new();
    let mut reference = -(index as i32) - 1;
    while reference != 0 && parts.len() <= pkg.imports.len() {
        let Some(name) = pkg.object_name(reference) else { break };
        parts.push(name);
        reference = match reference {
            ..0 => pkg.imports[reference.unsigned_abs() as usize - 1].package_index,
            _ => pkg.exports[reference as usize - 1].package_index,
        };
    }
    parts.reverse();
    parts.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_paths_from_core_system_only() {
        let ini = "[Engine.Engine]\nPaths=nope\n[Core.System]\nCachePath=..\\Cache\nPaths=..\\System\\*.u\n\
                   paths = ..\\Maps\\*.fuk\n[Other]\nPaths=..\\x\\*.y\n";
        assert_eq!(search_paths(ini), ["..\\System\\*.u", "..\\Maps\\*.fuk"]);
    }

    #[test]
    fn extra_search_dir_found_after_install() {
        let root = std::env::temp_dir().join(format!("p2-loader-extra-{}", std::process::id()));
        let (system, extra) = (root.join("System"), root.join("Packages"));
        for dir in [&system, &extra] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(system.join("Postal2.ini"), "[Core.System]\nPaths=..\\System\\*.u\n").unwrap();
        for file in [system.join("Shell.u"), extra.join("Shell.u"), extra.join("P2Modern.u")] {
            std::fs::write(file, b"").unwrap();
        }
        let loader = Loader::open_with(&system, &[format!("{}/*.u", extra.display())]).unwrap();
        assert!(loader.find("P2Modern").is_some_and(|path| path.starts_with(&extra)));
        assert!(loader.find("Shell").is_some_and(|path| path.starts_with(&system)));
    }
}
