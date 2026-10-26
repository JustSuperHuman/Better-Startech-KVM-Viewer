//! The real Windows side of setup: hidden processes, WinSxS, shortcuts and
//! the Apps & features registry entry.
use crate::install::{PRODUCT, System, UninstallEntry, VERSION};
use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsString,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
};
use winreg::{RegKey, enums::HKEY_LOCAL_MACHINE};

const UNINSTALL_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\USBCrashCartViewer";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct Windows;

fn windows_dir() -> PathBuf {
    std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
}

pub fn installed_dir() -> Option<PathBuf> {
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(UNINSTALL_KEY)
        .ok()?;
    let dir: String = key.get_value("InstallLocation").ok()?;
    Some(PathBuf::from(dir)).filter(|dir| dir.is_absolute())
}

impl System for Windows {
    fn run(&self, program: &Path, args: &[OsString]) -> Result<(i32, String)> {
        let output = Command::new(program)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .with_context(|| format!("Start {}", program.display()))?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        // Keep reports short; pnputil can be verbose.
        let text: String = text.chars().take(600).collect();
        Ok((output.status.code().unwrap_or(-1), text))
    }

    /// python27.dll binds to the x86 VC90 CRT through WinSxS.
    fn vc_runtime_present(&self) -> bool {
        std::fs::read_dir(windows_dir().join("WinSxS")).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                name.starts_with("x86_microsoft.vc90.crt_1fc8b3b9a1e18e3b_9.0.")
                    && entry.path().join("msvcr90.dll").is_file()
            })
        })
    }

    fn temp_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }

    fn system32(&self) -> PathBuf {
        windows_dir().join("System32")
    }

    fn syswow64(&self) -> PathBuf {
        windows_dir().join("SysWOW64")
    }

    fn shortcut_dirs(&self, desktop: bool) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(data) = std::env::var_os("ProgramData") {
            dirs.push(PathBuf::from(data).join(r"Microsoft\Windows\Start Menu\Programs"));
        }
        if desktop && let Some(public) = std::env::var_os("PUBLIC") {
            dirs.push(PathBuf::from(public).join("Desktop"));
        }
        dirs
    }

    /// Paths travel as environment variables, never inside the script text.
    fn shortcut(&self, link: &Path, target: &Path, arguments: &str) -> Result<()> {
        let script = "$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:CART_LINK); \
            $s.TargetPath = $env:CART_TARGET; $s.Arguments = $env:CART_ARGS; \
            $s.WorkingDirectory = $env:CART_DIR; $s.IconLocation = $env:CART_TARGET + ',0'; \
            $s.Description = 'See and control a computer through the USB Crash Cart adapter'; $s.Save()";
        let status = Command::new(
            self.system32()
                .join(r"WindowsPowerShell\v1.0\powershell.exe"),
        )
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .env("CART_LINK", link)
        .env("CART_TARGET", target)
        .env("CART_ARGS", arguments)
        .env(
            "CART_DIR",
            target.parent().context("Shortcut target has no folder")?,
        )
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .context("Start PowerShell to create a shortcut")?;
        ensure!(
            status.success() && link.is_file(),
            "Could not create {}",
            link.display()
        );
        Ok(())
    }

    fn register_uninstall(&self, entry: &UninstallEntry) -> Result<()> {
        let (key, _) = RegKey::predef(HKEY_LOCAL_MACHINE)
            .create_subkey(UNINSTALL_KEY)
            .context("Create the Apps & features entry")?;
        let dir = entry.dir.display().to_string();
        let uninstaller = entry.dir.join("uninstall.exe").display().to_string();
        key.set_value("DisplayName", &PRODUCT)?;
        key.set_value("DisplayVersion", &VERSION)?;
        key.set_value("DisplayIcon", &format!("{dir}\\cart-viewer.exe,0"))?;
        key.set_value("InstallLocation", &dir)?;
        key.set_value("UninstallString", &format!("\"{uninstaller}\" --uninstall"))?;
        key.set_value(
            "QuietUninstallString",
            &format!("\"{uninstaller}\" --uninstall --silent"),
        )?;
        key.set_value("EstimatedSize", &entry.size_kb)?;
        key.set_value("NoModify", &1_u32)?;
        key.set_value("NoRepair", &1_u32)?;
        Ok(())
    }

    fn remove_uninstall(&self) -> Result<()> {
        match RegKey::predef(HKEY_LOCAL_MACHINE).delete_subkey_all(UNINSTALL_KEY) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(error).context("Remove the Apps & features entry")
            }
            _ => Ok(()),
        }
    }

    fn current_exe(&self) -> Result<PathBuf> {
        Ok(std::env::current_exe()?)
    }
}

/// A running uninstaller cannot delete itself: a hidden shell removes it and
/// the then-empty folder shortly after this process exits.
pub fn delete_after_exit(exe: &Path) -> Result<()> {
    let dir = exe.parent().context("Uninstaller has no folder")?;
    let unsafe_chars = ['"', '%', '^', '&', '|', '<', '>', '!'];
    let text = exe.display().to_string();
    ensure!(
        !text.contains(unsafe_chars),
        "Unexpected characters in the install path"
    );
    Command::new(windows_dir().join(r"System32\cmd.exe"))
        .raw_arg(format!(
            "/D /C ping -n 3 127.0.0.1 >NUL & del /F /Q \"{}\" & rmdir \"{}\"",
            exe.display(),
            dir.display()
        ))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("Schedule removal of the uninstaller")?;
    Ok(())
}
