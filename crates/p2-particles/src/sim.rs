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

/// The most particle slots an emitter gets, whatever `MaxParticles` says: a bad value must not abort on allocation.
const MAX_PARTICLE_SLOTS: usize = 65_536;

/// The most times one spawn starts a particle over because its age passed its lifetime. Values that settle do so long
/// before this (each pass takes the age modulo the lifetime); `InitialTimeRange` above `LifetimeRange` never does.
const MAX_SPAWN_PASSES: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Particle {
    location: Vector,
    old_location: Vector,
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
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteParticle {
    pub location: Vector,
    pub size: [f32; 2],
    /// Half axes of the quad, world coordinates, spin applied: the corners are `location ∓ up ∓ right`; the texture's
    /// `v0` edge is at `-up`, its `u0` edge at `-right`. Camera-facing sprites (`PTDU_None`) are squares of half size
    /// `Size.X` with `-up` at the top of the screen; oriented ones (`UseDirectionAs`) are `Size.Y` along `up` and
    /// `Size.X` along `right`.
    pub up: Vector,
    pub right: Vector,
    pub color: [u8; 4],
    pub uv: [f32; 4],
    /// `BlendBetweenSubdivisions`: the next frame's rectangle and the blend factor (0..=255). The engine writes the
    /// factor into the vertex alpha in place of the particle's alpha.
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
}
