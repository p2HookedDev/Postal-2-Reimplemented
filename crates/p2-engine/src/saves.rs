//! Save files in `%APPDATA%\p2-reimplemented\Save`, written on the IO pool through a temp file and
//! a rename, so a crash leaves the old save whole; reads and exit wait for queued writes.

use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use bevy::tasks::{IoTaskPool, Task, block_on};
use p2_vm::{SaveGame, TravelUrl, save_file_name};

pub fn save_dir() -> Option<PathBuf> {
    Some(crate::game_dir::config_folder()?.join("Save"))
}

fn save_file(slot: i32) -> Option<PathBuf> {
    Some(save_dir()?.join(save_file_name(slot)))
}

pub fn load_slot(url: &str) -> Option<i32> {
    TravelUrl::parse(url).options.iter().find_map(|option| {
        let (key, value) = option.split_once('=')?;
        key.trim().eq_ignore_ascii_case("load").then(|| value.trim().parse().ok()).flatten()
    })
}

pub fn read(slot: i32) -> std::io::Result<Vec<u8>> {
    finish_writes();
    std::fs::read(save_file(slot).ok_or_else(|| std::io::Error::other("no config folder"))?)
}

pub fn saved_map(slot: i32) -> Option<String> {
    finish_writes();
    let mut start = Vec::new();
    std::fs::File::open(save_file(slot)?).ok()?.take(4096).read_to_end(&mut start).ok()?;
    p2_vm::save_map(&start).ok()
}

static LAST_WRITE: Mutex<Option<Task<()>>> = Mutex::new(None);

pub fn write(slot: i32, save: Box<SaveGame>) {
    let mut last = LAST_WRITE.lock().unwrap_or_else(PoisonError::into_inner);
    let before = last.take();
    *last = Some(IoTaskPool::get().spawn(async move {
        if let Some(before) = before {
            before.await;
        }
        if catch_unwind(AssertUnwindSafe(|| write_now(slot, &save))).is_err() {
            bevy::log::warn!("save game: slot {slot} not written: the writer panicked");
        }
    }));
}

/// The lock is held through the wait, so a save queued meanwhile starts after it; the writers never take the lock.
pub fn finish_writes() {
    let mut last = LAST_WRITE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(last) = last.take() {
        block_on(last);
    }
}

fn write_now(slot: i32, save: &SaveGame) {
    let started = std::time::Instant::now();
    let written = save_file(slot).ok_or_else(|| std::io::Error::other("no config folder")).and_then(|file| {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = save.encode().map_err(std::io::Error::other)?;
        let temp = file.with_extension("tmp");
        std::fs::write(&temp, &data)?;
        std::fs::rename(&temp, &file)?;
        Ok(data.len())
    });
    match written {
        Ok(bytes) => bevy::log::info!("save game: slot {slot}, {} KB, written in {:?}", bytes / 1024, started.elapsed()),
        Err(error) => bevy::log::warn!("save game: slot {slot} not written: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::load_slot;

    #[test]
    fn load_urls() {
        assert_eq!(load_slot("?load=21"), Some(21));
        assert_eq!(load_slot("?Game=GameTypes.AWPGameInfo?LOAD=3"), Some(3));
        assert_eq!(load_slot("Suburbs-3?peer#Telepad"), None);
        assert_eq!(load_slot("?load=x"), None);
    }
}
