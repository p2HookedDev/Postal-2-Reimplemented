use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::Prop;
use crate::{Error, LoaderRef, ObjectId, Value, Vm};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoMode {
    pub width: u32,
    pub height: u32,
    pub refresh_rate: u32,
    pub bit_depth: u32,
}

#[derive(Debug, Default)]
pub(crate) struct Client {
    pub master: Option<ObjectId>,
    pub viewport: Option<ObjectId>,
    keys: Option<Arc<[String]>>,
    key_codes: FxHashMap<String, u8>,
    pub cinematics: bool,
    pub cinematics_ratio: Option<f32>,
    pub menu_exclusive: i32,
    pub video_modes: &'static [VideoMode],
}

const CINEMATICS_RATIO: f32 = 1.66;

pub(crate) const IST_PRESS: u8 = 1;
pub(crate) const IST_RELEASE: u8 = 3;
pub(crate) const IST_AXIS: u8 = 4;

impl<L: LoaderRef> Vm<L> {
    /// Call after the first map loaded and before [`Vm::login`], which makes the viewport the player's.
    pub fn init_client(&mut self) -> Result<(), Error> {
        let class = self.find_class("Engine", "InteractionMaster")?;
        let master = self.new_object(class, None);
        self.host.client.master = Some(master);
        let console = self.add_interaction(master, "ini:Engine.Engine.Console", None)?;
        if console.is_some() {
            self.set(master, Prop::Console, Value::Object(console));
        }
        let class = self.find_class("Engine", "Player")?;
        let viewport = self.new_object(class, None);
        self.set(viewport, Prop::InteractionMaster, Value::Object(Some(master)));
        self.host.client.viewport = Some(viewport);
        let menu = self.add_interaction(master, "ini:Engine.Engine.BaseMenu", Some(viewport))?;
        if menu.is_some() {
            self.set(master, Prop::BaseMenu, Value::Object(menu));
        }
        self.add_interaction(master, "ini:Engine.Engine.DefaultPlayerMenu", Some(viewport))?;
        Ok(())
    }

    pub fn viewport(&self) -> Option<ObjectId> {
        self.host.client.viewport
    }

    /// `UWindow` menus place their pointer from `WindowsMouseX` / `WindowsMouseY` while drawing; `x`, `y` are canvas pixels.
    pub fn set_mouse_position(&mut self, x: f32, y: f32) {
        let Some(viewport) = self.host.client.viewport else { return };
        self.set(viewport, Prop::bWindowsMouseAvailable, Value::Bool(true));
        self.set(viewport, Prop::WindowsMouseX, Value::Float(x));
        self.set(viewport, Prop::WindowsMouseY, Value::Float(y));
    }

    /// `Some(ratio)` while on. The host draws the level in a band `height / ratio` high, centred, the rest black; the HUD
    /// still draws over the whole screen.
    pub fn cinematics(&self) -> Option<f32> {
        let client = &self.host.client;
        client.cinematics.then(|| client.cinematics_ratio.unwrap_or(CINEMATICS_RATIO))
    }

    pub fn shows_mouse(&self) -> bool {
        self.host.client.viewport.is_some_and(|viewport| self.flag_of(viewport, Prop::bShowWindowsMouse))
    }

    pub fn set_video_modes(&mut self, modes: &'static [VideoMode]) {
        self.host.client.video_modes = modes;
    }

    pub(crate) fn supported_resolution(&self, command: &str) -> String {
        let Some((_, number)) = command.split_once("index=") else { return String::new() };
        let index = number.split_whitespace().next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        self.host.client.video_modes.get(index).map_or(String::new(), |mode| format!("{}x{}x{}x{}", mode.width, mode.height, mode.refresh_rate, mode.bit_depth))
    }

    fn add_interaction(&mut self, master: ObjectId, name: &str, owner: Option<ObjectId>) -> Result<Option<ObjectId>, Error> {
        let Some(function) = self.find_object_function(master, "AddInteraction")? else { return Ok(None) };
        let Some(class) = self.object(master).class else { return Ok(None) };
        let args = vec![Value::Str(name.to_owned()), Value::Object(owner)];
        Ok(match self.call(&function, Some(master), class, args)? {
            Some(Value::Object(interaction)) => interaction,
            _ => None,
        })
    }

    pub(crate) fn interactions_key_event(&mut self, key: &str, action: u8, delta: f32) -> Result<bool, Error> {
        let (Some(master), Some(key)) = (self.host.client.master, self.input_key(key)) else { return Ok(false) };
        let Some(function) = self.find_object_function(master, "Process_KeyEvent")? else { return Ok(false) };
        let Some(class) = self.object(master).class else { return Ok(false) };
        for list in self.interaction_lists(master).into_iter().flatten() {
            let args = vec![list, Value::Byte(key), Value::Byte(action), Value::Float(delta)];
            if self.call(&function, Some(master), class, args)?.as_ref().is_some_and(crate::interp::truthy) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `Key` is the character's code where it fits a byte, else 0 (unverified); `Unicode` the character.
    pub fn typed(&mut self, text: &str) -> Result<(), Error> {
        let Some(master) = self.host.client.master else { return Ok(()) };
        let Some(function) = self.find_object_function(master, "Process_KeyType")? else { return Ok(()) };
        let Some(class) = self.object(master).class else { return Ok(()) };
        for c in text.chars().filter(|c| !c.is_control()) {
            let key = u8::try_from(u32::from(c)).unwrap_or(0);
            for list in self.interaction_lists(master).into_iter().flatten() {
                let args = vec![list, Value::Byte(key), Value::Str(c.to_string())];
                if self.call(&function, Some(master), class, args)?.as_ref().is_some_and(crate::interp::truthy) {
                    break;
                }
            }
        }
        Ok(())
    }

    /// The `ISKEYBIND Key Command` console command; matches by substring (unverified).
    pub(crate) fn is_key_bound(&mut self, query: &str) -> bool {
        let mut parts = query.split_whitespace();
        let (Some(Ok(code)), Some(command)) = (parts.next().map(str::parse::<usize>), parts.next()) else { return false };
        self.load_keys();
        let Some(key) = self.host.client.keys.as_ref().and_then(|keys| keys.get(code)).map(|k| k.to_ascii_lowercase()) else { return false };
        self.key_binding(&key).is_some_and(|binding| binding.to_ascii_lowercase().contains(&command.to_ascii_lowercase()))
    }

    /// Keys go in `EInputKey` order; a key's binding is its `|`-separated commands.
    /// `BINDING2KEYSTR` and `BINDING2KEYVAL` match one command, ignoring case (unverified).
    pub(crate) fn input_command(&mut self, command: &str) -> Option<String> {
        const VERBS: [&str; 10] =
            ["KEYNAME", "KEYNO", "KEYBINDING", "BINDING2KEYSTR", "BINDING2KEYVAL", "FINDAXIS", "MOVEBIND", "LOOKBIND", "ISKEYDOWN", "MENUEXCLUSIVEMODE"];
        let (verb, rest) = command.trim().split_once(char::is_whitespace).unwrap_or((command.trim(), ""));
        if !VERBS.iter().any(|v| v.eq_ignore_ascii_case(verb)) {
            return None;
        }
        self.load_keys();
        let keys = self.host.client.keys.clone().unwrap_or_default();
        let rest = rest.trim();
        let commands = |vm: &Self, key: &str| -> Vec<String> {
            vm.key_binding(&key.to_ascii_lowercase()).map(|b| b.split('|').map(|c| c.trim().to_ascii_uppercase()).collect()).unwrap_or_default()
        };
        let nth = |vm: &Self, n: usize, test: &dyn Fn(&str) -> bool| {
            keys.iter().enumerate().filter(|(_, key)| commands(vm, key).iter().any(|c| test(c))).nth(n).map(|(i, key)| (i, key.clone()))
        };
        let quoted = |rest: &str| -> (String, usize) {
            let (text, tail) = match rest.strip_prefix('"').and_then(|r| r.split_once('"')) {
                Some((text, tail)) => (text.to_owned(), tail),
                None => rest.split_once(char::is_whitespace).map_or((rest.to_owned(), ""), |(a, b)| (a.to_owned(), b)),
            };
            (text, tail.trim().parse().unwrap_or(0))
        };
        Some(match verb.to_ascii_uppercase().as_str() {
            "KEYNAME" => keys.get(rest.parse::<usize>().ok()?).cloned().unwrap_or_default(),
            "KEYNO" => self.input_key(rest).map_or(String::new(), |k| k.to_string()),
            "KEYBINDING" => self.key_binding(&rest.to_ascii_lowercase()).unwrap_or_default().to_owned(),
            "ISKEYDOWN" => {
                let down = keys.get(rest.parse::<usize>().unwrap_or(0)).is_some_and(|key| self.key_down(key));
                (if down { "1" } else { "0" }).to_owned()
            }
            "MENUEXCLUSIVEMODE" => match rest.split_whitespace().next() {
                Some(number) => {
                    self.host.client.menu_exclusive = number.parse().unwrap_or(0);
                    "1".to_owned()
                }
                None => "0".to_owned(),
            },
            "BINDING2KEYSTR" | "BINDING2KEYVAL" => {
                let (wanted, n) = quoted(rest);
                let wanted = wanted.to_ascii_uppercase();
                let found = nth(self, n, &|c| c == wanted);
                match (verb.eq_ignore_ascii_case("BINDING2KEYVAL"), found) {
                    (true, Some((i, _))) => i.to_string(),
                    (true, None) => "0".to_owned(),
                    (false, found) => found.map(|(_, key)| key).unwrap_or_default(),
                }
            }
            "FINDAXIS" => {
                let (axis, n) = quoted(rest);
                let prefix = format!("AXIS {}", axis.to_ascii_uppercase());
                nth(self, n, &|c| c.starts_with(&prefix)).map(|(i, _)| i.to_string()).unwrap_or_default()
            }
            "MOVEBIND" | "LOOKBIND" => {
                const MOVE: [&str; 6] = ["MOVEFORWARD", "MOVEBACKWARD", "STRAFELEFT", "STRAFERIGHT", "AXIS ABASEY", "AXIS ASTRAFE"];
                const LOOK: [&str; 8] =
                    ["LOOKUP", "LOOKDOWN", "TURNLEFT", "TURNRIGHT", "AXIS AMOUSEX", "AXIS AMOUSEY", "AXIS ALOOKUP", "AXIS ABASEX"];
                let list: &[&str] = if verb.eq_ignore_ascii_case("MOVEBIND") { &MOVE } else { &LOOK };
                let n = rest.parse().unwrap_or(0);
                nth(self, n, &|c| list.iter().any(|w| c.starts_with(w))).map(|(_, key)| key).unwrap_or_default()
            }
            _ => return None,
        })
    }

    fn input_key(&mut self, key: &str) -> Option<u8> {
        self.load_keys();
        self.host.client.key_codes.get(&key.to_ascii_lowercase()).copied()
    }

    fn load_keys(&mut self) {
        if self.host.client.keys.is_none() {
            let names = self.enum_names("Engine.Interactions.EInputKey");
            let names: Arc<[String]> = names.map(|names| names.iter().map(|name| name.strip_prefix("IK_").unwrap_or(name).to_owned()).collect()).unwrap_or_default();
            let mut codes = FxHashMap::default();
            for (code, name) in names.iter().enumerate() {
                if let Ok(code) = u8::try_from(code) {
                    codes.entry(name.to_ascii_lowercase()).or_insert(code);
                }
            }
            self.host.client.key_codes = codes;
            self.host.client.keys = Some(names);
        }
    }

    fn interaction_lists(&self, master: ObjectId) -> [Option<Value>; 2] {
        let global = self.get(master, Prop::GlobalInteractions);
        let local = self.host.client.viewport.and_then(|viewport| self.get(viewport, Prop::LocalInteractions));
        [global, local]
    }

    pub fn process_interactions(&mut self, event: &str, argument: Value, order: InteractionOrder) -> Result<(), Error> {
        let Some(master) = self.host.client.master else { return Ok(()) };
        let [global, local] = self.interaction_lists(master);
        let lists = match order {
            InteractionOrder::LocalFirst => [local, global],
            InteractionOrder::GlobalFirst => [global, local],
        };
        for list in lists.into_iter().flatten() {
            self.event(master, event, [list, argument.clone()])?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionOrder {
    GlobalFirst,
    LocalFirst,
}

#[cfg(test)]
mod tests {
    use p2_package::Loader;

    use crate::{Value, Vm};

    #[test]
    fn cinematics_command() {
        let empty = std::env::temp_dir();
        let loader = Loader::new(&empty, &[]).unwrap();
        let mut vm = Vm::new(&loader);
        let command = |vm: &mut Vm<&Loader>, text: &str| {
            assert_eq!(vm.call_native("ConsoleCommand", None, &mut [Value::Str(text.to_owned())]).unwrap(), Some(Value::Str(String::new())), "{text}");
        };
        assert_eq!(vm.cinematics(), None);
        command(&mut vm, "CINEMATICS 1");
        assert_eq!(vm.cinematics(), Some(1.66));
        command(&mut vm, "CINEMATICS 0");
        assert_eq!(vm.cinematics(), None);
        command(&mut vm, "cinematics");
        assert_eq!(vm.cinematics(), Some(1.66));
        command(&mut vm, "CINEMATICSRATIO 2.35");
        assert_eq!(vm.cinematics(), Some(2.35));
        command(&mut vm, "CINEMATICS");
        assert_eq!(vm.cinematics(), None);
    }

    #[test]
    fn console_command_answers() {
        let empty = std::env::temp_dir();
        let loader = Loader::new(&empty, &[]).unwrap();
        let mut vm = Vm::new(&loader);
        let answer = |vm: &mut Vm<&Loader>, text: &str| match vm.call_native("ConsoleCommand", None, &mut [Value::Str(text.to_owned())]).unwrap() {
            Some(Value::Str(answer)) => answer,
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(answer(&mut vm, "ISKEYDOWN 18"), "0");
        assert_eq!(answer(&mut vm, "MENUEXCLUSIVEMODE 1"), "1");
        assert_eq!(vm.host.client.menu_exclusive, 1);
        assert_eq!(answer(&mut vm, "MENUEXCLUSIVEMODE 0"), "1");
        assert_eq!(vm.host.client.menu_exclusive, 0);
        assert_eq!(answer(&mut vm, "MENUEXCLUSIVEMODE"), "0");

        vm.set_video_modes(&[
            crate::VideoMode { width: 1024, height: 768, refresh_rate: 60, bit_depth: 32 },
            crate::VideoMode { width: 1920, height: 1080, refresh_rate: 144, bit_depth: 32 },
        ]);
        assert_eq!(answer(&mut vm, "GETSUPPORTEDRESOLUTION INDEX=0"), "1024x768x60x32");
        assert_eq!(answer(&mut vm, "GETSUPPORTEDRESOLUTION INDEX=1"), "1920x1080x144x32");
        assert_eq!(answer(&mut vm, "GETSUPPORTEDRESOLUTION INDEX=2"), "");
        assert_eq!(answer(&mut vm, "GETSUPPORTEDRESOLUTION"), "");

        assert_eq!(answer(&mut vm, "SETMOUSE 12 34"), "");
        assert!(vm.host.commands.iter().any(|command| matches!(command, crate::HostCommand::SetMouse([12, 34]))));
        assert_eq!(answer(&mut vm, "MenuCancel"), "");

        let exe = std::env::current_exe().unwrap();
        vm.set_save_dir(exe.parent().unwrap().to_path_buf());
        let time = answer(&mut vm, &format!("GETFILETIMESTR {}", exe.file_name().unwrap().to_str().unwrap()));
        assert_eq!((time.len(), &time[4..5], &time[10..13], &time[15..16]), (18, "-", " : ", ":"), "{time}");
        assert_eq!(answer(&mut vm, "GETFILETIMESTR Save9999.usa"), "");
    }
}
