use std::collections::HashMap;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use p2_assets::pose::LocalBone;
use p2_assets::read_skeletal_mesh;
use p2_package::PackageId;
use p2_script::{ScriptObject, read_export};

use p2_math::Rotator;
use crate::enums::EDrawType;
use crate::native_id::Native;
use crate::interp::int;
use crate::natives::{AUDIO_DEFAULT_RADIUS, name_arg, optional_float};

use crate::{Error, HostCommand, LoaderRef, NameId, ObjectId, PlayedSound, Value, Vm, Prop, Kind};

const MAX_CHANNEL: i32 = 256;
const PROBE_ANIM_END: u32 = 24;
const START_LOOP: f32 = 1e-4;
const START_ONCE: f32 = 1e-3;
const MAX_PASSES: usize = 4;
const MIN_SPEED_RATE: f32 = 0.3;
const DEFAULT_SEQUENCE_RATE: f32 = 30.0;

#[derive(Debug, Clone)]
struct Sequence {
    name: NameId,
    groups: Vec<NameId>,
    frames: f32,
    rate: f32,
    has_notifies: bool,
    /// Time as a fraction of the sequence, and the notify export (`None`: none, or package not loaded).
    notifies: Vec<(f32, Option<(PackageId, usize)>)>,
}

/// Frames are fractions of the sequence: 0 is its first frame, 1 is one past its last.
/// A negative frame tweens from the previous pose to frame 0.
#[derive(Debug, Clone, Default, PartialEq)]
#[expect(clippy::struct_excessive_bools, reason = "mirrors `MeshAnimChannel`")]
pub struct AnimChannel {
    pub sequence: NameId,
    pub animation: usize,
    /// Sequence fractions per second; negative means scaled by the actor's speed.
    pub rate: f32,
    pub frame: f32,
    pub last: f32,
    pub tween_rate: f32,
    /// The rate before the latest `PlayAnim`, used for a tween time of -1.
    pub old_rate: f32,
    pub orig_rate: f32,
    pub looping: bool,
    pub has_notifies: bool,
    pub notify_disabled: bool,
    pub blend_alpha: f32,
    pub blend_in_time: f32,
    pub blend_out_time: f32,
    pub blend_bone: NameId,
    pub global_pose: bool,
    blending: Option<(f32, f32)>,
    tween_from: Option<Arc<[LocalBone]>>,
    tween_start: f32,
}

impl AnimChannel {
    /// The pose kept when this tween began, and how far the channel has moved from it. The start pose's share falls
    /// linearly with the frame.
    pub fn tween(&self) -> Option<(&Arc<[LocalBone]>, f32)> {
        self.tween_from.as_ref().filter(|_| self.frame < 0.0).map(|from| (from, 1.0 - self.frame / self.tween_start))
    }

    fn begin_tween(&mut self, from: Option<Arc<[LocalBone]>>) {
        (self.tween_from, self.tween_start) = if self.frame < 0.0 { (from, self.frame) } else { (None, 0.0) };
    }
}

#[derive(Debug, Clone)]
struct Instance {
    mesh: ObjectId,
    linked: Vec<ObjectId>,
    channels: Vec<AnimChannel>,
    bone_scales: Vec<(NameId, f32)>,
    bone_turns: Vec<(NameId, Rotator)>,
    rates: Vec<(NameId, f32)>,
}

impl Instance {
    fn new(mesh: ObjectId) -> Self {
        Self { mesh, linked: Vec::new(), channels: Vec::new(), bone_scales: Vec::new(), bone_turns: Vec::new(), rates: Vec::new() }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Anims {
    instances: FxHashMap<ObjectId, Instance>,
    sequences: FxHashMap<ObjectId, Option<Arc<[Sequence]>>>,
    default_anims: HashMap<ObjectId, Option<ObjectId>>,
    probe_masks: FxHashMap<(PackageId, usize), (u64, u64)>,
    notify_objects: HashMap<(PackageId, usize), Option<ObjectId>>,
    skeletal_meshes: FxHashMap<ObjectId, bool>,
}

impl Anims {
    pub(crate) fn forget(&mut self, actors: &std::collections::HashSet<ObjectId>) {
        self.instances.retain(|actor, _| !actors.contains(actor));
    }

    /// The loader changed: a mesh stub may have a skeletal mesh export now.
    pub(crate) fn forget_skeletal_meshes(&mut self) {
        self.skeletal_meshes.clear();
    }
}

impl<L: LoaderRef> Vm<L> {
    pub fn anim_channel(&self, actor: ObjectId, channel: usize) -> Option<(&AnimChannel, Option<ObjectId>)> {
        let instance = self.instance(actor)?;
        let state = instance.channels.get(channel)?;
        Some((state, instance.linked.get(state.animation).copied()))
    }

    pub fn bone_scales(&self, actor: ObjectId) -> impl Iterator<Item = (NameId, f32)> + '_ {
        self.instance(actor).into_iter().flat_map(|instance| instance.bone_scales.iter().copied()).filter(|&(bone, _)| bone != NameId::NONE)
    }

    pub fn bone_rotations(&self, actor: ObjectId) -> &[(NameId, Rotator)] {
        self.instance(actor).map_or(&[], |instance| instance.bone_turns.as_slice())
    }

    pub(crate) fn set_twist_look(&mut self, pawn: ObjectId, look: i32) {
        if !self.flag_of(pawn, Prop::bDoTorsoTwist) {
            return;
        }
        let bones = [Prop::HeadBone, Prop::SpineBone1, Prop::SpineBone2].map(|p| match self.get(pawn, p) {
            Some(Value::Name(name)) => name,
            _ => NameId::NONE,
        });
        let Some(instance) = self.instance_mut(pawn) else { return };
        for (bone, share) in bones.into_iter().zip([-0.45, -0.35, -0.2]) {
            let turn = [0, (f64::from(look) * share) as i32, 0];
            match instance.bone_turns.iter_mut().find(|(b, _)| *b == bone) {
                Some(slot) => slot.1 = turn.into(),
                None => instance.bone_turns.push((bone, turn.into())),
            }
        }
    }

    pub(crate) fn anim_native(
        &mut self,
        native: Native,
        this: Option<ObjectId>,
        args: &mut [Value],
    ) -> Result<Option<Value>, Error> {
        // Omitted optional arguments arrive as `Value::Map`: the engine's defaults apply.
        let int_at = |args: &[Value], n: usize| args.get(n).map_or(0, int);
        let Some(this) = this else { return Ok(None) };
        Ok(Some(match native {
            Native::PlayAnim | Native::LoopAnim | Native::PlayAnimAt => {
                let channel = int_at(args, 3);
                let (rate, tween) = (optional_float(args, 1, 1.0), optional_float(args, 2, 0.0));
                self.play(this, channel, name_arg(args, 0), rate, tween, native == Native::LoopAnim);
                if native == Native::PlayAnimAt
                    && let Some(state) = self.channel_mut(this, channel)
                {
                    state.frame = optional_float(args, 4, 0.0);
                }
                return Ok(None);
            }
            Native::TweenAnim => {
                self.play(this, int_at(args, 2), name_arg(args, 0), 0.0, optional_float(args, 1, 0.0), false);
                return Ok(None);
            }
            Native::IsAnimating => Value::Bool(self.is_animating(this, int_at(args, 0))),
            Native::IsTweening => Value::Bool(
                self.channel(this, int_at(args, 0)).is_some_and(|state| state.frame < 0.0 && channel_animating(state)),
            ),
            Native::HasAnim => Value::Bool(self.find_sequence_with(this, name_arg(args, 0), |_| ()).is_some()),
            Native::AnimIsInGroup => {
                let sequence = self.channel(this, int_at(args, 0)).map_or(NameId::NONE, |state| state.sequence);
                let group = name_arg(args, 1);
                Value::Bool(
                    sequence != NameId::NONE
                        && self.find_sequence_with(this, sequence, |s| s.groups.contains(&group)).is_some_and(|(_, in_group)| in_group),
                )
            }
            Native::GetAnimParams => {
                let state = self.channel(this, int_at(args, 0)).cloned();
                let (sequence, frame, rate) = match state {
                    Some(s) => (s.sequence, s.frame, if s.orig_rate == 0.0 { 1.0 } else { s.rate / s.orig_rate }),
                    None => (NameId::NONE, 0.0, 0.0),
                };
                for (slot, value) in args.iter_mut().skip(1).zip([Value::Name(sequence), Value::Float(frame), Value::Float(rate)]) {
                    *slot = value;
                }
                return Ok(None);
            }
            Native::FinishAnim => {
                self.finish_anim(this, int_at(args, 0));
                return Ok(None);
            }
            Native::StopAnimating => {
                if let Some(instance) = self.instance_mut(this) {
                    for state in &mut instance.channels {
                        state.rate = 0.0;
                        state.tween_rate = 0.0;
                    }
                }
                return Ok(None);
            }
            // A no-op on purpose: retail's skeletal mesh instances do not implement it.
            Native::FreezeAnimAt => return Ok(None),
            Native::LinkSkelAnim => {
                if let (Some(&Value::Object(Some(animation))), Some(instance)) = (args.first(), self.instance_mut(this))
                    && !instance.linked.contains(&animation)
                {
                    instance.linked.push(animation);
                    instance.rates.clear();
                }
                return Ok(None);
            }
            Native::GetDefaultAnim => Value::Object(match args.first() {
                Some(&Value::Object(Some(mesh))) => self.default_anim(mesh),
                _ => None,
            }),
            Native::AnimBlendParams => {
                let stage = int_at(args, 0);
                let (alpha, bone, global) = (optional_float(args, 1, 0.0), name_arg(args, 4), args.get(5).is_some_and(crate::interp::truthy));
                let (blend_in, blend_out) = (optional_float(args, 2, 0.0).min(1.0), optional_float(args, 3, 0.0).min(1.0));
                if stage == 0 {
                    self.diagnostics.log.push("Warning: attempt to set blending parameters for base stage".to_owned());
                } else if let Some(state) = self.valid_channel(this, stage) {
                    state.blend_alpha = alpha;
                    state.blend_in_time = blend_in;
                    state.blend_out_time = blend_out;
                    state.blend_bone = bone;
                    state.global_pose = global;
                }
                return Ok(None);
            }
            Native::AnimBlendToAlpha => {
                let (target, time) = (optional_float(args, 1, 0.0), optional_float(args, 2, 0.0));
                if let Some(state) = self.channel_mut(this, int_at(args, 0)) {
                    state.blending = Some((target, time));
                }
                return Ok(None);
            }
            Native::SetBoneScale => {
                let (slot, scale, bone) = (int_at(args, 0), optional_float(args, 1, 1.0), name_arg(args, 2));
                if let (Ok(slot @ 0..=256), Some(instance)) = (usize::try_from(slot), self.instance_mut(this)) {
                    if instance.bone_scales.len() <= slot {
                        instance.bone_scales.resize(slot + 1, (NameId::NONE, 1.0));
                    }
                    instance.bone_scales[slot] = if scale != 1.0 && bone != NameId::NONE { (bone, scale) } else { (NameId::NONE, 1.0) };
                }
                return Ok(None);
            }
            Native::EnableChannelNotify => {
                let enable = optional_float(args, 1, 1.0) != 0.0;
                if let Some(state) = self.valid_channel(this, int_at(args, 0)) {
                    state.notify_disabled = !enable;
                }
                return Ok(None);
            }
            _ => return Err(Error::UnknownNative(format!("{native:?}"))),
        }))
    }

    pub(crate) fn update_animation(&mut self, actor: ObjectId, delta: f32) -> Result<(), Error> {
        let Some(count) = self.instance(actor).map(|instance| instance.channels.len()) else { return Ok(()) };
        for index in 0..count {
            if let Some(state) = self.channel_mut(actor, index as i32)
                && let Some((target, left)) = state.blending
                && delta > 0.0
            {
                state.blend_alpha += (target - state.blend_alpha) * (delta / left).min(1.0);
                let left = (left - delta).max(0.0);
                state.blending = (left != 0.0).then_some((target, left));
            }
            let mut remaining = delta;
            for _ in 0..MAX_PASSES {
                if remaining <= 0.0 || self.is_deleted(actor) || !self.is_animating(actor, index as i32) {
                    break;
                }
                let velocity = self.vec3_of(actor, Prop::Velocity);
                let Some(state) = self.channel_mut(actor, index as i32) else { break };
                let old = state.frame;
                let notify_disabled = state.notify_disabled;
                let end = if old < 0.0 {
                    state.frame += state.tween_rate * remaining;
                    if state.frame < 0.0 {
                        break;
                    }
                    remaining = leftover(remaining, state.frame, state.frame - old);
                    state.frame = 0.0;
                    state.tween_from = None;
                    state.rate == 0.0
                } else {
                    let rate = if state.rate < 0.0 { (-state.rate * velocity.size()).max(MIN_SPEED_RATE) } else { state.rate };
                    state.frame += rate * remaining;
                    let (new, sequence, animation) = (state.frame, state.sequence, state.animation);
                    if state.has_notifies
                        && !notify_disabled
                        && let Some((time, notify)) = self.next_notify(actor, animation, sequence, old, new)
                    {
                        if let Some(state) = self.channel_mut(actor, index as i32) {
                            state.frame = time;
                        }
                        remaining = leftover(remaining, new - time, new - old);
                        if let Some(notify) = notify {
                            self.run_notify(actor, notify)?;
                        }
                        continue;
                    }
                    let Some(state) = self.channel_mut(actor, index as i32) else { break };
                    if state.frame < state.last {
                        break;
                    }
                    if state.looping {
                        if state.frame >= 1.0 {
                            remaining = leftover(remaining, state.frame - 1.0, state.frame - old);
                            state.frame = 0.0;
                        } else {
                            remaining = 0.0;
                        }
                        old < state.last
                    } else {
                        remaining = leftover(remaining, state.frame - state.last, state.frame - old);
                        state.frame = state.last;
                        let playing = state.rate > 0.0;
                        state.rate = 0.0;
                        playing
                    }
                };
                if end && !notify_disabled {
                    self.notify_anim_end(actor, index)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn anim_finished(&self, actor: ObjectId, channel: i32) -> bool {
        self.skeletal_mesh(actor).is_none()
            || !self.is_animating(actor, channel)
            || self.channel(actor, channel).is_some_and(|state| state.last <= state.frame)
    }

    fn finish_anim(&mut self, this: ObjectId, channel: i32) {
        let target = if self.is_a(this, Kind::Controller) {
            let Some(pawn) = self.object_of(this, Prop::Pawn) else { return };
            pawn
        } else {
            this
        };
        if let Some(state) = self.channel_mut(target, channel) {
            state.looping = false;
        }
        if !self.anim_finished(target, channel) {
            self.script.latent.sleep = Some(0.0);
            self.script.latent.finish_anim = Some((target, channel));
        }
    }

    /// Rate 0 holds the first frame; `tween` -1 tweens at the previous rate.
    pub(crate) fn play(&mut self, actor: ObjectId, channel: i32, sequence: NameId, rate: f32, tween: f32, looping: bool) -> bool {
        if sequence == NameId::NONE || self.skeletal_mesh(actor).is_none() {
            return false;
        }
        let Some((animation, (frames, own_rate, has_notifies))) = self.find_sequence_with(actor, sequence, |s| (s.frames, s.rate, s.has_notifies)) else {
            let mesh = self.object_of(actor, Prop::Mesh).map_or_else(String::new, |mesh| self.object(mesh).path.clone());
            self.diagnostics.log.push(format!("PlayAnim: Sequence '{}' not found for mesh '{mesh}'", self.name(sequence)));
            return false;
        };
        let animating = self.is_animating(actor, channel);
        let velocity = self.vec3_of(actor, Prop::Velocity);
        let keeps_looping = looping && animating && self.channel(actor, channel).is_some_and(|state| state.looping && state.sequence == sequence);
        let from = if !keeps_looping && (tween > 0.0 || tween == -1.0 || frames <= 1.0) { self.kept_pose(actor) } else { None };
        let Some(state) = self.valid_channel(actor, channel) else { return false };
        state.animation = animation;
        if frames <= 0.0 || (!looping && rate < 0.0) {
            return false;
        }
        let orig_rate = own_rate / frames;
        if !looping && rate == 0.0 {
            state.sequence = sequence;
            state.last = 0.0;
            state.has_notifies = false;
            state.looping = false;
            state.rate = 0.0;
            state.old_rate = 0.0;
            state.orig_rate = orig_rate;
            (state.tween_rate, state.frame) = if tween > 0.0 { (1.0 / (frames * tween), -1.0 / frames) } else { (0.0, 0.0) };
            state.begin_tween(from);
            return true;
        }
        if looping && state.sequence == sequence && state.looping && animating {
            state.orig_rate = orig_rate;
            state.rate = orig_rate * rate;
            state.old_rate = state.rate;
            return true;
        }
        let tween = if !looping && state.sequence == NameId::NONE { 0.0 } else { tween };
        state.sequence = sequence;
        state.orig_rate = orig_rate;
        state.rate = orig_rate * rate;
        state.last = 1.0 - 1.0 / frames;
        state.has_notifies = has_notifies;
        state.looping = looping;
        if state.last == 0.0 {
            state.has_notifies = false;
            state.tween_rate = if tween > 0.0 { 1.0 / tween } else { 10.0 };
            state.frame = -1.0 / frames;
            state.rate = 0.0;
            state.old_rate = 0.0;
        } else if tween > 0.0 {
            state.tween_rate = 1.0 / (frames * tween);
            state.frame = -1.0 / frames;
            state.old_rate = state.rate;
        } else if tween == -1.0 {
            state.frame = -1.0 / frames;
            state.tween_rate = match state.old_rate {
                old if old > 0.0 => old,
                old if old < 0.0 => (state.rate * 0.5).max(velocity.size() * -old),
                _ => 1.0 / (frames * 0.025),
            };
            state.old_rate = state.rate;
        } else {
            state.tween_rate = 0.0;
            state.frame = if looping { START_LOOP } else { START_ONCE };
            state.old_rate = state.rate;
        }
        state.begin_tween(from);
        true
    }

    fn notify_anim_end(&mut self, actor: ObjectId, channel: usize) -> Result<(), Error> {
        let mut target = actor;
        if self.is_a(actor, Kind::Pawn) {
            if channel == 0 {
                self.set(actor, Prop::AnimAction, Value::Name(NameId::NONE));
            }
            if let Some(controller) = self.object_of(actor, Prop::Controller)
                && self.is_probing(controller, PROBE_ANIM_END)?
            {
                target = controller;
            }
        }
        if self.is_probing(target, PROBE_ANIM_END)? {
            self.event(target, "AnimEnd", [Value::Int(channel as i32)])?;
        }
        Ok(())
    }

    fn next_notify(&mut self, actor: ObjectId, animation: usize, sequence: NameId, old: f32, new: f32) -> Option<(f32, Option<ObjectId>)> {
        let animation = *self.instance(actor)?.linked.get(animation)?;
        let sequences = self.sequences(animation)?;
        let found = sequences.iter().find(|s| s.name == sequence)?;
        let mut best: Option<(f32, Option<(PackageId, usize)>)> = None;
        for &(time, object) in &found.notifies {
            if old < time && time <= new && best.is_none_or(|(earliest, _)| time < earliest) {
                best = Some((time, object));
            }
        }
        let (time, object) = best?;
        Some((time, object.and_then(|(package, export)| self.notify_object(package, export))))
    }

    fn notify_object(&mut self, package: PackageId, export: usize) -> Option<ObjectId> {
        if let Some(&cached) = self.media.anims.notify_objects.get(&(package, export)) {
            return cached;
        }
        let object = self.load_object(package, export).ok();
        self.media.anims.notify_objects.insert((package, export), object);
        object
    }

    /// `bExpireParticles` emitters are destroyed here; retail lets their particles die out (unverified).
    /// Unattached bone effects use the owner's placement; rotation adds `OffsetRotation` (unverified).
    fn run_notify(&mut self, owner: ObjectId, notify: ObjectId) -> Result<(), Error> {
        let name_of = |vm: &Self, property: Prop| match vm.get(notify, property) {
            Some(Value::Name(name)) => name,
            _ => NameId::NONE,
        };
        let names = &self.script.lookups.class_names;
        let (scripted, script, sound, destroy_effect, effect) =
            (names.anim_notify_scripted, names.anim_notify_script, names.anim_notify_sound, names.anim_notify_destroy_effect, names.anim_notify_effect);
        if self.is_a(notify, scripted) {
            return self.event(notify, "Notify", [Value::Object(Some(owner))]);
        }
        if self.is_a(notify, script) {
            let function = name_of(self, Prop::NotifyName);
            if function != NameId::NONE {
                let function = self.name(function).to_owned();
                self.event(owner, &function, Vec::new())?;
            }
            return Ok(());
        }
        if self.is_a(notify, sound) {
            if let Some(sound) = self.object_of(notify, Prop::Sound) {
                let radius = match self.get(notify, Prop::Radius).as_ref().map_or(0, int) {
                    0 => AUDIO_DEFAULT_RADIUS,
                    radius => radius as f32,
                };
                let volume = (self.float_of(notify, Prop::Volume) * self.sound_volume()).clamp(0.0, 1.0);
                let sound = self.object(sound).path.clone();
                self.subtitle_insert_sound(Some(owner), &sound);
                self.host.commands.push(HostCommand::Sound(PlayedSound { actor: Some(owner), sound, slot: 0, volume, radius, pitch: 1.0, attenuate: true, no_override: false, flashbang: None }));
            }
            return Ok(());
        }
        if self.is_a(notify, destroy_effect) {
            let tag = name_of(self, Prop::DestroyTag);
            if tag == NameId::NONE {
                return Ok(());
            }
            let owned: Vec<ObjectId> = self
                .actors()
                .filter(|&a| !self.is_deleted(a) && self.object_of(a, Prop::Owner) == Some(owner) && self.get(a, Prop::Tag) == Some(Value::Name(tag)))
                .collect();
            for actor in owned.into_iter().rev() {
                self.destroy(actor)?;
            }
            return Ok(());
        }
        if self.is_a(notify, effect) {
            let Some(class) = self.object_of(notify, Prop::EffectClass) else { return Ok(()) };
            let Some(class) = self.class_of_object(class)? else { return Ok(()) };
            let (location, rotation) = (self.vector(owner, Prop::Location), self.rotator_of(owner, Prop::Rotation));
            let (offset, turn) = (self.vector(notify, Prop::OffsetLocation), self.rotator_of(notify, Prop::OffsetRotation));
            let bone = name_of(self, Prop::Bone);
            let attach = bone != NameId::NONE && self.flag_of(notify, Prop::Attach);
            let (at, facing) = if attach {
                (location, rotation)
            } else {
                ((location + rotation.rotate(offset)), [0, 1, 2].map(|i| rotation[i].wrapping_add(turn[i])).into())
            };
            let Some(effect) = self.spawn(class, Some(owner), [Some(Value::Vector(at)), Some(Value::Rotator(facing))])? else { return Ok(()) };
            let tag = name_of(self, Prop::Tag);
            if tag != NameId::NONE {
                self.set(effect, Prop::Tag, Value::Name(tag));
            }
            for property in ["DrawScale", "DrawScale3D"] {
                if let Some(value) = self.get(notify, property) {
                    self.set(effect, property, value);
                }
            }
            if attach {
                self.call_native("AttachToBone", Some(owner), &mut [Value::Object(Some(effect)), Value::Name(bone)])?;
                self.set(effect, Prop::RelativeLocation, Value::Vector(offset));
                self.set(effect, Prop::RelativeRotation, Value::Rotator(turn));
            }
        }
        Ok(())
    }

    pub(crate) fn is_probing(&mut self, object: ObjectId, bit: u32) -> Result<bool, Error> {
        let Some(class) = self.object(object).class else { return Ok(false) };
        let class = (self.class(class).package, self.class(class).export);
        let node = self.script.states.objects.get(&object).map_or(class, |s| s.state);
        let (class_probe, _) = self.probe_masks(class)?;
        let (probe, ignore) = self.probe_masks(node)?;
        let disabled = self.script.states.probes_disabled.get(&object).copied().unwrap_or(0);
        Ok((class_probe | probe) & ignore & !disabled & (1 << bit) != 0)
    }

    fn probe_masks(&mut self, node: (PackageId, usize)) -> Result<(u64, u64), Error> {
        if let Some(&masks) = self.media.anims.probe_masks.get(&node) {
            return Ok(masks);
        }
        let loaded = self.loader.package(node.0);
        let masks = match read_export(&loaded.package, &loaded.data, node.1)? {
            Some(ScriptObject::State(state)) => (state.probe_mask, state.ignore_mask),
            Some(ScriptObject::Class(class)) => (class.base.probe_mask, class.base.ignore_mask),
            _ => (0, u64::MAX),
        };
        self.media.anims.probe_masks.insert(node, masks);
        Ok(masks)
    }

    fn is_animating(&self, actor: ObjectId, channel: i32) -> bool {
        self.draw_type_of(actor) == Some(EDrawType::Mesh) && self.channel(actor, channel).is_some_and(channel_animating)
    }

    fn find_sequence_with<T>(&mut self, actor: ObjectId, name: NameId, read: impl Fn(&Sequence) -> T) -> Option<(usize, T)> {
        let instance = self.instance_mut(actor)?;
        let (mesh, unlinked) = (instance.mesh, instance.linked.is_empty());
        if unlinked && let Some(default) = self.default_anim(mesh) {
            let instance = self.instance_mut(actor)?;
            instance.linked.push(default);
            instance.rates.clear();
        }
        let count = self.media.anims.instances.get(&actor)?.linked.len();
        (0..count).find_map(|index| {
            let animation = *self.media.anims.instances.get(&actor)?.linked.get(index)?;
            let sequences = self.sequences(animation)?;
            sequences.iter().find(|s| s.name == name).map(|s| (index, read(s)))
        })
    }

    pub(crate) fn ensure_instance(&mut self, actor: ObjectId) -> bool {
        self.instance_mut(actor).is_some()
    }

    pub(crate) fn set_anim_sequence(&mut self, actor: ObjectId, channel: i32, sequence: NameId) {
        if self.channel(actor, channel).is_none() {
            return;
        }
        let Some((animation, (rate, frames))) = self.find_sequence_with(actor, sequence, |s| (s.rate, s.frames)) else { return };
        if let Some(state) = self.channel_mut(actor, channel) {
            state.animation = animation;
            state.sequence = sequence;
            state.orig_rate = if frames > 0.0 { rate / frames } else { 0.0 };
        }
    }

    pub(crate) fn anim_rate_on_channel(&mut self, actor: ObjectId, channel: i32) -> f32 {
        let Some(sequence) = self.valid_channel(actor, channel).map(|state| state.sequence) else { return DEFAULT_SEQUENCE_RATE };
        let known = self.media.anims.instances.get(&actor).and_then(|instance| instance.rates.iter().find(|(name, _)| *name == sequence));
        if let Some(&(_, rate)) = known {
            return rate;
        }
        let rate = match self.find_sequence_with(actor, sequence, |s| s.rate) {
            Some((_, rate)) if rate != 0.0 => rate,
            _ => DEFAULT_SEQUENCE_RATE,
        };
        if let Some(instance) = self.media.anims.instances.get_mut(&actor) {
            instance.rates.push((sequence, rate));
        }
        rate
    }

    pub(crate) fn copy_animation(&mut self, actor: ObjectId, from: i32, to: i32) {
        let Some(source) = self.channel(actor, from).cloned() else { return };
        if let Some(state) = self.valid_channel(actor, to) {
            *state = AnimChannel {
                notify_disabled: state.notify_disabled,
                blend_in_time: state.blend_in_time,
                blend_out_time: state.blend_out_time,
                blend_bone: state.blend_bone,
                global_pose: state.global_pose,
                ..source
            };
        }
    }

    pub(crate) fn update_blend_alpha(&mut self, actor: ObjectId, channel: i32, target: f32, step: f32) {
        if let Some(state) = self.channel_mut(actor, channel) {
            let alpha = state.blend_alpha;
            state.blend_alpha = if (alpha - target).abs() > step {
                if alpha < target { alpha + step } else { alpha - step }
            } else {
                target
            };
        }
    }

    fn sequences(&mut self, animation: ObjectId) -> Option<Arc<[Sequence]>> {
        if let Some(cached) = self.media.anims.sequences.get(&animation) {
            return cached.clone();
        }
        let phase = self.profile_start();
        let (loader, path) = (self.loader.clone(), self.object(animation).path.clone());
        let read = loader.loaded_export_by_path_of_class(&path, "MeshAnimation").and_then(|(id, export)| {
            let loaded = loader.package(id);
            let pkg = &loaded.package;
            let entry = pkg.exports.get(export)?;
            if !pkg.object_name(entry.class_index).is_some_and(|c| c.eq_ignore_ascii_case("MeshAnimation")) {
                return None;
            }
            let animation = self.mesh_animation_at(id, export)?;
            let sequences: Arc<[Sequence]> = animation
                .sequences
                .iter()
                .map(|s| Sequence {
                    name: self.intern(pkg.name(s.name)),
                    groups: s.groups.iter().map(|&g| self.intern(pkg.name(g))).collect(),
                    frames: s.num_frames as f32,
                    rate: s.rate,
                    has_notifies: !s.notifies.is_empty(),
                    notifies: s
                        .notifies
                        .iter()
                        .map(|n| (n.time, loader.resolve_loaded(id, n.object).and_then(|found| Some((found.package, found.export?)))))
                        .collect(),
                })
                .collect();
            Some(sequences)
        });
        self.media.anims.sequences.insert(animation, read.clone());
        self.profile_phase("load: mesh animation sequences", phase);
        read
    }

    fn default_anim(&mut self, mesh: ObjectId) -> Option<ObjectId> {
        if let Some(&cached) = self.media.anims.default_anims.get(&mesh) {
            return cached;
        }
        let phase = self.profile_start();
        let (loader, path) = (self.loader.clone(), self.object(mesh).path.clone());
        let found = loader.loaded_export_by_path_of_class(&path, "SkeletalMesh").and_then(|(id, export)| {
            let loaded = loader.package(id);
            let entry = loaded.package.exports.get(export)?;
            let skeletal = read_skeletal_mesh(&loaded.package, &loaded.data, entry).ok()?;
            self.object_ref(&loaded.package, id, skeletal.default_animation)
        });
        self.media.anims.default_anims.insert(mesh, found);
        self.profile_phase("load: skeletal mesh default anim", phase);
        found
    }

    pub(crate) fn skeletal_mesh(&self, actor: ObjectId) -> Option<ObjectId> {
        let mesh = self.object_of(actor, Prop::Mesh)?;
        let skeletal = match self.media.anims.skeletal_meshes.get(&mesh) {
            Some(&known) => known,
            None => self.is_skeletal_mesh(mesh),
        };
        skeletal.then_some(mesh)
    }

    fn skeletal_mesh_kept(&mut self, actor: ObjectId) -> Option<ObjectId> {
        let mesh = self.object_of(actor, Prop::Mesh)?;
        let skeletal = match self.media.anims.skeletal_meshes.get(&mesh) {
            Some(&known) => known,
            None => {
                let known = self.is_skeletal_mesh(mesh);
                self.media.anims.skeletal_meshes.insert(mesh, known);
                known
            }
        };
        skeletal.then_some(mesh)
    }

    /// Also true for a stub of class `Mesh` with a skeletal mesh export at its path.
    fn is_skeletal_mesh(&self, mesh: ObjectId) -> bool {
        let object = self.object(mesh);
        if self.name(object.class_name).eq_ignore_ascii_case("SkeletalMesh") {
            return true;
        }
        let general = ["Mesh", "LodMesh", "Primitive"].iter().any(|c| self.name(object.class_name).eq_ignore_ascii_case(c));
        general && self.loader.loaded_export_by_path_of_class(&object.path, "SkeletalMesh").is_some()
    }

    pub(crate) fn has_animation(&self, actor: ObjectId) -> bool {
        self.media.anims.instances.contains_key(&actor)
    }

    fn instance(&self, actor: ObjectId) -> Option<&Instance> {
        let instance = self.media.anims.instances.get(&actor)?;
        (Some(instance.mesh) == self.skeletal_mesh(actor)).then_some(instance)
    }

    fn instance_mut(&mut self, actor: ObjectId) -> Option<&mut Instance> {
        let mesh = self.skeletal_mesh_kept(actor)?;
        let instance = self.media.anims.instances.entry(actor).or_insert_with(|| Instance::new(mesh));
        if instance.mesh != mesh {
            *instance = Instance::new(mesh);
        }
        Some(instance)
    }

    pub(crate) fn channel(&self, actor: ObjectId, channel: i32) -> Option<&AnimChannel> {
        self.instance(actor)?.channels.get(usize::try_from(channel).ok()?)
    }

    pub(crate) fn channel_mut(&mut self, actor: ObjectId, channel: i32) -> Option<&mut AnimChannel> {
        let mesh = self.skeletal_mesh_kept(actor)?;
        let instance = self.media.anims.instances.get_mut(&actor).filter(|i| i.mesh == mesh)?;
        instance.channels.get_mut(usize::try_from(channel).ok()?)
    }

    pub(crate) fn valid_channel(&mut self, actor: ObjectId, channel: i32) -> Option<&mut AnimChannel> {
        if !(0..=MAX_CHANNEL).contains(&channel) {
            return None;
        }
        let index = channel as usize;
        let instance = self.instance_mut(actor)?;
        if instance.channels.len() <= index {
            instance.channels.resize(index + 1, AnimChannel::default());
        }
        instance.channels.get_mut(index)
    }
}

fn channel_animating(state: &AnimChannel) -> bool {
    let rate = if state.frame < 0.0 { state.tween_rate } else { state.rate };
    state.sequence != NameId::NONE && rate != 0.0
}

fn leftover(remaining: f32, over: f32, moved: f32) -> f32 {
    if moved > 0.0 { remaining * over / moved } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tween_runs_while_the_frame_is_negative() {
        let from: Arc<[LocalBone]> = Arc::new([([0.0, 0.0, 0.0, 1.0], [0.0; 3])]);
        let mut channel = AnimChannel { frame: -0.2, ..AnimChannel::default() };
        channel.begin_tween(Some(from));
        assert_eq!(channel.tween().map(|(_, weight)| weight), Some(0.0));
        channel.frame = -0.05;
        assert_eq!(channel.tween().map(|(_, weight)| weight), Some(0.75));
        channel.frame = 0.0;
        assert!(channel.tween().is_none());
    }

    #[test]
    fn a_start_without_a_tween_keeps_no_pose() {
        let from: Arc<[LocalBone]> = Arc::new([([0.0, 0.0, 0.0, 1.0], [0.0; 3])]);
        let mut channel = AnimChannel { frame: 0.001, ..AnimChannel::default() };
        channel.begin_tween(Some(from));
        assert!(channel.tween_from.is_none());
    }
}
