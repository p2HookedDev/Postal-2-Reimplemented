use bevy::ecs::prelude::{MessageReader, Res, ResMut, Single};
use bevy::input::keyboard::{KeyCode, KeyboardInput};
use bevy::input::mouse::{AccumulatedMouseMotion, MouseButton, MouseButtonInput, MouseWheel};
use bevy::input::{ButtonInput, ButtonState};
use bevy::window::{CursorGrabMode, CursorOptions, Window};
use p2_engine::play::PlayerInput;

pub fn read_player_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut buttons: ResMut<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    window: Single<(&Window, &CursorOptions)>,
    (mut keyboard, mut clicks, mut wheel): (MessageReader<KeyboardInput>, MessageReader<MouseButtonInput>, MessageReader<MouseWheel>),
    mut input: ResMut<PlayerInput>,
) {
    let (window, cursor) = *window;
    if !window.focused {
        buttons.release_all();
    }
    input.typed.clear();
    input.key_events.clear();
    for key in keyboard.read() {
        let pressed = key.state == ButtonState::Pressed;
        if pressed && let Some(text) = &key.text {
            input.typed.push_str(text);
        }
        let Some(name) = engine_key(key.key_code).filter(|_| window.focused && key.key_code != KeyCode::F10) else { continue };
        if pressed || !keys.get_pressed().any(|&other| engine_key(other) == Some(name)) {
            input.key_events.push((name, pressed));
        }
    }
    for click in clicks.read().filter(|_| window.focused) {
        if let Some(name) = mouse_key(click.button) {
            input.key_events.push((name, click.state == ButtonState::Pressed));
        }
    }
    for notch in wheel.read().filter(|notch| window.focused && notch.y != 0.0) {
        let name = if notch.y > 0.0 { "MouseWheelUp" } else { "MouseWheelDown" };
        input.key_events.extend([(name, true), (name, false)]);
    }
    input.cursor = window.cursor_position().map(|position| position.to_array());
    input.playing = true;
    input.held.clear();
    input.held.extend(keys.get_pressed().filter(|&&key| key != KeyCode::F10).filter_map(|&key| engine_key(key)));
    input.held.extend(buttons.get_pressed().filter_map(|&button| mouse_key(button)));
    input.mouse = if cursor.grab_mode == CursorGrabMode::None || !window.focused { [0.0, 0.0] } else { [motion.delta.x, -motion.delta.y] };
}

fn mouse_key(button: MouseButton) -> Option<&'static str> {
    match button {
        MouseButton::Left => Some("LeftMouse"),
        MouseButton::Right => Some("RightMouse"),
        MouseButton::Middle => Some("MiddleMouse"),
        _ => None,
    }
}

fn engine_key(key: KeyCode) -> Option<&'static str> {
    Some(match key {
        KeyCode::KeyA => "A",
        KeyCode::KeyB => "B",
        KeyCode::KeyC => "C",
        KeyCode::KeyD => "D",
        KeyCode::KeyE => "E",
        KeyCode::KeyF => "F",
        KeyCode::KeyG => "G",
        KeyCode::KeyH => "H",
        KeyCode::KeyI => "I",
        KeyCode::KeyJ => "J",
        KeyCode::KeyK => "K",
        KeyCode::KeyL => "L",
        KeyCode::KeyM => "M",
        KeyCode::KeyN => "N",
        KeyCode::KeyO => "O",
        KeyCode::KeyP => "P",
        KeyCode::KeyQ => "Q",
        KeyCode::KeyR => "R",
        KeyCode::KeyS => "S",
        KeyCode::KeyT => "T",
        KeyCode::KeyU => "U",
        KeyCode::KeyV => "V",
        KeyCode::KeyW => "W",
        KeyCode::KeyX => "X",
        KeyCode::KeyY => "Y",
        KeyCode::KeyZ => "Z",
        KeyCode::Digit0 => "0",
        KeyCode::Digit1 => "1",
        KeyCode::Digit2 => "2",
        KeyCode::Digit3 => "3",
        KeyCode::Digit4 => "4",
        KeyCode::Digit5 => "5",
        KeyCode::Digit6 => "6",
        KeyCode::Digit7 => "7",
        KeyCode::Digit8 => "8",
        KeyCode::Digit9 => "9",
        KeyCode::F1 => "F1",
        KeyCode::F2 => "F2",
        KeyCode::F3 => "F3",
        KeyCode::F4 => "F4",
        KeyCode::F5 => "F5",
        KeyCode::F6 => "F6",
        KeyCode::F7 => "F7",
        KeyCode::F8 => "F8",
        KeyCode::F9 => "F9",
        KeyCode::F10 => "F10",
        KeyCode::F11 => "F11",
        KeyCode::F12 => "F12",
        KeyCode::Numpad0 => "NumPad0",
        KeyCode::Numpad1 => "NumPad1",
        KeyCode::Numpad2 => "NumPad2",
        KeyCode::Numpad3 => "NumPad3",
        KeyCode::Numpad4 => "NumPad4",
        KeyCode::Numpad5 => "NumPad5",
        KeyCode::Numpad6 => "NumPad6",
        KeyCode::Numpad7 => "NumPad7",
        KeyCode::Numpad8 => "NumPad8",
        KeyCode::Numpad9 => "NumPad9",
        KeyCode::Space => "Space",
        KeyCode::ShiftLeft | KeyCode::ShiftRight => "Shift",
        KeyCode::ControlLeft | KeyCode::ControlRight => "Ctrl",
        KeyCode::AltLeft | KeyCode::AltRight => "Alt",
        KeyCode::Tab => "Tab",
        KeyCode::Enter | KeyCode::NumpadEnter => "Enter",
        KeyCode::Escape => "Escape",
        KeyCode::Backspace => "Backspace",
        KeyCode::ArrowUp => "Up",
        KeyCode::ArrowDown => "Down",
        KeyCode::ArrowLeft => "Left",
        KeyCode::ArrowRight => "Right",
        KeyCode::PageUp => "PageUp",
        KeyCode::PageDown => "PageDown",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::Insert => "Insert",
        KeyCode::Delete => "Delete",
        KeyCode::Pause => "Pause",
        KeyCode::CapsLock => "CapsLock",
        KeyCode::NumLock => "NumLock",
        KeyCode::ScrollLock => "ScrollLock",
        KeyCode::PrintScreen => "PrintScrn",
        KeyCode::NumpadMultiply => "GreyStar",
        KeyCode::NumpadAdd => "GreyPlus",
        KeyCode::NumpadSubtract => "GreyMinus",
        KeyCode::NumpadDivide => "GreySlash",
        KeyCode::NumpadDecimal => "NumPadPeriod",
        KeyCode::Semicolon => "Semicolon",
        KeyCode::Equal => "Equals",
        KeyCode::Comma => "Comma",
        KeyCode::Minus => "Minus",
        KeyCode::Period => "Period",
        KeyCode::Slash => "Slash",
        KeyCode::Backquote => "Tilde",
        KeyCode::BracketLeft => "LeftBracket",
        KeyCode::Backslash => "Backslash",
        KeyCode::BracketRight => "RightBracket",
        KeyCode::Quote => "SingleQuote",
        _ => return None,
    })
}
