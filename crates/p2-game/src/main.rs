#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod input;
mod launcher;

use std::path::Path;

use anyhow::Context;
use bevy::DefaultPlugins;
use bevy::render::RenderPlugin;
use bevy::render::settings::{Backends, WgpuSettings};
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
use p2_engine::display::{self, Display, Settings};
use p2_engine::game_dir::known_game_dir;
use p2_engine::map::{MapFailed, MapLoadPlugin, MapSystems, SceneTargets, Session, map_package, start_map};
use p2_engine::play::{Play, PlaySystems};
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
    let mut window = Window { title: "POSTAL 2".into(), present_mode: display::present_mode(start.vsync, start.triple_buffering && start.window.fullscreen), ..Default::default() };
    display::apply_window(&mut window, &start.window, None, start.display);

    p2_engine::play::init_script_stacks();
    let mut app = App::new();
    // Never the timed, event-woken updates Bevy runs a window without focus on: every mouse event then makes a frame
    // of its own between the timed ones, and what moves on screen jitters.
    app.insert_resource(bevy::winit::WinitSettings { focused_mode: bevy::winit::UpdateMode::Continuous, unfocused_mode: bevy::winit::UpdateMode::Continuous });
    app.add_plugins((
        DefaultPlugins
            // Without DX12 among the backends Bevy turns off wgpu's validation of every indirect draw.
            .set(RenderPlugin { render_creation: WgpuSettings { backends: Some(Backends::VULKAN), ..Default::default() }.into(), ..Default::default() })
            .set(WindowPlugin { primary_window: Some(window), ..Default::default() }).set(LogPlugin { custom_layer: capture_layer, fmt_layer: console_layer, ..Default::default() }),
        EguiPlugin::default(),
        P2EnginePlugins,
        DiagnosticsPlugin,
        StageTimingPlugin,
        LauncherPlugin,
        MapLoadPlugin,
        GamePlugin,
    ));
    #[cfg(feature = "profile")]
    {
        app.world_mut().resource_mut::<p2_engine::play::FrameTimes>().vm_profile = p2_engine::play::VmProfile::TickByClass;
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
            ),
        )
        .add_systems(EguiPrimaryContextPass, frame.after(MapSystems::Load).run_if(resource_exists::<Session>));
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

fn grab_cursor(window: Single<(&Window, &mut CursorOptions)>, play: Res<Play>) {
    let Some(world) = play.world() else { return };
    let (window, mut cursor) = window.into_inner();
    let free = world.shows_mouse() || !window.focused;
    let grab_mode = if free { CursorGrabMode::None } else { CursorGrabMode::Locked };
    if cursor.visible != free || cursor.grab_mode != grab_mode {
        cursor.visible = free;
        cursor.grab_mode = grab_mode;
    }
}

fn frame(
    mut contexts: EguiContexts,
    mut t: SceneTargets,
    mut session: ResMut<Session>,
    mut exit: MessageWriter<AppExit>,
    mut launcher: ResMut<Launcher>,
    mut next: ResMut<NextState<GameState>>,
    mut failed: MessageReader<MapFailed>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    let rect = ctx.viewport_rect();
    let scale = ctx.pixels_per_point();
    let size = UVec2::new((rect.width() * scale) as u32, (rect.height() * scale) as u32);
    t.view.viewport = (size.x > 0 && size.y > 0).then_some((UVec2::ZERO, size));
    if let Some(world) = t.play.world()
        && world.wants_quit()
    {
        exit.write(AppExit::Success);
    }
    if let Some(MapFailed(failure)) = failed.read().last() {
        launcher.failure = Some(failure.clone());
        session.clear(&mut t);
        t.scenes.write(p2_engine::mesh_view::SceneChange::Clear);
        t.view.viewport = None;
        t.commands.remove_resource::<Session>();
        next.set(GameState::Launcher);
        return Ok(());
    }
    if session.loading().is_some() {
        session.loading_frame(&ctx, rect, &mut contexts, &mut t);
    } else {
        session.frame(&ctx, rect, &mut contexts, &mut t);
    }
    Ok(())
}
