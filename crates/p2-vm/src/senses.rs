use p2_math::Vector;
use rustc_hash::FxHashSet;
use crate::{Error, LoaderRef, ObjectId, Value, Vm, Prop, Kind};

const PROBE_HEAR_NOISE: u32 = 42;
const PROBE_SEE_PLAYER: u32 = 40;
const PROBE_ENEMY_NOT_VISIBLE: u32 = 41;
const PROBE_SEE_MONSTER: u32 = 44;
const SIGHT_CHECK_SECONDS: f32 = 0.1;
const SIGHT_IDLE_SECONDS: f32 = 0.2;
const NOISE_SLOTS: [[Prop; 3]; 2] = [[Prop::noise1spot, Prop::noise1time, Prop::noise1loudness], [Prop::noise2spot, Prop::noise2time, Prop::noise2loudness]];
const MAX_CONTROLLERS: usize = 100_000;

/// A noise this close (squared distance) to one of the instigator's last two, within these seconds, is only heard
/// again when louder than this fraction.
const REPEAT_DISTANCE_SQUARED: f32 = 2500.0;
const REPEAT_SECONDS: f32 = 0.2;
const REPEAT_LOUDNESS: f32 = 0.9;
const SLOT_SECONDS: f32 = 0.18;
const SIGHT_FAR_SQUARED: f32 = 64_000_000.0;
const SIGHT_NEAR_SQUARED: f32 = 4_000_000.0;
const SIGHT_SIDE_RATIO: f32 = 0.0001;
const SIGHT_HEAD: f32 = 0.8;
const SEEN_SECONDS: f32 = 1.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SightCheck {
    Full,
    Periodic,
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn make_noise(&mut self, actor: ObjectId, mut loudness: f32) -> Result<(), Error> {
        let Some(instigator) = self.store.object_of(actor, Prop::Instigator) else { return Ok(()) };
        if self.store.object_of(actor, Prop::Owner) == Some(instigator) || instigator == actor {
            loudness *= self.store.float_of(instigator, Prop::SoundDampening);
        }
        self.check_noise_hearing(actor, instigator, loudness)
    }

    fn check_noise_hearing(&mut self, actor: ObjectId, instigator: ObjectId, loudness: f32) -> Result<(), Error> {
        let Some(controller) = self.store.object_of(instigator, Prop::Controller) else { return Ok(()) };
        let now = self.level.map_or(0.0, |level| self.store.float_of(level, Prop::TimeSeconds));
        let location = self.store.vector(actor, Prop::Location);
        let slot = |vm: &Self, [spot, time, loud]: [Prop; 3]| {
            let offset = vm.store.vector(instigator, spot) - location;
            (vm.store.float_of(instigator, time), offset.dot(offset) < REPEAT_DISTANCE_SQUARED, vm.store.float_of(instigator, loud))
        };
        let (first, second) = (slot(self, NOISE_SLOTS[0]), slot(self, NOISE_SLOTS[1]));
        for (time, near, louder) in [first, second] {
            if now - REPEAT_SECONDS < time && near && loudness * REPEAT_LOUDNESS <= louder {
                return Ok(());
            }
        }
        // Both slots recent: retail writes slot 1 also when only slot 2's noise was quieter (unverified).
        let taken = if now - SLOT_SECONDS > first.0 {
            Some(0)
        } else if now - SLOT_SECONDS > second.0 {
            Some(1)
        } else {
            ((first.1 && first.2 <= loudness) || second.2 <= loudness).then_some(0)
        };
        if let Some(n) = taken {
            let [spot, time, loud] = NOISE_SLOTS[n];
            self.store.set(instigator, time, Value::Float(now));
            self.set_vector(instigator, spot, location);
            self.store.set(instigator, loud, Value::Float(loudness));
        }
        let everyone = self.is_player(instigator) || self.store.object_of(controller, Prop::Enemy).is_some_and(|enemy| self.is_player(enemy));
        let tag = self.store.get(actor, Prop::Tag);
        for listener in self.controllers() {
            if self.store.object_of(listener, Prop::Pawn) == Some(instigator) || (!everyone && self.store.get(listener, Prop::Tag) != tag) {
                continue;
            }
            if !self.is_probing(listener, PROBE_HEAR_NOISE)? {
                continue;
            }
            if self.can_hear(listener, location, loudness, actor) {
                self.event(listener, "HearNoise", [Value::Float(loudness), Value::Object(Some(actor))])?;
            }
        }
        Ok(())
    }

    /// Which eye height is (unverified).
    /// Not modelled: adjacent zones, `bMuffledHearing`, `bAroundCornerHearing`.
    fn can_hear(&self, listener: ObjectId, noise: Vector, loudness: f32, other: ObjectId) -> bool {
        let Some(instigator) = self.store.object_of(other, Prop::Instigator) else { return false };
        let Some(pawn) = self.store.object_of(listener, Prop::Pawn) else { return false };
        if self.store.object_of(instigator, Prop::Controller).is_none() {
            return false;
        }
        let at = self.store.vector(pawn, Prop::Location);
        let offset = at - noise;
        let threshold = self.store.float_of(pawn, Prop::HearingThreshold);
        let perceived = (self.store.float_of(pawn, Prop::Alertness) + 1.0).max(0.0) * threshold * threshold * loudness;
        if offset.dot(offset) > perceived {
            return false;
        }
        let zone = |actor: ObjectId| match self.store.get_path(actor, &["Region", "Zone"]) {
            Some(Value::Object(zone)) => zone,
            _ => None,
        };
        let zone_hearing = self.store.flag_of(pawn, Prop::bSameZoneHearing) || self.store.flag_of(pawn, Prop::bAdjacentZoneHearing);
        if zone_hearing && zone(pawn).is_some() && zone(pawn) == zone(other) {
            return true;
        }
        if !self.store.flag_of(pawn, Prop::bLOSHearing) {
            return false;
        }
        let eyes = [at[0], at[1], at[2] + self.store.float_of(pawn, Prop::BaseEyeHeight)];
        self.trace_bsp(noise, eyes.into(), Vector::ZERO).is_none()
    }

    /// Actors do not block here (unverified). Of four side points retail drops the nearest and farthest
    /// to the world origin, not to the viewer, and traces the remaining ones among the first three.
    pub(crate) fn line_of_sight_to(&mut self, controller: ObjectId, other: ObjectId, check: SightCheck) -> bool {
        let pawn = self.store.object_of(controller, Prop::Pawn);
        let target = self.store.object_of(controller, Prop::ViewTarget).or(pawn).unwrap_or(controller);
        let mut view = self.store.vector(target, Prop::Location);
        if Some(target) == pawn {
            view[2] += self.store.float_of(target, Prop::BaseEyeHeight);
        }
        let clear = |vm: &Self, from: Vector| vm.trace_world(from, view, Vector::ZERO).is_none();
        let location = self.store.vector(other, Prop::Location);
        let offset = location - self.store.vector(target, Prop::Location);
        let distance_squared = offset.dot(offset);
        let radius = self.store.float_of(other, Prop::CollisionRadius);
        if self.store.object_of(controller, Prop::Enemy) == Some(other) {
            let eyes = [location[0], location[1], location[2] + self.store.float_of(other, Prop::BaseEyeHeight)];
            if clear(self, location) || clear(self, eyes.into()) {
                let now = self.level.map_or(0.0, |level| self.store.float_of(level, Prop::TimeSeconds));
                self.store.set(controller, Prop::LastSeenTime, Value::Float(now));
                self.set_vector(controller, Prop::LastSeeingPos, self.store.vector(target, Prop::Location));
                self.set_vector(controller, Prop::LastSeenPos, location);
                self.store.set(controller, Prop::bEnemyInfoValid, Value::Bool(true));
                return true;
            }
        } else {
            if clear(self, location) {
                return true;
            }
            let player = self.store.flag_of(controller, Prop::bIsPlayer);
            if distance_squared > SIGHT_FAR_SQUARED || ((!player || !self.store.is_a(other, Kind::Pawn)) && distance_squared > SIGHT_NEAR_SQUARED) {
                return false;
            }
            let los_flag = self.store.flag_of(controller, Prop::bLOSflag);
            if check == SightCheck::Full || !los_flag {
                let head = [location[0], location[1], location[2] + self.store.float_of(other, Prop::CollisionHeight) * SIGHT_HEAD];
                if clear(self, head.into()) {
                    return true;
                }
            }
            if check == SightCheck::Periodic && !los_flag {
                return false;
            }
        }
        if radius * radius / distance_squared < SIGHT_SIDE_RATIO {
            return false;
        }
        let Vector { x, y, z } = location;
        let points = [Vector::new(x - radius, y + radius, z), Vector::new(x + radius, y + radius, z), Vector::new(x - radius, y - radius, z), Vector::new(x + radius, y - radius, z)];
        let (mut nearest, mut farthest) = (0, 0);
        let (mut min, mut max) = (points[0].dot(points[0]), points[0].dot(points[0]));
        for (index, point) in points.iter().enumerate().skip(1) {
            let size = (*point).dot(*point);
            if size > max {
                (max, farthest) = (size, index);
            } else if size < min {
                (min, nearest) = (size, index);
            }
        }
        (0..3).any(|index| index != nearest && index != farthest && clear(self, points[index]))
    }

    /// Standalone: drawn by the host within the last second. Servers test each player's view instead.
    pub(crate) fn player_can_see_me(&self, actor: ObjectId) -> bool {
        let now = self.level.map_or(0.0, |level| self.store.float_of(level, Prop::TimeSeconds));
        now - self.store.float_of(actor, Prop::LastRenderTime) < SEEN_SECONDS
    }

    fn sight_paused(&self) -> bool {
        self.level.is_some_and(|level| self.store.object_of(level, Prop::Pauser).is_some())
    }

    /// Runs before the pawn turns; stand-alone play always has authority.
    /// The name `bAmbientCreature` is (unverified).
    pub(crate) fn controller_sight(&mut self, controller: ObjectId, delta: f32) -> Result<(), Error> {
        // Retail returns before the sight part in stasis, so the counter freezes and no sight event wakes it.
        if self.sight_paused() || self.store.flag_of(controller, Prop::bStasis) {
            return Ok(());
        }
        if self.store.float_of(controller, Prop::SightCounter) < 0.0 {
            let gain = if self.is_probing(controller, PROBE_ENEMY_NOT_VISIBLE)? {
                self.check_enemy_visible(controller)?;
                SIGHT_CHECK_SECONDS
            } else {
                SIGHT_IDLE_SECONDS
            };
            let counter = self.store.float_of(controller, Prop::SightCounter) + gain;
            self.store.set(controller, Prop::SightCounter, Value::Float(counter));
        }
        let counter = self.store.float_of(controller, Prop::SightCounter) - delta;
        self.store.set(controller, Prop::SightCounter, Value::Float(counter));
        if self.store.is_deleted(controller) {
            return Ok(());
        }
        let Some(pawn) = self.store.object_of(controller, Prop::Pawn).filter(|&p| !self.store.is_deleted(p)) else { return Ok(()) };
        if !self.store.flag_of(pawn, Prop::bHidden) && !self.store.flag_of(pawn, Prop::bAmbientCreature) {
            self.show_self(controller)?;
        }
        Ok(())
    }

    pub(crate) fn player_sight(&mut self, controller: ObjectId, delta: f32) -> Result<(), Error> {
        if self.sight_paused() {
            return Ok(());
        }
        let dilation = self.level.map_or(1.0, |level| self.store.float_of(level, Prop::TimeDilation));
        let seconds = if dilation > 0.0 { delta / dilation } else { delta };
        let counter = self.store.float_of(controller, Prop::SightCounter);
        let counter = if counter < 0.0 { counter + SIGHT_IDLE_SECONDS } else { counter };
        self.store.set(controller, Prop::SightCounter, Value::Float(counter - seconds));
        let Some(pawn) = self.store.object_of(controller, Prop::Pawn).filter(|&p| !self.store.is_deleted(p)) else { return Ok(()) };
        if !self.store.flag_of(pawn, Prop::bHidden) {
            self.show_self(controller)?;
        }
        Ok(())
    }

    fn check_enemy_visible(&mut self, controller: ObjectId) -> Result<(), Error> {
        let Some(enemy) = self.store.object_of(controller, Prop::Enemy) else { return Ok(()) };
        if !self.line_of_sight_to(controller, enemy, SightCheck::Full) {
            self.event(controller, "EnemyNotVisible", Vec::new())?;
        }
        Ok(())
    }

    fn show_self(&mut self, controller: ObjectId) -> Result<(), Error> {
        let mut next = self.level.and_then(|level| self.store.object_of(level, Prop::ControllerList));
        for _ in 0..MAX_CONTROLLERS {
            let Some(other) = next else { break };
            let player = self.store.flag_of(controller, Prop::bIsPlayer);
            if other != controller && (player || self.store.flag_of(other, Prop::bIsPlayer)) && self.store.float_of(other, Prop::SightCounter) < 0.0 {
                let (probe, event) = if player { (PROBE_SEE_PLAYER, "SeePlayer") } else { (PROBE_SEE_MONSTER, "SeeMonster") };
                if self.is_probing(other, probe)?
                    && let Some(pawn) = self.store.object_of(controller, Prop::Pawn)
                    && self.see_pawn(other, pawn, SightCheck::Periodic)
                {
                    self.event(other, event, [Value::Object(Some(pawn))])?;
                }
            }
            next = self.store.object_of(other, Prop::nextController);
        }
        Ok(())
    }

    pub(crate) fn is_player(&self, pawn: ObjectId) -> bool {
        self.store.object_of(pawn, Prop::Controller).is_some_and(|controller| self.store.flag_of(controller, Prop::bIsPlayer))
    }

    pub(crate) fn controllers(&self) -> Vec<ObjectId> {
        let (mut list, mut seen) = (Vec::new(), FxHashSet::default());
        let mut next = self.level.and_then(|level| self.store.object_of(level, Prop::ControllerList));
        while let Some(controller) = next {
            if !seen.insert(controller) {
                break;
            }
            list.push(controller);
            next = self.store.object_of(controller, Prop::nextController);
        }
        list
    }

    pub(crate) fn any_controller(&self, test: impl FnMut(ObjectId) -> bool) -> bool {
        let first = self.level.and_then(|level| self.store.object_of(level, Prop::ControllerList));
        std::iter::successors(first, |&controller| self.store.object_of(controller, Prop::nextController)).take(MAX_CONTROLLERS).any(test)
    }
}
