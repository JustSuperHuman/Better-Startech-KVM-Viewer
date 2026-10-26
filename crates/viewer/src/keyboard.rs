//! Physical positions, not text or layout-translated egui key events.
use std::collections::BTreeSet;
use winit::keyboard::KeyCode;

#[derive(Default)]
pub struct PhysicalKeys {
    held: BTreeSet<u8>,
}

impl PhysicalKeys {
    /// Local deduplication only. Helper owns remote InputState and modifiers.
    pub fn transition(&mut self, usage: u8, down: bool) -> bool {
        if down {
            self.held.insert(usage)
        } else {
            self.held.remove(&usage)
        }
    }
    pub fn clear(&mut self) {
        self.held.clear();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRoute {
    Local,
    Consume,
    ToggleCapture,
    Remote { usage: u8, down: bool },
}

pub fn route_key(
    captured: bool,
    pasting: bool,
    code: Option<KeyCode>,
    down: bool,
    repeat: bool,
    synthetic: bool,
) -> KeyRoute {
    if code == Some(KeyCode::F12) {
        return if down && !repeat && !synthetic {
            KeyRoute::ToggleCapture
        } else {
            KeyRoute::Consume
        };
    }
    if pasting {
        return KeyRoute::Consume;
    }
    if !captured {
        return KeyRoute::Local;
    }
    if repeat || synthetic {
        return KeyRoute::Consume;
    }
    match code.and_then(usb_usage) {
        Some(usage) => KeyRoute::Remote { usage, down },
        None => KeyRoute::Consume,
    }
}

pub fn usb_usage(code: KeyCode) -> Option<u8> {
    use KeyCode::*;
    Some(match code {
        KeyA => 0x04,
        KeyB => 0x05,
        KeyC => 0x06,
        KeyD => 0x07,
        KeyE => 0x08,
        KeyF => 0x09,
        KeyG => 0x0a,
        KeyH => 0x0b,
        KeyI => 0x0c,
        KeyJ => 0x0d,
        KeyK => 0x0e,
        KeyL => 0x0f,
        KeyM => 0x10,
        KeyN => 0x11,
        KeyO => 0x12,
        KeyP => 0x13,
        KeyQ => 0x14,
        KeyR => 0x15,
        KeyS => 0x16,
        KeyT => 0x17,
        KeyU => 0x18,
        KeyV => 0x19,
        KeyW => 0x1a,
        KeyX => 0x1b,
        KeyY => 0x1c,
        KeyZ => 0x1d,
        Digit1 => 0x1e,
        Digit2 => 0x1f,
        Digit3 => 0x20,
        Digit4 => 0x21,
        Digit5 => 0x22,
        Digit6 => 0x23,
        Digit7 => 0x24,
        Digit8 => 0x25,
        Digit9 => 0x26,
        Digit0 => 0x27,
        Enter => 0x28,
        Escape => 0x29,
        Backspace => 0x2a,
        Tab => 0x2b,
        Space => 0x2c,
        Minus => 0x2d,
        Equal => 0x2e,
        BracketLeft => 0x2f,
        BracketRight => 0x30,
        Backslash => 0x31,
        Semicolon => 0x33,
        Quote => 0x34,
        Backquote => 0x35,
        Comma => 0x36,
        Period => 0x37,
        Slash => 0x38,
        CapsLock => 0x39,
        F1 => 0x3a,
        F2 => 0x3b,
        F3 => 0x3c,
        F4 => 0x3d,
        F5 => 0x3e,
        F6 => 0x3f,
        F7 => 0x40,
        F8 => 0x41,
        F9 => 0x42,
        F10 => 0x43,
        F11 => 0x44,
        // F12 (0x45) is exclusively the local capture/release toggle.
        F12 => return None,
        PrintScreen => 0x46,
        ScrollLock => 0x47,
        Pause => 0x48,
        Insert => 0x49,
        Home => 0x4a,
        PageUp => 0x4b,
        Delete => 0x4c,
        End => 0x4d,
        PageDown => 0x4e,
        ArrowRight => 0x4f,
        ArrowLeft => 0x50,
        ArrowDown => 0x51,
        ArrowUp => 0x52,
        NumLock => 0x53,
        NumpadDivide => 0x54,
        NumpadMultiply => 0x55,
        NumpadSubtract => 0x56,
        NumpadAdd => 0x57,
        NumpadEnter => 0x58,
        Numpad1 => 0x59,
        Numpad2 => 0x5a,
        Numpad3 => 0x5b,
        Numpad4 => 0x5c,
        Numpad5 => 0x5d,
        Numpad6 => 0x5e,
        Numpad7 => 0x5f,
        Numpad8 => 0x60,
        Numpad9 => 0x61,
        Numpad0 => 0x62,
        NumpadDecimal => 0x63,
        IntlBackslash => 0x64,
        ContextMenu => 0x65,
        NumpadEqual => 0x67,
        F13 => 0x68,
        F14 => 0x69,
        F15 => 0x6a,
        F16 => 0x6b,
        F17 => 0x6c,
        F18 => 0x6d,
        F19 => 0x6e,
        F20 => 0x6f,
        F21 => 0x70,
        F22 => 0x71,
        F23 => 0x72,
        F24 => 0x73,
        Help => 0x75,
        Again => 0x79,
        Undo => 0x7a,
        Cut => 0x7b,
        Copy => 0x7c,
        Paste => 0x7d,
        Find => 0x7e,
        AudioVolumeMute => 0x7f,
        AudioVolumeUp => 0x80,
        AudioVolumeDown => 0x81,
        NumpadComma => 0x85,
        IntlRo => 0x87,
        KanaMode => 0x88,
        IntlYen => 0x89,
        Convert => 0x8a,
        NonConvert => 0x8b,
        Lang1 => 0x90,
        Lang2 => 0x91,
        Lang3 => 0x92,
        Lang4 => 0x93,
        Lang5 => 0x94,
        ControlLeft => 0xe0,
        ShiftLeft => 0xe1,
        AltLeft => 0xe2,
        SuperLeft => 0xe3,
        ControlRight => 0xe4,
        ShiftRight => 0xe5,
        AltRight => 0xe6,
        SuperRight => 0xe7,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modifiers_are_distinct_and_f12_is_local() {
        let modifiers = [
            KeyCode::ControlLeft,
            KeyCode::ShiftLeft,
            KeyCode::AltLeft,
            KeyCode::SuperLeft,
            KeyCode::ControlRight,
            KeyCode::ShiftRight,
            KeyCode::AltRight,
            KeyCode::SuperRight,
        ];
        for (index, code) in modifiers.into_iter().enumerate() {
            assert_eq!(usb_usage(code), Some(0xe0 + index as u8));
        }
        assert_eq!(usb_usage(KeyCode::F12), None);
        assert_eq!(usb_usage(KeyCode::KeyV), Some(0x19));
    }
    #[test]
    fn repeats_and_unmatched_releases_never_forward() {
        let mut keys = PhysicalKeys::default();
        assert!(!keys.transition(4, false));
        assert!(keys.transition(4, true));
        assert!(!keys.transition(4, true));
        assert!(keys.transition(4, false));
        keys.transition(0xe4, true);
        keys.clear();
        assert!(!keys.transition(0xe4, false));
    }
    #[test]
    fn f12_never_forwards_and_only_initial_down_toggles() {
        for captured in [false, true] {
            for pasting in [false, true] {
                assert_eq!(
                    route_key(captured, pasting, Some(KeyCode::F12), true, false, false),
                    KeyRoute::ToggleCapture
                );
                assert_eq!(
                    route_key(captured, pasting, Some(KeyCode::F12), false, false, false),
                    KeyRoute::Consume
                );
                assert_eq!(
                    route_key(captured, pasting, Some(KeyCode::F12), true, true, false),
                    KeyRoute::Consume
                );
            }
        }
    }
    #[test]
    fn captured_ctrl_v_is_remote_while_paste_blocks_other_input() {
        for (code, usage) in [(KeyCode::ControlLeft, 0xe0), (KeyCode::KeyV, 0x19)] {
            assert_eq!(
                route_key(true, false, Some(code), true, false, false),
                KeyRoute::Remote { usage, down: true }
            );
            assert_eq!(
                route_key(true, false, Some(code), false, false, false),
                KeyRoute::Remote { usage, down: false }
            );
            assert_eq!(
                route_key(false, false, Some(code), true, false, false),
                KeyRoute::Local
            );
            assert_eq!(
                route_key(true, true, Some(code), true, false, false),
                KeyRoute::Consume
            );
            assert_eq!(
                route_key(false, true, Some(code), true, false, false),
                KeyRoute::Consume
            );
            assert_eq!(
                route_key(true, false, Some(code), true, true, false),
                KeyRoute::Consume
            );
            assert_eq!(
                route_key(true, false, Some(code), true, false, true),
                KeyRoute::Consume
            );
        }
    }
    #[test]
    fn punctuation_and_keypad_use_physical_positions() {
        assert_eq!(usb_usage(KeyCode::Quote), Some(0x34));
        assert_eq!(usb_usage(KeyCode::Backquote), Some(0x35));
        assert_eq!(usb_usage(KeyCode::NumpadEnter), Some(0x58));
        assert_ne!(usb_usage(KeyCode::NumpadEnter), usb_usage(KeyCode::Enter));
    }
}
