//! Retail shrinks a hidden sound's radius, never its volume: to 0.35, and by 0.85 to the square of the zone steps.
//! A later change glides over a second.

use bevy::ecs::prelude::{Component, Resource};
use p2_math::Vector;
use p2_render::world::level_collision::StaticCollision;
use p2_vm::ESoundOcclusion;

const HIDDEN_SHARE: f32 = 0.35;
const ZONE_FACTOR: f32 = 0.85;
const GLIDE_SECONDS: f32 = 1.0;
const LEAST_RADIUS: f32 = 1.0;

#[derive(Component)]
pub(crate) struct Muffling {
    occlusion: ESoundOcclusion,
    from: f32,
    to: Option<f32>,
    elapsed: f32,
}

impl Muffling {
    pub(super) fn new(occlusion: ESoundOcclusion) -> Self {
        Self { occlusion, from: 0.0, to: None, elapsed: GLIDE_SECONDS }
    }

    #[expect(clippy::too_many_arguments, reason = "the level, its cached zone steps, the two places and the clock")]
    pub(super) fn heard_radius(&mut self, level: &StaticCollision, steps: &mut ZoneSteps, generation: u64, sound: Vector, listener: Vector, radius: f32, seconds: f32) -> f32 {
        let mut target = radius;
        if !audible(level, self.occlusion, sound, listener) {
            let zones = [sound, listener].map(|at| p2_collision::point_zone(&level.model, at));
            let hops = f32::from(steps.between(level, generation, zones));
            target *= ZONE_FACTOR.powf(hops * hops) * HIDDEN_SHARE;
        }
        self.glide(target, seconds)
    }

    fn now(&self) -> f32 {
        let to = self.to.unwrap_or(self.from);
        (self.from + (to - self.from) * (self.elapsed / GLIDE_SECONDS).min(1.0)).max(LEAST_RADIUS)
    }

    fn glide(&mut self, target: f32, seconds: f32) -> f32 {
        match self.to {
            None => {
                self.from = target;
                self.to = Some(target);
            }
            Some(to) if to != target => {
                self.from = self.now();
                self.to = Some(target);
                self.elapsed = 0.0;
            }
            Some(_) => {}
        }
        self.elapsed += seconds;
        self.now()
    }
}

#[derive(Resource, Default)]
pub(crate) struct ZoneSteps {
    level: Option<(u64, usize)>,
    zones: usize,
    hops: Vec<u8>,
}

impl ZoneSteps {
    fn between(&mut self, level: &StaticCollision, generation: u64, [a, b]: [u8; 2]) -> u8 {
        let key = Some((generation, std::ptr::from_ref(level).addr()));
        if self.level != key {
            self.hops = p2_collision::zone_hops(&level.model);
            self.zones = level.model.zones.len().max(1);
            self.level = key;
        }
        let (a, b) = (usize::from(a), usize::from(b));
        if a < self.zones && b < self.zones { self.hops[a * self.zones + b] } else { u8::MAX }
    }
}

fn audible(level: &StaticCollision, occlusion: ESoundOcclusion, sound: Vector, listener: Vector) -> bool {
    match occlusion {
        ESoundOcclusion::None => true,
        ESoundOcclusion::Default | ESoundOcclusion::Bsp => p2_collision::line_check(&level.model, sound, listener).is_none(),
        ESoundOcclusion::StaticMeshes | ESoundOcclusion::All => level.first_hit(sound, listener).is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sound_starts_at_its_radius_then_glides_to_a_new_one_over_a_second() {
        let mut muffling = Muffling::new(ESoundOcclusion::Default);
        assert_eq!(muffling.glide(35.0, 0.0), 35.0);
        assert_eq!(muffling.glide(35.0, 0.1), 35.0);
        assert!((muffling.glide(100.0, 0.5) - 67.5).abs() < 1e-3);
        assert!((muffling.glide(100.0, 0.5) - 100.0).abs() < 1e-3);
        assert!((muffling.glide(35.0, 0.25) - 83.75).abs() < 1e-3);
        assert_eq!(muffling.glide(0.0, 2.0), LEAST_RADIUS);
    }
}
