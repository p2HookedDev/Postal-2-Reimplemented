//! Packages the loader has not loaded, mapped from their files, so only the pages read come from disk.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use p2_package::{Mmap, Package};

use crate::{LoaderRef, Vm};

pub(crate) struct SidePackage {
    pub package: Package,
    pub data: Mmap,
    by_name: HashMap<String, Vec<usize>>,
}

impl SidePackage {
    fn open(file: &Path) -> Option<Self> {
        let data = p2_package::map_file(file).ok()?;
        let package = Package::parse(&data).ok()?;
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, export) in package.exports.iter().enumerate() {
            by_name.entry(package.name(export.object_name).to_ascii_lowercase()).or_default().push(index);
        }
        Some(Self { package, data, by_name })
    }

    fn export_by_path(&self, path: &str) -> Option<usize> {
        let parts: Vec<&str> = path.split('.').collect();
        let (name, outers) = parts.split_last()?;
        let named = self.by_name.get(&name.to_ascii_lowercase())?;
        let exact = named.iter().copied().find(|&i| self.package.export_in(i, outers));
        exact.or_else(|| if outers.is_empty() { named.first().copied() } else { None })
    }
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn side_export(&mut self, path: &str) -> Option<(Arc<SidePackage>, usize)> {
        let (package, rest) = path.split_once('.')?;
        let key = package.to_ascii_lowercase();
        if !self.side_packages.contains_key(&key) {
            let phase = self.profile_start();
            let opened = self.loader.find(package).and_then(SidePackage::open);
            self.side_packages.insert(key.clone(), opened.map(Arc::new));
            self.profile_phase("load: package file", phase);
        }
        let side = self.side_packages.get(&key)?.as_ref()?;
        let export = side.export_by_path(rest)?;
        Some((Arc::clone(side), export))
    }

    pub(crate) fn side_sound_duration(&mut self, path: &str) -> Option<f32> {
        let (side, export) = self.side_export(path)?;
        p2_assets::read_sound_duration(&side.package, &side.data, &side.package.exports[export]).ok()?
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn mapped_packages_match_the_whole_file() {
        let Some(dir) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
        let mut checked = 0;
        for folder in ["Sounds", "Textures", "StaticMeshes", "Animations"] {
            let Ok(entries) = std::fs::read_dir(dir.join(folder)) else { continue };
            let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
            files.sort();
            for file in files.iter().step_by(files.len().div_ceil(4).max(1)) {
                let side = SidePackage::open(file).unwrap_or_else(|| panic!("{} does not open", file.display()));
                let data = std::fs::read(file).unwrap();
                let whole = Package::parse(&data).unwrap();
                let names = |p: &Package| p.names.iter().map(|n| n.text.clone()).collect::<Vec<_>>();
                assert_eq!(names(&side.package), names(&whole), "{}", file.display());
                assert_eq!(side.package.exports.len(), whole.exports.len());
                for export in [0, whole.exports.len() / 2, whole.exports.len().saturating_sub(1)] {
                    let Some(entry) = whole.exports.get(export) else { continue };
                    if whole.object_name(entry.class_index).is_some_and(|c| c.eq_ignore_ascii_case("Sound")) {
                        let duration = p2_assets::read_sound_duration(&whole, &data, entry).ok().flatten();
                        let mapped = p2_assets::read_sound_duration(&side.package, &side.data, &side.package.exports[export]).ok().flatten();
                        assert_eq!(mapped, duration, "{} export {export}", file.display());
                    }
                }
                checked += 1;
            }
        }
        assert!(checked >= 8, "{checked} packages");
    }
}
