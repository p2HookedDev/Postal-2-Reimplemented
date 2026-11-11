use rapier3d::prelude::*;

use crate::{
    BodyId, BodyParams, Contacts, Error, LEVEL_FRICTION, LEVEL_RESTITUTION, Shape, World, body_collider, body_pair, collider, from_rapier,
    level_contact, pose, to_rapier, vectors, with_material,
};

impl World {
    fn add_level(&mut self, collider: ColliderBuilder) {
        self.level.push(self.colliders.insert(with_material(collider, LEVEL_FRICTION, LEVEL_RESTITUTION)));
    }

    /// A triangle's front is the side its corners run counter-clockwise from.
    pub fn add_static_mesh(&mut self, vertices: &[[f32; 3]], triangles: Vec<[u32; 3]>) -> Result<(), Error> {
        let flags = TriMeshFlags::FIX_INTERNAL_EDGES_TWO_SIDED | TriMeshFlags::DELETE_DEGENERATE_TRIANGLES;
        let collider = ColliderBuilder::trimesh_with_flags(vectors(vertices), triangles, flags)
            .map_err(|e| Error::Mesh(e.to_string()))?
            .user_data(level_contact::ONE_SIDED)
            .active_hooks(ActiveHooks::MODIFY_SOLVER_CONTACTS);
        self.add_level(collider);
        Ok(())
    }

    pub fn add_static_hull(&mut self, points: &[[f32; 3]]) -> Result<(), Error> {
        let points = vectors(points);
        let collider = ColliderBuilder::convex_hull(&points).ok_or_else(|| Error::Mesh(format!("no convex hull of {} points", points.len())))?;
        self.add_level(collider);
        Ok(())
    }

    pub fn add_static_shape(&mut self, shape: &Shape, location: p2_math::Vector, rotation: [f32; 4]) -> Result<(), Error> {
        let Some(collider) = collider(shape) else { return Err(Error::Mesh("no convex hull".to_owned())) };
        self.add_level(collider.position(pose(location, rotation)));
        Ok(())
    }

    pub fn add_body(&mut self, params: &BodyParams) -> BodyId {
        let mut body = RigidBodyBuilder::dynamic()
            .pose(pose(params.location, params.rotation))
            .linear_damping(params.linear_damping)
            .angular_damping(params.angular_damping)
            .ccd_enabled(true)
            .build();
        if params.stay_upright {
            body.set_enabled_rotations(false, false, params.allow_yaw, false);
        }
        if params.asleep {
            body.set_body_type(RigidBodyType::Fixed, false);
        }
        let handle = self.bodies.insert(body);
        if params.asleep {
            self.dormant.insert(handle);
        }
        let mut builders: Vec<ColliderBuilder> = params.shapes.iter().filter_map(collider).collect();
        if builders.is_empty() {
            builders.push(ColliderBuilder::ball(1.0));
        }
        let colliders: Vec<Collider> = builders.into_iter().map(|builder| body_collider(builder, params.friction, params.restitution).build()).collect();
        let volume: f32 = colliders.iter().map(Collider::volume).sum();
        for mut collider in colliders {
            if params.mass > 0.0 && volume > 0.0 {
                collider.set_mass(params.mass * collider.volume() / volume);
            }
            self.colliders.insert_with_parent(collider, handle, &mut self.bodies);
        }
        // Rapier applies the mass at the next step; until then an impulse would move nothing.
        if let Some(body) = self.bodies.get_mut(handle) {
            body.recompute_mass_properties_from_colliders(&self.colliders);
        }
        BodyId(handle)
    }

    /// Kinematic: pushes and carries bodies, never pushed back. Even ragdolls touching only the level touch it.
    pub fn add_mover(&mut self, shapes: &[Shape], location: p2_math::Vector, rotation: [f32; 4]) -> Result<BodyId, Error> {
        let builders: Vec<ColliderBuilder> = shapes.iter().filter_map(collider).collect();
        if builders.is_empty() {
            return Err(Error::Mesh("no mover shape".to_owned()));
        }
        let handle = self.bodies.insert(RigidBodyBuilder::kinematic_position_based().pose(pose(location, rotation)).build());
        for builder in builders {
            self.colliders.insert_with_parent(with_material(builder, LEVEL_FRICTION, LEVEL_RESTITUTION), handle, &mut self.bodies);
        }
        self.hooks.movers.insert(handle);
        Ok(BodyId(handle))
    }

    pub fn move_mover(&mut self, id: BodyId, location: p2_math::Vector, rotation: [f32; 4]) {
        if self.hooks.movers.contains(&id.0) {
            self.moving.insert(id.0, pose(location, rotation));
        }
    }

    pub fn remove_body(&mut self, id: BodyId) {
        self.hooks.disabled.retain(|&(a, b)| a != id.0 && b != id.0);
        self.hooks.level_only.remove(&id.0);
        self.hooks.movers.remove(&id.0);
        self.moving.remove(&id.0);
        self.take_out_body(id);
    }

    pub(crate) fn take_out_body(&mut self, id: BodyId) {
        self.dormant.remove(&id.0);
        self.bodies.remove(id.0, &mut self.islands, &mut self.colliders, &mut self.impulse_joints, &mut self.multibody_joints, &mut self.soft_bodies, true);
    }

    pub fn apply_impulse(&mut self, id: BodyId, impulse: p2_math::Vector, point: Option<p2_math::Vector>) {
        self.push(id, to_rapier(impulse), point.map(to_rapier));
    }

    pub fn add_force(&mut self, id: BodyId, force: p2_math::Vector, point: Option<p2_math::Vector>) {
        self.wake(id);
        let Some(body) = self.bodies.get_mut(id.0) else { return };
        self.forced.push(id.0);
        match point {
            Some(point) => body.add_force_at_point(to_rapier(force), to_rapier(point), true),
            None => body.add_force(to_rapier(force), true),
        }
    }

    pub fn add_torque(&mut self, id: BodyId, torque: p2_math::Vector) {
        self.wake(id);
        if let Some(body) = self.bodies.get_mut(id.0) {
            self.forced.push(id.0);
            body.add_torque(to_rapier(torque), true);
        }
    }

    pub fn mass(&self, id: BodyId) -> Option<f32> {
        Some(self.bodies.get(id.0)?.mass())
    }

    pub fn set_mass(&mut self, id: BodyId, mass: f32) {
        let Some(body) = self.bodies.get(id.0) else { return };
        let old = body.mass();
        if old <= 0.0 || mass <= 0.0 {
            return;
        }
        for &handle in body.colliders() {
            if let Some(collider) = self.colliders.get_mut(handle) {
                collider.set_mass(collider.mass() * mass / old);
            }
        }
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.recompute_mass_properties_from_colliders(&self.colliders);
        }
    }

    pub fn set_friction(&mut self, id: BodyId, friction: f32) {
        self.each_collider(id, |c| c.set_friction(friction));
    }

    pub fn set_restitution(&mut self, id: BodyId, restitution: f32) {
        self.each_collider(id, |c| c.set_restitution(restitution));
    }

    pub fn set_damping(&mut self, id: BodyId, linear: f32, angular: f32) {
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.set_linear_damping(linear);
            body.set_angular_damping(angular);
        }
    }

    pub fn set_gravity_scale(&mut self, id: BodyId, scale: f32) {
        if self.bodies.get(id.0).is_some_and(|body| body.gravity_scale() != scale)
            && let Some(body) = self.bodies.get_mut(id.0)
        {
            body.set_gravity_scale(scale, false);
        }
    }

    pub fn set_stay_upright(&mut self, id: BodyId, upright: bool, allow_yaw: bool) {
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.set_enabled_rotations(!upright, !upright, !upright || allow_yaw, true);
        }
    }

    pub fn set_pair_collision(&mut self, a: BodyId, b: BodyId, enabled: bool) {
        let pair = body_pair(a.0, b.0);
        if enabled {
            self.hooks.disabled.remove(&pair);
        } else {
            self.hooks.disabled.insert(pair);
        }
    }

    pub fn set_contacts(&mut self, id: BodyId, contacts: Contacts) {
        match contacts {
            Contacts::All => self.hooks.level_only.remove(&id.0),
            Contacts::LevelOnly => self.hooks.level_only.insert(id.0),
        };
    }

    pub fn set_pose(&mut self, id: BodyId, location: p2_math::Vector, rotation: [f32; 4]) {
        self.before.remove(&id.0);
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.set_translation(to_rapier(location), true);
            body.set_rotation(Rotation::from_array(rotation).normalize(), true);
        }
    }

    pub fn set_velocity(&mut self, id: BodyId, velocity: p2_math::Vector) {
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.set_linvel(to_rapier(velocity), true);
        }
    }

    pub fn set_spin(&mut self, id: BodyId, spin: p2_math::Vector) {
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.set_angvel(to_rapier(spin), true);
        }
    }

    pub fn gravity(&self) -> p2_math::Vector {
        from_rapier(self.gravity)
    }

    pub fn limit_speed(&mut self, id: BodyId, max: f32) {
        let Some(body) = self.bodies.get_mut(id.0) else { return };
        let velocity = body.linvel();
        let squared = velocity.length_squared();
        if squared > max * max {
            body.set_linvel(velocity * (max / squared.sqrt()), false);
        }
    }

    fn each_collider(&mut self, id: BodyId, mut change: impl FnMut(&mut Collider)) {
        let Some(body) = self.bodies.get(id.0) else { return };
        for &handle in body.colliders() {
            if let Some(collider) = self.colliders.get_mut(handle) {
                change(collider);
            }
        }
    }
}
