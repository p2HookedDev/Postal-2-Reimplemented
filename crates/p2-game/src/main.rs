#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod input;
mod launcher;

use std::path::Path;

use anyhow::Context;
use bevy::DefaultPlugins;
use bevy::app::{App, AppExit, PluginGroup, PostUpdate, Startup, Update};
use bevy::ecs::error::Result;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::prelude::{Commands, Local, MessageWriter, Query, Res, ResMut, Resource, Single};
use bevy::ecs::schedule::common_conditions::resource_exists;
use bevy::state::prelude::{NextState, in_state};
use bevy::time::{Real, Time};
use bevy::input::ButtonInput;
use bevy::input::keyboard::KeyCode;
use bevy::input::mouse::MouseButton;
use bevy::log::LogPlugin;
use bevy::math::UVec2;
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::query::With;
use bevy::window::{CursorGrabMode, CursorOptions, Monitor, PrimaryWindow, Window, WindowMode, WindowPlugin};
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass};
use p2_engine::diagnostics::{DiagnosticsPlugin, capture_layer, install_panic_hook};
use p2_engine::display::{self, Display, Settings};
use p2_engine::game_dir::known_game_dir;
use p2_engine::map::{SceneTargets, Session, SessionHolder, map_package, present_session, spawn_hud_camera, travel_package};
use p2_engine::play::{FrameTimes, Play, PlaySystems, PlayWorld};
use p2_engine::P2EnginePlugins;
use p2_package::Ini;

use launcher::{GameState, Launcher, LauncherPlugin};

#[derive(Resource)]
struct Game {
    session: Session,
    map: String,
    load_started: bool,
    travel: Option<String>,
}

impl SessionHolder for Game {
    fn session(&mut self) -> &mut Session {
        &mut self.session
    }
}

fn main() {
    install_panic_hook();
    run();
}

fn start_map(args: impl IntoIterator<Item = String>, root: &Path) -> Option<String> {
    args.into_iter().find(|arg| {
        let path = Path::new(arg);
        let extension = |wanted: &str| path.extension().is_some_and(|e| e.eq_ignore_ascii_case(wanted));
        if extension("exe") || path.is_file() || arg.starts_with(['-', '+']) || arg.contains('=') {
            return false;
        }
        let file = if extension("fuk") { arg.clone() } else { format!("{arg}.fuk") };
        root.join("Maps").join(file).is_file()
    })
}

fn system_ini(root: &Path) -> anyhow::Result<Ini> {
    let ini = root.join("System").join("Postal2.ini");
    Ini::read(&ini).with_context(|| ini.display().to_string())
}

fn new_game(root: &Path) -> anyhow::Result<Game> {
    let system = system_ini(root)?;
    let map = match start_map(std::env::args().skip(1), root) {
        Some(name) => name,
        None => system.get("URL", "LocalMap").context("no [URL] LocalMap in Postal2.ini")?.to_owned(),
    };
    Ok(Game { session: Session::new(root.to_path_buf()), map: map_package(&map), load_started: false, travel: None })
}

fn run() {
    let system = known_game_dir().and_then(|root| system_ini(&root).ok()).unwrap_or_default();
    let settings = display::load_settings();
    let start = Display::read(|section, key| settings.get(section, key).or_else(|| system.get(section, key)).map(str::to_owned));
    let mut window = Window { title: "POSTAL 2".into(), present_mode: display::present_mode(start.vsync, start.triple_buffering && start.window.fullscreen), ..Default::default() };
    display::apply_window(&mut window, &start.window, None, start.display);

    p2_engine::play::init_script_stacks();
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins.set(WindowPlugin { primary_window: Some(window), ..Default::default() }).set(LogPlugin { custom_layer: capture_layer, ..Default::default() }),
        EguiPlugin::default(),
        P2EnginePlugins,
        DiagnosticsPlugin::<Game>::default(),
        LauncherPlugin,
    ))
    .add_systems(Startup, spawn_hud_camera)
    .add_systems(Update, (grab_cursor.run_if(in_state(GameState::Playing)), apply_display))
    .add_systems(Update, input::read_player_input.in_set(PlaySystems::Input).run_if(in_state(GameState::Playing)))
    .add_systems(PostUpdate, present_session::<Game>.in_set(PlaySystems::Present))
    .add_systems(EguiPrimaryContextPass, frame.run_if(resource_exists::<Game>))
    .add_systems(Update, log_frame_times);
    app.run();
}

#[derive(Default)]
struct FrameWindow {
    seconds: f32,
    frames: u32,
    shortest: f32,
    longest: f32,
}

fn log_frame_times(mut times: ResMut<FrameTimes>, time: Res<Time<Real>>, mut since: Local<FrameWindow>) {
    let delta = time.delta_secs();
    if since.frames == 0 {
        (since.shortest, since.longest) = (delta, delta);
    }
    since.seconds += delta;
    since.frames += 1;
    (since.shortest, since.longest) = (since.shortest.min(delta), since.longest.max(delta));
    if since.seconds < 1.0 {
        return;
    }
    let frames = f64::from(since.frames);
    let ms = |time: &std::time::Duration| time.as_secs_f64() * 1000.0;
    let list = |parts: &mut dyn Iterator<Item = (&str, std::time::Duration)>, per: f64| {
        parts.map(|(name, time)| format!("{name} {:.2}", ms(&time) / per)).collect::<Vec<_>>().join(", ")
    };
    bevy::log::info!(
        "frames {:.2} to {:.2} ms; per frame (ms), main thread: {}; vm thread: {}",
        since.shortest * 1000.0,
        since.longest * 1000.0,
        list(&mut times.totals.iter().map(|(n, t)| (*n, *t)), frames),
        list(&mut times.vm_totals.iter().map(|(n, t)| (*n, *t)), frames)
    );
    if times.vm_profile == p2_engine::play::VmProfile::TickByClass {
        let mut vm: Vec<_> = times.vm_parts.drain().collect();
        vm.sort_by_key(|(_, total)| std::cmp::Reverse(*total));
        bevy::log::info!("vm tick by class and phase (ms per frame): {}", list(&mut vm.iter().take(10).map(|(n, t)| (n.as_str(), *t)), frames));
    }
    if let Some(mut slowest) = times.take_slowest() {
        slowest.main.sort_by_key(|(_, time)| std::cmp::Reverse(*time));
        slowest.vm.sort_by_key(|(_, time)| std::cmp::Reverse(*time));
        let named: std::time::Duration = slowest.main.iter().map(|(_, time)| *time).sum();
        bevy::log::info!(
            "slowest frame {:.2} ms: main thread: {}, rest (Bevy, render) {:.2}; its vm frame: {}{}",
            ms(&slowest.length),
            list(&mut slowest.main.iter().take(6).map(|(n, t)| (*n, *t)), 1.0),
            ms(&slowest.length.saturating_sub(named)),
            list(&mut slowest.vm.iter().map(|(n, t)| (*n, *t)), 1.0),
            if slowest.profile.is_empty() {
                String::new()
            } else {
                format!("; vm tick: {}", list(&mut slowest.profile.iter().take(8).map(|(n, t)| (n.as_str(), *t)), 1.0))
            }
        );
    }
    times.totals.clear();
    times.vm_totals.clear();
    *since = Default::default();
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
    let mode = display::present_mode(settings.flag("ini:Engine.Engine.RenderDevice UseVSync"), fullscreen && settings.flag("ini:Engine.Engine.RenderDevice UseTripleBuffering"));
    if window.present_mode != mode {
        window.present_mode = mode;
    }
}

fn grab_cursor(mut cursor: Single<&mut CursorOptions>, mouse: Res<ButtonInput<MouseButton>>, keys: Res<ButtonInput<KeyCode>>, play: Res<Play>) {
    if play.world().is_some_and(PlayWorld::shows_mouse) {
        if !cursor.visible || cursor.grab_mode != CursorGrabMode::None {
            cursor.visible = true;
            cursor.grab_mode = CursorGrabMode::None;
        }
        return;
    }
    if mouse.just_pressed(MouseButton::Left) {
        cursor.visible = false;
        cursor.grab_mode = CursorGrabMode::Locked;
    }
    if keys.just_pressed(KeyCode::Escape) {
        cursor.visible = true;
        cursor.grab_mode = CursorGrabMode::None;
    }
}

fn frame(
    mut contexts: EguiContexts,
    mut t: SceneTargets,
    mut game: ResMut<Game>,
    mut exit: MessageWriter<AppExit>,
    mut launcher: ResMut<Launcher>,
    mut next: ResMut<NextState<GameState>>,
    mut commands: Commands,
) -> Result {
    let game = &mut *game;
    if let Some(url) = t.play.world_mut().and_then(PlayWorld::take_travel) {
        if let Some(package) = travel_package(&url) {
            game.map = package;
        }
        game.load_started = false;
        game.travel = Some(url);
    }
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
    let mut failure = None;
    if !game.load_started {
        game.load_started = true;
        failure = game.session.begin_load(&game.map, game.travel.take(), &mut t).err().map(|e| e.to_string());
    }
    if let Some(Err(e)) = game.session.finish_load(&mut t) {
        failure = Some(e.to_string());
    }
    let mut failure = failure.map(|failure| format!("{} could not be loaded: {failure}", game.map));
    if t.play.world().is_some_and(PlayWorld::vm_lost) {
        failure = Some(format!("{} stopped: the game's script thread crashed. Details are in diagnostics.txt.", game.map));
    }
    if let Some(failure) = failure {
        launcher.failure = Some(failure);
        game.session.clear(&mut t);
        t.view.clear();
        t.view.viewport = None;
        commands.remove_resource::<Game>();
        next.set(GameState::Launcher);
        return Ok(());
    }
    if game.session.loading().is_some() {
        game.session.loading_frame(&ctx, rect, &mut contexts, &mut t);
    } else {
        game.session.frame(&ctx, rect, &mut contexts, &mut t);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_map_skips_what_names_no_map() {
        let root = std::env::temp_dir().join("p2-game-start-map");
        let maps = root.join("Maps");
        std::fs::create_dir_all(&maps).expect("making the test Maps folder");
        std::fs::write(maps.join("Church.fuk"), b"").expect("writing the test map");
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let file = maps.join("Church.fuk").display().to_string();
        let skipped = args(&["C:\\Games\\p2-game.exe", file.as_str(), "-windowed", "+connect", "Game=x", "Nope", "Church"]);
        assert_eq!(start_map(skipped, &root).as_deref(), Some("Church"));
        assert_eq!(start_map(args(&["Church.fuk"]), &root).as_deref(), Some("Church.fuk"));
        assert_eq!(start_map(args(&["Nope", "-log", "x=1"]), &root), None);
        assert_eq!(start_map(args(&[file.as_str()]), &root), None);
        assert_eq!(start_map(Vec::new(), &root), None);
    }
}
