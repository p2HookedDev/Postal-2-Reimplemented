use crate::math::{Rotator, Vector};
use crate::props::{EmitterProperties, EmitterSettings};
use crate::Rng;

mod basefx;
pub use basefx::StripPoint;
mod beam;
pub use beam::{Beam, BeamPoint};
use beam::BeamPoints;
mod draw;
mod emitter;
mod particle_emitter;
mod spawn;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActorForce {
    pub location: Vector,
    pub velocity: Vector,
    pub radius: f32,
    pub scale: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Owner {
    pub location: Vector,
    pub rotation: Rotator,
    pub velocity: Vector,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CollisionHit {
    pub location: Vector,
    pub normal: Vector,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub location: Vector,
    pub right: Vector,
    pub up: Vector,
}

impl Default for View {
    fn default() -> Self {
        Self { location: Vector::ZERO, right: [0.0, 1.0, 0.0].into(), up: [0.0, 0.0, 1.0].into() }
    }
}

pub type Collide<'a> = dyn FnMut(Vector, Vector) -> Option<CollisionHit> + 'a;

const SURFACE_OFFSET: f32 = 0.01;

const MAX_PARTICLE_SLOTS: usize = 65_536;

const MAX_SPAWN_PASSES: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Particle {
    location: Vector,
    old_location: Vector,
    heading: Vector,
    velocity: Vector,
    start_size: Vector,
    /// Rotation units per second.
    spins_per_second: Vector,
    /// Rotation units.
    start_spin: Vector,
    size: Vector,
    color_multiplier: Vector,
    color: [u8; 4],
    time: f32,
    max_lifetime: f32,
    mass: f32,
    hit_count: i32,
    active: bool,
    stopped: bool,
    fresh: bool,
    subdivision: i32,
    serial: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteParticle {
    pub location: Vector,
    pub previous: Vector,
    pub size: [f32; 2],
    /// Corners are `location ∓ up ∓ right`, texture `v0` at `-up` and `u0` at `-right`.
    /// Camera-facing sprites have `-up` at the screen's top.
    pub up: Vector,
    pub right: Vector,
    pub color: [u8; 4],
    pub uv: [f32; 4],
    pub serial: u32,
    pub blend: Option<([f32; 4], u8)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshParticle {
    pub location: Vector,
    pub spin: Rotator,
    pub frame: Rotator,
    pub size: Vector,
    pub color: [u8; 4],
}

#[derive(Debug, Clone)]
#[expect(clippy::struct_excessive_bools, reason = "the engine's own separate emitter flags")]
pub struct ParticleEmitter {
    props: EmitterProperties,
    particles: Vec<Particle>,
    particle_index: usize,
    active_particles: usize,
    pps_fraction: f32,
    all_particles_dead: bool,
    warmed_up: bool,
    other_index: i32,
    initial_delay: f32,
    global_offset: Vector,
    time_till_reset: f32,
    kill_pending: bool,
    inactive: bool,
    inactive_time: f32,
    beam_points: BeamPoints,
    spawned: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FirstUpdate {
    Skip,
    Run,
}

struct SpawnContext<'c, 'o> {
    owner: &'c Owner,
    rng: &'c mut Rng,
    others: &'c Others<'o>,
}

struct Others<'a> {
    before: &'a [ParticleEmitter],
    after: &'a [ParticleEmitter],
}

impl Others<'_> {
    fn get<'s>(&'s self, index: Option<usize>, own: &'s ParticleEmitter) -> Option<&'s ParticleEmitter> {
        let before = self.before.len();
        let index = index?.min(before + self.after.len());
        match index.checked_sub(before) {
            None => self.before.get(index),
            Some(0) => Some(own),
            Some(after) => self.after.get(after - 1),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Emitter {
    settings: EmitterSettings,
    emitters: Vec<ParticleEmitter>,
    rng: Rng,
    global_offset: Vector,
    time_till_reset: f32,
    destroyed: bool,
    owner: Owner,
    forces: std::sync::Arc<[ActorForce]>,
    hits: Vec<(CollisionHit, f32, usize)>,
}
