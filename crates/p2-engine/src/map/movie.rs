//! A game pause holds the movie's sound (`pause_movie_sound`) but not the picture's clock.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use bevy::asset::RenderAssetUsages;
use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings};
use bevy::ecs::prelude::{Commands, Entity};
use bevy::image::Image;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use bevy_egui::{EguiContexts, EguiTextureHandle, egui};
use p2_movie::{RoqDecoder, SAMPLE_RATE};
use p2_vm::{MovieState, ObjectId};
use rustc_hash::FxHashMap;

use super::SceneTargets;
use crate::play::MovieSound;

struct Opened {
    bytes: Arc<[u8]>,
    decoder: RoqDecoder,
    sound: Option<Vec<u8>>,
}

struct Shown {
    bytes: Arc<[u8]>,
    decoder: RoqDecoder,
    image: bevy::asset::Handle<Image>,
    frame_rate: f32,
    frames: u32,
    clock: f32,
    sound: Option<Entity>,
}

enum Slot {
    Opening(u32, Task<Option<Opened>>),
    Shown(u32, Box<Shown>),
    Ended(u32),
}

impl Slot {
    fn plays(&self) -> u32 {
        match self {
            Self::Opening(plays, _) | Self::Shown(plays, _) | Self::Ended(plays) => *plays,
        }
    }

    fn stop(&mut self, commands: &mut Commands) {
        if let Self::Shown(_, shown) = self
            && let Some(sound) = shown.sound.take()
        {
            commands.entity(sound).try_despawn();
        }
    }
}

#[derive(Default)]
pub(super) struct Movies {
    playing: FxHashMap<ObjectId, Slot>,
    images: FxHashMap<String, bevy::asset::Handle<Image>>,
    last: Option<Instant>,
}

impl Movies {
    pub(super) fn stop(&mut self, commands: &mut Commands) {
        for slot in self.playing.values_mut() {
            slot.stop(commands);
        }
        self.playing.clear();
        self.images.clear();
        self.last = None;
    }

    pub(super) fn update(
        &mut self,
        root: &Path,
        contexts: &mut EguiContexts,
        t: &mut SceneTargets,
        hud_textures: &mut FxHashMap<String, Option<(egui::TextureId, egui::Vec2)>>,
    ) {
        let now = Instant::now();
        let delta = self.last.map_or(0.0, |last| now.duration_since(last).as_secs_f32());
        self.last = Some(now);
        if self.playing.is_empty() && t.play.world().is_none_or(|world| world.movies().is_empty()) {
            return;
        }
        let movies: Vec<(String, ObjectId, MovieState)> = t.play.world().map(|world| world.movies().to_vec()).unwrap_or_default();
        let state = |movie: ObjectId| movies.iter().find(|(_, m, _)| *m == movie);
        self.playing.retain(|&movie, slot| {
            let keep = state(movie).is_some_and(|(_, _, s)| s.plays == slot.plays() && (s.playing || matches!(slot, Slot::Ended(_))));
            if !keep {
                slot.stop(&mut t.commands);
            }
            keep
        });
        let started: Vec<&(String, ObjectId, MovieState)> = movies.iter().filter(|(_, movie, s)| s.playing && !self.playing.contains_key(movie)).collect();
        for (_, movie, s) in started {
            let (path, use_sound) = (root.join("Movies").join(&s.file), s.use_sound);
            let task = AsyncComputeTaskPool::get().spawn(async move { open(&path, use_sound) });
            self.playing.insert(*movie, Slot::Opening(s.plays, task));
        }
        let mut reports = Vec::new();
        for (&movie, slot) in &mut self.playing {
            let Some((texture, _, s)) = state(movie) else { continue };
            match slot {
                Slot::Opening(plays, task) if task.is_finished() => {
                    let plays = *plays;
                    *slot = match block_on(task) {
                        Some(opened) => {
                            let (width, height) = (opened.decoder.width(), opened.decoder.height());
                            reports.push(crate::play::MovieStatus { movie, width: width as i32, height: height as i32, playing: true });
                            let shown = show(opened, movie, s.looping, texture, contexts, t, hud_textures);
                            self.images.insert(texture.clone(), shown.image.clone());
                            Slot::Shown(plays, Box::new(shown))
                        }
                        None => {
                            bevy::log::warn!("Movie {} not found or not a RoQ file", s.file);
                            reports.push(crate::play::MovieStatus { movie, width: 0, height: 0, playing: false });
                            Slot::Ended(plays)
                        }
                    };
                }
                Slot::Shown(plays, shown) if !s.paused => {
                    if !advance(shown, delta, s.looping, &mut t.assets.images) {
                        reports.push(crate::play::MovieStatus { movie, width: shown.decoder.width() as i32, height: shown.decoder.height() as i32, playing: false });
                        let plays = *plays;
                        slot.stop(&mut t.commands);
                        *slot = Slot::Ended(plays);
                    }
                }
                _ => {}
            }
        }
        if let Some(world) = t.play.world_mut() {
            world.movie_status.extend(reports);
        }
    }
}

fn show(
    opened: Opened,
    movie: ObjectId,
    looping: bool,
    texture: &str,
    contexts: &mut EguiContexts,
    t: &mut SceneTargets,
    hud_textures: &mut FxHashMap<String, Option<(egui::TextureId, egui::Vec2)>>,
) -> Shown {
    let (width, height) = (opened.decoder.width(), opened.decoder.height());
    let size = Extent3d { width, height, depth_or_array_layers: 1 };
    let black = vec![0; width as usize * height as usize * 4];
    let image = t.assets.images.add(Image::new(size, TextureDimension::D2, black, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default()));
    let id = contexts.add_image(EguiTextureHandle::Weak(image.id()));
    hud_textures.insert(texture.to_owned(), Some((id, egui::vec2(width as f32, height as f32))));
    let sound = match (opened.sound, t.assets.audio.as_mut()) {
        (Some(wav), Some(sources)) => {
            let settings = if looping { PlaybackSettings::LOOP } else { PlaybackSettings::DESPAWN };
            Some(t.commands.spawn((AudioPlayer::new(sources.add(AudioSource { bytes: wav.into() })), settings, MovieSound(movie))).id())
        }
        _ => None,
    };
    let frame_rate = f32::from(opened.decoder.frame_rate().max(1));
    Shown { bytes: opened.bytes, decoder: opened.decoder, image, frame_rate, frames: 0, clock: 0.0, sound }
}

fn advance(shown: &mut Shown, delta: f32, looping: bool, images: &mut bevy::asset::Assets<Image>) -> bool {
    shown.clock += delta;
    let due = (shown.clock * shown.frame_rate) as u32 + 1;
    let mut newest = None;
    while shown.frames < due {
        match shown.decoder.next_frame() {
            Ok(Some(frame)) => {
                shown.frames += 1;
                newest = Some(frame.rgba);
            }
            _ if !looping => return false,
            _ => match RoqDecoder::from_shared(Arc::clone(&shown.bytes)) {
                Ok(decoder) => {
                    (shown.decoder, shown.frames, shown.clock) = (decoder, 0, 0.0);
                    break;
                }
                Err(_) => return false,
            },
        }
    }
    if let Some(rgba) = newest
        && let Some(mut image) = images.get_mut(&shown.image)
    {
        image.data = Some(rgba);
    }
    true
}

fn open(path: &Path, use_sound: bool) -> Option<Opened> {
    let bytes: Arc<[u8]> = std::fs::read(path).ok()?.into();
    let decoder = RoqDecoder::from_shared(Arc::clone(&bytes)).ok()?;
    let sound = if use_sound { sound_wav(&bytes) } else { None };
    Some(Opened { bytes, decoder, sound })
}

fn sound_wav(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = RoqDecoder::new(bytes).ok()?;
    let (mut samples, mut channels) = (Vec::new(), 0u16);
    while let Ok(Some((audio, frame_channels))) = decoder.next_sound() {
        if frame_channels != 0 {
            channels = frame_channels;
        }
        samples.extend_from_slice(&audio);
    }
    if channels == 0 || samples.is_empty() {
        return None;
    }
    let data = u32::try_from(samples.len() * 2).ok()?;
    let mut wav = Vec::with_capacity(44 + samples.len() * 2);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * u32::from(channels) * 2).to_le_bytes());
    wav.extend_from_slice(&(channels * 2).to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    Some(wav)
}
