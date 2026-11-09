use p2_math::{Rng, Vector};

mod mesh;
mod pling;
mod simulation;
#[cfg(test)]
mod tests;

const ROW: f32 = p2_math::HEX_ROW_SPACING;

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

impl FluidSettings {
    pub fn row_spacing(&self) -> f32 {
        if self.hexagonal { ROW } else { 1.0 }
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
    drawn: Vec<f32>,
    latest: usize,
    alpha: Vec<u8>,
    clamped: Vec<i32>,
    rollover: f32,
    noise_due: f32,
    height_limit: f32,
    warmed_up: bool,
    rng: Rng,
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
