//! Private, bounded helper transport. No device ABI is loaded by the x64 viewer.
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use cart_core::ipc::{FrameMetadata, MAX_FRAME_CAPACITY};
use serde::{Deserialize, Serialize};
use winit::event_loop::EventLoopProxy;

use crate::Wake;

const HEADER_LEN: usize = 24;
const MAX_STATUS: usize = 65_536;
const MAX_COMMANDS: usize = 128;
pub const MAX_PASTE_BYTES: usize = 16_384;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandMessage {
    Key {
        usage: u8,
        down: bool,
    },
    Mouse {
        x: u32,
        y: u32,
        buttons: u8,
        wheel: i8,
    },
    Release,
    Paste {
        text: String,
        delay_ms: u32,
    },
    Cancel,
    Quit,
    /// Original Auto: the mode's X default, then image-guided fine-tuning.
    AlignAuto,
    /// The mode's X default, with Y returned to zero.
    AlignReset,
    /// Picture positions chosen per video mode; the helper applies them.
    Positions {
        positions: Vec<SavedPosition>,
    },
    /// One helper-timed Shift tap that may wake a sleeping display.
    Wake,
    /// Present a disk image to the target as a USB drive or CD-ROM.
    DriveAttach {
        path: String,
        format: crate::drive::ImageFormat,
    },
    /// Unplug the virtual drive.
    DriveDetach,
}

/// The virtual USB drive as the helper serves it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct DriveStatus {
    /// The adapter has the file-transfer endpoints.
    pub supported: bool,
    pub attached: bool,
    /// "disk" or "cdrom" while attached.
    pub media: Option<String>,
    /// The target has prevented removal, typically because it is mounted.
    pub locked: bool,
    /// The target ejected the drive itself.
    pub ejected: bool,
    /// Advances whenever the helper has handled an attach or detach.
    pub sequence: u16,
    pub size: u64,
    pub read: u64,
    pub written: u64,
    pub error: Option<String>,
}

/// A picture position the user chose for one video mode. Relative to the
/// adapter's own reading, so it is never an EEPROM setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedPosition {
    pub width: u16,
    pub height: u16,
    pub hz: u8,
    pub x: i8,
    pub y: i8,
}

#[derive(Clone, Default, Deserialize)]
pub struct Status {
    pub state: Option<String>,
    pub message: Option<String>,
    pub keyboard_ready: Option<bool>,
    pub caps_lock: Option<bool>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub paste_total: Option<u32>,
    pub paste_done: Option<u32>,
    pub pasting: Option<bool>,
    pub keyboard_link: Option<bool>,
    pub hz: Option<u8>,
    pub position_x: Option<i8>,
    pub position_y: Option<i8>,
    pub saved_position: Option<bool>,
    pub aligning: Option<bool>,
    pub drive: Option<DriveStatus>,
}

impl Status {
    /// The adapter's current video mode, as the original identified it.
    pub fn mode(&self) -> Option<(u16, u16, u8)> {
        Some((
            u16::try_from(self.width?).ok()?,
            u16::try_from(self.height?).ok()?,
            self.hz?,
        ))
        .filter(|&(width, height, hz)| width > 0 && height > 0 && hz > 0)
    }
}

impl Status {
    fn merge(&mut self, next: Self) {
        macro_rules! fields {
            ($($name:ident),*) => { $(if next.$name.is_some() { self.$name = next.$name; })* };
        }
        fields!(
            state,
            message,
            keyboard_ready,
            caps_lock,
            width,
            height,
            paste_total,
            paste_done,
            pasting,
            keyboard_link,
            hz,
            position_x,
            position_y,
            saved_position,
            aligning,
            drive
        );
    }
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Default)]
pub struct Inbox {
    pub frame: Option<Frame>,
    pub status: Status,
    pub error: Option<String>,
    pub paste_revision: u64,
}

type Outbox = Arc<(Mutex<VecDeque<CommandMessage>>, Condvar)>;

pub struct Helper {
    child: Child,
    pub inbox: Arc<Mutex<Inbox>>,
    outbox: Outbox,
    stopping: Arc<AtomicBool>,
}

pub fn helper_path(override_path: Option<&Path>) -> PathBuf {
    if let Some(path) = override_path {
        return path.to_owned();
    }
    if let Some(path) = std::env::var_os("CART_HELPER_PATH") {
        return path.into();
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let executable = std::env::current_exe().unwrap_or_default();
    let directory = executable.parent().unwrap_or(Path::new("."));
    let profile = if directory.file_name().is_some_and(|p| p == "release") {
        "release"
    } else {
        "debug"
    };
    let candidates = [
        directory.join("cart-device-helper.exe"),
        directory.join(format!(
            "../../i686-pc-windows-msvc/{profile}/cart-device-helper.exe"
        )),
        root.join(format!(
            "target/i686-pc-windows-msvc/{profile}/cart-device-helper.exe"
        )),
    ];
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| root.join("target/i686-pc-windows-msvc/debug/cart-device-helper.exe"))
}

impl Helper {
    pub fn spawn(path: &Path, wake: EventLoopProxy<Wake>) -> Result<Self> {
        let mut command = Command::new(path);
        command
            .arg("--serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW; never starts a console beside the GUI.
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("Could not start helper at {}", path.display()))?;
        let mut stdin = child.stdin.take().context("Helper stdin unavailable")?;
        let mut stdout = child.stdout.take().context("Helper stdout unavailable")?;
        let mut stderr = child.stderr.take().context("Helper stderr unavailable")?;
        let inbox = Arc::new(Mutex::new(Inbox::default()));
        let outbox: Outbox = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let stopping = Arc::new(AtomicBool::new(false));
        let reader_inbox = inbox.clone();
        let reader_stop = stopping.clone();
        let reader_wake = wake.clone();
        thread::spawn(move || {
            let result = read_records(&mut stdout, &reader_inbox, || {
                let _ = reader_wake.send_event(Wake::Helper);
            });
            if !reader_stop.load(Ordering::Acquire) {
                reader_inbox.lock().unwrap().error = Some(match result {
                    Ok(()) => {
                        "Helper closed its video stream unexpectedly. Disconnect and reconnect."
                            .into()
                    }
                    Err(error) => format!("Helper stream failed: {error}"),
                });
                let _ = reader_wake.send_event(Wake::Helper);
            }
        });
        let writer_inbox = inbox.clone();
        let writer_outbox = outbox.clone();
        let writer_stop = stopping.clone();
        thread::spawn(move || {
            loop {
                let message = {
                    let (lock, ready) = &*writer_outbox;
                    let mut pending = lock.lock().unwrap();
                    while pending.is_empty() && !writer_stop.load(Ordering::Acquire) {
                        pending = ready.wait(pending).unwrap();
                    }
                    let Some(message) = pending.pop_front() else {
                        break;
                    };
                    message
                };
                let quit = matches!(message, CommandMessage::Quit);
                let result = serde_json::to_writer(&mut stdin, &message)
                    .map_err(io::Error::other)
                    .and_then(|()| stdin.write_all(b"\n"))
                    .and_then(|()| stdin.flush());
                if let Err(error) = result {
                    if !writer_stop.load(Ordering::Acquire) {
                        writer_inbox.lock().unwrap().error =
                            Some(format!("Helper command pipe failed: {error}"));
                        let _ = wake.send_event(Wake::Helper);
                    }
                    break;
                }
                if quit {
                    break;
                }
            }
        });
        // Drain private stderr so a full pipe cannot stall the helper. Deliberately
        // do not log its contents: clipboard or target text must not enter logs.
        thread::spawn(move || {
            let _ = io::copy(&mut stderr, &mut io::sink());
        });
        Ok(Self {
            child,
            inbox,
            outbox,
            stopping,
        })
    }

    pub fn send(&self, message: CommandMessage) -> Result<()> {
        let (lock, ready) = &*self.outbox;
        let mut queue = lock.lock().unwrap();
        if self.stopping.load(Ordering::Acquire) {
            bail!("Helper is shutting down");
        }
        if queue.len() >= MAX_COMMANDS {
            // Emergency controls replace queued input rather than being trapped
            // behind it. The UI stops capture on this error.
            queue.clear();
            queue.push_back(CommandMessage::Cancel);
            queue.push_back(CommandMessage::Release);
            ready.notify_one();
            bail!("Input queue filled. Input released; reconnect before continuing.");
        }
        queue.push_back(message);
        ready.notify_one();
        Ok(())
    }

    pub fn cancel_release(&self) {
        let (lock, ready) = &*self.outbox;
        let mut queue = lock.lock().unwrap();
        queue.clear();
        queue.push_back(CommandMessage::Cancel);
        queue.push_back(CommandMessage::Release);
        ready.notify_one();
    }

    /// Only this instance's child is ever waited for or terminated.
    pub fn shutdown(&mut self) -> Result<()> {
        {
            let (lock, ready) = &*self.outbox;
            let mut queue = lock.lock().unwrap();
            // Set stop under the queue lock so the writer cannot observe an
            // empty stopped queue before the final Quit is installed.
            if self.stopping.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            queue.clear();
            queue.extend([
                CommandMessage::Cancel,
                CommandMessage::Release,
                CommandMessage::Quit,
            ]);
            ready.notify_one();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.child
                    .kill()
                    .context("Could not stop unresponsive owned helper")?;
                let _ = self.child.wait();
                bail!(
                    "Helper did not exit within 3 seconds and was terminated. Remote release could not be confirmed."
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("{error}");
        }
    }
}

fn read_records(
    reader: &mut impl Read,
    inbox: &Mutex<Inbox>,
    mut wake: impl FnMut(),
) -> Result<()> {
    let mut previous = None;
    loop {
        let mut header = [0u8; HEADER_LEN];
        // Distinguish a clean EOF from a truncated record.
        if reader.read(&mut header[..1])? == 0 {
            return Ok(());
        }
        reader.read_exact(&mut header[1..])?;
        if &header[..4] != b"CART" {
            bail!("Invalid CART record magic");
        }
        let u16_at = |i| u16::from_le_bytes([header[i], header[i + 1]]);
        let u32_at = |i| u32::from_le_bytes(header[i..i + 4].try_into().unwrap());
        if u16_at(4) != 1 {
            bail!("Unsupported CART protocol version {}", u16_at(4));
        }
        let kind = u16_at(6);
        let sequence = u32_at(8);
        let width = u32_at(12);
        let height = u32_at(16);
        let length = u32_at(20) as usize;
        match kind {
            1 => {
                let layout = FrameMetadata {
                    generation: u64::from(sequence),
                    width,
                    height,
                    stride_bytes: u64::from(width) * 4,
                }
                .validate(MAX_FRAME_CAPACITY, previous)?;
                if layout.byte_len() != length {
                    bail!("Frame length does not match active dimensions");
                }
                let mut rgba = vec![0; length];
                reader.read_exact(&mut rgba)?;
                previous = Some(u64::from(sequence));
                // A single latest-frame slot: stale frames are replaced, never queued.
                inbox.lock().unwrap().frame = Some(Frame {
                    width,
                    height,
                    rgba,
                });
            }
            2 => {
                if length > MAX_STATUS {
                    bail!("Status record exceeds 64 KiB");
                }
                let mut payload = vec![0; length];
                reader.read_exact(&mut payload)?;
                let status: Status =
                    serde_json::from_slice(&payload).context("Invalid helper status JSON")?;
                let mut data = inbox.lock().unwrap();
                if status.pasting.is_some() {
                    data.paste_revision += 1;
                }
                data.status.merge(status);
            }
            _ => bail!("Unknown CART record kind {kind}"),
        }
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn record(kind: u16, seq: u32, width: u32, height: u32, payload: &[u8]) -> Vec<u8> {
        let mut bytes = b"CART".to_vec();
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(kind.to_le_bytes());
        for value in [seq, width, height, payload.len() as u32] {
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn latest_frame_only_and_partial_status_merge() {
        let mut bytes = record(1, 1, 1, 1, &[1; 4]);
        bytes.extend(record(
            2,
            2,
            0,
            0,
            br#"{"state":"viewing","keyboard_ready":true,"future":123}"#,
        ));
        bytes.extend(record(1, 3, 1, 1, &[9; 4]));
        bytes.extend(record(2, 4, 0, 0, br#"{"caps_lock":false}"#));
        let inbox = Mutex::new(Inbox::default());
        read_records(&mut Cursor::new(bytes), &inbox, || {}).unwrap();
        let data = inbox.lock().unwrap();
        assert_eq!(data.frame.as_ref().unwrap().rgba, [9; 4]);
        assert_eq!(data.status.keyboard_ready, Some(true));
        assert_eq!(data.status.state.as_deref(), Some("viewing"));
        assert_eq!(data.status.caps_lock, Some(false));
    }

    #[test]
    fn malformed_frames_never_enter_slot() {
        for bytes in [
            record(1, 1, 1921, 1, &[]),
            record(1, 1, 1, 1, &[0; 3]),
            record(1, 1, 0, 1, &[]),
        ] {
            let inbox = Mutex::new(Inbox::default());
            assert!(read_records(&mut Cursor::new(bytes), &inbox, || {}).is_err());
            assert!(inbox.lock().unwrap().frame.is_none());
        }
    }

    #[test]
    fn truncated_header_payload_and_sequence_regression_fail() {
        let valid = record(1, 4, 1, 1, &[0; 4]);
        for count in [1, 23, 25, 27] {
            assert!(
                read_records(
                    &mut Cursor::new(&valid[..count]),
                    &Mutex::new(Inbox::default()),
                    || {}
                )
                .is_err()
            );
        }
        let mut bytes = valid.clone();
        bytes.extend(valid);
        assert!(
            read_records(
                &mut Cursor::new(bytes),
                &Mutex::new(Inbox::default()),
                || {}
            )
            .is_err()
        );
    }

    #[test]
    fn command_contract_is_json_lines_without_extra_fields() {
        assert_eq!(
            serde_json::to_string(&CommandMessage::Key {
                usage: 4,
                down: true
            })
            .unwrap(),
            r#"{"type":"key","usage":4,"down":true}"#
        );
        let encoded = serde_json::to_string(&CommandMessage::Paste {
            text: "a\n\"b".into(),
            delay_ms: 50,
        })
        .unwrap();
        assert!(!encoded.contains('\n'));
        assert!(encoded.contains("\\n"));
        assert_eq!(
            serde_json::to_string(&CommandMessage::AlignAuto).unwrap(),
            r#"{"type":"align_auto"}"#
        );
        assert_eq!(
            serde_json::to_string(&CommandMessage::Wake).unwrap(),
            r#"{"type":"wake"}"#
        );
        assert_eq!(
            serde_json::to_string(&CommandMessage::Positions {
                positions: vec![SavedPosition {
                    width: 1920,
                    height: 1080,
                    hz: 60,
                    x: -45,
                    y: 0
                }]
            })
            .unwrap(),
            r#"{"type":"positions","positions":[{"width":1920,"height":1080,"hz":60,"x":-45,"y":0}]}"#
        );
        assert_eq!(
            serde_json::to_string(&CommandMessage::DriveAttach {
                path: r"C:\Disks\boot.iso".into(),
                format: crate::drive::ImageFormat::Iso,
            })
            .unwrap(),
            r#"{"type":"drive_attach","path":"C:\\Disks\\boot.iso","format":"iso"}"#
        );
        assert_eq!(
            serde_json::to_string(&CommandMessage::DriveDetach).unwrap(),
            r#"{"type":"drive_detach"}"#
        );
    }

    #[test]
    fn drive_status_merges_as_one_value() {
        let mut status = Status::default();
        status.merge(
            serde_json::from_str(
                r#"{"drive":{"supported":true,"attached":true,"media":"disk","locked":true,"ejected":false,"sequence":3,"size":536870912,"read":4096,"written":0,"error":null}}"#,
            )
            .unwrap(),
        );
        let drive = status.drive.clone().unwrap();
        assert!(drive.attached && drive.locked && drive.media.as_deref() == Some("disk"));
        status.merge(serde_json::from_str(r#"{"state":"viewing"}"#).unwrap());
        assert_eq!(status.drive, Some(drive));
    }
}
