use p2_physics::{Hit, Throw};
use crate::native_id::Native;
use crate::karma::IMPULSE_SCALE;
use crate::{Error, LoaderRef, NameId, ObjectId, Value, Vm, Prop};
use super::{AIR_THROW, Corpse, HIT_TICKS, MELEE_THROW, SHOT_THROW, SeenHit};

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn ragdoll_native(&mut self, native: Native, this: ObjectId, args: &[Value]) -> Option<Result<Option<Value>, Error>> {
        let live = self.karma.ragdolls.live.contains_key(&this);
        match native {
            Native::KSetSkelVel => {
                if !live {
                    self.diagnostics.log.push("(Karma:) KSetSkelVelocity: Ragdoll not initialised.".to_owned());
                    return Some(Ok(None));
                }
                let velocity = crate::natives::vec3(args.first());
                let (Some(world), Some(ragdoll)) = (self.karma.world.as_mut(), self.karma.ragdolls.live.get(&this)) else { return Some(Ok(None)) };
                world.wake_ragdoll(&ragdoll.all);
                for &id in ragdoll.all.iter() {
                    world.set_velocity(id, velocity);
                }
                Some(Ok(None))
            }
            Native::KFreezeRagdoll => {
                if !live {
                    self.diagnostics.log.push("(Karma:) KFreezeRagdoll: Ragdoll not initialised.".to_owned());
                    return Some(Ok(None));
                }
                self.end_ragdoll(this, Corpse::Kept);
                self.karma.ragdolls.frozen.insert(this);
                Some(Ok(None))
            }
            Native::KWake if live => {
                if let (Some(world), Some(ragdoll)) = (self.karma.world.as_mut(), self.karma.ragdolls.live.get(&this)) {
                    world.wake_ragdoll(&ragdoll.all);
                }
                Some(Ok(None))
            }
            Native::KAddImpulse if live => {
                let impulse = crate::natives::vec3(args.first()).map(|c| c * IMPULSE_SCALE);
                let at = crate::natives::vec3(args.get(1));
                let named = match args.get(2) {
                    Some(&Value::Name(name)) if name != NameId::NONE => {
                        let bone = self.store.name(name);
                        Some(self.karma.ragdolls.live.get(&this).and_then(|ragdoll| ragdoll.mesh.names.iter().position(|n| n.eq_ignore_ascii_case(bone))))
                    }
                    _ => None,
                };
                let seen = self.take_hit(this);
                let (Some(world), Some(ragdoll)) = (self.karma.world.as_mut(), self.karma.ragdolls.live.get(&this)) else { return Some(Ok(None)) };
                let target = if let Some(index) = named {
                    // Else every shot and kick on a cut corpse's top half is lost.
                    index.and_then(|i| ragdoll.bodies.get(i).copied().flatten()).or_else(|| ragdoll.parts.nearest_body(at))
                } else if ragdoll.fresh {
                    ragdoll.bodies.iter().flatten().next().copied()
                } else {
                    ragdoll.parts.nearest_body(at)
                };
                if let Some(id) = target {
                    match seen {
                        Some(hit) => world.ragdoll_hit(&ragdoll.all, &Hit { impulse, body: Some(id), ..hit }),
                        None => world.ragdoll_impulse(&ragdoll.all, impulse, Some(id)),
                    }
                }
                Some(Ok(None))
            }
            _ => None,
        }
    }

    pub(crate) fn hit_seen(&mut self, victim: ObjectId, args: &[Value]) {
        let damage = args.first().map_or(0.0, crate::interp::float);
        let instigator = match args.get(1) {
            Some(&Value::Object(Some(by))) => Some(by),
            _ => None,
        };
        let (at, momentum) = (crate::natives::vec3(args.get(2)), crate::natives::vec3(args.get(3)));
        let length = momentum.size();
        let throw = match args.get(4) {
            Some(&Value::Object(Some(class))) if length > 0.0 => self.throw_of(class, instigator),
            _ => None,
        };
        let dir = if length > 0.0 { momentum.map(|c| c / length) } else { [0.0; 3].into() };
        let hit = Hit { impulse: [0.0; 3].into(), body: None, damage, throw, at, dir };
        let ragdolls = &mut self.karma.ragdolls;
        let tick = ragdolls.tick;
        ragdolls.hits.retain(|h| tick - h.tick <= HIT_TICKS);
        ragdolls.hits.push(SeenHit { actor: victim, tick, hit });
    }

    fn take_hit(&mut self, actor: ObjectId) -> Option<Hit> {
        let ragdolls = &mut self.karma.ragdolls;
        let tick = ragdolls.tick;
        let index = ragdolls.hits.iter().rposition(|h| h.actor == actor && tick - h.tick <= HIT_TICKS)?;
        Some(ragdolls.hits.remove(index).hit)
    }

    /// From flags the game's scripts already read, so a mod's damage types that set them get the same throw.
    fn throw_of(&mut self, damage_type: ObjectId, instigator: Option<ObjectId>) -> Option<Throw> {
        let class = self.class_of_object(damage_type).ok().flatten()?;
        if self.store.default_flag(class, Prop::bAllowZThrow) {
            return Some(AIR_THROW);
        }
        let by = instigator?;
        let melee_weapon = self.store.object_of(by, Prop::Weapon).is_some_and(|weapon| self.store.flag_of(weapon, Prop::bMeleeWeapon));
        Some(if self.store.default_flag(class, Prop::bMeleeDamage) || melee_weapon { MELEE_THROW } else { SHOT_THROW })
    }

    pub(crate) fn ragdoll_leftover_hits(&mut self) {
        let ragdolls = &mut self.karma.ragdolls;
        let tick = ragdolls.tick;
        ragdolls.tick += 1;
        let hits = std::mem::take(&mut ragdolls.hits);
        for seen in hits {
            match ragdolls.live.get(&seen.actor) {
                Some(ragdoll) if seen.hit.throw.is_some() => {
                    if let Some(world) = self.karma.world.as_mut() {
                        world.ragdoll_hit(&ragdoll.all, &seen.hit);
                    }
                }
                None if tick - seen.tick < HIT_TICKS => ragdolls.hits.push(seen),
                _ => {}
            }
        }
    }
}
