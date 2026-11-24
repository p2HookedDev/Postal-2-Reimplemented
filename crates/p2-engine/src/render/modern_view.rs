//! Modern graphics: the view widens on a screen wider than 16:9, a Matinee shot keeps 16:9 between side bars, the HUD can
//! stay inside a centred 16:9 area and be drawn at whole pixels, and the tone pass can draw a wide view on a cylinder
//! (Panini) so its edges do not stretch.

use bevy::app::{App, Plugin, Update};
use bevy::ecs::prelude::{Res, ResMut};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::math::{UVec2, Vec2, Vec3};
use bevy_egui::egui;

use crate::display::{MODERN_HUD_SAFE_AREA, MODERN_SHARP_HUD, MODERN_WIDE_VIEW, Settings};
use crate::mesh_view::{MeshView, SceneViewSystems};
use crate::play::{Play, PlayWorld};

const WIDEST: f32 = 16.0 / 9.0;
const PANINI_PER_LEVEL: f32 = 0.1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "four switches of the view, each its own setting")]
pub(crate) struct ModernView {
    pub(crate) wide: bool,
    pub(crate) bars: bool,
    pub(crate) safe_area: bool,
    pub(crate) sharp_hud: bool,
    pub(crate) sharp_text: bool,
}

pub struct ModernViewPlugin;

impl Plugin for ModernViewPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, follow_settings.before(SceneViewSystems::Place));
    }
}

fn follow_settings(settings: Option<Res<Settings>>, play: Option<Res<Play>>, view: Option<ResMut<MeshView>>) {
    let Some(mut view) = view else { return };
    let level = |name: &str| settings.as_ref().map_or(0, |settings| settings.modern_level(name));
    let wide = level(MODERN_WIDE_VIEW) > 0;
    let wanted = ModernView {
        wide,
        bars: wide && play.as_ref().and_then(|play| play.world()).is_some_and(PlayWorld::cutscene),
        safe_area: level(MODERN_HUD_SAFE_AREA) > 0,
        sharp_hud: level(MODERN_SHARP_HUD) > 0,
        sharp_text: level(MODERN_SHARP_HUD) > 1,
    };
    if view.modern != wanted {
        view.modern = wanted;
    }
}

/// The vertical field of view in radians for Unreal's horizontal one in degrees. Wide: past 16:9 the vertical one stays
/// what 16:9 gives and the horizontal one grows.
pub(crate) fn vertical_fov(degrees: f32, aspect: f32, wide: bool) -> f32 {
    let aspect = if wide { aspect.min(WIDEST) } else { aspect };
    2.0 * ((degrees.to_radians() / 2.0).tan() / aspect).atan()
}

/// The centred 16:9 part of a wider viewport, else the viewport itself.
pub(crate) fn centred_wide(position: UVec2, size: UVec2) -> (UVec2, UVec2) {
    let width = (size.y as f32 * WIDEST).round() as u32;
    if width >= size.x {
        return (position, size);
    }
    (position + UVec2::new((size.x - width) / 2, 0), UVec2::new(width, size.y))
}

pub(crate) fn hud_area(rect: egui::Rect) -> egui::Rect {
    let width = (rect.height() * WIDEST).round();
    if width >= rect.width() {
        return rect;
    }
    egui::Rect::from_min_size(rect.min + egui::vec2(((rect.width() - width) / 2.0).round(), 0.0), egui::vec2(width, rect.height()))
}

/// What the tone pass reads for the Panini warp: its strength, the tangent of half the vertical field of view, and the
/// zoom that keeps the warped frame filling the screen. Zero at level 0.
pub(crate) fn panini(level: u8, vertical_fov: f32, aspect: f32) -> Vec3 {
    if level == 0 {
        return Vec3::ZERO;
    }
    let strength = f32::from(level) * PANINI_PER_LEVEL;
    let half = (vertical_fov / 2.0).tan();
    let extent = Vec2::new(half * aspect, half);
    let to_cylinder = 1.0 / (extent.x * extent.x + 1.0).sqrt();
    let edge = extent * to_cylinder * ((1.0 + strength) / (to_cylinder + strength));
    Vec3::new(strength, half, (edge.length() / extent.length()).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wider_screen_widens_the_view_and_keeps_its_height() {
        let sixteen_nine = vertical_fov(90.0, WIDEST, true);
        assert_eq!(sixteen_nine, vertical_fov(90.0, WIDEST, false));
        assert_eq!(vertical_fov(90.0, 32.0 / 9.0, true), sixteen_nine);
        assert!(vertical_fov(90.0, 32.0 / 9.0, false) < sixteen_nine);
        assert_eq!(vertical_fov(90.0, 4.0 / 3.0, true), vertical_fov(90.0, 4.0 / 3.0, false));
    }

    #[test]
    fn side_bars_leave_a_centred_sixteen_nine() {
        assert_eq!(centred_wide(UVec2::ZERO, UVec2::new(3440, 1440)), (UVec2::new(440, 0), UVec2::new(2560, 1440)));
        assert_eq!(centred_wide(UVec2::ZERO, UVec2::new(1920, 1080)), (UVec2::ZERO, UVec2::new(1920, 1080)));
        assert_eq!(centred_wide(UVec2::ZERO, UVec2::new(1024, 768)), (UVec2::ZERO, UVec2::new(1024, 768)));
    }

    #[test]
    fn the_hud_area_is_sixteen_nine_in_the_middle() {
        let wide = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(3840.0, 1080.0));
        assert_eq!(hud_area(wide), egui::Rect::from_min_size(egui::pos2(960.0, 0.0), egui::vec2(1920.0, 1080.0)));
        let narrow = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 1024.0));
        assert_eq!(hud_area(narrow), narrow);
    }

    #[test]
    fn panini_is_off_at_zero_and_zooms_in_to_fill_the_frame() {
        assert_eq!(panini(0, 1.5, 2.4), Vec3::ZERO);
        let warp = panini(10, vertical_fov(110.0, 21.0 / 9.0, true), 21.0 / 9.0);
        assert_eq!(warp.x, 1.0);
        assert!(warp.z > 0.0 && warp.z < 1.0, "{warp}");
    }
}
