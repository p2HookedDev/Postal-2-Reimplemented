use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use p2_package::{Error, Loader, Package, ResolveError};

const EXTENSIONS: &[&str] = &["u", "utx", "usx", "ukx", "uax", "fuk"];

fn game_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("P2_GAME_DIR").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("P2_GAME_DIR unset, skipping");
    }
    dir
}

fn collect_packages(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_packages(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        {
            out.push(path);
        }
    }
}

fn bad_reference(pkg: &Package) -> Option<String> {
    let in_range = |index: i32| match index {
        0 => true,
        1.. => (index as usize) <= pkg.exports.len(),
        _ => index.unsigned_abs() as usize <= pkg.imports.len(),
    };
    let import_refs = pkg.imports.iter().map(|i| ("import outer", i.package_index));
    let export_refs = pkg.exports.iter().flat_map(|e| {
        [("export class", e.class_index), ("export super", e.super_index), ("export outer", e.package_index)]
    });
    import_refs
        .chain(export_refs)
        .find(|&(_, index)| !in_range(index))
        .map(|(what, index)| format!("{what} {index} out of range"))
}

/// Known exception: `SoundThing` actors in maps store a struct tag whose size is 2 bytes short, so size-based framing
/// fails. Needs declaration-driven parsing.
fn bad_object(pkg: &Package, data: &[u8]) -> Option<String> {
    pkg.exports
        .iter()
        .filter(|e| e.serial_size > 0 && pkg.object_name(e.class_index) != Some("SoundThing"))
        .find_map(|e| {
            pkg.read_object(data, e).err().map(|err| format!("export {}: {err}", pkg.name(e.object_name)))
        })
}

#[test]
fn core_u_tables() {
    let Some(dir) = game_dir() else { return };
    let data = std::fs::read(dir.join("System/Core.u")).unwrap();
    let pkg = Package::parse(&data).unwrap();

    assert_eq!((pkg.summary.version, pkg.summary.licensee), (118, 2));
    assert_eq!(pkg.names.len(), pkg.summary.name_count);
    assert_eq!(pkg.name(0), "None");
    assert!(pkg.names.iter().any(|n| n.text == "Object"));
    assert!(pkg.exports.iter().any(|e| pkg.name(e.object_name) == "Object"));
    for export in &pkg.exports {
        assert!(export.serial_offset + export.serial_size <= data.len());
    }
}

#[derive(Debug, Default)]
struct Resolution {
    packages: usize,
    not_packages: usize,
    imports: usize,
    resolved: usize,
    intrinsic: usize,
    missing_package: usize,
    missing_packages: BTreeSet<String>,
    stale: usize,
    unexplained: Vec<String>,
}

fn is_native_class(system: &Path, path: &str, cache: &mut HashMap<String, bool>) -> bool {
    let Some((package, class)) = path.split_once('.') else { return false };
    *cache.entry(path.to_owned()).or_insert_with(|| {
        let needle = format!("autoclassU{class}\0").into_bytes();
        ["dll", "exe"]
            .iter()
            .filter_map(|ext| std::fs::read(system.join(format!("{package}.{ext}"))).ok())
            .any(|binary| binary.windows(needle.len()).any(|w| w == needle))
    })
}

fn resolve_all(system: &Path) -> Resolution {
    let mut loader = Loader::open(system).unwrap();
    let mut names: Vec<String> = loader.package_names().map(str::to_owned).collect();
    names.sort();
    let mut native = HashMap::new();
    let mut out = Resolution::default();
    for name in &names {
        let id = match loader.load(name) {
            Ok(id) => id,
            Err(ResolveError::Parse { source: Error::BadTag(_), .. }) => {
                out.not_packages += 1;
                continue;
            }
            Err(e) => {
                out.unexplained.push(format!("{name}: {e}"));
                continue;
            }
        };
        out.packages += 1;
        let import_count = loader.package(id).package.imports.len();
        out.imports += import_count;
        for index in 0..import_count {
            match loader.resolve_import(id, index) {
                Ok(resolved) => {
                    let pkg = &loader.package(id).package;
                    let import_name = pkg.name(pkg.imports[index].object_name);
                    let source = loader.package(resolved.package);
                    let found_name = match resolved.export {
                        Some(export) => source.package.name(source.package.exports[export].object_name),
                        None => &source.name,
                    };
                    if found_name.eq_ignore_ascii_case(import_name) {
                        out.resolved += 1;
                    } else {
                        out.unexplained.push(format!("{name}: import {import_name} resolved to {found_name}"));
                    }
                }
                Err(_) if name.starts_with("(name_conflict)") => out.stale += 1,
                Err(ResolveError::PackageNotFound(package)) => {
                    out.missing_package += 1;
                    out.missing_packages.insert(package);
                }
                Err(ResolveError::ObjectNotFound { class, path })
                    if class == "Core.Class" && is_native_class(system, &path, &mut native) =>
                {
                    out.intrinsic += 1;
                }
                Err(e) => out.unexplained.push(format!("{name}: {e}")),
            }
        }
    }
    out
}

#[test]
fn resolve_every_import() {
    let Some(dir) = game_dir() else { return };
    let configs = [
        ("System", &["RevSys", "RevTextures01", "Strippers"][..]),
        ("Paradise Lost/System", &["Strippers"]),
    ];
    for (system, missing) in configs {
        let r = resolve_all(&dir.join(system));
        eprintln!("{system}: {r:?}");
        let unexplained = r.unexplained.join("\n");
        assert!(r.unexplained.is_empty(), "{system}: {} unexplained:\n{unexplained}", r.unexplained.len());
        assert_eq!(r.missing_packages.iter().map(String::as_str).collect::<Vec<_>>(), missing, "{system}");
        assert!(r.resolved > 0 && r.intrinsic > 0);
    }
}

#[test]
fn parse_every_package() {
    let Some(dir) = game_dir() else { return };
    let mut paths = Vec::new();
    collect_packages(&dir, &mut paths);
    assert!(!paths.is_empty(), "no packages under {}", dir.display());

    let failures: Vec<String> = paths
        .iter()
        .filter_map(|path| {
            let data = std::fs::read(path).unwrap();
            let error = match Package::parse(&data) {
                Ok(pkg) => bad_reference(&pkg).or_else(|| bad_object(&pkg, &data)),
                Err(e) => Some(e.to_string()),
            };
            error.map(|e| format!("{}: {e}", path.display()))
        })
        .collect();
    assert!(failures.is_empty(), "{} of {} failed:\n{}", failures.len(), paths.len(), failures.join("\n"));
}

#[test]
fn church_imports_load() {
    let Some(dir) = game_dir() else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    let map = loader.load("Church").unwrap();
    loader.load_imports(map).unwrap();
    let pkg = &loader.package(map).package;
    let unresolved: Vec<_> = (0..pkg.imports.len())
        .filter(|&i| pkg.name(pkg.imports[i].class_name) != "Class")
        .filter(|&i| loader.resolve_loaded(map, -(i as i32) - 1).is_none())
        .map(|i| pkg.name(pkg.imports[i].object_name).to_owned())
        .collect();
    assert!(unresolved.is_empty(), "unresolved after load_imports: {unresolved:?}");
    let import = pkg.imports.iter().position(|i| pkg.name(i.class_name) == "Sound").unwrap();
    let resolved = loader.resolve_loaded(map, -(import as i32) - 1).unwrap();
    let (sound_pkg, export) = (resolved.package, resolved.export.unwrap());
    let loaded = loader.package(sound_pkg);
    let mut path = vec![loaded.package.name(loaded.package.exports[export].object_name).to_owned()];
    let mut outer = loaded.package.exports[export].package_index;
    while outer > 0 {
        let e = &loaded.package.exports[outer as usize - 1];
        path.push(loaded.package.name(e.object_name).to_owned());
        outer = e.package_index;
    }
    path.push(loaded.name.clone());
    path.reverse();
    assert_eq!(loader.loaded_export_by_path(&path.join(".")), Some((sound_pkg, export)));
}

#[test]
fn adopt_clone_packages() {
    let Some(dir) = game_dir() else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    let core = loader.load("Core").unwrap();
    let mut clone = loader.clone();
    clone.load("Engine").unwrap();
    let church = loader.load("Church").unwrap();
    clone.load("Church").unwrap();
    loader.adopt(&clone);
    let engine = loader.loaded_id("engine").unwrap();
    assert_eq!((loader.loaded_id("Core"), loader.loaded_id("Church")), (Some(core), Some(church)));
    assert!(std::ptr::eq(&loader.package(engine).package, &clone.package(clone.loaded_id("Engine").unwrap()).package));
}

#[test]
fn install_ini_files() {
    let Some(dir) = game_dir() else { return };
    let read = |name: &str| p2_package::Ini::read(dir.join("System").join(name)).unwrap();
    let system = read("Postal2.ini");
    assert!(system.get_all("Core.System", "Paths").count() > 5);
    assert_eq!(system.get("ALAudio.ALAudioSubsystem", "Rolloff"), Some("0.900000"));
    let user = read("User.ini");
    assert!(user.get("Engine.Input", "W").is_some_and(|w| !w.is_empty()), "User.ini binds W");
}

#[test]
fn same_name_objects_found_by_class() {
    let Some(dir) = game_dir() else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    let class_of = |loader: &Loader, (id, export): (p2_package::PackageId, usize)| {
        let pkg = &loader.package(id).package;
        pkg.object_name(pkg.exports[export].class_index).unwrap_or_default().to_owned()
    };
    let path = "ED_Weapons.ED_Chainsaw_NEW";
    for class in ["SkeletalMesh", "MeshAnimation"] {
        let found = loader.export_by_path_of_class(path, class).unwrap().expect(class);
        assert_eq!(class_of(&loader, found), class);
        assert_eq!(loader.loaded_export_by_path_of_class(path, class), Some(found));
    }
    assert_eq!(loader.loaded_export_by_path_of_class(path, "Texture"), None);
}