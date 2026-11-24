//! Lights for fire and gun flashes under the modern look. The game's scripts hold these lights and leave them off: a
//! fire lights only where a flag no script sets allows it, a flash only with an option of the game's patch menu.

use super::after_hud::level_time;
use super::*;
use p2_render_cpu::{LE_NON_INCIDENCE, LT_STEADY, LT_SUBTLE_PULSE, MeshLight};

// The scripts' own colour and brightness for a fire's light. Their radius reaches a street and their type flickers
// frame by frame: the radius and the steady pulse here are ours.
const FIRE_HUE: u8 = 15;
const FIRE_SATURATION: u8 = 150;
const FIRE_BRIGHTNESS: f32 = 150.0;
const FIRE_RADIUS: f32 = 24.0;
/// One fire lights within this distance, as the scripts mean it: a burning field is a few lights.
const FIRE_SPACING: f32 = 600.0;
const FIRE_FADE_SECONDS: f32 = 2.0;

// The middle of the ranges the scripts draw a flash's light from.
const FLASH_BRIGHTNESS: f32 = 150.0;
const FLASH_RADIUS: f32 = 15.0;
const FLASH_RADIUS_MOST: f32 = 20.0;
const FLASH_FULL_SECONDS: f32 = 0.06;
const FLASH_FADE_SECONDS: f32 = 0.08;

pub(in crate::play) fn glows(vm: &Vm<Arc<Loader>>, actor: ObjectId) -> bool {
    [Kind::FireEmitter, Kind::XMuzzleFlashEmitter, Kind::MuzzleFlashAttachment].into_iter().any(|kind| vm.store.is_a(actor, kind))
}

/// How much of its light a fire gives beside the fire lights already lit: none next to a full one, the rest of a fading one.
fn fire_share(place: Vector, fade: f32, lit: &[MeshLight]) -> f32 {
    let shade = lit.iter().filter(|other| (other.location - place).size() < FIRE_SPACING).map(|other| other.brightness / FIRE_BRIGHTNESS).fold(0.0, f32::max);
    fade * (1.0 - shade)
}

fn flash_share(age: f32) -> f32 {
    1.0 - ((age - FLASH_FULL_SECONDS) / FLASH_FADE_SECONDS).clamp(0.0, 1.0)
}

/// A burning fire's place and how far its last seconds have dimmed it.
fn fire(vm: &Vm<Arc<Loader>>, actor: ObjectId) -> Option<(Vector, f32)> {
    let burning = vm.store.objects_of(actor, Prop::Emitters).flatten().next().is_some_and(|emitter| !vm.store.flag_of(emitter, Prop::Disabled));
    if !burning || vm.store.flag_of(actor, Prop::bHidden) {
        return None;
    }
    let life = vm.store.float_of(actor, Prop::LifeSpan);
    Some((vm.store.location(actor), if life > 0.0 { (life / FIRE_FADE_SECONDS).min(1.0) } else { 1.0 }))
}

fn flash(vm: &Vm<Arc<Loader>>, actor: ObjectId) -> Option<MeshLight> {
    let values = crate::actors::light_values(vm, actor);
    if vm.store.is_a(actor, Kind::MuzzleFlashAttachment) {
        // Shown for the ticks of a shot, with light values its script picks at each one.
        let shown = !vm.store.flag_of(actor, Prop::bHidden) && values.brightness > 0.0;
        return shown.then_some(MeshLight { light_type: LT_STEADY, effect: LE_NON_INCIDENCE, radius: values.radius.min(FLASH_RADIUS_MOST), dynamic: true, ..values });
    }
    let Some(p2_vm::Value::Float(whole)) = vm.store.default_value(vm.store.object(actor).class?, Prop::LifeSpan) else { return None };
    let share = flash_share(whole - vm.store.float_of(actor, Prop::LifeSpan));
    // Hung on the held weapon, which has no place of its own in the world: its holder's.
    let location = vm.store.object_of(actor, Prop::Owner).map_or(values.location, |holder| vm.store.location(holder));
    (share > 0.0).then_some(MeshLight { location, light_type: LT_STEADY, brightness: FLASH_BRIGHTNESS * share, radius: FLASH_RADIUS, dynamic: true, ..values })
}

impl Simulation {
    pub(super) fn light_glows(&mut self, frame: &mut VmFrame) {
        let vm = &self.vm;
        let now = level_time(vm);
        let unlit = |actor: &&ObjectId| vm.store.byte_of(**actor, Prop::LightType) == 0 && !vm.store.is_deleted(**actor);
        let first = frame.dynamic_lights.len();
        for &actor in self.glows.iter().filter(unlit).filter(|&&actor| vm.store.is_a(actor, Kind::FireEmitter)) {
            let Some((location, fade)) = fire(vm, actor) else { continue };
            let share = fire_share(location, fade, &frame.dynamic_lights[first..]);
            if share > 0.0 {
                let light = MeshLight { location, light_type: LT_SUBTLE_PULSE, hue: FIRE_HUE, saturation: FIRE_SATURATION, brightness: FIRE_BRIGHTNESS * share, radius: FIRE_RADIUS, dynamic: true, ..MeshLight::default() };
                frame.dynamic_lights.push(self.light_clock.animate(light, now));
            }
        }
        frame.dynamic_lights.extend(self.glows.iter().filter(unlit).filter(|&&actor| !vm.store.is_a(actor, Kind::FireEmitter)).filter_map(|&actor| flash(vm, actor)));
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
