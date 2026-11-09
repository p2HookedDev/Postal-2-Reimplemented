use crate::enums::{EDrawType, EProbe};
use crate::{Error, LoaderRef, NameId, ObjectId, Value, Vm, Prop, Kind};
use super::{AnimChannel, DEFAULT_SEQUENCE_RATE, MIN_SPEED_RATE, Playback, START_LOOP, START_ONCE, channel_animating};
use crate::anim::MAX_PASSES;

impl<L: LoaderRef> Vm<L> {
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
                if remaining <= 0.0 || self.store.is_deleted(actor) || !self.is_animating(actor, index as i32) {
                    break;
                }
                let velocity = self.store.vec3_of(actor, Prop::Velocity);
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

    pub(super) fn finish_anim(&mut self, this: ObjectId, channel: i32) {
        let target = if self.store.is_a(this, Kind::Controller) {
            let Some(pawn) = self.store.object_of(this, Prop::Pawn) else { return };
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
    pub(crate) fn play(&mut self, actor: ObjectId, channel: i32, sequence: NameId, rate: f32, tween: f32, playback: Playback) -> bool {
        let looping = playback == Playback::Looping;
        if sequence == NameId::NONE || self.skeletal_mesh(actor).is_none() {
            return false;
        }
        let Some((animation, (frames, own_rate, has_notifies))) = self.find_sequence_with(actor, sequence, |s| (s.frames, s.rate, s.has_notifies)) else {
            let mesh = self.store.object_of(actor, Prop::Mesh).map_or_else(String::new, |mesh| self.store.object(mesh).path.clone());
            self.diagnostics.log.push(format!("PlayAnim: Sequence '{}' not found for mesh '{mesh}'", self.store.name(sequence)));
            return false;
        };
        let animating = self.is_animating(actor, channel);
        let velocity = self.store.vec3_of(actor, Prop::Velocity);
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
        if self.store.is_a(actor, Kind::Pawn) {
            if channel == 0 {
                self.store.set(actor, Prop::AnimAction, Value::Name(NameId::NONE));
            }
            if let Some(controller) = self.store.object_of(actor, Prop::Controller)
                && self.is_probing(controller, EProbe::AnimEnd as u32)?
            {
                target = controller;
            }
        }
        if self.is_probing(target, EProbe::AnimEnd as u32)? {
            self.event(target, "AnimEnd", [Value::Int(channel as i32)])?;
        }
        Ok(())
    }

    pub(super) fn is_animating(&self, actor: ObjectId, channel: i32) -> bool {
        self.draw_type_of(actor) == Some(EDrawType::Mesh) && self.channel(actor, channel).is_some_and(channel_animating)
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
}

fn leftover(remaining: f32, over: f32, moved: f32) -> f32 {
    if moved > 0.0 { remaining * over / moved } else { 0.0 }
}
