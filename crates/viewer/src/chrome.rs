use anyhow::{Context, Result, ensure};
use std::io::Cursor;
use winit::window::{Icon, Theme, WindowAttributes};

const SMALL_ICON: &[u8] = include_bytes!("../../../assets/icons/32/cart.png");
const LARGE_ICON: &[u8] = include_bytes!("../../../assets/icons/cart.png");

pub fn attributes(
    attributes: WindowAttributes,
    background: [u8; 3],
    foreground: [u8; 3],
) -> Result<WindowAttributes> {
    let attributes = attributes
        .with_theme(Some(Theme::Dark))
        .with_window_icon(Some(icon(SMALL_ICON)?));
    #[cfg(windows)]
    let attributes = {
        use winit::platform::windows::{Color, WindowAttributesExtWindows};
        let background = Color::from_rgb(background[0], background[1], background[2]);
        let foreground = Color::from_rgb(foreground[0], foreground[1], foreground[2]);
        attributes
            .with_title_background_color(Some(background))
            .with_title_text_color(foreground)
            .with_border_color(Some(background))
            .with_taskbar_icon(Some(icon(LARGE_ICON)?))
    };
    #[cfg(not(windows))]
    let _ = (background, foreground);
    Ok(attributes)
}

fn rgba(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let mut reader = png::Decoder::new(Cursor::new(bytes))
        .read_info()
        .context("Read embedded viewer icon")?;
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut pixels)
        .context("Decode viewer icon")?;
    ensure!(
        info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight,
        "Viewer icon must use 8-bit RGBA"
    );
    pixels.truncate(info.buffer_size());
    Ok((pixels, info.width, info.height))
}

fn icon(bytes: &[u8]) -> Result<Icon> {
    let (pixels, width, height) = rgba(bytes)?;
    Icon::from_rgba(pixels, width, height).context("Create native viewer icon")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_and_taskbar_icons_have_transparent_edges_and_correct_sizes() {
        for (bytes, expected) in [(SMALL_ICON, 32), (LARGE_ICON, 256)] {
            let (pixels, width, height) = rgba(bytes).unwrap();
            assert_eq!((width, height), (expected, expected));
            assert_eq!(pixels.len(), (width * height * 4) as usize);
            assert_eq!(pixels[3], 0);
            assert!(
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] == 255)
            );
        }
    }
}
