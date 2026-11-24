use bevy::asset::Handle;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::Resource;
use bevy::image::Image;
use bevy::math::{Vec3, Vec4};
use p2_bevy_assets::to_bevy;
use p2_math::Vector;
use p2_particles::{Beam, EmitterKind, EmitterProperties, SpriteParticle, StripPoint};

/// What the particle simulation shows this frame, one slot per emitter actor; the slots and their vectors are refilled
/// in place every frame.
#[derive(Resource, Default)]
pub struct DrawnParticles {
    pub actors: Vec<DrawnActor>,
    /// The emitters spawned since the last frame drawn, once each.
    pub born: Vec<EmitterLook>,
    /// Bevy coordinates relative to the world's origin, as the particles.
    pub eye: Vec3,
    /// Changed when a map is left: the drawing side drops what it keeps.
    pub level: u32,
}

/// A slot is a place, not an identity: `parent` says whose particles it holds this frame.
pub struct DrawnActor {
    pub parent: Entity,
    pub seen: bool,
    pub drawn: bool,
    pub overlay: Option<usize>,
    pub puddle_seed: Option<u32>,
    /// Where the actor stands this frame; `None` before it is placed.
    pub place: Option<Vector>,
    /// The box of the actor's mesh particles, which its sprites' bounds start from.
    pub mesh_reach: Option<(Vec3, Vec3)>,
    pub emitters: Vec<DrawnEmitter>,
}

impl Default for DrawnActor {
    fn default() -> Self {
        Self {
            parent: Entity::PLACEHOLDER,
            seen: false,
            drawn: false,
            overlay: None,
            puddle_seed: None,
            place: None,
            mesh_reach: None,
            emitters: Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct DrawnEmitter {
    pub sprites: Vec<SpriteParticle>,
    pub strip: Vec<StripPoint>,
    pub beams: Vec<Beam>,
    pub shape: EmitterShape,
}

#[derive(Clone, Copy, Default)]
pub struct EmitterShape {
    pub kind: EmitterKind,
    pub facing: Option<Vec3>,
    pub beam_scale: [f32; 2],
    pub sheets: usize,
}

/// What an emitter with a texture is drawn with, handed over once when it spawns.
pub struct EmitterLook {
    pub parent: Entity,
    pub order: u32,
    pub image: Handle<Image>,
    pub props: EmitterProperties,
    pub name: String,
    pub fogless: bool,
}

/// A sprite lying on the ground as a disc: its centre in the world and the smaller of its half sides.
fn puddle_disc(sprite: &SpriteParticle, origin: Vec3) -> Vec4 {
    (Vec3::from(to_bevy(sprite.location)) + origin).extend(sprite.up.size().min(sprite.right.size()))
}

fn nearest_discs(mut discs: Vec<Vec4>, eye: Vec3, cap: usize) -> Vec<Vec4> {
    discs.sort_by(|a, b| eye.distance_squared(a.truncate()).total_cmp(&eye.distance_squared(b.truncate())));
    discs.truncate(cap);
    discs
}

impl DrawnParticles {
    /// The drawn puddles' sprites as discs, the `cap` nearest to `eye`, for the reflection pass to mirror.
    pub fn puddle_discs(&self, origin: Vec3, eye: Vec3, cap: usize) -> Vec<Vec4> {
        let puddles = self.actors.iter().filter(|slot| slot.drawn && slot.overlay.is_none() && slot.puddle_seed.is_some());
        let sprites = puddles.flat_map(|slot| slot.emitters.iter().flat_map(|emitter| &emitter.sprites));
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
