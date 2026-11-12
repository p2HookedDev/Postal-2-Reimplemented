use super::*;

/// Tick periods the shown game time is steered to stand behind the newest tick: longer lets a tick come later without
/// the motion stopping, but shows the world later.
const TICK_DELAY: f64 = 1.25;
/// How much faster or slower than real time the shown game time may run while it is steered: more finds its place
/// sooner, less keeps the shown speed steadier.
const STEER_LIMIT: f64 = 0.05;
/// Seconds over which the shown game time closes its distance from its place: shorter follows the ticks closer, but
/// lets their uneven arrival show in the speed.
const STEER_TIME: f64 = 0.25;
/// Tick periods behind its place at which the shown game time jumps there instead of catching up (after a pause, a
/// load, a hitch): lower jumps more often, higher catches up for longer.
const SNAP_TICKS: f64 = 2.0;

/// Where a frame stands between the three places each glide keeps: what was shown when the newest tick came, the tick
/// before the newest, and the newest.
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

/// The game time frames show, in seconds: it runs with real time a little behind the newest tick, so ticks that come
/// unevenly still show as even motion.
pub(crate) struct TickClock {
    shown: f64,
    /// The game time shown when the newest tick came.
    from: f64,
    mid: f64,
    newest: f64,
    steps: StepClock,
    period: f64,
}

impl Default for TickClock {
    fn default() -> Self {
        // Far behind, so the first tick snaps to it.
        Self { shown: f64::NEG_INFINITY, from: 0.0, mid: 0.0, newest: 0.0, steps: StepClock::default(), period: 0.0 }
    }
}

impl TickClock {
    /// A tick `step` game seconds after the one before.
    pub(in crate::play) fn tick(&mut self, step: f32) {
        self.period = f64::from(self.steps.step(step));
        self.mid = self.newest;
        self.newest += f64::from(step);
        if self.newest - TICK_DELAY * self.period - self.shown > SNAP_TICKS * self.period {
            // Steered, the clock stands half a tick further back than its place when a tick comes.
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

    /// Game ticks the shown time stands behind the newest tick, for what keeps only its last step (particle sprites).
    pub(crate) fn behind(&self) -> f32 {
        if self.newest > self.mid { ((self.newest - self.shown) / (self.newest - self.mid)).clamp(0.0, SNAP_TICKS) as f32 } else { 0.0 }
    }

    pub(in crate::play) fn glide(&self) -> Glide {
        let part = |start: f64, end: f64| if end > start { ((self.shown - start) / (end - start)).clamp(0.0, 1.0) as f32 } else { 1.0 };
        Glide { to_mid: part(self.from, self.mid), to_newest: part(self.mid, self.newest) }
    }
}
