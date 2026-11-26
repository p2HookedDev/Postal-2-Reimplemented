use bevy::ecs::prelude::Resource;
use bevy::math::Vec3;
use p2_math::{Rotator, Vector};
use p2_render_cpu::LightEffect;
use p2_vm::{FluidHit, ObjectId, PersonHit};
use rustc_hash::FxHashMap;
use crate::world::scene::ZoneFog;

#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct FrameFacts {
    pub in_play: bool,
    pub paused: bool,
    pub game_step: f32,
    pub origin: Vec3,
    pub cutscene: bool,
    pub eye_to_view_target: Option<f32>,
    pub cull_distances: FxHashMap<ObjectId, f32>,
    pub cull_version: u32,
    pub person_hits: Vec<PersonHit>,
    pub fluid_hits: Vec<FluidHit>,
    pub level_time: f32,
    pub glows: Vec<GlowSource>,
    /// The view's horizontal field of view in degrees, as Unreal gives it (0: 90).
    pub fov: f32,
    pub volume_fog: Option<ZoneFog>,
}

impl FrameFacts {
    pub fn step(&self) -> f32 {
        if self.paused { 0.0 } else { self.game_step }
    }

    pub fn capped_step(&self, real: f32, most: f32) -> f32 {
        if self.paused { 0.0 } else { real.min(most) }
    }
}

#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct HudView {
    pub overlays: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlowSource {
    pub location: Vector,
    pub kind: Glow,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Glow {
    Fire { life_left: f32 },
    ShotFlash(ScriptLight),
    Flash { light: ScriptLight, age: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScriptLight {
    pub rotation: Rotator,
    pub effect: LightEffect,
    pub brightness: f32,
    pub hue: u8,
    pub saturation: u8,
    pub radius: f32,
    pub cone: u8,
}
