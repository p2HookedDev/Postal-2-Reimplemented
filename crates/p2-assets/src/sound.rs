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
        riff_chunks(&self.data)
            .find(|(id, _)| id == b"smpl")
            .is_some_and(|(_, body)| self.data.get(body.start + 28..body.start + 32).is_some_and(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) > 0))
    }
}

fn read_slice(data: &[u8]) -> impl FnMut(usize, &mut [u8]) + '_ {
    |at, buf| {
        if let Some(bytes) = data.get(at..at + buf.len()) {
            buf.copy_from_slice(bytes);
        }
    }
}

fn wave_duration(data: &[u8]) -> Option<f32> {
    wave_duration_by(data.len(), read_slice(data))
}

/// The chunks of a RIFF WAVE file: id and body range, walked by each declared size padded to even. A truncated last
/// chunk's range runs past the data. Nothing for a file that is not RIFF WAVE.
pub fn riff_chunks(data: &[u8]) -> impl Iterator<Item = ([u8; 4], Range<usize>)> + '_ {
    riff_chunks_by(data.len(), read_slice(data))
}

/// [`riff_chunks`] of a file of `file_len` bytes read through `read(offset, buffer)`, which only ever gets ranges inside it.
fn riff_chunks_by(file_len: usize, mut read: impl FnMut(usize, &mut [u8])) -> impl Iterator<Item = ([u8; 4], Range<usize>)> {
    let mut get = move |at: usize, buf: &mut [u8]| (at + buf.len() <= file_len).then(|| read(at, buf));
    let mut riff = [0u8; 12];
    let riff_wave = get(0, &mut riff).is_some() && &riff[0..4] == b"RIFF" && &riff[8..12] == b"WAVE";
    let mut at = 12;
    std::iter::from_fn(move || {
        let mut header = [0u8; 8];
        riff_wave.then_some(())?;
        get(at, &mut header)?;
        let [a, b, c, d, length @ ..] = header;
        let length = u32::from_le_bytes(length) as usize;
        let body = at + 8..at + 8 + length;
        at = body.end + (length & 1);
        Some(([a, b, c, d], body))
    })
}

pub fn wave_duration_by(file_len: usize, mut read: impl FnMut(usize, &mut [u8])) -> Option<f32> {
    let (mut fmt, mut size) = (None, None);
    for (id, body) in riff_chunks_by(file_len, &mut read) {
        match &id {
            b"fmt " => fmt = Some(body.start),
            b"data" => size = Some(body.len()),
            _ => {}
        }
    }
    let mut rate = [0u8; 4];
    let byte_rate = fmt.filter(|&at| at + 12 <= file_len).map(|at| {
        read(at + 8, &mut rate);
        u32::from_le_bytes(rate)
    });
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
