use bevy::app::{App, AppExit, Plugin, Update};
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::prelude::{MessageWriter, Query, Res, ResMut, Single, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::schedule::common_conditions::resource_exists;
use bevy::state::prelude::{OnEnter, in_state};
use bevy::window::{CursorGrabMode, CursorOptions, Monitor, PrimaryWindow, Window, WindowMode};
use bevy_egui::EguiPrimaryContextPass;
use p2_engine::map::Session;
use p2_engine::play::{Play, PlaySystems};
use p2_host::settings::{self, Settings};
use p2_host::window::{Display, apply_window, note_video_modes, present_mode, video_modes};
use p2_host::game_dir::known_game_dir;
use p2_vm::Pointer;

use crate::launcher::{GameState, system_ini};

pub fn primary_window() -> Window {
    let system = known_game_dir().and_then(|root| system_ini(&root).ok()).unwrap_or_default();
    let settings = settings::load_settings();
    let start = Display::read(|section, key| settings.get(section, key).or_else(|| system.get(section, key)).map(str::to_owned));
    let mut window = Window {
        title: "POSTAL 2".into(),
        present_mode: present_mode(start.vsync, start.triple_buffering && start.window.fullscreen),
        visible: false,
        ..Default::default()
    };
    apply_window(&mut window, &start.window, None, start.display);
    window
}

pub struct GameWindowPlugin;

impl Plugin for GameWindowPlugin {
    fn build(&self, app: &mut App) {
        // Unfocused frames come from a timer alone: waking on mouse events would make motion jitter.
        let unfocused_mode = bevy::winit::UpdateMode::Reactive {
            wait: std::time::Duration::from_secs_f64(1.0 / 30.0),
            react_to_device_events: false,
            react_to_user_events: false,
            react_to_window_events: false,
        };
        app.insert_resource(bevy::camera::ClearColor(bevy::color::Color::BLACK))
            .insert_resource(bevy::winit::WinitSettings { focused_mode: bevy::winit::UpdateMode::Continuous, unfocused_mode })
            .add_systems(OnEnter(GameState::Launcher), release_cursor)
            .add_systems(Update, (grab_cursor.before(PlaySystems::Input).run_if(in_state(GameState::Playing)), apply_display.after(PlaySystems::Tick), show_window))
            .add_systems(EguiPrimaryContextPass, quit_when_asked.run_if(resource_exists::<Session>));
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
    if video_modes().is_empty()
        && let Some(monitor) = monitors.iter().nth(settings.display_number())
    {
        note_video_modes(monitor);
    }
    if let Some([x, y]) = world.take_set_mouse() {
        window.set_cursor_position(Some(bevy::math::Vec2::new(x, y)));
    }
    if let Some(request) = request {
        let display = settings.display_number();
        apply_window(&mut window, &request, monitors.iter().nth(display), display);
    }
    let fullscreen = !matches!(window.mode, WindowMode::Windowed);
    let mode = present_mode(settings.flag(settings::USE_VSYNC), fullscreen && settings.flag(settings::USE_TRIPLE_BUFFERING));
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

fn release_cursor(mut cursor: Single<&mut CursorOptions>) {
    cursor.visible = true;
    cursor.grab_mode = CursorGrabMode::None;
}

fn quit_when_asked(play: Res<Play>, mut exit: MessageWriter<AppExit>) {
    if play.world().is_some_and(|world| world.wants_quit()) {
        exit.write(AppExit::Success);
    }
}
