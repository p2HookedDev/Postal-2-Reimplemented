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

/// How far the occlusion-culling `Aabb` reaches past the particle bounds (Unreal units). Flat sprites on a
/// surface (puddles, splats) lie in it or a hair under it, and the culling shader compares the box's nearest
/// depth with the surface's own: from steep angles the floor is in front of the box and the puddle vanishes.
pub const OCCLUSION_MARGIN: f32 = 8.0;

#[derive(Resource, Default)]
pub struct DrawnParticles {
    pub actors: Vec<DrawnActor>,
    pub born: Vec<EmitterLook>,
    pub born_meshes: Vec<MeshEmitterLook>,
    pub blasts: Vec<Blast>,
    pub eye: Vec3,
    /// Changed when a map is left: the drawing side drops what it keeps.
    pub level: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Blast {
    pub at: Vector,
    pub radius: f32,
    /// The speed it gives a piece at its middle, in Unreal units a second; none at `radius`.
    pub push: f32,
    pub way: Vector,
}

/// A shot's reach in Unreal units and its push in Unreal units a second.
pub const SHOT_PUSH: (f32, f32) = (48.0, 450.0);

/// A shot or a kick that hits a person throws what lies about and what it throws itself along its way: the push
/// of an effect spawned where it hit takes that way, else the hit pushes as a shot. A hit stays readable for a
/// game tick, so one already pushed is skipped.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidRole {
    Stream,
    Drip,
    Puddle,
    Trail,
    Flow,
    Splash,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Liquid {
    pub kind: FluidType,
    pub role: LiquidRole,
    pub burning: bool,
    pub pour: Option<Pour>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pour {
    pub from: Vector,
    pub velocity: [Vector; 2],
    pub acceleration: Vector,
    pub rate: f32,
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
    pub place: Option<Vector>,
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

pub struct EmitterLook {
    pub parent: Entity,
    pub order: u32,
    pub image: Handle<Image>,
    pub props: EmitterProperties,
    pub name: String,
    pub fogless: bool,
}

pub struct MeshEmitterLook {
    pub parent: Entity,
    pub order: u32,
    pub sections: Vec<SectionView>,
    pub props: EmitterProperties,
}

fn puddle_disc(sprite: &SpriteParticle, origin: Vec3) -> Vec4 {
    (Vec3::from(to_bevy(sprite.location)) + origin).extend(sprite.up.size().min(sprite.right.size()))
}

fn nearest_discs(mut discs: Vec<Vec4>, eye: Vec3, cap: usize) -> Vec<Vec4> {
    discs.sort_by(|a, b| eye.distance_squared(a.truncate()).total_cmp(&eye.distance_squared(b.truncate())));
    discs.truncate(cap);
    discs
}

impl DrawnParticles {
    pub fn leave_level(&mut self) {
        self.born.clear();
        self.born_meshes.clear();
        self.blasts.clear();
        self.level = self.level.wrapping_add(1);
    }

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
