//! Drops in the air, simulated on the CPU in world space (Bevy coordinates from the world's origin, Unreal
//! units): thrown drops and a wound's heartbeat spurts.

use std::f32::consts::PI;

use bevy::math::Vec3;
use p2_vm::FluidType;

use super::LiquidLanding;
use crate::particle_throw::next_unit;

/// Unreal units a second squared, the engine's default gravity.
pub const GRAVITY: f32 = 950.0;
const DROPS_PER_LEVEL: usize = 128;
const MOST_DROPS: usize = 1024;
/// A drop that has not landed by then fell out of the world.
const MOST_AGE: f32 = 4.0;
const BEAT_SECONDS: f32 = 0.85;
const SYSTOLE: (f32, f32) = (0.1, 0.07);
const DIASTOLE: f32 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drop {
    pub at: Vec3,
    pub velocity: Vec3,
    pub radius: f32,
    pub age: f32,
    pub kind: FluidType,
    pub volume: f32,
    pub source: u64,
}

impl Drop {
    pub fn thrown(at: Vec3, velocity: Vec3, radius: f32, kind: FluidType, volume: f32, source: u64) -> Self {
        Self { at, velocity, radius, age: 0.0, kind, volume, source }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Feed {
    pub source: u64,
    pub kind: FluidType,
    pub from: Vec3,
    pub velocity: [Vec3; 2],
    pub radius: [f32; 2],
    pub pours: bool,
}

pub fn drop_cap(level: u8) -> usize {
    (DROPS_PER_LEVEL * usize::from(level)).min(MOST_DROPS)
}

pub fn sphere_volume(radius: f32) -> f32 {
    4.0 / 3.0 * PI * radius.powi(3)
}

pub fn heartbeat(seconds: f32) -> f32 {
    let phase = (seconds / BEAT_SECONDS).rem_euclid(1.0);
    let systole = ((phase - SYSTOLE.0) / SYSTOLE.1).powi(2);
    DIASTOLE + (1.0 - DIASTOLE) * (-systole).exp()
}

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

    pub fn unit(&mut self) -> f32 {
        next_unit(&mut self.seed)
    }

    pub fn throw_from(&mut self, feed: &Feed, strength: f32, count: usize, seconds: f32, cap: usize) {
        let [low, high] = feed.velocity;
        for made in 0..count {
            let [a, b, c, d] = [(); 4].map(|()| next_unit(&mut self.seed));
            let velocity = (low + (high - low) * Vec3::new(a, b, c)) * strength;
            let lead = seconds * (count - made) as f32 / count as f32;
            let radius = feed.radius[0] + (feed.radius[1] - feed.radius[0]) * d * d;
            let volume = if feed.pours { sphere_volume(radius) } else { 0.0 };
            self.throw(Drop::thrown(feed.from + velocity * lead, velocity, radius, feed.kind, volume, feed.source), cap);
        }
    }

    /// Moves every drop on by `seconds`, sweeping its motion through `hit` (from, to: the surface's place and
    /// normal). A drop that meets a surface is gone; one that carries volume lands in `landed`.
    pub fn step(&mut self, seconds: f32, hit: impl Fn(Vec3, Vec3) -> Option<(Vec3, Vec3)>, landed: &mut Vec<LiquidLanding>) {
        let gravity = Vec3::NEG_Y * GRAVITY;
        self.list.retain_mut(|drop| {
            drop.age += seconds;
            let to = drop.at + drop.velocity * seconds + gravity * (0.5 * seconds * seconds);
            let Some((at, normal)) = hit(drop.at, to) else {
                (drop.at, drop.velocity) = (to, drop.velocity + gravity * seconds);
                return drop.age < MOST_AGE;
            };
            if drop.volume > 0.0 {
                landed.push(LiquidLanding { at, normal, kind: drop.kind, volume: drop.volume, velocity: drop.velocity });
            }
            false
        });
    }
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
            drops.step(0.1, |_, _| None, &mut landed);
        }
        let drop = drops.list[0];
        assert!((drop.at.y + 0.5 * GRAVITY * 0.25).abs() < 1e-2, "{}", drop.at.y);
        assert!((drop.velocity.y + GRAVITY * 0.5).abs() < 1e-2);
        for _ in 0..40 {
            drops.step(0.1, |_, _| None, &mut landed);
        }
        assert!(drops.list.is_empty() && landed.is_empty());
    }

    #[test]
    fn a_landing_pours_once_and_leaves_nothing_behind() {
        let mut drops = falling(Vec3::new(0.0, 10.0, 0.0), Vec3::new(0.0, -300.0, 0.0), 2.0);
        let mut landed = Vec::new();
        drops.step(0.1, floor, &mut landed);
        assert_eq!(landed.len(), 1);
        assert!(landed[0].at.y.abs() < 1e-4, "{}", landed[0].at.y);
        assert_eq!((landed[0].normal, landed[0].volume), (Vec3::Y, 2.0));
        assert!(drops.list.is_empty());
    }

    #[test]
    fn a_drop_that_is_only_seen_pours_nothing() {
        let mut drops = falling(Vec3::new(0.0, 0.5, 0.0), Vec3::ZERO, 0.0);
        let mut landed = Vec::new();
        drops.step(0.05, floor, &mut landed);
        assert!(drops.list.is_empty() && landed.is_empty());
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
