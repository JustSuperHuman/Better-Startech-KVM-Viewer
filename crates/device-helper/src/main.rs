mod python;
mod service;

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

fn main() -> Result<()> {
    // An installed viewer carries the vendor runtime beside the helper;
    // otherwise use the original application's installation.
    let mut vendor = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("runtime")))
        .filter(|runtime| runtime.join("python27.dll").is_file())
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)\USB Crash Cart Adapter"));
    let mut command = None;
    let mut output = None;
    let mut seconds = 5_u32;
    let mut initialize = false;
    let mut probes: Vec<(String, String, u32)> = Vec::new();
    let mut drive: Option<PathBuf> = None;
    let mut drive_kind = String::from("raw");
    let mut selftest: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--vendor-dir" => vendor = args.next().context("--vendor-dir requires a path")?.into(),
            "--runtime-info" => command = Some("runtime-info"),
            "--inspect-video-settings" => command = Some("inspect-video-settings"),
            "--symbol" => {
                // MODULE:Qualified.Name[@instruction], read from the static archive only.
                let spec = args
                    .next()
                    .context("--symbol requires MODULE:Qualified.Name")?;
                let (module, rest) = spec.split_once(':').context("--symbol needs MODULE:Name")?;
                let (symbol, start) = match rest.split_once('@') {
                    Some((symbol, start)) => {
                        (symbol, start.parse().context("Invalid @instruction")?)
                    }
                    None => (rest, 0),
                };
                let valid = |text: &str| {
                    text.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                };
                // Package members are named like usbcon/win32usb.
                let package =
                    |text: &str| text.split('/').all(|part| !part.is_empty() && valid(part));
                anyhow::ensure!(
                    package(module) && valid(symbol) && probes.len() < 32,
                    "Invalid static symbol selection"
                );
                probes.push((module.into(), symbol.into(), start));
            }
            "--outline" => {
                // Every code object in one archive module, read statically.
                let module = args.next().context("--outline requires a MODULE")?;
                anyhow::ensure!(
                    module.split('/').all(|part| {
                        !part.is_empty()
                            && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    }) && probes.len() < 32,
                    "Invalid static module selection"
                );
                probes.push((module, "*".into(), 0));
            }
            "--drive" => {
                drive = Some(PathBuf::from(
                    args.next().context("--drive requires a disk image path")?,
                ));
            }
            "--drive-kind" => {
                let kind = args
                    .next()
                    .context("--drive-kind requires raw, vhd or iso")?;
                anyhow::ensure!(
                    matches!(kind.as_str(), "raw" | "vhd" | "iso"),
                    "--drive-kind must be raw, vhd or iso"
                );
                drive_kind = kind;
            }
            "--drive-selftest" => {
                command = Some("drive-selftest");
                selftest = Some(PathBuf::from(
                    args.next()
                        .context("--drive-selftest requires a new directory")?,
                ));
            }
            "--reset-connection" => command = Some("reset-connection"),
            "--probe" => command = Some("probe"),
            "--serve" => command = Some("serve"),
            "--initialize" => initialize = true,
            "--output" => output = Some(args.next().context("--output requires a new frame path")?),
            "--seconds" => {
                seconds = args
                    .next()
                    .context("--seconds requires a number")?
                    .parse()?;
                anyhow::ensure!(
                    (1..=60).contains(&seconds),
                    "Probe duration must be 1–60 seconds"
                );
            }
            "--help" | "-h" => {
                help();
                return Ok(());
            }
            _ => bail!("Unknown argument: {arg}. Use --help."),
        }
    }
    let Some(command) = command else {
        help();
        return Ok(());
    };
    if let Some(ref path) = output {
        anyhow::ensure!(
            !PathBuf::from(path).exists(),
            "Refusing to overwrite frame output: {path}"
        );
    }
    anyhow::ensure!(
        drive.is_none() || command == "probe",
        "--drive is only available with --probe"
    );
    // Paths reach Python as hexadecimal UTF-8, never as source text.
    let hex_path = |path: &PathBuf| -> Result<String> {
        let path = std::path::absolute(path)?;
        let text = path.to_str().context("Path is not valid Unicode")?;
        Ok(text.bytes().map(|byte| format!("{byte:02x}")).collect())
    };
    let drive_literal = match &drive {
        Some(path) => format!("'{}'", hex_path(path)?),
        None => "None".into(),
    };
    let selftest_literal = match &selftest {
        Some(path) => {
            anyhow::ensure!(!path.exists(), "Refusing to reuse {}", path.display());
            format!("'{}'.decode('hex').decode('utf-8')", hex_path(path)?)
        }
        None => "None".into(),
    };
    // The helper runs from its own trusted directory, never from inspected packages.
    let executable = std::env::current_exe()?;
    std::env::set_current_dir(
        executable
            .parent()
            .context("Executable has no parent directory")?,
    )?;
    let position_x = std::env::var("CART_POSITION_X")
        .ok()
        .map(|value| value.parse::<i8>())
        .transpose()
        .context("CART_POSITION_X must be -128..127")?;
    let runtime = python::Python::load(&vendor)?;
    let output_literal = output
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?
        .unwrap_or_else(|| "None".into());
    runtime.run(&format!(
        "__cart_config = {{'command': {}, 'vendor_dir': {}, 'output': {}, 'seconds': {seconds}, 'initialize': {}, 'position_x': {}, 'probes': {}, 'drive': {drive_literal}, 'drive_kind': '{drive_kind}'}}",
        serde_json::to_string(command)?,
        serde_json::to_string(vendor.to_str().context("Invalid vendor path")?)?,
        output_literal,
        if initialize || command == "serve" { "True" } else { "False" },
        position_x.map(|value| value.to_string()).unwrap_or_else(|| "None".into()),
        serde_json::to_string(&probes)?,
    ))?;
    runtime.run(include_str!("bridge.py"))?;
    runtime.run(include_str!("drive.py"))?;
    runtime.run(include_str!("session.py"))?;
    match command {
        "runtime-info" => {
            runtime.run("runtime_info()")?;
            runtime.run(include_str!("inspect.py"))
        }
        "inspect-video-settings" => runtime.run(include_str!("video_settings.py")),
        "reset-connection" => runtime.run("reset_connection()"),
        "probe" => runtime.run("probe_session()"),
        "drive-selftest" => runtime.run(&format!("drive_selftest({selftest_literal})")),
        "serve" => service::serve(&runtime),
        _ => unreachable!(),
    }
}

fn help() {
    println!(
        "Crash Cart device helper (32-bit vendor compatibility bridge)\n\n\
        --runtime-info       Verify isolated runtime and list decoder/COM API metadata; no device I/O\n\
        --inspect-video-settings  Inspect original settings bytecode; no device I/O\n\
        --symbol MOD:Name[@N]  With --inspect-video-settings, show that code object instead\n\
        --probe              Inspect real video/status; never sends keyboard/mouse input\n\
        --initialize         Include verified volatile FPGA initialization in a probe\n\
        --serve              Run the viewer's private IPC service with normal initialization\n\
        --reset-connection   Request USB reconnect for the single verified adapter\n\
        --vendor-dir PATH    Existing vendor installation directory\n\
        --seconds 1..60      Bounded video probe duration (default: 5)\n\
        --output PATH        Save one decoded frame to a new local PNG file\n\
        --drive PATH         With --probe, attach this disk image to the target, then detach\n\
        --drive-kind KIND    raw (default), vhd or iso\n\
        --drive-selftest DIR Check every disk image format in a new directory; no device I/O\n\n\
        No persistent firmware update, driver installation, or COM registration operations are available."
    );
}
