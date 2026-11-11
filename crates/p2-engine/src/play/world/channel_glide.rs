use super::*;

/// Most a loop's wrap may move a channel on in one tick, in steps of the tick before: more blends a restart near the
/// end as if it wrapped, less shows an uneven tick's wrap at once.
const WRAP_STEP_LIMIT: f32 = 2.0;

/// A channel's place in its sequence in one tick, in frames of the sequence.
#[derive(Clone, Copy)]
pub(in crate::play) struct ChannelTime {
    pub(in crate::play) animation: usize,
    pub(in crate::play) sequence: usize,
    pub(in crate::play) time: f32,
    pub(in crate::play) length: f32,
}

impl ChannelTime {
    pub(in crate::play) fn of(channel: &ChannelPose) -> Self {
        let frames = channel.animation.animation.sequences.get(channel.sequence).map_or(1, |sequence| sequence.num_frames.max(1));
        Self { animation: Arc::as_ptr(&channel.animation).addr(), sequence: channel.sequence, time: channel.time, length: frames as f32 }
    }
}

/// What a channel showed when the newest tick came, its place in the tick before the newest, and how far that tick
/// moved it on.
#[derive(Clone, Copy)]
pub(in crate::play) struct ChannelFrom {
    shown: f32,
    mid: ChannelTime,
    step: f32,
}

impl ChannelFrom {
    pub(in crate::play) fn new(before: Option<&Self>, mid: ChannelTime, glide: Glide) -> Self {
        Self { shown: glided_time(before, mid, glide), mid, step: before.and_then(|before| before.advance(mid)).unwrap_or(0.0) }
    }

    /// How far `to` moves the channel on from the tick before, across a loop's wrap too; `None` for a new sequence, a
    /// backward play or a restart. Only the times cross from the scripts, so a wrap is told by its step matching the
    /// step before.
    fn advance(&self, to: ChannelTime) -> Option<f32> {
        let mid = self.mid;
        if (mid.animation, mid.sequence) != (to.animation, to.sequence) {
            return None;
        }
        if to.time >= mid.time {
            return Some(to.time - mid.time);
        }
        let wrapped = to.time + to.length - mid.time;
        (wrapped <= self.step * WRAP_STEP_LIMIT && wrapped < to.length * 0.5).then_some(wrapped)
    }
}

pub(in crate::play) fn glided_time(from: Option<&ChannelFrom>, to: ChannelTime, glide: Glide) -> f32 {
    let Some((from, step)) = from.and_then(|from| Some((from, from.advance(to)?))) else {
        return to.time;
    };
    let mid = from.mid.time;
    // What was shown trails `mid` by less than half the sequence, so far above it means it is still before a wrap.
    let shown = if from.shown > mid + to.length * 0.5 { from.shown - to.length } else { from.shown };
    let time = glide.pick(shown, mid, mid + step, |a, b, t| a + (b - a) * t);
    if shown < 0.0 || to.time < mid { time.rem_euclid(to.length) } else { time }
}
