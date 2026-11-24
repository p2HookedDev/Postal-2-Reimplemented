use bevy::math::Vec4;

use super::*;

/// A sprite lying on the ground as a disc: its centre in the world and the smaller of its half sides.
fn puddle_disc(sprite: &SpriteParticle, origin: Vec3) -> Vec4 {
    (Vec3::from(to_bevy(sprite.location)) + origin).extend(sprite.up.size().min(sprite.right.size()))
}

fn nearest_discs(mut discs: Vec<Vec4>, eye: Vec3, cap: usize) -> Vec<Vec4> {
    discs.sort_by(|a, b| eye.distance_squared(a.truncate()).total_cmp(&eye.distance_squared(b.truncate())));
    discs.truncate(cap);
    discs
}

impl Particles {
    /// The drawn puddles' sprites as discs, the `cap` nearest to `eye`, for the reflection pass to mirror.
    pub fn puddle_discs(&self, origin: Vec3, eye: Vec3, cap: usize) -> Vec<Vec4> {
        let puddles = self.actors.iter().filter(|p| p.drawn && p.overlay.is_none() && p.puddle_seed.is_some());
        let sprites = puddles.flat_map(|p| p.buffers.particles.iter().flat_map(|(_, sprites)| sprites));
        nearest_discs(sprites.map(|sprite| puddle_disc(sprite, origin)).collect(), eye, cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_discs_are_kept_nearest_first() {
        let disc = |x: f32| Vec4::new(x, 0.0, 0.0, 10.0);
        assert_eq!(nearest_discs(vec![disc(50.0), disc(-5.0), disc(20.0)], Vec3::ZERO, 2), [disc(-5.0), disc(20.0)]);
        assert_eq!(nearest_discs(Vec::new(), Vec3::ZERO, 2), []);
    }
}
