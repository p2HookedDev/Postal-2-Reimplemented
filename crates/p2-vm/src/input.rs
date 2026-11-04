use std::collections::HashMap;
use std::sync::Arc;

use p2_package::Ini;
use p2_script::Type;

use p2_math::{Rotator, Vector};
use crate::Prop;
use crate::{Error, LoaderRef, ObjectId, Value, Vm, zero_slot};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerView {
    pub actor: Option<ObjectId>,
    pub location: Vector,
    pub rotation: Rotator,
    pub behind: bool,
}

const CPF_INPUT: u32 = 0x4;
const CPF_OPTIONAL_PARM: u32 = 0x10;
const FUNC_EXEC: u32 = 0x200;
const AXIS_RATE: f32 = 20.0;
const ANALOG_SCALE: f32 = 0.01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Press,
    Hold,
    Release,
    Axis,
}

#[derive(Debug, Clone, PartialEq)]
enum InputCommand {
    Byte { kind: &'static str, name: Box<str> },
    Axis { name: Box<str>, speed: f32, invert: f32, joystick: bool },
    Word(Box<str>),
}

impl InputCommand {
    fn parse(command: &str) -> Self {
        let (word, rest) = split_word(command);
        let (name, _) = split_word(rest);
        if let Some(kind) = ["BUTTON", "PULSE", "TOGGLE", "COUNT"].into_iter().find(|kind| word.eq_ignore_ascii_case(kind)) {
            return Self::Byte { kind, name: name.into() };
        }
        if word.eq_ignore_ascii_case("AXIS") {
            return Self::Axis {
                name: name.into(),
                speed: parse_option(rest, "SPEED=").unwrap_or(1.0),
                invert: parse_option(rest, "INVERT=").map_or(1.0, f32::trunc),
                joystick: parse_option(rest, "SPEEDBASE=").unwrap_or(0.0) > 0.0,
            };
        }
        Self::Word(word.into())
    }
}

#[derive(Debug, PartialEq)]
struct BoundCommand {
    text: Box<str>,
    on_release: Option<Box<str>>,
    input: InputCommand,
}

#[derive(Debug, PartialEq)]
struct BoundCommands {
    text: Box<str>,
    commands: Vec<BoundCommand>,
}

impl BoundCommands {
    fn parse(text: &str) -> Self {
        let commands = text.split('|').map(str::trim).filter(|c| !c.is_empty());
        let commands = commands.map(|command| BoundCommand { text: command.into(), on_release: parse_command(command, "OnRelease").map(Into::into), input: InputCommand::parse(command) });
        Self { text: text.into(), commands: commands.collect() }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Input {
    keys: HashMap<String, Arc<BoundCommands>>,
    aliases: Vec<(String, Arc<BoundCommands>)>,
    player: Option<ObjectId>,
    held: Vec<Arc<str>>,
    in_alias: bool,
}

impl Input {
    pub(crate) fn forget_player(&mut self) {
        self.player = None;
    }

    pub(crate) fn bind(&mut self, user: &Ini) {
        self.keys.clear();
        self.aliases.clear();
        for (key, value) in user.section("Engine.Input") {
            if key.to_ascii_lowercase().starts_with("aliases[") {
                let (mut command, mut alias) = (String::new(), String::new());
                for (name, text) in crate::config::tuple_fields(value) {
                    match name.to_ascii_lowercase().as_str() {
                        "command" => text.clone_into(&mut command),
                        "alias" => text.clone_into(&mut alias),
                        _ => {}
                    }
                }
                if !command.is_empty() && !alias.is_empty() && !alias.eq_ignore_ascii_case("None") {
                    self.aliases.push((alias, Arc::new(BoundCommands::parse(&command))));
                }
            } else if !value.is_empty() {
                self.keys.insert(key.to_ascii_lowercase(), Arc::new(BoundCommands::parse(value)));
            }
        }
    }
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn key_binding(&self, key: &str) -> Option<&str> {
        self.host.input.keys.get(key).map(|commands| &*commands.text)
    }

    pub fn player(&self) -> Option<ObjectId> {
        self.host.input.player
    }

    pub fn player_view(&mut self) -> Result<Option<PlayerView>, Error> {
        let Some(player) = self.host.input.player else { return Ok(None) };
        let Some(function) = self.find_object_function(player, "PlayerCalcView")? else { return Ok(None) };
        let Some(class) = self.object(player).class else { return Ok(None) };
        let args = vec![Value::Object(None), Value::Vector(Vector::ZERO), Value::Rotator([0; 3].into())];
        let (_, out) = self.run(&function, Some(player), class, args)?;
        let actor = match out.first() {
            Some(&Value::Object(actor)) => actor,
            _ => None,
        };
        let location = crate::natives::vec3(out.get(1));
        let rotation = crate::natives::rot3(out.get(2));
        Ok(Some(PlayerView { actor, location, rotation, behind: self.flag_of(player, Prop::bBehindView) }))
    }

    /// An actor on a bone of a skeletal base is drawn by its base, through the same filter (unverified),
    /// so the player's third-person weapon goes with their body. A base in `hud_drawn` is always drawn.
    pub fn filtered_from_view(&self, actor: ObjectId, view: Option<&PlayerView>, hud_drawn: &[ObjectId]) -> bool {
        let mut at = actor;
        for _ in 0..8 {
            if self.filtered_alone(at, view) {
                return true;
            }
            match self.bone_base(at) {
                Some(base) if hud_drawn.contains(&base) => return false,
                Some(base) => at = base,
                None => return false,
            }
        }
        false
    }

    fn bone_base(&self, actor: ObjectId) -> Option<ObjectId> {
        let base = self.object_of(actor, Prop::Base)?;
        let on_bone = matches!(self.get(actor, Prop::AttachmentBone), Some(Value::Name(bone)) if bone != crate::NameId::NONE);
        (on_bone && self.skeletal_mesh(base).is_some()).then_some(base)
    }

    fn filtered_alone(&self, actor: ObjectId, view: Option<&PlayerView>) -> bool {
        if self.flag_of(actor, Prop::bHidden) {
            return true;
        }
        let viewer = view.and_then(|view| view.actor);
        if viewer.is_some_and(|viewer| self.is_owned_by(actor, viewer)) {
            self.flag_of(actor, Prop::bOwnerNoSee) && !view.is_some_and(|view| view.behind)
        } else {
            self.flag_of(actor, Prop::bOnlyOwnerSee)
        }
    }

    pub(crate) fn set_player(&mut self, controller: ObjectId) -> Result<(), Error> {
        self.host.input.player = Some(controller);
        if let Some(viewport) = self.viewport() {
            self.set(controller, Prop::Player, Value::Object(Some(viewport)));
            self.set(viewport, Prop::Actor, Value::Object(Some(controller)));
        }
        self.host.input.held.clear();
        self.event(controller, "InitInputSystem", Vec::new())
    }

    pub(crate) fn key_down(&self, key: &str) -> bool {
        self.host.input.held.iter().any(|held| held.eq_ignore_ascii_case(key))
    }

    pub fn player_input(&mut self, held_keys: &[&str], mouse: [f32; 2]) -> Result<(), Error> {
        let released: Vec<Arc<str>> = self.host.input.held.iter().filter(|k| !held_keys.iter().any(|held| held.eq_ignore_ascii_case(k))).cloned().collect();
        let pressed: Vec<Arc<str>> = held_keys
            .iter()
            .enumerate()
            .filter(|&(i, key)| (i == 0 || !held_keys[i - 1].eq_ignore_ascii_case(key)) && !self.key_down(key))
            .map(|(_, key)| Arc::from(key.to_ascii_lowercase()))
            .collect();
        for key in released {
            self.key_event(&key, false)?;
        }
        for key in pressed {
            self.key_event(&key, true)?;
        }
        for (axis, value) in [("mousex", mouse[0]), ("mousey", mouse[1])] {
            if value != 0.0 {
                self.input_event(axis, Action::Axis, value)?;
            }
        }
        Ok(())
    }

    pub fn key_event(&mut self, key: &str, pressed: bool) -> Result<(), Error> {
        let key: Arc<str> = Arc::from(key.to_ascii_lowercase());
        match (pressed, self.key_down(&key)) {
            (true, false) => {
                self.host.input.held.push(Arc::clone(&key));
                self.input_event(&key, Action::Press, 0.0)
            }
            (false, true) => {
                self.host.input.held.retain(|k| *k != key);
                self.input_event(&key, Action::Release, 0.0)
            }
            _ => Ok(()),
        }
    }

    fn input_event(&mut self, key: &str, action: Action, delta: f32) -> Result<(), Error> {
        let code = match action {
            Action::Press => crate::client::IST_PRESS,
            Action::Release => crate::client::IST_RELEASE,
            _ => crate::client::IST_AXIS,
        };
        if self.interactions_key_event(key, code, delta)? {
            return Ok(());
        }
        match self.host.input.player {
            Some(player) => self.process(player, key, action, delta),
            None => Ok(()),
        }
    }

    pub(crate) fn player_tick(&mut self, controller: ObjectId, delta: f32) -> Result<(), Error> {
        let has_input = |vm: &Self| vm.object_of(controller, Prop::PlayerInput).is_some();
        if !has_input(self) {
            self.event(controller, "InitInputSystem", Vec::new())?;
            if !has_input(self) {
                return Ok(());
            }
        }
        self.read_input(controller, Some(delta))?;
        let result = self.event(controller, "PlayerTick", [Value::Float(delta)]);
        self.read_input(controller, None)?;
        result
    }

    fn read_input(&mut self, controller: ObjectId, delta: Option<f32>) -> Result<(), Error> {
        if let Some(delta) = delta {
            let mut next = 0;
            while let Some(key) = self.host.input.held.get(next).map(Arc::clone) {
                next += 1;
                self.process(controller, &key, Action::Hold, delta)?;
            }
        }
        let scale = match delta {
            Some(delta) if delta > 0.0 => AXIS_RATE / delta,
            _ => 0.0,
        };
        let Some(class) = self.object(controller).class else { return Ok(()) };
        let layout = Arc::clone(&self.class(class).layout);
        let fields = &self.script.memory.get(self.class(class).mem).fields;
        let words = &mut self.store.objects[controller.0 as usize].mem.words;
        for (property, field) in layout.properties.iter().zip(fields) {
            if property.flags & CPF_INPUT != 0
                && field.shape == crate::memory::Shape::Float
                && let Some(word) = words.get_mut(field.word as usize)
            {
                *word = (f32::from_bits(*word) * scale).to_bits();
            }
        }
        Ok(())
    }

    fn process(&mut self, controller: ObjectId, key: &str, action: Action, delta: f32) -> Result<(), Error> {
        let Some(commands) = self.host.input.keys.get(key).cloned() else { return Ok(()) };
        self.exec_input_commands(controller, &commands, action, delta)
    }

    fn exec_input_commands(&mut self, controller: ObjectId, commands: &BoundCommands, action: Action, delta: f32) -> Result<(), Error> {
        if self.host.client.menu_exclusive != 0
            && matches!(action, Action::Press)
            && let Some(button) = exclusive_menu_button(&commands.text)
        {
            self.viewport_exec(controller, button, action, delta)?;
            return Ok(());
        }
        for command in &commands.commands {
            match (action, &command.on_release) {
                (Action::Press, _) => {
                    self.viewport_exec(controller, &command.text, action, delta)?;
                }
                (Action::Release, Some(rest)) => {
                    self.viewport_exec(controller, rest, action, delta)?;
                }
                _ => {
                    self.run_input_command(controller, &command.text, &command.input, action, delta)?;
                }
            }
        }
        Ok(())
    }

    /// Input commands first, then exec functions of: game, HUD, cheat and admin managers, the controller,
    /// its `PlayerInput`, its pawn, the pawn's weapon and selected item. Renderer commands go unhandled.
    fn viewport_exec(&mut self, controller: ObjectId, command: &str, action: Action, delta: f32) -> Result<bool, Error> {
        if self.input_exec(controller, command, action, delta)? {
            return Ok(true);
        }
        let object = |vm: &Self, of: Option<ObjectId>, name: Prop| of.and_then(|o| vm.object_of(o, name));
        let pawn = object(self, Some(controller), Prop::Pawn);
        let targets = [
            object(self, self.level, Prop::Game),
            object(self, Some(controller), Prop::myHUD),
            object(self, Some(controller), Prop::CheatManager),
            object(self, Some(controller), Prop::AdminManager),
            Some(controller),
            object(self, Some(controller), Prop::PlayerInput),
            pawn,
            object(self, pawn, Prop::Weapon),
            object(self, pawn, Prop::SelectedItem),
        ];
        for target in targets.into_iter().flatten() {
            if self.script_console_exec(target, command)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn player_console_exec(&mut self, command: &str) -> Result<bool, Error> {
        let Some(player) = self.host.input.player else { return Ok(false) };
        self.viewport_exec(player, command, Action::Press, 0.0)
    }

    fn input_exec(&mut self, controller: ObjectId, command: &str, action: Action, delta: f32) -> Result<bool, Error> {
        self.run_input_command(controller, command, &InputCommand::parse(command), action, delta)
    }

    fn run_input_command(&mut self, controller: ObjectId, command: &str, input: &InputCommand, action: Action, delta: f32) -> Result<bool, Error> {
        match *input {
            InputCommand::Byte { kind, ref name } => {
                let pawn = self.object_of(controller, Prop::Pawn).filter(|_| matches!(kind, "BUTTON" | "PULSE"));
                let found = [Some(controller), pawn].into_iter().flatten().find_map(|o| self.input_slot(o, name, &Type::Byte).map(|i| (o, i)));
                let Some((object, index)) = found else {
                    self.diagnostics.log.push(format!("{command} Bad {kind} command"));
                    return Ok(true);
                };
                let Some(Value::Byte(value)) = self.property_at(object, index) else { return Ok(true) };
                let new = match (kind, action) {
                    ("BUTTON" | "PULSE", Action::Press) => Some(1),
                    ("BUTTON", Action::Release) if value != 0 => Some(0),
                    ("TOGGLE", Action::Press) => Some(value ^ 0x80),
                    ("COUNT", _) => Some(value.wrapping_add(1)),
                    _ => None,
                };
                if let Some(new) = new {
                    self.set_property_at(object, index, Value::Byte(new));
                }
                Ok(true)
            }
            InputCommand::Axis { ref name, speed, invert, joystick } => {
                let Some(index) = self.input_slot(controller, name, &Type::Float) else {
                    self.diagnostics.log.push(format!("{command} Bad Axis command"));
                    return Ok(true);
                };
                let added = match action {
                    _ if joystick => 0.0,
                    Action::Axis => delta * ANALOG_SCALE * speed * invert,
                    Action::Hold => delta * speed * invert,
                    Action::Press | Action::Release => 0.0,
                };
                if let Some(Value::Float(value)) = self.property_at(controller, index) {
                    self.set_property_at(controller, index, Value::Float(value + added));
                }
                Ok(true)
            }
            InputCommand::Word(ref word) if !self.host.input.in_alias => {
                let Some((_, commands)) = self.host.input.aliases.iter().find(|(alias, _)| alias.eq_ignore_ascii_case(word)) else {
                    return Ok(false);
                };
                let commands = Arc::clone(commands);
                self.host.input.in_alias = true;
                let result = self.exec_input_commands(controller, &commands, action, delta);
                self.host.input.in_alias = false;
                result.map(|()| true)
            }
            InputCommand::Word(_) => Ok(false),
        }
    }

    fn input_slot(&self, object: ObjectId, name: &str, ty: &Type) -> Option<usize> {
        let class = self.object(object).class?;
        self.class(class).layout.properties.iter().position(|f| {
            f.flags & CPF_INPUT != 0 && f.name.eq_ignore_ascii_case(name) && std::mem::discriminant(&f.ty) == std::mem::discriminant(ty)
        })
    }

    /// The last string parameter takes the line's rest; a missing required one logs and calls nothing.
    fn script_console_exec(&mut self, object: ObjectId, command: &str) -> Result<bool, Error> {
        let Some(class) = self.object(object).class else { return Ok(false) };
        let (name, mut rest) = split_word(command);
        let Some(function) = self.find_object_function(object, name)? else { return Ok(false) };
        if function.flags & FUNC_EXEC == 0 {
            return Ok(false);
        }
        let mut args = Vec::with_capacity(function.params.len());
        for (position, &param) in function.params.iter().enumerate() {
            let field = &function.layout.properties[param];
            let text = if position + 1 == function.params.len() && matches!(field.ty, Type::Str | Type::String) {
                std::mem::take(&mut rest).trim()
            } else {
                let (word, after) = split_word(rest.trim_start_matches([' ', ',']));
                rest = after;
                word
            };
            let value = if text.is_empty() { None } else { self.import(&field.ty, text) };
            // Retail float and int (unverified) text import never fails: empty is 0,
            // so the bare `Fire` binding runs `DudePlayer.Fire(float Value)`.
            let numeric = text.is_empty() && matches!(field.ty, Type::Int | Type::Float);
            match value {
                Some(value) => args.push(value),
                None if numeric || field.flags & CPF_OPTIONAL_PARM != 0 => args.push(zero_slot(&field.ty, field.array_dim)),
                None => {
                    self.diagnostics.log.push(format!("{command}: bad or missing parameter {}", field.name));
                    return Ok(true);
                }
            }
        }
        self.call(&function, Some(object), class, args)?;
        Ok(true)
    }
}

fn parse_command<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let text = text.trim_start();
    let head = text.get(..word.len())?;
    let rest = &text[word.len()..];
    (head.eq_ignore_ascii_case(word) && !rest.starts_with(|c: char| c.is_ascii_alphanumeric())).then(|| rest.trim_start())
}

fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    (&text[..end], &text[end..])
}

fn parse_option(text: &str, key: &str) -> Option<f32> {
    let at = text.as_bytes().windows(key.len()).position(|window| window.eq_ignore_ascii_case(key.as_bytes()))? + key.len();
    let value = &text[at..];
    let end = value.find(|c: char| !(c.is_ascii_digit() || matches!(c, '+' | '-' | '.'))).unwrap_or(value.len());
    value[..end].parse().ok()
}

fn exclusive_menu_button(commands: &str) -> Option<&'static str> {
    ["MenuRightButton", "MenuLeftButton", "MenuDownButton", "MenuUpButton"].into_iter().find(|button| commands.contains(*button))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_and_options() {
        assert_eq!(parse_command("OnRelease HideScoreboard", "onrelease"), Some("HideScoreboard"));
        assert_eq!(parse_command("OnReleased X", "OnRelease"), None);
        assert_eq!(split_word("  Axis aBaseY  Speed=+1200.0"), ("Axis", " aBaseY  Speed=+1200.0"));
        assert_eq!(parse_option(" aBaseY  Speed=+1200.0", "SPEED="), Some(1200.0));
        assert_eq!(parse_option(" aStrafe SPEEDBASE=4.00 DEADZONE=0.10", "SPEED="), None);
        assert_eq!(parse_option(" aStrafe SPEEDBASE=4.00 DEADZONE=0.10", "SPEEDBASE="), Some(4.0));
        assert_eq!(parse_option(" aBaseY  speed=+1200.0", "SPEED="), Some(1200.0));
        assert_eq!(InputCommand::parse("Axis aBaseY  Speed=+1200.0 Invert=-1"), InputCommand::Axis { name: "aBaseY".into(), speed: 1200.0, invert: -1.0, joystick: false });
        assert_eq!(InputCommand::parse("button bFire"), InputCommand::Byte { kind: "BUTTON", name: "bFire".into() });
        assert_eq!(InputCommand::parse("MoveForward"), InputCommand::Word("MoveForward".into()));
    }

    #[test]
    fn binds_keys_and_aliases() {
        let ini = Ini::parse(
            "[Engine.Input]\nAliases[2]=(Command=\"Axis aBaseY  Speed=+1200.0\",alias=\"MoveForward\")\n\
             Aliases[32]=(Command=\"\",Alias=\"None\")\nW=MoveForward\nX=\n",
        );
        let mut input = Input::default();
        input.bind(&ini);
        assert_eq!(input.keys.get("w").map(|commands| &*commands.text), Some("MoveForward"));
        assert!(!input.keys.contains_key("x"));
        assert_eq!(input.aliases, [("MoveForward".to_owned(), Arc::new(BoundCommands::parse("Axis aBaseY  Speed=+1200.0")))]);
    }

    #[test]
    fn exclusive_menu_button_takes_the_table_order() {
        assert_eq!(exclusive_menu_button("InventoryNext | MenuRightButton"), Some("MenuRightButton"));
        assert_eq!(exclusive_menu_button("MenuUpButton | MenuLeftButton"), Some("MenuLeftButton"));
        assert_eq!(exclusive_menu_button("Fire"), None);
    }
}
