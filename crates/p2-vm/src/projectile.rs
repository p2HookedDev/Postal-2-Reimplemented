use p2_math::{Rotator, Vector};

use crate::enums::{EDrawType, EPhysics};
use crate::physics::{Bases, MAX_ITERATIONS, MIN_FLOOR_Z, MIN_LANDING_SECONDS, MIN_LANDING_TIME, MoveHit, VolumeProps, nearly_zero, substep, two_wall_adjust};
use crate::{Encroachment, Error, LoaderRef, ObjectId, Value, Vm, Prop, Kind};

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn actor_physics(&mut self, actor: ObjectId, delta: f32) -> Result<(), Error> {
        match self.physics_of(actor) {
            Some(EPhysics::Projectile) if self.karma.limbs.drives(actor) => Ok(()),
            Some(EPhysics::Projectile) => self.phys_projectile(actor, delta),
            Some(EPhysics::Falling) => self.actor_phys_falling(actor, delta, 0),
            Some(EPhysics::Trailer) => self.phys_trailer(actor),
            Some(EPhysics::RootMotion) => {
                self.phys_root_motion(actor);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// After the last move retail calls one more unidentified function, not done here (unverified).
    fn phys_trailer(&mut self, actor: ObjectId) -> Result<(), Error> {
        let Some(owner) = self.object_of(actor, Prop::Owner) else { return Ok(()) };
        if self.object_of(actor, Prop::Base).is_some() {
            return Ok(());
        }
        let (draw_type, same_rotation) = (self.draw_type_of(actor), self.flag_of(actor, Prop::bTrailerSameRotation));
        let owner_location = self.vector(owner, Prop::Location);
        let behind = owner_location - (self.rotator_of(owner, Prop::Rotation).vector() * self.float_of(actor, Prop::Mass));
        if draw_type == Some(EDrawType::Sprite) {
            let to = if self.flag_of(actor, Prop::bTrailerPrePivot) {
                owner_location + self.vector(actor, Prop::PrePivot)
            } else if same_rotation {
                behind
            } else {
                owner_location
            };
            return self.place_far_moved(actor, to, Encroachment::Ignored);
        }
        if draw_type == Some(EDrawType::Particle) && same_rotation {
            return self.place_far_moved(actor, behind, Encroachment::Ignored);
        }
        self.place_far_moved(actor, owner_location, Encroachment::Ignored)?;
        let rotation = if same_rotation {
            self.rotator_of(owner, Prop::Rotation)
        } else {
            let velocity = self.vector(owner, Prop::Velocity);
            if nearly_zero(velocity) { Rotator::new(16384, 0, 0) } else { (velocity * (-1.0)).rotation() }
        };
        self.set(actor, Prop::Rotation, Value::Rotator(rotation));
        Ok(())
    }

    fn below_kill_z(&mut self, actor: ObjectId) -> Result<bool, Error> {
        let kill_z = self.kill_z(actor, false).unwrap_or(f32::MIN);
        if self.vector(actor, Prop::Location)[2] < kill_z {
            self.event(actor, "FellOutOfWorld", Vec::new())?;
            return Ok(true);
        }
        Ok(false)
    }

    fn phys_projectile(&mut self, actor: ObjectId, delta: f32) -> Result<(), Error> {
        if self.below_kill_z(actor)? {
            return Ok(());
        }
        if self.out_of_world(actor) {
            self.destroy(actor)?;
            return Ok(());
        }
        let start = self.vector(actor, Prop::Location);
        self.set(actor, Prop::bJustTeleported, Value::Bool(false));
        let (mut remaining, mut bounces, mut iterations) = (delta, 0, 0);
        while remaining > 0.0 {
            iterations += 1;
            let acceleration = self.vector(actor, Prop::Acceleration);
            let mut velocity = self.vector(actor, Prop::Velocity) + (acceleration * remaining);
            let bound = if self.is_a(actor, Kind::Projectile) {
                Some(self.float_of(actor, Prop::MaxSpeed))
            } else {
                (!nearly_zero(acceleration)).then(|| acceleration.size())
            };
            if let Some(bound) = bound
                && velocity.size() > bound
            {
                velocity = velocity.safe_normal() * bound;
            }
            self.set_vector(actor, Prop::Velocity, velocity);
            let tick = remaining;
            remaining = 0.0;
            let hit = self.move_actor(actor, velocity * delta, Bases::Block)?;
            if hit.time < 1.0 && !self.is_deleted(actor) && !self.flag_of(actor, Prop::bJustTeleported) {
                self.event(actor, "HitWall", [Value::Vector(hit.normal), Value::Object(hit.actor)])?;
                if self.flag_of(actor, Prop::bBounce) {
                    if bounces < 2 {
                        remaining = (1.0 - hit.time) * tick;
                    }
                    bounces += 1;
                    if self.physics_of(actor) == Some(EPhysics::Falling) {
                        self.actor_phys_falling(actor, remaining, iterations)?;
                        return Ok(());
                    }
                }
            }
            if self.is_deleted(actor) || self.physics_of(actor) != Some(EPhysics::Projectile) {
                return Ok(());
            }
        }
        if !self.flag_of(actor, Prop::bBounce) && !self.flag_of(actor, Prop::bJustTeleported) && delta > 0.0 {
            let moved = self.vector(actor, Prop::Location) - start;
            self.set_vector(actor, Prop::Velocity, moved * (1.0 / delta));
        }
        Ok(())
    }

    fn actor_phys_falling(&mut self, actor: ObjectId, delta: f32, mut iterations: u32) -> Result<(), Error> {
        if self.below_kill_z(actor)? {
            return Ok(());
        }
        let VolumeProps { gravity, terminal_velocity: terminal, zone_velocity, .. } = self.volume(actor);
        let mut remaining = delta;
        let mut bounces = 0;
        while remaining > 0.0 && iterations < MAX_ITERATIONS {
            iterations += 1;
            let tick = substep(remaining);
            remaining -= tick;
            let old_location = self.vector(actor, Prop::Location);
            self.set(actor, Prop::bJustTeleported, Value::Bool(false));
            let mut old_velocity = self.vector(actor, Prop::Velocity);
            let acceleration = self.vector(actor, Prop::Acceleration);
            let velocity = old_velocity + ((acceleration + gravity) * (0.5 * tick));
            self.set_vector(actor, Prop::Velocity, velocity);
            let adjusted = (velocity + zone_velocity) * tick;
            let hit = self.move_actor(actor, adjusted, Bases::Block)?;
            if self.is_deleted(actor) {
                return Ok(());
            }
            if hit.time < 1.0 {
                if self.flag_of(actor, Prop::bBounce) {
                    self.event(actor, "HitWall", [Value::Vector(hit.normal), Value::Object(hit.actor)])?;
                    if self.is_deleted(actor) || self.physics_of(actor) == Some(EPhysics::None) {
                        return Ok(());
                    }
                    if bounces < 2 {
                        remaining += tick * (1.0 - hit.time);
                    }
                    bounces += 1;
                } else if hit.normal[2] >= MIN_FLOOR_Z {
                    if !self.flag_of(actor, Prop::bJustTeleported) && hit.time > MIN_LANDING_TIME && hit.time * tick > MIN_LANDING_SECONDS {
                        let moved = self.vector(actor, Prop::Location) - old_location;
                        self.set_vector(actor, Prop::Velocity, moved * (1.0 / (tick * hit.time)));
                    }
                    return self.actor_landed(actor, hit);
                } else {
                    self.actor_hit_wall(actor, hit)?;
                    let old_normal = hit.normal;
                    let mut slide = (adjusted - (hit.normal * adjusted.dot(hit.normal))) * (1.0 - hit.time);
                    if slide.dot(adjusted) >= 0.0 {
                        if slide[2] > 0.0 {
                            slide *= 0.5;
                        }
                        let second = self.move_actor(actor, slide, Bases::Block)?;
                        if second.time < 1.0 {
                            if second.normal[2] >= MIN_FLOOR_Z {
                                return self.actor_landed(actor, second);
                            }
                            self.actor_hit_wall(actor, second)?;
                            let desired = adjusted.safe_normal();
                            let along = two_wall_adjust(desired, slide, second.normal, old_normal, second.time);
                            let ditch = old_normal[2] > 0.0 && second.normal[2] > 0.0 && along[2] == 0.0 && second.normal.dot(old_normal) < 0.0;
                            let third = self.move_actor(actor, along, Bases::Block)?;
                            if ditch || (third.time < 1.0 && third.normal[2] >= MIN_FLOOR_Z) {
                                return self.actor_landed(actor, if third.time < 1.0 { third } else { second });
                            }
                        }
                    }
                    let moved = self.vector(actor, Prop::Location) - old_location;
                    old_velocity = [moved[0] / tick, moved[1] / tick, old_velocity[2]].into();
                }
            }
            if self.is_deleted(actor) || self.physics_of(actor) != Some(EPhysics::Falling) {
                return Ok(());
            }
            if !self.flag_of(actor, Prop::bBounce) && !self.flag_of(actor, Prop::bJustTeleported) {
                let moved = self.vector(actor, Prop::Location) - old_location;
                let mut velocity = (moved * (1.0 / tick)) - zone_velocity;
                if velocity[2] < old_velocity[2] || old_velocity[2] >= 0.0 {
                    velocity = (velocity * 2.0) - old_velocity;
                }
                if terminal > 0.0 && velocity.size() > terminal {
                    velocity = velocity.safe_normal() * terminal;
                }
                self.set_vector(actor, Prop::Velocity, velocity);
            }
        }
        Ok(())
    }

    fn actor_hit_wall(&mut self, actor: ObjectId, hit: MoveHit) -> Result<(), Error> {
        if hit.actor.is_some_and(|wall| self.is_a(wall, Kind::Pawn)) {
            return Ok(());
        }
        self.event(actor, "HitWall", [Value::Vector(hit.normal), Value::Object(hit.actor)])
    }

    fn actor_landed(&mut self, actor: ObjectId, hit: MoveHit) -> Result<(), Error> {
        self.event(actor, "Landed", [Value::Vector(hit.normal)])?;
        if !self.is_deleted(actor) && self.physics_of(actor) == Some(EPhysics::Falling) {
            self.set_physics(actor, EPhysics::None, Some((hit.actor, hit.normal)))?;
            self.set_vector(actor, Prop::Velocity, Vector::ZERO);
        }
        Ok(())
    }
}
