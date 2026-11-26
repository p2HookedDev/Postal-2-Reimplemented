#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod input;
mod launcher;

use std::path::Path;

use anyhow::Context;
use bevy::DefaultPlugins;
use bevy::render::RenderPlugin;
use bevy::render::settings::{Backends, WgpuFeatures, WgpuSettings};
use bevy::app::{App, AppExit, Plugin, PluginGroup, Update};
use bevy::ecs::error::Result;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::prelude::{MessageReader, MessageWriter, Query, Res, ResMut, Single};
use bevy::ecs::schedule::common_conditions::resource_exists;
use bevy::state::prelude::{NextState, in_state};
use bevy::log::LogPlugin;
use bevy::math::UVec2;
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::query::With;
use bevy::window::{CursorGrabMode, CursorOptions, Monitor, PrimaryWindow, Window, WindowMode, WindowPlugin};
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass};
use p2_engine::diagnostics::{DiagnosticsPlugin, StageTimingPlugin, capture_layer, console_layer, install_panic_hook, log_frame_times};
use p2_host::display::{self, Display, Settings};
use p2_host::game_dir::known_game_dir;
use p2_engine::map::{CanvasArea, MapFailed, MapLoadPlugin, MapSystems, Session, map_package, start_map};
use p2_engine::play::{Play, PlaySystems};
use p2_vm::Pointer;
use p2_engine::P2EnginePlugins;
use p2_package::Ini;

use launcher::{GameState, Launcher, LauncherPlugin};

fn main() {
    install_panic_hook();
    run();
}

fn system_ini(root: &Path) -> anyhow::Result<Ini> {
    let ini = root.join("System").join("Postal2.ini");
    Ini::read(&ini).with_context(|| ini.display().to_string())
}

fn new_game(root: &Path) -> anyhow::Result<(String, Session)> {
    let system = system_ini(root)?;
    let map = match start_map(std::env::args().skip(1), root) {
        Some(name) => name,
        None => system.get("URL", "LocalMap").context("no [URL] LocalMap in Postal2.ini")?.to_owned(),
    };
    Ok((map_package(&map), Session::new(root.to_path_buf())))
}

fn run() {
    let system = known_game_dir().and_then(|root| system_ini(&root).ok()).unwrap_or_default();
    let settings = display::load_settings();
    let start = Display::read(|section, key| settings.get(section, key).or_else(|| system.get(section, key)).map(str::to_owned));
    // Shown once it has drawn, so the system's blank window never flashes white.
    let mut window = Window {
        title: "POSTAL 2".into(),
        present_mode: display::present_mode(start.vsync, start.triple_buffering && start.window.fullscreen),
        visible: false,
        ..Default::default()
    };
    display::apply_window(&mut window, &start.window, None, start.display);

    p2_engine::play::init_script_stacks();
    let log = LogPlugin { custom_layer: capture_layer, fmt_layer: console_layer, ..Default::default() };
    // wgpu's scopes are spans at its info level, which the default filter keeps out; wgpu_core logs nothing there.
    #[cfg(feature = "profile")]
    let log = LogPlugin { filter: format!("{}wgpu_core=info", bevy::log::DEFAULT_FILTER), ..log };
    let mut app = App::new();
    // Without focus, frames come from a timer alone, so the game leaves the CPU and GPU to the program in front. No
    // event wakes a frame: each mouse event would make one of its own between the timed ones, and motion would jitter.
    app.insert_resource(bevy::camera::ClearColor(bevy::color::Color::BLACK));
    let unfocused_mode = bevy::winit::UpdateMode::Reactive {
        wait: std::time::Duration::from_secs_f64(1.0 / 30.0),
        react_to_device_events: false,
        react_to_user_events: false,
        react_to_window_events: false,
    };
    app.insert_resource(bevy::winit::WinitSettings { focused_mode: bevy::winit::UpdateMode::Continuous, unfocused_mode });
    app.add_plugins((
        DefaultPlugins
            // Without DX12 among the backends Bevy turns off wgpu's validation of every indirect draw. Bevy's pass timing
            // also counts a pass's shader runs when the device can; nothing reads those counts, so they are not made.
            .set(RenderPlugin {
                render_creation: WgpuSettings { backends: Some(Backends::VULKAN), disabled_features: Some(WgpuFeatures::PIPELINE_STATISTICS_QUERY), ..Default::default() }.into(),
                ..Default::default()
            })
            .set(WindowPlugin { primary_window: Some(window), ..Default::default() }).set(log),
        EguiPlugin::default(),
        P2EnginePlugins,
        DiagnosticsPlugin,
        StageTimingPlugin,
        p2_render::asset_churn::AssetChurnPlugin,
        LauncherPlugin,
        MapLoadPlugin,
        GamePlugin,
    ));
    #[cfg(feature = "profile")]
    {
        app.world_mut().resource_mut::<p2_host::frame_times::FrameTimes>().vm_profile = p2_host::frame_times::VmProfile::TickByClass;
    }
    app.run();
}

struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                grab_cursor.before(PlaySystems::Input).run_if(in_state(GameState::Playing)),
                input::read_player_input.in_set(PlaySystems::Input).run_if(in_state(GameState::Playing)),
                apply_display.after(PlaySystems::Tick),
                log_frame_times,
                show_window,
            ),
        )
        .add_systems(
            EguiPrimaryContextPass,
            (quit_when_asked, place_map.after(MapSystems::Load).before(MapSystems::Paint)).run_if(resource_exists::<Session>),
        );
    }
}

fn apply_display(
    mut play: ResMut<Play>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    monitors: Query<&Monitor>,
    mut settings: ResMut<Settings>,
) {
    let Some(world) = play.world_mut() else { return };
    let (request, new) = world.take_display();
    if let Some(new) = new {
        settings.set_if_neq(new);
    }
    if display::video_modes().is_empty()
        && let Some(monitor) = monitors.iter().nth(settings.display_number())
    {
        display::note_video_modes(monitor);
    }
    if let Some([x, y]) = world.take_set_mouse() {
        window.set_cursor_position(Some(bevy::math::Vec2::new(x, y)));
    }
    if let Some(request) = request {
        let display = settings.display_number();
        display::apply_window(&mut window, &request, monitors.iter().nth(display), display);
    }
    let fullscreen = !matches!(window.mode, WindowMode::Windowed);
    let mode = display::present_mode(settings.flag(display::USE_VSYNC), fullscreen && settings.flag(display::USE_TRIPLE_BUFFERING));
    if window.present_mode != mode {
        window.present_mode = mode;
    }
}

const FRAMES_BEFORE_SHOWN: u32 = 3;

fn show_window(mut window: Single<&mut Window, With<PrimaryWindow>>, frames: Res<bevy::diagnostic::FrameCount>) {
    if !window.visible && frames.0 >= FRAMES_BEFORE_SHOWN {
        window.visible = true;
    }
}

fn grab_cursor(window: Single<(&Window, &mut CursorOptions)>, play: Res<Play>) {
    let Some(world) = play.world() else { return };
    let (window, mut cursor) = window.into_inner();
    let pointer = if window.focused { world.pointer() } else { Pointer::System };
    let visible = pointer == Pointer::System;
    let grab_mode = match pointer {
        // Not confined while hidden: Windows then holds the pointer at the window's centre.
        Pointer::System | Pointer::Drawn => CursorGrabMode::None,
        Pointer::Captured => CursorGrabMode::Locked,
    };
    if cursor.visible != visible || cursor.grab_mode != grab_mode {
        cursor.visible = visible;
        cursor.grab_mode = grab_mode;
    }
}

fn quit_when_asked(play: Res<Play>, mut exit: MessageWriter<AppExit>) {
    if play.world().is_some_and(|world| world.wants_quit()) {
        exit.write(AppExit::Success);
    }
}

fn place_map(
    mut contexts: EguiContexts,
    mut view: ResMut<p2_render::view::MeshView>,
    mut area: ResMut<CanvasArea>,
    mut launcher: ResMut<Launcher>,
    mut next: ResMut<NextState<GameState>>,
    mut failed: MessageReader<MapFailed>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    let rect = ctx.viewport_rect();
    let scale = ctx.pixels_per_point();
    let size = UVec2::new((rect.width() * scale) as u32, (rect.height() * scale) as u32);
    view.viewport = (size.x > 0 && size.y > 0).then_some((UVec2::ZERO, size));
    area.0 = Some(rect);
    if let Some(MapFailed(failure)) = failed.read().last() {
        launcher.failure = Some(failure.clone());
        view.viewport = None;
        area.0 = None;
        next.set(GameState::Launcher);
    }
    Ok(())
}
