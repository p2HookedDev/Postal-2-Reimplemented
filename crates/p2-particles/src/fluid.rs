use p2_math::{Rng, Vector};

const ROW: f32 = 0.866_025;

fn bit_set(words: &[i32], i: usize) -> bool {
    words.get(i >> 5).is_some_and(|word| word & (1 << (i & 31)) != 0)
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
    latest: usize,
    alpha: Vec<u8>,
    clamped: Vec<i32>,
    rollover: f32,
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
        Self {
            origin: Vector::new(location.x - half_x, location.y - half_y, location.z),
            verts: [vec![0.0; count], vec![0.0; count]],
            latest: 1,
            alpha: vec![0; count],
            clamped: clamp_bitmap,
            rollover: 0.0,
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
    pub fn tick(&mut self, delta: f32, drawn_lately: bool, touchers: &[FluidToucher]) {
        if self.settings.update_rate.is_nan() || self.settings.update_rate <= 0.0 {
            return;
        }
        let step = 1.0 / self.settings.update_rate;
        if !self.warmed_up {
            let mut time = 0.0;
            while time < self.settings.warm_up_time {
                self.update(step, touchers);
                time += step;
            }
            self.warmed_up = true;
        }
        if drawn_lately {
            self.rollover += delta;
            while step < self.rollover {
                self.update(step, touchers);
                self.rollover -= step;
            }
        }
    }

    pub fn update(&mut self, step: f32, touchers: &[FluidToucher]) {
        for toucher in touchers {
            let speed = (toucher.velocity.x * toucher.velocity.x + toucher.velocity.y * toucher.velocity.y).sqrt();
            self.pling(toucher.location, self.settings.ripple_velocity_factor * speed, toucher.radius);
        }
        let (nx, ny) = (self.settings.x_size, self.settings.y_size);
        // Retail divides by `FluidXSize - 2` here; a grid that narrow would fault, so it gets no noise.
        if nx > 2 && ny > 2 {
            for _ in 0..(self.settings.noise_frequency * step).floor() as i32 {
                let x = self.rng.rand() % (nx - 2) + 1;
                let y = self.rng.rand() % (ny - 2) + 1;
                if !self.is_clamped(self.index(x, y)) {
                    let (min, max) = self.settings.noise_strength;
                    let strength = min + (max - min) * self.rng.frand();
                    self.pling_vertex(x, y, strength);
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
    }

    /// The wave step, into the older buffer. Hexagonal: 6 neighbours, odd rows shifted half a spacing
    /// right. Edge vertices never change here.
    fn wave_step(&mut self, step: f32) {
        let s = &self.settings;
        let (nx, ny) = (s.x_size, s.y_size);
        let damp = 1.0 - s.damping * step;
        let speed = s.speed * step * s.speed * step;
        let inv = 1.0 / (s.spacing * s.spacing);
        let (src, dst) = (self.latest, 1 - self.latest);
        self.latest = dst;
        let [a, b] = &mut self.verts;
        let (cur, out) = if src == 0 { (&*a, b) } else { (&*b, a) };
        let width = nx as usize;
        let row = |y: i32| &cur[y as usize * width..(y + 1) as usize * width];
        for y in 1..ny - 1 {
            let (below, here, above) = (row(y - 1), row(y), row(y + 1));
            let odd = y & 1 != 0;
            for x in 1..width.saturating_sub(1) {
                let i = y as usize * width + x;
                if !self.clamped.is_empty() && bit_set(&self.clamped, i) {
                    continue;
                }
                let c = here[x];
                let lap = if !s.hexagonal {
                    (here[x - 1] + here[x + 1] + below[x] + above[x] - c * 4.0) * inv
                } else if !odd {
                    (here[x - 1] + above[x - 1] + above[x] + here[x + 1] + below[x] + below[x - 1] - c * 6.0) * inv
                } else {
                    (here[x - 1] + above[x] + above[x + 1] + here[x + 1] + below[x + 1] + below[x] - c * 6.0) * inv
                };
                out[i] = ((c * 2.0 + lap * speed) - out[i]) * damp;
                let alpha = (s.alpha_height_scale * out[i] + s.alpha_curve_scale * lap).round_ties_even() as i32;
                self.alpha[i] = alpha.clamp(0, i32::from(s.alpha_max)) as u8;
            }
        }
    }

    pub fn pling_vertex(&mut self, x: i32, y: i32, strength: f32) {
        if (0..self.settings.x_size).contains(&x) && (0..self.settings.y_size).contains(&y) {
            let i = self.index(x, y);
            if !self.is_clamped(i) {
                self.verts[self.latest][i] += (1.0 / self.settings.update_rate) * strength;
            }
        }
    }

    pub fn pling(&mut self, position: Vector, strength: f32, radius: f32) {
        if radius <= 0.01 {
            let (x, y) = self.nearest_index(position.x, position.y);
            self.pling_vertex(x, y, strength);
            return;
        }
        let (x0, y0) = self.nearest_index(position.x - radius, position.y - radius);
        let (x1, y1) = self.nearest_index(position.x + radius, position.y + radius);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (vx, vy) = self.vertex_xy(x, y);
                let d2 = (vx - position.x) * (vx - position.x) + (vy - position.y) * (vy - position.y);
                if d2 < radius * radius {
                    self.pling_vertex(x, y, ((radius * radius - d2).sqrt() / radius) * strength);
                }
            }
        }
    }

    fn nearest_index(&self, x: f32, y: f32) -> (i32, i32) {
        let s = &self.settings;
        let row = if s.hexagonal { s.spacing * ROW } else { s.spacing };
        let ix = ((x - self.origin.x) / s.spacing).round_ties_even() as i32;
        let iy = ((y - self.origin.y) / row).round_ties_even() as i32;
        let hold = |v: i32, count: i32| if v < 0 { 0 } else { v.min(count - 1) };
        (hold(ix, s.x_size), hold(iy, s.y_size))
    }

    /// Retail shifts a hexagonal vertex half a spacing by the parity of its column `x`, not of its row
    /// as the drawn grid does; `pling` measures from these.
    fn vertex_xy(&self, x: i32, y: i32) -> (f32, f32) {
        let s = &self.settings;
        if s.hexagonal {
            let mut vx = x as f32 * s.spacing + self.origin.x;
            if x & 1 != 0 {
                vx = (f64::from(s.spacing) * 0.5 + f64::from(vx)) as f32;
            }
            (vx, y as f32 * ROW * s.spacing + self.origin.y)
        } else {
            (x as f32 * s.spacing + self.origin.x, y as f32 * s.spacing + self.origin.y)
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
        let h = &self.verts[self.latest];
        let at = |x: i32, y: i32| h[(y * nx + x) as usize];
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
        let h = &self.verts[self.latest];
        let at = |x: i32, y: i32| h[(y * nx + x) as usize];
        let hs = s.height_scale;
        let per_u = 1.0 / (nx as f32 - 1.0);
        let nz = s.spacing * s.spacing * -4.0;
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
    /// `OscTime` advances by `step`; strength is `Strength` at `Frequency` <= 1e-4, else `sin((Frequency *
    /// fmod(OscTime, 1 / Frequency) + Phase / 255) * 2π) * Strength` (fmod order (unverified)).
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

    #[test]
    fn a_bad_update_rate_ends_the_tick() {
        for update_rate in [-30.0, 0.0] {
            let settings = FluidSettings { update_rate, x_size: 5, y_size: 5, ..FluidSettings::default() };
            let mut surface = FluidSurface::new(settings, Vector::ZERO, Vec::new(), 1);
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
    fn a_pling_raises_the_nearest_vertex_by_strength_over_update_rate() {
        let mut surface = square(5);
        surface.pling(Vector::new(3.0, -4.0, 99.0), 30.0, 0.0);
        assert_eq!(surface.height(2, 2), 1.0);
        assert_eq!(surface.verts[0].iter().chain(&surface.verts[1]).filter(|&&h| h != 0.0).count(), 1);
    }

    #[test]
    fn a_wide_pling_falls_off_with_distance() {
        let mut surface = square(5);
        surface.pling(Vector::ZERO, 30.0, 40.0);
        assert_eq!(surface.height(2, 2), 1.0);
        assert!((surface.height(3, 2) - 0.6).abs() < 1e-6);
        assert_eq!(surface.height(3, 3), 0.0);
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
        surface.wave_step(1.0 / 30.0);
        assert_eq!(surface.height(2, 2), 0.0);
    }

    #[test]
    fn simulation_waits_until_drawn_after_warm_up() {
        let mut surface = square(5);
        surface.pling_vertex(2, 2, 30.0);
        surface.tick(1.0, false, &[]);
        assert_eq!(surface.height(2, 2), 1.0);
        surface.tick(0.05, true, &[]);
        assert!((surface.height(2, 2) - (2.0 - 4.0 / 1024.0 * 25.0)).abs() < 1e-5);
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
}
