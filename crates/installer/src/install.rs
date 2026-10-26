//! Install and remove the viewer. Every system change is made through
//! `System`, so the whole sequence can be exercised in a temporary folder.
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::OsString,
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
};

mod payload {
    include!(concat!(env!("OUT_DIR"), "/payload.rs"));
}

pub const PRODUCT: &str = "USB Crash Cart Viewer";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const VCREDIST: &str = "vcredist/vcredist_x86.exe";
const DRIVER_INF: &str = "driver/ulcvm.inf";
const COM_SERVER: &str = "runtime/usbiocom.dll";
const VIEWER: &str = "cart-viewer.exe";
const UNINSTALLER: &str = "uninstall.exe";
const LINK_NAME: &str = "USB Crash Cart.lnk";
/// The original application registers the same COM server from here.
pub const VENDOR_COM_SERVER: &str = r"C:\Program Files (x86)\USB Crash Cart Adapter\usbiocom.dll";

pub fn payload() -> &'static [(&'static str, &'static [u8])] {
    payload::FILES
}

pub fn license_text() -> Option<&'static str> {
    payload()
        .iter()
        .find(|(name, _)| *name == "STARTECH-LICENSE.txt")
        .and_then(|(_, bytes)| std::str::from_utf8(bytes).ok())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Pending,
    Running,
    Done,
    Skipped,
    Failed,
}

pub const INSTALL_STEPS: [&str; 7] = [
    "Check that the viewer is closed",
    "Visual C++ 2008 runtime",
    "Viewer and adapter runtime",
    "Adapter driver",
    "Adapter transport",
    "Start menu shortcuts",
    "Apps & features entry",
];

pub const REMOVE_STEPS: [&str; 5] = [
    "Check that the viewer is closed",
    "Start menu shortcuts",
    "Adapter transport",
    "Apps & features entry",
    "Viewer and adapter runtime",
];

pub struct Options {
    pub dir: PathBuf,
    pub desktop: bool,
}

pub fn default_dir() -> PathBuf {
    std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .map_or_else(|| PathBuf::from(r"C:\Program Files"), PathBuf::from)
        .join(PRODUCT)
}

/// What `System::finish` reports back to the person installing.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub restart_required: bool,
    pub runtime_installed: bool,
}

pub struct UninstallEntry {
    pub dir: PathBuf,
    pub size_kb: u32,
}

pub trait System {
    /// Exit code of a hidden-window process.
    fn run(&self, program: &Path, args: &[OsString]) -> Result<(i32, String)>;
    fn vc_runtime_present(&self) -> bool;
    fn temp_dir(&self) -> PathBuf;
    fn system32(&self) -> PathBuf;
    fn syswow64(&self) -> PathBuf;
    fn shortcut_dirs(&self, desktop: bool) -> Vec<PathBuf>;
    fn shortcut(&self, link: &Path, target: &Path, arguments: &str) -> Result<()>;
    fn register_uninstall(&self, entry: &UninstallEntry) -> Result<()>;
    fn remove_uninstall(&self) -> Result<()>;
    fn current_exe(&self) -> Result<PathBuf>;
}

type Report<'a> = &'a mut dyn FnMut(usize, State, String);

fn step<T>(report: Report<'_>, index: usize, action: impl FnOnce() -> Result<T>) -> Result<T> {
    report(index, State::Running, String::new());
    match action() {
        Ok(value) => {
            report(index, State::Done, String::new());
            Ok(value)
        }
        Err(error) => {
            report(index, State::Failed, format!("{error:#}"));
            Err(error)
        }
    }
}

/// A payload name maps to a relative path with ordinary components only.
fn relative(name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(name.replace('/', "\\"));
    ensure!(
        !name.is_empty()
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "Invalid payload path: {name}"
    );
    Ok(path)
}

/// Windows refuses write access to a running program's image.
pub fn ensure_closed(dir: &Path) -> Result<()> {
    for name in [VIEWER, "cart-device-helper.exe", "runtime\\python27.dll"] {
        let path = dir.join(name);
        match fs::OpenOptions::new().append(true).open(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) if matches!(error.raw_os_error(), Some(5 | 32 | 33)) => {
                bail!("USB Crash Cart Viewer is open. Close it, then choose Try again.")
            }
            Err(error) => return Err(error).with_context(|| format!("Check {}", path.display())),
        }
    }
    Ok(())
}

pub fn write_files(dir: &Path, files: &[(&str, &[u8])]) -> Result<u64> {
    let mut total = 0;
    for (name, bytes) in files
        .iter()
        .filter(|(name, _)| !name.starts_with("vcredist/"))
    {
        let target = dir.join(relative(name)?);
        fs::create_dir_all(target.parent().context("Payload file has no folder")?)?;
        let staged = target.with_extension("setup-new");
        fs::write(&staged, bytes).with_context(|| format!("Write {}", target.display()))?;
        fs::rename(&staged, &target).with_context(|| format!("Replace {}", target.display()))?;
        total += bytes.len() as u64;
    }
    Ok(total)
}

fn exit_ok(label: &str, (code, output): (i32, String), accepted: &[i32]) -> Result<i32> {
    if accepted.contains(&code) {
        Ok(code)
    } else {
        let output = output.trim();
        bail!(
            "{label} failed with exit code {code}{}{output}",
            if output.is_empty() { "" } else { ": " }
        )
    }
}

pub fn install(system: &dyn System, options: &Options, report: Report<'_>) -> Result<Outcome> {
    let files = payload();
    ensure!(
        files.iter().any(|(name, _)| *name == VIEWER),
        "This setup was built without its files. Rebuild it with scripts\\package.py."
    );
    install_files(system, options, files, report)
}

pub fn install_files(
    system: &dyn System,
    options: &Options,
    files: &[(&str, &[u8])],
    report: Report<'_>,
) -> Result<Outcome> {
    let dir = &options.dir;
    let mut outcome = Outcome::default();
    step(report, 0, || ensure_closed(dir))?;

    report(1, State::Running, String::new());
    if system.vc_runtime_present() {
        report(1, State::Skipped, "Already installed".into());
    } else {
        let result = (|| {
            let (_, bytes) = files
                .iter()
                .find(|(name, _)| *name == VCREDIST)
                .context("The Visual C++ runtime installer is missing from this setup")?;
            let folder = system
                .temp_dir()
                .join(format!("cart-setup-{}", std::process::id()));
            fs::create_dir_all(&folder)?;
            let installer = folder.join("vcredist_x86.exe");
            fs::write(&installer, bytes)?;
            let result = system.run(&installer, &["/q".into(), "/norestart".into()]);
            let _ = fs::remove_dir_all(&folder);
            let code = exit_ok("Visual C++ runtime setup", result?, &[0, 1638, 3010])?;
            outcome.restart_required |= code == 3010;
            outcome.runtime_installed = true;
            ensure!(
                system.vc_runtime_present(),
                "The Visual C++ 2008 runtime is still missing after its setup finished"
            );
            Ok(())
        })();
        match result {
            Ok(()) => report(1, State::Done, String::new()),
            Err(error) => {
                report(1, State::Failed, format!("{error:#}"));
                return Err(error);
            }
        }
    }

    let size = step(report, 2, || {
        let size = write_files(dir, files)?;
        // The uninstaller is this setup program, run with --uninstall.
        let source = system.current_exe()?;
        let uninstaller = dir.join(UNINSTALLER);
        if source != uninstaller {
            fs::copy(&source, uninstaller.with_extension("setup-new"))?;
            fs::rename(uninstaller.with_extension("setup-new"), &uninstaller)?;
        }
        Ok(size)
    })?;

    step(report, 3, || {
        let code = exit_ok(
            "Driver installation",
            system.run(
                &system.system32().join("pnputil.exe"),
                &[
                    "/add-driver".into(),
                    dir.join(relative(DRIVER_INF)?).into_os_string(),
                    "/install".into(),
                ],
            )?,
            // 259: staged, with no connected adapter needing an update.
            &[0, 259, 3010],
        )?;
        outcome.restart_required |= code == 3010;
        Ok(())
    })?;

    step(report, 4, || {
        exit_ok(
            "Transport registration",
            system.run(
                &system.syswow64().join("regsvr32.exe"),
                &[
                    "/s".into(),
                    dir.join(relative(COM_SERVER)?).into_os_string(),
                ],
            )?,
            &[0],
        )
        .map(drop)
    })?;

    step(report, 5, || {
        for folder in system.shortcut_dirs(options.desktop) {
            fs::create_dir_all(&folder)?;
            system.shortcut(&folder.join(LINK_NAME), &dir.join(VIEWER), "--connect")?;
        }
        Ok(())
    })?;

    step(report, 6, || {
        system.register_uninstall(&UninstallEntry {
            dir: dir.clone(),
            size_kb: u32::try_from(size.div_ceil(1024)).unwrap_or(u32::MAX),
        })
    })?;
    Ok(outcome)
}

/// Removes everything setup created except the shared adapter driver and the
/// viewer's own picture-position settings.
pub fn uninstall(system: &dyn System, dir: &Path, report: Report<'_>) -> Result<bool> {
    uninstall_files(system, dir, payload(), report)
}

pub fn uninstall_files(
    system: &dyn System,
    dir: &Path,
    files: &[(&str, &[u8])],
    report: Report<'_>,
) -> Result<bool> {
    step(report, 0, || ensure_closed(dir))?;
    step(report, 1, || {
        for folder in system.shortcut_dirs(true) {
            let link = folder.join(LINK_NAME);
            if link.exists() {
                fs::remove_file(link)?;
            }
        }
        Ok(())
    })?;
    step(report, 2, || {
        let ours = dir.join(relative(COM_SERVER)?);
        let regsvr32 = system.syswow64().join("regsvr32.exe");
        // Leave the original application working if it is still installed.
        let result = if Path::new(VENDOR_COM_SERVER).is_file() {
            system.run(&regsvr32, &["/s".into(), VENDOR_COM_SERVER.into()])?
        } else if ours.is_file() {
            system.run(
                &regsvr32,
                &["/u".into(), "/s".into(), ours.into_os_string()],
            )?
        } else {
            (0, String::new())
        };
        exit_ok("Transport registration", result, &[0]).map(drop)
    })?;
    step(report, 3, || system.remove_uninstall())?;
    step(report, 4, || {
        let running = system.current_exe()?;
        let mut deferred = false;
        let mut names: Vec<PathBuf> = files
            .iter()
            .filter(|(name, _)| !name.starts_with("vcredist/"))
            .map(|(name, _)| relative(name))
            .collect::<Result<_>>()?;
        names.push(PathBuf::from(UNINSTALLER));
        for name in names {
            let path = dir.join(name);
            if path == running {
                deferred = true;
            } else if path.exists() {
                fs::remove_file(&path).with_context(|| format!("Remove {}", path.display()))?;
            }
        }
        remove_empty_dirs(dir);
        Ok(deferred)
    })
}

fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                remove_empty_dirs(&entry.path());
            }
        }
    }
    let _ = fs::remove_dir(dir);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::HashSet};

    struct Fake {
        root: PathBuf,
        runtime: RefCell<bool>,
        calls: RefCell<Vec<String>>,
        codes: RefCell<Vec<i32>>,
        registered: RefCell<Option<(PathBuf, u32)>>,
    }

    impl Fake {
        fn new(name: &str, runtime: bool) -> Self {
            let root =
                std::env::temp_dir().join(format!("cart-setup-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("bin")).unwrap();
            fs::write(root.join("bin").join("cart-setup.exe"), b"setup").unwrap();
            Self {
                root,
                runtime: RefCell::new(runtime),
                calls: RefCell::default(),
                codes: RefCell::default(),
                registered: RefCell::default(),
            }
        }
    }

    impl System for Fake {
        fn run(&self, program: &Path, args: &[OsString]) -> Result<(i32, String)> {
            let name = program.file_name().unwrap().to_string_lossy().into_owned();
            if name == "vcredist_x86.exe" {
                assert_eq!(fs::read(program).unwrap(), b"redist");
                *self.runtime.borrow_mut() = true;
            }
            let args: Vec<_> = args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            self.calls
                .borrow_mut()
                .push(format!("{name} {}", args.join(" ")));
            Ok((self.codes.borrow_mut().pop().unwrap_or(0), String::new()))
        }
        fn vc_runtime_present(&self) -> bool {
            *self.runtime.borrow()
        }
        fn temp_dir(&self) -> PathBuf {
            self.root.join("temp")
        }
        fn system32(&self) -> PathBuf {
            self.root.join("System32")
        }
        fn syswow64(&self) -> PathBuf {
            self.root.join("SysWOW64")
        }
        fn shortcut_dirs(&self, desktop: bool) -> Vec<PathBuf> {
            let mut dirs = vec![self.root.join("Start Menu")];
            if desktop {
                dirs.push(self.root.join("Desktop"));
            }
            dirs
        }
        fn shortcut(&self, link: &Path, target: &Path, arguments: &str) -> Result<()> {
            fs::write(link, format!("{} {arguments}", target.display()))?;
            Ok(())
        }
        fn register_uninstall(&self, entry: &UninstallEntry) -> Result<()> {
            *self.registered.borrow_mut() = Some((entry.dir.clone(), entry.size_kb));
            Ok(())
        }
        fn remove_uninstall(&self) -> Result<()> {
            *self.registered.borrow_mut() = None;
            Ok(())
        }
        fn current_exe(&self) -> Result<PathBuf> {
            Ok(self.root.join("bin").join("cart-setup.exe"))
        }
    }

    const FILES: &[(&str, &[u8])] = &[
        ("cart-viewer.exe", b"viewer"),
        ("cart-device-helper.exe", b"helper"),
        ("runtime/python27.dll", b"python"),
        ("runtime/usbiocom.dll", b"com"),
        ("runtime/data/ulcvm.fgz", b"fpga"),
        ("driver/ulcvm.inf", b"inf"),
        ("vcredist/vcredist_x86.exe", b"redist"),
    ];

    #[test]
    fn install_writes_files_runs_each_system_step_once_and_skips_present_runtime() {
        let fake = Fake::new("install", true);
        let dir = fake.root.join("Program Files").join(PRODUCT);
        let mut states = Vec::new();
        let outcome = install_files(
            &fake,
            &Options {
                dir: dir.clone(),
                desktop: true,
            },
            FILES,
            &mut |index, state, _| states.push((index, state)),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::default());
        assert_eq!(
            fs::read(dir.join("runtime/data/ulcvm.fgz")).unwrap(),
            b"fpga"
        );
        assert_eq!(fs::read(dir.join(UNINSTALLER)).unwrap(), b"setup");
        assert!(
            !dir.join("vcredist").exists(),
            "the redistributable is never installed as a file"
        );
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(
            calls[0].starts_with("pnputil.exe /add-driver ")
                && calls[0].ends_with("ulcvm.inf /install")
        );
        assert!(calls[1].starts_with("regsvr32.exe /s ") && calls[1].ends_with("usbiocom.dll"));
        for folder in ["Start Menu", "Desktop"] {
            let link = fs::read_to_string(fake.root.join(folder).join(LINK_NAME)).unwrap();
            assert!(link.ends_with("cart-viewer.exe --connect"));
        }
        assert_eq!(fake.registered.borrow().as_ref().unwrap().0, dir);
        assert!(states.contains(&(1, State::Skipped)));
        assert_eq!(
            states
                .iter()
                .filter(|(_, state)| *state == State::Failed)
                .count(),
            0
        );
        let _ = fs::remove_dir_all(&fake.root);
    }

    #[test]
    fn missing_runtime_is_installed_and_restart_codes_are_reported() {
        let fake = Fake::new("runtime", false);
        // Popped from the end: regsvr32, pnputil, then the redistributable.
        *fake.codes.borrow_mut() = vec![0, 3010, 0];
        let dir = fake.root.join("app");
        let outcome = install_files(
            &fake,
            &Options {
                dir,
                desktop: false,
            },
            FILES,
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            outcome,
            Outcome {
                restart_required: true,
                runtime_installed: true
            }
        );
        assert!(fake.calls.borrow()[0].starts_with("vcredist_x86.exe /q /norestart"));
        assert!(!fake.root.join("Desktop").exists());
        let _ = fs::remove_dir_all(&fake.root);
    }

    #[test]
    fn failed_driver_step_stops_before_later_changes() {
        let fake = Fake::new("driver", true);
        *fake.codes.borrow_mut() = vec![5];
        let mut failed = HashSet::new();
        let error = install_files(
            &fake,
            &Options {
                dir: fake.root.join("app"),
                desktop: false,
            },
            FILES,
            &mut |index, state, _| {
                if state == State::Failed {
                    failed.insert(index);
                }
            },
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("Driver installation failed with exit code 5"));
        assert_eq!(failed, HashSet::from([3]));
        assert_eq!(fake.calls.borrow().len(), 1);
        assert!(fake.registered.borrow().is_none());
        let _ = fs::remove_dir_all(&fake.root);
    }

    #[test]
    fn uninstall_removes_only_what_setup_created_and_defers_its_own_program() {
        let fake = Fake::new("remove", true);
        let dir = fake.root.join("app");
        install_files(
            &fake,
            &Options {
                dir: dir.clone(),
                desktop: true,
            },
            FILES,
            &mut |_, _, _| {},
        )
        .unwrap();
        fs::write(dir.join("runtime").join("notes.txt"), b"keep").unwrap();
        fake.calls.borrow_mut().clear();
        let deferred = uninstall_files(&fake, &dir, FILES, &mut |_, _, _| {}).unwrap();
        assert!(!deferred);
        assert!(!dir.join(VIEWER).exists() && !dir.join(UNINSTALLER).exists());
        assert!(
            !dir.join("runtime/data").exists(),
            "emptied folders are removed"
        );
        assert_eq!(
            fs::read(dir.join("runtime").join("notes.txt")).unwrap(),
            b"keep"
        );
        assert!(!fake.root.join("Start Menu").join(LINK_NAME).exists());
        assert!(!fake.root.join("Desktop").join(LINK_NAME).exists());
        assert!(fake.registered.borrow().is_none());
        // The transport is re-registered for the original app or unregistered.
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].starts_with("regsvr32.exe "));
        assert!(
            !calls.iter().any(|call| call.contains("pnputil")),
            "the shared driver stays"
        );
        drop(calls);

        // Running from the install folder, the uninstaller removes itself later.
        let running = Fake::new("remove-self", true);
        let dir = running.root.join("bin");
        install_files(
            &running,
            &Options {
                dir: dir.clone(),
                desktop: false,
            },
            FILES,
            &mut |_, _, _| {},
        )
        .unwrap();
        fs::copy(dir.join("cart-setup.exe"), dir.join(UNINSTALLER)).unwrap();
        struct SelfAt<'a>(&'a Fake, PathBuf);
        impl System for SelfAt<'_> {
            fn run(&self, p: &Path, a: &[OsString]) -> Result<(i32, String)> {
                self.0.run(p, a)
            }
            fn vc_runtime_present(&self) -> bool {
                true
            }
            fn temp_dir(&self) -> PathBuf {
                self.0.temp_dir()
            }
            fn system32(&self) -> PathBuf {
                self.0.system32()
            }
            fn syswow64(&self) -> PathBuf {
                self.0.syswow64()
            }
            fn shortcut_dirs(&self, d: bool) -> Vec<PathBuf> {
                self.0.shortcut_dirs(d)
            }
            fn shortcut(&self, l: &Path, t: &Path, a: &str) -> Result<()> {
                self.0.shortcut(l, t, a)
            }
            fn register_uninstall(&self, e: &UninstallEntry) -> Result<()> {
                self.0.register_uninstall(e)
            }
            fn remove_uninstall(&self) -> Result<()> {
                self.0.remove_uninstall()
            }
            fn current_exe(&self) -> Result<PathBuf> {
                Ok(self.1.clone())
            }
        }
        let system = SelfAt(&running, dir.join(UNINSTALLER));
        assert!(uninstall_files(&system, &dir, FILES, &mut |_, _, _| {}).unwrap());
        assert!(dir.join(UNINSTALLER).exists());
        assert!(!dir.join(VIEWER).exists());
        let _ = fs::remove_dir_all(&fake.root);
        let _ = fs::remove_dir_all(&running.root);
    }

    #[test]
    fn payload_paths_cannot_escape_the_install_folder() {
        for name in ["../evil.exe", "/abs.exe", "C:/abs.exe", "", "a/../../b"] {
            assert!(relative(name).is_err(), "{name}");
        }
        assert_eq!(
            relative("runtime/data/ulcvm.fgz").unwrap(),
            PathBuf::from(r"runtime\data\ulcvm.fgz")
        );
    }

    #[test]
    fn a_running_viewer_blocks_setup_with_an_actionable_message() {
        let fake = Fake::new("running", true);
        let dir = fake.root.join("app");
        fs::create_dir_all(&dir).unwrap();
        ensure_closed(&dir).unwrap();
        let viewer = dir.join(VIEWER);
        fs::write(&viewer, b"old").unwrap();
        // Exclusive access stands in for Windows' lock on a running image.
        let _lock = {
            use std::os::windows::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&viewer)
                .unwrap()
        };
        let error = ensure_closed(&dir).unwrap_err().to_string();
        assert!(error.contains("Close it, then choose Try again"));
        drop(_lock);
        let _ = fs::remove_dir_all(&fake.root);
    }
}
