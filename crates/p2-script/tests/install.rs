use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use p2_package::Package;
use p2_script::{
    Expr, Layouts, NativeName, Objects, Resolve, ScriptObject, Source, Type, StoredValue, class_name, decode_script, disassemble,
    native_names, read_export, read_properties,
};

fn game_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("P2_GAME_DIR").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("P2_GAME_DIR unset, skipping");
    }
    dir
}

struct Loaded {
    name: String,
    data: Vec<u8>,
    package: Package,
    by_name: HashMap<String, Vec<usize>>,
}

impl Loaded {
    fn read(path: &Path) -> Self {
        let data = std::fs::read(path).unwrap();
        let package = Package::parse(&data).unwrap();
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, e) in package.exports.iter().enumerate() {
            by_name.entry(package.name(e.object_name).to_ascii_lowercase()).or_default().push(i);
        }
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        Self { name, data, package, by_name }
    }

    fn source(&self) -> Source<'_> {
        Source { package: &self.package, data: &self.data }
    }

    fn find(&self, name: &str, class: &str, outers: &[&str]) -> Option<usize> {
        let pkg = &self.package;
        self.by_name.get(&name.to_ascii_lowercase())?.iter().copied().find(|&i| {
            let export = &pkg.exports[i];
            if !class_name(pkg, export).eq_ignore_ascii_case(class) {
                return false;
            }
            let mut outer = export.package_index;
            for expected in outers {
                match outer {
                    1.. if pkg.name(pkg.exports[outer as usize - 1].object_name).eq_ignore_ascii_case(expected) => {
                        outer = pkg.exports[outer as usize - 1].package_index;
                    }
                    _ => return false,
                }
            }
            outer == 0
        })
    }
}

struct Install {
    packages: Vec<Loaded>,
}

impl Install {
    fn load(dir: &Path) -> Self {
        let mut packages: Vec<Loaded> = std::fs::read_dir(dir.join("System"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("u")))
            .map(|p| Loaded::read(&p))
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Self { packages }
    }
}

impl Resolve for Install {
    fn resolve_import(&self, from: &Package, import: usize) -> Option<(Source<'_>, usize)> {
        let imp = from.imports.get(import)?;
        let mut outers = Vec::new();
        let mut outer = imp.package_index;
        while outer < 0 {
            let o = from.imports.get(outer.unsigned_abs() as usize - 1)?;
            outers.push(from.name(o.object_name));
            outer = o.package_index;
        }
        let package = outers.pop()?;
        let loaded = self.packages.iter().find(|p| p.name.eq_ignore_ascii_case(package))?;
        let export = loaded.find(from.name(imp.object_name), from.name(imp.class_name), &outers)?;
        Some((loaded.source(), export))
    }
}

fn has_unnamed_native(line: &str) -> bool {
    line.match_indices("native").any(|(i, _)| {
        let rest = &line[i + "native".len()..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && rest[digits..].starts_with('(')
    })
}

#[test]
fn decode_every_script() {
    let Some(dir) = game_dir() else { return };
    let install = Install::load(&dir);
    let (mut scripts, mut statements, mut targets) = (0, 0, 0);
    let mut failures = Vec::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let natives: HashMap<u16, NativeName> =
        install.packages.iter().flat_map(|loaded| native_names(&loaded.package, &loaded.data)).collect();
    for loaded in &install.packages {
        let pkg = &loaded.package;
        for (i, export) in pkg.exports.iter().enumerate() {
            let Ok(Some(object)) = read_export(pkg, &loaded.data, i) else { continue };
            let Some(base) = object.as_struct() else { continue };
            let name = format!("{}.{}", loaded.name, pkg.name(export.object_name));
            let decoded = match decode_script(pkg, &base.script, base.script_size) {
                Ok(decoded) => decoded,
                Err(e) => {
                    failures.push(format!("{name}: {e}"));
                    continue;
                }
            };
            scripts += 1;
            statements += decoded.len();
            let text = disassemble(pkg, &decoded, &natives);
            for line in text.lines().filter(|l| l.contains("cast0x") || l.contains("<invalid>") || has_unnamed_native(l)) {
                failures.push(format!("{name}: {line}"));
            }
            let starts: std::collections::HashSet<u32> =
                decoded.iter().map(|s| s.offset).chain([base.script_size]).collect();
            for statement in &decoded {
                let kind = format!("{:?}", statement.expr);
                let kind = kind.split(['(', ' ', '{']).next().unwrap_or_default().to_owned();
                *kinds.entry(kind).or_insert(0) += 1;
                let target = match &statement.expr {
                    Expr::Jump(target) | Expr::JumpIfNot { target, .. } => Some(*target),
                    Expr::Case { next, .. } if *next != 0xffff => Some(*next),
                    Expr::Iterator { end, .. } => Some(*end),
                    Expr::EndFunctionParms => {
                        failures.push(format!("{name}: EndFunctionParms at {}", statement.offset));
                        None
                    }
                    _ => None,
                };
                if let Some(target) = target {
                    targets += 1;
                    if !starts.contains(&u32::from(target)) {
                        failures.push(format!("{name}: target {target} at {} is not a statement", statement.offset));
                    }
                }
            }
        }
    }
    eprintln!("{scripts} scripts, {statements} statements, {targets} jump targets; top-level kinds {kinds:#?}");
    assert!(scripts > 10000);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.iter().take(30).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn parse_every_definition() {
    let Some(dir) = game_dir() else { return };
    let install = Install::load(&dir);
    assert!(install.packages.len() > 40);

    let mut layouts = Layouts::new(&install);
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    for loaded in &install.packages {
        let pkg = &loaded.package;
        for (i, export) in pkg.exports.iter().enumerate() {
            let name = format!("{}.{}", loaded.name, pkg.name(export.object_name));
            let object = match read_export(pkg, &loaded.data, i) {
                Ok(Some(object)) => object,
                Ok(None) => continue,
                Err(e) => {
                    failures.push(format!("{name}: {e}"));
                    continue;
                }
            };
            *counts.entry(class_name(pkg, export)).or_default() += 1;

            let ScriptObject::Class(class) = object else { continue };
            let result = layouts.layout(loaded.source(), i).and_then(|layout| {
                let mut r = p2_package::Reader::new(&loaded.data[..export.serial_offset + export.serial_size]);
                r.seek(class.defaults_offset)?;
                let defaults = read_properties(pkg, &mut r, &layout)?;
                Ok((r.at_end(), defaults.skipped.len()))
            });
            match result {
                Ok((true, 0)) => {}
                Ok((at_end, skipped)) => failures.push(format!("{name}: defaults end {at_end}, skipped {skipped}")),
                Err(e) => failures.push(format!("{name} defaults: {e}")),
            }
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    assert!(counts["Class"] > 2000 && counts["Function"] > 10000);
}

fn own_properties(loaded: &Loaded, class: &str) -> Vec<String> {
    let pkg = &loaded.package;
    let index = loaded.find(class, "Class", &[]).unwrap();
    let Some(ScriptObject::Class(c)) = read_export(pkg, &loaded.data, index).unwrap() else { panic!() };
    let mut names = Vec::new();
    let mut child = c.base.base.children;
    while child > 0 {
        let object = read_export(pkg, &loaded.data, child as usize - 1).unwrap().unwrap();
        if matches!(object, ScriptObject::Property(_)) {
            names.push(pkg.name(pkg.exports[child as usize - 1].object_name).to_owned());
        }
        child = object.field().unwrap().next;
    }
    names
}

#[test]
fn spot_check_against_source() {
    let Some(dir) = game_dir() else { return };
    let core = Loaded::read(&dir.join("System/Core.u"));
    let object = own_properties(&core, "Object");
    assert_eq!(object, ["ObjectInternal", "Outer", "ObjectFlags", "Name", "Class"]);

    let engine = Loaded::read(&dir.join("System/Engine.u"));
    let actor = own_properties(&engine, "Actor");
    assert_eq!(&actor[..3], ["bStatic", "bHidden", "bNoDelete"]);
    assert_eq!(&actor[actor.len() - 2..], ["bReplicateSkin", "RepSkin"]);

    let index = engine.find("EPhysics", "Enum", &["Actor"]).unwrap();
    let Some(ScriptObject::Enum(physics)) = read_export(&engine.package, &engine.data, index).unwrap() else {
        panic!()
    };
    let names: Vec<&str> = physics.names.iter().map(|&n| engine.package.name(n)).collect();
    assert_eq!(&names[..3], ["PHYS_None", "PHYS_Walking", "PHYS_Falling"]);

    let fps = Loaded::read(&dir.join("System/FPSGame.u"));
    let index = fps.find("SoundThing", "Class", &[]).unwrap();
    let Some(ScriptObject::Class(class)) = read_export(&fps.package, &fps.data, index).unwrap() else { panic!() };
    let hidden: Vec<&str> = class.hide_categories.iter().map(|&n| fps.package.name(n)).collect();
    assert_eq!(hidden, ["Shadow", "Collision", "Force", "Karma", "LightColor", "Lighting", "Shadow"]);

    let install = Install { packages: vec![core] };
    let core = &install.packages[0];
    let plane = core.find("Plane", "Struct", &["Object"]).unwrap();
    let layout = Layouts::new(&install).layout(core.source(), plane).unwrap();
    let fields: Vec<&str> = layout.properties.iter().map(|p| &*p.name).collect();
    assert_eq!(fields, ["W", "X", "Y", "Z"]);
}

#[test]
fn sound_things_by_declaration() {
    let Some(dir) = game_dir() else { return };
    let install = Install::load(&dir);
    let map = Loaded::read(&dir.join("Maps/Church.fuk"));
    let pkg = &map.package;
    let mut layouts = Layouts::new(&install);

    let (mut count, mut framing_failed) = (0, 0);
    let mut size_mismatches = BTreeMap::new();
    for (i, export) in pkg.exports.iter().enumerate() {
        if pkg.object_name(export.class_index) != Some("SoundThing") {
            continue;
        }
        framing_failed += usize::from(pkg.read_object(&map.data, export).is_err());

        let layout = layouts.class_layout(map.source(), i).unwrap();
        let mut r = pkg.export_reader(&map.data, export).unwrap();
        let props = read_properties(pkg, &mut r, &layout).unwrap();
        assert!(r.at_end(), "{} bytes left", r.rest().len());
        assert!(props.skipped.is_empty(), "{:?}", props.skipped);

        let settings = props.values.iter().find(|v| &*v.name == "Settings").unwrap();
        let StoredValue::Struct(settings) = &settings.value else { panic!() };
        let sounds = settings.values.iter().find(|v| &*v.name == "Sounds").unwrap();
        let StoredValue::Array(sounds) = &sounds.value else { panic!() };
        assert!(sounds.iter().all(|s| matches!(s, StoredValue::Object(_))));

        let Some(Type::Struct(settings_layout)) = layout.find("Settings").map(|f| &f.ty) else { panic!() };
        let mut r = pkg.export_reader(&map.data, export).unwrap();
        while let Some(tag) = pkg.read_tag(&mut r).unwrap() {
            if pkg.name(tag.name) != "Settings" {
                r.bytes(if tag.kind == 3 { 0 } else { tag.size }).unwrap();
                continue;
            }
            let start = r.pos();
            read_properties(pkg, &mut r, settings_layout).unwrap();
            let actual = r.pos() - start;
            if actual != tag.size {
                let key = format!("size {} actual {} sounds {}", tag.size, actual, sounds.len());
                *size_mismatches.entry(key).or_insert(0) += 1;
            }
            break;
        }
        count += 1;
    }
    eprintln!("{count} SoundThings decoded, size framing fails on {framing_failed}; {size_mismatches:#?}");
    assert!(count > 0 && framing_failed > 0);
}

#[test]
fn objects_read_with_class_defaults() {
    let Some(dir) = game_dir() else { return };
    let install = Install::load(&dir);
    let map = Loaded::read(&dir.join("Maps/Church.fuk"));
    let pkg = &map.package;
    let mut objects = Objects::new(&install);

    let (mut actors, mut failures) = (0, Vec::new());
    let scripted = |e: &&p2_package::Export| e.class_index > 0 || (e.class_index < 0 && install.resolve_import(pkg, (-e.class_index - 1) as usize).is_some());
    for (i, export) in pkg.exports.iter().enumerate().filter(|(_, e)| scripted(e)) {
        let object = match objects.read(map.source(), i) {
            Ok(object) => object,
            Err(e) => {
                failures.push(format!("{}: {e}", pkg.name(export.object_name)));
                continue;
            }
        };
        if object.defaults.iter().any(|layer| layer.properties.values.iter().any(|v| &*v.name == "DrawType")) {
            assert!(matches!(object.get("DrawType", 0), Some((_, StoredValue::Byte(_)))));
            actors += 1;
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(actors > 0);
}
