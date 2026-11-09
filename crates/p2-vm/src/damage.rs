use p2_math::Vector;
use crate::native_id::Native;
use crate::Prop;
use crate::enums::ENetRole;
use crate::interp::float;
use crate::natives::{bool_arg, float_arg, object_arg, vec3};

use crate::world::trace;
use crate::{Error, Kind, LoaderRef, NameId, ObjectId, Value, Vm};

const PICK_TARGET_RANGE_SQUARED: f32 = 16_000_000.0;
const MAX_CONTROLLERS: u32 = 10_000;

struct RadiusDamage {
    amount: f32,
    radius: f32,
    damage_type: Value,
    momentum: f32,
    centre: Vector,
}

#[derive(Default)]
#[expect(clippy::struct_excessive_bools, reason = "the arguments of HurtRadiusEX")]
struct HurtOptions<'a> {
    no_scale: bool,
    round_up_if: f32,
    stop_at_wall: bool,
    ignore_instigator: bool,
    ignore: Option<ObjectId>,
    target_class: Option<NameId>,
    explosion: bool,
    class_check: &'a str,
    instigator_radius: f32,
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn hurt_radius_native(&mut self, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let Some(this) = this else { return Ok(None) };
        let mut options = HurtOptions::default();
        if native == Native::HurtRadiusEX {
            options = HurtOptions {
                no_scale: bool_arg(args, 5),
                round_up_if: float_arg(args, 6),
                stop_at_wall: bool_arg(args, 7),
                ignore_instigator: bool_arg(args, 8),
                ignore: object_arg(args, 9),
                target_class: object_arg(args, 10).map(|class| self.store.object(class).name),
                explosion: bool_arg(args, 11),
                class_check: match args.get(12) {
                    Some(Value::Str(list)) => list.as_str(),
                    _ => "",
                },
                instigator_radius: float_arg(args, 14),
            };
        }
        let damage_type = args.get(2).cloned().unwrap_or(Value::Object(None));
        let (hit, matched) = self.hurt_radius(this, RadiusDamage { amount: float_arg(args, 0), radius: float_arg(args, 1), damage_type, momentum: float_arg(args, 3), centre: vec3(args.get(4)) }, &options)?;
        if native == Native::HurtRadiusEX {
            if let Some(slot) = args.get_mut(13) {
                *slot = Value::Int(i32::from(matched));
            }
            return Ok(Some(Value::Int(hit)));
        }
        Ok(None)
    }

    /// With an instigator radius every victim uses it as radius, as retail, despite the script comment.
    /// The instigator is hit only when within it less its collision radius.
    fn hurt_radius(&mut self, this: ObjectId, hurt: RadiusDamage, options: &HurtOptions<'_>) -> Result<(i32, bool), Error> {
        let RadiusDamage { amount, radius, damage_type, momentum, centre } = hurt;
        let instigator = self.store.object_of(this, Prop::Instigator);
        let of_target_class = |vm: &Self, victim: ObjectId| match options.target_class {
            Some(class) => vm.store.is_a(victim, class),
            None => vm.store.is_a(victim, Kind::Actor),
        };
        let class_matches = !options.class_check.is_empty() && {
            let own_class = self.store.name(self.store.object(this).class_name);
            options.class_check.split(',').any(|c| c.trim().eq_ignore_ascii_case(own_class))
        };
        let candidates: Vec<ObjectId> = self
            .colliders_near([centre[0], centre[1]], [centre[0], centre[1]], radius)
            .into_iter()
            .map(|e| e.actor)
            .filter(|&o| self.store.flag_of(o, Prop::bCollideActors) && (self.store.vector(o, Prop::Location) - centre).size() < radius)
            .collect();
        let (mut live_pawns, mut matched) = (0, false);
        for victim in candidates {
            if self.store.is_deleted(victim)
                || !of_target_class(self, victim)
                || victim == this
                || ENetRole::from_byte(self.store.byte_of(victim, Prop::Role)) != Some(ENetRole::Authority)
                || self.store.flag_of(victim, Prop::bHidden)
                || (options.ignore_instigator && Some(victim) == instigator)
                || Some(victim) == options.ignore
            {
                continue;
            }
            let at = self.store.vector(victim, Prop::Location);
            if options.stop_at_wall && self.is_player(victim) {
                let from = self.store.vector(this, Prop::Location);
                if self.any_hit(from, at, [0.0; 3].into(), Some(this), trace::WORLD) {
                    continue;
                }
            }
            let mut reach = radius;
            if options.instigator_radius > 0.0 {
                if Some(victim) == instigator {
                    let distance = (self.store.vector(this, Prop::Location) - at).size();
                    if distance > options.instigator_radius - self.store.float_of(victim, Prop::CollisionRadius) {
                        continue;
                    }
                }
                reach = options.instigator_radius;
            }
            let offset = at - centre;
            let distance = offset.size().max(1.0);
            let direction = offset * (1.0 / distance);
            let share = if options.no_scale { 1.0 } else { 1.0 - ((distance - self.store.float_of(victim, Prop::CollisionRadius)) / reach).max(0.0) };
            let mut damage = share * amount;
            let mut push = direction * (share * momentum);
            if options.explosion && direction[2] > 0.0 {
                push[2] += (1.0 - distance / reach) * momentum;
            }
            if options.round_up_if == 0.0 {
                if damage == 0.0 {
                    continue;
                }
            } else if damage < options.round_up_if {
                continue;
            } else {
                damage = 1.0;
            }
            if self.store.is_a(victim, Kind::Pawn) && self.store.float_of(victim, Prop::Health) > 0.0 {
                live_pawns += 1;
            }
            let half = 0.5 * (self.store.float_of(victim, Prop::CollisionHeight) + self.store.float_of(victim, Prop::CollisionRadius));
            let hit = at - (direction * half);
            let args = [Value::Int((damage as i32).max(1)), Value::Object(instigator), Value::Vector(hit), Value::Vector(push), damage_type.clone()];
            self.bolton_struck(victim, &args)?;
            self.event(victim, crate::states::Event::TakeDamage, args)?;
            matched |= class_matches;
        }
        Ok((live_pawns, matched))
    }

    /// A zoomed player (`FovAngle` ≠ `DefaultFOV`, names (unverified)) looks any distance.
    pub(crate) fn pick_target(&mut self, this: ObjectId, args: &mut [Value]) -> Option<ObjectId> {
        let best_aim = args.first().map_or(0.0, float);
        let (fire_dir, start) = (vec3(args.get(2)), vec3(args.get(3)));
        let min_aim = best_aim * 3.0 - 2.0;
        let flat = Vector::new(fire_dir[0], fire_dir[1], 0.0).safe_normal();
        let own_pawn = self.store.object_of(this, Prop::Pawn);
        let eyes = |vm: &Self, pawn: ObjectId| {
            let at = vm.store.vector(pawn, Prop::Location);
            Vector::new(at[0], at[1], at[2] + vm.store.float_of(pawn, Prop::EyeHeight))
        };
        let zoomed = self.store.is_a(this, Kind::PlayerController) && self.store.float_of(this, Prop::FovAngle) != self.store.float_of(this, Prop::DefaultFOV);
        let team_game = self.level.and_then(|l| self.store.object_of(l, Prop::Game)).is_some_and(|g| self.store.flag_of(g, Prop::bTeamGame));
        let team = |vm: &Self, c: ObjectId| vm.store.object_of(c, Prop::PlayerReplicationInfo).map(|pri| vm.store.object_of(pri, Prop::Team));
        // The engine traces each candidate at its turn; the result is the same as tracing best aim first and stopping at the
        // first visible one (a shotgun's 12 pellets traced every pawn in front of the player, ~320 us a call in a crowd).
        let mut strong: Vec<(ObjectId, f32, f32)> = Vec::new();
        let mut weak: Vec<(ObjectId, f32)> = Vec::new();
        let mut next = self.level.and_then(|l| self.store.object_of(l, Prop::ControllerList));
        for _ in 0..MAX_CONTROLLERS {
            let Some(controller) = next else { break };
            next = self.store.object_of(controller, Prop::nextController);
            let Some(pawn) = self.store.object_of(controller, Prop::Pawn) else { continue };
            if controller == this || self.store.float_of(pawn, Prop::Health) <= 0.0 || !self.store.flag_of(pawn, Prop::bProjTarget) {
                continue;
            }
            if team_game && let (Some(ours), Some(theirs)) = (team(self, this), team(self, controller)) && ours == theirs {
                continue;
            }
            let delta = self.store.vector(pawn, Prop::Location) - start;
            let aim = fire_dir.dot(delta);
            let distance_squared = delta.dot(delta);
            if aim <= 0.0 || !(distance_squared < PICK_TARGET_RANGE_SQUARED || zoomed) {
                continue;
            }
            let distance = distance_squared.sqrt();
            let aim = aim / distance;
            if aim > best_aim {
                strong.push((pawn, aim, distance));
            } else if best_aim < flat.dot(delta) / distance && min_aim < aim {
                weak.push((pawn, distance));
            }
        }
        let visible = |vm: &mut Self, pawn: ObjectId| {
            own_pawn.is_some_and(|own| {
                let mine = eyes(vm, own);
                !vm.any_hit(vm.store.vector(pawn, Prop::Location), mine, [0.0; 3].into(), Some(this), trace::WORLD)
                    || !vm.any_hit(eyes(vm, pawn), mine, [0.0; 3].into(), Some(this), trace::WORLD)
            })
        };
        // Stable: of equal aims the first in list order wins, as the walk's strict `>` keeps it.
        strong.sort_by(|a, b| b.1.total_cmp(&a.1));
        let picked = match strong.into_iter().find(|&(pawn, ..)| visible(self, pawn)) {
            Some((pawn, aim, distance)) => Some((pawn, distance, Some(aim))),
            None => weak.into_iter().find(|&(pawn, _)| visible(self, pawn)).map(|(pawn, distance)| (pawn, distance, None)),
        };
        let (pawn, distance, better) = picked?;
        if let (Some(aim), Some(slot)) = (better, args.first_mut()) {
            *slot = Value::Float(aim);
        }
        if let Some(slot) = args.get_mut(1) {
            *slot = Value::Float(distance);
        }
        Some(pawn)
    }
}
