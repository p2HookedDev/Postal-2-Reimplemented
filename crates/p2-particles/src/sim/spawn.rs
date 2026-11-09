use super::{
    CollisionHit, FirstUpdate, MAX_SPAWN_PASSES, Others, Owner, Particle, ParticleEmitter, SpawnContext, basefx,
};
use crate::math::Vector;
use crate::math::vrand;
use crate::props::{CoordinateSystem, EmitterKind, EmitterProperties};
use crate::props::{RangeVector, RotationSource, StartLocationShape, VelocityDirection};
use crate::Rng;

impl ParticleEmitter {
    fn spawn_on_surface(&mut self, hit: CollisionHit, dt: f32, velocity_scale: Option<RangeVector>, context: SpawnContext) {
        let SpawnContext { owner, rng, others } = context;
        let max = self.particles.len();
        if max == 0 {
            return;
        }
        let index = self.particle_index;
        self.spawn_particle(index, dt, FirstUpdate::Run, Some(hit.location), SpawnContext { owner, rng: &mut *rng, others });
        if let Some(range) = velocity_scale {
            let add_velocity = hit.normal * range.rand(rng);
            let p = &mut self.particles[index];
            p.velocity += add_velocity;
        }
        self.active_particles = self.active_particles.max(index + 1);
        self.particle_index = (index + 1) % max;
    }

    pub(super) fn spawn_particles(&mut self, fraction: f32, pps: f32, dt: f32, owner: &Owner, rng: &mut Rng, others: &Others) -> f32 {
        if !basefx::spawns(&self.props) {
            return 0.0;
        }
        let max = self.particles.len();
        let total = pps * dt + fraction;
        let whole = total.floor();
        let interval = 1.0 / pps;
        let first = (interval * fraction + dt) - interval;
        let count = (whole.max(0.0) as usize).min(max);
        for k in 0..count {
            self.spawn_particle(self.particle_index, first - k as f32 * interval, FirstUpdate::Skip, None, SpawnContext { owner, rng: &mut *rng, others });
            let next = self.particle_index + 1;
            self.active_particles = self.active_particles.max(next);
            self.particle_index = next % max;
        }
        total - whole
    }

    pub(super) fn spawn_particle(&mut self, index: usize, time: f32, first_update: FirstUpdate, at: Option<Vector>, context: SpawnContext) {
        let SpawnContext { owner, rng, others } = context;
        let props = &self.props;
        let strip = props.kind == EmitterKind::Strip;
        if strip && !basefx::strip_spawns(props) {
            return;
        }
        if !strip && (props.max_particles == 0 || self.kill_pending || props.lifetime_range.max <= 0.0) {
            return;
        }
        let (mut time, mut fresh, mut at) = (time, first_update == FirstUpdate::Skip, at);
        let mut passes = 0;
        loop {
            let context = SpawnContext { owner, rng: &mut *rng, others };
            let Some((age, max_lifetime)) = self.start_particle(index, time, fresh, at, context) else { return };
            passes += 1;
            let over = max_lifetime < age && max_lifetime != 0.0;
            if !over {
                break;
            }
            let remainder = age % max_lifetime;
            if passes == MAX_SPAWN_PASSES {
                if let Some(p) = self.particles.get_mut(index) {
                    p.time = remainder;
                }
                break;
            }
            (time, fresh, at) = (remainder, false, None);
        }
        for _ in 0..passes {
            self.all_particles_dead = false;
            self.beam_spawned(index, rng);
        }
    }

    fn start_particle(&mut self, index: usize, time: f32, fresh: bool, at: Option<Vector>, context: SpawnContext) -> Option<(f32, f32)> {
        let SpawnContext { owner, rng, others } = context;
        let props = &self.props;
        let strip = props.kind == EmitterKind::Strip;
        let extended = basefx::start_location(props, rng);
        let mut location = extended.unwrap_or(props.start_location_offset);
        if extended.is_none() {
            location += start_offset(props, rng);
        }
        self.other_index += 1;
        let other_particle = |emitter: Option<usize>| {
            let other = others.get(emitter, self)?;
            let count = i32::try_from(other.active_particles).ok().filter(|&n| n > 0)?;
            other.particles.get(usize::try_from(self.other_index % count).ok()?)
        };
        if let Some(other) = other_particle(props.add_location_from_other_emitter) {
            location += other.location - owner.location;
        }
        let rotation = |v: Vector, normal_axis: bool| match props.use_rotation_from {
            RotationSource::None => v,
            RotationSource::Actor => props.rotation_offset.rotate(owner.rotation.rotate(v)),
            RotationSource::Offset => props.rotation_offset.rotate(v),
            RotationSource::Normal => {
                let mut r = props.rotation_normal.rotation();
                if normal_axis && props.effect_axis_positive_z {
                    r[0] -= 0x4000;
                }
                r.rotate(v)
            }
        };
        location = rotation(location, true);
        if let Some(at) = at {
            location += at;
        } else {
            if props.coordinate_system == CoordinateSystem::Independent {
                location += basefx::independent_origin(props, owner, time);
            }
            location += self.global_offset;
        }

        let color_multiplier = props.color_multiplier_range.rand(rng);
        let mut velocity = props.start_velocity_range.rand(rng);
        let max_lifetime = props.lifetime_range.rand(rng);
        let age = props.initial_time_range.rand(rng) + time;
        let mass = props.start_mass_range.rand(rng);
        let mut start_size = props.start_size_range.rand(rng);
        if props.uniform_size {
            start_size = [start_size.x; 3].into();
        }
        if !strip {
            velocity += props.acceleration * time;
        }
        // The velocity is rotated without the `EffectAxis` pitch change, as retail (unverified).
        velocity = rotation(velocity, false);
        velocity = directed_velocity(props, velocity, location, owner, rng);
        // Which particle of the other emitter lends its velocity (unverified: the one used for locations).
        if let Some(other) = other_particle(props.add_velocity_from_other_emitter) {
            let scaled = if strip { other.velocity } else { props.add_velocity_multiplier_range.rand(rng) * other.velocity };
            velocity += scaled;
        }
        let old_location = location;
        if !strip {
            location += velocity * time;
        }
        let (start_spin, spins_per_second) = start_spins(props, rng);
        let subdivision = random_subdivision(props, rng);
        let p = self.particles.get_mut(index)?;
        *p = Particle {
            location,
            old_location,
            velocity,
            start_size,
            spins_per_second: spins_per_second * 65535.0,
            start_spin: start_spin * 65535.0,
            size: start_size,
            color_multiplier,
            color: p.color,
            time: age,
            max_lifetime,
            mass: if mass == 0.0 { mass } else { 1.0 / mass },
            hit_count: 0,
            active: true,
            stopped: false,
            fresh,
            subdivision,
        };
        Some((age, max_lifetime))
    }
}

fn start_offset(props: &EmitterProperties, rng: &mut Rng) -> Vector {
    match props.start_location_shape {
        StartLocationShape::Box => props.start_location_range.rand(rng),
        StartLocationShape::Sphere => {
            let direction = vrand(rng);
            direction * props.sphere_radius_range.rand(rng)
        }
        StartLocationShape::Polar => {
            let Vector { x: around, y: from_z, z: radius } = props.start_location_polar_range.rand(rng);
            let (around, from_z) = (around.to_radians(), from_z.to_radians());
            [
                from_z.sin() * around.cos() * radius,
                from_z.sin() * around.sin() * radius,
                from_z.cos() * radius,
            ]
            .into()
        }
    }
}

fn directed_velocity(props: &EmitterProperties, velocity: Vector, location: Vector, owner: &Owner, rng: &mut Rng) -> Vector {
    if props.get_velocity_direction_from == VelocityDirection::None {
        return velocity;
    }
    let towards = if props.coordinate_system == CoordinateSystem::Relative {
        location
    } else {
        owner.location - location
    };
    let direction = towards.safe_normal();
    match props.get_velocity_direction_from {
        VelocityDirection::StartPositionAndOwner => velocity * -1.0 * direction,
        VelocityDirection::OwnerAndStartPosition => velocity * direction,
        VelocityDirection::AddRadial if props.kind != EmitterKind::Strip => velocity + direction * props.start_velocity_radial_range.rand(rng),
        VelocityDirection::AddRadial | VelocityDirection::None => velocity,
    }
}

fn start_spins(props: &EmitterProperties, rng: &mut Rng) -> (Vector, Vector) {
    let start_spin = props.start_spin_range.rand(rng);
    let mut spins_per_second = props.spins_per_second_range.rand(rng);
    for axis in 0..3 {
        if rng.frand() < props.spin_ccw_or_cw[axis] {
            spins_per_second[axis] *= -1.0;
        }
    }
    (start_spin, spins_per_second)
}

fn random_subdivision(props: &EmitterProperties, rng: &mut Rng) -> i32 {
    if !props.use_random_subdivision {
        -1
    } else if props.subdivision_end == 0 {
        (rng.frand() * props.texture_u_subdivisions as f32 * props.texture_v_subdivisions as f32) as i32
    } else {
        let start = props.subdivision_start;
        (rng.frand() * (props.subdivision_end - start) as f32 + start as f32) as i32
    }
}

pub(super) fn spawn_hits(emitters: &mut [ParticleEmitter], target: usize, hits: Vec<(CollisionHit, f32)>, scale: Option<RangeVector>, owner: &Owner, rng: &mut Rng) {
    let (before, rest) = emitters.split_at_mut(target);
    let Some((other, after)) = rest.split_first_mut() else { return };
    let others = Others { before, after };
    for (hit, step) in hits {
        other.spawn_on_surface(hit, step, scale, SpawnContext { owner, rng: &mut *rng, others: &others });
    }
}
