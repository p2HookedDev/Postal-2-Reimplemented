//! Host order: [`Vm::leave_level`], [`Vm::set_loader`] (a clone of this one, so package ids stay the
//! same), [`Vm::begin_play`], [`Vm::login`].

use std::collections::{HashMap, HashSet};

use crate::Prop;
use crate::{ClassId, Error, HostCommand, Kind, LoaderRef, ObjectId, Value, Vm};

/// Options and portal come in any order: POSTAL 2 sends `Map#Telepad?peer`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TravelUrl {
    pub map: String,
    pub options: Vec<String>,
    pub portal: String,
}

impl TravelUrl {
    pub fn parse(url: &str) -> Self {
        let url = url.trim_start();
        let split = url.find(['?', '#']).unwrap_or(url.len());
        let mut parsed = Self { map: url[..split].to_owned(), ..Self::default() };
        let mut rest = &url[split..];
        while let Some(kind) = rest.chars().next() {
            let end = rest[1..].find(['?', '#']).map_or(rest.len(), |i| i + 1);
            let part = &rest[1..end];
            if kind == '?' {
                if !part.is_empty() {
                    parsed.options.push(part.to_owned());
                }
            } else {
                part.clone_into(&mut parsed.portal);
            }
            rest = &rest[end..];
        }
        parsed
    }

    pub fn options_text(&self) -> String {
        self.options.iter().map(|option| format!("?{option}")).collect()
    }

    pub fn text(&self) -> String {
        let portal = if self.portal.is_empty() { String::new() } else { format!("#{}", self.portal) };
        format!("{}{portal}{}", self.map, self.options_text())
    }

    pub fn resolve(&self, base: &[String], travel_type: ETravelType) -> Self {
        let key = |option: &str| option.split('=').next().unwrap_or(option).to_ascii_lowercase();
        let mut options: Vec<String> = Vec::new();
        if travel_type == ETravelType::Absolute {
            options.clone_from(&self.options);
            if !self.map.is_empty() {
                const DEFAULTS: [&str; 7] = ["name", "team", "class", "skin", "face", "voice", "overrideclass"];
                for option in base {
                    let k = key(option);
                    if option.contains('=') && DEFAULTS.contains(&k.as_str()) && !options.iter().any(|o| key(o) == k) {
                        options.push(option.clone());
                    }
                }
            }
        } else {
            const DROPPED: [&str; 5] = ["push", "pop", "peer", "load", "quiet"];
            options.extend(base.iter().filter(|option| !DROPPED.contains(&option.to_ascii_lowercase().as_str())).cloned());
            for option in &self.options {
                match options.iter_mut().find(|o| key(o) == key(option)) {
                    Some(old) => old.clone_from(option),
                    None => options.push(option.clone()),
                }
            }
        }
        Self { map: self.map.clone(), options, portal: self.portal.clone() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ETravelType {
    Absolute,
    Partial,
    Relative,
}

impl ETravelType {
    pub(crate) fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Absolute,
            1 => Self::Partial,
            _ => Self::Relative,
        }
    }
}

const CPF_TRAVEL: u32 = 0x0001_0000;

#[derive(Debug, Clone)]
pub(crate) struct TravelActor {
    old: ObjectId,
    class: ClassId,
    properties: Vec<(usize, Value)>,
}

impl<L: LoaderRef> Vm<L> {
    pub fn leave_level(&mut self) -> Result<(), Error> {
        if let Some(player) = self.player() {
            self.event(player, "PreClientTravel", Vec::new())?;
            self.travelling = Vec::new();
            if self.travel_items
                && let Some(pawn) = self.object_of(player, Prop::Pawn)
            {
                let mut chain = vec![pawn];
                let mut next = self.object_of(pawn, Prop::Inventory);
                while let Some(item) = next.filter(|item| !chain.contains(item)) {
                    chain.push(item);
                    next = self.object_of(item, Prop::Inventory);
                }
                chain.retain(|&a| !self.is_deleted(a) && (a == pawn || self.flag_of(a, Prop::bTravel)));
                for actor in chain {
                    let Some(class) = self.object(actor).class else { continue };
                    let layout = &self.class(class).layout;
                    let properties = (0..layout.properties.len())
                        .filter(|&i| layout.properties[i].flags & CPF_TRAVEL != 0)
                        .filter_map(|i| self.property_at(actor, i).map(|v| (i, v)))
                        .collect();
                    self.travelling.push(TravelActor { old: actor, class, properties });
                }
            }
        }
        self.travel_items = false;
        let map_prefix = format!("{}.", self.map_name.to_ascii_lowercase());
        let gone: HashSet<ObjectId> = self
            .object_ids()
            .filter(|&o| self.is_a(o, Kind::Actor) || self.object(o).path.to_ascii_lowercase().starts_with(&map_prefix))
            .collect();
        let actors: HashSet<ObjectId> = gone.iter().copied().filter(|&o| self.is_a(o, Kind::Actor)).collect();
        for &actor in &actors {
            self.set(actor, Prop::bDeleteMe, Value::Bool(true));
        }
        // The garbage collection after the unload clears every reference to a deleted actor (interactions, menus and
        // class defaults that pointed into the level now hold `None`).
        let is_gone = |o: ObjectId| actors.contains(&o);
        for (id, object) in self.store.objects.iter_mut().enumerate() {
            if let Some(class) = object.class
                && !actors.contains(&ObjectId(id as u32))
            {
                self.store.memory.forget(self.store.classes[class.0 as usize].mem, &mut object.mem, &is_gone);
            }
        }
        for class in &mut self.store.classes {
            self.store.memory.forget(class.mem, &mut class.defaults, &is_gone);
            if class.class_object.is_some_and(|object| gone.contains(&object)) {
                class.class_object = None;
            }
        }
        self.store.object_ids.retain(|_, id| !gone.contains(id));
        self.script.lookups.object_consts.clear();
        self.script.lookups.class_objects.clear();
        self.store.actor_ids.clear();
        self.store.dynamic_ids.clear();
        self.script.states.forget(&gone);
        self.actor_fx.forget(&gone);
        self.media.anims.forget(&gone);
        self.karma = self.karma.for_next_level();
        self.level = None;
        self.nav_graph = None;
        self.media.subtitle_sounds = crate::util_natives::SubtitleSounds::default();
        self.world = crate::world::World::default();
        self.hash = crate::hash::CollisionHash::default();
        self.script.latent = crate::states::Latent::default();
        self.host.commands.retain(|command| !matches!(command, HostCommand::Projector(_) | HostCommand::Particle(_)));
        self.host.input.forget_player();
        Ok(())
    }

    pub fn url_game_class(&mut self, url: &str) -> Option<ClassId> {
        let path = url.split(['?', '#']).find_map(|option| option.get(..5)?.eq_ignore_ascii_case("Game=").then(|| &option[5..]))?;
        let (package, name) = path.split_once('.')?;
        self.find_class(package, name).ok()
    }

    /// It must have every package of the current one at the same id (a clone of it that loaded more), so classes, functions
    /// and caches stay valid.
    pub fn set_loader(&mut self, loader: L) {
        self.layouts = p2_script::Layouts::new(loader.clone());
        self.loader = loader;
        self.media.anims.forget_skeletal_meshes();
        self.media.bone_data.parsed = crate::bones::MeshAnimations::default();
    }

    /// References between travelled actors follow them; other actor references become `None` (unverified). The items join the
    /// pawn's inventory in their `TravelPreAccept` (`GiveTo`), as in the engine.
    pub(crate) fn arrive(&mut self, pawn: ObjectId) -> Result<Vec<ObjectId>, Error> {
        let travelling = std::mem::take(&mut self.travelling);
        let Some((first, items)) = travelling.split_first() else { return Ok(Vec::new()) };
        let mut map: HashMap<ObjectId, ObjectId> = HashMap::from([(first.old, pawn)]);
        let mut targets = vec![(pawn, first)];
        let placement = [self.get(pawn, Prop::Location), self.get(pawn, Prop::Rotation)];
        for item in items {
            let Some(actor) = self.spawn(item.class, Some(pawn), placement.clone())? else { continue };
            map.insert(item.old, actor);
            targets.push((actor, item));
        }
        for (actor, travel) in &targets {
            for (index, value) in &travel.properties {
                let value = self.remap(value, &map);
                if let Some(class) = self.object(*actor).class
                    && let Some(&field) = self.store.memory.get(self.class(class).mem).fields.get(*index)
                {
                    self.store.memory.write_field(&field, &mut self.store.objects[actor.0 as usize].mem, value);
                }
            }
        }
        Ok(targets[1..].iter().map(|&(actor, _)| actor).collect())
    }

    fn remap(&self, value: &Value, map: &HashMap<ObjectId, ObjectId>) -> Value {
        match value {
            &Value::Object(Some(old)) => match map.get(&old) {
                Some(&new) => Value::Object(Some(new)),
                None if self.is_a(old, Kind::Actor) => Value::Object(None),
                None => Value::Object(Some(old)),
            },
            Value::Struct(members) => Value::Struct(members.iter().map(|m| self.remap(m, map)).collect()),
            Value::Array(items) => Value::Array(items.iter().map(|i| self.remap(i, map)).collect()),
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn travel_keeps_the_level_options() {
        let base = ["Game=GameTypes.GameSinglePlayer", "Mutator=", "Workshop=0", "LOAD", "peer", "Name=Player"].map(String::from);
        let url = TravelUrl::parse("homeatnight#").resolve(&base, ETravelType::Relative);
        assert_eq!(url.text(), "homeatnight?Game=GameTypes.GameSinglePlayer?Mutator=?Workshop=0?Name=Player");
        let url = TravelUrl::parse("suburbs-1#FromHouse?peer?Workshop=1").resolve(&base, ETravelType::Partial);
        assert_eq!(url.options, ["Game=GameTypes.GameSinglePlayer", "Mutator=", "Workshop=1", "Name=Player", "peer"]);
        let url = TravelUrl::parse("startup?Game=Engine.GameInfo").resolve(&base, ETravelType::Absolute);
        assert_eq!(url.options, ["Game=Engine.GameInfo", "Name=Player"]);
    }

    #[test]
    fn url_portal_before_options() {
        let url = TravelUrl::parse("suburbs-1#FromHouse?peer?Game=GameTypes.GameSinglePlayer");
        assert_eq!((url.map.as_str(), url.portal.as_str()), ("suburbs-1", "FromHouse"));
        assert_eq!(url.options_text(), "?peer?Game=GameTypes.GameSinglePlayer");
        let url = TravelUrl::parse("intro.fuk?Game=X?Mutator=?Workshop=0#");
        assert_eq!((url.map.as_str(), url.portal.as_str()), ("intro.fuk", ""));
        assert_eq!(url.options, ["Game=X", "Mutator=", "Workshop=0"]);
        assert_eq!(TravelUrl::parse("?Name=Dude#Start").portal, "Start");
    }
}