#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

mod app;
mod chrome;
mod demo;
mod drive;
mod graphics;
mod keyboard;
mod navigation;
mod settings;
mod transport;

#[derive(Clone, Copy, Debug)]
pub enum Wake {
    Helper,
}

fn main() -> anyhow::Result<()> {
    let mut options = app::Options::default();
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--demo") => options.demo = true,
            Some("--smoke-test") => options.smoke_test = true,
            Some("--smoke-seconds") => {
                let value = arguments.next().ok_or_else(|| {
                    anyhow::anyhow!("--smoke-seconds requires a duration in seconds")
                })?;
                options.smoke_seconds = Some(parse_smoke_seconds(&value)?);
            }
            Some("--connect") => options.connect = true,
            Some("--show-usb-drive") => options.show_drive = true,
            Some("--smoke-attach") => {
                options.smoke_attach = Some(
                    arguments
                        .next()
                        .ok_or_else(|| {
                            anyhow::anyhow!("--smoke-attach requires a disk image path")
                        })?
                        .into(),
                );
            }
            Some("--screenshot") => {
                options.screenshot = Some(
                    arguments
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--screenshot requires a PNG output path"))?
                        .into(),
                );
            }
            Some("--helper") => {
                options.helper = Some(
                    arguments
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--helper requires an executable path"))?
                        .into(),
                )
            }
            Some("--help" | "-h") => {
                println!(
                    "USB Crash Cart viewer\n\n  --demo          Show a labeled test image; never starts the helper\n  --smoke-test    Run GPU/video smoke test; target input disabled\n  --smoke-seconds N  Set smoke duration to 1..600 seconds\n  --connect       Connect at startup with automatic input capture\n  --helper PATH   Override the private x86 helper executable\n  --screenshot P  Save our GPU-rendered UI as PNG during --smoke-test\n  --show-usb-drive  Open the USB drive panel during --smoke-test\n  --smoke-attach IMG  Plug a disk image into the target during --smoke-test --connect\n\nDrop files on the window to send them to the target on a virtual USB drive.\n\nWithout --connect, the helper starts only when Connect adapter is clicked.\nControl mode captures automatically over focused video. F12 switches Control/Navigate.\nCtrl+scroll zooms and dragging pans in Navigate. Remote F12 is in More > Special keys.\nCART_SMOKE_SECONDS sets the smoke duration when --smoke-seconds is not supplied."
                );
                return Ok(());
            }
            _ => anyhow::bail!("Unknown option: {}", argument.to_string_lossy()),
        }
    }
    if options.demo && options.connect {
        anyhow::bail!("--demo and --connect cannot be combined");
    }
    if options.screenshot.is_some() && !options.smoke_test {
        anyhow::bail!("--screenshot requires --smoke-test");
    }
    if options.smoke_attach.is_some() && !(options.smoke_test && options.connect) {
        anyhow::bail!("--smoke-attach requires --smoke-test and --connect");
    }
    if options.show_drive && !options.smoke_test {
        anyhow::bail!("--show-usb-drive requires --smoke-test");
    }
    if options.smoke_seconds.is_some() && !options.smoke_test {
        anyhow::bail!("--smoke-seconds requires --smoke-test");
    }
    if options.smoke_test
        && options.smoke_seconds.is_none()
        && let Some(value) = std::env::var_os("CART_SMOKE_SECONDS")
    {
        options.smoke_seconds = Some(parse_smoke_seconds(&value)?);
    }
    let event_loop = winit::event_loop::EventLoop::<Wake>::with_user_event().build()?;
    let mut app = app::App::new(options, event_loop.create_proxy());
    event_loop.run_app(&mut app)?;
    if app.failed {
        anyhow::bail!("Viewer failed; see the diagnostic above");
    }
    Ok(())
}

fn parse_smoke_seconds(value: &std::ffi::OsStr) -> anyhow::Result<u64> {
    value
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (1..=600).contains(seconds))
        .ok_or_else(|| anyhow::anyhow!("Smoke duration must be an integer from 1 to 600 seconds"))
}

#[cfg(test)]
mod tests {
    use super::parse_smoke_seconds;
    use std::ffi::OsStr;

    #[test]
    fn smoke_duration_accepts_hardware_startup_budget_and_rejects_invalid_values() {
        for (value, expected) in [("1", 1), ("2", 2), ("15", 15), ("20", 20), ("600", 600)] {
            assert_eq!(parse_smoke_seconds(OsStr::new(value)).unwrap(), expected);
        }
        for value in ["0", "-1", "601", "1.5", "", "abc"] {
            assert!(parse_smoke_seconds(OsStr::new(value)).is_err());
        }
    }
}
