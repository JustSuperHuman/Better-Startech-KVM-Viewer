//! US-layout paste planning. Plans contain HID strokes, never retained input text.
//!
//! A plan is returned only after the entire string has passed validation. The
//! caller must provide the target's Caps Lock state; it is not inferred here.

use std::fmt;

use crate::input::LEFT_SHIFT_MASK;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Stroke {
    pub usb_key: u8,
    pub modifiers: u8,
}

/// A zero-based Unicode scalar index and UTF-8 byte offset into the original input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PasteError {
    pub character_index: usize,
    pub byte_index: usize,
    pub character: char,
}

impl fmt::Display for PasteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unsupported character U+{:04X} at character {} (byte offset {})",
            u32::from(self.character),
            self.character_index + 1,
            self.byte_index
        )
    }
}

impl std::error::Error for PasteError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PastePlan {
    strokes: Vec<Stroke>,
}

impl PastePlan {
    /// Accepts the 95 printable ASCII characters, Tab, CR, and LF. A CRLF pair
    /// becomes one Enter; standalone CR/LF each become one Enter. No trailing
    /// Enter is added. Caps Lock changes letter Shift only, not punctuation.
    pub fn new(text: &str, caps_lock: bool) -> Result<Self, PasteError> {
        // Validate before allocating or exposing any stroke. In particular, a
        // valid prefix followed by Unicode cannot become a partial paste.
        for (character_index, (byte_index, character)) in text.char_indices().enumerate() {
            if !matches!(character, ' '..='~' | '\t' | '\r' | '\n') {
                return Err(PasteError {
                    character_index,
                    byte_index,
                    character,
                });
            }
        }

        let mut strokes = Vec::with_capacity(text.len());
        let mut bytes = text.bytes().peekable();
        while let Some(byte) = bytes.next() {
            if byte == b'\r' && bytes.peek() == Some(&b'\n') {
                bytes.next();
            }
            strokes.push(ascii_stroke(byte, caps_lock));
        }
        Ok(Self { strokes })
    }

    pub fn strokes(&self) -> &[Stroke] {
        &self.strokes
    }

    pub fn into_strokes(self) -> Vec<Stroke> {
        self.strokes
    }

    pub fn len(&self) -> usize {
        self.strokes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
    }
}

// Called only after validating printable ASCII and the three supported controls.
fn ascii_stroke(byte: u8, caps_lock: bool) -> Stroke {
    let (usb_key, shift) = match byte {
        b'a'..=b'z' => (0x04 + byte - b'a', caps_lock),
        b'A'..=b'Z' => (0x04 + byte - b'A', !caps_lock),
        b'1'..=b'9' => (0x1e + byte - b'1', false),
        b'0' => (0x27, false),
        b'!' => (0x1e, true),
        b'@' => (0x1f, true),
        b'#' => (0x20, true),
        b'$' => (0x21, true),
        b'%' => (0x22, true),
        b'^' => (0x23, true),
        b'&' => (0x24, true),
        b'*' => (0x25, true),
        b'(' => (0x26, true),
        b')' => (0x27, true),
        b'\r' | b'\n' => (0x28, false),
        b'\t' => (0x2b, false),
        b' ' => (0x2c, false),
        b'-' => (0x2d, false),
        b'_' => (0x2d, true),
        b'=' => (0x2e, false),
        b'+' => (0x2e, true),
        b'[' => (0x2f, false),
        b'{' => (0x2f, true),
        b']' => (0x30, false),
        b'}' => (0x30, true),
        b'\\' => (0x31, false),
        b'|' => (0x31, true),
        b';' => (0x33, false),
        b':' => (0x33, true),
        b'\'' => (0x34, false),
        b'"' => (0x34, true),
        b'`' => (0x35, false),
        b'~' => (0x35, true),
        b',' => (0x36, false),
        b'<' => (0x36, true),
        b'.' => (0x37, false),
        b'>' => (0x37, true),
        b'/' => (0x38, false),
        b'?' => (0x38, true),
        _ => unreachable!("paste input must be validated before encoding"),
    };
    Stroke {
        usb_key,
        modifiers: if shift { LEFT_SHIFT_MASK } else { 0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(character: char, caps_lock: bool) -> Stroke {
        PastePlan::new(&character.to_string(), caps_lock)
            .unwrap()
            .strokes()[0]
    }

    #[test]
    fn all_95_printable_ascii_characters_are_supported() {
        let text: String = (' '..='~').collect();
        for caps_lock in [false, true] {
            let plan = PastePlan::new(&text, caps_lock).unwrap();
            assert_eq!(plan.len(), 95);
            for stroke in plan.strokes() {
                assert!(crate::input::is_supported_usage(stroke.usb_key));
                assert!(matches!(stroke.modifiers, 0 | LEFT_SHIFT_MASK));
            }
        }
    }

    #[test]
    fn every_letter_uses_caps_lock_xor_requested_case() {
        for index in 0..26 {
            for caps_lock in [false, true] {
                let lower = stroke(char::from(b'a' + index), caps_lock);
                let upper = stroke(char::from(b'A' + index), caps_lock);
                assert_eq!(lower.usb_key, 0x04 + index);
                assert_eq!(upper.usb_key, lower.usb_key);
                assert_eq!(lower.modifiers, if caps_lock { 2 } else { 0 });
                assert_eq!(upper.modifiers, if caps_lock { 0 } else { 2 });
            }
        }
    }

    #[test]
    fn digits_and_shifted_number_row_are_complete() {
        let unshifted = "1234567890";
        let shifted = "!@#$%^&*()";
        for caps_lock in [false, true] {
            for (index, (plain, symbol)) in unshifted.chars().zip(shifted.chars()).enumerate() {
                assert_eq!(
                    stroke(plain, caps_lock),
                    Stroke {
                        usb_key: 0x1e + index as u8,
                        modifiers: 0
                    }
                );
                assert_eq!(
                    stroke(symbol, caps_lock),
                    Stroke {
                        usb_key: 0x1e + index as u8,
                        modifiers: 2
                    }
                );
            }
        }
    }

    #[test]
    fn every_other_punctuation_pair_uses_the_us_layout() {
        let pairs = [
            ('-', '_', 0x2d),
            ('=', '+', 0x2e),
            ('[', '{', 0x2f),
            (']', '}', 0x30),
            ('\\', '|', 0x31),
            (';', ':', 0x33),
            ('\'', '"', 0x34),
            ('`', '~', 0x35),
            (',', '<', 0x36),
            ('.', '>', 0x37),
            ('/', '?', 0x38),
        ];
        for caps_lock in [false, true] {
            for (plain, shifted, usb_key) in pairs {
                assert_eq!(
                    stroke(plain, caps_lock),
                    Stroke {
                        usb_key,
                        modifiers: 0
                    }
                );
                assert_eq!(
                    stroke(shifted, caps_lock),
                    Stroke {
                        usb_key,
                        modifiers: 2
                    }
                );
            }
            assert_eq!(
                stroke(' ', caps_lock),
                Stroke {
                    usb_key: 0x2c,
                    modifiers: 0
                }
            );
        }
    }

    #[test]
    fn tabs_and_mixed_newlines_preserve_logical_line_breaks() {
        let plan = PastePlan::new("a\r\nb\rc\n\r\n\n\r\t", false).unwrap();
        let keys: Vec<_> = plan.strokes().iter().map(|stroke| stroke.usb_key).collect();
        assert_eq!(keys, [4, 0x28, 5, 0x28, 6, 0x28, 0x28, 0x28, 0x28, 0x2b]);
        assert!(plan.strokes().iter().all(|stroke| stroke.modifiers == 0));
        assert_eq!(PastePlan::new("\r\r\n", false).unwrap().len(), 2);
        assert_eq!(PastePlan::new("\n\r", false).unwrap().len(), 2);
    }

    #[test]
    fn empty_input_and_no_appended_enter() {
        assert!(PastePlan::new("", false).unwrap().is_empty());
        assert_eq!(PastePlan::new("abc", false).unwrap().len(), 3);
        assert_eq!(PastePlan::new("abc\n", false).unwrap().len(), 4);
        assert_eq!(stroke('\t', true).usb_key, 0x2b);
        assert_eq!(stroke('\r', true).usb_key, 0x28);
        assert_eq!(stroke('\n', true).usb_key, 0x28);
    }

    #[test]
    fn every_other_ascii_control_is_rejected() {
        for byte in (0..=0x1f).chain([0x7f]) {
            if matches!(byte, b'\t' | b'\r' | b'\n') {
                continue;
            }
            let character = char::from(byte);
            let error = PastePlan::new(&format!("abc{character}def"), false).unwrap_err();
            assert_eq!(
                error,
                PasteError {
                    character_index: 3,
                    byte_index: 3,
                    character
                }
            );
        }
    }

    #[test]
    fn unicode_is_rejected_even_after_a_valid_prefix() {
        for character in ['é', '中', '\u{301}', '\u{a0}', '\u{1f642}'] {
            let text = format!("valid\r\nprefix{character}");
            let error = PastePlan::new(&text, false).unwrap_err();
            assert_eq!(error.character_index, 13);
            assert_eq!(error.byte_index, 13);
            assert_eq!(error.character, character);
        }
    }

    #[test]
    fn error_position_refers_to_original_text_before_crlf_normalization() {
        let error = PastePlan::new("a\r\n€", false).unwrap_err();
        assert_eq!(error.character_index, 3);
        assert_eq!(error.byte_index, 3);
        assert_eq!(
            error.to_string(),
            "unsupported character U+20AC at character 4 (byte offset 3)"
        );
    }
}
