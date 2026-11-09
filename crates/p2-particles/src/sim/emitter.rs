use super::{
    ActorForce, Collide, Emitter, FirstUpdate, MeshParticle, Others, Owner, ParticleEmitter, SpawnContext,
    SpriteParticle, View,
};
use super::spawn::spawn_hits;
use crate::math::{Rotator, Vector};
use crate::props::{EmitterProperties, EmitterSettings};
use crate::Rng;

impl Emitter {
    pub fn new(settings: EmitterSettings, emitters: Vec<EmitterProperties>, seed: u32) -> Self {
        let mut rng = Rng::new(seed);
        let emitters = emitters.into_iter().map(|props| ParticleEmitter::new(props, &mut rng)).collect();
        let global_offset = settings.global_offset_range.rand(&mut rng);
        let time_till_reset = settings.time_till_reset_range.rand(&mut rng);
        Self { settings, emitters, rng, global_offset, time_till_reset, destroyed: false, owner: Owner::default(), forces: std::sync::Arc::default() }
    }

    pub fn emitters(&self) -> &[ParticleEmitter] {
        &self.emitters
    }

    pub fn emitters_mut(&mut self) -> &mut [ParticleEmitter] {
        &mut self.emitters
    }

    pub fn destroyed(&self) -> bool {
        self.destroyed
    }

    pub fn owner(&self) -> &Owner {
        &self.owner
    }

    /// Retail applies the actors touching the emitter's collision cylinder, but forces only reach
    /// particles within the actor's `ForceRadius`, so all are passed and the radius decides.
    pub fn set_actor_forces(&mut self, forces: std::sync::Arc<[ActorForce]>) {
        self.forces = forces;
    }

    pub fn kill(&mut self) {
        for e in &mut self.emitters {
            e.props.respawn_dead_particles = false;
            e.props.initial_particles_per_second = 0.0;
            e.props.particles_per_second = 0.0;
            e.props.auto_reset = false;
            e.props.auto_destroy = false;
            e.kill_pending = true;
        }
        self.settings.auto_destroy = true;
        self.settings.auto_reset = false;
    }

    pub fn tick(&mut self, dt: f32, location: Vector, rotation: Rotator) {
        self.tick_owner(dt, Owner { location, rotation, velocity: Vector::ZERO });
    }

    pub fn tick_owner(&mut self, dt: f32, owner: Owner) {
        self.tick_colliding(dt, owner, &mut |_, _| None);
    }

    pub fn tick_colliding(&mut self, dt: f32, owner: Owner, collide: &mut Collide) {
        self.owner = owner;
        if self.destroyed {
            return;
        }
        let mut all_dead = true;
        let mut any_enabled = false;
        let count = self.emitters.len();
        for index in 0..count {
            let (before, rest) = self.emitters.split_at_mut(index);
            let Some((e, after)) = rest.split_first_mut() else { break };
            if e.props.disabled {
                continue;
            }
            any_enabled = true;
            let others = Others { before, after };
            if e.initial_delay != 0.0 {
                e.initial_delay -= dt;
                if e.initial_delay > 0.0 {
                    continue;
                }
                e.initial_delay = 0.0;
            }
            e.global_offset = self.global_offset;
            let (ticks_per_second, warmup) = (e.props.warmup_ticks_per_second, e.props.relative_warmup_time);
            let mut hits = Vec::new();
            if !e.warmed_up && ticks_per_second != 0.0 && warmup != 0.0 {
                let ticks = (e.props.lifetime_range.center() * warmup * ticks_per_second) as i32;
                for _ in 0..ticks {
                    let step = 1.0 / ticks_per_second;
                    hits.extend(e.update(step, &owner, &mut self.rng, &others, collide).into_iter().map(|hit| (hit, step)));
                }
                e.warmed_up = true;
            }
            if !e.all_particles_dead {
                e.inactive_time = if e.inactive { e.inactive_time + dt } else { 0.0 };
                let limit = e.props.seconds_before_inactive;
                if limit == 0.0 || e.inactive_time <= limit {
                    hits.extend(e.update(dt, &owner, &mut self.rng, &others, collide).into_iter().map(|hit| (hit, dt)));
                }
                e.inactive = true;
                all_dead = false;
                if e.props.use_actor_forces {
                    for force in self.forces.iter() {
                        e.handle_actor_force(force, dt);
                    }
                }
                if !hits.is_empty() {
                    let scale = e.props.use_spawned_velocity_scale.then_some(e.props.spawned_velocity_scale_range);
                    let target = e.props.spawn_from_other_emitter.unwrap_or(0).min(count - 1);
                    spawn_hits(&mut self.emitters, target, hits, scale, &owner, &mut self.rng);
                }
            } else if e.props.auto_reset && !self.settings.auto_reset {
                if e.time_till_reset - dt <= 0.0 {
                    e.time_till_reset = e.props.auto_reset_time_range.rand(&mut self.rng);
                    e.reset(&mut self.rng);
                } else {
                    e.time_till_reset -= dt;
                }
            } else if e.props.auto_destroy && !self.settings.auto_destroy {
                e.props.disabled = true;
            }
        }
        if !all_dead {
            return;
        }
        if any_enabled && self.settings.auto_destroy {
            self.destroyed = true;
            return;
        }
        if self.settings.auto_reset && !self.settings.auto_destroy {
            if self.time_till_reset - dt <= 0.0 {
                self.global_offset = self.settings.global_offset_range.rand(&mut self.rng);
                self.time_till_reset = self.settings.time_till_reset_range.rand(&mut self.rng);
                for e in &mut self.emitters {
                    e.reset(&mut self.rng);
                }
            } else {
                self.time_till_reset -= dt;
            }
        }
    }

    pub fn reinitialize(&mut self, index: usize, props: EmitterProperties) {
        if let Some(slot) = self.emitters.get_mut(index) {
            *slot = ParticleEmitter::new(props, &mut self.rng);
        }
    }

    pub fn spawn_one(&mut self, index: usize, offset: Vector) {
        let owner = self.owner;
        let count = self.emitters.len();
        let (before, rest) = self.emitters.split_at_mut(index.min(count));
        let Some((e, after)) = rest.split_first_mut() else { return };
        let max = e.particles.len();
        if max == 0 {
            return;
        }
        e.props.start_location_offset = offset;
        e.spawn_particle(e.particle_index, 0.0, FirstUpdate::Skip, None, SpawnContext { owner: &owner, rng: &mut self.rng, others: &Others { before, after } });
        let next = e.particle_index + 1;
        e.active_particles = e.active_particles.max(next);
        e.particle_index = next % max;
        e.all_particles_dead = false;
    }

    pub fn keep_alive(&mut self, index: usize) {
        if let Some(e) = self.emitters.get_mut(index) {
            e.all_particles_dead = false;
        }
    }

    pub fn cap_time(&mut self, index: usize, time: f32) {
        let Some(e) = self.emitters.get_mut(index) else { return };
        let active = e.active_particles;
        for p in e.particles[..active].iter_mut().filter(|p| p.active && p.time > time) {
            p.time = time;
        }
    }

    pub fn sprites(&self, index: usize, view: View) -> impl Iterator<Item = SpriteParticle> + '_ {
        self.emitters.get(index).into_iter().flat_map(move |e| e.sprites(self.owner, view))
    }

    pub fn meshes(&self, index: usize) -> impl Iterator<Item = MeshParticle> + '_ {
        self.emitters.get(index).into_iter().flat_map(move |e| e.meshes(self.owner))
    }
}
