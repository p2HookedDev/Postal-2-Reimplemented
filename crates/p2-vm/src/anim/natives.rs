use crate::native_id::Native;
use crate::natives::{int_arg, name_arg, optional_float};
use crate::{Error, LoaderRef, NameId, ObjectId, Value, Vm};
use super::{Playback, channel_animating};

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn anim_native(
        &mut self,
        native: Native,
        this: Option<ObjectId>,
        args: &mut [Value],
    ) -> Result<Option<Value>, Error> {
        // Omitted optional arguments arrive as `Value::Map`: the engine's defaults apply.
        let Some(this) = this else { return Ok(None) };
        Ok(Some(match native {
            Native::PlayAnim | Native::LoopAnim | Native::PlayAnimAt => {
                let channel = int_arg(args, 3);
                let (rate, tween) = (optional_float(args, 1, 1.0), optional_float(args, 2, 0.0));
                self.play(this, channel, name_arg(args, 0), rate, tween, if native == Native::LoopAnim { Playback::Looping } else { Playback::Once });
                if native == Native::PlayAnimAt
                    && let Some(state) = self.channel_mut(this, channel)
                {
                    state.frame = optional_float(args, 4, 0.0);
                }
                return Ok(None);
            }
            Native::TweenAnim => {
                self.play(this, int_arg(args, 2), name_arg(args, 0), 0.0, optional_float(args, 1, 0.0), Playback::Once);
                return Ok(None);
            }
            Native::IsAnimating => Value::Bool(self.is_animating(this, int_arg(args, 0))),
            Native::IsTweening => Value::Bool(
                self.channel(this, int_arg(args, 0)).is_some_and(|state| state.frame < 0.0 && channel_animating(state)),
            ),
            Native::HasAnim => Value::Bool(self.find_sequence_with(this, name_arg(args, 0), |_| ()).is_some()),
            Native::AnimIsInGroup => {
                let sequence = self.channel(this, int_arg(args, 0)).map_or(NameId::NONE, |state| state.sequence);
                let group = name_arg(args, 1);
                Value::Bool(
                    sequence != NameId::NONE
                        && self.find_sequence_with(this, sequence, |s| s.groups.contains(&group)).is_some_and(|(_, in_group)| in_group),
                )
            }
            Native::GetAnimParams => {
                let state = self.channel(this, int_arg(args, 0)).cloned();
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
                self.finish_anim(this, int_arg(args, 0));
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
                let stage = int_arg(args, 0);
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
                if let Some(state) = self.channel_mut(this, int_arg(args, 0)) {
                    state.blending = Some((target, time));
                }
                return Ok(None);
            }
            Native::SetBoneScale => {
                let (slot, scale, bone) = (int_arg(args, 0), optional_float(args, 1, 1.0), name_arg(args, 2));
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
                if let Some(state) = self.valid_channel(this, int_arg(args, 0)) {
                    state.notify_disabled = !enable;
                }
                return Ok(None);
            }
            _ => return Err(Error::UnknownNative(format!("{native:?}"))),
        }))
    }
}
