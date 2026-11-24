use bevy::ecs::prelude::Resource;
use bevy::math::Vec3;
use p2_math::{Rotator, Vector};
use p2_render_cpu::LightEffect;
use p2_vm::{ObjectId, PersonHit};
use rustc_hash::FxHashMap;

/// What the game told the view for this frame; the default is no game in play.
#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct FrameFacts {
    pub in_play: bool,
    pub paused: bool,
    pub game_step: f32,
    pub origin: Vec3,
    pub cutscene: bool,
    pub eye_to_view_target: Option<f32>,
    /// Only the actors whose distance is above 0: the others cull nothing.
    pub cull_distances: FxHashMap<ObjectId, f32>,
    pub person_hits: Vec<PersonHit>,
    /// The level's clock in game seconds: it stands while the game is paused.
    pub level_time: f32,
    pub glows: Vec<GlowSource>,
}

/// A fire or a gun's flash the game shows, whose light its scripts leave off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlowSource {
    pub location: Vector,
    pub kind: Glow,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Glow {
    /// `life_left`: its `LifeSpan` in game seconds, 0 for a fire that burns on.
    Fire { life_left: f32 },
    /// Hung on a weapon for the ticks of a shot, in the light its script picks at each one.
    ShotFlash(ScriptLight),
    /// `age`: game seconds since it was spawned.
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
