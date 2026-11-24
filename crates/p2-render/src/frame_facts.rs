use bevy::ecs::prelude::Resource;
use bevy::math::Vec3;
use p2_math::{Rotator, Vector};
use p2_render_cpu::LightEffect;
use p2_vm::{FluidHit, ObjectId, PersonHit};
use rustc_hash::FxHashMap;
use crate::scene::ZoneFog;

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
    /// Moves on whenever `cull_distances` changes, so a reader can tell without comparing the map.
    pub cull_version: u32,
    pub person_hits: Vec<PersonHit>,
    pub fluid_hits: Vec<FluidHit>,
    /// The level's clock in game seconds: it stands while the game is paused.
    pub level_time: f32,
    pub glows: Vec<GlowSource>,
    /// The view's horizontal field of view in degrees, as Unreal gives it (0: 90).
    pub fov: f32,
    /// The fog of the volume the eye is in; it comes before the zone's.
    pub volume_fog: Option<ZoneFog>,
}

impl FrameFacts {
    /// The game seconds this frame moves on: none while paused.
    pub fn step(&self) -> f32 {
        if self.paused { 0.0 } else { self.game_step }
    }

    /// `real` host seconds, at most `most`, while the game runs: for motion that keeps to the host's clock.
    pub fn capped_step(&self, real: f32, most: f32) -> f32 {
        if self.paused { 0.0 } else { real.min(most) }
    }
}

/// What the HUD's frame told the view; the overlay cameras take it in the frame it is written.
#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct HudView {
    /// A horizontal field of view in degrees, as Unreal gives it, per actor the HUD drew this frame (`DrawActor`), in
    /// call order (0: the view's); each is drawn by the overlay camera of its slot.
    pub overlays: Vec<f32>,
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
