use std::sync::Arc;

use p2_package::PackageId;
use p2_script::flags::STATE_AUTO;
use p2_script::{ScriptObject, read_export};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::interp::Function;
use crate::{ClassId, Error, NameId, ObjectId, LoaderRef, Value, Vm};

/// Retail's hard-coded probe names (indices 300..363): an event with one of these names is only
/// delivered to objects probing its bit (index − 300).
const PROBE_NAMES: [&str; 64] = [
    "Spawned", "Destroyed", "GainedChild", "LostChild", "Probe4", "Probe5", "Trigger", "UnTrigger", "Timer", "HitWall",
    "Falling", "Landed", "PhysicsVolumeChange", "Touch", "UnTouch", "Bump", "BeginState", "EndState", "BaseChange",
    "Attach", "Detach", "ActorEntered", "ActorLeaving", "KillCredit", "AnimEnd", "EndedRotation", "InterpolateEnd",
    "EncroachingOn", "EncroachedBy", "NotifyTurningInPlace", "HeadVolumeChange", "PainTimer", "SpeechTimer", "MayFall",
    "CheckDirectionChange", "Die", "Tick", "PlayerTick", "Expired", "CheckMovementTransition", "SeePlayer",
    "EnemyNotVisible", "HearNoise", "UpdateEyeHeight", "SeeMonster", "SeeFriend", "SpecialHandling", "BotDesireability",
    "NotifyBump", "NotifyPhysicsVolumeChange", "AIHearSound", "NotifyHeadVolumeChange", "NotifyLanded", "NotifyHitWall",
    "Probe54", "Probe55", "Probe56", "Probe57", "Probe58", "Probe59", "Probe60", "Probe61", "Probe62", "All",
];

/// Callers pass the script spelling, so an exact match is tried first (about 1 700 events a frame).
pub(crate) fn probe_bit(name: &str) -> Option<u32> {
    PROBE_NAMES
        .iter()
        .position(|&probe| probe == name)
        .or_else(|| PROBE_NAMES.iter().position(|probe| probe.eq_ignore_ascii_case(name)))
        .map(|bit| bit as u32)
}

#[derive(Debug, Clone, Copy)]
#[expect(clippy::enum_variant_names, reason = "a variant is spelled as the script event it sends, and one of those is TriggerEvent")]
pub(crate) enum Event {
    Tick,
    Timer,
    PlayerTick,
    Touch,
    UnTouch,
    Bump,
    HitWall,
    Landed,
    Falling,
    BeginState,
    EndState,
    BaseChange,
    Attach,
    Detach,
    AnimEnd,
    EndedRotation,
    EncroachingOn,
    EncroachedBy,
    UpdateEyeHeight,
    MayFall,
    NotifyBump,
    NotifyLanded,
    NotifyHitWall,
    Destroyed,
    GainedChild,
    LostChild,
    HearNoise,
    EnemyNotVisible,
    BreathTimer,
    ChangeAnimation,
    PostRender,
    KeyFrameReached,
    PhysicsChangedFor,
    TakeDamage,
    KImpact,
    Notify,
    Reset,
    DetachFromBone,
    GiveMomentum,
    SuggestMovePreparation,
    SetWalking,
    LongFall,
    HitPathGoal,
    InitInputSystem,
    TravelPreAccept,
    AcceptInventory,
    TravelPostAccept,
    InitGame,
    ComeOutOfStasis,
    SceneStarted,
    SceneEnded,
    TriggerEvent,
    AddCameraEffect,
    RemoveCameraEffect,
    PlayDying,
    PlayLandingAnimation,
    PlayJump,
    PlayFalling,
    CallSubtitle,
    Initialized,
    FellOutOfWorld,
    KExcessiveJointError,
    StartCrouch,
    EndCrouch,
    PostLoadGame,
    PreClientTravel,
    ActorLeaving,
    ZoneChange,
    ActorEntered,
    ActorLeavingVolume,
    PhysicsVolumeChange,
    ActorEnteredVolume,
    PawnLeavingVolume,
    NotifyPhysicsVolumeChange,
    PawnEnteredVolume,
    NotifyHeadVolumeChange,
    HeadVolumeChange,
    SeePlayer,
    SeeMonster,
    Spawned,
    PreBeginPlay,
    BeginPlay,
    PostBeginPlay,
    SetInitialState,
    PostNetBeginPlay,
    PreSaveGame,
    PostSaveGame,
}

/// Script spellings of [`Event`], in the same order as its variants.
pub(crate) const EVENT_NAMES: [&str; 87] = [
    "Tick", "Timer", "PlayerTick", "Touch", "UnTouch", "Bump", "HitWall", "Landed", "Falling", "BeginState", "EndState",
    "BaseChange", "Attach", "Detach", "AnimEnd", "EndedRotation", "EncroachingOn", "EncroachedBy", "UpdateEyeHeight",
    "MayFall", "NotifyBump", "NotifyLanded", "NotifyHitWall", "Destroyed", "GainedChild", "LostChild", "HearNoise",
    "EnemyNotVisible", "BreathTimer", "ChangeAnimation", "PostRender", "KeyFrameReached", "PhysicsChangedFor", "TakeDamage",
    "KImpact", "Notify", "Reset", "DetachFromBone", "GiveMomentum", "SuggestMovePreparation", "SetWalking", "LongFall",
    "HitPathGoal", "InitInputSystem", "TravelPreAccept", "AcceptInventory", "TravelPostAccept", "InitGame",
    "ComeOutOfStasis", "SceneStarted", "SceneEnded", "TriggerEvent", "AddCameraEffect", "RemoveCameraEffect", "PlayDying",
    "PlayLandingAnimation", "PlayJump", "PlayFalling", "CallSubtitle", "Initialized", "FellOutOfWorld",
    "KExcessiveJointError", "StartCrouch", "EndCrouch", "PostLoadGame", "PreClientTravel", "ActorLeaving", "ZoneChange",
    "ActorEntered", "ActorLeavingVolume", "PhysicsVolumeChange", "ActorEnteredVolume", "PawnLeavingVolume",
    "NotifyPhysicsVolumeChange", "PawnEnteredVolume", "NotifyHeadVolumeChange", "HeadVolumeChange", "SeePlayer",
    "SeeMonster", "Spawned", "PreBeginPlay", "BeginPlay", "PostBeginPlay", "SetInitialState", "PostNetBeginPlay",
    "PreSaveGame", "PostSaveGame",
];

/// An event's name and probe bit: from its script spelling, or interned once for a fixed engine event.
pub trait EventKey {
    fn resolve<L: LoaderRef>(self, vm: &mut Vm<L>) -> (NameId, Option<u32>);
}

impl EventKey for &str {
    fn resolve<L: LoaderRef>(self, vm: &mut Vm<L>) -> (NameId, Option<u32>) {
        if let Some(&found) = vm.script.lookups.event_names.get(self) {
            return found;
        }
        let found = (vm.intern(self), probe_bit(self));
        vm.script.lookups.event_names.insert(self.to_owned(), found);
        found
    }
}

impl EventKey for Event {
    fn resolve<L: LoaderRef>(self, vm: &mut Vm<L>) -> (NameId, Option<u32>) {
        vm.script.lookups.events[self as usize]
    }
}

const MAX_STATEMENTS: usize = 10_000;
const MAX_STATE_CHANGES: usize = 5;

#[derive(Debug, Default)]
pub(crate) struct Latent {
    pub sleep: Option<f32>,
    pub finish_anim: Option<(ObjectId, i32)>,
    pub interpolation: Option<ObjectId>,
    pub movement: Option<crate::ai_move::LatentMove>,
}

#[derive(Debug, Default)]
pub(crate) struct States {
    objects: Vec<Option<ObjectState>>,
    /// By object index, the [`ObjectState::key`] of its state (0: none, or past the end): what every virtual call reads.
    keys: Vec<u32>,
    pub probes_disabled: FxHashMap<ObjectId, u64>,
    /// Objects running their `EndState`: a `GotoState` from there does not call `EndState` again.
    pub in_end_state: FxHashSet<ObjectId>,
    pub changed: FxHashSet<ObjectId>,
    generations: u32,
    /// By object index: the store's stamp, plus one, when the actor was last found resting; 0 when it was not, or
    /// when its state, probes or animation were touched since. With no write to its memory since that stamp either,
    /// it still rests.
    rested: Vec<u64>,
}

impl States {
    pub(crate) fn of(&self, object: ObjectId) -> Option<&ObjectState> {
        self.objects.get(object.0 as usize)?.as_ref()
    }

    pub(crate) fn of_mut(&mut self, object: ObjectId) -> Option<&mut ObjectState> {
        self.stir(object);
        self.objects.get_mut(object.0 as usize)?.as_mut()
    }

    /// Something a resting actor's rest depends on, outside its memory, may have changed.
    pub(crate) fn stir(&mut self, object: ObjectId) {
        if let Some(rested) = self.rested.get_mut(object.0 as usize) {
            *rested = 0;
        }
    }

    /// The stamp the actor was found resting at, if nothing stirred it since.
    pub(crate) fn rested_at(&self, object: ObjectId) -> Option<u64> {
        self.rested.get(object.0 as usize)?.checked_sub(1)
    }

    pub(crate) fn set_rested(&mut self, object: ObjectId, at: Option<u64>) {
        let index = object.0 as usize;
        if self.rested.len() <= index {
            if at.is_none() {
                return;
            }
            self.rested.resize(index + 1, 0);
        }
        self.rested[index] = at.map_or(0, |at| at + 1);
    }

    pub(crate) fn enter(&mut self, object: ObjectId, entry: ObjectState) {
        self.stir(object);
        let index = object.0 as usize;
        if self.keys.len() <= index {
            self.keys.resize(index + 1, 0);
        }
        if self.objects.len() <= index {
            self.objects.resize_with(index + 1, || None);
        }
        self.keys[index] = entry.key;
        self.objects[index] = Some(entry);
    }

    pub(crate) fn leave(&mut self, object: ObjectId) {
        self.stir(object);
        if let Some(key) = self.keys.get_mut(object.0 as usize) {
            *key = 0;
        }
        if let Some(entry) = self.objects.get_mut(object.0 as usize) {
            *entry = None;
        }
    }

    pub(crate) fn forget(&mut self, gone: &FxHashSet<ObjectId>) {
        for &object in gone {
            self.leave(object);
        }
        self.probes_disabled.retain(|id, _| !gone.contains(id));
        self.in_end_state.retain(|id| !gone.contains(id));
        self.changed.retain(|id| !gone.contains(id));
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ObjectState {
    pub state: (PackageId, usize),
    pub name: NameId,
    pub code_state: (PackageId, usize),
    pub code: Option<usize>,
    pub sleep: f32,
    pub finish_anim: Option<(ObjectId, i32)>,
    pub finish_interpolation: Option<ObjectId>,
    pub latent_move: Option<crate::ai_move::LatentMove>,
    /// Changes on every `GotoState`, so running state code notices it was replaced.
    pub generation: u32,
    pub key: u32,
}

impl<L: LoaderRef> Vm<L> {
    pub fn state_name(&self, object: ObjectId) -> NameId {
        self.script.states.of(object).map_or(NameId::NONE, |s| s.name)
    }

    pub(crate) fn is_in_state(&mut self, object: ObjectId, name: NameId) -> Result<bool, Error> {
        let mut state = self.script.states.of(object).map(|s| s.state);
        while let Some((package, export)) = state {
            let state_name = self.loader.package(package).package.exports.get(export).map(|e| e.object_name);
            if state_name.is_some_and(|state_name| self.package_name(package, state_name) == name) {
                return Ok(true);
            }
            state = self.super_state((package, export))?;
        }
        Ok(false)
    }

    pub(crate) fn state_key(&mut self, state: (PackageId, usize)) -> u32 {
        let next = u32::try_from(self.script.lookups.state_keys.len() + 1).unwrap_or(u32::MAX);
        *self.script.lookups.state_keys.entry(state).or_insert(next)
    }

    pub(crate) fn object_state_key(&self, object: ObjectId) -> u32 {
        self.script.states.keys.get(object.0 as usize).copied().unwrap_or(0)
    }

    pub fn find_object_function(&mut self, object: ObjectId, name: &str) -> Result<Option<Arc<Function>>, Error> {
        let name = self.intern(name);
        self.find_object_function_id(object, name)
    }

    pub fn find_object_function_id(&mut self, object: ObjectId, name: NameId) -> Result<Option<Arc<Function>>, Error> {
        let Some(class) = self.store.object(object).class else { return Ok(None) };
        let start = self.script.states.of(object).map(|s| s.state);
        if let Some(found) = self.script.lookups.functions.get(&(class, start, name)) {
            return Ok(found.clone());
        }
        self.count(crate::profiling::Work::FunctionSearch);
        let text = self.store.name(name).to_owned();
        let mut found = None;
        let mut state = start;
        while let Some((package, export)) = state {
            if let Some((package, export)) = self.find_child(package, export, &text, false)? {
                found = Some(self.load_function(package, export)?);
                break;
            }
            state = self.super_state((package, export))?;
        }
        if found.is_none() {
            found = self.find_function_id(class, name)?;
        }
        self.script.lookups.functions.insert((class, start, name), found.clone());
        Ok(found)
    }

    pub fn goto_state(&mut self, object: ObjectId, state: NameId, label: NameId) -> Result<(), Error> {
        let Some(class) = self.store.object(object).class else { return Ok(()) };
        let auto = state == self.script.lookups.class_names.auto;
        let (target, state) = if auto {
            match self.auto_state(class)? {
                Some((target, name)) => (Some(target), name),
                None => (None, NameId::NONE),
            }
        } else if state == NameId::NONE {
            (None, state)
        } else {
            let found = self.class_state(class, state)?;
            if found.is_none() {
                let line = format!("Attempt to go to non-existent state {}", self.store.name(state));
                self.diagnostics.log.push(line);
                return Ok(());
            }
            (found, state)
        };
        let current = self.script.states.of(object).map(|s| s.state);
        let changed = current != target;
        // A `GotoState` inside `EndState` skips `EndState`; if it entered a state, this one is preempted.
        if changed && current.is_some() && !self.script.states.in_end_state.contains(&object) {
            self.script.states.changed.remove(&object);
            self.script.states.in_end_state.insert(object);
            let ended = self.event(object, Event::EndState, Vec::new());
            self.script.states.in_end_state.remove(&object);
            ended?;
            if self.script.states.changed.contains(&object) {
                return Ok(());
            }
        }
        self.script.states.generations = self.script.states.generations.wrapping_add(1);
        let generation = self.script.states.generations;
        match target {
            Some(target) => {
                let label = if label == NameId::NONE { self.script.lookups.class_names.begin } else { label };
                let (code_state, code) = self.find_label(target, label)?;
                let key = self.state_key(target);
                let entry = ObjectState {
                    state: target,
                    name: state,
                    code_state,
                    code,
                    sleep: 0.0,
                    finish_anim: None,
                    finish_interpolation: None,
                    latent_move: None,
                    generation,
                    key,
                };
                self.script.states.enter(object, entry);
            }
            None => self.script.states.leave(object),
        }
        self.script.states.probes_disabled.remove(&object);
        self.script.states.stir(object);
        if changed && target.is_some() {
            self.event(object, Event::BeginState, Vec::new())?;
        }
        if target.is_some() {
            self.script.states.changed.insert(object);
        }
        Ok(())
    }

    pub fn tick(&mut self, object: ObjectId, delta: f32) -> Result<(), Error> {
        let mut budget = MAX_STATEMENTS;
        let waiting = self.script.states.of(object).is_some_and(|s| s.finish_anim.is_some() || s.finish_interpolation.is_some() || s.latent_move.is_some());
        if waiting && !self.poll_latents(object, delta)? {
            return Ok(());
        }
        let Some(entry) = self.script.states.of_mut(object) else { return Ok(()) };
        if entry.sleep > 0.0 {
            entry.sleep -= delta;
            if entry.sleep > 0.0 {
                return Ok(());
            }
            entry.sleep = 0.0;
        }
        // Stops at the fifth change of state in a frame: that state's code runs next frame.
        let mut changes = 0;
        loop {
            if changes == MAX_STATE_CHANGES {
                return Ok(());
            }
            changes += 1;
            let Some((start, code_state, generation)) = self.script.states.of(object).map(|s| (s.code, s.code_state, s.generation)) else { return Ok(()) };
            let Some(mut pc) = start else { return Ok(()) };
            let code = self.load_function(code_state.0, code_state.1)?;
            let mut frame = self.state_frame(&code, object)?;
            let compiled = self.compiled(&code);
            let replaced = loop {
                if pc >= code.statements.len() {
                    break false;
                }
                let Some(left) = budget.checked_sub(1) else {
                    if let Some(entry) = self.script.states.of_mut(object) {
                        entry.code = None;
                    }
                    return Err(Error::Unsupported("state code never sleeps".to_owned()));
                };
                budget = left;
                self.script.latent = Latent::default();
                let flow = self.exec_statement(&mut frame, &compiled, pc)?;
                if self.script.states.of(object).map(|s| s.generation) != Some(generation) {
                    break true;
                }
                match flow {
                    crate::interp::StepResult::Next => pc += 1,
                    crate::interp::StepResult::Goto(index) => pc = index,
                    crate::interp::StepResult::End => break false,
                }
                if let Some(seconds) = self.script.latent.sleep.take() {
                    if let Some(entry) = self.script.states.of_mut(object) {
                        entry.code = Some(pc);
                        entry.sleep = seconds;
                        let latent = std::mem::take(&mut self.script.latent);
                        (entry.finish_anim, entry.finish_interpolation, entry.latent_move) = (latent.finish_anim, latent.interpolation, latent.movement);
                    }
                    self.give_locals(frame.locals);
                    return Ok(());
                }
            };
            self.give_locals(frame.locals);
            if !replaced {
                if let Some(entry) = self.script.states.of_mut(object) {
                    entry.code = None;
                }
                return Ok(());
            }
        }
    }

    fn poll_latents(&mut self, object: ObjectId, delta: f32) -> Result<bool, Error> {
        if let Some((actor, channel)) = self.script.states.of(object).and_then(|s| s.finish_anim) {
            if !self.anim_finished(actor, channel) {
                return Ok(false);
            }
            if let Some(entry) = self.script.states.of_mut(object) {
                entry.finish_anim = None;
            }
        }
        if let Some(actor) = self.script.states.of(object).and_then(|s| s.finish_interpolation) {
            if !self.interpolation_finished(actor) {
                return Ok(false);
            }
            if let Some(entry) = self.script.states.of_mut(object) {
                entry.finish_interpolation = None;
            }
        }
        if let Some(action) = self.script.states.of(object).and_then(|s| s.latent_move) {
            let generation = self.script.states.of(object).map(|s| s.generation);
            let done = self.poll_latent_move(object, action, delta)?;
            if self.script.states.of(object).map(|s| s.generation) != generation {
                return Ok(false);
            }
            if !done {
                return Ok(false);
            }
            if let Some(entry) = self.script.states.of_mut(object) {
                entry.latent_move = None;
            }
        }
        Ok(true)
    }

    fn class_state(&mut self, class: ClassId, name: NameId) -> Result<Option<(PackageId, usize)>, Error> {
        if let Some(&found) = self.script.lookups.class_states.get(&(class, name)) {
            return Ok(found);
        }
        let text = self.store.name(name).to_owned();
        let mut found = None;
        let mut current = Some(class);
        while let (None, Some(class)) = (found, current) {
            found = self.find_child(self.store.class(class).package, self.store.class(class).export, &text, true)?;
            current = self.store.class(class).super_class;
        }
        self.script.lookups.class_states.insert((class, name), found);
        Ok(found)
    }

    fn auto_state(&mut self, class: ClassId) -> Result<Option<((PackageId, usize), NameId)>, Error> {
        if let Some(&found) = self.script.lookups.auto_states.get(&class) {
            return Ok(found);
        }
        let mut found = None;
        let mut current = Some(class);
        'chain: while let Some(of) = current {
            let (package, export) = (self.store.class(of).package, self.store.class(of).export);
            let states: Vec<(String, (PackageId, usize))> = self
                .children_of(package, export)?
                .iter()
                .filter(|((_, is_state), _)| *is_state)
                .map(|((name, _), &at)| (name.clone(), at))
                .collect();
            for (name, (state_package, state_export)) in states {
                let loaded = self.loader.package(state_package);
                if let Some(ScriptObject::State(state)) = read_export(&loaded.package, &loaded.data, state_export)?
                    && state.flags & STATE_AUTO != 0
                {
                    found = Some(((state_package, state_export), self.intern(&name)));
                    break 'chain;
                }
            }
            current = self.store.class(of).super_class;
        }
        self.script.lookups.auto_states.insert(class, found);
        Ok(found)
    }

    pub fn event(&mut self, object: ObjectId, name: impl EventKey, args: impl IntoIterator<Item = Value>) -> Result<(), Error> {
        self.count(crate::profiling::Work::Event);
        let (name, probe) = name.resolve(self);
        if let Some(bit) = probe
            && !self.is_probing(object, bit)?
        {
            self.count(crate::profiling::Work::EventNotProbed);
            return Ok(());
        }
        let Some(function) = self.find_object_function_id(object, name)? else {
            self.count(crate::profiling::Work::EventNoFunction);
            return Ok(());
        };
        let Some(class) = self.store.object(object).class else { return Ok(()) };
        self.call(&function, Some(object), class, args).map(drop)
    }

    /// A state without code runs its parent's. `None`: no such label; the code is `state`'s and won't run.
    fn find_label(&mut self, state: (PackageId, usize), label: NameId) -> Result<((PackageId, usize), Option<usize>), Error> {
        let mut node = Some(state);
        while let Some(current) = node {
            let code = self.load_function(current.0, current.1)?;
            let index = code.labels.get(&label).and_then(|offset| code.statements.binary_search_by_key(offset, |s| s.offset).ok());
            if index.is_some() {
                return Ok((current, index));
            }
            node = self.super_state(current)?;
        }
        Ok((state, None))
    }

    fn super_state(&mut self, (package, export): (PackageId, usize)) -> Result<Option<(PackageId, usize)>, Error> {
        if let Some(&found) = self.script.lookups.super_states.get(&(package, export)) {
            return Ok(found);
        }
        let loaded = self.loader.package(package);
        let parent = match read_export(&loaded.package, &loaded.data, export)? {
            Some(ScriptObject::State(s)) => s.base.field.super_field,
            _ => 0,
        };
        let found = self.loader.loaded_export(package, parent);
        self.script.lookups.super_states.insert((package, export), found);
        Ok(found)
    }
}

#[cfg(test)]
mod rest_tests {
    use super::*;

    #[test]
    fn a_stirred_actor_is_no_longer_known_to_rest() {
        let mut states = States::default();
        let actor = ObjectId(7);
        assert_eq!(states.rested_at(actor), None);
        states.set_rested(actor, None);
        assert_eq!(states.rested_at(actor), None);
        states.set_rested(actor, Some(0));
        assert_eq!(states.rested_at(actor), Some(0));
        states.set_rested(actor, Some(12));
        assert_eq!(states.rested_at(actor), Some(12));
        assert!(states.of_mut(actor).is_none());
        assert_eq!(states.rested_at(actor), None);
        states.set_rested(actor, Some(12));
        states.leave(actor);
        assert_eq!(states.rested_at(actor), None);
    }
}

#[cfg(test)]
mod event_tests {
    use super::Event::*;
    use super::{EVENT_NAMES, PROBE_NAMES, probe_bit};

    #[test]
    fn each_event_name_is_its_variant_and_keeps_its_probe_bit() {
        let all = [
            Tick, Timer, PlayerTick, Touch, UnTouch, Bump, HitWall, Landed, Falling, BeginState, EndState, BaseChange,
            Attach, Detach, AnimEnd, EndedRotation, EncroachingOn, EncroachedBy, UpdateEyeHeight, MayFall, NotifyBump,
            NotifyLanded, NotifyHitWall, Destroyed, GainedChild, LostChild, HearNoise, EnemyNotVisible, BreathTimer,
            ChangeAnimation, PostRender, KeyFrameReached, PhysicsChangedFor, TakeDamage, KImpact, Notify, Reset,
            DetachFromBone, GiveMomentum, SuggestMovePreparation, SetWalking, LongFall, HitPathGoal, InitInputSystem,
            TravelPreAccept, AcceptInventory, TravelPostAccept, InitGame, ComeOutOfStasis, SceneStarted, SceneEnded,
            TriggerEvent, AddCameraEffect, RemoveCameraEffect, PlayDying, PlayLandingAnimation, PlayJump, PlayFalling,
            CallSubtitle, Initialized, FellOutOfWorld, KExcessiveJointError, StartCrouch, EndCrouch, PostLoadGame,
            PreClientTravel, ActorLeaving, ZoneChange, ActorEntered, ActorLeavingVolume, PhysicsVolumeChange,
            ActorEnteredVolume, PawnLeavingVolume, NotifyPhysicsVolumeChange, PawnEnteredVolume, NotifyHeadVolumeChange,
            HeadVolumeChange, SeePlayer, SeeMonster, Spawned, PreBeginPlay, BeginPlay, PostBeginPlay, SetInitialState,
            PostNetBeginPlay, PreSaveGame, PostSaveGame,
        ];
        assert_eq!(all.len(), EVENT_NAMES.len());
        for (index, event) in all.into_iter().enumerate() {
            let name = EVENT_NAMES[index];
            assert_eq!(event as usize, index);
            assert_eq!(format!("{event:?}"), name);
            let exact = PROBE_NAMES.iter().position(|&probe| probe == name).map(|bit| bit as u32);
            assert_eq!(probe_bit(name), exact, "{name}");
        }
        assert_eq!(probe_bit(EVENT_NAMES[Touch as usize]), Some(13));
        assert_eq!(probe_bit(EVENT_NAMES[PhysicsVolumeChange as usize]), Some(12));
        assert_eq!(probe_bit(EVENT_NAMES[NotifyHeadVolumeChange as usize]), Some(51));
    }
}
