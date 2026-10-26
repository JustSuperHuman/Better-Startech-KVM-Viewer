#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

use anyhow::{Context, Result, bail, ensure};
use cart_installer::{install, system, ui};
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let value = |name: &str| {
        args.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| PathBuf::from(&pair[1]))
    };
    for arg in &args {
        if arg.starts_with("--")
            && ![
                "--uninstall",
                "--silent",
                "--accept-license",
                "--desktop",
                "--dir",
                "--list",
                "--extract",
                "--help",
            ]
            .contains(&arg.as_str())
        {
            bail!("Unknown option: {arg}. Use --help.");
        }
    }
    if flag("--help") {
        println!(
            "USB Crash Cart setup {}\n\n  (no options)       Install with the setup window\n  --uninstall        Remove the viewer\n  --silent           No window; install needs --accept-license\n  --accept-license   Accept the StarTech license for the adapter components\n  --desktop          Also add a desktop shortcut (silent install)\n  --dir PATH         Install location (default: Program Files)\n  --list             List the embedded files; changes nothing\n  --extract PATH     Copy the embedded files into a new folder; changes nothing else",
            install::VERSION
        );
        return Ok(());
    }
    if flag("--list") {
        for (name, bytes) in install::payload() {
            println!("{:>10}  {name}", bytes.len());
        }
        return Ok(());
    }
    if let Some(target) = value("--extract") {
        ensure!(!target.exists(), "Refusing to replace {}", target.display());
        for (name, bytes) in install::payload() {
            let path = target.join(name.replace('/', "\\"));
            std::fs::create_dir_all(path.parent().context("Invalid payload path")?)?;
            std::fs::write(&path, bytes)?;
        }
        println!(
            "Extracted {} files to {}",
            install::payload().len(),
            target.display()
        );
        return Ok(());
    }
    let remove = flag("--uninstall");
    let dir = match value("--dir") {
        Some(dir) => dir,
        None if remove => {
            let exe = std::env::current_exe()?;
            if exe
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("uninstall.exe"))
            {
                exe.parent()
                    .context("Uninstaller has no folder")?
                    .to_owned()
            } else {
                system::installed_dir().context("USB Crash Cart Viewer is not installed")?
            }
        }
        None => system::installed_dir().unwrap_or_else(install::default_dir),
    };
    ensure!(
        dir.is_absolute(),
        "The install location must be a full path"
    );
    if flag("--silent") {
        let mut report = |index: usize, state: install::State, detail: String| {
            let labels: &[&str] = if remove {
                &install::REMOVE_STEPS
            } else {
                &install::INSTALL_STEPS
            };
            println!(
                "{:?}: {} {detail}",
                state,
                labels.get(index).unwrap_or(&"?")
            );
        };
        if remove {
            if install::uninstall(&system::Windows, &dir, &mut report)? {
                system::delete_after_exit(&std::env::current_exe()?)?;
            }
        } else {
            ensure!(
                flag("--accept-license"),
                "A silent install needs --accept-license (StarTech license for the adapter components)"
            );
            let outcome = install::install(
                &system::Windows,
                &install::Options {
                    dir,
                    desktop: flag("--desktop"),
                },
                &mut report,
            )?;
            if outcome.restart_required {
                println!("Restart Windows to finish the driver installation.");
            }
        }
        return Ok(());
    }
    ui::run(
        if remove {
            ui::Mode::Remove
        } else {
            ui::Mode::Install
        },
        dir,
    )
}
