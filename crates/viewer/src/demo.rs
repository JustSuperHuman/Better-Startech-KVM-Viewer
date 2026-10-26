use crate::transport::Frame;

pub fn frame() -> Frame {
    let mut frame = Frame {
        width: 1920,
        height: 1080,
        rgba: vec![0; 1920 * 1080 * 4],
    };
    rectangle(&mut frame, 0, 0, 1920, 1080, [15, 20, 29, 255]);
    text(&mut frame, 48, 48, 3, "DISPLAY CHECK", [227, 233, 242, 255]);
    text(
        &mut frame,
        48,
        86,
        2,
        "SYNTHETIC VIDEO / NO DEVICE CONNECTED",
        [147, 161, 181, 255],
    );
    for (left, background, foreground) in [
        (48, [4, 7, 11, 255], [231, 236, 242, 255]),
        (1000, [238, 240, 243, 255], [15, 20, 27, 255]),
    ] {
        rectangle(&mut frame, left, 156, 872, 570, background);
        for (row, line) in [
            "CONSOLE TEXT / NATIVE SOURCE PIXELS",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789",
            "THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG",
            "PATH: /USR/LOCAL/BIN  [READY]  VALUE=1024",
            "THIN STROKES SHOULD STAY STABLE WHILE ZOOMING",
            "CTRL + SCROLL TO ZOOM / DRAG TO PAN",
        ]
        .iter()
        .enumerate()
        {
            text(
                &mut frame,
                left + 24,
                184 + row as u32 * 34,
                2,
                line,
                foreground,
            );
        }
        text(
            &mut frame,
            left + 24,
            418,
            1,
            "SMALL TEXT / 7 PIXEL GLYPHS / ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789",
            foreground,
        );
        text(
            &mut frame,
            left + 24,
            452,
            3,
            "1:1 FOR FINE DETAIL",
            foreground,
        );
        for x in 0..800 {
            for y in 0..48 {
                let color = if (x + y) % 2 == 0 {
                    foreground
                } else {
                    background
                };
                pixel(&mut frame, left + 24 + x, 540 + y, color);
            }
        }
        for x in (0..800).step_by(4) {
            rectangle(&mut frame, left + 24 + x, 626, 1, 42, foreground);
        }
    }
    text(
        &mut frame,
        48,
        804,
        2,
        "FILTERING CHECK: ONE-PIXEL LINES, CHECKERS AND BOTH TEXT POLARITIES",
        [191, 205, 224, 255],
    );
    for x in 0..1824 {
        let value = (x * 255 / 1823) as u8;
        rectangle(&mut frame, 48 + x, 860, 1, 60, [value, value, value, 255]);
    }
    text(
        &mut frame,
        48,
        968,
        2,
        "FIT SHOWS THE COMPLETE FRAME. REDUCING GLYPHS TOO FAR LOSES READABLE DETAIL.",
        [147, 161, 181, 255],
    );
    for (x, y, color) in [
        (0, 0, [255, 80, 80, 255]),
        (1912, 0, [80, 230, 120, 255]),
        (0, 1072, [80, 160, 255, 255]),
        (1912, 1072, [255, 230, 80, 255]),
    ] {
        rectangle(&mut frame, x, y, 8, 8, color);
    }
    frame
}

fn pixel(frame: &mut Frame, x: u32, y: u32, color: [u8; 4]) {
    if x < frame.width && y < frame.height {
        let index = ((y * frame.width + x) * 4) as usize;
        frame.rgba[index..index + 4].copy_from_slice(&color);
    }
}

fn rectangle(frame: &mut Frame, x: u32, y: u32, width: u32, height: u32, color: [u8; 4]) {
    for row in y..(y + height).min(frame.height) {
        for column in x..(x + width).min(frame.width) {
            pixel(frame, column, row, color);
        }
    }
}

fn text(frame: &mut Frame, x: u32, y: u32, scale: u32, text: &str, color: [u8; 4]) {
    for (index, character) in text.chars().enumerate() {
        for (row, bits) in glyph(character).into_iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) != 0 {
                    rectangle(
                        frame,
                        x + index as u32 * 6 * scale + column * scale,
                        y + row as u32 * scale,
                        scale,
                        scale,
                        color,
                    );
                }
            }
        }
    }
}

fn glyph(character: char) -> [u8; 7] {
    match character {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 18, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        ':' => [0, 4, 4, 0, 4, 4, 0],
        '.' => [0, 0, 0, 0, 0, 4, 4],
        '/' => [1, 1, 2, 4, 8, 16, 16],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '=' => [0, 0, 31, 0, 31, 0, 0],
        '[' => [14, 8, 8, 8, 8, 8, 14],
        ']' => [14, 2, 2, 2, 2, 2, 14],
        _ => [0; 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_contains_text_in_source_pixels_and_all_four_edges() {
        let frame = frame();
        assert_eq!(frame.rgba.len(), 1920 * 1080 * 4);
        let at = |x, y| &frame.rgba[((y * frame.width + x) * 4) as usize..][..4];
        assert_eq!(at(0, 0), [255, 80, 80, 255]);
        assert_eq!(at(1919, 1079), [255, 230, 80, 255]);
        assert_ne!(at(74, 184), at(72, 184));
        assert!(
            frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 255)
        );
    }
}
