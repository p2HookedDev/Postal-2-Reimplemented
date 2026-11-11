//! Hats and caps come off as loose bodies at death and when the head goes; retail keeps them on and destroys them with it.

use std::collections::VecDeque;

use p2_math::{Vector, radians};
use p2_physics::{BodyId, Contacts};
use p2_script::Type;

use crate::enums::EDrawType;
use crate::{Error, LoaderRef, ObjectId, Prop, Value, Vm};

const SLOTS: usize = 8;
const CAP: usize = 32;
const RETIRE_LIFE: f32 = 0.01;
/// UU/s.
const THROW_MAX: f32 = 1500.0;
const DEATH_BACK: f32 = 120.0;
/// A thrown bolt-on starts inside its owner's corpse; until then it touches only the level, so it is not shot out.
const LEVEL_ONLY: f32 = 0.3;
/// `Limb`'s default mass: a hit moves a loose bolt-on as it moves a cut limb.
const LIMB_MASS: f32 = 90.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoltonHook {
    DropBoltons,
    DestroyHeadBoltons,
    PopOffHead,
    PinataStyleExplodeEffects,
}

#[derive(Debug, Clone, Copy)]
enum Throw {
    HeadVelocity,
    Momentum(Vector),
}

#[derive(Default)]
pub(crate) struct Boltons {
    losing: Option<(ObjectId, Throw)>,
    depth: u32,
    bodies: VecDeque<ObjectId>,
    young: Vec<(BodyId, f32)>,
}

impl Boltons {
    fn stash(&mut self, pawn: Option<ObjectId>, throw: Throw) {
        self.losing = pawn.map(|pawn| (pawn, throw));
        self.depth += 1;
    }
}

fn clamp_throw(v: Vector) -> Vector {
    let length = v.size();
    if length <= THROW_MAX { v } else { v.map(|c| c * THROW_MAX / length) }
}

fn struck_velocity(damage: f32, momentum: Vector) -> Option<Vector> {
    (damage > 0.0 && momentum.size() > 0.0).then(|| clamp_throw(momentum * (1.0 / LIMB_MASS)))
}

/// Radius and half height about the body's origin.
pub(crate) fn hull_size(points: &[[f32; 3]]) -> [f32; 2] {
    points.iter().fold([0.0, 0.0], |[radius, height]: [f32; 2], p| [radius.max(p[0].hypot(p[1])), height.max(p[2].abs())])
}

fn thrown_back(velocity: Vector, yaw: i32) -> Vector {
    let yaw = radians(yaw);
    Vector::new(velocity[0] - yaw.cos() * DEATH_BACK, velocity[1] - yaw.sin() * DEATH_BACK, velocity[2])
}

/// The oldest body past the cap.
fn admit(bodies: &mut VecDeque<ObjectId>, actor: ObjectId) -> Option<ObjectId> {
    bodies.push_back(actor);
    if bodies.len() > CAP { bodies.pop_front() } else { None }
}

impl<L: LoaderRef> Vm<L> {
    /// `momentum`: the function's second argument.
    pub(crate) fn bolton_before(&mut self, hook: BoltonHook, this: ObjectId, momentum: Option<Value>) -> Result<(), Error> {
        match hook {
            BoltonHook::PopOffHead => self.karma.boltons.stash(Some(this), Throw::HeadVelocity),
            BoltonHook::PinataStyleExplodeEffects => {
                let momentum = match momentum {
                    Some(Value::Vector(momentum)) => clamp_throw(momentum),
                    _ => Vector::ZERO,
                };
                let body = self.store.object_of(this, "MyBody");
                self.karma.boltons.stash(body, Throw::Momentum(momentum));
            }
            BoltonHook::DestroyHeadBoltons => {
                let Some((pawn, throw)) = self.karma.boltons.losing.filter(|&(pawn, _)| pawn == this) else { return Ok(()) };
                let velocity = match (throw, self.store.object_of(pawn, "MyHead")) {
                    (Throw::Momentum(momentum), _) => momentum,
                    (Throw::HeadVelocity, Some(head)) => clamp_throw(self.store.vector(head, Prop::Velocity)),
                    (Throw::HeadVelocity, None) => return Ok(()),
                };
                return self.throw_head_boltons(pawn, velocity);
            }
            BoltonHook::DropBoltons => {}
        }
        Ok(())
    }

    pub(crate) fn bolton_after(&mut self, hook: BoltonHook, this: ObjectId) -> Result<(), Error> {
        match hook {
            BoltonHook::PopOffHead | BoltonHook::PinataStyleExplodeEffects => {
                let boltons = &mut self.karma.boltons;
                boltons.depth = boltons.depth.saturating_sub(1);
                if boltons.depth == 0 {
                    boltons.losing = None;
                }
            }
            BoltonHook::DropBoltons if self.store.int_of(this, Prop::Health) <= 0 => {
                let velocity = thrown_back(self.store.vector(this, Prop::Velocity), self.store.rotator_of(this, Prop::Rotation).yaw);
                return self.throw_head_boltons(this, velocity);
            }
            BoltonHook::DropBoltons | BoltonHook::DestroyHeadBoltons => {}
        }
        Ok(())
    }

    /// Static-mesh ones only: a skeletal one (a cigarette) has no hull and keeps retail's way.
    fn throw_head_boltons(&mut self, pawn: ObjectId, velocity: Vector) -> Result<(), Error> {
        let (Some(head), Some([part_member, head_member])) = (self.store.object_of(pawn, "MyHead"), self.bolton_members(pawn)) else {
            return Ok(());
        };
        let life = self.bolton_life();
        for i in 0..SLOTS {
            let Some(Value::Struct(mut slot)) = self.store.element_of(pawn, "boltons", i) else { continue };
            let (Some(&Value::Object(Some(part))), Some(Value::Bool(true))) = (slot.get(part_member), slot.get(head_member)) else { continue };
            if self.draw_type_of(part) != Some(EDrawType::StaticMesh) || self.store.object_of(part, Prop::StaticMesh).is_none() {
                continue;
            }
            self.event(head, crate::states::Event::DetachFromBone, [Value::Object(Some(part))])?;
            self.event(part, crate::states::Event::GiveMomentum, [Value::Vector(velocity)])?;
            self.store.set(part, Prop::LifeSpan, Value::Float(life));
            slot[part_member] = Value::Object(None);
            self.store.set_element(pawn, "boltons", i, Value::Struct(slot));
        }
        Ok(())
    }

    fn bolton_members(&self, pawn: ObjectId) -> Option<[usize; 2]> {
        let class = self.store.object(pawn).class?;
        let index = self.store.property_index(class, "boltons")?;
        let Type::Struct(layout) = &self.store.class(class).layout.properties[index].ty else { return None };
        let member = |name: &str| layout.properties.iter().position(|p| p.name.eq_ignore_ascii_case(name));
        Some([member("part")?, member("bAttachToHead")?])
    }

    /// Seconds; 0 is no limit.
    fn bolton_life(&self) -> f32 {
        self.level.and_then(|level| self.store.object_of(level, Prop::Game)).map_or(0.0, |game| self.store.float_of(game, "LimbsLifetimeMax"))
    }


    pub(crate) fn bolton_adopted(&mut self, actor: ObjectId, id: BodyId, [radius, height]: [f32; 2]) {
        for (property, value) in [(Prop::bCollideActors, true), (Prop::bBlockActors, false), (Prop::bBlockPlayers, false), (Prop::bProjTarget, true)] {
            self.store.set(actor, property, Value::Bool(value));
        }
        self.store.set(actor, Prop::CollisionRadius, Value::Float(radius));
        self.store.set(actor, Prop::CollisionHeight, Value::Float(height));
        self.relocate(actor);
        if self.karma.boltons.bodies.contains(&actor) {
            return;
        }
        let life = self.bolton_life();
        self.store.set(actor, Prop::LifeSpan, Value::Float(life));
        let store = &self.store;
        let boltons = &mut self.karma.boltons;
        boltons.bodies.retain(|&a| a != actor && !store.is_deleted(a));
        let retired = admit(&mut boltons.bodies, actor);
        boltons.young.retain(|&(young, _)| young != id);
        boltons.young.push((id, 0.0));
        if let Some(oldest) = retired {
            self.store.set(oldest, Prop::LifeSpan, Value::Float(RETIRE_LIFE));
        }
        if let Some(world) = &mut self.karma.world {
            world.set_contacts(id, Contacts::LevelOnly);
        }
    }

    pub(crate) fn bolton_struck(&mut self, part: ObjectId, args: &[Value]) -> Result<(), Error> {
        if !self.karma.limbs.drives(part) || !self.store.is_a(part, self.script.lookups.class_names.bolton_part) {
            return Ok(());
        }
        let damage = args.first().map_or(0.0, crate::interp::float);
        let Some(velocity) = struck_velocity(damage, crate::natives::vec3(args.get(3))) else { return Ok(()) };
        self.event(part, crate::states::Event::GiveMomentum, [Value::Vector(velocity)])
    }

    pub(crate) fn age_young_boltons(&mut self, delta: f32) {
        let young = &mut self.karma.boltons.young;
        if young.is_empty() {
            return;
        }
        for (_, age) in young.iter_mut() {
            *age += delta;
        }
        let grown: Vec<BodyId> = young.extract_if(.., |&mut (_, age)| age >= LEVEL_ONLY).map(|(id, _)| id).collect();
        if let Some(world) = &mut self.karma.world {
            for id in grown {
                world.set_contacts(id, Contacts::All);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thrown_backwards_from_facing() {
        let east = thrown_back(Vector::new(10.0, 0.0, 5.0), 0);
        assert!((east[0] - (10.0 - DEATH_BACK)).abs() < 1e-3 && east[1].abs() < 1e-3 && east[2] == 5.0);
        let north = thrown_back(Vector::ZERO, 16384);
        assert!(north[0].abs() < 1e-3 && (north[1] + DEATH_BACK).abs() < 1e-3);
    }

    #[test]
    fn throw_clamped_to_max() {
        let slow = Vector::new(300.0, 400.0, 0.0);
        assert_eq!(clamp_throw(slow), slow);
        let fast = clamp_throw(Vector::new(3000.0, 4000.0, 0.0));
        assert!((fast.size() - THROW_MAX).abs() < 1e-2 && (fast[0] - 900.0).abs() < 1e-2);
    }

    #[test]
    fn cap_retires_oldest() {
        let mut bodies = VecDeque::new();
        for i in 0..CAP as u32 {
            assert_eq!(admit(&mut bodies, ObjectId(i)), None);
        }
        assert_eq!(admit(&mut bodies, ObjectId(99)), Some(ObjectId(0)));
        assert_eq!(bodies.len(), CAP);
        assert_eq!(bodies.back(), Some(&ObjectId(99)));
    }

    #[test]
    fn struck_scaled_like_a_limb() {
        let push = struck_velocity(5.0, Vector::new(9000.0, 0.0, 4500.0)).unwrap_or(Vector::ZERO);
        assert!((push[0] - 100.0).abs() < 1e-3 && (push[2] - 50.0).abs() < 1e-3);
        assert_eq!(struck_velocity(0.0, Vector::new(9000.0, 0.0, 0.0)), None);
        assert_eq!(struck_velocity(5.0, Vector::ZERO), None);
        let blast = struck_velocity(50.0, Vector::new(0.0, 0.0, 1.0e6)).unwrap_or(Vector::ZERO);
        assert!((blast.size() - THROW_MAX).abs() < 1e-2);
    }

    #[test]
    fn hull_size_about_origin() {
        assert_eq!(hull_size(&[[3.0, 4.0, -2.0], [-1.0, 0.0, 6.0]]), [5.0, 6.0]);
        assert_eq!(hull_size(&[]), [0.0, 0.0]);
    }
}
