use super::{
    ActorForce, BeamPoints, Collide, CollisionHit, FirstUpdate, MAX_PARTICLE_SLOTS, Others, Owner, Particle,
    ParticleEmitter, SURFACE_OFFSET, SpawnContext,
};
use super::draw::{particle_color, size_scale};
use crate::math::{Vector, mirror};
use crate::props::{CoordinateSystem, EmitterProperties};
use crate::Rng;

impl ParticleEmitter {
    pub fn mark_rendered(&mut self) {
        self.inactive = false;
    }

    pub(super) fn handle_actor_force(&mut self, force: &ActorForce, dt: f32) {
        let radius2 = force.radius * force.radius;
        if radius2 == 0.0 {
            return;
        }
        let speed = force.velocity.size();
        for p in self.particles[..self.active_particles].iter_mut().filter(|p| p.active && p.mass != 0.0) {
            let offset = force.location - p.location;
            let distance2 = offset.size_squared();
            if distance2 >= radius2 {
                continue;
            }
            let away = (-offset).safe_normal();
            let side = if force.velocity.dot(away) > 0.0 { 1.0 } else { -1.0 };
            let direction = away * side;
            let excess = speed - p.velocity.dot(direction);
            let push = excess * dt * force.scale;
            if excess > 0.0 && push > 0.0 {
                p.velocity += direction * (push * distance2 / radius2);
                p.stopped = false;
            }
        }
    }

    pub(super) fn new(mut props: EmitterProperties, rng: &mut Rng) -> Self {
        props.real_projection_normal = props.projection_normal.safe_normal();
        let slots = usize::try_from(props.max_particles).unwrap_or(0).min(MAX_PARTICLE_SLOTS);
        let beam_points = BeamPoints::new(&props, slots);
        let mut emitter = Self {
            props,
            particles: vec![Particle::default(); slots],
            particle_index: 0,
            active_particles: 0,
            pps_fraction: 0.0,
            all_particles_dead: false,
            inactive: false,
            inactive_time: 0.0,
            warmed_up: false,
            other_index: 0,
            initial_delay: 0.0,
            global_offset: Vector::ZERO,
            time_till_reset: 0.0,
            kill_pending: false,
            beam_points,
        };
        emitter.reset(rng);
        emitter
    }

    pub fn props(&self) -> &EmitterProperties {
        &self.props
    }

    pub fn set_disabled(&mut self, disabled: bool) {
        self.props.disabled = disabled;
    }

    pub fn live_particles(&self) -> usize {
        self.particles[..self.active_particles].iter().filter(|p| p.active).count()
    }

    pub fn inactive_for(&self) -> f32 {
        if self.inactive { self.inactive_time } else { 0.0 }
    }

    pub fn all_particles_dead(&self) -> bool {
        self.all_particles_dead
    }

    pub(super) fn reset(&mut self, rng: &mut Rng) {
        self.active_particles = 0;
        self.particle_index = 0;
        self.other_index = 0;
        self.all_particles_dead = false;
        self.warmed_up = false;
        self.initial_delay = self.props.initial_delay_range.rand(rng);
        self.time_till_reset = self.props.auto_reset_time_range.rand(rng);
    }

    /// No collision planes, sounds or actor forces. Returns where particles hit the level, for
    /// `SpawnFromOtherEmitter` (one entry per particle to spawn there).
    pub(super) fn update(&mut self, dt: f32, owner: &Owner, rng: &mut Rng, others: &Others, collide: &mut Collide) -> Vec<CollisionHit> {
        let max = self.particles.len();
        let pps = if self.active_particles < max {
            if self.props.automatic_initial_spawning {
                max as f32 / self.props.lifetime_range.center()
            } else {
                self.props.initial_particles_per_second
            }
        } else {
            self.props.particles_per_second
        };
        if pps > 0.0 && !self.kill_pending {
            self.pps_fraction = self.spawn_particles(self.pps_fraction, pps, dt, owner, rng, others);
        }
        let relative = self.props.coordinate_system == CoordinateSystem::Relative;
        let mut spawns = Vec::new();
        let mut dead = 0;
        for index in 0..self.active_particles {
            let p = &mut self.particles[index];
            if !p.active {
                dead += 1;
                continue;
            }
            let mut moving = !p.stopped || relative;
            if p.fresh {
                p.fresh = false;
                moving = false;
            } else {
                p.time += dt;
            }
            if p.time <= p.max_lifetime {
                if moving {
                    p.velocity += self.props.acceleration * dt;
                    p.old_location = p.location;
                    p.location += p.velocity * dt;
                }
            } else if self.props.respawn_dead_particles {
                let overshoot = self.props.initial_time_range.rand(rng) + (p.time - p.max_lifetime);
                let time = if p.max_lifetime == 0.0 { 0.0 } else { overshoot % p.max_lifetime };
                self.spawn_particle(index, time, FirstUpdate::Run, None, SpawnContext { owner, rng: &mut *rng, others });
            } else {
                p.active = false;
                dead += 1;
                continue;
            }
            let p = &self.particles[index];
            let hit = if moving && !relative && self.props.use_collision && p.location != p.old_location {
                collide(p.old_location, p.location)
            } else {
                None
            };
            if let Some(hit) = hit
                && !self.survives_hit(index, hit, dt, &mut spawns, SpawnContext { owner, rng: &mut *rng, others })
            {
                dead += 1;
                continue;
            }
            let p = &mut self.particles[index];
            p.size = p.start_size * size_scale(&self.props, p.time, p.max_lifetime);
            p.color = particle_color(&self.props, p);
            for axis in 0..3 {
                let limit = self.props.max_abs_velocity[axis];
                if limit != 0.0 {
                    let v = p.velocity[axis];
                    p.velocity[axis] = if v < -limit { -limit } else if v >= limit { limit } else { v };
                }
            }
            let loss = self.props.velocity_loss_range.rand(rng);
            p.velocity = p.velocity - ((p.velocity * loss) * dt);
            let min_speed = self.props.min_squared_velocity;
            if hit.is_some() && min_speed != 0.0 && p.velocity.dot(p.velocity) < min_speed {
                p.stopped = true;
            }
        }
        self.trace_beams(collide, rng);
        self.all_particles_dead =
            (dead >= max || (self.active_particles == dead && pps == 0.0)) && !self.props.respawn_dead_particles;
        spawns
    }

    fn survives_hit(&mut self, index: usize, hit: CollisionHit, dt: f32, spawns: &mut Vec<CollisionHit>, context: SpawnContext) -> bool {
        if self.props.spawn_from_other_emitter.is_some() && dt != 0.0 {
            let on_surface = hit.location + hit.normal * SURFACE_OFFSET;
            let count = usize::try_from(self.props.spawn_amount).unwrap_or(0);
            spawns.extend(std::iter::repeat_n(CollisionHit { location: on_surface, normal: hit.normal }, count));
        }
        let last = self.props.use_max_collisions
            && self.particles[index].hit_count + 1 >= self.props.max_collisions.rand(&mut *context.rng).floor() as i32;
        if last && self.props.respawn_dead_particles {
            self.spawn_particle(index, 0.5 * dt, FirstUpdate::Run, None, context);
        } else if last {
            self.particles[index].active = false;
            return false;
        } else {
            let p = &mut self.particles[index];
            p.velocity -= self.props.acceleration * (dt * 0.5);
            p.velocity = mirror(p.velocity, hit.normal);
            p.velocity *= self.props.damping_factor_range.rand(&mut *context.rng);
            if self.props.damp_rotation {
                p.spins_per_second *= self.props.rotation_damping_factor_range.rand(&mut *context.rng);
            }
            p.hit_count += 1;
            p.location = hit.location + hit.normal * SURFACE_OFFSET;
            p.old_location = p.location;
        }
        true
    }
}
