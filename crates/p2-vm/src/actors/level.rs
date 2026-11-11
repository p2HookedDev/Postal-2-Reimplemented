use crate::states::{EVENT_NAMES, Event};
use crate::volumes::{OldZone, ZoneEvents};
use crate::{ClassId, Error, HostCommand, InteractionOrder, Kind, ObjectId, LoaderRef, Prop, Value, Vm};
use super::{MAX_TICK, MIN_TICK, add_time};

impl<L: LoaderRef> Vm<L> {
    /// `options` are a URL's (`?Name=Value...`), with the portal after a `#`; the `[DefaultPlayer]`
    /// options of `User.ini` it lacks are added (`Name=` is how `Login` finds a saved game's player).
    pub fn login(&mut self, options: &str) -> Result<Option<(ObjectId, Option<ObjectId>)>, Error> {
        let Some(level) = self.level() else { return Ok(None) };
        let Some(Value::Object(Some(game))) = self.store.get(level, Prop::Game) else { return Ok(None) };
        let Some(class) = self.store.object(game).class else { return Ok(None) };
        let Some(login) = self.find_object_function(game, "Login")? else { return Ok(None) };
        let mut url = crate::travel::TravelUrl::parse(options);
        self.add_default_player_options(&mut url);
        let args = vec![Value::Str(url.portal.clone()), Value::Str(url.options_text()), Value::Str(String::new())];
        let Some(Value::Object(Some(player))) = self.call(&login, Some(game), class, args)? else { return Ok(None) };
        self.set_player(player)?;
        if let Some(post_login) = self.find_object_function(game, "PostLogin")? {
            self.call(&post_login, Some(game), class, vec![Value::Object(Some(player))])?;
        }
        let pawn = match self.store.get(player, Prop::Pawn) {
            Some(Value::Object(pawn)) => pawn,
            _ => None,
        };
        if let Some(pawn) = pawn {
            let mut accepted = vec![player, pawn];
            accepted.extend(self.arrive(pawn)?);
            for &actor in &accepted {
                self.event(actor, Event::TravelPreAccept, Vec::new())?;
            }
            self.event(game, Event::AcceptInventory, [Value::Object(Some(pawn))])?;
            for &actor in &accepted {
                self.event(actor, Event::TravelPostAccept, Vec::new())?;
            }
        }
        Ok(Some((player, pawn)))
    }

    pub(crate) fn load_level(&mut self, map: p2_package::PackageId) -> Result<Vec<ObjectId>, Error> {
        let mut actors = Vec::new();
        for export in 0..self.loader.package(map).package.exports.len() {
            let object = self.load_object(map, export)?;
            if self.store.is_a(object, Kind::LevelInfo) && self.level.is_none() {
                self.level = Some(object);
            }
            if self.store.is_a(object, Kind::Actor) {
                actors.push(object);
            }
            if self.store.is_a(object, Kind::SceneManager) {
                self.construct_scene_manager(object);
            }
        }
        self.build_world(map);
        self.map_name = self.loader.package(map).name.clone();
        Ok(actors)
    }

    pub fn begin_play(
        &mut self,
        map: p2_package::PackageId,
        game_class: ClassId,
        options: &str,
    ) -> Result<Vec<(String, &'static str, Error)>, Error> {
        let actors = self.load_level(map)?;
        let level = self.level.ok_or(Error::Unsupported("map without a LevelInfo".to_owned()))?;
        self.url_options = crate::TravelUrl::parse(options).options;
        crate::natives::update_time(self, level);
        let mut errors = Vec::new();
        // `LevelInfo.bStartup` is set while the map's actors begin play (pawns placed in the map spawn their
        // controllers in `PostBeginPlay` only then).
        self.store.set(level, Prop::bStartup, Value::Bool(true));
        let game = self.spawn(game_class, None, [None, None])?;
        self.store.set(level, Prop::Game, Value::Object(game));
        if let Some(game) = game
            && let Err(e) = self.event(game, Event::InitGame, [Value::Str(options.to_owned()), Value::Str(String::new())])
        {
            errors.push((self.store.object(game).path.clone(), "InitGame", e));
        }
        for event in [Event::PreBeginPlay, Event::BeginPlay, Event::PostBeginPlay, Event::SetInitialState] {
            let post_begin_play = matches!(event, Event::PostBeginPlay);
            if post_begin_play {
                for &actor in &actors {
                    if let Err(e) = self.set_zone(actor, ZoneEvents::Silent, OldZone::Level) {
                        errors.push((self.store.object(actor).path.clone(), "SetZone", e));
                    }
                }
            }
            for &actor in &actors {
                if self.store.is_deleted(actor) {
                    continue;
                }
                if let Err(e) = self.event(actor, event, Vec::new()) {
                    errors.push((self.store.object(actor).path.clone(), EVENT_NAMES[event as usize], e));
                }
                if post_begin_play && self.store.is_a(actor, Kind::SceneManager) && !self.store.is_deleted(actor) {
                    self.prepare_path(actor);
                }
            }
        }
        self.store.set(level, Prop::bStartup, Value::Bool(false));
        Ok(errors)
    }

    pub fn paused(&self) -> bool {
        self.level.is_some_and(|level| matches!(self.store.get(level, Prop::Pauser), Some(Value::Object(Some(_)))))
    }

    pub fn tick_level(&mut self, real: f32) -> Vec<(String, Error)> {
        self.store.count_work = self.diagnostics.count_work;
        self.world.frame += 1;
        let mut delta = real;
        let mut paused = false;
        if let Some(level) = self.level {
            crate::natives::update_time(self, level);
            let dilation = match self.store.get(level, Prop::TimeDilation) {
                Some(Value::Float(dilation)) => dilation,
                _ => 1.0,
            };
            delta = real * dilation;
            paused = self.store.object_of(level, Prop::Pauser).is_some();
            if !paused {
                self.store.set(level, Prop::TimeSeconds, Value::Float(self.store.float_of(level, Prop::TimeSeconds) + delta));
            }
            self.store.set(level, Prop::TimeSecondsAlways, Value::Float(self.store.float_of(level, Prop::TimeSecondsAlways) + delta));
            // `ServerTravel` set `NextURL`: the host travels, from `Vm::travel`, once `NextSwitchCountdown` runs out.
            if let Some(Value::Str(url)) = self.store.get(level, Prop::NextURL)
                && !url.is_empty()
            {
                let countdown = match self.store.get(level, Prop::NextSwitchCountdown) {
                    Some(Value::Float(countdown)) => countdown - delta,
                    _ => 0.0,
                };
                self.store.set(level, Prop::NextSwitchCountdown, Value::Float(countdown));
                if countdown <= 0.0 {
                    self.host.commands.push(HostCommand::Travel(url));
                    self.store.set(level, Prop::NextURL, Value::Str(String::new()));
                }
            }
        }
        let phase = self.profile_start();
        let dynamic = self.dynamic_actors();
        let actors = if paused { dynamic.into_iter().filter(|&o| self.store.flag_of(o, Prop::bAlwaysTick)).collect() } else { dynamic };
        self.profile_phase("actor list", phase);
        let delta = delta.clamp(MIN_TICK, MAX_TICK);
        let phase = self.profile_start();
        let mut errors = if paused { Vec::new() } else { self.karma_tick(delta) };
        self.profile_phase("karma", phase);
        for actor in actors {
            let started = self.profile_start();
            if let Err(e) = self.tick_actor(actor, delta) {
                errors.push((self.store.object(actor).path.clone(), e));
            }
            if let Some(started) = started {
                let class = self.store.names[self.store.object(actor).class_name.0 as usize].as_str();
                add_time(self.diagnostics.tick_profile.get_or_insert_default(), class, started.elapsed());
            }
        }
        if let Err(e) = self.subtitle_tick() {
            errors.push(("subtitles".to_owned(), e));
        }
        let phase = self.profile_start();
        if let Err(e) = self.process_interactions("Process_Tick", Value::Float(delta), InteractionOrder::GlobalFirst) {
            errors.push(("interactions".to_owned(), e));
        }
        self.profile_phase("interactions", phase);
        errors
    }

    pub fn class_of_object(&mut self, object: ObjectId) -> Result<Option<ClassId>, Error> {
        if let Some(&class) = self.script.lookups.class_objects.get(&object) {
            return Ok(class);
        }
        let Some((package, name)) = self.store.object(object).path.split_once('.') else { return Ok(None) };
        let (package, name) = (package.to_owned(), name.to_owned());
        // Not found is kept too (each miss scanned the package's exports by name, ~18 us a `ClassIsChildOf`): the
        // loaded packages only change with the loader, which clears the cache (`travel.rs`).
        let class = match self.find_class(&package, &name) {
            Ok(class) => Some(class),
            Err(Error::ClassNotFound(_) | Error::PackageNotLoaded(_)) => None,
            Err(e) => return Err(e),
        };
        self.script.lookups.class_objects.insert(object, class);
        Ok(class)
    }
}
