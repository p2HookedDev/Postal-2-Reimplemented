use std::ops::Range;

use p2_package::{Export, Package};

use crate::Error;
use crate::read::{invalid, lazy_bytes, lazy_span};

#[derive(Debug, Clone)]
pub struct Sound {
    pub file_type: String,
    pub data: Vec<u8>,
}

impl Sound {
    /// Length in seconds of a RIFF WAVE file: `data` chunk size / `fmt ` byte rate; `None` otherwise.
    /// Assumed to match the engine's duration (unverified).
    pub fn duration(&self) -> Option<f32> {
        wave_duration(&self.data)
    }

    /// Whether a RIFF WAVE file loops: a `smpl` chunk with at least one sample loop (`cSampleLoops`,
    /// 28 bytes into the chunk). Retail plays such a sound looping.
    pub fn loops(&self) -> bool {
        let data = &self.data;
        if data.get(0..4) != Some(b"RIFF") || data.get(8..12) != Some(b"WAVE") {
            return false;
        }
        let mut at = 12;
        while let Some(header) = data.get(at..at + 8) {
            let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
            if &header[..4] == b"smpl" {
                return data.get(at + 36..at + 40).is_some_and(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) > 0);
            }
            at += 8 + length + (length & 1);
        }
        false
    }
}

fn wave_duration(data: &[u8]) -> Option<f32> {
    wave_duration_by(data.len(), |at, buf| {
        if let Some(bytes) = data.get(at..at + buf.len()) {
            buf.copy_from_slice(bytes);
        }
    })
}

pub fn wave_duration_by(file_len: usize, mut read: impl FnMut(usize, &mut [u8])) -> Option<f32> {
    let mut get = |at: usize, buf: &mut [u8]| (at + buf.len() <= file_len).then(|| read(at, buf));
    let mut riff = [0u8; 12];
    get(0, &mut riff)?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return None;
    }
    let (mut byte_rate, mut size, mut at) = (None, None, 12);
    let mut header = [0u8; 8];
    while get(at, &mut header).is_some() {
        let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        match &header[..4] {
            b"fmt " => {
                let mut rate = [0u8; 4];
                byte_rate = get(at + 16, &mut rate).map(|()| u32::from_le_bytes(rate));
            }
            b"data" => size = Some(length),
            _ => {}
        }
        at += 8 + length + (length & 1);
    }
    let byte_rate = byte_rate.filter(|&rate| rate > 0)?;
    Some(size? as f32 / byte_rate as f32)
}

pub fn read_sound_duration(pkg: &Package, file: &[u8], export: &Export) -> Result<Option<f32>, Error> {
    let mut r = pkg.native_reader(file, export)?;
    pkg.read_name(&mut r)?;
    Ok(wave_duration(lazy_bytes(&mut r)?))
}

pub fn sound_file_span(pkg: &Package, file: &[u8], export: &Export) -> Result<Range<usize>, Error> {
    let mut r = pkg.native_reader(file, export)?;
    pkg.read_name(&mut r)?;
    lazy_span(&mut r)
}

pub fn read_sound(pkg: &Package, file: &[u8], export: &Export) -> Result<Sound, Error> {
    let mut r = pkg.native_reader(file, export)?;
    let file_type = pkg.name(pkg.read_name(&mut r)?).to_owned();
    let data = lazy_bytes(&mut r)?.to_vec();
    if !r.at_end() {
        return Err(invalid("sound end", r.pos()));
    }
    Ok(Sound { file_type, data })
}

#[cfg(test)]
mod tests {
    use super::{Sound, wave_duration_by};

    fn wav(chunks: &[(&[u8; 4], Vec<u8>)]) -> Sound {
        let mut body = b"WAVE".to_vec();
        for (id, bytes) in chunks {
            body.extend_from_slice(*id);
            body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            body.extend_from_slice(bytes);
        }
        let mut data = b"RIFF".to_vec();
        data.extend_from_slice(&(body.len() as u32).to_le_bytes());
        data.extend_from_slice(&body);
        Sound { file_type: "WAV".to_owned(), data }
    }

    #[test]
    fn a_sample_loop_makes_a_wav_loop() {
        let smpl = |loops: u32| {
            let mut bytes = vec![0; 36];
            bytes[28..32].copy_from_slice(&loops.to_le_bytes());
            bytes
        };
        assert!(wav(&[(b"data", vec![0; 4]), (b"smpl", smpl(1))]).loops());
        assert!(!wav(&[(b"data", vec![0; 4]), (b"smpl", smpl(0))]).loops());
        assert!(!wav(&[(b"data", vec![0; 4])]).loops());
    }

    #[test]
    fn a_wav_duration_reads_only_chunk_headers() {
        let mut fmt = vec![0; 16];
        fmt[8..12].copy_from_slice(&1000u32.to_le_bytes());
        let sound = wav(&[(b"fmt ", fmt), (b"data", vec![7; 500]), (b"smpl", vec![0; 36])]);
        let samples = 44..544;
        let mut touched = false;
        let duration = wave_duration_by(sound.data.len(), |at, buf| {
            touched |= at < samples.end && samples.start < at + buf.len();
            buf.copy_from_slice(&sound.data[at..at + buf.len()]);
        });
        assert_eq!(duration, Some(0.5));
        assert_eq!(duration, sound.duration());
        assert!(!touched);
        assert_eq!(wave_duration_by(10, |_, _| {}), None);
    }
}
