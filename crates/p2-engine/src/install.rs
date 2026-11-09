use std::fs;
use std::path::{Path, PathBuf};

const REQUIRED: &[&[&str]] = &[&["Core.u"], &["Engine.u"], &["Postal2.ini", "Default.ini"]];

const STEAM_APP_ID: u32 = 223470;

const STEAM_FOLDERS: [&str; 2] = ["POSTAL2Complete", "POSTAL 2"];

const COMMON_FOLDERS: [&str; 4] = [
    r"C:\Program Files (x86)\Steam\steamapps\common\POSTAL2Complete",
    r"C:\Program Files (x86)\Steam\steamapps\common\POSTAL 2",
    r"C:\GOG Games\POSTAL 2",
    r"C:\Program Files (x86)\GOG Galaxy\Games\POSTAL 2",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallInfo {
    pub dir: PathBuf,
    pub installed_type: Option<String>,
    pub installed_version: Option<String>,
    pub paradise_lost: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("This folder is not a POSTAL 2 install ({missing} is missing). Pick the folder that contains the System and Maps folders.")]
    NotAnInstall {
        missing: &'static str,
    },
}

/// A subfolder of an install (the player picked `System` or `Maps`) resolves to its parent.
pub fn validate(dir: &Path) -> Result<InstallInfo, InstallError> {
    match missing_file(dir) {
        None => Ok(InstallInfo::read(dir)),
        Some(missing) => match dir.parent() {
            Some(parent) if missing_file(parent).is_none() => Ok(InstallInfo::read(parent)),
            _ => Err(InstallError::NotAnInstall { missing }),
        },
    }
}

fn missing_file(dir: &Path) -> Option<&'static str> {
    let system = dir.join("System");
    REQUIRED.iter().find(|names| !names.iter().any(|name| system.join(name).is_file())).map(|names| names[0])
}

impl InstallInfo {
    fn read(dir: &Path) -> Self {
        let system = dir.join("System");
        let version = fs::read(system.join("Version.ini")).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default();
        Self {
            dir: dir.to_path_buf(),
            installed_type: ini_value(&version, "InstalledType"),
            installed_version: ini_value(&version, "InstalledVersion"),
            paradise_lost: dir.join("Paradise Lost").is_dir(),
        }
    }

    pub fn edition(&self) -> String {
        let mut parts = vec!["POSTAL 2".to_owned()];
        parts.extend(self.installed_type.clone());
        parts.extend(self.installed_version.as_ref().map(|version| format!("version {version}")));
        if self.paradise_lost {
            parts.push("Paradise Lost found".to_owned());
        }
        parts.join(", ")
    }
}

fn ini_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        name.trim().eq_ignore_ascii_case(key).then(|| value.trim().to_owned())
    })
}

pub fn detect() -> Vec<PathBuf> {
    let exe = std::env::current_exe().ok();
    let near_exe = exe.iter().flat_map(|exe| exe.ancestors().skip(1)).map(Path::to_path_buf);
    let candidates = near_exe.chain(steam_paths()).chain(gog_paths()).chain(COMMON_FOLDERS.map(PathBuf::from));
    let (mut found, mut seen) = (Vec::new(), Vec::new());
    for info in candidates.filter_map(|dir| validate(&dir).ok()) {
        let key = fs::canonicalize(&info.dir).unwrap_or_else(|_| info.dir.clone());
        if !seen.contains(&key) {
            seen.push(key);
            found.push(info.dir);
        }
    }
    found
}

fn steam_paths() -> Vec<PathBuf> {
    let Ok(steam) = steamlocate::SteamDir::locate() else {
        return Vec::new();
    };
    let app = steam.find_app(STEAM_APP_ID).ok().flatten().map(|(app, library)| library.resolve_app_dir(&app));
    let libraries = steam.libraries().into_iter().flatten().filter_map(Result::ok).map(|library| library.path().to_path_buf());
    app.into_iter().chain(steam_folders(libraries)).collect()
}

/// Stands in for retail's Steam ownership check: Steam keeps no readable list of owned apps, so an app counts when it
/// is installed in any library, and a DLC when POSTAL 2's manifest lists one of its depots.
pub fn steam_installed_apps() -> Vec<u32> {
    let Ok(steam) = steamlocate::SteamDir::locate() else {
        return Vec::new();
    };
    let mut apps: Vec<u32> = steam.libraries().into_iter().flatten().filter_map(Result::ok).flat_map(|library| library.app_ids().to_vec()).collect();
    if let Ok(Some((app, _))) = steam.find_app(STEAM_APP_ID) {
        apps.extend(app.installed_depots.values().filter_map(|depot| depot.dlc_app_id).filter_map(|id| u32::try_from(id).ok()));
    }
    apps
}

fn steam_folders(libraries: impl IntoIterator<Item = PathBuf>) -> impl Iterator<Item = PathBuf> {
    libraries.into_iter().flat_map(|library| STEAM_FOLDERS.map(|name| library.join("steamapps").join("common").join(name)))
}

#[cfg(windows)]
fn gog_paths() -> Vec<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::HKEY_LOCAL_MACHINE;
    let Ok(games) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\WOW6432Node\GOG.com\Games") else {
        return Vec::new();
    };
    games
        .enum_keys()
        .filter_map(Result::ok)
        .filter_map(|id| games.open_subkey(id).ok())
        .filter(|game| game.get_value::<String, _>("gameName").is_ok_and(|name| name.to_uppercase().contains("POSTAL 2")))
        .filter_map(|game| game.get_value::<String, _>("path").ok())
        .map(PathBuf::from)
        .collect()
}

#[cfg(not(windows))]
fn gog_paths() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_install(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("p2-install-test-{}-{name}", std::process::id()));
        for file in files {
            let path = dir.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        fs::create_dir_all(dir.join("Maps")).unwrap();
        dir
    }

    const VALID: [&str; 3] = [r"System\Core.u", r"System\Engine.u", r"System\Postal2.ini"];

    #[test]
    fn valid_install() {
        let dir = fake_install("valid", &VALID);
        let info = validate(&dir).unwrap();
        assert_eq!(info, InstallInfo { dir, installed_type: None, installed_version: None, paradise_lost: false });
    }

    #[test]
    fn missing_engine_is_not_an_install() {
        let dir = fake_install("no-engine", &[r"System\Core.u", r"System\Postal2.ini"]);
        let error = validate(&dir).unwrap_err();
        assert!(matches!(error, InstallError::NotAnInstall { missing: "Engine.u" }));
        assert!(error.to_string().starts_with("This folder is not a POSTAL 2 install"));
    }

    #[test]
    fn subfolder_resolves_to_parent() {
        let dir = fake_install("subfolder", &VALID);
        assert_eq!(validate(&dir.join("System")).unwrap().dir, dir);
        assert_eq!(validate(&dir.join("Maps")).unwrap().dir, dir);
    }

    #[test]
    fn edition_facts() {
        let dir = fake_install("edition", &[r"System\Core.u", r"System\Engine.u", r"System\Default.ini", r"System\xPatch.ini"]);
        fs::write(dir.join(r"System\Version.ini"), "[Version]\r\nInstalledType=Steam\r\ninstalledversion = 5100\r\n").unwrap();
        fs::create_dir_all(dir.join(r"Paradise Lost\System")).unwrap();
        let info = validate(&dir).unwrap();
        assert_eq!((info.installed_type.as_deref(), info.installed_version.as_deref()), (Some("Steam"), Some("5100")));
        assert!(info.paradise_lost);
        assert_eq!(info.edition(), "POSTAL 2, Steam, version 5100, Paradise Lost found");
    }

    #[test]
    fn edition_line_without_version_ini() {
        let info = InstallInfo { dir: PathBuf::new(), installed_type: None, installed_version: None, paradise_lost: false };
        assert_eq!(info.edition(), "POSTAL 2");
    }

    #[test]
    fn steam_folders_per_library() {
        let folders: Vec<_> = steam_folders([PathBuf::from("A"), PathBuf::from("B")]).collect();
        let expected = |library: &str, name: &str| Path::new(library).join("steamapps").join("common").join(name);
        assert_eq!(
            folders,
            [expected("A", "POSTAL2Complete"), expected("A", "POSTAL 2"), expected("B", "POSTAL2Complete"), expected("B", "POSTAL 2")]
        );
    }
}
