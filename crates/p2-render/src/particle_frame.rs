use bevy::asset::Handle;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Local, Res, ResMut, Resource};
use bevy::image::Image;
use bevy::math::{Vec3, Vec4};
use p2_bevy_assets::to_bevy;
use p2_math::Vector;
use p2_particles::{Beam, EmitterKind, EmitterProperties, MeshParticle, SpriteParticle, StripPoint};
use p2_vm::FluidType;
use crate::frame_facts::FrameFacts;
use crate::scene::SectionView;

/// How far the occlusion-culling `Aabb` reaches past the particle bounds (Unreal units). Flat sprites on a surface
/// (puddles, splats) lie in it or a hair under it, and the culling shader compares the box's nearest depth with the
/// surface's own: from steep angles the floor is in front of the box and the puddle vanishes.
pub const OCCLUSION_MARGIN: f32 = 8.0;

/// What the particle simulation shows this frame, one slot per emitter actor; the slots and their vectors are refilled
/// in place every frame.
#[derive(Resource, Default)]
pub struct DrawnParticles {
    pub actors: Vec<DrawnActor>,
    /// The emitters spawned since the last frame drawn, once each.
    pub born: Vec<EmitterLook>,
    /// The mesh emitters whose model was built since the last frame drawn, once each.
    pub born_meshes: Vec<MeshEmitterLook>,
    /// The pushes the game gave since the last frame drawn, once each.
    pub blasts: Vec<Blast>,
    /// Bevy coordinates relative to the world's origin, as the particles.
    pub eye: Vec3,
    /// Changed when a map is left: the drawing side drops what it keeps.
    pub level: u32,
}

/// What an explosion or a shot's impact does to loose pieces around it, in Unreal units and axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Blast {
    pub at: Vector,
    pub radius: f32,
    /// The speed it gives a piece at its middle, in Unreal units a second; none at `radius`.
    pub push: f32,
    /// The hit's way, of length 1, in Unreal axes; zero for a push out of its middle alone.
    pub way: Vector,
}

/// A shot's reach in Unreal units and its push in Unreal units a second.
pub const SHOT_PUSH: (f32, f32) = (48.0, 450.0);

/// A shot or a kick that hits a person throws what lies about and what it throws itself along its way: the push of an
/// effect spawned where it hit takes that way, else the hit pushes as a shot. A hit stays readable for a game tick, so
/// one already pushed is skipped.
pub(crate) fn push_from_person_hits(facts: Res<FrameFacts>, drawn: Option<ResMut<DrawnParticles>>, mut pushed: Local<Vec<[u32; 3]>>) {
    let Some(mut drawn) = drawn else { return };
    let hits: Vec<_> = facts.person_hits.iter().filter(|hit| hit.damage > 0.0).collect();
    let fresh: Vec<_> = hits.iter().filter(|hit| !pushed.contains(&<[f32; 3]>::from(hit.at).map(f32::to_bits))).collect();
    if !fresh.is_empty() {
        let drawn = &mut *drawn;
        for hit in fresh {
            let way = if hit.momentum.size() > 1e-3 { hit.momentum / hit.momentum.size() } else { Vector::ZERO };
            let (radius, push) = SHOT_PUSH;
            match drawn.blasts.iter_mut().find(|blast| (blast.at - hit.at).size() < blast.radius.max(radius)) {
                Some(blast) => blast.way = way,
                None => drawn.blasts.push(Blast { at: hit.at, radius, push, way }),
            }
        }
    }
    pushed.clear();
    pushed.extend(hits.iter().map(|hit| <[f32; 3]>::from(hit.at).map(f32::to_bits)));
}

/// What a liquid actor of the scripts shows with its emitters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidRole {
    /// A feeder pouring or spurting: its strip and droplets are the stream.
    Stream,
    /// A feeder dripping from a ledge or a wound.
    Drip,
    Puddle,
    Trail,
    /// A trail the scripts slide down a wall under a still aim.
    Flow,
    /// Droplets bursting where a stream lands.
    Splash,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Liquid {
    pub kind: FluidType,
    pub role: LiquidRole,
    /// The scripts' `bOnFire`, read every frame.
    pub burning: bool,
    /// A stream's or drip's aim this frame; `None` for puddles and trails.
    pub pour: Option<Pour>,
}

/// How the scripts aim a feeder this frame, from its first emitter, world axes and Unreal units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pour {
    /// Where its particles start: the feeder's place.
    pub from: Vector,
    /// `StartVelocityRange`'s low and high ends, in Unreal units a second.
    pub velocity: [Vector; 2],
    pub acceleration: Vector,
    /// Particles a second the emitter spawns while it flows.
    pub rate: f32,
    /// False once the scripts stopped the flow (`ToggleFlow` off, `bStoppedFlow`).
    pub flowing: bool,
}

/// A slot is a place, not an identity: `parent` says whose particles it holds this frame.
pub struct DrawnActor {
    pub parent: Entity,
    pub seen: bool,
    pub drawn: bool,
    pub overlay: Option<usize>,
    pub puddle_seed: Option<u32>,
    pub liquid: Option<Liquid>,
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
            liquid: None,
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
    pub meshes: Vec<MeshParticle>,
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

/// What a mesh emitter is drawn with, handed over once when its model is built.
pub struct MeshEmitterLook {
    pub parent: Entity,
    pub order: u32,
    pub sections: Vec<SectionView>,
    pub props: EmitterProperties,
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
    /// The once-each lists are dropped and `level` moves on; the slots are refilled by the next frame.
    pub fn leave_level(&mut self) {
        self.born.clear();
        self.born_meshes.clear();
        self.blasts.clear();
        self.level = self.level.wrapping_add(1);
    }

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
