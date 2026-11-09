//! Severed limbs are Karma bodies by our choice; retail uses `PHYS_Projectile`. Scripts settle them.

use std::collections::BTreeMap;

use p2_physics::{Background, BodyId, BodyParams, Shape};

use p2_math::{Rotator, Vector};
use crate::enums::{EDrawType, EPhysics};
use crate::karma::{quat_to_rotator, rotator_to_quat};
use crate::profiling::Work;
use crate::{Error, LoaderRef, ObjectId, Prop, Value, Vm};

/// A limb's mass, in the corpses' units (a ragdoll part is about 0.07).
const MASS: f32 = 0.15;
const FRICTION: f32 = 0.7;
const RESTITUTION: f32 = 0.3;
const LINEAR_DAMPING: f32 = 0.1;
const ANGULAR_DAMPING: f32 = 0.3;
/// A velocity the script wrote (`GiveMomentum`, a kick, a shot) differs from the last one written by more (UU/s).
const SCRIPT_WROTE: f32 = 5.0;
/// A script move farther than this from the body teleports the body (`CutInHalf`'s `SetLocation`).
const STRAY: f32 = 100.0;
/// A change of velocity over a step that gravity does not explain, above this, is an impact (UU/s).
const IMPACT: f32 = 120.0;
const HEARD: f32 = 200.0;
/// A hit this hard reports [`SPLAT`]: 0.3 of it clears the script's 200, which leaves blood on the wall.
const HARD_HIT: f32 = 150.0;
const SPLAT: f32 = 700.0;
const SPLAT_TICKS: u64 = 12;
const ROTATION_TO_RADIANS: f32 = std::f32::consts::TAU / 65536.0;

#[derive(Default)]
pub(crate) struct Limbs {
    live: BTreeMap<ObjectId, Limb>,
    tick: u64,
    step: f32,
}

struct Limb {
    id: BodyId,
    mesh: ObjectId,
    wrote: Vector,
    before: Vector,
    at: Vector,
    splat_tick: u64,
}

impl Limbs {
    pub(crate) fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    pub(crate) fn drives(&self, actor: ObjectId) -> bool {
        self.live.contains_key(&actor)
    }
}

fn length(v: Vector) -> f32 {
    v.size()
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn limbs_before_step(&mut self, mut flying: Vec<ObjectId>, delta: f32) -> Vec<(String, Error)> {
        let mut errors = Vec::new();
        self.karma.limbs.tick += 1;
        self.karma.limbs.step = delta;
        let gone: Vec<ObjectId> =
            self.karma.limbs.live.keys().copied().filter(|&a| self.store.is_deleted(a) || self.store.flag_of(a, Prop::bHidden)).collect();
        for actor in gone {
            self.remove_limb(actor);
        }
        let resting: Vec<(ObjectId, BodyId)> =
            self.karma.limbs.live.iter().filter(|&(&a, _)| self.store.physics_of(a) == Some(EPhysics::None)).map(|(&a, limb)| (a, limb.id)).collect();
        let knocked: Vec<ObjectId> = match self.karma.world.as_mut().and_then(Background::world) {
            Some(world) => resting.into_iter().filter(|&(_, id)| !world.is_sleeping(id)).map(|(a, _)| a).collect(),
            None => Vec::new(),
        };
        for actor in knocked {
            match self.set_physics(actor, EPhysics::Projectile, None) {
                Ok(()) => flying.push(actor),
                Err(e) => errors.push((self.store.object(actor).path.clone(), e)),
            }
        }
        let (limb, bolton_part) = (self.script.lookups.class_names.limb, self.script.lookups.class_names.bolton_part);
        for actor in flying {
            // Dropped bolt-ons (`DropBoltons`: briefcase, purse, broom; `PeoplePart.GiveMomentum`) fly like cut limbs:
            // a body instead of the projectile arc (our choice; retail flies them as projectiles).
            if self.store.flag_of(actor, Prop::bHidden) || !(self.store.is_a(actor, limb) || self.store.is_a(actor, bolton_part)) {
                continue;
            }
            self.count(Work::LimbFlying);
            let mesh = (self.draw_type_of(actor) == Some(EDrawType::StaticMesh)).then(|| self.store.object_of(actor, Prop::StaticMesh)).flatten();
            match (mesh, self.karma.limbs.live.get(&actor).map(|limb| limb.mesh)) {
                (Some(mesh), Some(had)) if mesh == had => self.limb_motion(actor),
                (Some(mesh), had) => {
                    if had.is_some() {
                        self.remove_limb(actor);
                    }
                    self.add_limb(actor, mesh);
                }
                (None, _) => self.remove_limb(actor),
            }
            if !self.karma.limbs.drives(actor) {
                self.count(Work::LimbNoBody);
            }
        }
        errors
    }

    /// After the step. `bCollideWorld` off: the hull is its collision. An impact goes to the script as `HitWall` with the
    /// velocity it came in at (at least 200, or 700 for a hard hit, so the script makes its sound and blood; zero once
    /// asleep, so it settles).
    pub(crate) fn limbs_after_step(&mut self) -> Vec<(String, Error)> {
        let driven: Vec<(ObjectId, BodyId)> = self
            .karma
            .limbs
            .live
            .iter()
            .filter(|&(&a, _)| !self.store.is_deleted(a) && self.store.physics_of(a) == Some(EPhysics::Projectile))
            .map(|(&a, limb)| (a, limb.id))
            .collect();
        if driven.is_empty() {
            return Vec::new();
        }
        let Some(world) = self.karma.world.as_mut().and_then(Background::world) else { return Vec::new() };
        let gravity = world.gravity();
        let moved: Vec<_> = driven
            .into_iter()
            .filter_map(|(a, id)| {
                let (location, quat) = world.pose(id)?;
                Some((a, location, quat_to_rotator(quat), world.velocity(id)?, world.is_sleeping(id)))
            })
            .collect();
        let mut errors = Vec::new();
        for (actor, location, rotation, after, asleep) in moved {
            if let Err(e) = self.limb_moved(actor, location, rotation, after, asleep, gravity) {
                errors.push((self.store.object(actor).path.clone(), e));
            }
        }
        errors
    }

    fn limb_moved(&mut self, actor: ObjectId, location: Vector, rotation: Rotator, after: Vector, asleep: bool, gravity: Vector) -> Result<(), Error> {
        // A velocity or move the script gave since the last step (a kick's `GiveMomentum` on a settled limb) is the
        // limb's next motion: the step's result must not overwrite it, `limbs_before_step` hands it to the body.
        if let Some(limb) = self.karma.limbs.live.get(&actor) {
            let (velocity, at) = (self.store.vector(actor, Prop::Velocity), self.store.vector(actor, Prop::Location));
            if length([0, 1, 2].map(|i| velocity[i] - limb.wrote[i]).into()) > SCRIPT_WROTE || length([0, 1, 2].map(|i| at[i] - limb.at[i]).into()) > STRAY {
                return Ok(());
            }
        }
        if length([0, 1, 2].map(|i| location[i] - self.store.vector(actor, Prop::Location)[i]).into()) > 0.1 {
            self.count(Work::LimbMoved);
        }
        self.store.set(actor, Prop::bCollideWorld, Value::Bool(false));
        self.set_vector(actor, Prop::Location, location);
        self.store.set(actor, Prop::Rotation, Value::Rotator(rotation));
        self.relocate(actor);
        let Some(limb) = self.karma.limbs.live.get_mut(&actor) else { return Ok(()) };
        limb.at = location;
        let before = limb.before;
        self.update_touching(actor)?;
        if self.store.is_deleted(actor) {
            return Ok(());
        }
        let step = self.karma.limbs.step;
        let jolt: Vector = [0, 1, 2].map(|i| after[i] - before[i] - gravity[i] * step).into();
        let jolt_speed = length(jolt);
        let mut wrote = after;
        if asleep || jolt_speed > IMPACT {
            let normal = if asleep || jolt_speed <= 0.0 { Vector::new(0.0, 0.0, 1.0) } else { jolt.map(|c| c / jolt_speed) };
            let tick = self.karma.limbs.tick;
            let splat = self.karma.limbs.live.get_mut(&actor).is_some_and(|limb| {
                let splat = !asleep && jolt_speed > HARD_HIT && tick - limb.splat_tick > SPLAT_TICKS;
                if splat {
                    limb.splat_tick = tick;
                }
                splat
            });
            let heard = if splat { SPLAT } else { HEARD };
            let speed = length(before);
            let incoming = if asleep {
                Vector::ZERO
            } else if speed < heard && speed > 1.0 {
                before.map(|c| c * heard / speed)
            } else if speed < heard {
                normal.map(|c| -c * heard)
            } else {
                before
            };
            self.count(Work::LimbImpact);
            self.set_vector(actor, Prop::Velocity, incoming);
            self.event(actor, "HitWall", [Value::Vector(normal), Value::Object(None)])?;
            if self.store.is_deleted(actor) {
                return Ok(());
            }
            if self.store.physics_of(actor) == Some(EPhysics::None) {
                self.count(Work::LimbSettled);
                self.store.set(actor, Prop::Rotation, Value::Rotator(rotation));
                wrote = [0.0; 3].into();
            }
        }
        self.set_vector(actor, Prop::Velocity, wrote);
        if let Some(limb) = self.karma.limbs.live.get_mut(&actor) {
            limb.wrote = wrote;
        }
        Ok(())
    }

    fn add_limb(&mut self, actor: ObjectId, mesh: ObjectId) {
        let Some(shape) = self.karma_mesh(mesh) else { return };
        let placement = self.drawn_placement(actor);
        let hull = shape.points.iter().map(|p| [0, 1, 2].map(|i| (p[i] - placement.pre_pivot[i]) * placement.scale[i])).collect();
        let location = placement.location;
        let body = BodyParams {
            shapes: vec![Shape::Hull(hull)],
            location,
            rotation: rotator_to_quat(placement.rotation),
            mass: MASS,
            linear_damping: LINEAR_DAMPING,
            angular_damping: ANGULAR_DAMPING,
            friction: FRICTION,
            restitution: RESTITUTION,
            asleep: false,
            stay_upright: false,
            allow_yaw: false,
        };
        self.karma_world(actor);
        let Some(world) = self.karma.world.as_mut().and_then(Background::world) else { return };
        let id = world.add_body(&body);
        self.count(Work::LimbMade);
        self.karma.actors.insert(id, actor);
        self.karma.limbs.live.insert(actor, Limb { id, mesh, wrote: [0.0; 3].into(), before: [0.0; 3].into(), at: location, splat_tick: 0 });
        self.take_limb_motion(actor);
    }

    fn limb_motion(&mut self, actor: ObjectId) {
        let Some(limb) = self.karma.limbs.live.get_mut(&actor) else { return };
        let (id, wrote, at) = (limb.id, limb.wrote, limb.at);
        limb.before = wrote;
        let velocity = self.store.vector(actor, Prop::Velocity);
        if length([0, 1, 2].map(|i| velocity[i] - wrote[i]).into()) > SCRIPT_WROTE {
            self.count(Work::LimbThrown);
            self.take_limb_motion(actor);
        }
        let location = self.store.vector(actor, Prop::Location);
        if length([0, 1, 2].map(|i| location[i] - at[i]).into()) > STRAY {
            let quat = rotator_to_quat(self.store.rotator_of(actor, Prop::Rotation));
            if let Some(world) = &mut self.karma.world {
                world.change(move |w| w.set_pose(id, location, quat));
            }
            if let Some(limb) = self.karma.limbs.live.get_mut(&actor) {
                limb.at = location;
            }
        }
    }

    fn take_limb_motion(&mut self, actor: ObjectId) {
        let velocity = self.store.vector(actor, Prop::Velocity);
        let Rotator { pitch, yaw, roll } = self.store.rotator_of(actor, Prop::RotationRate);
        self.store.set(actor, Prop::RotationRate, Value::Rotator([0; 3].into()));
        let spin = [roll, pitch, yaw].map(|r| r as f32 * ROTATION_TO_RADIANS);
        let Some(limb) = self.karma.limbs.live.get_mut(&actor) else { return };
        (limb.wrote, limb.before) = (velocity, velocity);
        let id = limb.id;
        if let Some(world) = &mut self.karma.world {
            world.change(move |w| {
                w.set_velocity(id, velocity);
                w.set_spin(id, spin.into());
            });
        }
    }

    fn remove_limb(&mut self, actor: ObjectId) {
        let Some(limb) = self.karma.limbs.live.remove(&actor) else { return };
        self.karma.actors.remove(&limb.id);
        if let Some(world) = &mut self.karma.world {
            world.change(move |w| w.remove_body(limb.id));
        }
    }
}
