//! Narrow counts over the install to run by hand, with `P2_GAME_DIR` set:
//! `cargo test -p p2-assets --test review_survey -- --ignored --nocapture --test-threads=1`.

#![expect(clippy::print_stdout, reason = "a survey prints its counts")]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use p2_assets::{read_model, read_skeletal_mesh};
use p2_assets::poly_flags;
use p2_package::{Loader, Package, PackageId};

fn system_dir() -> Option<PathBuf> {
    std::env::var_os("P2_GAME_DIR").map(|dir| PathBuf::from(dir).join("System"))
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

fn names_with_extension(loader: &Loader, extension: &str) -> Vec<String> {
    let mut names: Vec<String> = loader.package_names().map(str::to_owned).collect();
    names.retain(|name| loader.find(name).is_some_and(|path| has_extension(path, extension)));
    names.sort();
    names
}

#[test]
#[ignore = "reads the install"]
fn attach_aliases() {
    let Some(system) = system_dir() else { return };
    let loader = Loader::open(&system).unwrap();
    let (mut meshes, mut with_aliases, mut total, mut shadowing) = (0, 0, 0, 0);
    let mut pairs: BTreeMap<(String, String), usize> = BTreeMap::new();
    for name in names_with_extension(&loader, "ukx") {
        let Some(path) = loader.find(&name) else { continue };
        let file = std::fs::read(path).unwrap();
        let Ok(pkg) = Package::parse(&file) else { continue };
        for export in pkg.exports.iter().filter(|e| pkg.object_name(e.class_index) == Some("SkeletalMesh")) {
            let Ok(mesh) = read_skeletal_mesh(&pkg, &file, export) else { continue };
            meshes += 1;
            with_aliases += usize::from(!mesh.attach_aliases.is_empty());
            total += mesh.attach_aliases.len();
            let bones: Vec<String> = mesh.bones.iter().map(|b| pkg.name(b.name).to_ascii_lowercase()).collect();
            for alias in &mesh.attach_aliases {
                let (from, to) = (pkg.name(alias.alias).to_ascii_lowercase(), pkg.name(alias.bone).to_ascii_lowercase());
                if bones.contains(&from) && from != to {
                    shadowing += 1;
                    println!("shadows a bone: {name}.{}: {from} -> {to}", pkg.name(export.object_name));
                }
                *pairs.entry((from, to)).or_default() += 1;
            }
        }
    }
    println!("meshes {meshes}, with aliases {with_aliases}, aliases {total}, alias naming another bone {shadowing}");
    for ((from, to), count) in pairs {
        println!("{count:5} {from} -> {to}");
    }
}

#[test]
#[ignore = "reads the install"]
fn unlit_surfaces() {
    let Some(system) = system_dir() else { return };
    let loader = Loader::open(&system).unwrap();
    let (mut maps, mut polygons, mut unlit, mut unlit_lit, mut sections, mut unlit_sections, mut unlit_sections_lit) = (0, 0, 0, 0, 0, 0, 0);
    let mut materials: BTreeMap<String, usize> = BTreeMap::new();
    for name in names_with_extension(&loader, "fuk") {
        let Some(path) = loader.find(&name) else { continue };
        let file = std::fs::read(path).unwrap();
        let Ok(pkg) = Package::parse(&file) else { continue };
        maps += 1;
        let (mut map_unlit, mut map_lit) = (0, 0);
        for export in pkg.exports.iter().filter(|e| pkg.object_name(e.class_index) == Some("Model")) {
            let Ok(model) = read_model(&pkg, &file, export) else { continue };
            let hidden = poly_flags::INVISIBLE | poly_flags::PORTAL;
            for polygon in model.polygons().iter().filter(|p| p.poly_flags & hidden == 0) {
                polygons += 1;
                if polygon.poly_flags & poly_flags::UNLIT == 0 {
                    continue;
                }
                map_unlit += 1;
                if model.light_map_uvs(polygon).is_some() {
                    map_lit += 1;
                    *materials.entry(pkg.object_name(polygon.material).unwrap_or("?").to_owned()).or_default() += 1;
                }
            }
            for section in &model.sections {
                sections += 1;
                if section.poly_flags & poly_flags::UNLIT != 0 {
                    unlit_sections += 1;
                    unlit_sections_lit += usize::from(section.light_map_texture >= 0);
                }
            }
        }
        unlit += map_unlit;
        unlit_lit += map_lit;
        if map_lit > 0 {
            println!("{name}: unlit {map_unlit}, with a light map {map_lit}");
        }
    }
    println!("maps {maps}, polygons drawn {polygons}, unlit {unlit}, unlit with a light map {unlit_lit}");
    println!("sections {sections}, unlit {unlit_sections}, unlit with a light map texture {unlit_sections_lit}");
    let mut materials: Vec<_> = materials.into_iter().collect();
    materials.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    println!("materials of unlit polygons with a light map: {} {:?}", materials.len(), &materials[..materials.len().min(25)]);
}

fn loaded(loader: &Loader) -> BTreeSet<String> {
    loader.package_names().filter(|name| loader.loaded_id(name).is_some()).map(str::to_owned).collect()
}

fn imported(loader: &Loader, id: PackageId) -> Vec<String> {
    let pkg = &loader.package(id).package;
    let names = pkg.imports.iter().filter(|import| import.package_index == 0).map(|import| pkg.name(import.object_name).to_owned());
    names.filter(|name| loader.find(name).is_some()).collect()
}

fn follow_new(loader: &mut Loader, from: PackageId) {
    let mut pending = vec![from];
    while let Some(id) = pending.pop() {
        for name in imported(loader, id) {
            if loader.loaded_id(&name).is_none() {
                pending.push(loader.load(&name).unwrap());
            }
        }
    }
}

fn follow_all(loader: &mut Loader, from: Vec<PackageId>) {
    let mut seen: HashSet<PackageId> = from.iter().copied().collect();
    let mut pending = from;
    while let Some(id) = pending.pop() {
        for name in imported(loader, id) {
            let next = loader.load(&name).unwrap();
            if seen.insert(next) {
                pending.push(next);
            }
        }
    }
}

#[test]
#[ignore = "reads the install"]
fn import_closure() {
    let Some(system) = system_dir() else { return };
    let mut base = Loader::open(&system).unwrap();
    base.load_scripts().unwrap();
    let scripts = loaded(&base);
    println!("script packages {}", scripts.len());
    let classes = ["MeshAnimation", "SkeletalMesh", "Texture"];
    let maps = names_with_extension(&base, "fuk");
    let (mut from_map, mut from_all): (BTreeMap<String, usize>, BTreeMap<String, usize>) = (BTreeMap::new(), BTreeMap::new());
    let mut changed = 0;
    let mut sizes = (0, 0, 0);
    for map in &maps {
        let mut loader = base.clone();
        let id = loader.load(map).unwrap();
        follow_new(&mut loader, id);
        loader.load_imported(&classes).unwrap();
        let today = loaded(&loader);
        follow_all(&mut loader, vec![id]);
        loader.load_imported(&classes).unwrap();
        let walked = loaded(&loader);
        let every: Vec<PackageId> = walked.iter().filter_map(|name| loader.loaded_id(name)).collect();
        follow_all(&mut loader, every);
        loader.load_imported(&classes).unwrap();
        let all = loaded(&loader);
        changed += usize::from(walked != today);
        sizes.0 += today.len();
        sizes.1 += walked.len();
        sizes.2 += all.len();
        for name in walked.difference(&today) {
            *from_map.entry(name.clone()).or_default() += 1;
        }
        for name in all.difference(&walked) {
            *from_all.entry(name.clone()).or_default() += 1;
        }
    }
    let count = maps.len().max(1);
    println!("maps {}, changed by the walk {changed}, mean packages today {}, walked {}, all {}", maps.len(), sizes.0 / count, sizes.1 / count, sizes.2 / count);
    println!("added by walking from the map: {from_map:?}");
    println!("added by walking every loaded package: {from_all:?}");
}
