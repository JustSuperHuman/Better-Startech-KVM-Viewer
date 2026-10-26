//! Encoders for the bridge's byte-level input contract, not driver IOCTLs.

pub const KEY_PACKET_LEN: usize = 5;
pub const MOUSE_PACKET_LEN: usize = 9;

pub const fn encode_key(
    usb_key: u8,
    modifiers: u8,
    down: bool,
    all_up: bool,
) -> [u8; KEY_PACKET_LEN] {
    [b'k', usb_key, modifiers, down as u8, all_up as u8]
}

pub const fn encode_mouse(
    abs: bool,
    buttons: u8,
    x: i16,
    y: i16,
    dz: i16,
) -> [u8; MOUSE_PACKET_LEN] {
    let x = x.to_be_bytes();
    let y = y.to_be_bytes();
    let dz = dz.to_be_bytes();
    [
        b'm', abs as u8, buttons, x[0], x[1], y[0], y[1], dz[0], dz[1],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_layout_and_boolean_bytes() {
        for down in [false, true] {
            for all_up in [false, true] {
                assert_eq!(
                    encode_key(0xe7, 0x82, down, all_up),
                    [b'k', 0xe7, 0x82, u8::from(down), u8::from(all_up)]
                );
            }
        }
    }

    #[test]
    fn mouse_fields_are_signed_big_endian() {
        assert_eq!(
            encode_mouse(true, 0x05, 0x1234, -2, i16::MIN),
            [b'm', 1, 5, 0x12, 0x34, 0xff, 0xfe, 0x80, 0]
        );
        assert_eq!(
            encode_mouse(false, 0xff, i16::MIN, i16::MAX, 0),
            [b'm', 0, 0xff, 0x80, 0, 0x7f, 0xff, 0, 0]
        );
    }

    #[test]
    fn every_coordinate_boundary_round_trips() {
        for value in [i16::MIN, -256, -1, 0, 1, 255, 256, i16::MAX] {
            let packet = encode_mouse(false, 0, value, value, value);
            for offset in [3, 5, 7] {
                assert_eq!(
                    i16::from_be_bytes([packet[offset], packet[offset + 1]]),
                    value
                );
            }
        }
    }
}
