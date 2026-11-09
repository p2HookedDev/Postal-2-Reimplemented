use p2_package::PackageId;
use crate::interp::int;
use crate::natives::AUDIO_DEFAULT_RADIUS;
use crate::{Error, HostCommand, LoaderRef, NameId, ObjectId, PlayedSound, Value, Vm, Prop};

impl<L: LoaderRef> Vm<L> {
    pub(super) fn next_notify(&mut self, actor: ObjectId, animation: usize, sequence: NameId, old: f32, new: f32) -> Option<(f32, Option<ObjectId>)> {
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
    pub(super) fn run_notify(&mut self, owner: ObjectId, notify: ObjectId) -> Result<(), Error> {
        let name_of = |vm: &Self, property: Prop| match vm.store.get(notify, property) {
            Some(Value::Name(name)) => name,
            _ => NameId::NONE,
        };
        let names = &self.script.lookups.class_names;
        let (scripted, script, sound, destroy_effect, effect) =
            (names.anim_notify_scripted, names.anim_notify_script, names.anim_notify_sound, names.anim_notify_destroy_effect, names.anim_notify_effect);
        if self.store.is_a(notify, scripted) {
            return self.event(notify, crate::states::Event::Notify, [Value::Object(Some(owner))]);
        }
        if self.store.is_a(notify, script) {
            let function = name_of(self, Prop::NotifyName);
            if function != NameId::NONE {
                let function = self.store.name(function).to_owned();
                self.event(owner, function.as_str(), Vec::new())?;
            }
            return Ok(());
        }
        if self.store.is_a(notify, sound) {
            if let Some(sound) = self.store.object_of(notify, Prop::Sound) {
                let radius = match self.store.get(notify, Prop::Radius).as_ref().map_or(0, int) {
                    0 => AUDIO_DEFAULT_RADIUS,
                    radius => radius as f32,
                };
                let volume = (self.store.float_of(notify, Prop::Volume) * self.sound_volume()).clamp(0.0, 1.0);
                let sound = self.store.object(sound).path.clone();
                self.subtitle_insert_sound(Some(owner), &sound);
                self.host.commands.push(HostCommand::Sound(PlayedSound { actor: Some(owner), sound, slot: 0, volume, radius, pitch: 1.0, attenuate: true, no_override: false, flashbang: None }));
            }
            return Ok(());
        }
        if self.store.is_a(notify, destroy_effect) {
            let tag = name_of(self, Prop::DestroyTag);
            if tag == NameId::NONE {
                return Ok(());
            }
            let owned: Vec<ObjectId> = self
                .actors()
                .filter(|&a| !self.store.is_deleted(a) && self.store.object_of(a, Prop::Owner) == Some(owner) && self.store.get(a, Prop::Tag) == Some(Value::Name(tag)))
                .collect();
            for actor in owned.into_iter().rev() {
                self.destroy(actor)?;
            }
            return Ok(());
        }
        if self.store.is_a(notify, effect) {
            let Some(class) = self.store.object_of(notify, Prop::EffectClass) else { return Ok(()) };
            let Some(class) = self.class_of_object(class)? else { return Ok(()) };
            let (location, rotation) = (self.store.vector(owner, Prop::Location), self.store.rotator_of(owner, Prop::Rotation));
            let (offset, turn) = (self.store.vector(notify, Prop::OffsetLocation), self.store.rotator_of(notify, Prop::OffsetRotation));
            let bone = name_of(self, Prop::Bone);
            let attach = bone != NameId::NONE && self.store.flag_of(notify, Prop::Attach);
            let (at, facing) = if attach {
                (location, rotation)
            } else {
                ((location + rotation.rotate(offset)), [0, 1, 2].map(|i| rotation[i].wrapping_add(turn[i])).into())
            };
            let Some(effect) = self.spawn(class, Some(owner), [Some(Value::Vector(at)), Some(Value::Rotator(facing))])? else { return Ok(()) };
            let tag = name_of(self, Prop::Tag);
            if tag != NameId::NONE {
                self.store.set(effect, Prop::Tag, Value::Name(tag));
            }
            for property in ["DrawScale", "DrawScale3D"] {
                if let Some(value) = self.store.get(notify, property) {
                    self.store.set(effect, property, value);
                }
            }
            if attach {
                self.call_native("AttachToBone", Some(owner), &mut [Value::Object(Some(effect)), Value::Name(bone)])?;
                self.store.set(effect, Prop::RelativeLocation, Value::Vector(offset));
                self.store.set(effect, Prop::RelativeRotation, Value::Rotator(turn));
            }
        }
        Ok(())
    }
}
