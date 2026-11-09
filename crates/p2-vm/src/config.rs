use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use p2_package::Ini;
use p2_script::{StructLayout, Type};

use crate::{ClassId, HostCommand, LoaderRef, NameId, ObjectId, Value, Vm};

const CLASS_CONFIG: u32 = 0x4;
const CPF_CONFIG: u32 = 0x4000;
/// Loaded and saved in the section of the declaring class, not the object's class (unverified).
const CPF_GLOBAL_CONFIG: u32 = 0x0004_0000;

const SETTINGS: &str = "settings";
pub const VIEWPORT_MANAGER: &str = "ini:Engine.Engine.ViewportManager";
pub const RENDER_DEVICE: &str = "ini:Engine.Engine.RenderDevice";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowRequest {
    pub size: Option<[u32; 2]>,
    pub fullscreen: bool,
    pub borderless: bool,
}

pub fn ini_truthy(text: &str) -> bool {
    text.eq_ignore_ascii_case("true") || text == "1"
}

#[derive(Default)]
pub(crate) struct Configs {
    files: BTreeMap<String, Ini>,
    changed: bool,
    audio: AudioSettings,
    enums: HashMap<String, Arc<[String]>>,
}

struct AudioSettings {
    music_volume: f32,
    sound_volume: f32,
    ambient_volume: f32,
    channels: usize,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self { music_volume: 1.0, sound_volume: 1.0, ambient_volume: 1.0, channels: 32 }
    }
}

impl<L: LoaderRef> Vm<L> {
    pub fn add_config(&mut self, name: &str, ini: Ini) {
        if name.eq_ignore_ascii_case("User") {
            self.host.input.bind(&ini);
        }
        self.host.configs.files.insert(name.to_ascii_lowercase(), ini);
        self.refresh_audio_settings();
    }

    fn refresh_audio_settings(&mut self) {
        let value = |key: &str| self.config_value("ALAudio.ALAudioSubsystem", key);
        let volume = |key: &str| value(key).and_then(|v| v.trim().parse().ok()).unwrap_or(1.0);
        let audio = AudioSettings {
            music_volume: volume("MusicVolume"),
            sound_volume: volume("SoundVolume"),
            ambient_volume: volume("AmbientVolume"),
            channels: value("Channels").and_then(|v| v.trim().parse().ok()).unwrap_or(32),
        };
        self.host.configs.audio = audio;
    }

    pub fn take_changed_settings(&mut self) -> Option<Ini> {
        std::mem::take(&mut self.host.configs.changed).then(|| self.host.configs.files.get(SETTINGS).cloned().unwrap_or_default())
    }

    pub fn config_value(&self, section: &str, key: &str) -> Option<String> {
        let files = &self.host.configs.files;
        let first = [SETTINGS, "system"].into_iter().filter_map(|name| files.get(name));
        first.chain(files.values()).find_map(|ini| ini.get(section, key)).map(str::to_owned)
    }

    /// Kept through every travel. The settings go over `User.ini`, because `UpdateURL` saves there.
    pub(crate) fn add_default_player_options(&self, url: &mut crate::TravelUrl) {
        let files = &self.host.configs.files;
        for ini in [SETTINGS, "user"].into_iter().filter_map(|name| files.get(name)) {
            for (key, value) in ini.section("DefaultPlayer") {
                let set = url.options.iter().any(|option| option.split('=').next().is_some_and(|k| k.trim().eq_ignore_ascii_case(key)));
                if !set {
                    url.options.push(format!("{key}={value}"));
                }
            }
        }
    }

    /// Reads the `MusicVolume` setting; 1 if missing (unverified).
    pub fn music_volume(&self) -> f32 {
        self.host.configs.audio.music_volume
    }

    /// Scales every sound; 1 if missing (unverified).
    pub fn sound_volume(&self) -> f32 {
        self.host.configs.audio.sound_volume
    }

    /// Ambient sounds play at `SoundVolume` / 255 times this; 1 if missing (unverified).
    pub fn ambient_volume(&self) -> f32 {
        self.host.configs.audio.ambient_volume
    }

    /// Sounds playing at once; 32 if missing (unverified).
    pub fn audio_channels(&self) -> usize {
        self.host.configs.audio.channels
    }

    pub(crate) fn ini_name(&self, reference: &str) -> Option<String> {
        let (section, key) = reference.rsplit_once('.')?;
        self.config_value(section, key)
    }

    fn command_class_path(&self, path: &str) -> String {
        match path.get(..4).filter(|prefix| prefix.eq_ignore_ascii_case("ini:")) {
            Some(_) => self.ini_name(&path[4..]).unwrap_or_default(),
            None => path.to_owned(),
        }
    }

    pub fn get_command(&mut self, query: &str) -> Result<String, crate::Error> {
        let Some((path, property)) = query.trim().split_once(' ') else { return Ok(String::new()) };
        let (path, property) = (self.command_class_path(path), property.trim());
        let Some((package, class)) = path.split_once('.') else { return Ok(String::new()) };
        match self.find_class(package, class) {
            Ok(class) => Ok(self.store.default_value(class, property).map_or_else(String::new, |value| self.value_text(&value))),
            Err(_) => Ok(self.config_value(&path, property).unwrap_or_default()),
        }
    }

    fn value_text(&self, value: &Value) -> String {
        match value {
            Value::Bool(value) => if *value { "True" } else { "False" }.to_owned(),
            Value::Int(value) => value.to_string(),
            Value::Float(value) => format!("{value:.6}"),
            Value::Byte(value) => value.to_string(),
            Value::Str(value) => value.clone(),
            &Value::Name(name) => self.store.name(name).to_owned(),
            &Value::Object(object) => object.map_or_else(|| "None".to_owned(), |o| self.store.object(o).path.clone()),
            _ => String::new(),
        }
    }

    fn export_text(&self, ty: &Type, value: &Value) -> String {
        let members = match value {
            Value::Vector(v) => return format!("(X={:.6},Y={:.6},Z={:.6})", v.x, v.y, v.z),
            Value::Rotator(r) => return format!("(Pitch={},Yaw={},Roll={})", r.pitch, r.yaw, r.roll),
            Value::Struct(members) => members,
            _ => return self.value_text(value),
        };
        let Type::Struct(layout) = ty else { return String::new() };
        let members: Vec<String> = layout
            .properties
            .iter()
            .zip(members)
            .map(|(field, member)| match (&field.ty, member) {
                (Type::Str | Type::String, Value::Str(text)) => format!("{}=\"{text}\"", field.name),
                (ty, member) => format!("{}={}", field.name, self.export_text(ty, member)),
            })
            .collect();
        format!("({})", members.join(","))
    }

    pub(crate) fn property_text(&self, object: ObjectId, name: &str) -> String {
        self.store.get(object, name).map_or_else(String::new, |value| self.value_text(&value))
    }

    pub(crate) fn set_property_text(&mut self, object: ObjectId, name: &str, text: &str) {
        let Some(class) = self.store.object(object).class else { return };
        let Some(index) = self.store.property_index(class, name) else { return };
        let ty = self.store.class(class).layout.properties[index].ty.clone();
        if let Some(value) = self.import(&ty, text) {
            self.store.set(object, name, value);
        }
    }

    pub(crate) fn enum_name(&mut self, enumeration: ObjectId, index: i32) -> NameId {
        let path = self.store.object(enumeration).path.clone();
        let name = self.enum_names(&path).and_then(|names| names.get(usize::try_from(index).ok()?).cloned());
        name.map_or(NameId::NONE, |name| self.intern(&name))
    }

    pub(crate) fn enum_names(&mut self, path: &str) -> Option<Arc<[String]>> {
        if let Some(names) = self.host.configs.enums.get(path) {
            return Some(names.clone());
        }
        let loader = self.loader.clone();
        let (package, export) = loader.loaded_export_by_path(path)?;
        let loaded = loader.package(package);
        let p2_script::ScriptObject::Enum(e) = p2_script::read_export(&loaded.package, &loaded.data, export).ok()?? else { return None };
        let names: Arc<[String]> = e.names.iter().map(|&n| loaded.package.name(n).to_owned()).collect();
        self.host.configs.enums.insert(path.to_owned(), names.clone());
        Some(names)
    }

    /// The table holds the first object made at a path, without objects of a level that was left.
    /// Only when that one is of another class are the objects scanned for a later one.
    pub(crate) fn find_object_by_path(&mut self, path: &str, class: &str) -> Option<ObjectId> {
        let same_path = |vm: &Self, o: ObjectId| vm.store.object(o).path.eq_ignore_ascii_case(path);
        let of_class = |vm: &Self, o: ObjectId| class.is_empty() || vm.store.name(vm.store.object(o).class_name).eq_ignore_ascii_case(class) || vm.store.is_a(o, class) || vm.stub_is_a(o, class);
        if let Some(&first) = self.store.object_ids.get(&path.to_ascii_lowercase()) {
            if of_class(self, first) {
                return Some(first);
            }
            if let Some(found) = self.object_ids().find(|&o| same_path(self, o) && of_class(self, o)) {
                return Some(found);
            }
        }
        match self.export_class(path, class) {
            Some(Some(class)) => Some(self.stub(path, &class)),
            _ => None,
        }
    }

    /// A `globalconfig` property's section is its declaring class's, the topmost holding it (unverified).
    pub(crate) fn config_section(&self, from: Option<ClassId>, name: &str, flags: u32, section: &str) -> String {
        if flags & CPF_GLOBAL_CONFIG == 0 {
            return section.to_owned();
        }
        let mut owner = None;
        let mut class = from;
        while let Some(id) = class {
            if self.store.class(id).layout.properties.iter().any(|f| f.name.eq_ignore_ascii_case(name)) {
                owner = Some(id);
            }
            class = self.store.class(id).super_class;
        }
        owner.map_or_else(
            || section.to_owned(),
            |id| format!("{}.{}", self.loader.clone().package(self.store.class(id).package).name, self.store.name(self.store.class(id).name)),
        )
    }

    #[expect(clippy::too_many_arguments, reason = "one call site in class loading; the values are its locals")]
    pub(crate) fn load_config(
        &mut self,
        flags: u32,
        file: &str,
        section: &str,
        super_class: Option<ClassId>,
        layout: &StructLayout,
        mem_layout: crate::memory::LayoutId,
        defaults: &mut crate::memory::Mem,
    ) {
        if flags & CLASS_CONFIG == 0 {
            return;
        }
        let empty = Ini::default();
        let file = self.host.configs.files.get(&file.to_ascii_lowercase()).unwrap_or(&empty);
        let settings = self.host.configs.files.get(SETTINGS).unwrap_or(&empty);
        let values = |section: &str, key: &str| {
            let ini = if settings.get(section, key).is_some() { settings } else { file };
            ini.get_all(section, key).map(str::to_owned).collect::<Vec<_>>()
        };
        let mut found: Vec<(usize, Vec<String>)> = Vec::new();
        for (index, field) in layout.properties.iter().enumerate().filter(|(_, f)| f.flags & CPF_CONFIG != 0) {
            let section = self.config_section(super_class, &field.name, field.flags, section);
            let section = section.as_str();
            if file.section(section).is_empty() && settings.section(section).is_empty() {
                continue;
            }
            let values = |key: &str| values(section, key);
            let texts: Vec<String> = match (&field.ty, field.array_dim) {
                (Type::Array(_), _) => values(&field.name),
                (_, dim) if dim > 1 => (0..dim)
                    .map(|i| values(&format!("{}[{i}]", field.name)).into_iter().next().unwrap_or_default())
                    .collect(),
                _ => values(&field.name).into_iter().take(1).collect(),
            };
            if !texts.is_empty() {
                found.push((index, texts));
            }
        }
        for (index, texts) in found {
            let field = &layout.properties[index];
            let slot = self.store.memory.get(mem_layout).fields[index];
            let value = match (&field.ty, field.array_dim) {
                // A value that does not parse stays a zero item: `Subtitles.ini`'s arrays run in parallel, and many of
                // its `Priority=` are empty.
                (Type::Array(inner), _) => Some(Value::Array(texts.iter().map(|t| self.import(inner, t).unwrap_or_else(|| crate::zero_slot(inner, 1))).collect())),
                (ty, dim) if dim > 1 => {
                    let Value::Array(mut items) = self.store.memory.read_field(&slot, defaults) else { continue };
                    for (item, text) in items.iter_mut().zip(&texts).filter(|(_, t)| !t.is_empty()) {
                        if let Some(value) = self.import(ty, text) {
                            *item = value;
                        }
                    }
                    Some(Value::Array(items))
                }
                (ty, _) => self.import(ty, &texts[0]),
            };
            if let Some(value) = value {
                self.store.memory.write_field(&slot, defaults, value);
            }
        }
    }

    pub(crate) fn set_command(&mut self, query: &str) {
        let mut parts = query.trim().splitn(3, ' ');
        let (Some(path), Some(property), text) = (parts.next(), parts.next(), parts.next().unwrap_or("").trim()) else { return };
        let path = self.command_class_path(path);
        let Some((package, class)) = path.split_once('.') else { return };
        let Ok(class) = self.find_class(package, class) else {
            self.save_setting(&path, property, text);
            return;
        };
        let Some(index) = self.store.property_index(class, property) else { return };
        let field = &self.store.class(class).layout.properties[index];
        let (ty, config) = (field.ty.clone(), field.flags & CPF_CONFIG != 0);
        let Some(value) = self.import(&ty, text) else { return };
        let field = self.store.memory.get(self.store.class(class).mem).fields[index];
        self.store.memory.write_field(&field, &mut self.store.classes[class.0 as usize].defaults, value.clone());
        let class_name = self.store.name(self.store.class(class).name).to_owned();
        let objects: Vec<_> = self.object_ids().filter(|&o| self.store.is_a(o, &class_name)).collect();
        for object in objects {
            self.store.set(object, property, value.clone());
        }
        if property.eq_ignore_ascii_case("SubSound") {
            self.media.subtitle_sounds = crate::util_natives::SubtitleSounds::default();
        }
        if config {
            let text = self.value_text(&value);
            self.save_setting(&path, property, &text);
        }
    }

    pub fn viewport_setting(&mut self, key: &str) -> String {
        self.get_command(&format!("{VIEWPORT_MANAGER} {key}")).unwrap_or_default()
    }

    pub(crate) fn is_fullscreen(&mut self) -> bool {
        match self.host.fullscreen {
            Some(fullscreen) => fullscreen,
            None => ini_truthy(&self.viewport_setting("StartupFullscreen")),
        }
    }

    pub(crate) fn request_window(&mut self, size: Option<[u32; 2]>, fullscreen: bool) {
        self.host.fullscreen = Some(fullscreen);
        let borderless = ini_truthy(&self.viewport_setting("FullscreenWindowed"));
        self.host.commands.push(HostCommand::Window(WindowRequest { size, fullscreen, borderless }));
    }

    /// `SetRes WxH[xDepth][w|f]`: `w` windowed, `f` fullscreen, else as now (unverified).
    /// The size is saved as the mode's `WindowedViewportX`/`Y` or `FullscreenViewportX`/`Y` (unverified).
    pub(crate) fn set_res(&mut self, text: &str) {
        let text = text.trim().to_ascii_lowercase();
        let fullscreen = match text.chars().last() {
            Some('w') => false,
            Some('f') => true,
            _ => self.is_fullscreen(),
        };
        let mut numbers = text.trim_end_matches(['w', 'f']).split('x').map(|n| n.trim().parse::<u32>().ok());
        let size = match (numbers.next().flatten(), numbers.next().flatten()) {
            (Some(width), Some(height)) if width > 0 && height > 0 => Some([width, height]),
            _ => None,
        };
        if let Some(size) = size {
            let mode = if fullscreen { "Fullscreen" } else { "Windowed" };
            for (axis, value) in ["X", "Y"].into_iter().zip(size) {
                self.set_command(&format!("{VIEWPORT_MANAGER} {mode}Viewport{axis} {value}"));
            }
        }
        self.request_window(size, fullscreen);
    }

    pub(crate) fn save_setting(&mut self, section: &str, key: &str, value: &str) {
        self.host.configs.files.entry(SETTINGS.to_owned()).or_default().set(section, key, value);
        self.host.configs.changed = true;
        self.refresh_audio_settings();
    }

    /// A `globalconfig` property goes to its declaring class's section, so every subclass reads one value.
    pub(crate) fn save_config(&mut self, object: ObjectId, defaults: bool) {
        let Some(class) = self.store.object(object).class else { return };
        let package = self.loader.clone().package(self.store.class(class).package).name.clone();
        let own = format!("{package}.{}", self.store.name(self.store.class(class).name));
        let fields: Vec<_> = self
            .store.class(class)
            .layout
            .properties
            .iter()
            .filter(|f| f.flags & CPF_CONFIG != 0)
            .map(|f| (f.name.to_string(), f.array_dim, f.ty.clone(), self.config_section(Some(class), &f.name, f.flags, &own)))
            .collect();
        let configured: Vec<ClassId> = (0..self.store.classes.len())
            .map(|index| ClassId(index as u32))
            .filter(|&class| self.store.class(class).layout.properties.iter().any(|f| f.flags & CPF_CONFIG != 0))
            .collect();
        for (name, dim, ty, section) in fields {
            let value = if defaults { self.store.default_value(class, &name) } else { self.store.get(object, name.as_str()) };
            // Retail `SaveConfig` then reloads the config of every loaded class under the object's config base
            // (unverified): the loaded classes that read this property from this section take the value.
            if let Some(value) = &value {
                self.refresh_config_defaults(&configured, &name, &section, value);
            }
            match (&ty, value) {
                (Type::Array(inner), Some(Value::Array(items))) => {
                    let texts: Vec<String> = items.iter().map(|item| self.export_text(inner, item)).collect();
                    self.host.configs.files.entry(SETTINGS.to_owned()).or_default().set_all(&section, &name, &texts);
                    self.host.configs.changed = true;
                    self.refresh_audio_settings();
                }
                (_, Some(Value::Array(items))) if dim > 1 => {
                    for (i, item) in items.iter().enumerate() {
                        let text = self.export_text(&ty, item);
                        self.save_setting(&section, &format!("{name}[{i}]"), &text);
                    }
                }
                (Type::Struct(_), Some(value)) => {
                    let text = self.export_text(&ty, &value);
                    self.save_setting(&section, &name, &text);
                }
                (_, Some(value @ (Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Byte(_) | Value::Str(_) | Value::Name(_) | Value::Object(_)))) => {
                    let text = self.value_text(&value);
                    self.save_setting(&section, &name, &text);
                }
                _ => {}
            }
        }
    }

    fn refresh_config_defaults(&mut self, classes: &[ClassId], name: &str, section: &str, value: &Value) {
        for &class in classes {
            let Some(field) = self.store.class(class).layout.properties.iter().position(|f| f.flags & CPF_CONFIG != 0 && f.name.eq_ignore_ascii_case(name))
            else {
                continue;
            };
            let flags = self.store.class(class).layout.properties[field].flags;
            let package = self.loader.clone().package(self.store.class(class).package).name.clone();
            let own = format!("{package}.{}", self.store.name(self.store.class(class).name));
            if !self.config_section(Some(class), name, flags, &own).eq_ignore_ascii_case(section) {
                continue;
            }
            let slot = self.store.memory.get(self.store.class(class).mem).fields[field];
            self.store.memory.write_field(&slot, &mut self.store.classes[class.0 as usize].defaults, value.clone());
        }
    }

    pub(crate) fn import(&mut self, ty: &Type, text: &str) -> Option<Value> {
        let text = text.trim();
        Some(match ty {
            Type::Int => Value::Int(text.parse().ok().or_else(|| text.parse::<f32>().ok().map(|f| f as i32))?),
            Type::Float => Value::Float(text.parse().ok()?),
            Type::Byte => Value::Byte(text.parse().ok()?),
            Type::Bool => Value::Bool(match text.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => true,
                "false" | "0" | "no" | "off" => false,
                _ => return None,
            }),
            Type::Str | Type::String => Value::Str(text.trim_matches('"').to_owned()),
            Type::Name => Value::Name(self.intern(text.trim_matches('"'))),
            Type::Object | Type::Class => {
                let (written, path) = text.split_once('\'').map_or(("", text), |(class, rest)| (class, rest.trim_end_matches('\'')));
                if path.eq_ignore_ascii_case("None") {
                    Value::Object(None)
                } else {
                    // Uses the class as written, so `Sound'P.S'` finds the same object a script's `Sound'P.S'` finds.
                    let class = match ty {
                        Type::Class => "Class",
                        _ if !written.is_empty() => written,
                        _ => "Object",
                    };
                    Value::Object(Some(self.stub(path, class)))
                }
            }
            Type::Struct(layout) => {
                let inner = text.strip_prefix('(')?.strip_suffix(')')?;
                let mut members: Vec<Value> = layout.properties.iter().map(|f| crate::zero_slot(&f.ty, f.array_dim)).collect();
                for part in split_top_level(inner) {
                    let Some((name, value)) = part.split_once('=') else { continue };
                    let Some(index) = layout.properties.iter().position(|f| f.name.eq_ignore_ascii_case(name.trim())) else { continue };
                    if let Some(value) = self.import(&layout.properties[index].ty, value) {
                        members[index] = value;
                    }
                }
                Value::of_struct(layout, members)
            }
            // `(a,b)`: an empty place is a zero item; a comma before `)` adds none: `(,,)` has two (unverified).
            Type::Array(inner) => {
                let mut parts = split_top_level(text.strip_prefix('(')?.strip_suffix(')')?);
                if parts.last().is_some_and(|last| last.trim().is_empty()) {
                    parts.pop();
                }
                Value::Array(parts.into_iter().map(|part| self.import(inner, part).unwrap_or_else(|| crate::zero_slot(inner, 1))).collect())
            }
            Type::FixedArray(..) | Type::Map | Type::Delegate => return None,
        })
    }
}

pub(crate) fn unquote(value: &str) -> &str {
    value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value)
}

pub(crate) fn tuple_fields(text: &str) -> impl Iterator<Item = (&str, &str)> {
    let text = text.trim();
    let inner = text.strip_prefix('(').unwrap_or(text);
    let inner = inner.strip_suffix(')').unwrap_or(inner);
    split_top_level(inner).into_iter().filter_map(|field| field.split_once('=')).map(|(key, value)| (key.trim(), unquote(value.trim())))
}

pub(crate) fn split_top_level(text: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut quoted, mut start) = (Vec::new(), 0usize, false, 0);
    for (i, c) in text.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth = depth.saturating_sub(1),
            ',' if !quoted && depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use p2_package::{Ini, Loader};

    use super::{Type, WindowRequest, split_top_level, tuple_fields};
    use crate::{HostCommand, Value, Vm};

    #[test]
    fn splits_outside_brackets_and_quotes() {
        assert_eq!(split_top_level("X=1,V=(A=2,B=3),S=\"p,q\""), ["X=1", "V=(A=2,B=3)", "S=\"p,q\""]);
        let fields: Vec<_> = tuple_fields(" (Name=A.B, Desc=\"p,q\",V=(C=1,D=2),bare)").collect();
        assert_eq!(fields, [("Name", "A.B"), ("Desc", "p,q"), ("V", "(C=1,D=2)")]);
    }

    #[test]
    fn imports_lists_in_brackets() {
        let empty = std::env::temp_dir();
        let loader = Loader::new(&empty, &[]).unwrap();
        let mut vm = Vm::new(&loader);
        let text = Type::Array(Box::new(Type::Str));
        let strings = |items: &[&str]| Value::Array(items.iter().map(|item| Value::Str((*item).to_owned())).collect());
        assert_eq!(vm.import(&text, r#"("a,b","(c)")"#), Some(strings(&["a,b", "(c)"])));
        assert_eq!(vm.import(&text, "()"), Some(strings(&[])));
        let numbers = Type::Array(Box::new(Type::Int));
        assert_eq!(vm.import(&numbers, "(3,,)"), Some(Value::Array(vec![Value::Int(3), Value::Int(0)])));
        assert_eq!(vm.import(&numbers, "3"), None);
        let nested = Type::Array(Box::new(numbers));
        assert_eq!(vm.import(&nested, "((1),())"), Some(Value::Array(vec![Value::Array(vec![Value::Int(1)]), Value::Array(Vec::new())])));
    }

    #[test]
    fn imports_an_object_with_the_class_written() {
        let empty = std::env::temp_dir();
        let loader = Loader::new(&empty, &[]).unwrap();
        let mut vm = Vm::new(&loader);
        let Some(Value::Object(Some(sound))) = vm.import(&Type::Object, "Sound'Pack.Line'") else { panic!("no object") };
        assert_eq!(vm.store.object(sound).path, "Pack.Line");
        assert_eq!(vm.store.name(vm.store.object(sound).class_name), "Sound");
    }

    #[test]
    fn default_player_options_fill_the_url() {
        let empty = std::env::temp_dir();
        let loader = Loader::new(&empty, &[]).unwrap();
        let mut vm = Vm::new(&loader);
        vm.add_config("User", Ini::parse("[DefaultPlayer]\nName=Player\nClass=GameTypes.AWPostalDude\nteam=255\n"));
        let mut url = crate::TravelUrl::parse("?load=20?NAME=Bob#Telepad");
        vm.add_default_player_options(&mut url);
        assert_eq!(url.options, ["load=20", "NAME=Bob", "Class=GameTypes.AWPostalDude", "team=255"]);
        assert_eq!(url.portal, "Telepad");
    }

    #[test]
    fn save_config_without_script_class_saves_nothing() {
        let empty = std::env::temp_dir();
        let loader = Loader::new(&empty, &[]).unwrap();
        let mut vm = Vm::new(&loader);
        let stub = vm.stub("Engine.Thing", "Object");
        for name in ["SaveConfig", "StaticSaveConfig"] {
            assert_eq!(vm.call_native(name, Some(stub), &mut []).unwrap(), None);
        }
        assert!(vm.take_changed_settings().is_none());
    }

    #[test]
    fn music_volume_is_the_setting_unless_overridden() {
        let empty = std::env::temp_dir();
        let loader = Loader::new(&empty, &[]).unwrap();
        let mut vm = Vm::new(&loader);
        vm.add_config("System", Ini::parse("[ALAudio.ALAudioSubsystem]
MusicVolume=0.1
"));
        let play = |vm: &mut Vm<&Loader>, override_volume: f32| {
            let handle = vm.call_native("PlayMusic", None, &mut [Value::Str("song.ogg".to_owned()), Value::Float(0.0), Value::Float(override_volume)]);
            let volumes: Vec<f32> = vm
                .take_commands(|command| matches!(command, HostCommand::Music(_)))
                .filter_map(|command| if let HostCommand::Music(crate::MusicCommand::Play { volume, .. }) = command { Some(volume) } else { None })
                .collect();
            (handle.unwrap(), volumes)
        };
        assert_eq!(play(&mut vm, 0.0).1, [0.1]);
        assert_eq!(play(&mut vm, 0.5).1, [0.5]);
        assert_eq!(play(&mut vm, 3.0).1, [1.0]);
        vm.add_config("System", Ini::parse("[ALAudio.ALAudioSubsystem]
MusicVolume=0
"));
        assert_eq!(play(&mut vm, 0.0), (Some(Value::Int(0)), vec![]));
    }

    #[test]
    fn video_menu_commands() {
        let empty = std::env::temp_dir();
        let loader = Loader::new(&empty, &[]).unwrap();
        let mut vm = Vm::new(&loader);
        let system = "[Engine.Engine]\nViewportManager=WinDrv.WindowsClient\n\
                      [WinDrv.WindowsClient]\nBrightness=0.5\nStartupFullscreen=False\nFullscreenWindowed=True\n";
        vm.add_config("System", Ini::parse(system));
        let run = |vm: &mut Vm<&Loader>, command: &str| match vm.call_native("ConsoleCommand", None, &mut [Value::Str(command.to_owned())]) {
            Ok(Some(Value::Str(answer))) => answer,
            other => panic!("{command}: {other:?}"),
        };
        assert_eq!(run(&mut vm, "get ini:Engine.Engine.ViewportManager Brightness"), "0.5");
        run(&mut vm, "set ini:Engine.Engine.ViewportManager Brightness 0.7");
        run(&mut vm, "Gamma 1.2");
        assert_eq!(run(&mut vm, "get ini:Engine.Engine.ViewportManager Brightness"), "0.7");
        let saved = vm.take_changed_settings().expect("settings not changed");
        assert_eq!((saved.get("WinDrv.WindowsClient", "Brightness"), saved.get("WinDrv.WindowsClient", "Gamma")), (Some("0.7"), Some("1.2")));
        assert!(vm.take_changed_settings().is_none());
        assert_eq!(run(&mut vm, "GetFullScreen"), "False");
        run(&mut vm, "SetRes 1280x720x32f");
        assert_eq!(vm.take_commands(|command| matches!(command, HostCommand::Window(_))).collect::<Vec<_>>(), [HostCommand::Window(WindowRequest { size: Some([1280, 720]), fullscreen: true, borderless: true })]);
        assert_eq!(run(&mut vm, "get ini:Engine.Engine.ViewportManager FullscreenViewportY"), "720");
        run(&mut vm, "ToggleFullScreen");
        assert_eq!(vm.take_commands(|command| matches!(command, HostCommand::Window(_))).collect::<Vec<_>>(), [HostCommand::Window(WindowRequest { size: None, fullscreen: false, borderless: true })]);
        assert_eq!(run(&mut vm, "GetFullScreen"), "False");
    }
}
