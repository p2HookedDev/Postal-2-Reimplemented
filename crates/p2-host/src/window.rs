use bevy::math::UVec2;
use bevy::window::{Monitor, MonitorSelection, PresentMode, VideoModeSelection, Window, WindowMode};
use p2_vm::{WindowRequest, ini_truthy};

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
    use p2_package::Ini;

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
        assert_eq!((present_mode(true, true), present_mode(true, false), present_mode(false, true)), (PresentMode::Mailbox, PresentMode::AutoVsync, PresentMode::AutoNoVsync));
    }
}
