use p2_math::{Rng, Vector};
use super::{FluidSettings, FluidToucher, FluidSurface, ROW, MAX_CATCH_UP_STEPS, MAX_WARM_UP_SECONDS, MAX_NOISE_PER_STEP, VISCOSITY, MAX_SMOOTHING, MAX_WAVE_NUMBER, MAX_HEIGHT, bit_set, ring_difference};

impl FluidSurface {
    pub fn new(mut settings: FluidSettings, location: Vector, clamp_bitmap: Vec<i32>, seed: u32) -> Self {
        settings.x_size = settings.x_size.clamp(0, 256);
        settings.y_size = settings.y_size.clamp(0, 256);
        let half_x = (settings.x_size - 1) as f32 * 0.5 * settings.spacing;
        let mut half_y = (settings.y_size - 1) as f32 * 0.5 * settings.spacing;
        if settings.hexagonal {
            half_y *= ROW;
        }
        let count = (settings.x_size * settings.y_size) as usize;
        let height_limit = MAX_HEIGHT * settings.spacing / settings.height_scale.abs();
        Self {
            origin: Vector::new(location.x - half_x, location.y - half_y, location.z),
            verts: [vec![0.0; count], vec![0.0; count]],
            scratch: vec![0.0; count],
            latest: 1,
            alpha: vec![0; count],
            clamped: clamp_bitmap,
            rollover: 0.0,
            noise_due: 0.0,
            height_limit: if height_limit.is_finite() && height_limit > 0.0 { height_limit } else { f32::MAX },
            warmed_up: false,
            rng: Rng::new(seed),
            oscillators: Vec::new(),
            settings,
        }
    }

    pub fn height(&self, x: i32, y: i32) -> f32 {
        self.verts[self.latest][self.index(x, y)]
    }

    pub(super) fn index(&self, x: i32, y: i32) -> usize {
        (y * self.settings.x_size + x) as usize
    }

    pub(super) fn is_clamped(&self, i: usize) -> bool {
        bit_set(&self.clamped, i)
    }

    /// The first tick simulates `WarmUpTime`; later steps run only while the surface was drawn within the
    /// last second. `UpdateRate` not above 0 does nothing: those steps would never reach their end.
    /// Unlike retail, a long frame drops the steps past the cap instead of running them all at once.
    pub fn tick(&mut self, delta: f32, drawn_lately: bool, touchers: &[FluidToucher]) {
        if self.settings.update_rate.is_nan() || self.settings.update_rate <= 0.0 {
            return;
        }
        let step = 1.0 / self.settings.update_rate;
        if !self.warmed_up {
            let mut time = 0.0;
            while time < self.settings.warm_up_time.min(MAX_WARM_UP_SECONDS) {
                self.update(step, touchers);
                time += step;
            }
            self.warmed_up = true;
        }
        if drawn_lately {
            self.rollover += delta;
            let mut steps = 0;
            while step < self.rollover && steps < MAX_CATCH_UP_STEPS {
                self.update(step, touchers);
                self.rollover -= step;
                steps += 1;
            }
            self.rollover = self.rollover.min(step);
        }
    }

    pub fn update(&mut self, step: f32, touchers: &[FluidToucher]) {
        for toucher in touchers {
            let speed = (toucher.velocity.x * toucher.velocity.x + toucher.velocity.y * toucher.velocity.y).sqrt();
            self.pling(toucher.location, self.settings.ripple_velocity_factor * speed, toucher.radius);
        }
        let (nx, ny) = (self.settings.x_size, self.settings.y_size);
        if nx > 2 && ny > 2 {
            self.noise_due += self.settings.noise_frequency * step;
            let due = self.noise_due.floor();
            self.noise_due -= due;
            for _ in 0..(due as i32).min(MAX_NOISE_PER_STEP) {
                let x = self.rng.rand() % (nx - 2) + 1;
                let y = self.rng.rand() % (ny - 2) + 1;
                if !self.is_clamped(self.index(x, y)) {
                    let (min, max) = self.settings.noise_strength;
                    let strength = min + (max - min) * self.rng.frand();
                    let (vx, vy) = self.vertex_xy(x, y);
                    self.pling(Vector::new(vx, vy, 0.0), strength, 0.0);
                }
            }
        }
        let mut oscillators = std::mem::take(&mut self.oscillators);
        for oscillator in &mut oscillators {
            let strength = oscillator.oscillate(step);
            self.pling(oscillator.location, strength, oscillator.radius);
        }
        self.oscillators = oscillators;
        self.wave_step(step);
        self.smooth((VISCOSITY * step).min(MAX_SMOOTHING));
    }

    /// The wave step, into the older buffer. Edge vertices never change here.
    pub(super) fn wave_step(&mut self, step: f32) {
        let s = &self.settings;
        let (nx, ny) = (s.x_size, s.y_size);
        let damp = 1.0 - s.damping * step;
        let speed = (s.speed * step * s.speed * step).min(MAX_WAVE_NUMBER * s.spacing * s.spacing);
        let inv = 1.0 / (s.spacing * s.spacing);
        let limit = self.height_limit;
        let (src, dst) = (self.latest, 1 - self.latest);
        self.latest = dst;
        let [a, b] = &mut self.verts;
        let (cur, out) = if src == 0 { (&*a, b) } else { (&*b, a) };
        let width = nx as usize;
        let row = |y: i32| &cur[y as usize * width..(y + 1) as usize * width];
        for y in 1..ny - 1 {
            let rows = [row(y - 1), row(y), row(y + 1)];
            let odd = y & 1 != 0;
            for x in 1..width.saturating_sub(1) {
                let i = y as usize * width + x;
                if !self.clamped.is_empty() && bit_set(&self.clamped, i) {
                    continue;
                }
                let lap = ring_difference(s.hexagonal, odd, rows, x) * inv;
                out[i] = (((rows[1][x] * 2.0 + lap * speed) - out[i]) * damp).clamp(-limit, limit);
                let alpha = (s.alpha_height_scale * out[i] + s.alpha_curve_scale * lap).round_ties_even() as i32;
                self.alpha[i] = alpha.clamp(0, i32::from(s.alpha_max)) as u8;
            }
        }
    }

    /// Retail has no such step. Filtering only the new heights damps sharp ripples most, which hides single-vertex wiggles.
    fn smooth(&mut self, amount: f32) {
        let s = &self.settings;
        let (ny, width) = (s.y_size, s.x_size as usize);
        let out = &mut self.verts[self.latest];
        self.scratch.copy_from_slice(out);
        let row = |y: i32| &self.scratch[y as usize * width..(y + 1) as usize * width];
        for y in 1..ny - 1 {
            let rows = [row(y - 1), row(y), row(y + 1)];
            let odd = y & 1 != 0;
            for x in 1..width.saturating_sub(1) {
                let i = y as usize * width + x;
                if !self.clamped.is_empty() && bit_set(&self.clamped, i) {
                    continue;
                }
                out[i] = rows[1][x] + amount * ring_difference(s.hexagonal, odd, rows, x);
            }
        }
    }
}
