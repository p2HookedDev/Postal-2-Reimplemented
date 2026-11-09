//! The file is a magic, a version, then the [`SaveGame`] in `postcard`; references are indices/paths.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::ai_move::LatentMove;
use crate::memory::{self, Mem, Shape};
use crate::states::ObjectState;
use crate::{Error, HostCommand, LoaderRef, NameId, ObjectId, Prop, Value, Vm};

const MAGIC: &[u8; 8] = b"P2RSAVE\0";
/// The format's version (u32 little-endian after the magic): a save of another version is refused.
const VERSION: u32 = 3;
const TRANSIENT: &str = "Transient.";

pub fn save_file_name(slot: i32) -> String {
    format!("Save{slot}.p2save")
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("not a save game")]
    BadMagic,
    #[error("save game version {0} (this build reads {VERSION})")]
    Version(u32),
    #[error("save game data is not valid: {0}")]
    Data(#[from] postcard::Error),
}

/// `map` stays the first field: [`save_map`] reads only it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveGame {
    pub map: String,
    /// The level URL's options (`Game=...`): the engine saves its level with its URL, so a loaded game travels on
    /// as the same game.
    pub options: Vec<String>,
    names: Vec<String>,
    externals: Vec<(String, String)>,
    objects: Vec<SavedObject>,
}

/// An object of the level. Object words in `mem` hold a reference ([`SaveGame`]'s order: 0 `None`, then the saved
/// objects from 1, then the externals); name words an index into the names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SavedObject {
    path: String,
    class: String,
    mem: Mem,
    state: Option<SavedState>,
    probes_disabled: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SavedState {
    state: (String, u32),
    name: u32,
    code_state: (String, u32),
    code: Option<u32>,
    sleep: f32,
    finish_anim: Option<(u32, i32)>,
    finish_interpolation: u32,
    latent_move: Option<LatentMove>,
}

pub fn save_map(data: &[u8]) -> Result<String, SaveError> {
    Ok(postcard::take_from_bytes::<String>(body(data)?)?.0)
}

fn body(data: &[u8]) -> Result<&[u8], SaveError> {
    let rest = data.strip_prefix(MAGIC).ok_or(SaveError::BadMagic)?;
    let (version, body) = rest.split_first_chunk::<4>().ok_or(SaveError::BadMagic)?;
    match u32::from_le_bytes(*version) {
        VERSION => Ok(body),
        other => Err(SaveError::Version(other)),
    }
}

impl SaveGame {
    pub fn encode(&self) -> Result<Vec<u8>, SaveError> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        Ok(postcard::to_extend(self, bytes)?)
    }

    pub fn decode(data: &[u8]) -> Result<Self, SaveError> {
        Ok(postcard::from_bytes(body(data)?)?)
    }
}

impl<L: LoaderRef> Vm<L> {
    pub fn set_save_dir(&mut self, dir: std::path::PathBuf) {
        self.host.save_dir = Some(dir);
    }

    pub(crate) fn save_exists(&self, slot: i32) -> bool {
        self.host.saved_slots.contains(&slot) || self.host.save_dir.as_ref().is_some_and(|dir| dir.join(save_file_name(slot)).is_file())
    }

    /// Retail erases the slot's file; ours moves it to `Deleted` in the save folder so it is recoverable.
    /// A save of the same slot deleted later replaces it.
    pub(crate) fn delete_save(&mut self, slot: i32) -> bool {
        let Some(dir) = &self.host.save_dir else { return false };
        let (name, deleted) = (save_file_name(slot), dir.join("Deleted"));
        let moved = std::fs::create_dir_all(&deleted).is_ok() && std::fs::rename(dir.join(&name), deleted.join(&name)).is_ok();
        if moved {
            self.host.saved_slots.remove(&slot);
        }
        moved
    }

    /// Local time, `YYYY-MM-DD : HH:MM`. The save menu asks for `Save<N>.usa`,
    /// which is our [`save_file_name`] of slot `N`.
    pub(crate) fn file_time_text(&self, name: &str) -> String {
        let slot = name.get(..4).filter(|prefix| prefix.eq_ignore_ascii_case("save")).and_then(|_| name[4..].rsplit_once('.')).and_then(|(slot, _)| slot.parse().ok());
        let name = slot.map_or_else(|| name.to_owned(), save_file_name);
        let Some(dir) = &self.host.save_dir else { return String::new() };
        let Ok(modified) = std::fs::metadata(dir.join(name)).and_then(|file| file.modified()) else { return String::new() };
        chrono::DateTime::<chrono::Local>::from(modified).format("%Y-%m-%d : %H:%M").to_string()
    }

    /// Whether retail sends the events to actors only is (unverified).
    pub(crate) fn save_game(&mut self, slot: i32) -> Result<bool, Error> {
        if self.level.is_none() {
            return Ok(false);
        }
        self.each_live_actor("PreSaveGame")?;
        let save = self.snapshot();
        self.each_live_actor("PostSaveGame")?;
        self.host.saved_slots.insert(slot);
        self.host.commands.push(HostCommand::SaveGame { slot, save: Box::new(save) });
        Ok(true)
    }

    fn each_live_actor(&mut self, event: &str) -> Result<(), Error> {
        let actors: Vec<ObjectId> = self.actors().collect();
        for actor in actors {
            if !self.store.is_deleted(actor) {
                self.event(actor, event, Vec::new())?;
            }
        }
        Ok(())
    }

    pub fn post_load_game(&mut self) -> Vec<(String, Error)> {
        let actors: Vec<ObjectId> = self.actors().collect();
        let mut errors = Vec::new();
        for actor in actors {
            if !self.store.is_deleted(actor)
                && let Err(e) = self.event(actor, "PostLoadGame", Vec::new())
            {
                errors.push((self.store.object(actor).path.clone(), e));
            }
        }
        errors
    }

    fn is_transient_object(&self, object: ObjectId) -> bool {
        let object = self.store.object(object);
        object.path.starts_with(TRANSIENT) && object.class.is_some_and(|class| !self.store.class(class).is_actor())
    }

    fn object_refs(&mut self, object: ObjectId, out: &mut Vec<ObjectId>) {
        let Some(class) = self.store.object(object).class else { return };
        let layout = self.store.class(class).mem;
        let mut mem = std::mem::take(&mut self.store.objects[object.0 as usize].mem);
        self.store.memory.visit_refs(layout, &mut mem, &mut |shape, word| {
            if shape == Shape::Object
                && let Some(found) = memory::object(*word)
            {
                out.push(found);
            }
        });
        self.store.objects[object.0 as usize].mem = mem;
    }

    fn kept_transient(&mut self) -> HashSet<ObjectId> {
        let roots = [self.host.client.master, self.host.client.viewport, self.media.canvas.object];
        let mut stack: Vec<ObjectId> = roots.into_iter().flatten().collect();
        for class in 0..self.store.classes.len() {
            let layout = self.store.classes[class].mem;
            let mut defaults = std::mem::take(&mut self.store.classes[class].defaults);
            self.store.memory.visit_refs(layout, &mut defaults, &mut |shape, word| {
                if shape == Shape::Object
                    && let Some(found) = memory::object(*word)
                {
                    stack.push(found);
                }
            });
            self.store.classes[class].defaults = defaults;
        }
        let mut kept = HashSet::new();
        while let Some(object) = stack.pop() {
            if self.is_transient_object(object) && kept.insert(object) {
                self.object_refs(object, &mut stack);
            }
        }
        kept
    }

    fn snapshot(&mut self) -> SaveGame {
        let map_prefix = format!("{}.", self.map_name.to_ascii_lowercase());
        let mut saved: Vec<ObjectId> = self
            .object_ids()
            .filter(|&o| {
                let object = self.store.object(o);
                match object.class {
                    Some(class) if self.store.class(class).is_actor() => self.store.actor_ids.contains(&o) && !self.store.is_deleted(o),
                    Some(_) => object.path.to_ascii_lowercase().starts_with(&map_prefix),
                    None => false,
                }
            })
            .collect();
        let mut index: HashMap<ObjectId, u32> = saved.iter().enumerate().map(|(i, &o)| (o, i as u32)).collect();
        let kept = self.kept_transient();
        let mut next = 0;
        let mut refs = Vec::new();
        while next < saved.len() {
            refs.clear();
            self.object_refs(saved[next], &mut refs);
            for &found in &refs {
                if !index.contains_key(&found) && !kept.contains(&found) && self.is_transient_object(found) {
                    index.insert(found, saved.len() as u32);
                    saved.push(found);
                }
            }
            next += 1;
        }
        let mut names: Vec<String> = Vec::new();
        let mut name_index: HashMap<NameId, u32> = HashMap::new();
        let mut externals: Vec<(String, String)> = Vec::new();
        let mut external_index: HashMap<ObjectId, u32> = HashMap::new();
        let count = saved.len() as u32;
        let mut name_of = |vm: &Self, name: NameId| {
            *name_index.entry(name).or_insert_with(|| {
                names.push(vm.store.name(name).to_owned());
                names.len() as u32 - 1
            })
        };
        let mut reference = |vm: &Self, object: Option<ObjectId>| -> u32 {
            let Some(object) = object else { return 0 };
            if let Some(&i) = index.get(&object) {
                return i + 1;
            }
            let found = vm.store.object(object);
            if found.path.starts_with(TRANSIENT) || found.class.is_some_and(|class| vm.store.class(class).is_actor()) {
                return 0;
            }
            count
                + 1
                + *external_index.entry(object).or_insert_with(|| {
                    externals.push((found.path.clone(), vm.store.name(found.class_name).to_owned()));
                    externals.len() as u32 - 1
                })
        };
        let mut objects = Vec::with_capacity(saved.len());
        for &o in &saved {
            let Some(class) = self.store.object(o).class else { continue };
            let mut mem = self.store.object(o).mem.clone();
            self.store.memory.visit_refs(self.store.class(class).mem, &mut mem, &mut |shape, word| {
                *word = match shape {
                    Shape::Object => reference(self, memory::object(*word)),
                    _ => name_of(self, NameId(*word)),
                };
            });
            let package = |vm: &Self, (id, export): (p2_package::PackageId, usize)| (vm.loader.package(id).name.clone(), export as u32);
            let state = self.script.states.objects.get(&o).map(|s| SavedState {
                state: package(self, s.state),
                name: name_of(self, s.name),
                code_state: package(self, s.code_state),
                code: s.code.map(|code| code as u32),
                sleep: s.sleep,
                finish_anim: s.finish_anim.map(|(actor, channel)| (reference(self, Some(actor)), channel)),
                finish_interpolation: reference(self, s.finish_interpolation),
                latent_move: s.latent_move,
            });
            let class = self.store.class(class);
            objects.push(SavedObject {
                path: self.store.object(o).path.clone(),
                class: format!("{}.{}", self.loader.package(class.package).name, self.store.name(class.name)),
                mem,
                state,
                probes_disabled: self.script.states.probes_disabled.get(&o).copied().unwrap_or(0),
            });
        }
        SaveGame { map: self.map_name.clone(), options: self.url_options.clone(), names, externals, objects }
    }

    /// Objects whose class is missing or whose memory size differs (other script packages),
    /// or whose array lengths pass their data (a damaged file), are skipped and logged.
    pub fn restore(&mut self, map: p2_package::PackageId, save: &SaveGame) -> Result<(), Error> {
        let map_actors = self.load_level(map)?;
        let level = self.level.ok_or(Error::Unsupported("map without a LevelInfo".to_owned()))?;
        crate::natives::update_time(self, level);
        self.travelling = Default::default();
        let map_prefix = format!("{}.", self.map_name.to_ascii_lowercase());
        let names: Vec<NameId> = save.names.iter().map(|name| self.intern(name)).collect();
        let mut ids: Vec<Option<ObjectId>> = Vec::with_capacity(save.objects.len());
        for saved in &save.objects {
            let class = saved.class.split_once('.').and_then(|(package, name)| self.find_class(package, name).ok());
            let Some(class) = class else {
                self.diagnostics.log.push(format!("Load: class {} of {} not found", saved.class, saved.path));
                ids.push(None);
                continue;
            };
            let key = saved.path.to_ascii_lowercase();
            let existing = key
                .starts_with(&map_prefix)
                .then(|| self.store.object_ids.get(&key).copied())
                .flatten()
                .filter(|&o| self.store.object(o).class == Some(class));
            ids.push(Some(existing.unwrap_or_else(|| {
                let name = saved.path.rsplit('.').next().unwrap_or(&saved.path);
                let free = !self.store.object_ids.contains_key(&format!("{TRANSIENT}{name}").to_ascii_lowercase());
                self.new_object(class, free.then_some(name))
            })));
        }
        let externals: Vec<ObjectId> = save.externals.iter().map(|(path, class)| self.stub(path, class)).collect();
        let reference = |word: u32| -> u32 {
            let object = match (word as usize).checked_sub(1) {
                None => None,
                Some(i) if i < ids.len() => ids[i],
                Some(i) => externals.get(i - ids.len()).copied(),
            };
            memory::object_word(object)
        };
        let name = |word: u32| names.get(word as usize).copied().unwrap_or(NameId::NONE);
        for (saved, &id) in save.objects.iter().zip(&ids) {
            let Some(id) = id else { continue };
            let Some(class) = self.store.object(id).class else { continue };
            let defaults = &self.store.class(class).defaults;
            let layout = self.store.class(class).mem;
            if saved.mem.words.len() != defaults.words.len() || saved.mem.heap.len() != defaults.heap.len() || !self.store.memory.arrays_fit(layout, &saved.mem) {
                self.diagnostics.log.push(format!("Load: {} does not match its class {}, skipped", saved.path, saved.class));
                continue;
            }
            let mut mem = saved.mem.clone();
            self.store.memory.visit_refs(self.store.class(class).mem, &mut mem, &mut |shape, word| {
                *word = match shape {
                    Shape::Object => reference(*word),
                    _ => name(*word).0,
                };
            });
            self.store.objects[id.0 as usize].mem = mem;
            if let Some(state) = &saved.state {
                self.restore_state(id, state, &reference, &name);
            }
            if saved.probes_disabled != 0 {
                self.script.states.probes_disabled.insert(id, saved.probes_disabled);
            }
        }
        let restored: HashSet<ObjectId> = ids.iter().flatten().copied().collect();
        for actor in map_actors {
            if !restored.contains(&actor) {
                self.store.set(actor, Prop::bDeleteMe, Value::Bool(true));
                self.store.actor_ids.remove(&actor);
                self.store.dynamic_ids.removed(actor);
            }
        }
        self.store.dynamic_ids.forget_linked_statics();
        self.url_options.clone_from(&save.options);
        self.media.subtitle_sounds = crate::util_natives::SubtitleSounds::default();
        if !self.url_options.iter().any(|option| option.to_ascii_lowercase().starts_with("game="))
            && let Some(game) = self.level.and_then(|level| self.store.object_of(level, Prop::Game))
            && let Some(class) = self.store.object(game).class
        {
            let class = self.store.class(class);
            self.url_options.push(format!("Game={}.{}", self.loader.package(class.package).name, self.store.name(class.name)));
        }
        Ok(())
    }

    fn restore_state(&mut self, object: ObjectId, saved: &SavedState, reference: &impl Fn(u32) -> u32, name: &impl Fn(u32) -> NameId) {
        let export = |vm: &Self, (package, export): &(String, u32)| {
            let (id, export) = (vm.loader.loaded_id(package)?, *export as usize);
            (export < vm.loader.package(id).package.exports.len()).then_some((id, export))
        };
        let (Some(state), Some(code_state)) = (export(self, &saved.state), export(self, &saved.code_state)) else {
            self.diagnostics.log.push(format!("Load: state of {} not found", self.store.object(object).path));
            return;
        };
        let key = self.state_key(state);
        let entry = ObjectState {
            state,
            name: name(saved.name),
            code_state,
            code: saved.code.map(|code| code as usize),
            sleep: saved.sleep,
            finish_anim: saved.finish_anim.and_then(|(actor, channel)| Some((memory::object(reference(actor))?, channel))),
            finish_interpolation: memory::object(reference(saved.finish_interpolation)),
            latent_move: saved.latent_move,
            generation: 0,
            key,
        };
        self.script.states.enter(object, entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Array, Heap};

    fn sample() -> SaveGame {
        let mem = Mem {
            words: vec![1, 0, 3, 0x3f80_0000],
            heap: vec![
                Heap::Str("Hello, \"world\"".to_owned()),
                Heap::Array(Array { len: 2, mem: Mem { words: vec![2, 1], heap: vec![Heap::Str(String::new())] } }),
            ],
        };
        let state = SavedState {
            state: ("Postal2Game".to_owned(), 12),
            name: 1,
            code_state: ("Engine".to_owned(), 7),
            code: Some(4),
            sleep: 0.5,
            finish_anim: Some((1, 0)),
            finish_interpolation: 0,
            latent_move: Some(LatentMove::MoveToward),
        };
        SaveGame {
            map: "Suburbs-3".to_owned(),
            options: vec!["Game=GameTypes.GameSinglePlayer".to_owned()],
            names: vec!["None".to_owned(), "Walking".to_owned()],
            externals: vec![("Engine.Actor".to_owned(), "Class".to_owned())],
            objects: vec![
                SavedObject { path: "Suburbs-3.Pawn3".to_owned(), class: "Postal2Game.P2Pawn".to_owned(), mem, state: Some(state), probes_disabled: 1 << 40 },
                SavedObject { path: "Transient.Foo0".to_owned(), class: "Engine.Foo".to_owned(), mem: Mem::default(), state: None, probes_disabled: 0 },
            ],
        }
    }

    #[test]
    fn round_trips() {
        let save = sample();
        let bytes = save.encode().unwrap();
        assert_eq!(SaveGame::decode(&bytes).unwrap(), save);
        assert_eq!(save_map(&bytes).unwrap(), "Suburbs-3");
    }

    #[test]
    fn refuses_bad_files() {
        let bytes = sample().encode().unwrap();
        assert!(matches!(SaveGame::decode(b"nonsense"), Err(SaveError::BadMagic)));
        let mut other = bytes.clone();
        other[MAGIC.len()] = 99;
        assert!(matches!(SaveGame::decode(&other), Err(SaveError::Version(99))));
        for cut in [MAGIC.len() + 4, bytes.len() / 2, bytes.len() - 1] {
            assert!(SaveGame::decode(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        let mut huge = bytes[..MAGIC.len() + 4].to_vec();
        huge.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x0f]);
        assert!(matches!(SaveGame::decode(&huge), Err(SaveError::Data(_))));
    }

    #[test]
    fn refuses_old_versions() {
        let mut old = MAGIC.to_vec();
        old.extend_from_slice(&2u32.to_le_bytes());
        old.extend_from_slice(&9u32.to_le_bytes());
        old.extend_from_slice(b"Suburbs-3");
        assert!(matches!(SaveGame::decode(&old), Err(SaveError::Version(2))));
        assert!(matches!(save_map(&old), Err(SaveError::Version(2))));
    }

    #[test]
    fn console_commands() {
        let empty = std::env::temp_dir();
        let loader = p2_package::Loader::new(&empty, &[]).unwrap();
        let mut vm = Vm::new(&loader);
        let command = |vm: &mut Vm<&p2_package::Loader>, text: &str| match vm.call_native("ConsoleCommand", None, &mut [Value::Str(text.to_owned())]) {
            Ok(Some(Value::Str(answer))) => answer,
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(command(&mut vm, "SaveGame 3"), "0");
        let dir = empty.join(format!("p2-save-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(save_file_name(7)), sample().encode().unwrap()).unwrap();
        vm.set_save_dir(dir.clone());
        assert_eq!(command(&mut vm, "CHECKSAVEGAME 7"), "1");
        assert_eq!(command(&mut vm, "CHECKSAVEGAME 8"), "0");
        assert_eq!(command(&mut vm, "checksavegame nonsense"), "0");
        assert!(command(&mut vm, "GETGMTIME").parse::<u64>().unwrap() > 1_700_000_000);
        assert!(vm.delete_save(7));
        assert_eq!(command(&mut vm, "CHECKSAVEGAME 7"), "0");
        assert!(dir.join("Deleted").join(save_file_name(7)).is_file());
        assert!(!vm.delete_save(7));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn latent_moves_keep_their_numbers() {
        let moves = [
            LatentMove::MoveTo,
            LatentMove::MoveToward,
            LatentMove::FinishRotation,
            LatentMove::WaitForLanding,
            LatentMove::MoveToWithRadius,
            LatentMove::MoveTowardWithRadius,
            LatentMove::WaitToSeeEnemy,
        ];
        for (number, latent) in moves.into_iter().enumerate() {
            let bytes = postcard::to_extend(&latent, Vec::new()).unwrap();
            assert_eq!(bytes, [number as u8]);
            assert_eq!(postcard::from_bytes::<LatentMove>(&bytes).unwrap(), latent);
        }
    }
}
