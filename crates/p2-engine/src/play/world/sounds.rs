use super::*;

impl PlayWorld {
    pub(in crate::play) fn scene_position(&self, location: Vector) -> Vec3 {
        Vec3::from(to_bevy(location)) + self.origin
    }
}

/// `None` (warned once, the result kept per path) when Bevy's decoder cannot open its file: `AudioSource::decoder`
/// unwraps the decoder, so such a sound would panic the frame it plays.
pub(in crate::play) fn loaded_sound(path: &str, sound: p2_assets::Sound, sources: &mut Assets<AudioSource>) -> Option<LoadedSound> {
    let (duration, looping) = (sound.duration().unwrap_or(0.0), sound.loops());
    let bytes: Arc<[u8]> = sound.data.into();
    if !opens(&bytes) {
        bevy::log::warn!("sound {path}: {} file the audio decoder cannot open; not played", sound.file_type);
        return None;
    }
    Some(LoadedSound { source: sources.add(AudioSource { bytes }), duration, looping })
}

pub(in crate::play) fn opens(bytes: &Arc<[u8]>) -> bool {
    decodable(bytes) && rodio::Decoder::builder().with_byte_len(bytes.len() as u64).with_data(std::io::Cursor::new(Arc::clone(bytes))).build().is_ok()
}

/// Whether rodio (`hound` for WAV, `lewton` for Ogg Vorbis) opens `data` without panicking: an Ogg stream, or a RIFF WAVE
/// whose `fmt ` chunk (before its `data` chunk) has format PCM (1), IEEE float (3) or extensible (0xFFFE), at least one
/// channel and a sample rate. Others fail `hound::WavReader::new`; no channels or rate fail `WavDecoder::new`'s asserts.
fn decodable(data: &[u8]) -> bool {
    if data.starts_with(b"OggS") {
        return true;
    }
    let mut format_ok = false;
    for (id, body) in p2_assets::riff_chunks(data) {
        match &id {
            b"fmt " => {
                let Some(fmt) = data.get(body.start..body.start + 8) else { return false };
                let (tag, channels) = (u16::from_le_bytes([fmt[0], fmt[1]]), u16::from_le_bytes([fmt[2], fmt[3]]));
                let rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
                format_ok = matches!(tag, 1 | 3 | 0xFFFE) && channels > 0 && rate > 0;
            }
            b"data" => return format_ok,
            _ => {}
        }
    }
    false
}

#[derive(Clone)]
pub(in crate::play) struct LoadedSound {
    pub(in crate::play) source: Handle<AudioSource>,
    pub(in crate::play) duration: f32,
    pub(in crate::play) looping: bool,
}

#[cfg(test)]
mod tests {
    use super::{decodable, opens};

    fn wav(tag: u16, channels: u16, rate: u32) -> Vec<u8> {
        let mut body = b"WAVEfmt ".to_vec();
        body.extend(16u32.to_le_bytes());
        body.extend(tag.to_le_bytes());
        body.extend(channels.to_le_bytes());
        body.extend(rate.to_le_bytes());
        body.extend([0; 8]);
        body.extend(b"data");
        body.extend(0u32.to_le_bytes());
        let mut file = b"RIFF".to_vec();
        file.extend((body.len() as u32).to_le_bytes());
        file.extend(body);
        file
    }

    #[test]
    fn only_files_the_decoder_opens_are_decodable() {
        assert!(decodable(&wav(1, 1, 22050)));
        assert!(decodable(&wav(3, 2, 44100)));
        assert!(decodable(b"OggS and the rest of the stream"));
        assert!(!decodable(&wav(0x11, 1, 22050)));
        assert!(!decodable(&wav(0x55, 1, 22050)));
        assert!(!decodable(&wav(1, 0, 22050)));
        assert!(!decodable(&wav(1, 1, 0)));
        assert!(!decodable(b"ID3 an mp3 file"));
    }

    #[test]
    fn a_broken_song_does_not_open() {
        assert!(!opens(&std::sync::Arc::from(&b"OggS and the rest of the stream"[..])));
    }
}
