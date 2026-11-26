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

#[derive(Message, Debug, Clone, Copy, PartialEq)]
pub struct LiquidLanding {
    /// Bevy coordinates relative to the world's origin.
    pub at: Vec3,
    pub normal: Vec3,
    pub kind: FluidType,
    pub volume: f32,
    /// Bevy coordinates a second, as it landed.
    pub velocity: Vec3,
}

#[derive(Message, Debug, Clone, Copy, PartialEq)]
pub struct LiquidSpray {
    pub at: Vec3,
    pub velocity: Vec3,
    pub radius: f32,
    pub kind: FluidType,
    pub source: u64,
}

#[derive(Resource, Default, Clone, PartialEq)]
pub struct LiquidTextures {
    pub puke_splat: Option<Handle<Image>>,
    pub puke_pour: Option<Handle<Image>>,
    pub puke_splash: Option<Handle<Image>>,
}

/// `PUKE_SPLAT` in `projector.wesl`; the splat rides in the decal's metallic-roughness slot.
pub const PUKE_SPLAT_TAG: u32 = 0x8000_0000 | 0x40_0000 | 7;

pub fn is_puke(kind: FluidType) -> bool {
    matches!(kind, FluidType::Puke | FluidType::BloodyPuke)
}

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
    use crate::gpu::shaders::shader_const;

    #[test]
    fn the_shader_knows_the_puke_splat_tag() {
        let projector = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/shaders/projector.wesl"));
        let low = shader_const(projector, "PROJECTOR") as u32 | shader_const(projector, "FB_LIQUID") as u32;
        let bit = shader_const(projector, "PUKE_SPLAT") as u32;
        assert_eq!(PUKE_SPLAT_TAG, low | bit);
    }
}
