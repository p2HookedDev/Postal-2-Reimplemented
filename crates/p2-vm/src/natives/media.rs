use crate::native_id::Native;
use crate::{Error, HostCommand, LoaderRef, MusicCommand, ObjectId, PlayedSound, Prop, ProjectorEvent, Value, Vm};
use super::{AUDIO_DEFAULT_RADIUS, bool_arg, float_arg, given, int_arg, object_arg, optional_float, str_arg};

pub(super) fn sound_native<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>, args: &[Value]) -> Result<Option<Value>, Error> {
    let value = match native {
        Native::PlaySound | Native::PlayOwnedSound => {
            play_sound(vm, this, args);
            return Ok(None);
        }
        Native::PlayMusicAttenuate | Native::PlayMusic => {
            play_music(vm, native, this, args)
        }
        Native::StopAllMusic => {
            vm.host.commands.push(HostCommand::Music(MusicCommand::StopAll { fade_out: float_arg(args, 0) }));
            return Ok(None);
        }
        Native::StopMusic => {
            vm.host.commands.push(HostCommand::Music(MusicCommand::Stop { handle: int_arg(args, 0), fade_out: float_arg(args, 1) }));
            return Ok(None);
        }
        Native::GetSoundDuration => {
            let duration = object_arg(args, 0).and_then(|sound| vm.sound_duration(sound)).unwrap_or(0.0);
            Value::Float(duration)
        }
        _ => return Err(Error::UnknownNative(format!("{native:?}"))),
    };
    Ok(Some(value))
}

pub(super) fn presentation_native<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>, args: &[Value]) -> Result<Option<Value>, Error> {
    let value = match native {
        Native::AbandonProjector => return Ok(None),
        Native::AttachProjector | Native::DetachProjector => {
            if let Some(this) = this {
                if native == Native::AttachProjector {
                    vm.attach_static_meshes(this);
                } else {
                    vm.detach_projected_actors(this);
                }
                vm.host.commands.push(HostCommand::Projector(if native == Native::AttachProjector { ProjectorEvent::Attach(this) } else { ProjectorEvent::Detach(this) }));
            }
            return Ok(None);
        }
        // Stores `ViewTarget`. Retail then calls a virtual function, not analysed; `None` is taken to mean
        // the controller itself (unverified).
        Native::SetViewTarget => {
            if let Some(this) = this {
                let target = object_arg(args, 0).or(Some(this));
                vm.store.set(this, Prop::ViewTarget, Value::Object(target));
            }
            return Ok(None);
        }
        Native::ShowSubtitle => {
            if let Some(this) = this
                && let Some(index) = crate::util_natives::subtitle_index(vm.store.get(this, "SubTag").as_ref(), str_arg(args, 0))
                && let Some(player) = vm.player()
            {
                vm.event(player, "CallSubtitle", vec![Value::Object(None), Value::Int(index), Value::Int(-1), Value::Object(None)])?;
            }
            return Ok(None);
        }
        Native::GetTotalSceneTime => Value::Float(this.map_or(0.0, |this| vm.total_scene_time(this))),
        _ => return Err(Error::UnknownNative(format!("{native:?}"))),
    };
    Ok(Some(value))
}

fn play_sound<L: LoaderRef>(vm: &mut Vm<L>, this: Option<ObjectId>, args: &[Value]) {
    let Some(&Value::Object(Some(sound))) = args.first() else { return };
    let volume = (if given(args, 2) { float_arg(args, 2) } else { actor_float(vm, this, Prop::TransientSoundVolume) } * vm.sound_volume()).clamp(0.0, 1.0);
    let radius = match if given(args, 4) { float_arg(args, 4) } else { actor_float(vm, this, Prop::TransientSoundRadius) } {
        0.0 => AUDIO_DEFAULT_RADIUS,
        radius => radius,
    };
    let dilation = match vm.level().and_then(|level| vm.store.get(level, Prop::TimeDilation)) {
        Some(Value::Float(dilation)) => dilation,
        _ => 1.0,
    };
    let pitch = optional_float(args, 5, 1.0) * dilation;
    let slot = if given(args, 1) { int_arg(args, 1) as u8 } else { 1 };
    let attenuate = !given(args, 6) || bool_arg(args, 6);
    let played = PlayedSound { actor: this, sound: vm.store.object(sound).path.clone(), slot, volume, radius, pitch, attenuate, no_override: given(args, 3) && bool_arg(args, 3), flashbang: None };
    vm.subtitle_insert_sound(played.actor, &played.sound);
    vm.host.commands.push(HostCommand::Sound(played));
}

fn play_music<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>, args: &[Value]) -> Value {
    let attenuate = native == Native::PlayMusicAttenuate;
    let override_volume = float_arg(args, if attenuate { 5 } else { 2 });
    let music_volume = if override_volume > 0.0 { override_volume } else { vm.music_volume() };
    let volume = if attenuate { (if given(args, 2) { float_arg(args, 2) } else { actor_float(vm, this, Prop::TransientSoundVolume) }) * music_volume } else { music_volume };
    let volume = volume.clamp(0.0, 1.0);
    if volume == 0.0 {
        return Value::Int(0);
    }
    vm.host.music_handle += 1;
    let radius = if attenuate { if given(args, 3) { float_arg(args, 3) } else { actor_float(vm, this, Prop::TransientSoundRadius) } } else { 0.0 };
    let song = match args.first() {
        Some(Value::Str(song)) => song.clone(),
        _ => String::new(),
    };
    let play = MusicCommand::Play {
        handle: vm.host.music_handle,
        song,
        actor: this.filter(|_| attenuate),
        fade_in: float_arg(args, 1),
        volume,
        radius: if attenuate && radius <= 0.0 { 80.0 } else { radius },
        pitch: if attenuate && given(args, 4) { float_arg(args, 4) } else { 1.0 },
    };
    vm.host.commands.push(HostCommand::Music(play));
    Value::Int(vm.host.music_handle)
}

fn actor_float<L: LoaderRef>(vm: &Vm<L>, this: Option<ObjectId>, property: Prop) -> f32 {
    this.map_or(0.0, |this| vm.store.float_of(this, property))
}
