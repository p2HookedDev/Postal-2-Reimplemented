//! Panned as retail's software mixer pans: equal power, √(1 - p) left and √p right with p = (x + 1) / 2.
//! Sounds of more than one channel are not panned, as OpenAL leaves them.

use std::num::NonZero;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bevy::asset::Asset;
use bevy::audio::{AudioSource, Decodable};
use bevy::reflect::TypePath;
use rodio::Source;

const CENTRE: f32 = 0.5;

#[derive(Clone)]
pub(crate) struct Pan(Arc<AtomicU32>);

impl Default for Pan {
    fn default() -> Self {
        Self(Arc::new(AtomicU32::new(CENTRE.to_bits())))
    }
}

impl Pan {
    /// `side` is the sound's direction along the listener's right, -1 to 1.
    pub(crate) fn set(&self, side: f32) {
        self.0.store(((side.clamp(-1.0, 1.0) + 1.0) * 0.5).to_bits(), Ordering::Relaxed);
    }

    fn gains(&self) -> [f32; 2] {
        let right = f32::from_bits(self.0.load(Ordering::Relaxed));
        [(1.0 - right).sqrt(), right.sqrt()]
    }
}

#[derive(Asset, TypePath)]
pub(crate) struct PannedSound {
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) pan: Pan,
}

impl Decodable for PannedSound {
    type Decoder = Panned<<AudioSource as Decodable>::Decoder>;

    fn decoder(&self) -> Self::Decoder {
        let inner = AudioSource { bytes: Arc::clone(&self.bytes) }.decoder();
        Panned { mono: inner.channels().get() == 1, inner, pan: self.pan.clone(), right: None }
    }
}

pub(crate) struct Panned<S> {
    inner: S,
    pan: Pan,
    mono: bool,
    right: Option<f32>,
}

impl<S: Source> Iterator for Panned<S> {
    type Item = rodio::Sample;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.mono {
            return self.inner.next();
        }
        if let Some(right) = self.right.take() {
            return Some(right);
        }
        let sample = self.inner.next()?;
        let [left, right] = self.pan.gains();
        self.right = Some(sample * right);
        Some(sample * left)
    }
}

impl<S: Source> Source for Panned<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len().map(|samples| if self.mono { samples * 2 } else { samples })
    }

    fn channels(&self) -> rodio::ChannelCount {
        if self.mono { NonZero::<u16>::MIN.saturating_add(1) } else { self.inner.channels() }
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), rodio::source::SeekError> {
        self.right = None;
        self.inner.try_seek(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sound_ahead_is_even_and_one_to_a_side_is_heard_on_that_side_only() {
        let pan = Pan::default();
        let [left, right] = pan.gains();
        assert!((left - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6 && (right - left).abs() < 1e-6);
        pan.set(1.0);
        assert_eq!(pan.gains(), [0.0, 1.0]);
        pan.set(-0.5);
        let [left, right] = pan.gains();
        assert!((left * left + right * right - 1.0).abs() < 1e-5 && left > right);
    }

    fn mono_wave(samples: &[i16]) -> Arc<[u8]> {
        let data: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        let mut wave = Vec::new();
        wave.extend_from_slice(b"RIFF");
        wave.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wave.extend_from_slice(b"WAVEfmt ");
        for field in [16u32, 1 | (1 << 16), 22050, 44100, 2 | (16 << 16)] {
            wave.extend_from_slice(&field.to_le_bytes());
        }
        wave.extend_from_slice(b"data");
        wave.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wave.extend_from_slice(&data);
        wave.into()
    }

    #[test]
    fn a_mono_sound_comes_out_in_two_channels_by_its_pan() {
        let pan = Pan::default();
        pan.set(1.0);
        let sound = PannedSound { bytes: mono_wave(&[16384, -16384]), pan };
        let decoder = sound.decoder();
        assert_eq!(decoder.channels().get(), 2);
        let samples: Vec<f32> = decoder.collect();
        assert_eq!(samples.len(), 4);
        assert_eq!([samples[0], samples[2]], [0.0, 0.0]);
        assert!((samples[1] - 0.5).abs() < 1e-3 && (samples[3] + 0.5).abs() < 1e-3);
    }
}
