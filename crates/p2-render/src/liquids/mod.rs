//! Modern graphics: our own liquids. The scripts still decide where a stream lands, where puddles and trails lie and
//! what burns; with `MODERN_LIQUIDS` above 0 streams, spurts and drips are simulated in world space against the level
//! and drawn in place of the scripts' strip and droplet sprites, and what lands pools on the floor.

use bevy::app::{App, Plugin};
use bevy::ecs::message::Message;
use bevy::math::Vec3;
use p2_vm::FluidType;

pub mod air_draw;
pub mod drops;
pub mod pool_draw;
pub mod pools;
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
}

pub struct LiquidsPlugin;

impl Plugin for LiquidsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<LiquidLanding>();
        app.add_plugins((world::LiquidWorldPlugin, pool_draw::PoolDrawPlugin, air_draw::AirLiquidsPlugin));
    }
}
