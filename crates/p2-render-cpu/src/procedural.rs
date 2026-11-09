use p2_assets::{Mip, WaterDrop, WetParams};

const LEVEL: u8 = 0x80;

fn sine_table() -> [u8; 256] {
    std::array::from_fn(|i| {
        let angle = i as f32 * 0.003_906_25 * std::f32::consts::TAU;
        let value = (f64::from(angle).sin() * 127.5 + 127.45) as f32;
        (value + 0.5).floor() as i32 as u8
    })
}

fn damp(sum: i32) -> u8 {
    let k = sum + 512;
    (k / 2 + i32::from(sum < 256) - 256).clamp(0, 255) as u8
}

#[derive(Debug, Clone)]
pub struct WaterSim {
    w: usize,
    h: usize,
    u: usize,
    /// Per row `y` of `h`: field A (`w` cells), then field B (`w` cells); 0x80 is flat.
    waves: Vec<u8>,
    displacement: Vec<u8>,
    /// Gradient + 512 -> signed displacement.
    refraction: [u8; 1024],
    sine: [u8; 256],
    drops: Vec<WaterDrop>,
    parity: u8,
    rng: u32,
}

/// The water texture's drop types; 0x40 and 0x41 are whirlies with a negative speed.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum DropType {
    FixedDepth = 0,
    PhaseSpot = 1,
    ShallowSpot = 2,
    HalfAmpl = 3,
    RandomMover = 4,
    FixedRandomSpot = 5,
    WhirlyThing = 6,
    BigWhirly = 7,
    HorizontalLine = 8,
    VerticalLine = 9,
    DiagonalLine1 = 0xa,
    DiagonalLine2 = 0xb,
    HorizontalOsc = 0xc,
    VerticalOsc = 0xd,
    DiagonalOsc1 = 0xe,
    DiagonalOsc2 = 0xf,
    RainDrops = 0x10,
    AreaClamp = 0x11,
    LeakyTap = 0x12,
    DrippyTap = 0x13,
    WhirlyThingBack = 0x40,
    BigWhirlyBack = 0x41,
}

impl DropType {
    fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::FixedDepth,
            1 => Self::PhaseSpot,
            2 => Self::ShallowSpot,
            3 => Self::HalfAmpl,
            4 => Self::RandomMover,
            5 => Self::FixedRandomSpot,
            6 => Self::WhirlyThing,
            7 => Self::BigWhirly,
            8 => Self::HorizontalLine,
            9 => Self::VerticalLine,
            0xa => Self::DiagonalLine1,
            0xb => Self::DiagonalLine2,
            0xc => Self::HorizontalOsc,
            0xd => Self::VerticalOsc,
            0xe => Self::DiagonalOsc1,
            0xf => Self::DiagonalOsc2,
            0x10 => Self::RainDrops,
            0x11 => Self::AreaClamp,
            0x12 => Self::LeakyTap,
            0x13 => Self::DrippyTap,
            0x40 => Self::WhirlyThingBack,
            0x41 => Self::BigWhirlyBack,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
enum Step {
    Even,
    Odd,
}

impl WaterSim {
    pub fn new(width: u32, height: u32, params: &WetParams) -> Option<Self> {
        let valid = |n: u32| n >= 8 && n.is_power_of_two();
        if !valid(width) || !valid(height) {
            return None;
        }
        let (u, v) = (width as usize, height as usize);
        let amp = f32::from(params.wave_amp);
        Some(Self {
            w: u / 2,
            h: v / 2,
            u,
            waves: vec![LEVEL; u * v / 2],
            displacement: vec![0; u * v],
            refraction: std::array::from_fn(|i| ((i as f32 - 511.0) * amp * (1.0 / 512.0)) as i8 as u8),
            sine: sine_table(),
            drops: params.drops.clone(),
            parity: 0,
            rng: 0x9e37_79b9,
        })
    }

    /// The displacement of each texel (a signed byte, row-major) after the last step.
    pub fn displacement(&self) -> &[u8] {
        &self.displacement
    }

    fn rand(&mut self) -> u8 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 24) as u8
    }

    fn wrap(&self, x: isize, y: isize) -> (usize, usize) {
        ((x & (self.w as isize - 1)) as usize, (y & (self.h as isize - 1)) as usize)
    }

    fn a(&self, x: isize, y: isize) -> u8 {
        let (x, y) = self.wrap(x, y);
        self.waves[y * self.u + x]
    }

    fn b(&self, x: isize, y: isize) -> u8 {
        let (x, y) = self.wrap(x, y);
        self.waves[y * self.u + self.w + x]
    }

    fn set(&mut self, x: usize, y: usize, a: u8, b: u8) {
        let (x, y) = (x & (self.w - 1), y & (self.h - 1));
        self.waves[y * self.u + x] = a;
        self.waves[y * self.u + self.w + x] = b;
    }

    pub fn step(&mut self) {
        self.redraw_drops();
        self.parity = self.parity.wrapping_add(1);
        let even = self.parity & 1 == 0;
        let (w, h) = (self.w as isize, self.h as isize);
        for y in 0..h {
            for x in 0..w {
                let index = y as usize * self.u + x as usize;
                if even {
                    let sum = i32::from(self.b(x, y)) + i32::from(self.b(x - 1, y)) + i32::from(self.b(x, y - 1))
                        + i32::from(self.b(x - 1, y - 1))
                        - 2 * i32::from(self.waves[index]);
                    self.waves[index] = damp(sum);
                } else {
                    let index = index + self.w;
                    let sum = i32::from(self.a(x, y)) + i32::from(self.a(x + 1, y)) + i32::from(self.a(x, y + 1))
                        + i32::from(self.a(x + 1, y + 1))
                        - 2 * i32::from(self.waves[index]);
                    self.waves[index] = damp(sum);
                }
            }
        }
        for y in 0..h {
            for x in 0..w {
                if even {
                    let (g00, gm0) = (self.gradient(Step::Even, x, y), self.gradient(Step::Even, x - 1, y));
                    let (g0m, gmm) = (self.gradient(Step::Even, x, y - 1), self.gradient(Step::Even, x - 1, y - 1));
                    self.put(2 * x, 2 * y, 2 * g00);
                    self.put(2 * x - 1, 2 * y, g00 + gm0);
                    self.put(2 * x, 2 * y - 1, g00 + g0m);
                    self.put(2 * x - 1, 2 * y - 1, (g00 + gm0 + g0m + gmm) / 2);
                } else {
                    let (g00, gm0) = (self.gradient(Step::Odd, x, y), self.gradient(Step::Odd, x - 1, y));
                    let (g01, gm1) = (self.gradient(Step::Odd, x, y + 1), self.gradient(Step::Odd, x - 1, y + 1));
                    self.put(2 * x + 1, 2 * y + 1, 2 * g01);
                    self.put(2 * x, 2 * y + 1, g01 + gm1);
                    self.put(2 * x + 1, 2 * y, g00 + g01);
                    self.put(2 * x, 2 * y, (g00 + gm0 + g01 + gm1) / 2);
                }
            }
        }
    }

    /// Horizontal gradient of the field that was not updated: field B after an even step, A after an odd one.
    fn gradient(&self, step: Step, x: isize, y: isize) -> i32 {
        match step {
            Step::Even => i32::from(self.b(x, y)) - i32::from(self.b(x - 2, y)),
            Step::Odd => i32::from(self.a(x + 1, y)) - i32::from(self.a(x - 1, y)),
        }
    }

    fn put(&mut self, x: isize, y: isize, gradient: i32) {
        let (u, v) = (self.u as isize, 2 * self.h as isize);
        let index = (y & (v - 1)) as usize * self.u + (x & (u - 1)) as usize;
        self.displacement[index] = self.refraction[(gradient + 512).clamp(0, 1023) as usize];
    }

    fn redraw_drops(&mut self) {
        for i in 0..self.drops.len() {
            let mut drop = self.drops[i];
            self.draw_drop(&mut drop);
            self.drops[i] = drop;
        }
    }

    fn draw_drop(&mut self, drop: &mut WaterDrop) {
        let (x, y) = (usize::from(drop.x), usize::from(drop.y));
        let (depth, count) = (drop.depth, usize::from(drop.d >> 1) + 1);
        let Some(kind) = DropType::from_byte(drop.kind) else {
            return;
        };
        match kind {
            DropType::FixedDepth => self.set(x, y, drop.d, drop.d),
            DropType::PhaseSpot => {
                drop.depth = drop.depth.wrapping_add(drop.d);
                let value = self.sine[usize::from(drop.depth)];
                self.set(x, y, value, value);
            }
            DropType::ShallowSpot => {
                drop.depth = drop.depth.wrapping_add(drop.d);
                let value = (self.sine[usize::from(drop.depth)] >> 1) + 0x40;
                self.set(x, y, value, value);
            }
            DropType::HalfAmpl => {
                drop.depth = drop.depth.wrapping_add(drop.d);
                let value = self.sine[usize::from(drop.depth)].max(0x80);
                self.set(x, y, value, value);
            }
            DropType::RandomMover => {
                let step = |sim: &mut Self| i32::from(sim.rand() & 3) - i32::from(sim.rand() & 3);
                let (dx, dy) = (step(self), step(self));
                drop.x = (i32::from(drop.x) + dx) as u8 & (self.w - 1) as u8;
                drop.y = (i32::from(drop.y) + dy) as u8 & (self.h - 1) as u8;
                self.set(x, y, 0xb9, 0x47);
            }
            DropType::FixedRandomSpot => {
                let (a, b) = (self.rand(), self.rand());
                self.set(x, y, a, b);
            }
            DropType::WhirlyThing | DropType::BigWhirly | DropType::WhirlyThingBack | DropType::BigWhirlyBack => {
                let (a, b, c, d) = (i32::from(drop.a), i32::from(drop.b), i32::from(drop.c), i32::from(drop.d));
                let sum = match kind {
                    DropType::WhirlyThing => (b + d) * 256 + c + a,
                    DropType::BigWhirly => a + b * 256 + c + d * 256,
                    _ => b * 256 - c - d * 256 + a,
                } & 0xffff;
                drop.a = sum as u8;
                drop.b = (sum >> 8) as u8;
                let shift = if matches!(kind, DropType::WhirlyThing | DropType::WhirlyThingBack) { 4 } else { 3 };
                let dx = usize::from(self.sine[usize::from(drop.b)] >> shift);
                let dy = usize::from(self.sine[usize::from(drop.b.wrapping_add(0x40))] >> shift);
                self.set(x + dx, y + dy, depth, depth);
            }
            DropType::HorizontalLine => (0..count).for_each(|i| self.set(x + i, y, depth, depth)),
            DropType::VerticalLine => (0..count).for_each(|i| self.set(x, y + i, depth, depth)),
            DropType::DiagonalLine1 => (0..count).for_each(|i| self.set(x.wrapping_sub(i), y + i, depth, depth)),
            DropType::DiagonalLine2 => (0..count).for_each(|i| self.set(x + i, y + i, depth, depth)),
            DropType::HorizontalOsc | DropType::VerticalOsc | DropType::DiagonalOsc1 | DropType::DiagonalOsc2 => {
                drop.depth = drop.depth.wrapping_add(drop.c);
                let value = self.sine[usize::from(drop.depth)];
                for i in 0..count {
                    match kind {
                        DropType::HorizontalOsc => self.set(x + i, y, value, value),
                        DropType::VerticalOsc => self.set(x, y + i, value, value),
                        DropType::DiagonalOsc1 => self.set(x.wrapping_sub(i), y + i, value, value),
                        _ => self.set(x + i, y + i, value, value),
                    }
                }
            }
            DropType::RainDrops => {
                if self.rand() & 0xf == 0 {
                    let (ry, rx) = (usize::from(self.rand()), usize::from(self.rand()));
                    let range = usize::from(drop.d);
                    self.set(x + ((rx * range) >> 8), y + ((ry * range) >> 8), depth, !depth);
                }
            }
            DropType::AreaClamp => {
                let size = usize::from(drop.d >> 1);
                for j in 0..size {
                    for i in 0..size {
                        self.set(x + i, y + j, depth, depth);
                    }
                }
            }
            DropType::LeakyTap | DropType::DrippyTap => {
                drop.a = drop.a.wrapping_add(drop.d);
                if drop.a <= drop.d {
                    if kind == DropType::DrippyTap {
                        drop.a = self.rand();
                    }
                    self.set(x, y, depth, !depth);
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct WetSim {
    water: WaterSim,
    width: u32,
    source: Vec<u8>,
    indices: Vec<u8>,
}

impl WetSim {
    /// Over `source` (P8 palette indices): a larger source is sampled down, a smaller one gives `None`.
    pub fn new(params: &WetParams, width: u32, height: u32, source: &Mip) -> Option<Self> {
        let water = WaterSim::new(width, height, params)?;
        let (shift_x, shift_y) = (source.width.checked_ilog2()?.checked_sub(width.ilog2())?, source.height.checked_ilog2()?.checked_sub(height.ilog2())?);
        let area = source.width.checked_mul(source.height)?;
        if !source.width.is_power_of_two() || !source.height.is_power_of_two() || source.data.len() < area as usize {
            return None;
        }
        let mut sampled = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                sampled.push(source.data[(((y >> shift_y) * source.width) + (x >> shift_x)) as usize]);
            }
        }
        Some(Self { water, width, indices: sampled.clone(), source: sampled })
    }

    pub fn indices(&self) -> &[u8] {
        &self.indices
    }

    pub fn step(&mut self) {
        self.water.step();
        let (width, mask) = (self.width as usize, self.width as usize - 1);
        let rows = self.water.displacement().chunks_exact(width).zip(self.source.chunks_exact(width));
        for ((row, source), displaced) in rows.zip(self.indices.chunks_exact_mut(width)) {
            for (x, (&shift, out)) in row.iter().zip(displaced).enumerate() {
                *out = source[(x + usize::from(shift)) & mask];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(drops: Vec<WaterDrop>) -> WetParams {
        WetParams { wave_amp: 128, drops }
    }

    #[test]
    fn sine_table_matches_retail_endpoints() {
        let table = sine_table();
        assert_eq!([table[0], table[64], table[128], table[192]], [127, 255, 127, 0]);
    }

    #[test]
    fn damping_table_keeps_a_flat_pool_flat() {
        assert_eq!(damp(256), 128);
        assert_eq!(damp(-512), 0);
        assert_eq!(damp(1020), 255);
    }

    #[test]
    fn flat_pool_has_no_displacement() {
        let mut sim = WaterSim::new(16, 16, &params(Vec::new())).unwrap();
        for _ in 0..4 {
            sim.step();
        }
        assert!(sim.displacement().iter().all(|&d| d == 0));
        assert!(sim.waves.iter().all(|&w| w == LEVEL));
    }

    #[test]
    fn a_drop_makes_ripples_that_move_the_source() {
        let drop = WaterDrop { kind: 0, x: 3, y: 3, d: 255, ..WaterDrop::default() };
        let source = Mip { width: 16, height: 16, data: (0..256).map(|i| (i % 16) as u8).collect() };
        let mut sim = WetSim::new(&params(vec![drop]), 16, 16, &source).unwrap();
        assert_eq!(sim.indices(), &source.data[..]);
        for _ in 0..6 {
            sim.step();
        }
        assert_ne!(sim.indices(), &source.data[..]);
    }

    #[test]
    fn smaller_source_disables_the_effect() {
        let source = Mip { width: 8, height: 8, data: vec![0; 64] };
        assert!(WetSim::new(&params(Vec::new()), 16, 16, &source).is_none());
    }

    #[test]
    fn larger_source_is_sampled_down() {
        let source = Mip { width: 16, height: 16, data: (0..256).map(|i| (i % 16) as u8).collect() };
        let sim = WetSim::new(&params(Vec::new()), 8, 8, &source).unwrap();
        assert_eq!(&sim.indices()[..4], &[0, 0, 1, 1]);
    }
}
