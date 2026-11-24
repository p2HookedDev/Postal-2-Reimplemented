use bevy::ecs::entity::Entity;
use bevy::math::Vec3;
use p2_particles::{DrawStyle, SpriteParticle};
use rustc_hash::FxHashSet;

use super::sprites::sprite_box;
use super::ParticleDraws;
use crate::fire_volume::FireBox;
use crate::particle_frame::{DrawnEmitter, DrawnParticles};

const FIRE_WORDS: [&str; 2] = ["fire", "flame"];
const NEVER_FIRE_WORDS: [&str; 5] = ["muzzle", "flash", "spark", "smoke", "ember"];

/// Told by the texture's name and an adding blend, as retail draws flames; anything else under a fire actor (smoke,
/// embers, glows) stays sprites.
pub(crate) fn is_fire(texture: &str, style: DrawStyle) -> bool {
    let texture = texture.to_ascii_lowercase();
    let named = |word: &&str| texture.contains(*word);
    matches!(style, DrawStyle::Translucent | DrawStyle::Brighten) && FIRE_WORDS.iter().any(named) && !NEVER_FIRE_WORDS.iter().any(named)
}

/// The volume draws one rising column; a fire spread wider than it is tall, such as a burning trail, stays sprites.
pub(crate) fn is_column(fire: &FireBox) -> bool {
    fire.half.x.max(fire.half.z) <= 2.0 * fire.half.y
}

/// The indices of the nearest `cap` distances within `reach`, nearest first.
pub(crate) fn nearest_fires(distances: &[f32], cap: usize, reach: f32) -> Vec<usize> {
    let mut near: Vec<usize> = (0..distances.len()).filter(|&at| distances[at] <= reach).collect();
    near.sort_by(|&a, &b| distances[a].total_cmp(&distances[b]));
    near.truncate(cap);
    near
}

/// The box around the sprites, the alpha-weighted mean of their colour and their mean alpha.
pub(crate) fn fire_box(sprites: &[SpriteParticle], origin: Vec3) -> Option<FireBox> {
    let (mut lo, mut hi) = sprites.first().map(sprite_box)?;
    let (mut colour, mut alpha) = (Vec3::ZERO, 0.0);
    for sprite in sprites {
        let (min, max) = sprite_box(sprite);
        (lo, hi) = (lo.min(min), hi.max(max));
        let [r, g, b, a] = sprite.color.map(|c| f32::from(c) / 255.0);
        colour += Vec3::new(r, g, b) * a;
        alpha += a;
    }
    let tint = if alpha > 0.0 { colour / alpha } else { Vec3::ONE };
    Some(FireBox { centre: (lo + hi) / 2.0 + origin, half: (hi - lo) / 2.0, tint: tint.extend(alpha / sprites.len() as f32) })
}

impl ParticleDraws {
    /// The fires in `hidden`, whose sprites were hidden this frame, nearest first, to be drawn as volumes; and the
    /// nearest fires drawn this frame, at most `cap` within `reach` of `eye`, to hide next frame.
    pub fn fire_volumes(
        &self,
        drawn: &DrawnParticles,
        hidden: &FxHashSet<(Entity, u32)>,
        origin: Vec3,
        eye: Vec3,
        cap: usize,
        reach: f32,
    ) -> (Vec<FireBox>, FxHashSet<(Entity, u32)>) {
        let (mut volumes, mut candidates, mut distances) = (Vec::new(), Vec::new(), Vec::new());
        for slot in &drawn.actors {
            let Some(actor) = self.actors.get(&slot.parent) else { continue };
            let eligible = slot.drawn && slot.overlay.is_none();
            for (order, (draw, DrawnEmitter { sprites, .. })) in actor.emitters.iter().zip(&slot.emitters).enumerate() {
                if !draw.fire || draw.grid.is_none() {
                    continue;
                }
                let Some(fire) = fire_box(sprites, origin) else { continue };
                let (key, distance) = ((slot.parent, order as u32), eye.distance(fire.centre));
                if hidden.contains(&key) {
                    volumes.push((distance, fire));
                }
                if eligible && is_column(&fire) {
                    candidates.push(key);
                    distances.push(distance);
                }
            }
        }
        let next = nearest_fires(&distances, cap, reach).into_iter().map(|chosen| candidates[chosen]).collect();
        volumes.sort_by(|a, b| a.0.total_cmp(&b.0));
        (volumes.into_iter().take(cap).map(|(_, fire)| fire).collect(), next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fire_is_told_from_smoke_and_muzzle_flashes() {
        assert!(is_fire("nathans.Effects.flames1", DrawStyle::Translucent));
        assert!(is_fire("Zo_Smeg.fireball", DrawStyle::Brighten));
        assert!(!is_fire("nathans.Effects.flames1", DrawStyle::AlphaBlend));
        assert!(!is_fire("nathans.Effects.smoke2", DrawStyle::Translucent));
        assert!(!is_fire("nathans.Effects.fire_smoke", DrawStyle::Translucent));
        assert!(!is_fire("fx.fire_flash", DrawStyle::Translucent));
        assert!(!is_fire("fx.fire_embers", DrawStyle::Translucent));
        assert!(!is_fire("fx.blood", DrawStyle::AlphaBlend));
        assert!(!is_fire("-", DrawStyle::Translucent));
    }

    #[test]
    fn only_a_column_becomes_a_volume() {
        let fire = |half: Vec3| FireBox { centre: Vec3::ZERO, half, tint: Vec3::ONE.extend(1.0) };
        assert!(is_column(&fire(Vec3::new(40.0, 60.0, 40.0))));
        assert!(!is_column(&fire(Vec3::new(400.0, 40.0, 30.0))));
    }

    #[test]
    fn the_nearest_within_reach_are_picked_up_to_the_cap() {
        let distances = [900.0, 100.0, 5000.0, 300.0, 200.0];
        assert_eq!(nearest_fires(&distances, 3, 1000.0), vec![1, 4, 3]);
        assert_eq!(nearest_fires(&distances, 8, 1000.0), vec![1, 4, 3, 0]);
        assert!(nearest_fires(&distances, 0, 1000.0).is_empty());
        assert!(nearest_fires(&distances, 8, 50.0).is_empty());
    }

    #[test]
    fn no_sprites_make_no_volume() {
        assert_eq!(fire_box(&[], Vec3::ZERO), None);
    }
}
