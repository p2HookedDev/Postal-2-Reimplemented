use super::*;
use crate::render::fire_volume::FireBox;

const FIRE_WORDS: [&str; 2] = ["fire", "flame"];
const NEVER_FIRE_WORDS: [&str; 3] = ["muzzle", "flash", "spark"];

/// Told by name, as the class is not kept: the actor's object name carries its class, the texture its look. A smoke
/// texture under a fire actor stays a sprite.
pub(super) fn is_fire(actor: &str, texture: &str) -> bool {
    let (actor, texture) = (actor.to_ascii_lowercase(), texture.to_ascii_lowercase());
    let named = |words: &[&str]| words.iter().any(|word| actor.contains(word) || texture.contains(word));
    named(&FIRE_WORDS) && !named(&NEVER_FIRE_WORDS) && !texture.contains("smoke")
}

/// The indices of the nearest `cap` distances within `reach`, nearest first.
pub(super) fn nearest_fires(distances: &[f32], cap: usize, reach: f32) -> Vec<usize> {
    let mut near: Vec<usize> = (0..distances.len()).filter(|&at| distances[at] <= reach).collect();
    near.sort_by(|&a, &b| distances[a].total_cmp(&distances[b]));
    near.truncate(cap);
    near
}

/// The box around the sprites, the alpha-weighted mean of their colour and their mean alpha.
pub(super) fn fire_box(sprites: &[SpriteParticle], origin: Vec3) -> Option<FireBox> {
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

impl Particles {
    /// The fires whose sprites were hidden this frame, nearest first, to be drawn as volumes; then the nearest fires
    /// drawn this frame, at most `cap` within `reach` of `eye`, are the ones hidden next frame. A `cap` of 0 hands every
    /// fire back to its sprites.
    pub fn fire_volumes(&mut self, origin: Vec3, eye: Vec3, cap: usize, reach: f32) -> Vec<FireBox> {
        let (mut hidden, mut candidates, mut distances) = (Vec::new(), Vec::new(), Vec::new());
        for (at, p) in self.actors.iter().enumerate() {
            let eligible = p.drawn && p.overlay.is_none();
            for (index, (draw, (_, sprites))) in p.draws.iter().zip(&p.buffers.particles).enumerate() {
                if !draw.fire || draw.grid.is_none() {
                    continue;
                }
                let Some(fire) = fire_box(sprites, origin) else { continue };
                let distance = eye.distance(fire.centre);
                if draw.volume {
                    hidden.push((distance, fire));
                }
                if eligible {
                    candidates.push((at, index));
                    distances.push(distance);
                }
            }
        }
        for draw in self.actors.iter_mut().flat_map(|p| p.draws.iter_mut()) {
            draw.volume = false;
        }
        for chosen in nearest_fires(&distances, cap, reach) {
            let (at, index) = candidates[chosen];
            if let Some(draw) = self.actors.get_mut(at).and_then(|p| p.draws.get_mut(index)) {
                draw.volume = true;
            }
        }
        hidden.sort_by(|a, b| a.0.total_cmp(&b.0));
        hidden.into_iter().take(cap).map(|(_, fire)| fire).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fire_is_told_from_smoke_and_muzzle_flashes() {
        assert!(is_fire("Transient.FireEmitter3", "nathans.Effects.flames1"));
        assert!(is_fire("Transient.GasTrailEmitter0", "Zo_Smeg.fireball"));
        assert!(!is_fire("Transient.FireEmitter3", "nathans.Effects.smoke2"));
        assert!(!is_fire("Transient.MuzzleFlashEmitter1", "fx.fire_flash"));
        assert!(!is_fire("Transient.BloodSpray2", "fx.blood"));
        assert!(!is_fire("Transient.SparkEmitter0", "-"));
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
