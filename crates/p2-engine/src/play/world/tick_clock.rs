use super::*;

/// Tick periods the shown game time is steered to stand behind the newest tick: longer lets a tick come later
/// without the motion stopping, but shows the world later.
const TICK_DELAY: f64 = 1.25;
const STEER_LIMIT: f64 = 0.05;
const STEER_TIME: f64 = 0.25;
const SNAP_TICKS: f64 = 2.0;

#[derive(Clone, Copy)]
pub(in crate::play) struct Glide {
    pub(super) to_mid: f32,
    pub(super) to_newest: f32,
}

impl Glide {
    pub(in crate::play) fn pick<T, U>(self, from: T, mid: T, to: T, mix: impl Fn(T, T, f32) -> U) -> U {
        if self.to_newest > 0.0 { mix(mid, to, self.to_newest) } else { mix(from, mid, self.to_mid) }
    }
}

pub(crate) struct TickClock {
    shown: f64,
    from: f64,
    mid: f64,
    newest: f64,
    steps: StepClock,
    period: f64,
}

impl Default for TickClock {
    fn default() -> Self {
        Self { shown: f64::NEG_INFINITY, from: 0.0, mid: 0.0, newest: 0.0, steps: StepClock::default(), period: 0.0 }
    }
}

impl TickClock {
    pub(in crate::play) fn tick(&mut self, step: f32) {
        self.period = f64::from(self.steps.step(step));
        self.mid = self.newest;
        self.newest += f64::from(step);
        if self.newest - TICK_DELAY * self.period - self.shown > SNAP_TICKS * self.period {
            self.shown = self.newest - (TICK_DELAY + 0.5) * self.period;
        }
        self.from = self.shown.min(self.mid);
    }

    pub(in crate::play) fn frame(&mut self, real_delta: f32) -> Glide {
        let behind = self.newest - TICK_DELAY * self.period - self.shown;
        let speed = 1.0 + (behind / STEER_TIME).clamp(-STEER_LIMIT, STEER_LIMIT);
        self.shown = (self.shown + f64::from(real_delta) * speed).min(self.newest);
        self.glide()
    }

    pub(crate) fn behind(&self) -> f32 {
        if self.newest > self.mid { ((self.newest - self.shown) / (self.newest - self.mid)).clamp(0.0, SNAP_TICKS) as f32 } else { 0.0 }
    }

    pub(in crate::play) fn glide(&self) -> Glide {
        let part = |start: f64, end: f64| if end > start { ((self.shown - start) / (end - start)).clamp(0.0, 1.0) as f32 } else { 1.0 };
        Glide { to_mid: part(self.from, self.mid), to_newest: part(self.mid, self.newest) }
    }
}
