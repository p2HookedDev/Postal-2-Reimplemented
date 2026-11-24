//! Drops in the air, simulated on the CPU in world space (Bevy coordinates from the world's origin, Unreal units):
//! thrown drops, a wound's heartbeat spurts, drips that hang and let go, and crowns where a drop lands hard.

use std::f32::consts::PI;

use bevy::math::Vec3;
use p2_vm::FluidType;

use super::LiquidLanding;
use crate::particle_throw::next_unit;

/// Unreal units a second squared, the engine's default gravity.
pub const GRAVITY: f32 = 950.0;
const DROPS_PER_LEVEL: usize = 128;
const MOST_DROPS: usize = 1024;
/// A drop that has not landed by then fell out of the world; a drip that is not fed for as long lets go.
const MOST_AGE: f32 = 4.0;
/// A hanging drip lets go once it is this big.
pub const DETACH_RADIUS: f32 = 0.9;
/// Speed into the surface below which a landing makes no crown.
const CROWN_SPEED: f32 = 80.0;
pub const CROWN_SECONDS: f32 = 0.2;
const CROWN_DROPS: usize = 3;
/// A crown's drops: their radius against the drop's, and their speed sideways and off the surface against its impact.
const SECONDARY_RADIUS: f32 = 0.35;
const SECONDARY_THROW: (f32, f32) = (0.25, 0.2);
/// Seconds a beat, and where in it the systole peaks and how wide it is.
const BEAT_SECONDS: f32 = 0.85;
const SYSTOLE: (f32, f32) = (0.1, 0.07);
/// What a wound keeps flowing between beats.
const DIASTOLE: f32 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Hang,
    Fall,
    Crown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drop {
    pub at: Vec3,
    pub velocity: Vec3,
    pub radius: f32,
    pub age: f32,
    pub phase: Phase,
    pub kind: FluidType,
    /// Poured into the pools where it lands; 0 for a drop that is only seen.
    pub volume: f32,
    /// Thrown by a crown: it never splashes again.
    pub secondary: bool,
    /// The feeder it came from, as `Entity::to_bits`.
    pub source: u64,
    /// A crown's surface normal.
    pub normal: Vec3,
}

impl Drop {
    pub fn thrown(at: Vec3, velocity: Vec3, radius: f32, kind: FluidType, volume: f32, source: u64) -> Self {
        Self { at, velocity, radius, age: 0.0, phase: Phase::Fall, kind, volume, secondary: false, source, normal: Vec3::Y }
    }
}

/// Where a feeder throws drops from, in Bevy coordinates from the world's origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Feed {
    pub source: u64,
    pub kind: FluidType,
    pub from: Vec3,
    /// The low and high ends of the throw, Unreal units a second.
    pub velocity: [Vec3; 2],
    pub radius: [f32; 2],
    /// Whether its drops carry volume to the pools.
    pub pours: bool,
}

pub fn drop_cap(level: u8) -> usize {
    (DROPS_PER_LEVEL * usize::from(level)).min(MOST_DROPS)
}

pub fn sphere_volume(radius: f32) -> f32 {
    4.0 / 3.0 * PI * radius.powi(3)
}

fn sphere_radius(volume: f32) -> f32 {
    (volume * 3.0 / (4.0 * PI)).cbrt()
}

/// A wound's flow at `seconds` on its clock: 1 at the systole, `DIASTOLE` between beats.
pub fn heartbeat(seconds: f32) -> f32 {
    let phase = (seconds / BEAT_SECONDS).rem_euclid(1.0);
    let systole = ((phase - SYSTOLE.0) / SYSTOLE.1).powi(2);
    DIASTOLE + (1.0 - DIASTOLE) * (-systole).exp()
}

/// Whole drops due this frame at `per_second`; `owed` carries the part of a drop to the next frame.
pub fn due(owed: &mut f32, per_second: f32, seconds: f32) -> usize {
    let total = *owed + per_second.max(0.0) * seconds;
    let count = total.floor();
    *owed = total - count;
    count as usize
}

#[derive(Default)]
pub struct Drops {
    pub list: Vec<Drop>,
    seed: u32,
}

impl Drops {
    pub fn throw(&mut self, drop: Drop, cap: usize) {
        if self.list.len() < cap {
            self.list.push(drop);
        }
    }

    /// `count` drops thrown over the last `seconds`, the oldest farthest out, their speed scaled by `strength`.
    pub fn throw_from(&mut self, feed: &Feed, strength: f32, count: usize, seconds: f32, cap: usize) {
        let [low, high] = feed.velocity;
        for made in 0..count {
            let [a, b, c, d] = [(); 4].map(|()| next_unit(&mut self.seed));
            let velocity = (low + (high - low) * Vec3::new(a, b, c)) * strength;
            let lead = seconds * (count - made) as f32 / count as f32;
            let radius = feed.radius[0] + (feed.radius[1] - feed.radius[0]) * d;
            let volume = if feed.pours { sphere_volume(radius) } else { 0.0 };
            self.throw(Drop::thrown(feed.from + velocity * lead, velocity, radius, feed.kind, volume, feed.source), cap);
        }
    }

    /// Grows `source`'s hanging drip at `at` by `volume`; it falls once it reaches `DETACH_RADIUS`.
    pub fn feed_drip(&mut self, source: u64, at: Vec3, kind: FluidType, volume: f32, cap: usize) {
        match self.list.iter_mut().find(|drop| drop.source == source && drop.phase == Phase::Hang) {
            Some(drop) => {
                drop.volume += volume;
                (drop.radius, drop.at, drop.age) = (sphere_radius(drop.volume), at, 0.0);
            }
            None => self.throw(Drop { phase: Phase::Hang, ..Drop::thrown(at, Vec3::ZERO, sphere_radius(volume), kind, volume, source) }, cap),
        }
    }

    /// Moves every drop on by `seconds`. A falling drop sweeps its motion through `hit` (from, to: the surface's place
    /// and normal); one that carries volume lands in `landed`, and a hard landing leaves a crown that throws drops.
    pub fn step(&mut self, seconds: f32, cap: usize, hit: impl Fn(Vec3, Vec3) -> Option<(Vec3, Vec3)>, landed: &mut Vec<LiquidLanding>) {
        let gravity = Vec3::NEG_Y * GRAVITY;
        let mut thrown = Vec::new();
        let seed = &mut self.seed;
        self.list.retain_mut(|drop| {
            drop.age += seconds;
            match drop.phase {
                Phase::Hang => {
                    if drop.radius >= DETACH_RADIUS || drop.age >= MOST_AGE {
                        (drop.phase, drop.age) = (Phase::Fall, 0.0);
                    }
                    true
                }
                Phase::Crown => drop.age < CROWN_SECONDS,
                Phase::Fall => {
                    let to = drop.at + drop.velocity * seconds + gravity * (0.5 * seconds * seconds);
                    let Some((at, normal)) = hit(drop.at, to) else {
                        (drop.at, drop.velocity) = (to, drop.velocity + gravity * seconds);
                        return drop.age < MOST_AGE;
                    };
                    if drop.volume > 0.0 {
                        landed.push(LiquidLanding { at, normal, kind: drop.kind, volume: drop.volume });
                    }
                    let speed = -drop.velocity.dot(normal);
                    if drop.secondary || speed < CROWN_SPEED {
                        return false;
                    }
                    thrown.extend((0..CROWN_DROPS).map(|_| splashed(&*drop, at, normal, speed, seed)));
                    *drop = Drop { at, velocity: Vec3::ZERO, age: 0.0, phase: Phase::Crown, volume: 0.0, normal, ..*drop };
                    true
                }
            }
        });
        for drop in thrown {
            self.throw(drop, cap);
        }
    }
}

fn splashed(drop: &Drop, at: Vec3, normal: Vec3, speed: f32, seed: &mut u32) -> Drop {
    let mut unit = || next_unit(seed) * 2.0 - 1.0;
    let side = Vec3::new(unit(), unit(), unit()).reject_from_normalized(normal).normalize_or_zero();
    let velocity = (side * SECONDARY_THROW.0 + normal * SECONDARY_THROW.1) * speed;
    let radius = drop.radius * SECONDARY_RADIUS;
    Drop { secondary: true, ..Drop::thrown(at + normal * drop.radius, velocity, radius, drop.kind, 0.0, drop.source) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor(from: Vec3, to: Vec3) -> Option<(Vec3, Vec3)> {
        (from.y > 0.0 && to.y <= 0.0).then(|| (from.lerp(to, from.y / (from.y - to.y)), Vec3::Y))
    }

    fn falling(at: Vec3, velocity: Vec3, volume: f32) -> Drops {
        Drops { list: vec![Drop::thrown(at, velocity, 0.5, FluidType::Blood, volume, 1)], ..Drops::default() }
    }

    #[test]
    fn a_drop_falls_under_gravity_and_dies_out_of_the_world() {
        let mut drops = falling(Vec3::ZERO, Vec3::ZERO, 1.0);
        let mut landed = Vec::new();
        for _ in 0..5 {
            drops.step(0.1, 16, |_, _| None, &mut landed);
        }
        let drop = drops.list[0];
        assert!((drop.at.y + 0.5 * GRAVITY * 0.25).abs() < 1e-2, "{}", drop.at.y);
        assert!((drop.velocity.y + GRAVITY * 0.5).abs() < 1e-2);
        for _ in 0..40 {
            drops.step(0.1, 16, |_, _| None, &mut landed);
        }
        assert!(drops.list.is_empty() && landed.is_empty());
    }

    #[test]
    fn a_hard_landing_pours_once_and_crowns_and_its_drops_never_crown() {
        let mut drops = falling(Vec3::new(0.0, 10.0, 0.0), Vec3::new(0.0, -300.0, 0.0), 2.0);
        let mut landed = Vec::new();
        drops.step(0.1, 16, floor, &mut landed);
        assert_eq!(landed.len(), 1);
        assert!(landed[0].at.y.abs() < 1e-4, "{}", landed[0].at.y);
        assert_eq!((landed[0].normal, landed[0].volume), (Vec3::Y, 2.0));
        assert_eq!(drops.list.iter().filter(|drop| drop.phase == Phase::Crown).count(), 1);
        assert_eq!(drops.list.iter().filter(|drop| drop.secondary && drop.volume == 0.0 && drop.at.y > 0.0).count(), CROWN_DROPS);
        for _ in 0..20 {
            drops.step(0.05, 16, floor, &mut landed);
        }
        assert!(drops.list.is_empty(), "the crown is gone and its drops landed without crowns: {:?}", drops.list);
        assert_eq!(landed.len(), 1, "a drop that is only seen pours nothing");
    }

    #[test]
    fn a_soft_landing_makes_no_crown() {
        let mut drops = falling(Vec3::new(0.0, 0.5, 0.0), Vec3::ZERO, 0.0);
        let mut landed = Vec::new();
        drops.step(0.05, 16, floor, &mut landed);
        assert!(drops.list.is_empty() && landed.is_empty());
    }

    #[test]
    fn a_drip_hangs_until_it_is_big_enough_and_then_falls() {
        let mut drops = Drops::default();
        let mut landed = Vec::new();
        let full = sphere_volume(DETACH_RADIUS);
        for _ in 0..9 {
            drops.feed_drip(7, Vec3::new(0.0, 50.0, 0.0), FluidType::Blood, full / 10.0, 16);
            drops.step(0.05, 16, floor, &mut landed);
        }
        assert_eq!(drops.list.len(), 1);
        assert_eq!((drops.list[0].phase, drops.list[0].at.y), (Phase::Hang, 50.0));
        drops.feed_drip(7, Vec3::new(0.0, 50.0, 0.0), FluidType::Blood, full / 5.0, 16);
        drops.step(0.05, 16, floor, &mut landed);
        assert_eq!(drops.list[0].phase, Phase::Fall);
        drops.feed_drip(7, Vec3::new(0.0, 50.0, 0.0), FluidType::Blood, full / 10.0, 16);
        assert_eq!(drops.list.iter().filter(|drop| drop.phase == Phase::Hang).count(), 1, "a new drip starts");
        for _ in 0..20 {
            drops.step(0.05, 16, floor, &mut landed);
        }
        assert!(landed.first().is_some_and(|landing| (landing.volume - full * 1.1).abs() < 1e-3), "the drip pours what it held: {landed:?}");
    }

    #[test]
    fn the_heart_beats_and_the_drops_keep_to_their_cap() {
        assert!((heartbeat(SYSTOLE.0 * BEAT_SECONDS) - 1.0).abs() < 1e-5);
        assert!((heartbeat(0.6 * BEAT_SECONDS) - DIASTOLE).abs() < 1e-3);
        assert!((heartbeat(SYSTOLE.0 * BEAT_SECONDS + 3.0 * BEAT_SECONDS) - 1.0).abs() < 1e-3);
        assert_eq!((drop_cap(0), drop_cap(1), drop_cap(10)), (0, 128, MOST_DROPS));
        let mut owed = 0.0;
        assert_eq!((0..4).map(|_| due(&mut owed, 10.0, 0.05)).sum::<usize>(), 2);
        let feed = Feed { source: 1, kind: FluidType::Blood, from: Vec3::ZERO, velocity: [Vec3::X; 2], radius: [0.5, 1.0], pours: true };
        let mut drops = Drops::default();
        drops.throw_from(&feed, 1.0, 10, 0.1, 4);
        assert_eq!(drops.list.len(), 4);
        assert!(drops.list.iter().all(|drop| drop.volume >= sphere_volume(0.5) && drop.at.x > 0.0));
    }
}
