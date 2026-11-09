//! Dumps the header, name, import and export tables of a package.
//!
//! Usage: `p2-dump <package> [--props | --code]`. `--props` prints each export's tagged properties instead;
//! `--code` prints the decoded bytecode of every function, state and class (native names from the `.u`
//! packages next to it, or in `..\System`). `p2-dump <folder> --shaders` counts the `Shader` forms of every
//! `.utx` / `.usx` / `.fuk` under the folder.

use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::Context;
use p2_package::{Export, MAP_EXTENSION, Package, TagValue};
use p2_script::{NativeName, ScriptObject, decode_script, disassemble, native_names, read_export};

const USAGE: &str = "usage: p2-dump <package> [--props | --code] | p2-dump <folder> --shaders";

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().context(USAGE)?;
    let mode = args.next();
    let mode = mode.as_ref().and_then(|m| m.to_str());
    let mut out = BufWriter::new(std::io::stdout().lock());
    if mode == Some("--shaders") {
        return shader_census(Path::new(&path), &mut out);
    }
    let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let pkg = Package::parse(&data).with_context(|| format!("parsing {}", path.display()))?;
    match mode {
        None => dump(&pkg, &mut out)?,
        Some("--props") => dump_properties(&pkg, &data, &mut out)?,
        Some("--code") => dump_code(&pkg, &data, &natives_near(Path::new(&path)), &mut out)?,
        Some(_) => anyhow::bail!(USAGE),
    }
    Ok(())
}

fn natives_near(path: &Path) -> HashMap<u16, NativeName> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let system = [dir.to_path_buf(), dir.join("..").join("System")].into_iter().find(|d| d.join("Core.u").is_file());
    let mut natives = HashMap::new();
    let Some(entries) = system.and_then(|system| std::fs::read_dir(system).ok()) else { return natives };
    for path in entries.flatten().map(|e| e.path()) {
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("u")) {
            continue;
        }
        let Ok(data) = std::fs::read(&path) else { continue };
        match Package::parse(&data) {
            Ok(pkg) => natives.extend(native_names(&pkg, &data)),
            Err(e) => eprintln!("native names: {}: {e}", path.display()),
        }
    }
    natives
}

fn dump_code(pkg: &Package, data: &[u8], natives: &HashMap<u16, NativeName>, out: &mut impl Write) -> std::io::Result<()> {
    for index in 0..pkg.exports.len() {
        let object = match read_export(pkg, data, index) {
            Ok(Some(object)) => object,
            Ok(None) => continue,
            Err(e) => {
                writeln!(out, "\nexport {} ERROR {e}", index + 1)?;
                continue;
            }
        };
        let Some(base) = object.as_struct().filter(|s| s.script_size > 0) else { continue };
        let kind = match object {
            ScriptObject::Class(_) => "class",
            ScriptObject::State(_) => "state",
            ScriptObject::Function(_) => "function",
            _ => "struct",
        };
        let export = &pkg.exports[index];
        writeln!(out, "\n{kind} {}.{}", object_name(pkg, export.package_index), pkg.name(export.object_name))?;
        match decode_script(pkg, &base.script, base.script_size) {
            Ok(statements) => write!(out, "{}", disassemble(pkg, &statements, natives))?,
            Err(e) => writeln!(out, "ERROR {e}")?,
        }
    }
    out.flush()
}

fn dump_properties(pkg: &Package, data: &[u8], out: &mut impl Write) -> std::io::Result<()> {
    for (i, exp) in pkg.exports.iter().enumerate().filter(|(_, e)| e.serial_size > 0) {
        write!(out, "{:6} {} {}", i + 1, class_name(pkg, exp.class_index), pkg.name(exp.object_name))?;
        let object = match pkg.read_object(data, exp) {
            Ok(object) => object,
            Err(e) => {
                writeln!(out, " ERROR {e}")?;
                continue;
            }
        };
        let end = exp.serial_offset + exp.serial_size;
        let Some(native) = end.checked_sub(object.native_offset) else {
            writeln!(out, " ERROR properties end at {:#x}, past the export's end {end:#x}", object.native_offset)?;
            continue;
        };
        writeln!(out, " native {native:#x} bytes")?;
        for p in &object.properties {
            write!(out, "         {}[{}] = ", pkg.name(p.name), p.array_index)?;
            match &p.value {
                TagValue::Object(index) => writeln!(out, "Object {}", object_name(pkg, *index))?,
                TagValue::Name(name) => writeln!(out, "Name {}", pkg.name(*name))?,
                TagValue::Struct { name, data } => writeln!(out, "Struct {} {data:02x?}", pkg.name(*name))?,
                value => writeln!(out, "{value:?}")?,
            }
        }
    }
    out.flush()
}

fn shader_census(dir: &Path, out: &mut impl Write) -> anyhow::Result<()> {
    let mut forms: HashMap<String, usize> = HashMap::new();
    let mut multi_pass = Vec::new();
    let (mut skipped_packages, mut skipped_shaders) = (0usize, 0usize);
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for path in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?.flatten().map(|e| e.path()) {
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let extension = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
            if !matches!(extension.as_deref(), Some("utx" | "usx" | MAP_EXTENSION)) {
                continue;
            }
            let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let Ok(pkg) = Package::parse(&data) else {
                skipped_packages += 1;
                continue;
            };
            for exp in pkg.exports.iter().filter(|e| e.serial_size > 0 && object_name(&pkg, e.class_index) == "Shader") {
                let Some((form, several_passes)) = shader_form(&pkg, &data, exp) else {
                    skipped_shaders += 1;
                    continue;
                };
                if several_passes {
                    let file = path.file_name().map(|f| f.to_string_lossy()).unwrap_or_default();
                    multi_pass.push(format!("{file} {}: {form}", pkg.name(exp.object_name)));
                }
                *forms.entry(form).or_default() += 1;
            }
        }
    }
    let mut forms: Vec<_> = forms.into_iter().collect();
    forms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    writeln!(out, "forms ({} shaders)", forms.iter().map(|(_, n)| n).sum::<usize>())?;
    for (form, count) in forms {
        writeln!(out, "{count:5} {form}")?;
    }
    multi_pass.sort();
    writeln!(out, "\nmulti-pass ({})", multi_pass.len())?;
    for line in multi_pass {
        writeln!(out, "{line}")?;
    }
    writeln!(out, "\nskipped: {skipped_packages} packages and {skipped_shaders} shaders that could not be read")?;
    out.flush()?;
    Ok(())
}

fn shader_form(pkg: &Package, data: &[u8], exp: &Export) -> Option<(String, bool)> {
    let object = pkg.read_object(data, exp).ok()?;
    let property = |name: &str| object.properties.iter().find(|p| pkg.name(p.name) == name).map(|p| &p.value);
    let reference = |name| match property(name) {
        Some(TagValue::Object(index)) => *index,
        _ => 0,
    };
    let (diffuse, opacity) = (reference("Diffuse"), reference("Opacity"));
    let (glow, glow_mask) = (reference("SelfIllumination"), reference("SelfIlluminationMask"));
    let (specular, mask) = (reference("Specular"), reference("SpecularityMask"));
    let opacity_form = match opacity {
        0 => "none".to_owned(),
        _ if opacity == diffuse => "diffuse".to_owned(),
        _ => chain(pkg, data, opacity),
    };
    let glow_form = match (glow, glow_mask) {
        (0, _) => "none".to_owned(),
        (_, 0) => "unlit".to_owned(),
        _ if glow_mask == glow => "own-alpha".to_owned(),
        _ => format!("masked:{}", chain(pkg, data, glow_mask)),
    };
    let mask_form = match mask {
        0 => "none".to_owned(),
        _ if mask == diffuse => "diffuse".to_owned(),
        _ => chain(pkg, data, mask),
    };
    let blending = match property("OutputBlending") {
        Some(TagValue::Byte(b)) => *b,
        _ => 0,
    };
    let form = format!(
        "opacity={opacity_form} glow={glow_form} specular={} mask={mask_form} blend={blending}",
        chain(pkg, data, specular)
    );
    let several_passes = opacity != 0 && opacity != diffuse && ((glow != 0 && glow_mask != 0) || specular != 0);
    Some((form, several_passes))
}

fn chain(pkg: &Package, data: &[u8], mut index: i32) -> String {
    let mut classes = Vec::new();
    for _ in 0..8 {
        if index == 0 {
            break;
        }
        let Ok(slot) = usize::try_from(index) else {
            let import = usize::try_from(!index).ok().and_then(|i| pkg.imports.get(i));
            classes.push(format!("import:{}", import.map_or("?", |import| pkg.name(import.class_name))));
            break;
        };
        let Some(exp) = pkg.exports.get(slot - 1) else { break };
        classes.push(object_name(pkg, exp.class_index).to_owned());
        let material = pkg.read_object(data, exp).ok().and_then(|object| {
            object.properties.iter().find(|p| pkg.name(p.name) == "Material").and_then(|p| match p.value {
                TagValue::Object(next) => Some(next),
                _ => None,
            })
        });
        match material {
            Some(next) => index = next,
            None => break,
        }
    }
    if classes.is_empty() { "-".to_owned() } else { classes.join(">") }
}

fn class_name(pkg: &Package, class_index: i32) -> &str {
    if class_index == 0 { "Class" } else { object_name(pkg, class_index) }
}

fn object_name(pkg: &Package, index: i32) -> &str {
    match index {
        0 => "None",
        _ => pkg.object_name(index).unwrap_or("<invalid>"),
    }
}

fn dump(pkg: &Package, out: &mut impl Write) -> std::io::Result<()> {
    let s = &pkg.summary;
    writeln!(out, "version {} licensee {} flags {:#010x}", s.version, s.licensee, s.package_flags)?;
    let guid: String = s.guid.iter().map(|b| format!("{b:02x}")).collect();
    writeln!(out, "guid {guid}")?;
    for (i, g) in s.generations.iter().enumerate() {
        writeln!(out, "generation {i}: exports {} names {}", g.export_count, g.name_count)?;
    }

    writeln!(out, "\nnames ({}) at {:#x}", pkg.names.len(), s.name_offset)?;
    for (i, n) in pkg.names.iter().enumerate() {
        writeln!(out, "{i:6} {:#010x} {}", n.flags, n.text)?;
    }

    writeln!(out, "\nimports ({}) at {:#x}", pkg.imports.len(), s.import_offset)?;
    for (i, imp) in pkg.imports.iter().enumerate() {
        writeln!(
            out,
            "{:6} {}.{} {} outer {}",
            -(i as i64) - 1,
            pkg.name(imp.class_package),
            pkg.name(imp.class_name),
            pkg.name(imp.object_name),
            object_name(pkg, imp.package_index),
        )?;
    }

    writeln!(out, "\nexports ({}) at {:#x}", pkg.exports.len(), s.export_offset)?;
    for (i, exp) in pkg.exports.iter().enumerate() {
        writeln!(
            out,
            "{:6} {} {} super {} outer {} flags {:#010x} data {:#x}+{:#x}",
            i + 1,
            class_name(pkg, exp.class_index),
            pkg.name(exp.object_name),
            object_name(pkg, exp.super_index),
            object_name(pkg, exp.package_index),
            exp.object_flags,
            exp.serial_offset,
            exp.serial_size,
        )?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_core_u() {
        let Some(dir) = std::env::var_os("P2_GAME_DIR") else {
            eprintln!("P2_GAME_DIR unset, skipping");
            return;
        };
        let data = std::fs::read(Path::new(&dir).join("System/Core.u")).unwrap();
        let pkg = Package::parse(&data).unwrap();
        let mut out = Vec::new();
        dump(&pkg, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("version 118 licensee 2"));
        assert!(!text.contains("<invalid>"));
    }
}
