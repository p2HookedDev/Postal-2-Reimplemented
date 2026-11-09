use std::path::PathBuf;

use bevy::math::UVec2;
use bevy::window::{Monitor, MonitorSelection, PresentMode, VideoModeSelection, Window, WindowMode};
use p2_bevy_assets::TextureQuality;
use p2_package::Ini;
use p2_vm::{WindowRequest, ini_truthy};
use rustc_hash::FxHashMap;

fn settings_file() -> Option<PathBuf> {
    Some(crate::game_dir::config_folder()?.join("Settings.ini"))
}

pub fn load_settings() -> Ini {
    let bytes = settings_file().and_then(|file| std::fs::read(file).ok()).unwrap_or_default();
    Ini::from_bytes(&bytes)
}

/// Through a temp file and a rename, so a crash mid-write leaves the old file whole.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Display {
    pub window: WindowRequest,
    pub vsync: bool,
    pub triple_buffering: bool,
    pub display: usize,
}

impl Display {
    pub fn read(lookup: impl Fn(&str, &str) -> Option<String>) -> Self {
        let class = |key| lookup("Engine.Engine", key).unwrap_or_default();
        let (viewport, device) = (class("ViewportManager"), class("RenderDevice"));
        let flag = |section: &str, key| lookup(section, key).is_some_and(|value| ini_truthy(&value));
        let fullscreen = flag(&viewport, "StartupFullscreen");
        let mode = if fullscreen { "Fullscreen" } else { "Windowed" };
        let size = |axis| lookup(&viewport, &format!("{mode}Viewport{axis}")).and_then(|v| v.parse::<u32>().ok()).filter(|&v| v > 0);
        let size = size("X").zip(size("Y")).map(|(width, height)| [width, height]);
        let window = WindowRequest { size, fullscreen, borderless: flag(&viewport, "FullscreenWindowed") };
        let display = lookup(&device, "DisplayNumber").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        Self { window, vsync: flag(&device, "UseVSync"), triple_buffering: flag(&device, "UseTripleBuffering"), display }
    }
}

pub fn apply_window(window: &mut Window, request: &WindowRequest, monitor: Option<&Monitor>, display: usize) {
    let selection = MonitorSelection::Index(display);
    window.mode = match (request.fullscreen, request.borderless) {
        (false, _) => WindowMode::Windowed,
        (true, true) => WindowMode::BorderlessFullscreen(selection),
        (true, false) => {
            let modes = monitor.map(|monitor| monitor.video_modes.as_slice()).unwrap_or_default();
            let size = request.size.map(|[width, height]| UVec2::new(width, height));
            let mode = modes.iter().filter(|mode| Some(mode.physical_size) == size).max_by_key(|mode| mode.refresh_rate_millihertz);
            WindowMode::Fullscreen(selection, mode.map_or(VideoModeSelection::Current, |mode| VideoModeSelection::Specific(*mode)))
        }
    };
    if let (false, Some([width, height])) = (request.fullscreen, request.size) {
        window.resolution.set_physical_resolution(width, height);
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
    MODERN_SHADOWS,
    MODERN_CONTACT_SHADOWS,
    MODERN_AO,
    MODERN_RELIEF,
    MODERN_SPECULAR,
    MODERN_POST,
    MODERN_BAKE,
    MSAA,
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

pub const MODERN_GRAPHICS: &str = "ini:Engine.Engine.RenderDevice ModernGraphics";

pub const MODERN_SHADOWS: &str = "ini:Engine.Engine.RenderDevice ModernShadows";
pub const MODERN_CONTACT_SHADOWS: &str = "ini:Engine.Engine.RenderDevice ModernContactShadows";
pub const MODERN_AO: &str = "ini:Engine.Engine.RenderDevice ModernAO";
pub const MODERN_RELIEF: &str = "ini:Engine.Engine.RenderDevice ModernRelief";
pub const MODERN_SPECULAR: &str = "ini:Engine.Engine.RenderDevice ModernSpecular";
pub const MODERN_POST: &str = "ini:Engine.Engine.RenderDevice ModernPost";
pub const MODERN_BAKE: &str = "ini:Engine.Engine.RenderDevice ModernBake";
const MODERN_OFF_BY_DEFAULT: [&str; 1] = [MODERN_AO];

/// Read on the VM thread when a map starts and whenever a `set` changed the settings; features react to `Res<Settings>`
/// changing.
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

    pub fn display_number(&self) -> usize {
        self.number(DISPLAY_NUMBER, 0.0) as usize
    }

    pub fn msaa_samples(&self) -> u32 {
        match self.number(MSAA, 1.0) as u32 {
            samples @ (2 | 4 | 8) => samples,
            _ => 1,
        }
    }

    pub fn modern_graphics(&self) -> bool {
        self.flag(MODERN_GRAPHICS)
    }

    pub fn modern_effect(&self, query: &str) -> bool {
        let default = !MODERN_OFF_BY_DEFAULT.contains(&query);
        let text = self.text(query);
        self.modern_graphics() && if text.trim().is_empty() { default } else { ini_truthy(text) }
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

pub(crate) fn apply_texture_quality(settings: bevy::ecs::system::Res<Settings>) {
    p2_bevy_assets::set_texture_quality(settings.texture_quality());
}

pub fn present_mode(vsync: bool, triple_buffering: bool) -> PresentMode {
    match (vsync, triple_buffering) {
        (true, true) => PresentMode::Mailbox,
        (true, false) => PresentMode::AutoVsync,
        (false, _) => PresentMode::AutoNoVsync,
    }
}

static VIDEO_MODES: std::sync::OnceLock<Vec<p2_vm::VideoMode>> = std::sync::OnceLock::new();

pub fn note_video_modes(monitor: &Monitor) {
    if !monitor.video_modes.is_empty() {
        VIDEO_MODES.get_or_init(|| monitor.video_modes.iter().map(|mode| p2_vm::VideoMode { width: mode.physical_size.x, height: mode.physical_size.y, refresh_rate: mode.refresh_rate_millihertz / 1000, bit_depth: u32::from(mode.bit_depth) }).collect());
    }
}

pub fn video_modes() -> &'static [p2_vm::VideoMode] {
    VIDEO_MODES.get().map_or(&[], Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_from_the_viewport_manager_and_render_device() {
        let ini = Ini::parse(
            "[Engine.Engine]\nViewportManager=WinDrv.WindowsClient\nRenderDevice=D3DDrv.D3DRenderDevice\n\
             [WinDrv.WindowsClient]\nStartupFullscreen=True\nFullscreenViewportX=1920\nFullscreenViewportY=1080\n\
             WindowedViewportX=800\nWindowedViewportY=600\n[D3DDrv.D3DRenderDevice]\nUseVSync=True\nDisplayNumber=1\n",
        );
        let display = Display::read(|section, key| ini.get(section, key).map(str::to_owned));
        assert_eq!(display, Display { window: WindowRequest { size: Some([1920, 1080]), fullscreen: true, borderless: false }, vsync: true, triple_buffering: false, display: 1 });
        let mut window = Window::default();
        apply_window(&mut window, &WindowRequest { size: Some([800, 600]), fullscreen: false, borderless: false }, None, 0);
        assert_eq!((window.mode, window.resolution.physical_width()), (WindowMode::Windowed, 800));
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
        assert_eq!((present_mode(true, true), present_mode(true, false), present_mode(false, true)), (PresentMode::Mailbox, PresentMode::AutoVsync, PresentMode::AutoNoVsync));
    }

    #[test]
    fn modern_graphics_off_unless_set() {
        assert!(!Settings::default().modern_graphics());
        let settings = Settings::read(|query| if query == MODERN_GRAPHICS { "True".to_owned() } else { String::new() });
        assert!(settings.modern_graphics());
        let settings = Settings::read(|query| if query == MODERN_GRAPHICS { "False".to_owned() } else { String::new() });
        assert!(!settings.modern_graphics());
    }

    fn effects(settings: &Settings) -> [bool; 7] {
        EFFECTS.map(|key| settings.modern_effect(key))
    }

    const EFFECTS: [&str; 7] =
        [MODERN_SHADOWS, MODERN_CONTACT_SHADOWS, MODERN_AO, MODERN_RELIEF, MODERN_SPECULAR, MODERN_POST, MODERN_BAKE];

    #[test]
    fn modern_effects_default_on_but_ao() {
        let master = |value: &str| Settings::read(|query| if query == MODERN_GRAPHICS { value.to_owned() } else { String::new() });
        assert_eq!(effects(&master("True")), [true, true, false, true, true, true, true]);
        assert_eq!(effects(&Settings::default()), [false; 7]);
        assert_eq!(effects(&master("False")), [false; 7]);
        let all = |on: &str| Settings::read(|query| if query == MODERN_GRAPHICS { on.to_owned() } else { "True".to_owned() });
        assert_eq!(effects(&all("False")), [false; 7]);
        assert_eq!(effects(&all("True")), [true; 7]);
        for (index, key) in EFFECTS.into_iter().enumerate() {
            let settings = Settings::read(|query| if query == key { "False".to_owned() } else { "True".to_owned() });
            let mut wanted = [true; 7];
            wanted[index] = false;
            assert_eq!(effects(&settings), wanted, "{key}");
        }
    }
}
