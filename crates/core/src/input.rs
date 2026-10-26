//! Held-key bookkeeping. Modifier masks come from HID usages 0xe0 through 0xe7.
//!
//! These transitions describe the bridge contract, not observed hardware behavior.

use std::collections::BTreeSet;
use std::fmt;

use crate::protocol;

pub const LEFT_SHIFT_MASK: u8 = 0x02;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KeyEvent {
    pub usb_key: u8,
    pub modifiers: u8,
    pub down: bool,
    pub all_up: bool,
}

impl KeyEvent {
    pub const fn encode(self) -> [u8; protocol::KEY_PACKET_LEN] {
        protocol::encode_key(self.usb_key, self.modifiers, self.down, self.all_up)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidKeyUsage(pub u8);

impl fmt::Display for InvalidKeyUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported keyboard HID usage 0x{:02x}", self.0)
    }
}

impl std::error::Error for InvalidKeyUsage {}

/// Supported non-error Keyboard/Keypad page usages, including all eight modifiers.
/// Reserved usages and the no-event/error values 0x00..=0x03 are rejected.
pub const fn is_supported_usage(usb_key: u8) -> bool {
    matches!(usb_key, 0x04..=0xa4 | 0xe0..=0xe7)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InputState {
    held: BTreeSet<u8>,
}

impl InputState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn held_keys(&self) -> &BTreeSet<u8> {
        &self.held
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    pub fn modifiers(&self) -> u8 {
        self.held
            .range(0xe0..=0xe7)
            .fold(0, |mask, &key| mask | (1 << (key - 0xe0)))
    }

    /// Returns no event for repeated downs or releases of keys that are not held.
    /// The mask and `all_up` describe the state *after* this transition.
    pub fn transition(
        &mut self,
        usb_key: u8,
        down: bool,
    ) -> Result<Option<KeyEvent>, InvalidKeyUsage> {
        if !is_supported_usage(usb_key) {
            return Err(InvalidKeyUsage(usb_key));
        }
        let changed = if down {
            self.held.insert(usb_key)
        } else {
            self.held.remove(&usb_key)
        };
        Ok(changed.then(|| self.event(usb_key, down)))
    }

    pub fn key_down(&mut self, usb_key: u8) -> Result<Option<KeyEvent>, InvalidKeyUsage> {
        self.transition(usb_key, true)
    }

    pub fn key_up(&mut self, usb_key: u8) -> Result<Option<KeyEvent>, InvalidKeyUsage> {
        self.transition(usb_key, false)
    }

    /// Releases every held key once in ascending usage order. Only the final
    /// event has `all_up = true`; ordinary keys are released before modifiers.
    /// The caller remains responsible for delivering the returned events.
    pub fn release_all(&mut self) -> Vec<KeyEvent> {
        let mut events = Vec::with_capacity(self.held.len());
        while let Some(usb_key) = self.held.pop_first() {
            events.push(self.event(usb_key, false));
        }
        events
    }

    fn event(&self, usb_key: u8, down: bool) -> KeyEvent {
        KeyEvent {
            usb_key,
            modifiers: self.modifiers(),
            down,
            all_up: self.is_empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down(state: &mut InputState, key: u8) -> KeyEvent {
        state.key_down(key).unwrap().unwrap()
    }

    fn up(state: &mut InputState, key: u8) -> KeyEvent {
        state.key_up(key).unwrap().unwrap()
    }

    #[test]
    fn ordinary_transitions_and_repeat_deduplication() {
        let mut state = InputState::new();
        assert_eq!(up_missing(&mut state, 4), None);
        assert_eq!(down(&mut state, 4).encode(), [b'k', 4, 0, 1, 0]);
        assert_eq!(state.key_down(4).unwrap(), None);
        assert_eq!(state.held_keys().len(), 1);
        assert_eq!(up(&mut state, 4).encode(), [b'k', 4, 0, 0, 1]);
        assert_eq!(state.key_up(4).unwrap(), None);
    }

    fn up_missing(state: &mut InputState, key: u8) -> Option<KeyEvent> {
        state.key_up(key).unwrap()
    }

    #[test]
    fn all_up_waits_for_every_key_including_modifiers() {
        let mut state = InputState::new();
        assert_eq!(down(&mut state, 0xe1).modifiers, LEFT_SHIFT_MASK);
        assert_eq!(down(&mut state, 4).modifiers, LEFT_SHIFT_MASK);
        assert!(!up(&mut state, 4).all_up);
        assert_eq!(state.modifiers(), LEFT_SHIFT_MASK);
        let final_event = up(&mut state, 0xe1);
        assert!(final_event.all_up);
        assert_eq!(final_event.modifiers, 0);
    }

    #[test]
    fn releasing_a_modifier_does_not_release_other_keys() {
        let mut state = InputState::new();
        down(&mut state, 0xe0);
        down(&mut state, 4);
        let event = up(&mut state, 0xe0);
        assert_eq!(event.modifiers, 0);
        assert!(!event.all_up);
        assert_eq!(state.held_keys().iter().copied().collect::<Vec<_>>(), [4]);
    }

    #[test]
    fn all_modifier_bits_are_independent() {
        let mut state = InputState::new();
        for key in 0xe0..=0xe7 {
            let event = down(&mut state, key);
            assert_eq!(event.modifiers, ((1u16 << (key - 0xe0 + 1)) - 1) as u8);
        }
        assert_eq!(state.modifiers(), 0xff);
        for key in 0xe0..=0xe7 {
            let event = up(&mut state, key);
            let removed = ((1u16 << (key - 0xe0 + 1)) - 1) as u8;
            assert_eq!(event.modifiers, !removed);
            assert_eq!(event.all_up, key == 0xe7);
        }
    }

    #[test]
    fn release_all_is_complete_ordered_and_idempotent() {
        let mut state = InputState::new();
        for key in [0xe5, 9, 0xe1, 4] {
            down(&mut state, key);
        }
        let events = state.release_all();
        assert_eq!(
            events.iter().map(|event| event.usb_key).collect::<Vec<_>>(),
            [4, 9, 0xe1, 0xe5]
        );
        assert!(events.iter().all(|event| !event.down));
        assert_eq!(
            events
                .iter()
                .map(|event| event.modifiers)
                .collect::<Vec<_>>(),
            [0x22, 0x22, 0x20, 0]
        );
        assert_eq!(
            events.iter().map(|event| event.all_up).collect::<Vec<_>>(),
            [false, false, false, true]
        );
        assert!(state.is_empty());
        assert!(state.release_all().is_empty());
    }

    #[test]
    fn unsupported_usages_do_not_mutate_state() {
        let mut state = InputState::new();
        down(&mut state, 4);
        let before = state.clone();
        for key in [0, 1, 2, 3, 0xa5, 0xdf, 0xe8, 0xff] {
            assert_eq!(state.key_down(key), Err(InvalidKeyUsage(key)));
            assert_eq!(state.key_up(key), Err(InvalidKeyUsage(key)));
            assert_eq!(state, before);
        }
    }

    #[test]
    fn every_supported_usage_fits_and_is_released_once() {
        let mut state = InputState::new();
        let keys: Vec<_> = (u8::MIN..=u8::MAX)
            .filter(|&key| is_supported_usage(key))
            .collect();
        assert_eq!(keys.len(), 169);
        for &key in &keys {
            down(&mut state, key);
        }
        let events = state.release_all();
        assert_eq!(
            events.iter().map(|event| event.usb_key).collect::<Vec<_>>(),
            keys
        );
        assert_eq!(events.iter().filter(|event| event.all_up).count(), 1);
    }
}
