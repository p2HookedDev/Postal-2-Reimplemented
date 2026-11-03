//! How a corpse takes a hit (our choice, not retail's): a script's impulse moves the whole corpse as one rigid
//! body (with spin), a damage type with a [`Throw`] pushes the corpse along the shot up to a ceiling speed, and the
//! part the shot entered gets a damage-sized kick that folds the corpse around it. Units are Unreal units.

use rapier3d::prelude::*;

use crate::{BodyId, World, from_rapier, to_rapier};

const KICK_PER_DAMAGE: f32 = 17.5;
const KICK: [f32; 2] = [150.0, 500.0];
const KICK_ROOM: f32 = 700.0;
const RAY_HALF: f32 = 20.0;
const NEAR: f32 = 128.0;
const FAR: f32 = 1024.0;
const FAR_SHARE: f32 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Throw {
    pub per_damage: f32,
    pub ceiling: f32,
    pub lift: f32,
    pub falloff: bool,
    pub tumble: bool,
}

struct PartMass {
    id: BodyId,
    mass: f32,
    center: Vector,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub impulse: p2_math::Vector,
    pub body: Option<BodyId>,
    pub damage: f32,
    pub throw: Option<Throw>,
    pub range: Option<f32>,
    pub at: p2_math::Vector,
    pub dir: p2_math::Vector,
}

impl World {
    pub fn ragdoll_impulse(&mut self, bodies: &[BodyId], impulse: p2_math::Vector, at_body: Option<BodyId>) {
        let parts = self.masses(bodies);
        let total: f32 = parts.iter().map(|p| p.mass).sum();
        if total <= 0.0 {
            return;
        }
        let centre = parts.iter().map(|p| p.center * p.mass).sum::<Vector>() / total;
        let inertia: f32 = parts.iter().map(|p| p.mass * (p.center - centre).length_squared()).sum();
        let j = to_rapier(impulse);
        let lever = at_body.and_then(|b| parts.iter().find(|p| p.id == b)).map_or(Vector::ZERO, |p| p.center - centre);
        let spin = if inertia > 1e-6 { lever.cross(j) / inertia } else { Vector::ZERO };
        for p in &parts {
            self.push(p.id, (j / total + spin.cross(p.center - centre)) * p.mass, None);
        }
    }

    pub fn ragdoll_hit(&mut self, bodies: &[BodyId], hit: &Hit) {
        let parts = self.masses(bodies);
        let total: f32 = parts.iter().map(|p| p.mass).sum();
        if total <= 0.0 {
            return;
        }
        let j = to_rapier(hit.impulse);
        let size = hit.throw.map_or(0.0, |t| hit_size(hit, &t));
        let stock = hit.throw.is_none() || j.length() > size * total;
        let whole = match hit.throw {
            Some(throw) if !stock => self.throw_velocity(&parts, hit, &throw, size),
            _ => j / total,
        };
        if hit.throw.is_some_and(|t| t.tumble) {
            self.ragdoll_impulse(bodies, from_rapier(whole * total), hit.body);
            return;
        }
        let dir = to_rapier(hit.dir);
        let Some((entry, point)) = self.entry(&parts, to_rapier(hit.at), dir) else { return };
        let entry_mass = parts[entry].mass;
        let entry_velocity = self.bodies.get(parts[entry].id.0).map_or(Vector::ZERO, RigidBody::linvel);
        let room = KICK_ROOM - (entry_velocity + whole).length();
        let mut kick = (hit.damage * KICK_PER_DAMAGE).clamp(KICK[0], KICK[1]).min(room.max(0.0));
        let mut aim = dir;
        if hit.throw.is_none() {
            kick = kick.min(j.length() / entry_mass);
            aim = j.normalize_or_zero();
        } else if dir.length_squared() < 0.5 {
            aim = whole.normalize_or_zero();
        }
        let entry_impulse = aim * kick * entry_mass;
        let each = if hit.throw.is_none() { (j - entry_impulse) / total } else { whole };
        self.push(parts[entry].id, entry_impulse, Some(point));
        for p in &parts {
            self.push(p.id, each * p.mass, None);
        }
    }

    fn masses(&self, bodies: &[BodyId]) -> Vec<PartMass> {
        bodies.iter().filter_map(|&id| self.bodies.get(id.0).map(|b| PartMass { id, mass: b.mass(), center: b.center_of_mass() })).collect()
    }

    pub(crate) fn push(&mut self, id: BodyId, impulse: Vector, at: Option<Vector>) {
        self.wake(id);
        let Some(body) = self.bodies.get_mut(id.0) else { return };
        match at {
            Some(point) => body.apply_impulse_at_point(impulse, point, true),
            None => body.apply_impulse(impulse, true),
        }
    }

    fn throw_velocity(&self, parts: &[PartMass], hit: &Hit, throw: &Throw, size: f32) -> Vector {
        let mean = parts.iter().filter_map(|p| self.bodies.get(p.id.0).map(RigidBody::linvel)).sum::<Vector>() / parts.len() as f32;
        let dir = to_rapier(hit.dir);
        let flat = Vector::new(dir.x, dir.y, 0.0);
        let level = flat.length_squared() > 0.01;
        let heading = if level { flat.normalize() } else { dir };
        let along = size.min((throw.ceiling - mean.dot(heading)).max(0.0));
        let up = if level { (along * throw.lift).min((throw.ceiling * throw.lift - mean.z).max(0.0)) } else { 0.0 };
        heading * along + Vector::new(0.0, 0.0, up)
    }

    fn entry(&self, parts: &[PartMass], at: Vector, dir: Vector) -> Option<(usize, Vector)> {
        use rapier3d::parry::query::Ray;
        if dir.length_squared() > 0.5 {
            let ray = Ray::new(at - dir * RAY_HALF, dir * (2.0 * RAY_HALF));
            let crossed = parts
                .iter()
                .enumerate()
                .filter_map(|(i, p)| Some((i, self.bodies.get(p.id.0)?)))
                .flat_map(|(i, body)| body.colliders().iter().map(move |&c| (i, c)))
                .filter_map(|(i, c)| {
                    let collider = self.colliders.get(c)?;
                    Some((i, collider.shape().cast_ray(collider.position(), &ray, 1.0, true)?))
                })
                .min_by(|a, b| (a.1 - 0.5).abs().total_cmp(&(b.1 - 0.5).abs()));
            if let Some((i, time)) = crossed {
                return Some((i, ray.point_at(time)));
            }
        }
        let distance = |p: &PartMass| (p.center - at).length_squared();
        let nearest = (0..parts.len()).min_by(|&a, &b| distance(&parts[a]).total_cmp(&distance(&parts[b])))?;
        Some((nearest, parts[nearest].center))
    }
}

fn hit_size(hit: &Hit, throw: &Throw) -> f32 {
    let share = match hit.range {
        Some(range) if throw.falloff && range > NEAR => 1.0 - (1.0 - FAR_SHARE) * ((range - NEAR) / (FAR - NEAR)).min(1.0),
        _ => 1.0,
    };
    (throw.per_damage * hit.damage * share).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JointLimit, JointParams, PartParams, RagdollParams, Shape};

    fn corpse(world: &mut World) -> Vec<BodyId> {
        let part = |x: f32| PartParams {
            shapes: vec![Shape::Capsule([0.0; 3].into(), [40.0, 0.0, 0.0].into(), 5.0)],
            location: [x, 0.0, 0.0].into(),
            rotation: [0.0, 0.0, 0.0, 1.0],
            mass: 0.1,
            center_of_mass: [20.0, 0.0, 0.0].into(),
            inertia: [2.0, 10.0, 10.0].into(),
            velocity: [0.0; 3].into(),
        };
        let joint = |i: usize| JointParams {
            child: i,
            parent: i - 1,
            child_anchor: [0.0; 3].into(),
            child_axes: [[1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into()],
            parent_anchor: [40.0, 0.0, 0.0].into(),
            parent_axes: [[1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into()],
            limit: JointLimit::Cone { swing: Some([0.7, 0.7]), twist: None },
        };
        world.add_ragdoll(&RagdollParams {
            parts: vec![part(0.0), part(40.0), part(80.0)],
            joints: vec![joint(1), joint(2)],
            linear_damping: 0.0,
            angular_damping: 0.0,
            friction: 0.3,
            restitution: 0.0,
            asleep: false,
            safe_point: [0.0; 3].into(),
        })
    }

    fn momentum(world: &World, bodies: &[BodyId]) -> Vector {
        bodies.iter().map(|&b| Vector::from_array(world.velocity(b).unwrap().into()) * world.mass(b).unwrap()).sum()
    }

    #[test]
    fn a_script_impulse_moves_the_corpse_as_one_body() {
        let mut world = World::new([0.0; 3].into());
        let bodies = corpse(&mut world);
        world.ragdoll_impulse(&bodies, [0.0, 3.0, 0.0].into(), Some(bodies[1]));
        for &b in &bodies {
            assert!((world.velocity(b).unwrap()[1] - 10.0).abs() < 1e-3, "{:?}", world.velocity(b));
        }
        let mut world = World::new([0.0; 3].into());
        let bodies = corpse(&mut world);
        world.ragdoll_impulse(&bodies, [0.0, 3.0, 0.0].into(), Some(bodies[2]));
        assert!((momentum(&world, &bodies).y - 3.0).abs() < 1e-3);
        assert!(world.velocity(bodies[2]).unwrap()[1] > 10.0 && world.velocity(bodies[0]).unwrap()[1] < 10.0);
    }

    #[test]
    fn a_shotgun_throw_reaches_its_speed_and_kicks_the_entry() {
        let mut world = World::new([0.0; 3].into());
        let bodies = corpse(&mut world);
        let shotgun = Throw { per_damage: 11.0, ceiling: 650.0, lift: 0.3, falloff: true, tumble: false };
        let hit = Hit {
            impulse: [0.0; 3].into(),
            body: None,
            damage: 30.0,
            throw: Some(shotgun),
            range: Some(100.0),
            at: [60.0, 0.0, 0.0].into(),
            dir: [0.0, 1.0, 0.0].into(),
        };
        world.ragdoll_hit(&bodies, &hit);
        let p2_math::Vector { y: y0, z: z0, .. } = world.velocity(bodies[0]).unwrap();
        assert!((y0 - 330.0).abs() < 1.0 && (z0 - 99.0).abs() < 1.0, "{y0} {z0}");
        let entry = world.velocity(bodies[1]).unwrap()[1];
        let room = 700.0 - (330.0f32.powi(2) + 99.0f32.powi(2)).sqrt();
        assert!((entry - (330.0 + room)).abs() < 1.0, "entry {entry}");
    }

    #[test]
    fn the_throw_stops_at_its_ceiling_and_the_script_wins_when_stronger() {
        let mut world = World::new([0.0; 3].into());
        let bodies = corpse(&mut world);
        let bullet = Throw { per_damage: 6.0, ceiling: 260.0, lift: 0.0, falloff: false, tumble: true };
        let hit = Hit { impulse: [0.0; 3].into(), body: None, damage: 100.0, throw: Some(bullet), range: None, at: [60.0, 0.0, 0.0].into(), dir: [1.0, 0.0, 0.0].into() };
        world.ragdoll_hit(&bodies, &hit);
        assert!((world.velocity(bodies[0]).unwrap()[0] - 260.0).abs() < 1.0);
        let mut world = World::new([0.0; 3].into());
        let bodies = corpse(&mut world);
        world.ragdoll_hit(&bodies, &Hit { impulse: [300.0, 0.0, 0.0].into(), ..hit });
        assert!((momentum(&world, &bodies).x - 300.0).abs() < 1e-2);
    }
}
