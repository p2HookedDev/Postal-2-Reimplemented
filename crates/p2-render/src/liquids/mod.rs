//! Modern graphics: our own liquids. The scripts still decide where a stream lands, where puddles and trails lie and
//! what burns; with `MODERN_LIQUIDS` above 0 streams, spurts and drips are simulated in world space against the level
//! and drawn in place of the scripts' strip and droplet sprites, and what lands pools where it lands: floor, wall or
//! ceiling.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::Handle;
use bevy::ecs::bundle::Bundle;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::message::Message;
use bevy::ecs::prelude::{Entity, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::ClusteredDecal;
use bevy::math::Vec3;
use bevy::transform::components::Transform;
use p2_vm::FluidType;

pub mod air_draw;
pub mod drops;
pub mod layer;
pub mod layer_gpu;
pub mod spray;
pub mod stream_ribbon;
pub mod world;

/// Liquid that reached a surface this frame; drops and stream ends write it, the pools pour it in.
#[derive(Message, Debug, Clone, Copy, PartialEq)]
pub struct LiquidLanding {
    /// Bevy coordinates relative to the world's origin.
    pub at: Vec3,
    /// The surface's normal there, of length 1.
    pub normal: Vec3,
    pub kind: FluidType,
    /// In the pools' unit of volume.
    pub volume: f32,
    /// Bevy coordinates a second, as it landed.
    pub velocity: Vec3,
}

/// A heavy drop a hit throws, to fly as the other drops do and land in the pools or on a wall.
#[derive(Message, Debug, Clone, Copy, PartialEq)]
pub struct LiquidSpray {
    /// Bevy coordinates relative to the world's origin.
    pub at: Vec3,
    /// Bevy coordinates a second.
    pub velocity: Vec3,
    /// Unreal units.
    pub radius: f32,
    pub kind: FluidType,
    /// What threw it, as `Entity::to_bits`.
    pub source: u64,
}

/// The game's own puke textures, read from the player's install; without them puke keeps its procedural look.
#[derive(Resource, Default, Clone, PartialEq)]
pub struct LiquidTextures {
    pub puke_splat: Option<Handle<Image>>,
    pub puke_pour: Option<Handle<Image>>,
    pub puke_splash: Option<Handle<Image>>,
}

/// `PUKE_SPLAT` in `projector.wesl`: a liquid decal (frame buffer blending 7, so `project` skips it) that carries the
/// puke splat in its metallic-roughness slot.
pub const PUKE_SPLAT_TAG: u32 = 0x8000_0000 | 0x40_0000 | 7;

pub fn is_puke(kind: FluidType) -> bool {
    matches!(kind, FluidType::Puke | FluidType::BloodyPuke)
}

/// The puke splat's decal over the liquid decal `over`, in its box and despawned with it.
pub fn puke_decal(splat: &Handle<Image>, over: Entity) -> impl Bundle {
    (ClusteredDecal { metallic_roughness_texture: Some(splat.clone()), tag: PUKE_SPLAT_TAG, ..Default::default() }, Transform::IDENTITY, ChildOf(over))
}

pub struct LiquidsPlugin;

impl Plugin for LiquidsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<LiquidLanding>().add_message::<LiquidSpray>().init_resource::<LiquidTextures>();
        app.add_plugins((world::LiquidWorldPlugin, layer_gpu::LayerGpuPlugin, air_draw::AirLiquidsPlugin, spray::SprayPlugin));
        app.add_systems(PostUpdate, layer::mark_puke_tiles.after(world::step_liquids));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::shader_const;

    #[test]
    fn the_shader_knows_the_puke_splat_tag() {
        let projector = include_str!("../projector.wesl");
        let low = shader_const(projector, "PROJECTOR") as u32 | shader_const(projector, "FB_LIQUID") as u32;
        let bit = shader_const(projector, "PUKE_SPLAT") as u32;
        assert_eq!(PUKE_SPLAT_TAG, low | bit);
    }
}
