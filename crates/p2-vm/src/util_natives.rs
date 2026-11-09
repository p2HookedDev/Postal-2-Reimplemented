use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use p2_package::Ini;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::{tuple_fields, unquote};
use crate::native_id::Native;
use crate::interp::{float, int, truthy};
use crate::natives::text_arg;
use crate::{Error, LoaderRef, NameId, ObjectId, Prop, Value, Vm};

static INI_CACHE: Mutex<BTreeMap<PathBuf, Option<Arc<Ini>>>> = Mutex::new(BTreeMap::new());
static REGISTRY_CACHE: Mutex<BTreeMap<(PathBuf, String), Arc<Vec<RegistryEntry>>>> = Mutex::new(BTreeMap::new());
static CLIPBOARD: Mutex<String> = Mutex::new(String::new());

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RegistryEntry {
    name: String,
    class: String,
    meta_class: String,
    description: String,
}

fn set_out(args: &mut [Value], n: usize, value: Value) {
    if let Some(out) = args.get_mut(n) {
        *out = value;
    }
}

fn decode(bytes: &[u8]) -> String {
    match bytes.strip_prefix(&[0xFF, 0xFE]) {
        Some(rest) => String::from_utf16_lossy(&rest.as_chunks::<2>().0.iter().map(|&pair| u16::from_le_bytes(pair)).collect::<Vec<_>>()),
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn load_ini(path: &Path) -> Option<Arc<Ini>> {
    let mut cache = INI_CACHE.lock().ok()?;
    cache.entry(path.to_path_buf()).or_insert_with(|| std::fs::read(path).ok().map(|bytes| Arc::new(Ini::parse(&decode(&bytes))))).clone()
}

fn registry_entry(value: &str) -> RegistryEntry {
    let mut entry = RegistryEntry::default();
    for (key, value) in tuple_fields(value) {
        match key.to_ascii_lowercase().as_str() {
            "name" => value.clone_into(&mut entry.name),
            "class" => value.clone_into(&mut entry.class),
            "metaclass" => value.clone_into(&mut entry.meta_class),
            "description" => value.clone_into(&mut entry.description),
            _ => {}
        }
    }
    entry
}

pub(crate) fn subtitle_index(tags: Option<&Value>, tag: &str) -> Option<i32> {
    let Some(Value::Array(tags)) = tags else { return None };
    if tag.is_empty() {
        return None;
    }
    let index = tags.iter().position(|t| matches!(t, Value::Str(t) if t.eq_ignore_ascii_case(tag)))?;
    i32::try_from(index).ok()
}

/// `SubSound` (a config array, thousands long) by lower-case sound path. Script only reads that array's length; a console
/// `set`, a restored save and leaving the level drop this.
#[derive(Debug, Default)]
pub(crate) struct SubtitleSounds {
    manager: Option<ObjectId>,
    by_path: FxHashMap<String, (usize, ObjectId)>,
}

const SUBTITLE_DELAY: f32 = 0.3;

fn subtitle_member(entry: &Value, member: usize) -> Option<&Value> {
    let Value::Struct(members) = entry else { return None };
    members.get(member)
}

/// As in retail's loops, a removal steps over the entry after it, which waits for the next pass.
fn take_expired(entries: &mut Vec<Value>, time: usize, now: f32) -> Vec<Value> {
    let (mut expired, mut at) = (Vec::new(), 0);
    while at < entries.len() {
        if subtitle_member(&entries[at], time).is_some_and(|time| float(time) < now) {
            expired.push(entries.remove(at));
        }
        at += 1;
    }
    expired
}

fn valid_sound(invalid: &mut Vec<Value>, sound: Option<ObjectId>, now: f32) -> bool {
    take_expired(invalid, 1, now);
    !invalid.iter().any(|entry| subtitle_member(entry, 0) == Some(&Value::Object(sound)))
}

fn insert_pending(pending: &mut Vec<Value>, actor: Option<ObjectId>, entry: Value) {
    match pending.iter().position(|old| subtitle_member(old, 1) == Some(&Value::Object(actor))) {
        Some(at) => pending[at] = entry,
        None => pending.push(entry),
    }
}

/// `dir` 0 is `current`, -1 the one before (none before the first), anything else the one after
/// (past the end, the first). An unknown `current` gives the last.
fn next_skin<'a>(names: impl Iterator<Item = &'a str>, prefix: &str, current: &str, dir: i32) -> Option<usize> {
    let (mut first, mut last, mut take_next) = (None, None, false);
    for (index, name) in names.enumerate() {
        if !name.get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix)) {
            continue;
        }
        if take_next {
            return Some(index);
        }
        first.get_or_insert(index);
        if name.eq_ignore_ascii_case(current) {
            match dir {
                -1 => return last,
                0 => return Some(index),
                _ => take_next = true,
            }
        }
        last = Some(index);
    }
    if take_next { first } else { last }
}

fn registry(dir: &Path, language: &str) -> Arc<Vec<RegistryEntry>> {
    let key = (dir.to_path_buf(), language.to_owned());
    if let Ok(cache) = REGISTRY_CACHE.lock()
        && let Some(entries) = cache.get(&key)
    {
        return entries.clone();
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("int") || e.eq_ignore_ascii_case(language)))
        .collect();
    paths.sort_by_cached_key(|path| path.file_name().map(std::ffi::OsStr::to_ascii_lowercase));
    let mut entries = Vec::new();
    for path in paths {
        if let Some(ini) = load_ini(&path) {
            entries.extend(ini.get_all("Public", "Object").map(registry_entry));
        }
    }
    let entries = Arc::new(entries);
    if let Ok(mut cache) = REGISTRY_CACHE.lock() {
        cache.insert(key, entries.clone());
    }
    entries
}

fn wildcard(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some((&'*', rest)) => (0..=n.len()).any(|skip| go(rest, &n[skip..])),
            Some((&c, rest)) => n.split_first().is_some_and(|(&d, tail)| (c == '?' || c.eq_ignore_ascii_case(&d)) && go(rest, tail)),
        }
    }
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    go(&p, &n)
}

/// Retail's curve evaluation (unverified).
fn eval_curve(points: &[(f32, f32)], x: f32) -> f32 {
    match points {
        [] => 0.0,
        [first, ..] if x <= first.0 => first.1,
        [.., last] if x >= last.0 => last.1,
        _ => points
            .windows(2)
            .find(|w| x < w[1].0)
            .map_or(0.0, |w| if w[1].0 > w[0].0 { w[0].1 + (w[1].1 - w[0].1) * (x - w[0].0) / (w[1].0 - w[0].0) } else { w[1].1 }),
    }
}

fn curve_points(value: Option<&Value>) -> Vec<(f32, f32)> {
    let Some(Value::Struct(members)) = value else { return Vec::new() };
    let Some(Value::Array(points)) = members.first() else { return Vec::new() };
    points
        .iter()
        .filter_map(|point| match point {
            Value::Struct(m) => Some((m.first().map_or(0.0, float), m.get(1).map_or(0.0, float))),
            _ => None,
        })
        .collect()
}

fn data_path(data_dir: Option<&Path>, path: &str) -> Option<PathBuf> {
    let path = PathBuf::from(path.replace('\\', "/"));
    if path.is_absolute() {
        return None;
    }
    Some(data_dir?.join("System").join(path))
}

fn data_write_path(data_dir: Option<&Path>, path: &str) -> Option<PathBuf> {
    let path = data_path(data_dir, path)?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    Some(path)
}

fn data_read_path(data_dir: Option<&Path>, path: &str) -> Option<PathBuf> {
    data_path(data_dir, path).filter(|path| path.exists())
}

/// `GameInfo.ParseKillMessage` (unverified): `%k` is the killer, `%o` the victim.
fn parse_kill_message(killer: &str, victim: &str, message: &str) -> String {
    message.replace("%k", killer).replace("%o", victim)
}

impl<L: LoaderRef> Vm<L> {
    #[expect(clippy::unnecessary_wraps, reason = "every native dispatcher has this shape: `natives::call` returns its result")]
    pub(crate) fn util_native(&mut self, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let value = match native {
            Native::Floor => Value::Float(args.first().map_or(0.0, float).floor()),
            Native::Localize => Value::Str(self.localize(&text_arg(args, 0), &text_arg(args, 1), &text_arg(args, 2))),
            Native::GetLanguage => Value::Str(self.language()),
            Native::GetDisplayLanguage => Value::Str(text_arg(args, 0)),
            Native::SetLanguage => {
                self.save_setting("Engine.Engine", "Language", &text_arg(args, 0));
                Value::Bool(true)
            }
            Native::InterpCurveEval => Value::Float(eval_curve(&curve_points(args.first()), args.get(1).map_or(0.0, float))),
            Native::LoadFileToString => {
                let loaded = std::fs::read(self.read_path(&text_arg(args, 1))).ok().map(|bytes| decode(&bytes));
                let found = loaded.is_some();
                if let Some(loaded) = loaded {
                    set_out(args, 0, Value::Str(loaded));
                }
                Value::Bool(found)
            }
            Native::SaveStringToFile => Value::Bool(std::fs::write(self.write_path(&text_arg(args, 1)), text_arg(args, 0)).is_ok()),
            Native::FindFiles => {
                let pattern = text_arg(args, 0).replace('\\', "/");
                let (folder, file_pattern) = pattern.rsplit_once('/').unwrap_or((".", &pattern));
                let (files, dirs) = (args.get(1).is_some_and(truthy), args.get(2).is_some_and(truthy));
                let folders = [data_path(self.options.data_dir.as_deref(), folder), Some(self.resolve_path(folder))];
                let mut names: Vec<String> = folders
                    .into_iter()
                    .flatten()
                    .flat_map(|folder| std::fs::read_dir(folder).into_iter().flatten().flatten())
                    .filter(|entry| entry.file_type().is_ok_and(|t| if t.is_dir() { dirs } else { files }))
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .filter(|name| wildcard(file_pattern, name))
                    .collect();
                names.sort_by_key(|name| name.to_ascii_lowercase());
                names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
                Value::Array(names.into_iter().map(Value::Str).collect())
            }
            Native::OpenLog => {
                if let Some(this) = this {
                    let path = self.write_path(&format!("{}.txt", text_arg(args, 0)));
                    let opened = std::fs::OpenOptions::new().create(true).append(true).open(&path).is_ok();
                    if opened {
                        self.host.logs.insert(this, path);
                    } else {
                        self.host.logs.remove(&this);
                    }
                }
                return Ok(None);
            }
            Native::Logf => {
                let path = this.and_then(|this| self.host.logs.get(&this).cloned());
                if let Some(mut file) = path.and_then(|path| std::fs::OpenOptions::new().append(true).open(path).ok()) {
                    use std::io::Write;
                    let _ = writeln!(file, "{}", text_arg(args, 0));
                }
                return Ok(None);
            }
            Native::CloseLog => {
                if let Some(this) = this {
                    self.host.logs.remove(&this);
                }
                return Ok(None);
            }
            Native::Error => {
                self.diagnostics.log.push(format!("Error: {}", text_arg(args, 0)));
                return Ok(None);
            }
            Native::GetURLMap | Native::GetMapFileName => Value::Str(self.map_name.clone()),
            Native::GetMapName => Value::Str(self.get_map_name(&text_arg(args, 0), &text_arg(args, 1), args.get(2).map_or(0, int))),
            Native::DoesMapExist => {
                let wanted = text_arg(args, 0);
                let at = wanted.len().wrapping_sub(".fuk".len());
                let wanted = if wanted.get(at..).is_some_and(|end| end.eq_ignore_ascii_case(".fuk")) { &wanted[..at] } else { wanted.as_str() };
                Value::Bool(self.map_stems().iter().any(|stem| stem.eq_ignore_ascii_case(wanted)))
            }
            // A stub on purpose: retail only opens one of three web pages; "" claims success and opens nothing.
            Native::GetAddressURL | Native::GetNetworkNumber | Native::LaunchSocialNetwork => Value::Str(String::new()),
            Native::GetPlayerNetworkAddress => Value::Str("127.0.0.1".to_owned()),
            // The default URL's options come from `[URL]` (unverified).
            Native::GetDefaultURL => Value::Str(self.config_value("URL", &text_arg(args, 0)).unwrap_or_default()),
            // The VM keeps no URL and nothing reads its options yet; with `bSaveDefault` the option is saved
            // to `[DefaultPlayer]` in the settings.
            Native::UpdateURL => {
                if args.get(2).is_some_and(truthy) {
                    self.save_setting("DefaultPlayer", &text_arg(args, 0), &text_arg(args, 1));
                }
                return Ok(None);
            }
            Native::GetNextInt => Value::Str(self.registry_entry(&text_arg(args, 0), args.get(1).map_or(0, int)).map(|e| e.name).unwrap_or_default()),
            Native::GetNextIntDesc => {
                let entry = self.registry_entry(&text_arg(args, 0), args.get(1).map_or(0, int)).unwrap_or_default();
                set_out(args, 2, Value::Str(entry.name));
                set_out(args, 3, Value::Str(entry.description));
                return Ok(None);
            }
            Native::GetNextSkin => {
                let entry = self.next_skin_entry(&text_arg(args, 0), &text_arg(args, 1), args.get(2).map_or(0, int)).unwrap_or_default();
                set_out(args, 3, Value::Str(entry.name));
                set_out(args, 4, Value::Str(entry.description));
                return Ok(None);
            }
            Native::ParseKillMessage => Value::Str(parse_kill_message(&text_arg(args, 0), &text_arg(args, 1), &text_arg(args, 2))),
            Native::DeleteSave => Value::Bool(self.delete_save(args.first().map_or(0, int))),
            // Retail's `LaunchDLC` answers false without a local client, taking `MenuDLCLaunch`'s failure path.
            Native::IsSteamDeck | Native::IsWorkshopBuild | Native::SteamGetAchievement | Native::SteamOwnsGame | Native::LaunchDLC => Value::Bool(false),
            Native::SteamGetStatFloat => Value::Float(0.0),
            Native::SteamGetStatInt => Value::Int(0),
            Native::SteamUnlockAchievement | Native::SteamLockAchievement | Native::SteamRequestStats | Native::SteamSetStatInt | Native::SteamSetStatFloat
            | Native::SteamResetStats | Native::SteamIndicateAchievementProgress | Native::SteamPurgeWorkshop | Native::SteamViewStorePage | Native::ResetKeyboard => {
                return Ok(None);
            }
            Native::CopyToClipboard => {
                if let Ok(mut clipboard) = CLIPBOARD.lock() {
                    *clipboard = text_arg(args, 0);
                }
                return Ok(None);
            }
            Native::PasteFromClipboard => Value::Str(CLIPBOARD.lock().map(|c| c.clone()).unwrap_or_default()),
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    fn system_dir(&self) -> Option<PathBuf> {
        self.loader.find("engine").and_then(Path::parent).map(Path::to_path_buf)
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let path = PathBuf::from(path.replace('\\', "/"));
        match (path.is_absolute(), self.system_dir()) {
            (false, Some(dir)) => dir.join(path),
            _ => path,
        }
    }

    fn write_path(&self, path: &str) -> PathBuf {
        data_write_path(self.options.data_dir.as_deref(), path).unwrap_or_else(|| self.resolve_path(path))
    }

    fn read_path(&self, path: &str) -> PathBuf {
        data_read_path(self.options.data_dir.as_deref(), path).unwrap_or_else(|| self.resolve_path(path))
    }

    fn language(&self) -> String {
        self.config_value("Engine.Engine", "Language").filter(|l| !l.is_empty()).unwrap_or_else(|| "int".to_owned())
    }

    /// A missing entry gives the engine's error text `<?language?Package.Section.Key?>` (unverified).
    fn localize(&self, section: &str, key: &str, package: &str) -> String {
        let language = self.language();
        if let Some(dir) = self.system_dir() {
            for extension in [language.as_str(), "int"] {
                if let Some(value) = load_ini(&dir.join(format!("{package}.{extension}"))).and_then(|ini| ini.get(section, key).map(|v| unquote(v).to_owned())) {
                    return value;
                }
            }
        }
        format!("<?{language}?{package}.{section}.{key}?>")
    }

    fn next_skin_entry(&self, prefix: &str, current: &str, dir: i32) -> Option<RegistryEntry> {
        let entries = registry(&self.system_dir()?, &self.language());
        let textures: Vec<&RegistryEntry> = entries.iter().filter(|e| e.class.eq_ignore_ascii_case("Texture")).collect();
        let index = next_skin(textures.iter().map(|e| e.name.as_str()), prefix, current, dir)?;
        textures.get(index).map(|e| (*e).clone())
    }

    fn registry_entry(&self, class: &str, index: i32) -> Option<RegistryEntry> {
        let entries = registry(&self.system_dir()?, &self.language());
        entries.iter().filter(|e| e.meta_class.eq_ignore_ascii_case(class)).nth(usize::try_from(index).ok()?).cloned()
    }

    fn map_stems(&self) -> Vec<String> {
        let (mut stems, mut seen) = (Vec::new(), FxHashSet::default());
        for file in self.map_files() {
            let stem = file.rsplit_once('.').map_or(file.as_str(), |(stem, _)| stem).to_owned();
            if seen.insert(stem.to_ascii_lowercase()) {
                stems.push(stem);
            }
        }
        stems
    }

    /// `GetMapName(Prefix, MapName, Dir)`; the meaning of the arguments is (unverified).
    fn get_map_name(&self, prefix: &str, map_name: &str, dir: i32) -> String {
        let maps: Vec<String> = self.map_stems().into_iter().filter(|m| m.get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix))).collect();
        let current = map_name.rsplit_once('.').map_or(map_name, |(stem, _)| stem);
        let count = maps.len() as i64;
        let next = match maps.iter().position(|m| m.eq_ignore_ascii_case(current)) {
            Some(index) => (index as i64 + i64::from(dir)).rem_euclid(count.max(1)),
            None if dir < 0 => count - 1,
            None => 0,
        };
        usize::try_from(next).ok().and_then(|i| maps.get(i)).cloned().unwrap_or_default()
    }
}

impl<L: LoaderRef> Vm<L> {
    fn subtitle_manager(&self) -> Option<ObjectId> {
        self.store.object_of(self.player()?, Prop::SubtitleManager)
    }

    fn subtitle_time(&self) -> f32 {
        self.level.map_or(0.0, |level| self.store.float_of(level, Prop::TimeSeconds))
    }

    fn subtitle_array(&self, manager: ObjectId, array: Prop) -> Vec<Value> {
        match self.store.get(manager, array) {
            Some(Value::Array(items)) => items,
            _ => Vec::new(),
        }
    }

    fn subtitle_sound(&mut self, manager: ObjectId, sound: &str) -> Option<(usize, ObjectId)> {
        if self.media.subtitle_sounds.manager != Some(manager) {
            let mut by_path = FxHashMap::default();
            for (index, entry) in self.subtitle_array(manager, Prop::SubSound).iter().enumerate() {
                if let &Value::Object(Some(found)) = entry {
                    by_path.entry(self.store.object(found).path.to_ascii_lowercase()).or_insert((index, found));
                }
            }
            self.media.subtitle_sounds = SubtitleSounds { manager: Some(manager), by_path };
        }
        self.media.subtitle_sounds.by_path.get(&sound.to_ascii_lowercase()).copied()
    }

    /// The manager's array of that name hides `Actor.ActorID` (retail reads by offset), so it is taken by place in the
    /// layout.
    fn subtitle_actor_index(&self, manager: ObjectId, actor: ObjectId) -> i32 {
        let Some(Value::Name(id)) = self.store.get(actor, "ActorID") else { return -1 };
        let Some(class) = self.store.object(manager).class else { return -1 };
        let Some(index) = self.store.class(class).layout.properties.iter().rposition(|p| p.name.eq_ignore_ascii_case("ActorID")) else { return -1 };
        if id == NameId::NONE {
            return -1;
        }
        let field = self.store.memory.get(self.store.class(class).mem).fields[index];
        let Value::Array(ids) = self.store.memory.read_field(&field, &self.store.object(manager).mem) else { return -1 };
        let name = self.store.name(id);
        ids.iter().rposition(|entry| matches!(entry, Value::Name(entry) if self.store.name(*entry).eq_ignore_ascii_case(name))).map_or(-1, |at| at as i32)
    }

    /// Retail reaches this from its audio layer; the VM's sound sites stand in for that,
    /// so which manager and any check of its own are (unverified).
    pub(crate) fn subtitle_insert_sound(&mut self, actor: Option<ObjectId>, sound: &str) {
        let Some(manager) = self.subtitle_manager() else { return };
        let now = self.subtitle_time();
        let subtitled = self.subtitle_sound(manager, sound);
        let mut invalid = self.subtitle_array(manager, Prop::InvalidSounds);
        let before = invalid.len();
        let mut added = false;
        if valid_sound(&mut invalid, subtitled.map(|(_, found)| found), now)
            && let Some((index, found)) = subtitled
        {
            added = true;
            invalid.push(Value::Struct(vec![Value::Object(Some(found)), Value::Float(now + self.store.float_of(manager, Prop::InvalidTime))]));
            let actor_index = actor.map_or(-1, |actor| self.subtitle_actor_index(manager, actor));
            let mut pending = self.subtitle_array(manager, Prop::PendingSounds);
            let entry = Value::Struct(vec![Value::Object(Some(found)), Value::Object(actor), Value::Float(now + SUBTITLE_DELAY), Value::Int(index as i32), Value::Int(actor_index)]);
            insert_pending(&mut pending, actor, entry);
            self.store.set(manager, Prop::PendingSounds, Value::Array(pending));
        }
        if added || invalid.len() != before {
            self.store.set(manager, Prop::InvalidSounds, Value::Array(invalid));
        }
    }

    pub(crate) fn subtitle_tick(&mut self) -> Result<(), Error> {
        let (Some(manager), Some(player)) = (self.subtitle_manager(), self.player()) else { return Ok(()) };
        let mut pending = self.subtitle_array(manager, Prop::PendingSounds);
        let due = take_expired(&mut pending, 2, self.subtitle_time());
        if due.is_empty() {
            return Ok(());
        }
        self.store.set(manager, Prop::PendingSounds, Value::Array(pending));
        for entry in due {
            let member = |member| subtitle_member(&entry, member).cloned().unwrap_or(Value::Object(None));
            self.event(player, "CallSubtitle", [member(0), member(3), member(4), member(1)])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_skin_walks_the_names_with_the_prefix() {
        let next = |current: &str, dir: i32| next_skin(["Skin.A1", "Skin.B1", "Other.X", "Skin.C1"].into_iter(), "skin.", current, dir);
        assert_eq!(next("Skin.B1", 1), Some(3));
        assert_eq!(next("skin.b1", -1), Some(0));
        assert_eq!(next("Skin.B1", 0), Some(1));
        assert_eq!(next("Skin.C1", 1), Some(0), "past the end wraps to the first");
        assert_eq!(next("Skin.A1", -1), None, "nothing before the first");
        assert_eq!(next("Unknown", 1), Some(3), "an unknown current gives the last");
        assert_eq!(next_skin(["Other.X"].into_iter(), "skin.", "", 1), None);
    }

    fn pending_sound(sound: u32, actor: u32, time: f32) -> Value {
        Value::Struct(vec![Value::Object(Some(ObjectId(sound))), Value::Object(Some(ObjectId(actor))), Value::Float(time), Value::Int(0), Value::Int(-1)])
    }

    #[test]
    fn take_expired_steps_over_the_entry_after_a_removal() {
        let mut entries = vec![pending_sound(1, 1, 1.0), pending_sound(2, 2, 1.0), pending_sound(3, 3, 9.0)];
        assert_eq!(take_expired(&mut entries, 2, 5.0), [pending_sound(1, 1, 1.0)]);
        assert_eq!(entries, [pending_sound(2, 2, 1.0), pending_sound(3, 3, 9.0)]);
        assert_eq!(take_expired(&mut entries, 2, 5.0), [pending_sound(2, 2, 1.0)]);
    }

    #[test]
    fn a_time_equal_to_now_has_not_expired() {
        let mut entries = vec![pending_sound(1, 1, 5.0)];
        assert!(take_expired(&mut entries, 2, 5.0).is_empty());
    }

    #[test]
    fn valid_sound_refuses_a_remembered_sound_until_it_expires() {
        let filter = |sound, time| Value::Struct(vec![Value::Object(Some(ObjectId(sound))), Value::Float(time)]);
        let mut invalid = vec![filter(1, 10.0)];
        assert!(!valid_sound(&mut invalid, Some(ObjectId(1)), 5.0));
        assert!(valid_sound(&mut invalid, Some(ObjectId(2)), 5.0));
        assert!(valid_sound(&mut invalid, None, 5.0));
        assert!(valid_sound(&mut invalid, Some(ObjectId(1)), 11.0));
        assert!(invalid.is_empty());
    }

    #[test]
    fn an_actor_has_one_pending_subtitle() {
        let mut pending = vec![pending_sound(1, 7, 1.0)];
        insert_pending(&mut pending, Some(ObjectId(8)), pending_sound(2, 8, 2.0));
        insert_pending(&mut pending, Some(ObjectId(7)), pending_sound(3, 7, 3.0));
        assert_eq!(pending, [pending_sound(3, 7, 3.0), pending_sound(2, 8, 2.0)]);
    }

    #[test]
    fn subtitle_index_finds_a_tag_in_any_case() {
        let tags = Value::Array(vec![Value::Str("Intro".to_owned()), Value::Str("Outro".to_owned())]);
        assert_eq!(subtitle_index(Some(&tags), "outro"), Some(1));
        assert_eq!(subtitle_index(Some(&tags), "Missing"), None);
        assert_eq!(subtitle_index(Some(&tags), ""), None);
        assert_eq!(subtitle_index(None, "Intro"), None);
    }

    #[test]
    fn curve_clamps_and_interpolates() {
        let points = [(0.0, 0.0), (1.0, 10.0), (3.0, 30.0)];
        assert_eq!(eval_curve(&points, -1.0), 0.0);
        assert_eq!(eval_curve(&points, 2.0), 20.0);
        assert_eq!(eval_curve(&points, 9.0), 30.0);
        assert_eq!(eval_curve(&[], 1.0), 0.0);
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("*.fuk", "Foo.FUK"));
        assert!(wildcard("a?c*", "abcd"));
        assert!(!wildcard("*.fuk", "foo.u"));
    }

    #[test]
    fn registry_line() {
        let entry = registry_entry("(Name=Pkg.Cls,Class=Class,MetaClass=Engine.Pawn,Description=\"A, b\")");
        assert_eq!((entry.name.as_str(), entry.meta_class.as_str(), entry.description.as_str()), ("Pkg.Cls", "Engine.Pawn", "A, b"));
    }

    #[test]
    fn kill_message_and_utf16() {
        assert_eq!(parse_kill_message("A", "B", "%k beat %o"), "A beat B");
        assert_eq!(decode(&[0xFF, 0xFE, b'h', 0, b'i', 0]), "hi");
    }

    #[test]
    fn writes_go_to_the_data_folder_and_read_back() {
        let data = std::env::temp_dir().join("p2-util-natives-data-folder");
        let written = data_write_path(Some(&data), "Sub\\Note.txt").expect("a relative path has a data path");
        assert_eq!(written, data.join("System").join("Sub/Note.txt"));
        assert!(written.parent().is_some_and(Path::is_dir));
        std::fs::write(&written, "hi").expect("writing the test file");
        assert_eq!(data_read_path(Some(&data), "Sub/Note.txt"), Some(written));
        assert_eq!(data_read_path(Some(&data), "Sub/Other.txt"), None);
        let absolute = std::env::temp_dir().join("x.txt").display().to_string();
        assert_eq!(data_write_path(Some(&data), &absolute), None);
        assert_eq!(data_write_path(None, "Note.txt"), None);
        assert_eq!(data_read_path(None, "Note.txt"), None);
    }
}
