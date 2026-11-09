use super::*;

impl Simulation {
    fn zone_sounds(&mut self, listener: Vector) -> Option<Vec<ZoneSound>> {
        let vm = &mut self.vm;
        let mut sources: Vec<ObjectId> = vm.zone_at(listener).into_iter().collect();
        sources.extend(self.volumes.iter().copied().filter(|&volume| matches!(vm.store.get(volume, Prop::ZoneSounds), Some(p2_vm::Value::Array(ref entries)) if !entries.is_empty()) && vm.encompasses(volume, listener)));
        let (sound_volume, music_volume) = (vm.sound_volume(), vm.music_volume());
        let mut heard: Vec<(ZoneSound, Option<ObjectId>)> = Vec::new();
        for source in sources {
            let Some(p2_vm::Value::Array(entries)) = vm.store.get(source, Prop::ZoneSounds) else { continue };
            let pitch = match vm.store.get(source, Prop::SoundPitch) {
                Some(p2_vm::Value::Byte(pitch)) => pitch_factor(f32::from(pitch) / 64.0),
                _ => 1.0,
            };
            for (index, entry) in entries.iter().enumerate() {
                let p2_vm::Value::Struct(fields) = entry else { continue };
                let sound = match fields.first() {
                    Some(p2_vm::Value::Object(Some(sound))) => Some(*sound),
                    _ => None,
                };
                let song_name = match fields.get(1) {
                    Some(p2_vm::Value::Str(song)) if sound.is_none() && !song.is_empty() => Some(song),
                    _ => None,
                };
                let entry_volume = match fields.get(2) {
                    Some(p2_vm::Value::Float(volume)) => *volume,
                    _ => 0.0,
                };
                let song = song_name.and_then(|name| match self.zone_songs.get(name) {
                    Some(song) => song.clone(),
                    None => {
                        let song = read_song(&self.music_dir, name);
                        self.zone_songs.insert(name.clone(), song.clone());
                        song
                    }
                });
                if sound.is_none() && song.is_none() {
                    continue;
                }
                let volume = if song.is_some() { music_volume } else { entry_volume * sound_volume };
                let float = |n: usize| match fields.get(n) {
                    Some(p2_vm::Value::Float(value)) => *value,
                    _ => 0.0,
                };
                heard.push((ZoneSound { key: (source, index), sound: None, song, volume: volume.clamp(0.0, 1.0), pitch, fade_in: float(3), fade_out: float(4) }, sound));
            }
        }
        if heard.iter().map(|(zone_sound, _)| zone_sound.key).eq(self.zone_keys.iter().copied()) {
            return None;
        }
        self.zone_keys.clear();
        self.zone_keys.extend(heard.iter().map(|(zone_sound, _)| zone_sound.key));
        let vm = &self.vm;
        Some(heard.into_iter().map(|(zone_sound, sound)| ZoneSound { sound: sound.map(|sound| vm.store.object(sound).path.clone()), ..zone_sound }).collect())
    }

    fn send_sound_files(&mut self, frame: &mut VmFrame, ambient_paths: &[Arc<str>]) {
        let mut named: Vec<String> = Vec::new();
        let paths = frame
            .sounds
            .iter()
            .map(|(played, _)| played.sound.as_str())
            .chain(ambient_paths.iter().map(|path| &**path))
            .chain(frame.zone_sounds.iter().flatten().filter_map(|zone_sound| zone_sound.sound.as_deref()));
        for path in paths {
            if !self.sound_paths.contains(path) {
                self.sound_paths.insert(path.to_owned());
                named.push(path.to_owned());
            }
        }
        for path in named {
            let file = self.vm.sound_file(&path);
            frame.sound_files.push((path, file));
        }
    }

    pub(super) fn read_audio(&mut self, frame: &mut VmFrame) {
        let (ambient_volume, sound_volume) = (self.vm.ambient_volume(), self.vm.sound_volume());
        let mut new_paths: Vec<Arc<str>> = Vec::new();
        let (vm, ambient_paths) = (&self.vm, &mut self.ambient_paths);
        for actor in vm.actors() {
            let Some(ambient) = crate::actors::ambient_actor(vm, actor) else { continue };
            let path = ambient_paths.entry(ambient.sound).or_insert_with(|| {
                let path: Arc<str> = Arc::from(vm.store.object(ambient.sound).path.as_str());
                new_paths.push(Arc::clone(&path));
                path
            });
            let path = Arc::clone(path);
            let volume = ambient_gain(ambient.volume, ambient_volume, sound_volume);
            frame.ambients.push((actor, crate::actors::AmbientActor { volume, ..ambient }, path));
        }
        frame.zone_sounds = self.view.and_then(|view| self.zone_sounds(view.location));
        self.send_sound_files(frame, &new_paths);
        frame.audio_channels = self.vm.audio_channels();
    }
}

pub(super) fn read_song(music_dir: &Path, song: &str) -> Option<Arc<[u8]>> {
    if song.is_empty() {
        return None;
    }
    let file = if Path::new(song).extension().is_some() { song.to_owned() } else { format!("{song}.ogg") };
    match std::fs::read(music_dir.join(&file)) {
        Ok(bytes) => {
            let bytes: Arc<[u8]> = bytes.into();
            let opened = opens(&bytes);
            if !opened {
                bevy::log::warn!("music {song}: the audio decoder cannot open it; not played");
            }
            opened.then_some(bytes)
        }
        Err(_) => {
            bevy::log::warn!("music {song} not found");
            None
        }
    }
}
