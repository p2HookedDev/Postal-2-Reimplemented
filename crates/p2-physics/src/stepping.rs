use rapier3d::prelude::*;

use crate::{BodyId, Impact, MOVER_REACH, STEP, World, from_rapier, ragdoll, steps_owed};

impl World {
    pub fn wake(&mut self, id: BodyId) {
        self.wakes += 1;
        self.wake_body(id.0);
        for handle in self.dormant_pile(id.0) {
            self.wake_body(handle);
        }
    }

    fn wake_body(&mut self, handle: RigidBodyHandle) {
        let dormant = self.dormant.remove(&handle);
        if let Some(body) = self.bodies.get_mut(handle) {
            if dormant {
                body.set_body_type(RigidBodyType::Dynamic, false);
                body.recompute_mass_properties_from_colliders(&self.colliders);
            }
            body.wake_up(true);
        }
    }

    pub fn sleep(&mut self, id: BodyId) {
        if let Some(body) = self.bodies.get_mut(id.0) {
            body.set_linvel(Vector::ZERO, false);
            body.set_angvel(Vector::ZERO, false);
            body.set_body_type(RigidBodyType::Fixed, false);
            self.dormant.insert(id.0);
        }
    }

    pub fn step(&mut self, dt: f32) -> Vec<Impact> {
        let mut impacts = Vec::new();
        self.step_into(dt, &mut impacts);
        impacts
    }

    pub fn step_into(&mut self, dt: f32, impacts: &mut Vec<Impact>) {
        if dt <= 0.0 {
            return;
        }
        let (steps, owed) = steps_owed(self.owed + dt);
        self.owed = owed;
        if steps == 0 {
            return;
        }
        let step = STEP;
        self.parameters.dt = step;
        let moves: Vec<(RigidBodyHandle, Pose, Pose)> = self.moving.drain().filter_map(|(handle, to)| Some((handle, *self.bodies.get(handle)?.position(), to))).collect();
        self.wake_near_movers(&moves);
        for substep in 1..=steps {
            if substep == steps {
                self.before.clear();
                self.before.extend(self.bodies.iter().filter(|(_, body)| body.is_dynamic() && !body.is_sleeping()).map(|(handle, body)| (handle, *body.position())));
            }
            for &(handle, from, to) in &moves {
                if let Some(body) = self.bodies.get_mut(handle) {
                    body.set_next_kinematic_position(from.lerp(&to, substep as f32 / steps as f32));
                }
            }
            self.pipeline.step(
                self.gravity,
                &self.parameters,
                &mut self.islands,
                &mut self.broad_phase,
                &mut self.narrow_phase,
                &mut self.bodies,
                &mut self.colliders,
                &mut self.impulse_joints,
                &mut self.multibody_joints,
                &mut self.soft_bodies,
                &mut self.ccd,
                &self.hooks,
                &self.hooks,
            );
            self.hold_cones(step);
            self.settle_ragdolls();
            let mut started = std::mem::take(&mut self.started);
            if let Ok(mut events) = self.hooks.started.lock() {
                std::mem::swap(&mut *events, &mut started);
            }
            for s in started.drain(..) {
                let bodies = s.colliders.map(|c| self.colliders.get(c).and_then(Collider::parent));
                for (this, other) in [(0, 1), (1, 0)] {
                    let awake = bodies[other].and_then(|h| self.bodies.get(h)).is_some_and(|b| b.is_dynamic() && !b.is_sleeping());
                    let hard = (s.velocities[0] - s.velocities[1]).length() > ragdoll::SLEEP_SPEED;
                    if let Some(this) = bodies[this].filter(|h| awake && self.dormant.contains(h) && (hard || !self.is_ragdoll_part(*h))) {
                        self.wake(BodyId(this));
                    }
                }
                for (this, other, normal) in [(0, 1, -s.normal), (1, 0, s.normal)] {
                    let Some(body) = bodies[this].filter(|&h| self.bodies.get(h).is_some_and(RigidBody::is_dynamic)) else { continue };
                    impacts.push(Impact {
                        body: BodyId(body),
                        other: bodies[other].map(BodyId),
                        position: from_rapier(s.point),
                        velocity: from_rapier(s.velocities[this] - s.velocities[other]),
                        normal: from_rapier(normal),
                    });
                }
            }
            self.started = started;
        }
        for handle in self.forced.drain(..) {
            if let Some(body) = self.bodies.get_mut(handle) {
                body.reset_forces(false);
                body.reset_torques(false);
            }
        }
    }

    /// A kinematic body makes no contacts with dormant (fixed) bodies and may not wake sleeping ones.
    fn wake_near_movers(&mut self, moves: &[(RigidBodyHandle, Pose, Pose)]) {
        let colliders = &self.colliders;
        let reach: Vec<Aabb> = moves
            .iter()
            .filter_map(|&(handle, _, to)| {
                let swept = |c: &Collider| Some(c.compute_aabb().merged(&c.shape().compute_aabb(&(to * *c.position_wrt_parent()?))).loosened(MOVER_REACH));
                self.bodies.get(handle)?.colliders().iter().filter_map(|&c| colliders.get(c)).filter_map(swept).reduce(|a, b| a.merged(&b))
            })
            .collect();
        if reach.is_empty() {
            return;
        }
        let near: Vec<RigidBodyHandle> = self
            .bodies
            .iter()
            .filter(|&(handle, body)| self.dormant.contains(&handle) || (body.is_dynamic() && body.is_sleeping()))
            .filter(|(_, body)| body.colliders().iter().filter_map(|&c| colliders.get(c)).any(|c| reach.iter().any(|r| r.intersects(&c.compute_aabb()))))
            .map(|(handle, _)| handle)
            .collect();
        for handle in near {
            self.wake(BodyId(handle));
        }
    }
}
