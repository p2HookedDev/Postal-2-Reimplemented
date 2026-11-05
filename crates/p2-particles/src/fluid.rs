use p2_math::{Rng, Vector};

const ROW: f32 = 0.866_025;

const MAX_CATCH_UP_STEPS: u32 = 8;
const MAX_WARM_UP_SECONDS: f32 = 10.0;
const MAX_NOISE_PER_STEP: i32 = 64;
const MIN_PLING_RADIUS: f32 = 1.5;
const VISCOSITY: f32 = 0.6;
const MAX_SMOOTHING: f32 = 0.1;
const MAX_WAVE_NUMBER: f32 = 0.4;
/// In grid spacings, drawn height.
const MAX_HEIGHT: f32 = 0.25;

fn bit_set(words: &[i32], i: usize) -> bool {
    words.get(i >> 5).is_some_and(|word| word & (1 << (i & 31)) != 0)
}

/// The sum of a vertex's neighbours less their count times the vertex. Hexagonal: 6 neighbours, odd rows
/// shifted half a spacing right.
#[inline]
fn ring_difference(hexagonal: bool, odd: bool, [below, here, above]: [&[f32]; 3], x: usize) -> f32 {
    let c = here[x];
    if !hexagonal {
        here[x - 1] + here[x + 1] + below[x] + above[x] - c * 4.0
    } else if !odd {
        here[x - 1] + above[x - 1] + above[x] + here[x + 1] + below[x] + below[x - 1] - c * 6.0
    } else {
        here[x - 1] + above[x] + above[x + 1] + here[x + 1] + below[x + 1] + below[x] - c * 6.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FluidSettings {
    pub hexagonal: bool,
    pub spacing: f32,
    pub x_size: i32,
    pub y_size: i32,
    pub height_scale: f32,
    pub speed: f32,
    pub damping: f32,
    pub noise_frequency: f32,
    pub noise_strength: (f32, f32),
    pub u_tiles: f32,
    pub u_offset: f32,
    pub v_tiles: f32,
    pub v_offset: f32,
    pub alpha_curve_scale: f32,
    pub alpha_height_scale: f32,
    pub alpha_max: u8,
    pub ripple_velocity_factor: f32,
    pub warm_up_time: f32,
    pub update_rate: f32,
}

impl Default for FluidSettings {
    fn default() -> Self {
        Self {
            hexagonal: true,
            spacing: 32.0,
            x_size: 32,
            y_size: 32,
            height_scale: 1.0,
            speed: 150.0,
            damping: 0.3,
            noise_frequency: 0.0,
            noise_strength: (-100.0, 100.0),
            u_tiles: 1.0,
            u_offset: 0.0,
            v_tiles: 1.0,
            v_offset: 0.0,
            alpha_curve_scale: 0.0,
            alpha_height_scale: 10.0,
            alpha_max: 128,
            ripple_velocity_factor: -0.04,
            warm_up_time: 2.0,
            update_rate: 30.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FluidOscillator {
    pub location: Vector,
    pub frequency: f32,
    pub phase: u8,
    pub strength: f32,
    pub radius: f32,
    pub time: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FluidToucher {
    pub location: Vector,
    pub velocity: Vector,
    pub radius: f32,
}

/// Unreal position, unnormalised normal as retail, vertex alpha (colour is white), texture coords.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FluidVertex {
    pub position: Vector,
    pub normal: Vector,
    pub alpha: u8,
    pub uv: [f32; 2],
}

#[derive(Debug, Clone)]
pub struct FluidSurface {
    pub settings: FluidSettings,
    pub oscillators: Vec<FluidOscillator>,
    origin: Vector,
    verts: [Vec<f32>; 2],
    scratch: Vec<f32>,
    latest: usize,
    alpha: Vec<u8>,
    clamped: Vec<i32>,
    rollover: f32,
    noise_due: f32,
    height_limit: f32,
    warmed_up: bool,
    rng: Rng,
}

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

    fn index(&self, x: i32, y: i32) -> usize {
        (y * self.settings.x_size + x) as usize
    }

    fn is_clamped(&self, i: usize) -> bool {
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
    fn wave_step(&mut self, step: f32) {
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

    pub fn pling_vertex(&mut self, x: i32, y: i32, strength: f32) {
        if (1..self.settings.x_size - 1).contains(&x) && (1..self.settings.y_size - 1).contains(&y) && self.settings.update_rate > 0.0 {
            let i = self.index(x, y);
            if !self.is_clamped(i) {
                let height = self.verts[self.latest][i] + (1.0 / self.settings.update_rate) * strength;
                self.verts[self.latest][i] = height.clamp(-self.height_limit, self.height_limit);
            }
        }
    }

    pub fn pling(&mut self, position: Vector, strength: f32, radius: f32) {
        let wide = self.settings.spacing * MIN_PLING_RADIUS;
        let (reach, scale) = if radius >= wide {
            (radius, strength)
        } else {
            let total = self.pling_weight_total(position, wide);
            if total <= 0.0 {
                return;
            }
            (wide, strength / total)
        };
        let (x0, y0) = self.nearest_index(position.x - reach, position.y - reach);
        let (x1, y1) = self.nearest_index(position.x + reach, position.y + reach);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let weight = self.pling_weight(x, y, position, reach);
                if weight > 0.0 {
                    self.pling_vertex(x, y, weight * scale);
                }
            }
        }
    }

    fn pling_weight_total(&self, position: Vector, reach: f32) -> f32 {
        let (x0, y0) = self.nearest_index(position.x - reach, position.y - reach);
        let (x1, y1) = self.nearest_index(position.x + reach, position.y + reach);
        let mut total = 0.0;
        for y in y0..=y1 {
            for x in x0..=x1 {
                total += self.pling_weight(x, y, position, reach);
            }
        }
        total
    }

    fn pling_weight(&self, x: i32, y: i32, position: Vector, reach: f32) -> f32 {
        let s = &self.settings;
        if !(1..s.x_size - 1).contains(&x) || !(1..s.y_size - 1).contains(&y) || self.is_clamped(self.index(x, y)) {
            return 0.0;
        }
        let (vx, vy) = self.vertex_xy(x, y);
        let d2 = (vx - position.x) * (vx - position.x) + (vy - position.y) * (vy - position.y);
        if d2 < reach * reach { (reach * reach - d2).sqrt() / reach } else { 0.0 }
    }

    fn nearest_index(&self, x: f32, y: f32) -> (i32, i32) {
        let s = &self.settings;
        let row = if s.hexagonal { s.spacing * ROW } else { s.spacing };
        let ix = ((x - self.origin.x) / s.spacing).round_ties_even() as i32;
        let iy = ((y - self.origin.y) / row).round_ties_even() as i32;
        let hold = |v: i32, count: i32| if v < 0 { 0 } else { v.min(count - 1) };
        (hold(ix, s.x_size), hold(iy, s.y_size))
    }

    fn vertex_xy(&self, x: i32, y: i32) -> (f32, f32) {
        let s = &self.settings;
        if s.hexagonal {
            let shift = if y & 1 != 0 { 0.5 } else { 0.0 };
            ((x as f32 + shift) * s.spacing + self.origin.x, y as f32 * ROW * s.spacing + self.origin.y)
        } else {
            (x as f32 * s.spacing + self.origin.x, y as f32 * s.spacing + self.origin.y)
        }
    }

    /// Heights between the last two steps, so the water moves at any frame rate, one step behind the simulation.
    fn drawn_heights(&self) -> impl Fn(i32, i32) -> f32 + '_ {
        let (old, new) = (&self.verts[1 - self.latest], &self.verts[self.latest]);
        let t = (self.rollover * self.settings.update_rate).clamp(0.0, 1.0);
        let nx = self.settings.x_size;
        move |x, y| {
            let i = (y * nx + x) as usize;
            old[i] + (new[i] - old[i]) * t
        }
    }

    pub fn vertices(&self) -> Vec<FluidVertex> {
        let (nx, ny) = (self.settings.x_size, self.settings.y_size);
        if nx < 1 || ny < 1 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity((nx * ny) as usize);
        if self.settings.hexagonal {
            self.hexagonal_vertices(&mut out);
        } else {
            self.square_vertices(&mut out);
        }
        out
    }

    fn edge_vertex(&self, x: f32, y: f32, uv: [f32; 2]) -> FluidVertex {
        FluidVertex { position: Vector::new(x, y, self.origin.z), normal: Vector::new(0.0, 0.0, 1.0), alpha: 0, uv }
    }

    fn hexagonal_vertices(&self, out: &mut Vec<FluidVertex>) {
        let s = &self.settings;
        let (nx, ny) = (s.x_size, s.y_size);
        let o = self.origin;
        let at = self.drawn_heights();
        let hs = s.height_scale;
        let per_u = 1.0 / (nx as f32 - 0.5);
        let row_spacing = s.spacing * ROW;
        let nz = row_spacing * 4.0 * s.spacing;
        let column = |x: f32| (s.u_tiles * x) * per_u + s.u_offset;
        for x in 0..nx {
            out.push(self.edge_vertex(x as f32 * s.spacing + o.x, o.y, [column(x as f32), s.v_offset]));
        }
        for y in 1..ny - 1 {
            let v = (s.v_tiles * y as f32) / (ny - 1) as f32 + s.v_offset;
            let vy = y as f32 * ROW * s.spacing + o.y;
            let odd = y & 1 != 0;
            let shift = if odd { 0.5 } else { 0.0 };
            out.push(self.edge_vertex(shift * s.spacing + o.x, vy, [if odd { column(0.5) } else { s.u_offset }, v]));
            for x in 1..nx - 1 {
                let c = at(x, y);
                let d = |x: i32, y: i32| (at(x, y) - c) * hs;
                let (l, r) = (d(x - 1, y), d(x + 1, y));
                let (up_left, up_right, down_right, down_left) = if y & 1 == 0 {
                    (d(x - 1, y + 1), d(x, y + 1), d(x, y - 1), d(x - 1, y - 1))
                } else {
                    (d(x, y + 1), d(x + 1, y + 1), d(x + 1, y - 1), d(x, y - 1))
                };
                let normal_x = ((((l * 2.0 * row_spacing - r * 2.0 * row_spacing) + up_left * row_spacing) - up_right * row_spacing) + down_left * row_spacing) - down_right * row_spacing;
                let normal_y = ((down_right - (up_right + up_left)) + down_left) * s.spacing * 1.5;
                let fx = x as f32 + shift;
                out.push(FluidVertex {
                    position: Vector::new(fx * s.spacing + o.x, vy, c * hs + o.z),
                    normal: Vector::new(normal_x, normal_y, nz),
                    alpha: self.alpha[(y * nx + x) as usize],
                    uv: [column(fx), v],
                });
            }
            let last = if odd { (nx as f32 - 0.5, s.u_offset + s.u_tiles) } else { ((nx - 1) as f32, column((nx - 1) as f32)) };
            out.push(self.edge_vertex(last.0 * s.spacing + o.x, vy, [last.1, v]));
        }
        let shift = if (ny - 1) & 1 == 0 { 0.0 } else { 0.5 };
        let vy = (ny - 1) as f32 * ROW * s.spacing + o.y;
        for x in 0..nx {
            let fx = x as f32 + shift;
            out.push(self.edge_vertex(fx * s.spacing + o.x, vy, [column(fx), s.v_offset + s.v_tiles]));
        }
    }

    fn square_vertices(&self, out: &mut Vec<FluidVertex>) {
        let s = &self.settings;
        let (nx, ny) = (s.x_size, s.y_size);
        let o = self.origin;
        let at = self.drawn_heights();
        let hs = s.height_scale;
        let per_u = 1.0 / (nx as f32 - 1.0);
        // Retail's z is negative, which points the normal under the water.
        let nz = s.spacing * s.spacing * 4.0;
        let column = |x: i32| (s.u_tiles * x as f32) * per_u + s.u_offset;
        for x in 0..nx {
            out.push(self.edge_vertex(x as f32 * s.spacing + o.x, o.y, [column(x), s.v_offset]));
        }
        for y in 1..ny - 1 {
            let v = (s.v_tiles * y as f32) / (ny - 1) as f32 + s.v_offset;
            let vy = y as f32 * s.spacing + o.y;
            out.push(self.edge_vertex(o.x, vy, [s.u_offset, v]));
            for x in 1..nx - 1 {
                let c = at(x, y);
                let d = |x: i32, y: i32| (at(x, y) - c) * hs;
                out.push(FluidVertex {
                    position: Vector::new(x as f32 * s.spacing + o.x, vy, c * hs + o.z),
                    normal: Vector::new((d(x - 1, y) - d(x + 1, y)) * s.spacing * 2.0, (d(x, y - 1) - d(x, y + 1)) * s.spacing * 2.0, nz),
                    alpha: self.alpha[(y * nx + x) as usize],
                    uv: [column(x), v],
                });
            }
            out.push(self.edge_vertex((nx - 1) as f32 * s.spacing + o.x, vy, [s.u_tiles + s.u_offset, v]));
        }
        let vy = (ny - 1) as f32 * s.spacing + o.y;
        for x in 0..nx {
            out.push(self.edge_vertex(x as f32 * s.spacing + o.x, vy, [column(x), s.v_offset + s.v_tiles]));
        }
    }

    pub fn indices(&self) -> Vec<u32> {
        let (nx, ny) = (self.settings.x_size, self.settings.y_size);
        let mut out = Vec::new();
        for y in 0..ny - 1 {
            for x in 0..nx - 1 {
                let a = (y * nx + x) as u32;
                let b = ((y + 1) * nx + x) as u32;
                if self.settings.hexagonal && y & 1 == 0 {
                    out.extend([a, b, a + 1, b, b + 1, a + 1]);
                } else {
                    out.extend([a, b + 1, a + 1, a, b, b + 1]);
                }
            }
        }
        out
    }
}

impl FluidOscillator {
    pub fn oscillate(&mut self, step: f32) -> f32 {
        self.time += step;
        if self.frequency <= 1e-4 {
            return self.strength;
        }
        let cycle = self.time % (1.0 / self.frequency);
        let angle = f64::from(self.frequency * cycle + f32::from(self.phase) / 255.0) * std::f64::consts::PI * 2.0;
        angle.sin() as f32 * self.strength
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(size: i32) -> FluidSurface {
        let settings = FluidSettings { hexagonal: false, x_size: size, y_size: size, damping: 0.0, warm_up_time: 0.0, ..FluidSettings::default() };
        FluidSurface::new(settings, Vector::ZERO, Vec::new(), 1)
    }

    fn pond(settings: FluidSettings) -> FluidSurface {
        FluidSurface::new(FluidSettings { warm_up_time: 0.0, x_size: 24, y_size: 24, ..settings }, Vector::ZERO, Vec::new(), 7)
    }

    fn peak(surface: &FluidSurface) -> f32 {
        surface.verts[surface.latest].iter().fold(0.0, |peak: f32, h| peak.max(h.abs()))
    }

    #[test]
    fn a_bad_update_rate_ends_the_tick() {
        for update_rate in [-30.0, 0.0] {
            let settings = FluidSettings { update_rate, x_size: 5, y_size: 5, ..FluidSettings::default() };
            let mut surface = FluidSurface::new(settings, Vector::ZERO, Vec::new(), 1);
            surface.pling(Vector::ZERO, 30.0, 0.0);
            surface.tick(1.0, true, &[]);
            assert_eq!(surface.height(2, 2), 0.0);
        }
    }

    #[test]
    fn origin_is_half_the_grid_below_the_location() {
        let surface = FluidSurface::new(FluidSettings::default(), Vector::new(100.0, 200.0, 50.0), Vec::new(), 1);
        assert_eq!(surface.origin, Vector::new(100.0 - 31.0 * 16.0, 200.0 - 31.0 * 16.0 * ROW, 50.0));
    }

    #[test]
    fn a_narrow_pling_spreads_the_same_kick_around_the_nearest_vertex() {
        let mut surface = square(9);
        surface.pling(Vector::new(3.0, -4.0, 99.0), 30.0, 0.0);
        let heights = &surface.verts[surface.latest];
        assert!((heights.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(heights.iter().all(|&h| h <= surface.height(4, 4)));
        assert!(heights.iter().filter(|&&h| h != 0.0).count() > 1);
    }

    #[test]
    fn a_wide_pling_falls_off_with_distance() {
        let mut surface = square(5);
        surface.pling(Vector::ZERO, 30.0, 96.0);
        assert_eq!(surface.height(2, 2), 1.0);
        assert!((surface.height(3, 2) - 0.942_809).abs() < 1e-5);
        assert_eq!(surface.height(4, 2), 0.0);
    }

    #[test]
    fn a_pling_lands_where_the_vertex_is_drawn_on_odd_rows() {
        let mut surface = FluidSurface::new(FluidSettings { x_size: 9, y_size: 9, ..FluidSettings::default() }, Vector::ZERO, Vec::new(), 1);
        for (x, y) in [(4, 3), (3, 3), (4, 4), (3, 4)] {
            let drawn = surface.vertices()[surface.index(x, y)].position;
            assert_eq!(surface.vertex_xy(x, y), (drawn.x, drawn.y));
            surface.verts[surface.latest].fill(0.0);
            surface.pling(drawn, 30.0, 0.0);
            assert!(surface.verts[surface.latest].iter().all(|&h| h <= surface.height(x, y)));
            assert!(surface.height(x, y) > 0.0);
        }
    }

    #[test]
    fn an_edge_takes_no_kick() {
        let mut surface = square(5);
        surface.pling_vertex(0, 2, 30.0);
        surface.pling_vertex(4, 2, 30.0);
        surface.pling_vertex(2, 0, 30.0);
        assert!(surface.verts.iter().flatten().all(|&h| h == 0.0));
    }

    #[test]
    fn one_wave_step_spreads_a_raised_vertex() {
        let mut surface = square(5);
        surface.pling_vertex(2, 2, 30.0);
        let step = 1.0 / 30.0;
        surface.wave_step(step);
        let c2 = 150.0 * step * 150.0 * step;
        let lap = -4.0 / (32.0 * 32.0);
        assert!((surface.height(2, 2) - (2.0 + lap * c2)).abs() < 1e-6);
        assert!((surface.height(2, 1) - c2 / (32.0 * 32.0)).abs() < 1e-6);
        assert_eq!(surface.height(2, 0), 0.0);
        assert_eq!(surface.verts[1 - surface.latest][surface.index(2, 2)], 1.0);
    }

    #[test]
    fn clamped_vertices_hold_still() {
        let mut surface = square(5);
        surface.clamped = vec![1 << 12];
        surface.pling_vertex(2, 2, 30.0);
        surface.pling_vertex(2, 1, 30.0);
        surface.update(1.0 / 30.0, &[]);
        assert_eq!(surface.height(2, 2), 0.0);
    }

    #[test]
    fn simulation_waits_until_drawn_after_warm_up() {
        let mut surface = square(5);
        surface.pling_vertex(2, 2, 30.0);
        surface.tick(1.0, false, &[]);
        assert_eq!(surface.height(2, 2), 1.0);
        surface.tick(0.05, true, &[]);
        assert!(surface.height(2, 2) > 1.5);
    }

    #[test]
    fn buffers_hold_one_vertex_per_grid_point_and_two_triangles_per_cell() {
        for hexagonal in [false, true] {
            let surface = FluidSurface::new(FluidSettings { hexagonal, x_size: 4, y_size: 5, ..FluidSettings::default() }, Vector::ZERO, Vec::new(), 1);
            assert_eq!(surface.vertices().len(), 20);
            assert_eq!(surface.indices().len(), 3 * (2 * 4 - 2) * 4);
        }
    }

    #[test]
    fn hexagonal_odd_rows_shift_half_a_spacing() {
        let surface = FluidSurface::new(FluidSettings { x_size: 3, y_size: 3, ..FluidSettings::default() }, Vector::ZERO, Vec::new(), 1);
        let vertices = surface.vertices();
        assert_eq!(vertices[3].position.x - vertices[0].position.x, 16.0);
        assert_eq!(vertices[5].uv[0], 1.0);
    }

    #[test]
    fn the_same_time_gives_the_same_water_at_any_frame_rate() {
        let run = |frames: u32, delta: f32| {
            let mut surface = pond(FluidSettings::default());
            surface.pling(Vector::new(10.0, -20.0, 0.0), -300.0, 0.0);
            for _ in 0..frames {
                surface.tick(delta, true, &[]);
            }
            surface.verts[surface.latest].clone()
        };
        let slow = run(61, 1.0 / 60.0);
        assert_eq!(slow, run(122, 1.0 / 120.0));
        assert_eq!(slow, run(244, 1.0 / 240.0));
    }

    #[test]
    fn a_long_frame_runs_only_the_capped_steps() {
        let mut surface = pond(FluidSettings::default());
        surface.tick(0.0, true, &[]);
        surface.tick(100.0, true, &[]);
        assert!(surface.rollover <= 1.0 / 30.0);
    }

    #[test]
    fn drawn_heights_move_between_the_last_two_steps() {
        let mut surface = pond(FluidSettings::default());
        surface.pling_vertex(12, 12, 300.0);
        surface.tick(1.5 / 30.0, true, &[]);
        let (old, new) = (surface.verts[1 - surface.latest][surface.index(12, 12)], surface.height(12, 12));
        let drawn = surface.drawn_heights()(12, 12);
        assert!(old != new && (drawn - (old + new) / 2.0).abs() < 1e-3, "{old} {new} {drawn}");
    }

    #[test]
    fn a_ripple_spreads_then_decays() {
        let mut surface = pond(FluidSettings::default());
        surface.pling(Vector::ZERO, -300.0, 0.0);
        let kicked = peak(&surface);
        assert!(kicked > 0.0);
        for _ in 0..15 {
            surface.tick(1.0 / 30.0, true, &[]);
        }
        let (near, far) = (surface.height(12, 12), surface.height(18, 12));
        assert!(near != 0.0 && far != 0.0);
        for _ in 0..1800 {
            surface.tick(1.0 / 30.0, true, &[]);
        }
        assert!(peak(&surface) < 0.05, "{}", peak(&surface));
    }

    #[test]
    fn heights_stay_in_bounds_at_any_frame_time() {
        let toucher = FluidToucher { location: Vector::new(-40.0, 30.0, 0.0), velocity: Vector::new(600.0, 0.0, 0.0), radius: 28.0 };
        for delta in [1.0 / 500.0, 1.0 / 60.0, 1.0 / 15.0, 0.5] {
            let settings = FluidSettings { noise_frequency: 200.0, noise_strength: (-128.0, 128.0), damping: 0.1, ..FluidSettings::default() };
            let mut surface = pond(settings);
            surface.oscillators.push(FluidOscillator { location: Vector::ZERO, frequency: 1.0, phase: 0, strength: 32.0, radius: 32.0, time: 0.0 });
            let limit = surface.height_limit;
            for _ in 0..(20.0 / delta) as u32 {
                surface.pling(Vector::new(30.0, 30.0, 0.0), -300.0, 0.0);
                surface.tick(delta, true, &[toucher]);
                assert!(surface.verts.iter().flatten().all(|h| h.is_finite() && h.abs() <= limit * 1.001));
            }
        }
    }

    #[test]
    fn a_wave_speed_too_fast_for_the_grid_stays_stable() {
        for hexagonal in [false, true] {
            let mut surface = pond(FluidSettings { hexagonal, speed: 5000.0, update_rate: 10.0, ..FluidSettings::default() });
            surface.pling(Vector::ZERO, -300.0, 0.0);
            for _ in 0..600 {
                surface.tick(0.1, true, &[]);
            }
            let unbounded = surface.height_limit * 0.5;
            assert!(peak(&surface) < unbounded, "{}", peak(&surface));
        }
    }

    #[test]
    fn one_shot_moves_the_water_a_visible_share_of_a_cell() {
        let mut surface = FluidSurface::new(FluidSettings { warm_up_time: 0.0, ..FluidSettings::default() }, Vector::ZERO, Vec::new(), 7);
        let hit = surface.vertices()[surface.index(16, 16)].position;
        surface.pling(hit, -300.0, 0.0);
        let mut deepest: f32 = 0.0;
        for _ in 0..60 {
            surface.update(1.0 / 30.0, &[]);
            deepest = deepest.max(peak(&surface));
        }
        assert!(deepest > 0.15 * 32.0, "{deepest}");
        assert!(deepest <= surface.height_limit, "{deepest}");
    }

    #[test]
    fn a_noise_rate_under_the_update_rate_still_fires() {
        let mut surface = pond(FluidSettings { noise_frequency: 5.0, ..FluidSettings::default() });
        for _ in 0..30 {
            surface.update(1.0 / 30.0, &[]);
        }
        assert!(peak(&surface) > 0.0);
    }
}
