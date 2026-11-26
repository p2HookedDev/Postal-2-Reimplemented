use bevy::ecs::prelude::{Local, Res, ResMut, Resource};
use p2_math::Vector;
use p2_render_cpu::{LightEffect, LightType, LightClock, MeshLight};

use p2_host::settings::{MODERN_FIRE_LIGHT, Settings};
use crate::world::frame_facts::{FrameFacts, Glow, GlowSource, ScriptLight};

const FIRE_HUE: u8 = 15;
const FIRE_SATURATION: u8 = 150;
const FIRE_BRIGHTNESS: f32 = 150.0;
const FIRE_RADIUS: f32 = 24.0;
const FIRE_SPACING: f32 = 600.0;
const FIRE_FADE_SECONDS: f32 = 2.0;

const FLASH_BRIGHTNESS: f32 = 150.0;
const FLASH_RADIUS: f32 = 15.0;
const FLASH_RADIUS_MOST: f32 = 20.0;
const FLASH_FULL_SECONDS: f32 = 0.06;
const FLASH_FADE_SECONDS: f32 = 0.08;

#[derive(Resource, Default)]
pub struct GlowLights(pub(super) Vec<MeshLight>);

fn fire_share(place: Vector, fade: f32, lit: &[MeshLight]) -> f32 {
    let shade = lit.iter().filter(|other| (other.location - place).size() < FIRE_SPACING).map(|other| other.brightness / FIRE_BRIGHTNESS).fold(0.0, f32::max);
    fade * (1.0 - shade)
}

fn flash_share(age: f32) -> f32 {
    1.0 - ((age - FLASH_FULL_SECONDS) / FLASH_FADE_SECONDS).clamp(0.0, 1.0)
}

fn flash(glow: &GlowSource) -> Option<MeshLight> {
    let lit = |light: ScriptLight| MeshLight {
        location: glow.location,
        rotation: light.rotation,
        light_type: LightType::Steady,
        effect: light.effect,
        brightness: light.brightness,
        hue: light.hue,
        saturation: light.saturation,
        radius: light.radius,
        cone: light.cone,
        dynamic: true,
        ..MeshLight::default()
    };
    match glow.kind {
        Glow::Fire { .. } => None,
        Glow::ShotFlash(light) => Some(MeshLight { effect: LightEffect::NonIncidence, radius: light.radius.min(FLASH_RADIUS_MOST), ..lit(light) }),
        Glow::Flash { light, age } => {
            let share = flash_share(age);
            (share > 0.0).then(|| MeshLight { brightness: FLASH_BRIGHTNESS * share, radius: FLASH_RADIUS, ..lit(light) })
        }
    }
}

pub(crate) fn light_glows(facts: Res<FrameFacts>, settings: Option<Res<Settings>>, mut glow_lights: ResMut<GlowLights>, mut made: Local<(LightClock, Vec<MeshLight>)>) {
    let (clock, lights) = &mut *made;
    lights.clear();
    if settings.is_some_and(|settings| settings.modern_level(MODERN_FIRE_LIGHT) > 0) {
        for glow in &facts.glows {
            let Glow::Fire { life_left } = glow.kind else { continue };
            let fade = if life_left > 0.0 { (life_left / FIRE_FADE_SECONDS).min(1.0) } else { 1.0 };
            let share = fire_share(glow.location, fade, lights);
            if share > 0.0 {
                let light = MeshLight { location: glow.location, light_type: LightType::SubtlePulse, hue: FIRE_HUE, saturation: FIRE_SATURATION, brightness: FIRE_BRIGHTNESS * share, radius: FIRE_RADIUS, dynamic: true, ..MeshLight::default() };
                lights.push(clock.animate(light, facts.level_time));
            }
        }
        lights.extend(facts.glows.iter().filter_map(flash));
    }
    if glow_lights.0 != *lights {
        std::mem::swap(&mut glow_lights.0, lights);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fire_beside_a_lit_one_gives_only_what_the_lit_one_lacks() {
        let lit = |x: f32, brightness: f32| MeshLight { location: Vector::new(x, 0.0, 0.0), brightness, ..MeshLight::default() };
        assert_eq!(fire_share(Vector::ZERO, 1.0, &[]), 1.0);
        assert_eq!(fire_share(Vector::ZERO, 1.0, &[lit(100.0, FIRE_BRIGHTNESS)]), 0.0);
        assert_eq!(fire_share(Vector::ZERO, 1.0, &[lit(100.0, FIRE_BRIGHTNESS * 0.25)]), 0.75);
        assert_eq!(fire_share(Vector::ZERO, 0.5, &[lit(FIRE_SPACING + 1.0, FIRE_BRIGHTNESS)]), 0.5);
    }

    #[test]
    fn a_flash_is_full_when_new_and_gone_after_its_fade() {
        assert_eq!(flash_share(0.0), 1.0);
        assert_eq!(flash_share(FLASH_FULL_SECONDS), 1.0);
        assert!((flash_share(FLASH_FULL_SECONDS + FLASH_FADE_SECONDS / 2.0) - 0.5).abs() < 1e-6);
        assert_eq!(flash_share(1.0), 0.0);
    }
}
