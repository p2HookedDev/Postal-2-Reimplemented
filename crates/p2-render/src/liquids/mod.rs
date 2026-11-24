//! Modern graphics: our own liquids. The scripts still decide where a stream lands, where puddles and trails lie and
//! what burns; with `MODERN_LIQUIDS` above 0 the stream is a GPU particle stream and its surface is drawn in screen
//! space in place of the scripts' strip and droplet sprites.

use bevy::app::{App, Plugin};
use bevy::ecs::prelude::Resource;
use bevy::ecs::schedule::{IntoScheduleConfigs, SystemSet};
use bevy::render::render_resource::Buffer;
use bevy::render::RenderApp;
use bevy::core_pipeline::schedule::Core3d;

use crate::MainPassExtraSystems;

pub mod stream;
pub mod surface;

/// Words of one `LiquidParticle` in `liquid_stream.wesl`: place and radius, velocity and the `LIQUID_*` bits word.
pub const LIQUID_PARTICLE_WORDS: usize = 8;
/// The low four bits of the bits word: the `FluidType` byte.
pub const LIQUID_KIND_MASK: u32 = 0xF;
/// Set once the particle lies on a surface.
pub const LIQUID_LANDED: u32 = 1 << 4;
pub const LIQUID_BURNING: u32 = 1 << 5;

/// The render world's liquid particles, stepped by `stream` and drawn by `surface`. Absent while no liquid lives.
#[derive(Resource)]
pub struct LiquidParticles {
    pub buffer: Buffer,
    pub capacity: u32,
    /// Particles from the start of `buffer` the surface pass draws; 0 draws nothing.
    pub live: u32,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum LiquidPassSystems {
    Step,
    Surface,
}

pub struct LiquidsPlugin;

impl Plugin for LiquidsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((stream::LiquidStreamPlugin, surface::LiquidSurfacePlugin));
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.configure_sets(Core3d, (LiquidPassSystems::Step, LiquidPassSystems::Surface).chain().in_set(MainPassExtraSystems::Liquids));
        }
    }
}
