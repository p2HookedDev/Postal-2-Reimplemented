use bevy::ecs::prelude::{Query, Res, With};
use bevy::ecs::system::SystemParam;
use bevy::math::Vec3;
use bevy::transform::components::Transform;
use p2_bevy_assets::to_bevy;
use p2_math::Vector;

use super::sprites::{emitter_light, glow_at, lamps_shown};
use p2_host::display::{MODERN_FIRE_LIGHT, MODERN_PARTICLE_LIGHT, Settings};
use crate::level_collision::LevelCollision;
use crate::bsp_light::DynamicLightList;
use crate::frame_facts::FrameFacts;
use crate::lamps::{Lamp, Placed, lamp_sum};
use crate::particle_frame::DrawnActor;
use crate::view::ShownScene;

const LAMP_RECHECK_UNITS: f32 = 16.0;
const LAMP_RECHECK_FRAMES: u8 = 30;
/// A lamp's fitting often holds it inside its own collision, so the wall test stops this short of it.
const LAMP_CLEARANCE: f32 = 16.0;

/// An actor's light for lit sprites under modern graphics: the `emitter_light` at the emitter and its lamps' share
/// (`lamps_shown`), where and how many frames ago they were summed, and this frame's share of the script lights.
pub(super) struct ActorLight {
    lamp_light: Vec3,
    lamps_alone: Vec3,
    lamp_at: Vec3,
    lamp_age: u8,
    fire_light: Vec3,
    /// Only an actor drawn last frame is lit: the light is summed before this frame's drawing was known.
    drawn_before: bool,
}

impl Default for ActorLight {
    fn default() -> Self {
        Self { lamp_light: Vec3::ZERO, lamps_alone: Vec3::ZERO, lamp_at: Vec3::ZERO, lamp_age: u8::MAX, fire_light: Vec3::ZERO, drawn_before: true }
    }
}

impl ActorLight {
    /// The two lights of the emitters' header records: the emitter's, and its lamps' share alone.
    pub(super) fn header(&self) -> [Vec3; 2] {
        [self.lamp_light + self.fire_light, self.lamps_alone + self.fire_light]
    }

    fn due(&self, here: Vec3) -> bool {
        self.lamp_age >= LAMP_RECHECK_FRAMES || here.distance_squared(self.lamp_at) > LAMP_RECHECK_UNITS * LAMP_RECHECK_UNITS
    }
}

#[derive(SystemParam)]
pub struct ParticleLights<'w, 's> {
    facts: Res<'w, FrameFacts>,
    lamps: Query<'w, 's, (&'static Transform, &'static Placed), With<Lamp>>,
    shown: Option<Res<'w, ShownScene>>,
    collision: Option<Res<'w, LevelCollision>>,
    lights: DynamicLightList<'w>,
}

impl ParticleLights<'_, '_> {
    /// Whether this frame lights the emitters, and whether the script lights count too.
    pub(super) fn wanted(&self, settings: Option<&Settings>) -> Option<LitBy> {
        let level = |name: &str| settings.map_or(0, |settings| settings.modern_level(name));
        (self.facts.in_play && level(MODERN_PARTICLE_LIGHT) > 0).then(|| if level(MODERN_FIRE_LIGHT) > 0 { LitBy::ScriptLightsToo } else { LitBy::Lamps })
    }

    /// Takes the actor's slot of this frame: an actor drawn last frame gets one sum at its place, again when it moves or
    /// on a slow rota. A lamp a wall hides adds nothing. The script lights are read every frame.
    pub(super) fn follow(&self, light: &mut ActorLight, slot: &DrawnActor, glow: Option<LitBy>) {
        let drawn_before = std::mem::replace(&mut light.drawn_before, slot.drawn);
        let Some(glow) = glow.filter(|_| drawn_before) else { return };
        light.lamp_age = light.lamp_age.saturating_add(1);
        let Some(location) = slot.place else { return };
        let lighting = self.shown.as_deref().and_then(|shown| shown.lighting.as_deref());
        let brightness = lighting.map_or(1.0, |lighting| lighting.level_brightness);
        light.fire_light = if glow == LitBy::ScriptLightsToo { glow_at(self.lights.iter(), location, brightness) } else { Vec3::ZERO };
        let origin = self.facts.origin;
        let here = Vec3::from(to_bevy(location)) + origin;
        if light.due(here) {
            let level = self.collision.as_deref().and_then(|collision| collision.0.as_deref());
            let reaches = |at: Vec3| {
                level.is_none_or(|level| {
                    let to_lamp = Vector::from(to_bevy(at - origin)) - location;
                    let distance = to_lamp.size();
                    distance <= LAMP_CLEARANCE || level.first_hit(location, location + to_lamp * (1.0 - LAMP_CLEARANCE / distance)).is_none()
                })
            };
            let zone = level.map_or(0, |level| p2_collision::point_zone(&level.model, location));
            let zone_ambients = lighting.map_or(&[][..], |lighting| &lighting.zone_ambients[..]);
            let ambient = Vec3::from(zone_ambients.get(usize::from(zone)).copied().unwrap_or_default());
            let sum = lamp_sum(self.lamps.iter().map(|(at, placed)| (at.translation, placed)), here, reaches);
            (light.lamp_light, light.lamps_alone, light.lamp_at, light.lamp_age) = (emitter_light(ambient, sum), lamps_shown(sum), here, 0);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LitBy {
    Lamps,
    ScriptLightsToo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lamps_sum_is_taken_again_after_30_frames_or_16_units() {
        let mut light = ActorLight::default();
        assert!(light.due(Vec3::ZERO), "never summed");
        (light.lamp_at, light.lamp_age) = (Vec3::ZERO, 0);
        let near = Vec3::new(LAMP_RECHECK_UNITS, 0.0, 0.0);
        for _ in 1..LAMP_RECHECK_FRAMES {
            light.lamp_age += 1;
            assert!(!light.due(near), "{}", light.lamp_age);
        }
        light.lamp_age += 1;
        assert!(light.due(Vec3::ZERO));
        light.lamp_age = 1;
        assert!(light.due(Vec3::new(LAMP_RECHECK_UNITS + 0.1, 0.0, 0.0)));
    }
}
