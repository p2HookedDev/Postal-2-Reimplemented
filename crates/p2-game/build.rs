use std::path::{Path, PathBuf};

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:67108864");
    }
    let Some(profile_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from).and_then(|out| Some(out.ancestors().nth(3)?.to_path_buf())) else { return };
    copy_dlls(&profile_dir);
}

fn copy_dlls(profile_dir: &Path) {
    let deps = profile_dir.join("deps");
    let mut found = Vec::new();
    found.extend(dlls_named(&deps, "bevy_dylib-"));
    if let Some(std_dir) = std_dll_dir() {
        found.extend(dlls_named(&std_dir, "std-"));
    }
    if !found.iter().any(|dll| file_name_starts(dll, "bevy_dylib-")) {
        // Bevy may still be compiling beside this script: a missing file counts as changed, so the next build copies it.
        println!("cargo:rerun-if-changed={}", deps.join("bevy_dylib-").display());
        println!("cargo:warning=bevy_dylib is not built yet: build once more to place it beside the exe");
    }
    for dll in found {
        println!("cargo:rerun-if-changed={}", dll.display());
        if let Some(name) = dll.file_name() {
            let to = profile_dir.join(name);
            let stale = std::fs::metadata(&to).and_then(|to| to.modified()).ok() < std::fs::metadata(&dll).and_then(|from| from.modified()).ok();
            if stale && let Err(error) = std::fs::copy(&dll, &to) {
                println!("cargo:warning=could not copy {}: {error}", dll.display());
            }
        }
    }
}

fn std_dll_dir() -> Option<PathBuf> {
    let rustc = std::env::var_os("RUSTC")?;
    let sysroot = std::process::Command::new(rustc).args(["--print", "sysroot"]).output().ok()?;
    let target = std::env::var("TARGET").ok()?;
    Some(PathBuf::from(String::from_utf8(sysroot.stdout).ok()?.trim()).join("lib").join("rustlib").join(target).join("lib"))
}

fn dlls_named(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| file_name_starts(path, prefix) && path.extension().is_some_and(|extension| extension == "dll"))
        .collect()
}

fn file_name_starts(path: &Path, prefix: &str) -> bool {
    path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with(prefix))
}
