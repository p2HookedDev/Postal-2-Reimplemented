use std::path::PathBuf;

use bevy::ecs::prelude::{Local, Res};
use p2_bevy_assets::TextureQuality;
use p2_package::Ini;
use p2_vm::ini_truthy;
use rustc_hash::FxHashMap;

fn settings_file() -> Option<PathBuf> {
    Some(crate::game_dir::config_folder()?.join("Settings.ini"))
}

pub fn load_settings() -> Ini {
    let bytes = settings_file().and_then(|file| std::fs::read(file).ok()).unwrap_or_default();
    Ini::from_bytes(&bytes)
}

pub fn save_settings(settings: &Ini) {
    let saved = settings_file().ok_or_else(|| std::io::Error::other("no config folder")).and_then(|file| {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = file.with_extension("tmp");
        std::fs::write(&temp, settings.to_text())?;
        std::fs::rename(&temp, &file)
    });
    if let Err(error) = saved {
        bevy::log::warn!("saving settings: {error}");
    }
}

pub const QUERIES: &[&str] = &[
    USE_VSYNC,
    BRIGHTNESS,
    CONTRAST,
    GAMMA,
    USE_TRIPLE_BUFFERING,
    LEVEL_OF_ANISOTROPY,
    USE_TRILINEAR,
    DISPLAY_NUMBER,
    DETAIL_TEXTURES,
    TEXTURE_DETAIL[0],
    TEXTURE_DETAIL[1],
    TEXTURE_DETAIL[2],
    TEXTURE_DETAIL[3],
    TEXTURE_DETAIL[4],
    TEXTURE_MIN_LOD,
    TEXTURE_MAX_LOD,
    MODERN_GRAPHICS,
    MODERN_PRESET,
    MODERN_SHADOWS,
    MODERN_SHADOW_DISTANCE,
    MODERN_SHADOW_FILTER,
    MODERN_LAMP_SHADOWS,
    MODERN_CONTACT_SHADOWS,
    MODERN_AO,
    MODERN_INDIRECT,
    MODERN_ANTI_ALIASING,
    MODERN_BLOOM,
    MODERN_SHARPEN,
    MODERN_RELIEF,
    MODERN_SPECULAR,
    MODERN_TERRAIN_DETAIL,
    MODERN_GOD_RAYS,
    MODERN_VIGNETTE,
    MODERN_CHROMATIC_ABERRATION,
    MODERN_SATURATION,
    MODERN_CONTRAST,
    MODERN_EXPOSURE,
    MODERN_REFLECTIONS,
    MODERN_REFLECTION_STRENGTH,
    MODERN_PARTICLE_LIGHT,
    MODERN_FIRE_LIGHT,
    MODERN_SOFT_PARTICLES,
    MODERN_DECAL_AGING,
    MODERN_WOUNDS,
    MODERN_SHARP_HUD,
    MODERN_WIDE_VIEW,
    MODERN_HUD_SAFE_AREA,
    MODERN_PANINI,
    MODERN_FADE_IN,
    MODERN_FOOT_PLACEMENT,
    MODERN_HIT_REACTIONS,
    MODERN_LAMP_SIZE,
    MODERN_TEMPORAL_AA,
    MODERN_BOUNCE_LIGHT,
    MODERN_LIQUID_FLOW,
    MODERN_LIQUIDS,
    MODERN_FIRE_VOLUMES,
    MODERN_VOLUMETRIC_FOG,
    MODERN_GPU_PARTICLES,
    MODERN_TESSELLATION,
    MODERN_PHOTO_ACTIVE,
    MODERN_PHOTO_FOV,
    MODERN_PHOTO_FOCUS,
    MODERN_PHOTO_BLUR,
    MODERN_PHOTO_EXPOSURE,
    MODERN_PHOTO_SATURATION,
    MODERN_PHOTO_CONTRAST,
    MODERN_PHOTO_VIGNETTE,
    MODERN_PHOTO_BLOOM,
    MODERN_PHOTO_GOD_RAYS,
    MODERN_PHOTO_FRINGE,
    MODERN_PHOTO_SHARPEN,
    MODERN_PHOTO_WARMTH,
    MODERN_PHOTO_HIDE_PLAYER,
    MSAA,
    FLUID_DETAIL,
];

const TEXTURE_DETAIL: [&str; 5] = [
    "ini:Engine.Engine.ViewportManager TextureDetailWorld",
    "ini:Engine.Engine.ViewportManager TextureDetailSkin",
    "ini:Engine.Engine.ViewportManager TextureDetailLightmap",
    "ini:Engine.Engine.ViewportManager TextureDetailTerrain",
    "ini:Engine.Engine.ViewportManager TextureDetailWeapon",
];
const TEXTURE_MIN_LOD: &str = "ini:Engine.Engine.ViewportManager TextureMinLOD";
const TEXTURE_MAX_LOD: &str = "ini:Engine.Engine.ViewportManager TextureMaxLOD";
const LEVEL_OF_ANISOTROPY: &str = "ini:Engine.Engine.RenderDevice LevelOfAnisotropy";
const USE_TRILINEAR: &str = "ini:Engine.Engine.RenderDevice UseTrilinear";
const DISPLAY_NUMBER: &str = "ini:Engine.Engine.RenderDevice DisplayNumber";
const DETAIL_TEXTURES: &str = "ini:Engine.Engine.RenderDevice DetailTextures";

pub const USE_VSYNC: &str = "ini:Engine.Engine.RenderDevice UseVSync";
pub const USE_TRIPLE_BUFFERING: &str = "ini:Engine.Engine.RenderDevice UseTripleBuffering";
pub const BRIGHTNESS: &str = "ini:Engine.Engine.ViewportManager Brightness";
pub const CONTRAST: &str = "ini:Engine.Engine.ViewportManager Contrast";
pub const GAMMA: &str = "ini:Engine.Engine.ViewportManager Gamma";

pub const MSAA: &str = "ini:Engine.Engine.RenderDevice MSAA";
pub const FLUID_DETAIL: &str = "Postal2Game.P2GameInfo FluidDetail";
pub const NORMAL_FLUID_DETAIL: i32 = 5;

pub const MODERN_GRAPHICS: &str = "ini:Engine.Engine.RenderDevice ModernGraphics";

pub const MODERN_PRESET: &str = "ini:Engine.Engine.RenderDevice ModernPreset";
pub const MODERN_SHADOWS: &str = "ini:Engine.Engine.RenderDevice ModernSunShadows";
pub const MODERN_SHADOW_DISTANCE: &str = "ini:Engine.Engine.RenderDevice ModernShadowDistance";
/// 0 sharp, 1 soft, 2 softer, 3 contact hardening.
pub const MODERN_SHADOW_FILTER: &str = "ini:Engine.Engine.RenderDevice ModernShadowFilter";
pub const MODERN_LAMP_SHADOWS: &str = "ini:Engine.Engine.RenderDevice ModernLampShadows";
pub const MODERN_CONTACT_SHADOWS: &str = "ini:Engine.Engine.RenderDevice ModernContactShadows";
pub const MODERN_AO: &str = "ini:Engine.Engine.RenderDevice ModernAO";
pub const MODERN_INDIRECT: &str = "ini:Engine.Engine.RenderDevice ModernIndirect";
pub const MODERN_ANTI_ALIASING: &str = "ini:Engine.Engine.RenderDevice ModernAntiAliasing";
pub const MODERN_BLOOM: &str = "ini:Engine.Engine.RenderDevice ModernBloom";
pub const MODERN_SHARPEN: &str = "ini:Engine.Engine.RenderDevice ModernSharpen";
pub const MODERN_RELIEF: &str = "ini:Engine.Engine.RenderDevice ModernRelief";
pub const MODERN_SPECULAR: &str = "ini:Engine.Engine.RenderDevice ModernSpecular";
pub const MODERN_TERRAIN_DETAIL: &str = "ini:Engine.Engine.RenderDevice ModernTerrainDetail";
pub const MODERN_GOD_RAYS: &str = "ini:Engine.Engine.RenderDevice ModernGodRays";
pub const MODERN_VIGNETTE: &str = "ini:Engine.Engine.RenderDevice ModernVignette";
pub const MODERN_CHROMATIC_ABERRATION: &str = "ini:Engine.Engine.RenderDevice ModernChromaticAberration";
pub const MODERN_SATURATION: &str = "ini:Engine.Engine.RenderDevice ModernSaturation";
pub const MODERN_CONTRAST: &str = "ini:Engine.Engine.RenderDevice ModernContrast";
pub const MODERN_EXPOSURE: &str = "ini:Engine.Engine.RenderDevice ModernExposure";
pub const MODERN_REFLECTIONS: &str = "ini:Engine.Engine.RenderDevice ModernReflections";
pub const MODERN_REFLECTION_STRENGTH: &str = "ini:Engine.Engine.RenderDevice ModernReflectionStrength";
pub const MODERN_PARTICLE_LIGHT: &str = "ini:Engine.Engine.RenderDevice ModernParticleLight";
pub const MODERN_FIRE_LIGHT: &str = "ini:Engine.Engine.RenderDevice ModernFireLight";
pub const MODERN_SOFT_PARTICLES: &str = "ini:Engine.Engine.RenderDevice ModernSoftParticles";
pub const MODERN_DECAL_AGING: &str = "ini:Engine.Engine.RenderDevice ModernDecalAging";
pub const MODERN_WOUNDS: &str = "ini:Engine.Engine.RenderDevice ModernWounds";
pub const MODERN_SHARP_HUD: &str = "ini:Engine.Engine.RenderDevice ModernSharpHud";
pub const MODERN_WIDE_VIEW: &str = "ini:Engine.Engine.RenderDevice ModernWideView";
pub const MODERN_HUD_SAFE_AREA: &str = "ini:Engine.Engine.RenderDevice ModernHudSafeArea";
pub const MODERN_PANINI: &str = "ini:Engine.Engine.RenderDevice ModernPanini";
pub const MODERN_FADE_IN: &str = "ini:Engine.Engine.RenderDevice ModernFadeIn";
pub const MODERN_FOOT_PLACEMENT: &str = "ini:Engine.Engine.RenderDevice ModernFootPlacement";
pub const MODERN_HIT_REACTIONS: &str = "ini:Engine.Engine.RenderDevice ModernHitReactions";
pub const MODERN_LAMP_SIZE: &str = "ini:Engine.Engine.RenderDevice ModernLampSize";
pub const MODERN_TEMPORAL_AA: &str = "ini:Engine.Engine.RenderDevice ModernTemporalAA";
pub const MODERN_BOUNCE_LIGHT: &str = "ini:Engine.Engine.RenderDevice ModernBounceLight";
pub const MODERN_LIQUID_FLOW: &str = "ini:Engine.Engine.RenderDevice ModernLiquidFlow";
pub const MODERN_LIQUIDS: &str = "ini:Engine.Engine.RenderDevice ModernLiquids";
pub const MODERN_FIRE_VOLUMES: &str = "ini:Engine.Engine.RenderDevice ModernFireVolumes";
pub const MODERN_VOLUMETRIC_FOG: &str = "ini:Engine.Engine.RenderDevice ModernVolumetricFog";
pub const MODERN_GPU_PARTICLES: &str = "ini:Engine.Engine.RenderDevice ModernGpuParticles";
pub const MODERN_TESSELLATION: &str = "ini:Engine.Engine.RenderDevice ModernTessellation";
pub const MODERN_PHOTO_ACTIVE: &str = "ini:Engine.Engine.RenderDevice ModernPhotoActive";
pub const MODERN_PHOTO_FOV: &str = "ini:Engine.Engine.RenderDevice ModernPhotoFov";
pub const MODERN_PHOTO_FOCUS: &str = "ini:Engine.Engine.RenderDevice ModernPhotoFocus";
pub const MODERN_PHOTO_BLUR: &str = "ini:Engine.Engine.RenderDevice ModernPhotoBlur";
pub const MODERN_PHOTO_EXPOSURE: &str = "ini:Engine.Engine.RenderDevice ModernPhotoExposure";
pub const MODERN_PHOTO_SATURATION: &str = "ini:Engine.Engine.RenderDevice ModernPhotoSaturation";
pub const MODERN_PHOTO_CONTRAST: &str = "ini:Engine.Engine.RenderDevice ModernPhotoContrast";
pub const MODERN_PHOTO_VIGNETTE: &str = "ini:Engine.Engine.RenderDevice ModernPhotoVignette";
pub const MODERN_PHOTO_BLOOM: &str = "ini:Engine.Engine.RenderDevice ModernPhotoBloom";
pub const MODERN_PHOTO_GOD_RAYS: &str = "ini:Engine.Engine.RenderDevice ModernPhotoGodRays";
pub const MODERN_PHOTO_FRINGE: &str = "ini:Engine.Engine.RenderDevice ModernPhotoFringe";
pub const MODERN_PHOTO_SHARPEN: &str = "ini:Engine.Engine.RenderDevice ModernPhotoSharpen";
pub const MODERN_PHOTO_WARMTH: &str = "ini:Engine.Engine.RenderDevice ModernPhotoWarmth";
pub const MODERN_PHOTO_HIDE_PLAYER: &str = "ini:Engine.Engine.RenderDevice ModernPhotoHidePlayer";

const PHOTO_TWINS: [(&str, &str); 8] = [
    (MODERN_EXPOSURE, MODERN_PHOTO_EXPOSURE),
    (MODERN_SATURATION, MODERN_PHOTO_SATURATION),
    (MODERN_CONTRAST, MODERN_PHOTO_CONTRAST),
    (MODERN_VIGNETTE, MODERN_PHOTO_VIGNETTE),
    (MODERN_BLOOM, MODERN_PHOTO_BLOOM),
    (MODERN_GOD_RAYS, MODERN_PHOTO_GOD_RAYS),
    (MODERN_CHROMATIC_ABERRATION, MODERN_PHOTO_FRINGE),
    (MODERN_SHARPEN, MODERN_PHOTO_SHARPEN),
];

const PHOTO_FOV_LEAST: u8 = 10;

pub const NEUTRAL_LEVEL: u8 = 5;

const LEVELS: [(&str, u8, u8); 53] = [
    (MODERN_SHADOWS, 3, 4),
    (MODERN_SHADOW_DISTANCE, 4, 5),
    (MODERN_LAMP_SHADOWS, 3, 4),
    (MODERN_CONTACT_SHADOWS, 1, 2),
    (MODERN_AO, 2, 3),
    (MODERN_INDIRECT, 2, 2),
    (MODERN_ANTI_ALIASING, 2, 2),
    (MODERN_BLOOM, 5, 10),
    (MODERN_SHARPEN, 4, 10),
    (MODERN_GOD_RAYS, 5, 10),
    (MODERN_VIGNETTE, 2, 10),
    (MODERN_CHROMATIC_ABERRATION, 0, 10),
    (MODERN_SATURATION, NEUTRAL_LEVEL, 10),
    (MODERN_CONTRAST, NEUTRAL_LEVEL, 10),
    (MODERN_EXPOSURE, NEUTRAL_LEVEL, 10),
    (MODERN_REFLECTIONS, 2, 3),
    (MODERN_REFLECTION_STRENGTH, 10, 10),
    (MODERN_PARTICLE_LIGHT, 1, 1),
    (MODERN_FIRE_LIGHT, 1, 1),
    (MODERN_SOFT_PARTICLES, 5, 10),
    (MODERN_DECAL_AGING, 1, 1),
    (MODERN_WOUNDS, 1, 1),
    (MODERN_SHARP_HUD, 1, 2),
    (MODERN_WIDE_VIEW, 1, 1),
    (MODERN_HUD_SAFE_AREA, 1, 1),
    (MODERN_PANINI, 0, 10),
    (MODERN_FADE_IN, 4, 10),
    (MODERN_FOOT_PLACEMENT, 1, 1),
    (MODERN_HIT_REACTIONS, 5, 10),
    (MODERN_LAMP_SIZE, 5, 10),
    // Held at 0 until each looks right; a saved value above 0 reads as 0.
    (MODERN_TEMPORAL_AA, 0, 0),
    (MODERN_BOUNCE_LIGHT, 0, 0),
    (MODERN_LIQUID_FLOW, 1, 1),
    (MODERN_LIQUIDS, 6, 10),
    (MODERN_FIRE_VOLUMES, 0, 0),
    (MODERN_VOLUMETRIC_FOG, 0, 0),
    (MODERN_GPU_PARTICLES, 5, 10),
    (MODERN_TESSELLATION, 1, 1),
    (MODERN_PHOTO_ACTIVE, 0, 1),
    (MODERN_PHOTO_FOV, 0, 150),
    (MODERN_PHOTO_FOCUS, 0, 36),
    (MODERN_PHOTO_BLUR, 0, 10),
    (MODERN_PHOTO_EXPOSURE, NEUTRAL_LEVEL, 10),
    (MODERN_PHOTO_SATURATION, NEUTRAL_LEVEL, 10),
    (MODERN_PHOTO_CONTRAST, NEUTRAL_LEVEL, 10),
    (MODERN_PHOTO_VIGNETTE, 2, 10),
    (MODERN_PHOTO_BLOOM, 5, 10),
    (MODERN_PHOTO_GOD_RAYS, 5, 10),
    (MODERN_PHOTO_FRINGE, 0, 10),
    (MODERN_PHOTO_SHARPEN, 4, 10),
    (MODERN_PHOTO_WARMTH, NEUTRAL_LEVEL, 10),
    (MODERN_PHOTO_HIDE_PLAYER, 0, 1),
    (MODERN_SHADOW_FILTER, 0, 3),
];


#[derive(Debug, Clone, Default, PartialEq, bevy::ecs::resource::Resource)]
pub struct Settings(FxHashMap<&'static str, String>);

impl Settings {
    pub fn read(mut get: impl FnMut(&str) -> String) -> Self {
        Self(QUERIES.iter().map(|&query| (query, get(query))).collect())
    }

    pub fn text(&self, query: &str) -> &str {
        self.0.get(query).map_or("", String::as_str)
    }

    pub fn flag(&self, query: &str) -> bool {
        ini_truthy(self.text(query))
    }

    pub fn number(&self, query: &str, default: f32) -> f32 {
        self.text(query).trim().parse().unwrap_or(default)
    }

    pub fn texture_quality(&self) -> TextureQuality {
        let detail = TEXTURE_DETAIL.map(|query| TextureQuality::detail_level(self.text(query)));
        let default = TextureQuality::default();
        TextureQuality {
            detail,
            min_lod: self.number(TEXTURE_MIN_LOD, default.min_lod as f32) as i32,
            max_lod: self.number(TEXTURE_MAX_LOD, default.max_lod as f32) as i32,
            anisotropy: self.number(LEVEL_OF_ANISOTROPY, 1.0) as u16,
            trilinear: self.text(USE_TRILINEAR).is_empty() || self.flag(USE_TRILINEAR),
            detail_textures: self.text(DETAIL_TEXTURES).is_empty() || self.flag(DETAIL_TEXTURES),
        }
    }

    pub fn fluid_detail(&self) -> i32 {
        self.number(FLUID_DETAIL, NORMAL_FLUID_DETAIL as f32) as i32
    }

    pub fn display_number(&self) -> usize {
        self.number(DISPLAY_NUMBER, 0.0) as usize
    }

    /// Modern screen passes read single-sample depth and normals, so they draw without MSAA.
    pub fn msaa_samples(&self) -> u32 {
        if self.modern_graphics() {
            return 1;
        }
        match self.number(MSAA, 1.0) as u32 {
            samples @ (2 | 4 | 8) => samples,
            _ => 1,
        }
    }

    pub fn modern_graphics(&self) -> bool {
        self.flag(MODERN_GRAPHICS)
    }

    pub fn terrain_detail(&self) -> usize {
        if self.modern_effect(MODERN_TERRAIN_DETAIL) { 3 } else { 1 }
    }

    pub fn modern_effect(&self, query: &str) -> bool {
        let text = self.text(query);
        self.modern_graphics() && (text.trim().is_empty() || ini_truthy(text))
    }

    pub fn material_level(&self) -> u8 {
        if self.modern_level(MODERN_SPECULAR) > 0 { 8 } else { 0 }
    }

    pub fn photo_active(&self) -> bool {
        self.modern_level(MODERN_PHOTO_ACTIVE) > 0
    }

    pub fn photo_fov(&self) -> Option<u8> {
        let degrees = self.modern_level(MODERN_PHOTO_FOV);
        (degrees > 0 && self.photo_active()).then(|| degrees.max(PHOTO_FOV_LEAST))
    }

    fn photo_twin(&self, query: &str) -> Option<&'static str> {
        let &(_, twin) = PHOTO_TWINS.iter().find(|&&(row, _)| row == query)?;
        (!self.text(twin).trim().is_empty() && self.photo_active()).then_some(twin)
    }

    pub fn modern_level(&self, query: &str) -> u8 {
        let query = self.photo_twin(query).unwrap_or(query);
        let Some(&(_, high, top)) = LEVELS.iter().find(|(key, ..)| *key == query) else { return 0 };
        if !self.modern_graphics() {
            return 0;
        }
        let text = self.text(query).trim();
        match text.parse::<u8>() {
            _ if text.is_empty() => high,
            Ok(level) => level.min(top),
            Err(_) if ini_truthy(text) => high.max(1).min(top),
            Err(_) => 0,
        }
    }
}

pub struct DisplayPlugin;

impl bevy::app::Plugin for DisplayPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        use bevy::ecs::schedule::IntoScheduleConfigs;
        app.init_resource::<Settings>()
            .add_systems(bevy::app::Update, apply_texture_quality.run_if(bevy::ecs::schedule::common_conditions::resource_changed::<Settings>));
    }
}

pub fn apply_texture_quality(settings: Res<Settings>) {
    p2_bevy_assets::set_texture_quality(settings.texture_quality());
    p2_bevy_assets::set_terrain_detail(settings.terrain_detail());
    p2_bevy_assets::set_character_smoothing(settings.modern_level(MODERN_TESSELLATION));
}

pub fn modern_on(key: &'static str) -> impl FnMut(Option<Res<Settings>>) -> bool {
    move |settings: Option<Res<Settings>>| settings.is_some_and(|settings| settings.modern_level(key) > 0)
}

pub fn modern_turned_off(key: &'static str) -> impl FnMut(Option<Res<Settings>>, Local<bool>) -> bool {
    move |settings: Option<Res<Settings>>, mut was_on: Local<bool>| {
        let on = settings.is_some_and(|settings| settings.modern_level(key) > 0);
        std::mem::replace(&mut *was_on, on) && !on
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(bevy::ecs::resource::Resource, Default)]
    struct Ran {
        work: u32,
        undo: u32,
    }

    #[test]
    fn a_feature_works_while_on_and_undoes_once_when_turned_off() {
        use bevy::ecs::prelude::ResMut;
        use bevy::ecs::schedule::IntoScheduleConfigs;
        let mut app = bevy::app::App::new();
        app.init_resource::<Ran>().add_systems(
            bevy::app::Update,
            (
                (|mut ran: ResMut<Ran>| ran.work += 1).run_if(modern_on(MODERN_WOUNDS)),
                (|mut ran: ResMut<Ran>| ran.undo += 1).run_if(modern_turned_off(MODERN_WOUNDS)),
            ),
        );
        let ran = |app: &bevy::app::App| {
            let ran = app.world().resource::<Ran>();
            (ran.work, ran.undo)
        };
        app.update();
        assert_eq!(ran(&app), (0, 0));
        app.insert_resource(with("True", MODERN_WOUNDS, "1"));
        app.update();
        app.update();
        assert_eq!(ran(&app), (2, 0));
        app.insert_resource(with("True", MODERN_WOUNDS, "0"));
        app.update();
        app.update();
        assert_eq!(ran(&app), (2, 1));
        app.insert_resource(with("False", MODERN_WOUNDS, "1"));
        app.update();
        assert_eq!(ran(&app), (2, 1));
        app.insert_resource(with("True", MODERN_WOUNDS, "1"));
        app.update();
        app.world_mut().remove_resource::<Settings>();
        app.update();
        app.update();
        assert_eq!(ran(&app), (3, 2));
    }

    #[test]
    fn texture_quality_from_followed_settings() {
        let mut settings = Settings::default();
        assert_eq!(settings.texture_quality(), TextureQuality::default());
        for (query, value) in [
            ("ini:Engine.Engine.ViewportManager TextureDetailWorld", "Medium"),
            ("ini:Engine.Engine.ViewportManager TextureDetailSkin", "UltraLow"),
            ("ini:Engine.Engine.ViewportManager TextureMinLOD", "2"),
            ("ini:Engine.Engine.ViewportManager TextureMaxLOD", "9"),
            ("ini:Engine.Engine.RenderDevice LevelOfAnisotropy", "4"),
            ("ini:Engine.Engine.RenderDevice UseTrilinear", "False"),
            ("ini:Engine.Engine.RenderDevice DisplayNumber", "2"),
        ] {
            settings.0.insert(QUERIES.iter().find(|&&q| q == query).copied().unwrap_or_default(), value.to_owned());
        }
        assert_eq!(settings.texture_quality(), TextureQuality { detail: [1, 3, 0, 0, 0], min_lod: 2, max_lod: 9, anisotropy: 4, trilinear: false, detail_textures: true });
        assert_eq!(settings.display_number(), 2);
    }

    #[test]
    fn modern_graphics_off_unless_set() {
        assert!(!Settings::default().modern_graphics());
        let settings = Settings::read(|query| if query == MODERN_GRAPHICS { "True".to_owned() } else { String::new() });
        assert!(settings.modern_graphics());
        let settings = Settings::read(|query| if query == MODERN_GRAPHICS { "False".to_owned() } else { String::new() });
        assert!(!settings.modern_graphics());
    }

    fn effects(settings: &Settings) -> [bool; 3] {
        EFFECTS.map(|key| settings.modern_effect(key))
    }

    const EFFECTS: [&str; 3] = [MODERN_RELIEF, MODERN_SPECULAR, MODERN_TERRAIN_DETAIL];

    #[test]
    fn modern_boxes_default_on() {
        let master = |value: &str| Settings::read(|query| if query == MODERN_GRAPHICS { value.to_owned() } else { String::new() });
        assert_eq!(effects(&master("True")), [true; 3]);
        assert_eq!(effects(&Settings::default()), [false; 3]);
        assert_eq!(effects(&master("False")), [false; 3]);
        let all = |on: &str| Settings::read(|query| if query == MODERN_GRAPHICS { on.to_owned() } else { "True".to_owned() });
        assert_eq!(effects(&all("False")), [false; 3]);
        assert_eq!(effects(&all("True")), [true; 3]);
        for (index, key) in EFFECTS.into_iter().enumerate() {
            let settings = Settings::read(|query| if query == key { "False".to_owned() } else { "True".to_owned() });
            let mut wanted = [true; 3];
            wanted[index] = false;
            assert_eq!(effects(&settings), wanted, "{key}");
        }
    }

    fn with(master: &str, key: &'static str, value: &str) -> Settings {
        Settings::read(|query| match query {
            MODERN_GRAPHICS => master.to_owned(),
            query if query == key => value.to_owned(),
            _ => String::new(),
        })
    }

    #[test]
    fn modern_levels_read_numbers_with_the_high_preset_as_default() {
        let high = LEVELS.map(|(key, ..)| with("True", key, "").modern_level(key));
        assert_eq!(high, [3, 4, 3, 1, 2, 2, 2, 5, 4, 5, 2, 0, 5, 5, 5, 2, 10, 1, 1, 5, 1, 1, 1, 1, 1, 0, 4, 1, 5, 5, 0, 0, 1, 6, 0, 0, 5, 1, 0, 0, 0, 0, 5, 5, 5, 2, 5, 5, 0, 4, 5, 0, 0]);
        let top = LEVELS.map(|(key, ..)| with("True", key, "99").modern_level(key));
        assert_eq!(top, [4, 5, 4, 2, 3, 2, 2, 10, 10, 10, 10, 10, 10, 10, 10, 3, 10, 1, 1, 10, 1, 1, 2, 1, 1, 10, 10, 1, 10, 10, 0, 0, 1, 10, 0, 0, 10, 1, 1, 99, 36, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 1, 3]);
        assert!(LEVELS.iter().all(|&(key, ..)| with("False", key, "1").modern_level(key) == 0));
        assert_eq!(with("True", MODERN_TEMPORAL_AA, "True").modern_level(MODERN_TEMPORAL_AA), 0);
        assert_eq!(with("True", MODERN_WOUNDS, "False").modern_level(MODERN_WOUNDS), 0);
        assert_eq!(with("True", MODERN_BLOOM, "7").modern_level(MODERN_BLOOM), 7);
        assert_eq!(with("True", MODERN_BLOOM, "0").modern_level(MODERN_BLOOM), 0);
        assert_eq!(with("True", MODERN_BLOOM, "40").modern_level(MODERN_BLOOM), 10);
        assert_eq!(with("True", MODERN_CONTACT_SHADOWS, "9").modern_level(MODERN_CONTACT_SHADOWS), 2);
        assert_eq!(with("True", MODERN_AO, "True").modern_level(MODERN_AO), 2);
        assert_eq!(with("True", MODERN_AO, "False").modern_level(MODERN_AO), 0);
        assert_eq!(with("True", MODERN_CONTACT_SHADOWS, "True").modern_level(MODERN_CONTACT_SHADOWS), 1);
        assert_eq!(with("True", MODERN_RELIEF, "3").modern_level(MODERN_RELIEF), 0);
    }

    fn photo(master: &str, active: &str, row: &str, twin: &str, fov: &str) -> Settings {
        Settings::read(|query| match query {
            MODERN_GRAPHICS => master.to_owned(),
            MODERN_PHOTO_ACTIVE => active.to_owned(),
            MODERN_PHOTO_FOV => fov.to_owned(),
            query if PHOTO_TWINS.iter().any(|&(key, _)| key == query) => row.to_owned(),
            query if PHOTO_TWINS.iter().any(|&(_, key)| key == query) => twin.to_owned(),
            _ => String::new(),
        })
    }

    #[test]
    fn photo_rows_answer_for_the_picture_rows_only_while_open() {
        let rows = |settings: Settings| PHOTO_TWINS.map(|(row, _)| settings.modern_level(row));
        assert_eq!(rows(photo("True", "0", "3", "7", "")), [3; 8]);
        assert_eq!(rows(photo("True", "", "3", "7", "")), [3; 8]);
        assert_eq!(rows(photo("True", "1", "3", "7", "")), [7; 8]);
        assert_eq!(rows(photo("True", "1", "3", "0", "")), [0; 8]);
        assert_eq!(rows(photo("True", "1", "3", "", "")), [3; 8]);
        assert_eq!(rows(photo("False", "1", "3", "7", "")), [0; 8]);
        let open = photo("True", "1", "3", "7", "");
        assert_eq!(PHOTO_TWINS.map(|(row, _)| open.text(row)), ["3"; 8]);
        assert_eq!(open.modern_level(MODERN_AO), 2);
    }

    #[test]
    fn photo_field_of_view_only_while_open_and_set() {
        let fov = |master: &str, active: &str, degrees: &str| photo(master, active, "", "", degrees).photo_fov();
        assert_eq!(fov("True", "0", "60"), None);
        assert_eq!(fov("True", "1", "0"), None);
        assert_eq!(fov("True", "1", ""), None);
        assert_eq!(fov("False", "1", "60"), None);
        assert_eq!(fov("True", "1", "60"), Some(60));
        assert_eq!(fov("True", "1", "3"), Some(PHOTO_FOV_LEAST));
        assert_eq!(fov("True", "1", "200"), Some(150));
    }

    #[test]
    fn msaa_is_the_retail_setting_and_off_under_modern() {
        let msaa = |master: &str, aa: &str, retail: &str| {
            Settings::read(|query| match query {
                MODERN_GRAPHICS => master.to_owned(),
                MODERN_ANTI_ALIASING => aa.to_owned(),
                MSAA => retail.to_owned(),
                _ => String::new(),
            })
            .msaa_samples()
        };
        assert_eq!(msaa("False", "4", "8"), 8);
        assert_eq!(msaa("False", "4", "3"), 1);
        assert_eq!(msaa("True", "4", "8"), 1);
        assert_eq!(msaa("True", "3", ""), 1);
    }
}
