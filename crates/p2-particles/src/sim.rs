use crate::math::{Rotator, Vector, fractional, mirror, non_parallel, rotate_about, rotate_by};
use crate::math::vrand;
use crate::props::{CoordinateSystem, DirectionUsage, DrawStyle, EmitterKind, EmitterProperties, EmitterSettings};
use crate::props::{RangeVector, RotationSource, StartLocationShape, VelocityDirection};
use crate::Rng;

mod basefx;
pub use basefx::StripPoint;
mod beam;
pub use beam::{Beam, BeamPoint};
use beam::BeamPoints;

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

impl ParticleEmitter {
    pub fn mark_rendered(&mut self) {
        self.inactive = false;
    }

    fn handle_actor_force(&mut self, force: &ActorForce, dt: f32) {
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

    fn new(mut props: EmitterProperties, rng: &mut Rng) -> Self {
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

    fn reset(&mut self, rng: &mut Rng) {
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
    fn update(&mut self, dt: f32, owner: &Owner, rng: &mut Rng, others: &Others, collide: &mut Collide) -> Vec<CollisionHit> {
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
            if let Some(hit) = hit {
                if self.props.spawn_from_other_emitter.is_some() && dt != 0.0 {
                    let on_surface = hit.location + hit.normal * SURFACE_OFFSET;
                    let count = usize::try_from(self.props.spawn_amount).unwrap_or(0);
                    spawns.extend(std::iter::repeat_n(CollisionHit { location: on_surface, normal: hit.normal }, count));
                }
                let last = self.props.use_max_collisions
                    && p.hit_count + 1 >= self.props.max_collisions.rand(rng).floor() as i32;
                if last && self.props.respawn_dead_particles {
                    self.spawn_particle(index, 0.5 * dt, FirstUpdate::Run, None, SpawnContext { owner, rng: &mut *rng, others });
                } else if last {
                    self.particles[index].active = false;
                    dead += 1;
                    continue;
                } else {
                    let p = &mut self.particles[index];
                    p.velocity -= self.props.acceleration * (dt * 0.5);
                    p.velocity = mirror(p.velocity, hit.normal);
                    p.velocity *= self.props.damping_factor_range.rand(rng);
                    if self.props.damp_rotation {
                        p.spins_per_second *= self.props.rotation_damping_factor_range.rand(rng);
                    }
                    p.hit_count += 1;
                    p.location = hit.location + hit.normal * SURFACE_OFFSET;
                    p.old_location = p.location;
                }
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

    fn spawn_particles(&mut self, fraction: f32, pps: f32, dt: f32, owner: &Owner, rng: &mut Rng, others: &Others) -> f32 {
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

    fn spawn_particle(&mut self, index: usize, time: f32, first_update: FirstUpdate, at: Option<Vector>, context: SpawnContext) {
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

    pub fn sprites(&self, owner: Owner, view: View) -> impl Iterator<Item = SpriteParticle> + '_ {
        let props = &self.props;
        let relative = props.coordinate_system == CoordinateSystem::Relative;
        let axes = owner.rotation.axes();
        let direction_to_world = move |v: Vector| if relative { rotate_by(&axes, v) } else { v };
        // Retail reads the cached `RealProjectionNormal` here, not `ProjectionNormal`.
        let normal = props.real_projection_normal;
        let rotated_normal = if props.use_rotation_from == RotationSource::Actor { rotate_by(&axes, normal) } else { normal };
        let (rotated_normal, normal) = (direction_to_world(rotated_normal), direction_to_world(normal));
        // The engine's camera "up" axis points down the screen (the texture's v0 edge is at -up).
        let (screen_right, screen_down) = (view.right, view.up * -1.0);
        let front = screen_right.cross(screen_down);
        let live: &[Particle] = if props.disabled { &[] } else { &self.particles[..self.active_particles] };
        live.iter().filter(|p| p.active).map(move |p| {
            let location = if relative { rotate_by(&axes, p.location) + owner.location } else { p.location };
            let moved = || direction_to_world((p.location - p.old_location).safe_normal());
            let [size_x, size_y] = [p.size.x, p.size.y];
            let (mut up, mut right, axis) = match props.use_direction_as {
                DirectionUsage::None => (screen_down * size_x, screen_right * size_x, front),
                DirectionUsage::Up | DirectionUsage::UpAndNormal => {
                    let moved = moved();
                    (moved * size_y, across(props.use_direction_as, moved, location, &view, normal) * size_x, front)
                }
                DirectionUsage::Right | DirectionUsage::RightAndNormal => {
                    let moved = moved();
                    (across(props.use_direction_as, moved, location, &view, normal) * size_y, moved * size_x, front)
                }
                DirectionUsage::Forward | DirectionUsage::Normal => {
                    let direction = if props.use_direction_as == DirectionUsage::Normal { rotated_normal } else { moved() };
                    // Not normalized (as in the engine): `Size.Y` times the length of the cross product.
                    let up = direction.cross(non_parallel(direction)) * size_y;
                    (up, direction.cross(up).safe_normal() * size_x, direction)
                }
            };
            if props.spin_particles {
                let angle = spin_units(p);
                (up, right) = (rotate_about(up, angle, axis), rotate_about(right, angle, axis));
            }
            let (uv, blend) = subdivision(props, p);
            SpriteParticle { location, size: [size_x, size_y], up, right, color: p.color, uv, blend }
        })
    }

    pub fn meshes(&self, owner: Owner) -> impl Iterator<Item = MeshParticle> + '_ {
        let props = &self.props;
        let relative = props.coordinate_system == CoordinateSystem::Relative;
        let frame = if relative { owner.rotation } else { Rotator::ZERO };
        let axes = owner.rotation.axes();
        let live: &[Particle] = if props.disabled { &[] } else { &self.particles[..self.active_particles] };
        live.iter().filter(|p| p.active).map(move |p| {
            let location = if relative { rotate_by(&axes, p.location) + owner.location } else { p.location };
            let spin = if props.spin_particles { mesh_spin(p) } else { Rotator::ZERO };
            MeshParticle { location, spin, frame, size: p.size, color: p.color }
        })
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

fn mesh_spin(p: &Particle) -> Rotator {
    let turned = |axis: usize| (p.start_spin[axis] + p.time * p.spins_per_second[axis]) as i32;
    [turned(1), turned(0), turned(2)].into()
}

fn across(usage: DirectionUsage, moved: Vector, location: Vector, view: &View, normal: Vector) -> Vector {
    match usage {
        DirectionUsage::UpAndNormal | DirectionUsage::RightAndNormal => moved.cross(normal),
        _ => moved.cross(location - view.location).safe_normal(),
    }
}

fn relative_time(time: f32, max_lifetime: f32) -> f32 {
    if max_lifetime == 0.0 {
        return 0.0;
    }
    (time / max_lifetime).clamp(0.0, 1.0)
}

fn key_span<K: Copy>(keys: &[K], t: f32, time: impl Fn(&K) -> f32) -> Option<(Option<K>, K, f32)> {
    let index = keys.iter().position(|key| t <= time(key))?;
    let key = keys[index];
    let previous = index.checked_sub(1).map(|before| keys[before]);
    let (key_time, previous_time) = (time(&key), previous.map_or(0.0, |before| time(&before)));
    let f = if key_time == 0.0 { 1.0 } else { (t - previous_time) / (key_time - previous_time) };
    Some((previous, key, f))
}

fn size_scale(props: &EmitterProperties, time: f32, max_lifetime: f32) -> f32 {
    if !props.use_size_scale {
        return 1.0;
    }
    if props.use_regular_size_scale {
        return 1.0 / (time + 1.0);
    }
    if max_lifetime == 0.0 {
        return 1.0;
    }
    let t = fractional((props.size_scale_repeats + 1.0) * relative_time(time, max_lifetime));
    let Some((previous, key, f)) = key_span(&props.size_scale, t, |key| key.time) else { return 1.0 };
    let prev_size = previous.map_or(1.0, |before| before.size);
    (key.size - prev_size) * f + prev_size
}

fn particle_color(props: &EmitterProperties, p: &Particle) -> [u8; 4] {
    let (time, max_lifetime) = (p.time, p.max_lifetime);
    let [mut r, mut g, mut b, mut a] = [255i32; 4];
    if props.use_color_scale && max_lifetime != 0.0 {
        let t = fractional((props.color_scale_repeats + 1.0) * relative_time(time, max_lifetime));
        if let Some((previous, key, f)) = key_span(&props.color_scale, t, |key| key.time) {
            let prev = previous.map_or([255; 4], |before| before.color);
            let lerp = |c: usize| ((i32::from(key.color[c]) - i32::from(prev[c])) as f32 * f + f32::from(prev[c])) as i32;
            [r, g, b] = [lerp(0), lerp(1), lerp(2)];
            a = if props.draw_style == DrawStyle::AlphaBlend { lerp(3) } else { 255 };
        }
    }
    let multiply = |c: i32, m: f32| i32::from(((c & 0xff) as f32 * m) as i32 as u8);
    [r, g, b] = [multiply(r, p.color_multiplier[0]), multiply(g, p.color_multiplier[1]), multiply(b, p.color_multiplier[2])];
    a &= 0xff;
    let fade_out = props.fade_out && props.fade_out_start_time < time && max_lifetime != props.fade_out_start_time;
    let fade_in = props.fade_in && time < props.fade_in_end_time && props.fade_in_end_time != 0.0;
    if fade_out || fade_in {
        let (amount, factor) = if props.fade_out && time > props.fade_out_start_time {
            ((time - props.fade_out_start_time) / (max_lifetime - props.fade_out_start_time), props.fade_out_factor)
        } else {
            ((props.fade_in_end_time - time) / props.fade_in_end_time, props.fade_in_factor)
        };
        let amount = amount * 255.0;
        let fade = |c: i32, factor: f32| {
            let v = c as f32 - factor * amount;
            if v < 0.0 { 0 } else if v >= 255.0 { 255 } else { v as i32 }
        };
        match props.draw_style {
            DrawStyle::Modulated => return [127, 127, 127, (255 - (factor[3] * amount) as i32) as u8],
            DrawStyle::AlphaBlend => a = fade(a, factor[3]),
            _ => [r, g, b, a] = [fade(r, factor[0]), fade(g, factor[1]), fade(b, factor[2]), fade(a, factor[3])],
        }
    }
    [r, g, b, a].map(|c| c as u8)
}

fn spin_units(p: &Particle) -> i32 {
    let turned = p.spins_per_second[0] * p.time;
    if p.spins_per_second[0] < 0.0 {
        0xffff - ((p.start_spin[0] - turned).round() as i32 & 0xffff)
    } else {
        (turned + p.start_spin[0]).round() as i32 & 0xffff
    }
}

/// Texture rectangle of subdivision `index`: column `index / V`, row `index % V` (unverified).
fn subdivision_uv(props: &EmitterProperties, index: i32, clamp: bool) -> [f32; 4] {
    let (u, v) = (props.texture_u_subdivisions, props.texture_v_subdivisions);
    let (du, dv) = (1.0 / u as f32, 1.0 / v as f32);
    let u0 = (index / v) as f32 * du;
    let v0 = (index % v) as f32 * dv;
    if clamp { [u0, v0, (u0 + du).min(1.0), (v0 + dv).min(1.0)] } else { [u0, v0, u0 + du, v0 + dv] }
}

fn subdivision(props: &EmitterProperties, p: &Particle) -> ([f32; 4], Option<([f32; 4], u8)>) {
    let whole = [0.0, 0.0, 1.0, 1.0];
    let (u, v) = (props.texture_u_subdivisions, props.texture_v_subdivisions);
    if u == 0 || v == 0 {
        return (whole, None);
    }
    if p.subdivision != -1 {
        return (subdivision_uv(props, p.subdivision, true), None);
    }
    if p.max_lifetime == 0.0 {
        return (whole, None);
    }
    let rel = relative_time(p.time, p.max_lifetime);
    let mut count = u * v;
    let index = if props.use_subdivision_scale {
        let n = props.subdivision_scale.iter().position(|&s| s >= rel).unwrap_or(props.subdivision_scale.len());
        i32::try_from(n).unwrap_or(i32::MAX)
    } else if props.subdivision_end == 0 {
        (count as f32 * rel).floor() as i32
    } else {
        count = (props.subdivision_end - props.subdivision_start).max(1);
        (count as f32 * rel).floor() as i32 + props.subdivision_start
    };
    let index = index.clamp(0, (u * v - 1).max(0));
    let uv = subdivision_uv(props, index, true);
    if !props.blend_between_subdivisions {
        return (uv, None);
    }
    let next = if index + 1 == count { uv } else { subdivision_uv(props, index + 1, false) };
    let scale_len = i32::try_from(props.subdivision_scale.len()).unwrap_or(i32::MAX);
    let factor = if !props.use_subdivision_scale || scale_len <= count {
        fractional(count as f32 * rel)
    } else {
        let at = usize::try_from(index).unwrap_or(0);
        let prev = if at == 0 { 0.0 } else { props.subdivision_scale[at - 1] };
        let current = props.subdivision_scale.get(at).copied().unwrap_or(1.0).min(1.0);
        (rel - prev) / (current - prev)
    };
    (uv, Some((next, (factor * 255.0) as i32 as u8)))
}

fn spawn_hits(emitters: &mut [ParticleEmitter], target: usize, hits: Vec<(CollisionHit, f32)>, scale: Option<RangeVector>, owner: &Owner, rng: &mut Rng) {
    let (before, rest) = emitters.split_at_mut(target);
    let Some((other, after)) = rest.split_first_mut() else { return };
    let others = Others { before, after };
    for (hit, step) in hits {
        other.spawn_on_surface(hit, step, scale, SpawnContext { owner, rng: &mut *rng, others: &others });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::props::{ColorKey, Range, SizeKey};

    fn drawn() -> EmitterProperties {
        EmitterProperties { seconds_before_inactive: 0.0, ..EmitterProperties::default() }
    }

    #[test]
    fn spawn_ends_on_values_that_never_settle() {
        let props = EmitterProperties { initial_time_range: Range::new(5.5, 5.5), lifetime_range: Range::new(1.0, 1.0), max_particles: 1, ..drawn() };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![props], 1);
        emitter.tick(1.0, Vector::ZERO, Rotator::ZERO);
        assert_eq!(emitter.emitters()[0].live_particles(), 1);
    }

    fn props() -> EmitterProperties {
        EmitterProperties { lifetime_range: Range::new(2.0, 2.0), ..drawn() }
    }

    fn particle(time: f32, max_lifetime: f32) -> Particle {
        Particle { time, max_lifetime, color_multiplier: [1.0; 3].into(), active: true, subdivision: -1, ..Particle::default() }
    }

    #[test]
    fn script_commands_spawn_cap_and_reinitialize() {
        let quiet = EmitterProperties { max_particles: 4, respawn_dead_particles: false, ..props() };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![quiet.clone()], 1);
        emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
        let before = emitter.emitters()[0].live_particles();
        emitter.spawn_one(0, [5.0, 0.0, 0.0].into());
        assert_eq!(emitter.emitters()[0].live_particles(), before + 1);
        emitter.tick(0.5, [0.0; 3].into(), [0; 3].into());
        emitter.cap_time(0, 0.1);
        assert!(emitter.emitters()[0].particles.iter().filter(|p| p.active).all(|p| p.time <= 0.1));
        emitter.reinitialize(0, EmitterProperties { max_particles: 8, ..quiet });
        assert_eq!(emitter.emitters()[0].particles.len(), 8);
        assert_eq!(emitter.emitters()[0].live_particles(), 0);
    }

    #[test]
    fn size_scale_interpolates_keys() {
        let mut p = props();
        p.use_size_scale = true;
        assert_eq!(size_scale(&p, 1.0, 2.0), 0.5);
        p.use_regular_size_scale = false;
        p.size_scale = vec![SizeKey { time: 0.5, size: 3.0 }, SizeKey { time: 1.0, size: 5.0 }];
        assert!((size_scale(&p, 0.5, 2.0) - 2.0).abs() < 1e-6);
        assert!((size_scale(&p, 1.5, 2.0) - 4.0).abs() < 1e-6);
        p.size_scale_repeats = 1.0;
        assert!((size_scale(&p, 1.25, 2.0) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn color_scale_and_fades() {
        let mut p = props();
        p.use_color_scale = true;
        p.color_scale = vec![ColorKey { time: 0.0, color: [255, 0, 0, 255] }, ColorKey { time: 1.0, color: [0, 0, 255, 0] }];
        assert_eq!(particle_color(&p, &particle(1.0, 2.0)), [127, 0, 127, 255]);
        p.draw_style = DrawStyle::AlphaBlend;
        assert_eq!(particle_color(&p, &particle(1.0, 2.0)), [127, 0, 127, 127]);
        let mut p = props();
        p.fade_out = true;
        p.fade_out_start_time = 1.0;
        assert_eq!(particle_color(&p, &particle(1.5, 2.0)), [127, 127, 127, 127]);
        p.draw_style = DrawStyle::AlphaBlend;
        assert_eq!(particle_color(&p, &particle(1.5, 2.0)), [255, 255, 255, 127]);
        let mut p = props();
        p.fade_in = true;
        p.fade_in_end_time = 1.0;
        assert_eq!(particle_color(&p, &particle(0.25, 2.0)), [63, 63, 63, 63]);
    }

    #[test]
    fn initial_spawning_fills_over_one_lifetime() {
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![props()], 1);
        let mut counts = Vec::new();
        for _ in 0..30 {
            emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
            counts.push(emitter.emitters()[0].live_particles());
        }
        assert_eq!(counts[0], 0);
        assert_eq!(counts[1], 1);
        assert!((4..=5).contains(&counts[9]), "{counts:?}");
        assert!((9..=10).contains(&counts[19]), "{counts:?}");
        assert_eq!(counts[29], 10);
    }

    #[test]
    fn rate_spawning_and_dying() {
        let mut p = props();
        p.automatic_initial_spawning = false;
        p.respawn_dead_particles = false;
        p.initial_particles_per_second = 4.0;
        p.particles_per_second = 4.0;
        p.max_particles = 100;
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
        for _ in 0..50 {
            emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
        }
        let live = emitter.emitters()[0].live_particles();
        assert!((8..=9).contains(&live), "{live}");
    }

    #[test]
    fn steady_stream_at_60_hz_is_evenly_spaced_every_frame() {
        let mut p = props();
        p.automatic_initial_spawning = false;
        p.respawn_dead_particles = false;
        p.initial_particles_per_second = 30.0;
        p.particles_per_second = 30.0;
        p.max_particles = 100;
        p.start_velocity_range.x = Range::new(100.0, 100.0);
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
        let spacing = 100.0 / 30.0;
        for frame in 0..180 {
            emitter.tick(1.0 / 60.0, [0.0; 3].into(), [0; 3].into());
            let mut xs: Vec<f32> = emitter.sprites(0, View::default()).map(|s| s.location[0]).collect();
            xs.sort_by(f32::total_cmp);
            if frame > 2 {
                assert!(xs[0] < spacing + 1e-3, "frame {frame}: youngest at {}", xs[0]);
            }
            for pair in xs.windows(2) {
                assert!((pair[1] - pair[0] - spacing).abs() < 1e-2, "frame {frame}: {xs:?}");
            }
        }
    }

    #[test]
    fn particles_move_by_each_frames_own_step() {
        let mut p = props();
        p.automatic_initial_spawning = false;
        p.respawn_dead_particles = false;
        p.initial_particles_per_second = 30.0;
        p.particles_per_second = 30.0;
        p.max_particles = 100;
        p.start_velocity_range.x = Range::new(100.0, 100.0);
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
        let steps = [1.0 / 60.0, 1.0 / 30.0, 0.011, 0.05, 1.0 / 144.0, 0.1, 0.004, 1.0 / 60.0];
        let spacing = 100.0 / 30.0;
        let mut before: Vec<(bool, f32, f32)> = Vec::new();
        for frame in 0..160 {
            let dt = steps[frame % steps.len()];
            emitter.tick(dt, [0.0; 3].into(), [0; 3].into());
            let now: Vec<(bool, f32, f32)> = emitter.emitters()[0].particles.iter().map(|p| (p.active, p.time, p.location[0])).collect();
            for (slot, (&(was, time, x), &(is, new_time, new_x))) in before.iter().zip(&now).enumerate() {
                if was && is && (new_time - time - dt).abs() < 1e-5 {
                    assert!((new_x - x - 100.0 * dt).abs() < 1e-3, "frame {frame} slot {slot}: moved {} for step {dt}", new_x - x);
                }
            }
            let mut xs: Vec<f32> = emitter.sprites(0, View::default()).map(|s| s.location[0]).collect();
            xs.sort_by(f32::total_cmp);
            for pair in xs.windows(2) {
                assert!((pair[1] - pair[0] - spacing).abs() < 1e-2, "frame {frame}: {xs:?}");
            }
            before = now;
        }
    }

    #[test]
    fn auto_destroy_after_burst() {
        let mut p = props();
        p.respawn_dead_particles = false;
        p.max_particles = 3;
        let settings = EmitterSettings { auto_destroy: true, ..EmitterSettings::default() };
        let mut emitter = Emitter::new(settings, vec![p], 1);
        let mut ticks = 0;
        while !emitter.destroyed() && ticks < 100 {
            emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
            ticks += 1;
        }
        assert!(emitter.destroyed());
        assert!((25..=45).contains(&ticks), "{ticks}");
    }

    #[test]
    fn actor_rotation_applies_before_rotation_offset() {
        let p = EmitterProperties {
            start_location_offset: [10.0, 0.0, 0.0].into(),
            use_rotation_from: RotationSource::Actor,
            rotation_offset: [0x4000, 0, 0].into(),
            ..props()
        };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
        emitter.tick(0.5, [0.0; 3].into(), [0, 0x4000, 0].into());
        let e = &emitter.emitters()[0];
        assert!(e.active_particles > 0);
        for p in e.particles.iter().filter(|p| p.active) {
            let expected = Vector::from([0.0, 10.0, 0.0]);
            assert!((p.location - expected).dot(p.location - expected) < 1e-3, "{:?}", p.location);
        }
    }

    #[test]
    fn independent_particles_spawn_at_owner_and_move() {
        let mut p = props();
        p.start_velocity_range = RangeVector::splat(10.0, 10.0);
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
        emitter.tick(0.5, [100.0, 0.0, 0.0].into(), [0; 3].into());
        emitter.tick(0.5, [100.0, 0.0, 0.0].into(), [0; 3].into());
        for sprite in emitter.sprites(0, View::default()) {
            assert!(sprite.location[0] >= 100.0 && sprite.location[0] <= 110.0, "{sprite:?}");
            assert_eq!(sprite.size, [100.0, 100.0]);
        }
    }

    fn floor(start: Vector, end: Vector) -> Option<CollisionHit> {
        (start[2] >= 0.0 && end[2] < 0.0).then(|| {
            let t = start[2] / (start[2] - end[2]);
            CollisionHit { location: (start + ((end - start) * t)), normal: [0.0, 0.0, 1.0].into() }
        })
    }

    #[test]
    fn collisions_bounce_damp_and_spawn_in_the_other_emitter() {
        let falling = EmitterProperties {
            acceleration: [0.0, 0.0, -1000.0].into(),
            use_collision: true,
            use_max_collisions: true,
            max_collisions: Range::new(3.0, 3.0),
            spawn_from_other_emitter: Some(1),
            spawn_amount: 1,
            damping_factor_range: RangeVector::splat(0.5, 0.5),
            max_particles: 1,
            respawn_dead_particles: false,
            automatic_initial_spawning: false,
            initial_particles_per_second: 1000.0,
            lifetime_range: Range::new(10.0, 10.0),
            ..drawn()
        };
        let lying = EmitterProperties {
            max_particles: 5,
            respawn_dead_particles: false,
            automatic_initial_spawning: false,
            lifetime_range: Range::new(30.0, 30.0),
            ..drawn()
        };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![falling, lying], 1);
        let owner = Owner { location: [0.0, 0.0, 100.0].into(), ..Owner::default() };
        let mut lowest = f32::MAX;
        let mut bounces = Vec::new();
        for frame in 0..300 {
            emitter.tick_colliding(1.0 / 60.0, owner, &mut floor);
            for p in emitter.emitters()[0].particles.iter().filter(|p| p.active) {
                lowest = lowest.min(p.location[2]);
                if bounces.last().is_none_or(|&(_, hits)| hits != p.hit_count) {
                    bounces.push((frame, p.hit_count));
                }
            }
        }
        assert!(lowest >= 0.0, "fell through the floor: {lowest}");
        assert_eq!(bounces.iter().map(|&(_, hits)| hits).collect::<Vec<_>>(), [0, 1, 2], "{bounces:?}");
        assert_eq!(emitter.emitters()[0].live_particles(), 0);
        let lying: Vec<Vector> = emitter.emitters()[1].particles.iter().filter(|p| p.active).map(|p| p.location).collect();
        assert_eq!(lying.len(), 3, "{lying:?}");
        assert!(lying.iter().all(|l| (l[2] - SURFACE_OFFSET).abs() < 1e-3), "{lying:?}");
    }

    #[test]
    fn shatter_glass_is_one_burst() {
        let range = |x: (f32, f32), y: (f32, f32), z: (f32, f32)| RangeVector {
            x: Range::new(x.0, x.1),
            y: Range::new(y.0, y.1),
            z: Range::new(z.0, z.1),
        };
        let falling = EmitterProperties {
            acceleration: [0.0, 0.0, -3000.0].into(),
            use_collision: true,
            use_max_collisions: true,
            max_collisions: Range::new(3.0, 3.0),
            spawn_from_other_emitter: Some(1),
            spawn_amount: 1,
            damping_factor_range: range((0.5, 0.9), (0.5, 0.9), (0.4, 0.7)),
            max_particles: 15,
            respawn_dead_particles: false,
            start_location_range: range((0.0, 0.0), (-100.0, 100.0), (-100.0, 100.0)),
            initial_particles_per_second: 5000.0,
            automatic_initial_spawning: false,
            lifetime_range: Range::new(6.0, 6.0),
            start_velocity_range: range((25.0, 250.0), (-30.0, 30.0), (-100.0, 50.0)),
            ..drawn()
        };
        let lying = EmitterProperties { max_particles: 15, respawn_dead_particles: false, lifetime_range: Range::new(30.0, 30.0), ..drawn() };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![falling, lying], 7);
        let owner = Owner { location: [0.0, 0.0, 100.0].into(), ..Owner::default() };
        let mut falling_spawned = 0;
        let mut last_falling = 0;
        for frame in 0..3600 {
            emitter.tick_colliding(1.0 / 60.0, owner, &mut floor);
            let [falling, lying] = [0, 1].map(|i| emitter.emitters()[i].live_particles());
            assert!(falling <= 15 && lying <= 15, "frame {frame}: {falling} falling, {lying} lying");
            if frame == 0 {
                falling_spawned = falling;
            }
            if falling > 0 {
                last_falling = frame;
            }
        }
        assert_eq!(falling_spawned, 15, "one burst");
        assert!(last_falling < 3 * 60, "falling shards still alive at frame {last_falling}");
        assert_eq!(emitter.emitters()[0].live_particles() + emitter.emitters()[1].live_particles(), 0);
    }

    #[test]
    fn particles_slower_than_min_squared_velocity_come_to_rest() {
        let p = EmitterProperties {
            acceleration: [0.0, 0.0, -1000.0].into(),
            use_collision: true,
            damping_factor_range: RangeVector::splat(0.2, 0.2),
            min_squared_velocity: 2500.0,
            max_particles: 1,
            respawn_dead_particles: false,
            automatic_initial_spawning: false,
            initial_particles_per_second: 1000.0,
            lifetime_range: Range::new(10.0, 10.0),
            ..drawn()
        };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
        let owner = Owner { location: [0.0, 0.0, 50.0].into(), ..Owner::default() };
        for _ in 0..240 {
            emitter.tick_colliding(1.0 / 60.0, owner, &mut floor);
        }
        let particle = emitter.emitters()[0].particles[0];
        assert!(particle.stopped && particle.active, "{particle:?}");
        assert!(particle.location[2] >= 0.0 && particle.location[2] < 0.1, "{particle:?}");
    }

    #[test]
    fn sprite_axes_follow_direction_usage() {
        let view = View { location: [-500.0, 0.0, 0.0].into(), right: [0.0, 1.0, 0.0].into(), up: [0.0, 0.0, 1.0].into() };
        let sized = |usage: DirectionUsage| EmitterProperties {
            use_direction_as: usage,
            start_size_range: RangeVector { x: Range::new(10.0, 10.0), y: Range::new(20.0, 20.0), z: Range::new(1.0, 1.0) },
            start_velocity_range: RangeVector { x: Range::new(0.0, 0.0), y: Range::new(0.0, 0.0), z: Range::new(100.0, 100.0) },
            ..props()
        };
        let axes = |usage: DirectionUsage| {
            let mut emitter = Emitter::new(EmitterSettings::default(), vec![sized(usage)], 1);
            for _ in 0..10 {
                emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
            }
            let sprite = emitter.sprites(0, view).next().expect("a sprite");
            (sprite.up, sprite.right)
        };
        let close = |a: Vector, b: Vector| (0..3).all(|i| (a[i] - b[i]).abs() < 1e-3);
        let (up, right) = axes(DirectionUsage::None);
        assert!(close(up, [0.0, 0.0, -10.0].into()) && close(right, [0.0, 10.0, 0.0].into()), "{up:?} {right:?}");
        let (up, right) = axes(DirectionUsage::Up);
        assert!(close(up, [0.0, 0.0, 20.0].into()) && right[2].abs() < 1e-3 && (right.dot(right).sqrt() - 10.0).abs() < 1e-3, "{up:?} {right:?}");
        let (up, right) = axes(DirectionUsage::Normal);
        assert!(up[2].abs() < 1e-6 && right[2].abs() < 1e-6, "{up:?} {right:?}");
        assert!((up.dot(up).sqrt() - 20.0).abs() < 1e-3 && (right.dot(right).sqrt() - 10.0).abs() < 1e-3);
    }

    #[test]
    fn mesh_particles_spin_and_scale_per_axis() {
        let fixed = |v: Vector| RangeVector { x: Range::new(v[0], v[0]), y: Range::new(v[1], v[1]), z: Range::new(v[2], v[2]) };
        let mesh = EmitterProperties {
            kind: EmitterKind::Mesh,
            spin_particles: true,
            start_spin_range: fixed([0.25, 0.0, 0.5].into()),
            start_size_range: fixed([2.0, 3.0, 4.0].into()),
            ..props()
        };
        let first = |props: EmitterProperties, rotation: Rotator| {
            let mut emitter = Emitter::new(EmitterSettings::default(), vec![props], 1);
            for _ in 0..10 {
                emitter.tick(0.1, [0.0; 3].into(), rotation);
            }
            emitter.meshes(0).next().expect("a mesh particle")
        };
        let m = first(mesh.clone(), [0; 3].into());
        let near = |a: i32, b: i32| (a - b).abs() <= 1;
        assert!(near(m.spin[0], 0) && near(m.spin[1], 16384) && near(m.spin[2], 32768), "{:?}", m.spin);
        assert_eq!(m.size, [2.0, 3.0, 4.0].into());
        assert_eq!(m.frame, [0; 3].into());
        let m = first(EmitterProperties { spin_particles: false, uniform_size: true, ..mesh.clone() }, [0; 3].into());
        assert_eq!((m.spin, m.size), (Rotator::ZERO, Vector::new(2.0, 2.0, 2.0)));
        let m = first(EmitterProperties { coordinate_system: CoordinateSystem::Relative, ..mesh }, [0, 16384, 0].into());
        assert_eq!(m.frame, [0, 16384, 0].into());
    }

    #[test]
    fn subdivisions_follow_age() {
        let mut p = props();
        p.texture_u_subdivisions = 2;
        p.texture_v_subdivisions = 2;
        let (uv, blend) = subdivision(&p, &particle(1.0, 2.0));
        assert_eq!(uv, [0.5, 0.0, 1.0, 0.5]);
        assert_eq!(blend, None);
        p.blend_between_subdivisions = true;
        let (_, blend) = subdivision(&p, &particle(1.25, 2.0));
        assert_eq!(blend, Some(([0.5, 0.5, 1.0, 1.0], 127)));
    }

    #[test]
    fn undrawn_emitters_pause_after_seconds_before_inactive() {
        let p = EmitterProperties { seconds_before_inactive: 0.5, max_particles: 100, ..props() };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![EmitterProperties { initial_particles_per_second: 10.0, particles_per_second: 10.0, ..p }], 1);
        for _ in 0..10 {
            emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
        }
        let paused = emitter.emitters()[0].live_particles();
        for _ in 0..10 {
            emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
        }
        assert_eq!(emitter.emitters()[0].live_particles(), paused, "no updates while undrawn");
        emitter.emitters_mut()[0].mark_rendered();
        emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
        assert!(emitter.emitters()[0].live_particles() > paused, "drawn again: updating");
    }

    #[test]
    fn drag_along_actors_push_particles() {
        let p = EmitterProperties {
            use_actor_forces: true,
            max_particles: 1,
            respawn_dead_particles: false,
            lifetime_range: Range::new(10.0, 10.0),
            initial_particles_per_second: 1000.0,
            automatic_initial_spawning: false,
            ..props()
        };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
        emitter.tick(0.01, [0.0; 3].into(), [0; 3].into());
        let force = |x: f32| ActorForce { location: [x, 0.0, 0.0].into(), velocity: [100.0, 0.0, 0.0].into(), radius: 100.0, scale: 1.0 };
        let e = &mut emitter.emitters_mut()[0];
        let before = e.particles[0].velocity;
        e.particles[0].mass = 1.0;
        e.handle_actor_force(&force(-500.0), 0.1);
        assert_eq!(e.particles[0].velocity, before, "out of range");
        e.handle_actor_force(&force(-50.0), 0.1);
        assert!((e.particles[0].velocity[0] - before[0] - 2.5).abs() < 1e-3, "{:?}", e.particles[0].velocity);
    }
}
