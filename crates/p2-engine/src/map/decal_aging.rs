use bevy::app::{App, Plugin, PostUpdate};
use bevy::ecs::prelude::{Component, Local, Query, Res};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::light::ClusteredDecal;

use crate::display::{MODERN_DECAL_AGING, Settings};
use crate::play::Play;
use crate::render::wounds::{DRY_SHIFT, DRY_STEPS, dryness};

use super::MapSystems;

pub(super) struct DecalAgingPlugin;

impl Plugin for DecalAgingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, age_decals.after(super::spawned::present_projections).in_set(MapSystems::Present));
    }
}

/// A spawned projector's decal: `tag` is the one `projector_tag` made, `life` the script's `LifeSpan` when it attached.
#[derive(Component)]
pub(super) struct DecalAge {
    tag: u32,
    life: f32,
    age: f32,
    blood: bool,
}

impl DecalAge {
    pub(super) fn new(tag: u32, life: f32, texture: &str) -> Self {
        Self { tag, life, age: 0.0, blood: is_blood(texture) }
    }
}

// Fading in projector tag bits 10..15 (material blend keeps 8..10, bit 15 marks a wound); the shader reads the same.
const FADE_SHIFT: u32 = 10;
const FADE_STEPS: f32 = 31.0;

/// The retail splat textures name blood in their package, group or name.
fn is_blood(texture: &str) -> bool {
    texture.to_ascii_lowercase().contains("blood")
}

/// 0 = drawn as ever, 1 = gone, over the last second of `life`. Without a life span the script removes it
/// some other way, so it never fades.
fn fade(life: f32, age: f32) -> f32 {
    if life <= 0.0 {
        return 0.0;
    }
    (1.0 - (life - age)).clamp(0.0, 1.0)
}

fn aged_tag(decal: &DecalAge) -> u32 {
    let fade = (fade(decal.life, decal.age) * FADE_STEPS).round() as u32;
    let dry = if decal.blood { (dryness(decal.age) * DRY_STEPS).round() as u32 } else { 0 };
    decal.tag | fade << FADE_SHIFT | dry << DRY_SHIFT
}

/// Ages count only while the setting is on and the game runs; switched off, every tag goes back to its own.
fn age_decals(settings: Option<Res<Settings>>, play: Res<Play>, mut decals: Query<(&mut ClusteredDecal, &mut DecalAge)>, mut aged: Local<bool>) {
    if settings.is_none_or(|settings| settings.modern_level(MODERN_DECAL_AGING) == 0) {
        if std::mem::take(&mut *aged) {
            for (mut decal, age) in &mut decals {
                decal.tag = age.tag;
            }
        }
        return;
    }
    *aged = true;
    let step = play.world().filter(|world| !world.game_paused()).map_or(0.0, |world| world.game_step());
    for (mut decal, mut age) in &mut decals {
        age.age += step;
        let tag = aged_tag(&age);
        if decal.tag != tag {
            decal.tag = tag;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fades_over_the_last_second_only() {
        assert_eq!(fade(10.0, 0.0), 0.0);
        assert_eq!(fade(10.0, 9.0), 0.0);
        assert!((fade(10.0, 9.5) - 0.5).abs() < 1e-5);
        assert_eq!(fade(10.0, 10.0), 1.0);
        assert_eq!(fade(10.0, 12.0), 1.0);
        assert_eq!(fade(0.0, 100.0), 0.0);
        assert!((fade(0.5, 0.0) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn blood_is_named_in_the_texture_path() {
        assert!(is_blood("P2Blood.Splats.BloodSplat1"));
        assert!(is_blood("nathans.Inventory.bloodsplat"));
        assert!(!is_blood("Effects.Scorch.BulletHole"));
        assert!(!is_blood("Fluids.PeeSplat"));
    }

    #[test]
    fn aging_keeps_the_projector_bits_and_a_fresh_decal_its_tag() {
        let used: u32 = 0xffff_0000 | 0x8000 | 0x300 | 0x87;
        assert_eq!((0x1f << FADE_SHIFT | 0xf << DRY_SHIFT) & used, 0);
        let fresh = DecalAge { tag: 0x8001_0102, life: 5.0, age: 0.0, blood: true };
        assert_eq!(aged_tag(&fresh), fresh.tag);
        let ending = DecalAge { age: 5.0, ..fresh };
        assert_eq!(aged_tag(&ending) & used, fresh.tag);
        assert_eq!(aged_tag(&ending) >> FADE_SHIFT & 0x1f, 31);
        let rock = DecalAge { tag: 1, life: 0.0, age: 30.0, blood: false };
        assert_eq!(aged_tag(&rock), 1);
    }
}
