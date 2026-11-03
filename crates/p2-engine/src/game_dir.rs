use std::path::{Path, PathBuf};

pub fn is_game_dir(path: &Path) -> bool {
    crate::install::validate(path).is_ok_and(|info| info.dir == path)
}

pub(crate) fn config_folder() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("p2-reimplemented"))
}

fn config_file() -> Option<PathBuf> {
    Some(config_folder()?.join("game_dir.txt"))
}

pub fn known_game_dir() -> Option<PathBuf> {
    let from_env = std::env::var_os("P2_GAME_DIR").map(PathBuf::from);
    let saved = config_file()
        .and_then(|file| std::fs::read_to_string(file).ok())
        .map(|text| PathBuf::from(text.trim()));
    from_env.into_iter().chain(saved).find(|dir| is_game_dir(dir))
}

pub fn save_game_dir(dir: &Path) -> std::io::Result<()> {
    let file = config_file().ok_or_else(|| std::io::Error::other("no config folder"))?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(file, dir.display().to_string())
}
