//! Private pipe service. Only the calling thread may enter the Python runtime.
//! Pipe workers never own Python objects or perform device I/O.

use std::{
    collections::VecDeque,
    fmt::Write as _,
    io::{self, BufRead, Write},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use cart_core::{
    input::{InputState, LEFT_SHIFT_MASK, is_supported_usage},
    ipc::{FrameMetadata, MAX_FRAME_CAPACITY},
    pacing::{PasteScheduler, SchedulePoll},
    paste::{PastePlan, Stroke},
    protocol,
};
use serde::Deserialize;
use serde_json::{Value, json};

const MAX_LINE: usize = 65_536;
const MAX_PASTE_CHARS: usize = 16_384;
const COMMAND_CAPACITY: usize = 128;
const STATUS_CAPACITY: usize = 128;
const HEADER_LEN: usize = 24;
const KEY_HOLD: Duration = Duration::from_millis(50);
const LOOP_PAUSE: Duration = Duration::from_millis(3);
const FRAME_INTERVAL: Duration = Duration::from_millis(15);
const STATUS_INTERVAL: Duration = Duration::from_millis(250);
/// The Python session queries status every 0.5 s; older readings are stale.
const STATUS_FRESH: Duration = Duration::from_secs(2);
const LEFT_SHIFT: u8 = 0xe1;
const MAX_SAVED_POSITIONS: usize = 64;
/// Windows extended-length paths stay well below this.
const MAX_DRIVE_PATH: usize = 4096;
const DRIVE_STATUS_LEN: usize = 31;

const INPUT_INVALID: u8 = 1;
const INPUT_OVERFLOW: u8 = 2;
const INPUT_IO: u8 = 3;
const OUTPUT_IO: u8 = 4;
const OUTPUT_OVERFLOW: u8 = 5;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Key {
        usage: u8,
        down: bool,
    },
    Mouse {
        x: i32,
        y: i32,
        buttons: u8,
        wheel: i16,
    },
    Release {},
    Paste {
        text: String,
        delay_ms: u64,
    },
    Cancel {},
    Quit {},
    /// Original Auto: mode X default, then image-guided fine-tuning.
    AlignAuto {},
    /// Original Default for X, with Y returned to zero.
    AlignReset {},
    /// Positions the user chose per video mode, applied when a mode appears.
    Positions {
        positions: Vec<SavedPosition>,
    },
    /// One helper-scheduled Shift tap; allowed without video to wake a display.
    Wake {},
    /// Present a disk image to the target as a USB drive or CD-ROM
    /// (original Disk Control Insert).
    DriveAttach {
        path: String,
        format: DriveFormat,
    },
    /// Unplug the virtual drive (original Eject).
    DriveDetach {},
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DriveFormat {
    Raw,
    Vhd,
    Iso,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SavedPosition {
    width: u16,
    height: u16,
    hz: u8,
    x: i8,
    y: i8,
}

fn parse_command(line: &[u8]) -> Result<Command> {
    ensure!(line.len() <= MAX_LINE, "Command exceeds 64 KiB");
    // Do not propagate serde's error: it can include clipboard text or field names.
    let command: Command =
        serde_json::from_slice(line).map_err(|_| anyhow::anyhow!("Invalid private command"))?;
    match &command {
        Command::Key { usage, .. } => {
            ensure!(is_supported_usage(*usage), "Unsupported HID usage");
        }
        Command::Mouse { buttons, wheel, .. } => {
            ensure!(buttons & !7 == 0, "Invalid mouse buttons");
            ensure!(*wheel != i16::MIN, "Mouse wheel cannot be inverted");
        }
        Command::Paste { text, .. } => {
            ensure!(
                text.chars().count() <= MAX_PASTE_CHARS,
                "Paste exceeds 16,384 characters"
            );
        }
        Command::DriveAttach { path, .. } => {
            ensure!(
                path.len() <= MAX_DRIVE_PATH
                    && !path.contains('\0')
                    && std::path::Path::new(path).is_absolute(),
                "Invalid disk image path"
            );
        }
        Command::Positions { positions } => {
            ensure!(
                positions.len() <= MAX_SAVED_POSITIONS,
                "Too many saved picture positions"
            );
            for position in positions {
                ensure!(
                    (1..=1920).contains(&position.width)
                        && (1..=1600).contains(&position.height)
                        && position.hz > 0,
                    "Invalid saved picture position"
                );
            }
        }
        _ => {}
    }
    Ok(command)
}

/// EOF is accepted only between complete newline-delimited messages. The
/// fill_buf/consume loop never allocates an unbounded read_until buffer.
fn read_line_bounded(reader: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<bool> {
    line.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(false)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Truncated command",
                ))
            };
        }
        let end = available.iter().position(|&byte| byte == b'\n');
        let count = end.unwrap_or(available.len());
        if count > MAX_LINE - line.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Command exceeds 64 KiB",
            ));
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count + usize::from(end.is_some()));
        if end.is_some() {
            return Ok(true);
        }
    }
}

#[derive(Default)]
struct Control {
    epoch: AtomicU64,
    quitting: AtomicBool,
    failure: AtomicU8,
}

impl Control {
    fn barrier(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    fn quit(&self) {
        self.quitting.store(true, Ordering::Release);
        self.barrier();
    }

    fn fail(&self, reason: u8) {
        let _ = self
            .failure
            .compare_exchange(0, reason, Ordering::AcqRel, Ordering::Acquire);
        self.quit();
    }

    fn current(&self, epoch: u64) -> bool {
        !self.quitting.load(Ordering::Acquire) && self.epoch.load(Ordering::Acquire) == epoch
    }

    fn check_failure(&self) -> Result<()> {
        match self.failure.load(Ordering::Acquire) {
            0 => Ok(()),
            INPUT_INVALID => {
                bail!("Invalid or oversized command; input released and connection closed")
            }
            INPUT_OVERFLOW => bail!("Command queue filled; input released and connection closed"),
            INPUT_IO => bail!("Private command pipe failed"),
            OUTPUT_IO => bail!("Private output pipe failed"),
            OUTPUT_OVERFLOW => bail!("Status queue filled; input released and connection closed"),
            _ => bail!("Private pipe worker failed"),
        }
    }
}

struct QueuedCommand {
    epoch: u64,
    command: Command,
}

fn read_commands(mut reader: impl BufRead, sender: SyncSender<QueuedCommand>, control: &Control) {
    let mut line = Vec::with_capacity(1024);
    loop {
        if control.quitting.load(Ordering::Acquire) {
            return;
        }
        match read_line_bounded(&mut reader, &mut line) {
            Ok(false) => {
                control.quit();
                return;
            }
            Err(error) => {
                control.fail(
                    if error.kind() == io::ErrorKind::InvalidData
                        || error.kind() == io::ErrorKind::UnexpectedEof
                    {
                        INPUT_INVALID
                    } else {
                        INPUT_IO
                    },
                );
                return;
            }
            Ok(true) => {}
        }
        let command = match parse_command(&line) {
            Ok(command) => command,
            Err(_) => {
                control.fail(INPUT_INVALID);
                return;
            }
        };
        match command {
            Command::Release {} | Command::Cancel {} => control.barrier(),
            Command::Quit {} => {
                control.quit();
                return;
            }
            command => {
                let queued = QueuedCommand {
                    epoch: control.epoch.load(Ordering::Acquire),
                    command,
                };
                match sender.try_send(queued) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        // A key-up may be the overflowing message. Never drop it
                        // and continue: release everything, then disconnect.
                        control.fail(INPUT_OVERFLOW);
                        return;
                    }
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
        }
    }
}

struct Record {
    kind: u16,
    width: u32,
    height: u32,
    bytes: Vec<u8>,
    offset: usize,
}

impl Record {
    fn status(value: Value) -> Result<Self> {
        let bytes = serde_json::to_vec(&value)?;
        ensure!(bytes.len() <= MAX_LINE, "Status exceeds 64 KiB");
        Ok(Self {
            kind: 2,
            width: 0,
            height: 0,
            bytes,
            offset: 0,
        })
    }

    fn frame(bytes: Vec<u8>) -> Result<Option<Self>> {
        if bytes.is_empty() {
            return Ok(None);
        }
        ensure!(bytes.len() >= 8, "Truncated Python frame metadata");
        let width = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let height = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let stride_bytes = u64::from(width)
            .checked_mul(4)
            .context("Frame stride overflow")?;
        let layout = FrameMetadata {
            generation: 0,
            width,
            height,
            stride_bytes,
        }
        .validate(MAX_FRAME_CAPACITY, None)?;
        ensure!(
            bytes.len().checked_sub(8) == Some(layout.byte_len()),
            "Frame length does not match active dimensions"
        );
        Ok(Some(Self {
            kind: 1,
            width,
            height,
            bytes,
            offset: 8,
        }))
    }

    fn payload(&self) -> &[u8] {
        &self.bytes[self.offset..]
    }

    fn header(&self, sequence: u32) -> [u8; HEADER_LEN] {
        let mut header = [0; HEADER_LEN];
        header[..4].copy_from_slice(b"CART");
        header[4..6].copy_from_slice(&1_u16.to_le_bytes());
        header[6..8].copy_from_slice(&self.kind.to_le_bytes());
        for (offset, value) in [
            (8, sequence),
            (12, self.width),
            (16, self.height),
            (20, self.payload().len() as u32),
        ] {
            header[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        header
    }
}

#[derive(Default)]
struct PendingOutput {
    statuses: VecDeque<Record>,
    frame: Option<Record>,
    finished: bool,
}

type OutputQueue = Arc<(Mutex<PendingOutput>, Condvar)>;

struct Output {
    queue: OutputQueue,
    control: Arc<Control>,
}

impl Output {
    fn status(&self, value: Value) -> Result<()> {
        let record = Record::status(value)?;
        let (lock, ready) = &*self.queue;
        let mut pending = lock.lock().unwrap_or_else(|error| error.into_inner());
        if pending.statuses.len() >= STATUS_CAPACITY {
            self.control.fail(OUTPUT_OVERFLOW);
            bail!("Private status queue filled");
        }
        pending.statuses.push_back(record);
        ready.notify_one();
        Ok(())
    }

    fn frame(&self, record: Record) {
        let (lock, ready) = &*self.queue;
        // Only completed, not-yet-started frames can be replaced. The writer
        // owns its in-flight record outside this lock until every byte is sent.
        lock.lock().unwrap_or_else(|error| error.into_inner()).frame = Some(record);
        ready.notify_one();
    }

    fn finish(&self, error: Option<&str>) {
        let (lock, ready) = &*self.queue;
        let mut pending = lock.lock().unwrap_or_else(|error| error.into_inner());
        pending.frame = None;
        pending.statuses.clear();
        if let Some(message) = error
            && let Ok(record) = Record::status(json!({
                "state": "error", "message": message, "keyboard_ready": false, "pasting": false,
            }))
        {
            pending.statuses.push_back(record);
        }
        if let Ok(record) = Record::status(json!({
            "state": "closed", "keyboard_ready": false, "pasting": false,
        })) {
            pending.statuses.push_back(record);
        }
        pending.finished = true;
        ready.notify_one();
    }
}

fn write_records(mut writer: impl Write, queue: &OutputQueue, control: &Control) {
    let mut sequence = 0_u32;
    loop {
        let record = {
            let (lock, ready) = &**queue;
            let mut pending = lock.lock().unwrap_or_else(|error| error.into_inner());
            while pending.statuses.is_empty() && pending.frame.is_none() && !pending.finished {
                pending = ready
                    .wait(pending)
                    .unwrap_or_else(|error| error.into_inner());
            }
            if let Some(record) = pending.statuses.pop_front() {
                record
            } else if let Some(record) = pending.frame.take() {
                record
            } else {
                return;
            }
        };
        let Some(next) = sequence.checked_add(1) else {
            control.fail(OUTPUT_IO);
            return;
        };
        sequence = next;
        let result = writer
            .write_all(&record.header(sequence))
            .and_then(|()| writer.write_all(record.payload()))
            .and_then(|()| writer.flush());
        if result.is_err() {
            control.fail(OUTPUT_IO);
            return;
        }
    }
}

#[derive(Clone, Copy, Default)]
struct DeviceStatus {
    width: u32,
    height: u32,
    keyboard_ready: bool,
    caps_lock: bool,
    fpga_loaded: bool,
    no_video: bool,
    error: bool,
    hz: u8,
    position_x: i8,
    position_y: i8,
    saved_position: bool,
    aligning: bool,
    sequence: u16,
}

impl DeviceStatus {
    fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() == 16, "Invalid Python status length");
        let status = Self {
            width: u32::from(u16::from_le_bytes([bytes[0], bytes[1]])),
            height: u32::from(u16::from_le_bytes([bytes[2], bytes[3]])),
            // byte 4 is refresh rate; keyboard readiness is byte 5, not 4.
            keyboard_ready: bytes[5] != 0,
            caps_lock: bytes[6] & 0x02 != 0,
            fpga_loaded: bytes[7] != 0,
            no_video: bytes[8] != 0,
            error: bytes[9] != 0,
            hz: bytes[4],
            position_x: i8::from_le_bytes([bytes[10]]),
            position_y: i8::from_le_bytes([bytes[11]]),
            saved_position: bytes[12] != 0,
            aligning: bytes[13] != 0,
            sequence: u16::from_le_bytes([bytes[14], bytes[15]]),
        };
        if status.width != 0 && status.height != 0 {
            FrameMetadata {
                generation: 0,
                width: status.width,
                height: status.height,
                stride_bytes: u64::from(status.width) * 4,
            }
            .validate(MAX_FRAME_CAPACITY, None)?;
        }
        Ok(status)
    }

    fn ready(self) -> bool {
        self.keyboard_ready && self.video_ready()
    }

    fn video_ready(self) -> bool {
        self.width > 0 && self.height > 0 && self.fpga_loaded && !self.no_video && !self.error
    }

    fn state(self) -> &'static str {
        if self.error {
            "error"
        } else if self.video_ready() {
            "viewing"
        } else if self.fpga_loaded && self.no_video {
            "no_video"
        } else {
            "connecting"
        }
    }

    fn contains(self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height
    }
}

/// The virtual drive as the Python session reports it (`poll_drive`).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct DriveStatus {
    supported: bool,
    attached: bool,
    media: u8,
    /// The target has prevented medium removal.
    locked: bool,
    /// The target ejected the medium itself.
    ejected: bool,
    sequence: u16,
    size: u64,
    read: u64,
    written: u64,
}

impl DriveStatus {
    fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() == DRIVE_STATUS_LEN,
            "Invalid drive status length"
        );
        let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let status = Self {
            supported: bytes[0] != 0,
            attached: bytes[1] != 0,
            media: bytes[2],
            locked: bytes[3] != 0,
            ejected: bytes[4] != 0,
            sequence: u16::from_le_bytes([bytes[5], bytes[6]]),
            size: word(7),
            read: word(15),
            written: word(23),
        };
        ensure!(
            matches!((status.attached, status.media), (false, 0) | (true, 1 | 2)),
            "Invalid drive media"
        );
        Ok(status)
    }

    fn json(self, error: &str) -> Value {
        json!({
            "supported": self.supported,
            "attached": self.attached,
            "media": match self.media { 1 => Some("disk"), 2 => Some("cdrom"), _ => None },
            "locked": self.locked,
            "ejected": self.ejected,
            "sequence": self.sequence,
            "size": self.size,
            "read": self.read,
            "written": self.written,
            "error": (!error.is_empty()).then_some(error),
        })
    }
}

/// Only the path's UTF-8 bytes as hexadecimal enter this Python source.
fn drive_attach_source(path: &str, format: DriveFormat) -> String {
    let mut source = String::from("cart_session.drive_attach('");
    for byte in path.bytes() {
        let _ = write!(source, "{byte:02x}");
    }
    let _ = write!(
        source,
        "', '{}')",
        match format {
            DriveFormat::Raw => "raw",
            DriveFormat::Vhd => "vhd",
            DriveFormat::Iso => "iso",
        }
    );
    source
}

/// A write is a transaction: even key-up state stays held until the transport
/// confirms success. The helper never assumes a failed release reached hardware.
fn transition_key(
    input: &mut InputState,
    usage: u8,
    down: bool,
    send: impl FnOnce(&[u8]) -> Result<()>,
) -> Result<()> {
    let mut next = input.clone();
    if let Some(event) = next.transition(usage, down)? {
        // Vendor keyEvent keeps wire meta at zero. InputState's HID modifier
        // mask is logical bookkeeping, not this opaque transport field.
        send(&protocol::encode_key(
            event.usb_key,
            0,
            event.down,
            event.all_up,
        ))?;
        *input = next;
    }
    Ok(())
}

fn send_packet(runtime: &crate::python::Python, packet: &[u8]) -> Result<()> {
    // Only generated hexadecimal byte escapes enter Python source. Clipboard
    // text and JSON fields are never interpolated into executable statements.
    let mut source = String::with_capacity(64 + packet.len() * 4);
    source.push_str("cart_session.transport.send_input('");
    for byte in packet {
        let _ = write!(source, "\\x{byte:02x}");
    }
    source.push_str("')");
    runtime.run(&source).context("Device input write failed")
}

struct Session<'a> {
    runtime: &'a crate::python::Python,
    input: InputState,
    // A failed press can have reached the device despite its failed write
    // acknowledgement. This is not committed held state, but needs cleanup.
    uncertain_key: Option<u8>,
    paste_final_usage: Option<u8>,
    status: DeviceStatus,
    mouse_position: Option<(i16, i16)>,
    mouse_buttons: u8,
    mouse_uncertain: bool,
    cleanup_attempted: bool,
    /// When the Python session last parsed a new adapter status response.
    status_seen: Option<Instant>,
    wake_release: Option<Instant>,
    drive: DriveStatus,
    drive_error: String,
}

impl<'a> Session<'a> {
    fn new(runtime: &'a crate::python::Python) -> Self {
        Self {
            runtime,
            input: InputState::new(),
            uncertain_key: None,
            paste_final_usage: None,
            status: DeviceStatus::default(),
            mouse_position: None,
            mouse_buttons: 0,
            mouse_uncertain: false,
            cleanup_attempted: false,
            status_seen: None,
            wake_release: None,
            drive: DriveStatus::default(),
            drive_error: String::new(),
        }
    }

    /// Keyboard transport readiness from fresh hardware status, independent of
    /// video. Only the explicit wake tap uses it; ordinary input needs video.
    fn keyboard_link(&self) -> bool {
        keyboard_link(self.status, self.status_seen)
    }

    fn wake(&mut self, control: &Control, epoch: u64) -> Result<()> {
        if self.keyboard_link()
            && self.wake_release.is_none()
            && self.input.is_empty()
            && self.uncertain_key.is_none()
            && self.mouse_buttons == 0
            && self.key(LEFT_SHIFT, true, control, epoch)?
        {
            self.wake_release = Some(Instant::now() + KEY_HOLD);
        }
        Ok(())
    }

    fn finish_wake(&mut self, control: &Control, epoch: u64) -> Result<()> {
        if self.wake_release.is_some_and(|due| Instant::now() >= due) {
            self.wake_release = None;
            self.key(LEFT_SHIFT, false, control, epoch)?;
        }
        Ok(())
    }

    fn key(&mut self, usage: u8, down: bool, control: &Control, epoch: u64) -> Result<bool> {
        if !control.current(epoch) {
            return Ok(false);
        }
        let result = transition_key(&mut self.input, usage, down, |packet| {
            send_packet(self.runtime, packet)
        });
        if result.is_err() && down {
            self.uncertain_key = Some(usage);
        }
        result?;
        Ok(true)
    }

    fn paste_key(&mut self, usage: u8, down: bool, control: &Control, epoch: u64) -> Result<bool> {
        if !control.current(epoch) {
            return Ok(false);
        }
        if down {
            self.paste_final_usage = Some(usage);
        }
        self.key(usage, down, control, epoch)
    }

    fn release(&mut self) -> Result<()> {
        let mut first_error = None;
        let mut next = self.input.clone();
        for event in next.release_all() {
            // Commit separately for each successful key-up, not the whole batch.
            if let Err(error) = transition_key(&mut self.input, event.usb_key, false, |packet| {
                send_packet(self.runtime, packet)
            }) {
                first_error = Some(error);
                break;
            }
        }
        if let Some(usage) = self.uncertain_key {
            let result = send_packet(
                self.runtime,
                &protocol::encode_key(usage, 0, false, self.input.is_empty()),
            );
            if result.is_ok() {
                self.uncertain_key = None;
            }
            if first_error.is_none() {
                first_error = result.err();
            }
        }
        if let Some(usage) = self.paste_final_usage {
            let paste_release = if self.input.is_empty() && self.uncertain_key.is_none() {
                send_packet(self.runtime, &protocol::encode_key(usage, 0, false, true))
            } else {
                Err(anyhow::anyhow!(
                    "Cannot finalize paste while keys may be held"
                ))
            };
            if paste_release.is_ok() {
                self.paste_final_usage = None;
            }
            if first_error.is_none() {
                first_error = paste_release.err();
            }
        }
        if self.mouse_buttons != 0 || self.mouse_uncertain {
            let result = (|| {
                // Readiness gates new input, never safety releases. A lost
                // video signal does not imply the input transport is unusable.
                // Keep held bookkeeping if this best-effort write fails.
                let (x, y) = self.mouse_position.context("Missing held mouse position")?;
                send_packet(self.runtime, &protocol::encode_mouse(true, 0, x, y, 0))?;
                self.mouse_buttons = 0;
                self.mouse_uncertain = false;
                Ok(())
            })();
            if first_error.is_none() {
                first_error = result.err();
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn mouse(
        &mut self,
        x: i32,
        y: i32,
        buttons: u8,
        wheel: i16,
        control: &Control,
        epoch: u64,
    ) -> Result<()> {
        ensure!(buttons & !7 == 0, "Invalid mouse buttons");
        let wheel = wheel
            .checked_neg()
            .context("Mouse wheel cannot be inverted")?;
        if !control.current(epoch) {
            return Ok(());
        }
        if !self.status.ready() {
            // No new movement, wheel, or button-down while unready; an all-up
            // for a known held button still uses the last validated position.
            if buttons == 0 && (self.mouse_buttons != 0 || self.mouse_uncertain) {
                let (x, y) = self.mouse_position.context("Missing held mouse position")?;
                send_packet(self.runtime, &protocol::encode_mouse(true, 0, x, y, 0))?;
                self.mouse_buttons = 0;
                self.mouse_uncertain = false;
            }
            return Ok(());
        }
        let in_image = self.status.contains(x, y);
        let position = if in_image {
            Some((i16::try_from(x)?, i16::try_from(y)?))
        } else if buttons == 0 && (self.mouse_buttons != 0 || self.mouse_uncertain) {
            // A focus/drag release can land outside the image. Release at the
            // last validated active-pixel position; never scale to 0..32767.
            self.mouse_position
        } else {
            None
        };
        if let Some((x, y)) = position {
            if in_image {
                self.mouse_position = Some((x, y));
            }
            self.mouse_uncertain |= buttons != 0;
            send_packet(
                self.runtime,
                &protocol::encode_mouse(true, buttons, x, y, if in_image { wheel } else { 0 }),
            )?;
            self.mouse_buttons = buttons;
            self.mouse_uncertain = false;
        }
        Ok(())
    }

    fn cleanup(&mut self) -> Result<()> {
        self.cleanup_attempted = true;
        let release = self.release();
        // This executes even if redirect, construction, start, or release failed.
        let close = self
            .runtime
            .run("if globals().get('cart_session') is not None:\n    cart_session.close()")
            .context("Close video session");
        release.and(close)
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            let _ = self.cleanup();
        }
    }
}

struct HeldStroke {
    stroke: Stroke,
    release_at: Instant,
}

#[derive(Debug, Eq, PartialEq)]
enum PasteTick {
    Wait,
    Released,
    Complete,
}

struct Paste {
    scheduler: PasteScheduler,
    started: Instant,
    held: Option<HeldStroke>,
    pause: Duration,
    next_press_at: Option<Instant>,
    total: usize,
    done: usize,
}

impl Paste {
    fn new(plan: PastePlan, delay_ms: u64) -> Self {
        Self::new_at(plan, delay_ms, Instant::now())
    }

    fn new_at(plan: PastePlan, delay_ms: u64, started: Instant) -> Self {
        let total = plan.len();
        let pause = Duration::from_millis(delay_ms.clamp(50, 500));
        Self {
            scheduler: PasteScheduler::new(plan, pause),
            started,
            held: None,
            pause,
            next_press_at: None,
            total,
            done: 0,
        }
    }

    fn progress(&self, output: &Output, pasting: bool) -> Result<()> {
        output.status(json!({
            "paste_total": self.total, "paste_done": self.done, "pasting": pasting,
        }))
    }

    /// Each call either presses, releases, or waits. In particular, a release
    /// never falls through to a new press, even when the scheduler is overdue.
    /// The injected clock is sampled AFTER confirmed writes, so slow USB cannot
    /// consume either the 50 ms hold or the all-keys-up pause between strokes.
    fn advance(
        &mut self,
        mut now: impl FnMut() -> Instant,
        mut key: impl FnMut(u8, bool) -> Result<bool>,
    ) -> Result<PasteTick> {
        if let Some(held) = &self.held {
            if now() < held.release_at {
                return Ok(PasteTick::Wait);
            }
            let lateness = now().saturating_duration_since(held.release_at);
            if lateness.as_millis() >= 100 && std::env::var_os("CART_TRACE_IO").is_some() {
                eprintln!("Paste release overdue: {} ms", lateness.as_millis());
            }
            let stroke = held.stroke;
            if !key(stroke.usb_key, false)? {
                return Ok(PasteTick::Wait);
            }
            if stroke.modifiers & LEFT_SHIFT_MASK != 0 && !key(0xe1, false)? {
                return Ok(PasteTick::Wait);
            }
            self.next_press_at = Some(now() + self.pause);
            self.held = None;
            self.done += 1;
            return Ok(if self.done == self.total {
                PasteTick::Complete
            } else {
                PasteTick::Released
            });
        }
        if self.next_press_at.is_some_and(|deadline| now() < deadline) {
            return Ok(PasteTick::Wait);
        }
        match self
            .scheduler
            .poll(now().saturating_duration_since(self.started))
        {
            SchedulePoll::Ready(stroke) => {
                if stroke.modifiers & LEFT_SHIFT_MASK != 0 && !key(0xe1, true)? {
                    return Ok(PasteTick::Wait);
                }
                if !key(stroke.usb_key, true)? {
                    return Ok(PasteTick::Wait);
                }
                self.held = Some(HeldStroke {
                    stroke,
                    release_at: now() + KEY_HOLD,
                });
                Ok(PasteTick::Wait)
            }
            SchedulePoll::Wait(_) => Ok(PasteTick::Wait),
            SchedulePoll::Complete | SchedulePoll::Cancelled => Ok(PasteTick::Complete),
        }
    }

    /// The loop remains responsive to urgent epoch barriers throughout holds
    /// and release pauses; no sleeping or catch-up typing occurs inside a tick.
    fn tick(
        &mut self,
        session: &mut Session<'_>,
        output: &Output,
        control: &Control,
        epoch: u64,
    ) -> Result<bool> {
        if !control.current(epoch) {
            return Ok(false);
        }
        let step = self.advance(Instant::now, |usage, down| {
            session.paste_key(usage, down, control, epoch)
        })?;
        if step != PasteTick::Wait {
            self.progress(output, step != PasteTick::Complete)?;
        }
        Ok(step == PasteTick::Complete)
    }
}

fn cancel_paste(paste: &mut Option<Paste>, output: &Output) -> Result<()> {
    if let Some(mut current) = paste.take() {
        current.scheduler.cancel();
        current.progress(output, false)
    } else {
        output.status(json!({ "pasting": false }))
    }
}

fn publish_device_status(session: &Session<'_>, output: &Output) -> Result<()> {
    let status = session.status;
    output.status(json!({
        "state": status.state(),
        "message": if status.error { "The adapter reported an I/O error." }
            else if status.no_video && status.fpga_loaded { "Waiting for a video signal." }
            else if !status.fpga_loaded { "Waiting for the adapter to become ready." }
            else if !status.keyboard_ready { "Waiting for keyboard input to become ready." }
            else { "" },
        "keyboard_ready": status.ready(), "caps_lock": status.caps_lock,
        "keyboard_link": session.keyboard_link(),
        "width": status.width, "height": status.height, "hz": status.hz,
        "position_x": status.position_x, "position_y": status.position_y,
        "saved_position": status.saved_position, "aligning": status.aligning,
        "drive": session.drive.json(&session.drive_error),
    }))
}

fn run_loop(
    session: &mut Session<'_>,
    receiver: &Receiver<QueuedCommand>,
    control: &Control,
    output: &Output,
    paste: &mut Option<Paste>,
) -> Result<()> {
    let mut epoch = control.epoch.load(Ordering::Acquire);
    let mut next_status = Instant::now();
    let mut next_frame = Instant::now();
    loop {
        let latest = control.epoch.load(Ordering::Acquire);
        if latest != epoch {
            // Release precedes reporting failure: output can already be broken.
            session.wake_release = None;
            let release = session.release();
            let cancelled = cancel_paste(paste, output);
            epoch = latest;
            release?;
            cancelled?;
        }
        control.check_failure()?;
        if control.quitting.load(Ordering::Acquire) {
            return Ok(());
        }
        if Instant::now() >= next_status {
            let bytes = session.runtime.bytes("cart_session.poll_status()")?;
            let error = session.runtime.bytes("cart_session.error[:4096]")?;
            ensure!(error.is_empty(), "{}", String::from_utf8_lossy(&error));
            if !bytes.is_empty() {
                let status = DeviceStatus::parse(&bytes)?;
                ensure!(!status.error, "Video session reported an I/O error");
                if session.status_seen.is_none() || status.sequence != session.status.sequence {
                    session.status_seen = Some(Instant::now());
                }
                let lost_readiness = session.status.ready() && !status.ready();
                let changed_mode =
                    session.status.width != status.width || session.status.height != status.height;
                session.status = status;
                session.drive =
                    DriveStatus::parse(&session.runtime.bytes("cart_session.poll_drive()")?)?;
                session.drive_error = String::from_utf8_lossy(
                    &session.runtime.bytes("cart_session.drive_error[:512]")?,
                )
                .into_owned();
                if lost_readiness || changed_mode {
                    control.barrier();
                    // Do not accept input/frame work with a changed mode until
                    // the next turn has cancelled paste and released held keys.
                    continue;
                }
                publish_device_status(session, output)?;
            }
            next_status = Instant::now() + STATUS_INTERVAL;
        }
        if !control.current(epoch) {
            continue;
        }
        // Bound each batch so a flood cannot starve stroke releases or video.
        for _ in 0..32 {
            if !control.current(epoch) {
                break;
            }
            let queued = match receiver.try_recv() {
                Ok(queued) => queued,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    control.quit();
                    break;
                }
            };
            // Picture alignment is not target input: release barriers and
            // pastes do not discard it. Everything else is fenced as before.
            // The virtual drive is not keyboard or mouse input either.
            let display = matches!(
                queued.command,
                Command::AlignAuto {}
                    | Command::AlignReset {}
                    | Command::Positions { .. }
                    | Command::DriveAttach { .. }
                    | Command::DriveDetach {}
            );
            if !display && (queued.epoch != epoch || !control.current(queued.epoch)) {
                continue;
            }
            // Manual input and a second paste cannot interleave with an active
            // paste. Completion also fences everything queued during this one.
            if !display && paste.is_some() {
                continue;
            }
            match queued.command {
                Command::Key { usage, down } => {
                    if session.status.ready() {
                        session.key(usage, down, control, epoch)?;
                    }
                }
                Command::Mouse {
                    x,
                    y,
                    buttons,
                    wheel,
                } => {
                    session.mouse(x, y, buttons, wheel, control, epoch)?;
                }
                Command::Paste { text, delay_ms } => {
                    let planned = if !session.status.ready() {
                        Err("Keyboard and video must be ready before pasting.")
                    } else if !session.input.is_empty() || session.mouse_buttons != 0 {
                        Err("Release held keys and mouse buttons before pasting.")
                    } else {
                        // Whole-string validation precedes the first packet.
                        // Error messages never contain the text.
                        PastePlan::new(&text, session.status.caps_lock).map_err(
                            |_| "Paste supports US-layout printable ASCII, Tab, and Enter only.",
                        )
                    };
                    match planned {
                        Ok(plan) => {
                            let current = Paste::new(plan, delay_ms);
                            current.progress(output, true)?;
                            *paste = Some(current);
                        }
                        Err(message) => {
                            // Focus may have changed between preview and start.
                            // Release known state and reject without partial text.
                            let release = session.release();
                            output.status(json!({ "message": message, "pasting": false }))?;
                            control.barrier();
                            release?;
                        }
                    }
                }
                Command::AlignAuto {} | Command::AlignReset {} => {
                    if session.status.video_ready() {
                        let automatic = matches!(queued.command, Command::AlignAuto {});
                        session
                            .runtime
                            .run(&format!(
                                "cart_session.default_position({})",
                                if automatic { "True" } else { "False" }
                            ))
                            .context("Picture alignment failed")?;
                    }
                }
                Command::Positions { positions } => {
                    session
                        .runtime
                        .run(&positions_source(&positions))
                        .context("Saved picture positions were rejected")?;
                }
                Command::Wake {} => session.wake(control, epoch)?,
                // Open or format problems become drive_error, not helper faults.
                Command::DriveAttach { path, format } => session
                    .runtime
                    .run(&drive_attach_source(&path, format))
                    .context("Disk image attach failed")?,
                Command::DriveDetach {} => session
                    .runtime
                    .run("cart_session.drive_detach()")
                    .context("Drive detach failed")?,
                // Urgent commands are consumed exclusively by the reader.
                Command::Release {} | Command::Cancel {} | Command::Quit {} => unreachable!(),
            }
        }
        if !control.current(epoch) {
            continue;
        }
        session.finish_wake(control, epoch)?;
        if let Some(current) = paste.as_mut()
            && current.tick(session, output, control, epoch)?
        {
            *paste = None;
            control.barrier();
            continue;
        }
        if !control.current(epoch) {
            continue;
        }
        if Instant::now() >= next_frame {
            let frame = session.runtime.bytes("cart_session.poll_frame()")?;
            if let Some(record) = Record::frame(frame)? {
                // A completed old-mode frame can race with Python's status
                // worker. Validate it, but do not publish it as the new mode.
                if session.status.video_ready()
                    && record.width == session.status.width
                    && record.height == session.status.height
                    && control.current(epoch)
                {
                    output.frame(record);
                }
            }
            next_frame = Instant::now() + FRAME_INTERVAL;
        }
        // All Python calls have returned; its GIL guard releases during sleep.
        thread::sleep(LOOP_PAUSE);
    }
}

fn keyboard_link(status: DeviceStatus, seen: Option<Instant>) -> bool {
    status.keyboard_ready
        && status.fpga_loaded
        && !status.error
        && seen.is_some_and(|seen| seen.elapsed() <= STATUS_FRESH)
}

/// Only typed integers enter this Python source.
fn positions_source(positions: &[SavedPosition]) -> String {
    let mut source = String::from("cart_session.set_preferred([");
    for position in positions {
        let _ = write!(
            source,
            "({}, {}, {}, {}, {}),",
            position.width, position.height, position.hz, position.x, position.y
        );
    }
    source.push_str("])");
    source
}

/// Serve one private inherited stdin/stdout connection; no sockets or listeners.
/// The runtime and all input state remain on this thread for the entire session.
pub fn serve(runtime: &crate::python::Python) -> Result<()> {
    let control = Arc::new(Control::default());
    let output = Output {
        queue: Arc::new((Mutex::new(PendingOutput::default()), Condvar::new())),
        control: control.clone(),
    };
    let (sender, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    // Construct the cleanup guard before any fallible Python startup operation.
    let mut session = Session::new(runtime);
    let mut paste = None;
    let mut writer_thread = None;
    let result = (|| -> Result<()> {
        let writer_queue = output.queue.clone();
        let writer_control = control.clone();
        writer_thread = Some(
            thread::Builder::new()
                .name("cart-stdout".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(|| {
                        let stdout = io::stdout();
                        write_records(stdout.lock(), &writer_queue, &writer_control);
                    });
                    if result.is_err() {
                        writer_control.fail(OUTPUT_IO);
                    }
                })
                .context("Start private output worker")?,
        );
        let reader_control = control.clone();
        thread::Builder::new()
            .name("cart-stdin".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(|| {
                    let stdin = io::stdin();
                    read_commands(stdin.lock(), sender, &reader_control);
                });
                if result.is_err() {
                    reader_control.fail(INPUT_IO);
                }
            })
            .context("Start private command worker")?;
        output.status(json!({
            "state": "connecting", "message": "Connecting to the crash cart adapter.",
            "keyboard_ready": false, "pasting": false,
        }))?;
        // Python/vendor diagnostics must never enter the binary stdout stream.
        runtime.run("import sys\nsys.stdout = sys.stderr\ncart_session = None")?;
        control.check_failure()?;
        if control.quitting.load(Ordering::Acquire) {
            return Ok(());
        }
        runtime.run("cart_session = VideoSession(); cart_session.start()")?;
        run_loop(&mut session, &receiver, &control, &output, &mut paste)
    })();
    control.quit();
    // Do not let a failed status write bypass key/button cleanup or close().
    if let Some(mut current) = paste.take() {
        current.scheduler.cancel();
        let _ = current.progress(&output, false);
    }
    let cleanup = session.cleanup();
    let result = match (result, cleanup) {
        (Ok(()), Ok(())) => control.check_failure(),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("Cleanup also failed: {cleanup:#}")))
        }
    };
    let message = result.as_ref().err().map(|error| format!("{error:#}"));
    output.finish(message.as_deref());
    // A stuck inherited output pipe must not prevent device cleanup or helper
    // exit. Give final status a bounded opportunity, never join a blocked writer.
    if let Some(worker) = writer_thread {
        let deadline = Instant::now() + Duration::from_millis(100);
        while !worker.is_finished() && Instant::now() < deadline {
            thread::sleep(LOOP_PAUSE);
        }
        if worker.is_finished() {
            let _ = worker.join();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn no_signal_is_distinct_from_startup_and_device_failure() {
        let startup = DeviceStatus::default();
        assert_eq!(startup.state(), "connecting");
        let no_signal = DeviceStatus {
            fpga_loaded: true,
            no_video: true,
            ..startup
        };
        assert_eq!(no_signal.state(), "no_video");
        assert!(!no_signal.ready());
        let video = DeviceStatus {
            width: 1920,
            height: 1080,
            fpga_loaded: true,
            ..startup
        };
        assert_eq!(video.state(), "viewing");
        assert!(!video.ready());
        assert_eq!(
            DeviceStatus {
                error: true,
                ..no_signal
            }
            .state(),
            "error"
        );
    }

    #[test]
    fn header_matches_private_cart_contract() {
        let record = Record::frame(
            [
                1_u32.to_le_bytes().as_slice(),
                1_u32.to_le_bytes().as_slice(),
                &[1, 2, 3, 4],
            ]
            .concat(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            record.header(0x12345678),
            [
                b'C', b'A', b'R', b'T', 1, 0, 1, 0, 0x78, 0x56, 0x34, 0x12, 1, 0, 0, 0, 1, 0, 0, 0,
                4, 0, 0, 0,
            ]
        );
        assert_eq!(record.payload(), [1, 2, 3, 4]);
        let status = Record::status(json!({ "pasting": false })).unwrap();
        assert_eq!(&status.header(2)[6..8], &2_u16.to_le_bytes());
        assert_eq!(&status.header(2)[12..20], &[0; 8]);
    }

    #[test]
    fn frames_require_exact_checked_active_layout() {
        assert!(Record::frame(Vec::new()).unwrap().is_none());
        for (width, height, length) in [
            (0_u32, 1_u32, 4),
            (1921, 1, 4),
            (1, 1601, 4),
            (1, 1, 3),
            (1, 1, 5),
            (u32::MAX, u32::MAX, 0),
        ] {
            let mut bytes = width.to_le_bytes().to_vec();
            bytes.extend(height.to_le_bytes());
            bytes.resize(8 + length, 0);
            assert!(Record::frame(bytes).is_err());
        }
        assert!(Record::frame(vec![0; 7]).is_err());
    }

    #[test]
    fn commands_are_typed_bounded_and_do_not_echo_text() {
        for line in [
            r#"{"type":"key","usage":256,"down":true}"#,
            r#"{"type":"key","usage":0,"down":true}"#,
            r#"{"type":"key","usage":4,"down":1}"#,
            r#"{"type":"mouse","x":0,"y":0,"buttons":8,"wheel":0}"#,
            r#"{"type":"mouse","x":0,"y":0,"buttons":0,"wheel":-32768}"#,
            r#"{"type":"paste","text":"secret","delay_ms":-1}"#,
            r#"{"type":"release","extra":"secret"}"#,
        ] {
            assert!(parse_command(line.as_bytes()).is_err());
            assert!(
                !parse_command(line.as_bytes())
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("secret")
            );
        }
        assert!(
            parse_command(br#"{"type":"mouse","x":-1,"y":-2,"buttons":0,"wheel":32767}"#).is_ok()
        );
        assert!(
            parse_command(br#"{"type":"paste","text":"","delay_ms":18446744073709551615}"#).is_ok()
        );
        for count in [MAX_PASTE_CHARS, MAX_PASTE_CHARS + 1] {
            let line = serde_json::to_vec(
                &json!({ "type": "paste", "text": "a".repeat(count), "delay_ms": 50 }),
            )
            .unwrap();
            assert_eq!(parse_command(&line).is_ok(), count == MAX_PASTE_CHARS);
        }
    }

    #[test]
    fn alignment_commands_are_bounded_and_format_only_integers() {
        for line in [
            r#"{"type":"nudge","dx":1,"dy":0}"#,
            r#"{"type":"align_auto","save":true}"#,
            r#"{"type":"positions","positions":[{"width":1921,"height":1080,"hz":60,"x":0,"y":0}]}"#,
            r#"{"type":"positions","positions":[{"width":1920,"height":1080,"hz":0,"x":0,"y":0}]}"#,
            r#"{"type":"positions","positions":[{"width":1920,"height":1080,"hz":60,"x":200,"y":0}]}"#,
            r#"{"type":"wake","usage":4}"#,
        ] {
            assert!(parse_command(line.as_bytes()).is_err(), "{line}");
        }
        assert!(parse_command(br#"{"type":"align_auto"}"#).is_ok());
        assert!(parse_command(br#"{"type":"align_reset"}"#).is_ok());
        assert!(parse_command(br#"{"type":"wake"}"#).is_ok());
        let many = json!({ "type": "positions", "positions": vec![
            json!({"width": 1920, "height": 1080, "hz": 60, "x": -45, "y": 0});
            MAX_SAVED_POSITIONS + 1
        ]});
        assert!(parse_command(&serde_json::to_vec(&many).unwrap()).is_err());
        let Command::Positions { positions } = parse_command(
            br#"{"type":"positions","positions":[{"width":1920,"height":1080,"hz":60,"x":-45,"y":-1}]}"#,
        )
        .unwrap() else {
            panic!("positions command");
        };
        assert_eq!(
            positions_source(&positions),
            "cart_session.set_preferred([(1920, 1080, 60, -45, -1),])"
        );
    }

    #[test]
    fn drive_commands_are_typed_and_paths_enter_python_as_hex() {
        for line in [
            r#"{"type":"drive_attach","path":"relative.img","format":"raw"}"#,
            r#"{"type":"drive_attach","path":"C:\\a.img","format":"vhdx"}"#,
            r#"{"type":"drive_attach","path":"C:\\a.img"}"#,
            r#"{"type":"drive_attach","path":"C:\\a\u0000.img","format":"raw"}"#,
            r#"{"type":"drive_attach","path":"C:\\a.img","format":"raw","read_only":true}"#,
            r#"{"type":"drive_detach","force":true}"#,
        ] {
            assert!(parse_command(line.as_bytes()).is_err(), "{line}");
        }
        let long = json!({ "type": "drive_attach", "format": "raw",
            "path": format!("C:\\{}", "a".repeat(MAX_DRIVE_PATH)) });
        assert!(parse_command(&serde_json::to_vec(&long).unwrap()).is_err());
        assert!(parse_command(br#"{"type":"drive_detach"}"#).is_ok());
        let Command::DriveAttach { path, format } = parse_command(
            r#"{"type":"drive_attach","path":"C:\\Disks\\it's \"é\".iso","format":"iso"}"#
                .as_bytes(),
        )
        .unwrap() else {
            panic!("drive command");
        };
        let source = drive_attach_source(&path, format);
        assert_eq!(
            source,
            "cart_session.drive_attach('433a5c4469736b735c697427732022c3a9222e69736f', 'iso')"
        );
        assert!(
            source
                .trim_start_matches("cart_session.drive_attach('")
                .split('\'')
                .next()
                .unwrap()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );
    }

    #[test]
    fn drive_status_layout_matches_python_and_rejects_impossible_media() {
        let mut bytes = vec![1, 1, 2, 1, 0, 0x34, 0x12];
        for value in [700_u64 << 20, 4096, 1 << 33] {
            bytes.extend(value.to_le_bytes());
        }
        let status = DriveStatus::parse(&bytes).unwrap();
        assert_eq!(
            status,
            DriveStatus {
                supported: true,
                attached: true,
                media: 2,
                locked: true,
                ejected: false,
                sequence: 0x1234,
                size: 700 << 20,
                read: 4096,
                written: 1 << 33,
            }
        );
        let value = status.json("");
        assert_eq!(value["media"], "cdrom");
        assert!(value["error"].is_null());
        assert_eq!(DriveStatus::default().json("Gone")["error"], "Gone");
        for (index, byte) in [(1, 0), (2, 3)] {
            let mut bad = bytes.clone();
            bad[index] = byte;
            assert!(DriveStatus::parse(&bad).is_err());
        }
        assert!(DriveStatus::parse(&bytes[..30]).is_err());
    }

    #[test]
    fn keyboard_link_needs_fresh_status_but_not_video() {
        let status = DeviceStatus {
            keyboard_ready: true,
            fpga_loaded: true,
            no_video: true,
            ..DeviceStatus::default()
        };
        assert!(!status.ready());
        let fresh = |seen, status| keyboard_link(status, seen);
        assert!(fresh(Some(Instant::now()), status));
        assert!(!fresh(None, status));
        assert!(!fresh(
            Instant::now().checked_sub(Duration::from_secs(3)),
            status
        ));
        assert!(!fresh(
            Some(Instant::now()),
            DeviceStatus {
                error: true,
                ..status
            }
        ));
        assert!(!fresh(
            Some(Instant::now()),
            DeviceStatus {
                keyboard_ready: false,
                ..status
            }
        ));
    }

    #[test]
    fn bounded_reader_rejects_long_and_partial_lines() {
        let mut line = Vec::new();
        let mut exact = Cursor::new([vec![b' '; MAX_LINE], b"\n".to_vec()].concat());
        assert!(read_line_bounded(&mut exact, &mut line).unwrap());
        assert_eq!(line.len(), MAX_LINE);
        assert!(!read_line_bounded(&mut exact, &mut line).unwrap());
        for bytes in [vec![b' '; MAX_LINE + 1], b"{\"type\":\"release\"}".to_vec()] {
            assert!(read_line_bounded(&mut Cursor::new(bytes), &mut line).is_err());
            assert!(line.len() <= MAX_LINE);
        }
    }

    #[test]
    fn urgent_barriers_bypass_full_queue_and_invalidate_stale_input() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let control = Control::default();
        let input = b"{\"type\":\"key\",\"usage\":4,\"down\":true}\n{\"type\":\"cancel\"}\n{\"type\":\"release\"}\n{\"type\":\"quit\"}\n";
        read_commands(Cursor::new(input), sender, &control);
        let queued = receiver.try_recv().unwrap();
        assert_eq!(queued.epoch, 0);
        assert!(!control.current(queued.epoch));
        assert_eq!(control.epoch.load(Ordering::Acquire), 3);
        assert_eq!(control.failure.load(Ordering::Acquire), 0);
    }

    #[test]
    fn ordinary_queue_overflow_fails_closed() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let control = Control::default();
        let input = b"{\"type\":\"key\",\"usage\":4,\"down\":true}\n{\"type\":\"key\",\"usage\":4,\"down\":false}\n";
        read_commands(Cursor::new(input), sender, &control);
        assert_eq!(control.failure.load(Ordering::Acquire), INPUT_OVERFLOW);
        assert!(control.quitting.load(Ordering::Acquire));
    }

    #[test]
    fn status_byte_offsets_caps_and_pixel_bounds_are_exact() {
        let status = DeviceStatus::parse(&[
            0x80, 7, 0x38, 4, 60, 1, 2, 1, 0, 0, 0xd3, 2, 0, 1, 0x34, 0x12,
        ])
        .unwrap();
        assert_eq!((status.width, status.height, status.hz), (1920, 1080, 60));
        assert_eq!((status.position_x, status.position_y), (-45, 2));
        assert!(!status.saved_position && status.aligning);
        assert_eq!(status.sequence, 0x1234);
        assert!(status.ready());
        assert!(status.caps_lock);
        assert!(status.contains(1919, 1079));
        assert!(!status.contains(1920, 1079));
        assert!(!status.contains(-1, 0));
        let status =
            DeviceStatus::parse(&[1, 0, 1, 0, 60, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert!(!status.ready());
        assert!(!status.caps_lock);
        assert!(DeviceStatus::parse(&[0; 10]).is_err());
    }

    #[test]
    fn input_state_commits_only_confirmed_writes() {
        let mut input = InputState::new();
        assert!(transition_key(&mut input, 4, true, |_| bail!("write failed")).is_err());
        assert!(input.is_empty());
        transition_key(&mut input, 4, true, |_| Ok(())).unwrap();
        assert!(transition_key(&mut input, 4, false, |_| bail!("write failed")).is_err());
        assert!(input.held_keys().contains(&4));
        transition_key(&mut input, 4, false, |_| Ok(())).unwrap();
        assert!(input.is_empty());
    }

    #[test]
    fn latest_frame_replacement_does_not_mix_record_bytes() {
        let control = Arc::new(Control::default());
        let output = Output {
            queue: Arc::new((Mutex::new(PendingOutput::default()), Condvar::new())),
            control: control.clone(),
        };
        for value in [1_u8, 9] {
            output.frame(
                Record::frame(
                    [
                        1_u32.to_le_bytes().as_slice(),
                        1_u32.to_le_bytes().as_slice(),
                        &[value; 4],
                    ]
                    .concat(),
                )
                .unwrap()
                .unwrap(),
            );
        }
        output.status(json!({ "pasting": false })).unwrap();
        output.queue.0.lock().unwrap().finished = true;
        let mut bytes = Vec::new();
        write_records(&mut bytes, &output.queue, &control);
        let status_len = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
        let frame_offset = HEADER_LEN + status_len;
        assert_eq!(&bytes[frame_offset..frame_offset + 4], b"CART");
        assert_eq!(&bytes[frame_offset + HEADER_LEN..], &[9; 4]);
        assert_eq!(
            u32::from_le_bytes(
                bytes[frame_offset + 8..frame_offset + 12]
                    .try_into()
                    .unwrap()
            ),
            2
        );
    }

    #[test]
    fn broken_output_requests_urgent_shutdown() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let control = Control::default();
        let queue = Arc::new((Mutex::new(PendingOutput::default()), Condvar::new()));
        queue
            .0
            .lock()
            .unwrap()
            .statuses
            .push_back(Record::status(json!({ "pasting": false })).unwrap());
        write_records(Broken, &queue, &control);
        assert_eq!(control.failure.load(Ordering::Acquire), OUTPUT_IO);
        assert!(!control.current(0));
    }

    struct PasteRig {
        clock: std::cell::Cell<Instant>,
        input: InputState,
        // Write start, confirmed completion, packet. No actual I/O occurs.
        packets: Vec<(Instant, Instant, [u8; 5])>,
        usb_delay: Duration,
    }

    impl PasteRig {
        fn new(now: Instant) -> Self {
            Self {
                clock: std::cell::Cell::new(now),
                input: InputState::new(),
                packets: Vec::new(),
                usb_delay: Duration::ZERO,
            }
        }

        fn tick(&mut self, paste: &mut Paste) -> PasteTick {
            let clock = &self.clock;
            let delay = self.usb_delay;
            paste
                .advance(
                    || clock.get(),
                    |usage, down| {
                        transition_key(&mut self.input, usage, down, |packet| {
                            let started = clock.get();
                            clock.set(started + delay);
                            self.packets
                                .push((started, clock.get(), packet.try_into().unwrap()));
                            Ok(())
                        })?;
                        Ok(true)
                    },
                )
                .unwrap()
        }
    }

    #[test]
    fn physical_wire_meta_is_zero_while_logical_shift_remains_tracked() {
        let mut input = InputState::new();
        let mut packets = Vec::new();
        for (usage, down, logical_mask) in [
            (0xe1, true, 2),
            (4, true, 2),
            (4, false, 2),
            (0xe1, false, 0),
        ] {
            transition_key(&mut input, usage, down, |packet| {
                packets.push(packet.to_vec());
                Ok(())
            })
            .unwrap();
            assert_eq!(input.modifiers(), logical_mask);
        }
        assert_eq!(
            packets,
            [
                vec![b'k', 0xe1, 0, 1, 0],
                vec![b'k', 4, 0, 1, 0],
                vec![b'k', 4, 0, 0, 0],
                vec![b'k', 0xe1, 0, 0, 1],
            ]
        );
        assert!(input.is_empty());
    }

    #[test]
    fn overdue_release_never_presses_again_in_the_same_tick() {
        let base = Instant::now();
        let mut paste = Paste::new_at(PastePlan::new("aa", false).unwrap(), 120, base);
        let mut rig = PasteRig::new(base);
        rig.usb_delay = Duration::from_millis(30);
        assert_eq!(rig.tick(&mut paste), PasteTick::Wait);
        assert_eq!(
            paste.held.as_ref().unwrap().release_at,
            rig.clock.get() + KEY_HOLD
        );
        // Both the scheduler interval and hold deadline are very overdue.
        rig.clock.set(base + Duration::from_secs(5));
        let before_release = rig.packets.len();
        assert_eq!(rig.tick(&mut paste), PasteTick::Released);
        assert_eq!(rig.packets.len(), before_release + 1);
        assert_eq!(rig.packets.last().unwrap().2, [b'k', 4, 0, 0, 1]);
        let confirmed_up = rig.clock.get();
        let deadline = confirmed_up + Duration::from_millis(120);
        assert_eq!(paste.next_press_at, Some(deadline));
        assert_eq!(rig.tick(&mut paste), PasteTick::Wait);
        rig.clock.set(deadline - Duration::from_nanos(1));
        assert_eq!(rig.tick(&mut paste), PasteTick::Wait);
        assert_eq!(rig.packets.len(), before_release + 1);
        rig.clock.set(deadline);
        assert_eq!(rig.tick(&mut paste), PasteTick::Wait);
        let next_down = rig.packets.last().unwrap();
        assert_eq!(next_down.2, [b'k', 4, 0, 1, 0]);
        assert_eq!(
            next_down.0.duration_since(confirmed_up),
            Duration::from_millis(120)
        );
    }

    #[test]
    fn release_pause_starts_after_confirmed_shift_up_not_before_usb_writes() {
        let base = Instant::now();
        let mut paste = Paste::new_at(PastePlan::new("Aa", false).unwrap(), 80, base);
        let mut rig = PasteRig::new(base);
        rig.usb_delay = Duration::from_millis(70);
        rig.tick(&mut paste);
        assert_eq!(rig.input.modifiers(), 2);
        rig.clock.set(base + Duration::from_secs(3));
        let count = rig.packets.len();
        assert_eq!(rig.tick(&mut paste), PasteTick::Released);
        assert_eq!(rig.packets.len(), count + 2);
        assert_eq!(rig.packets[count].2, [b'k', 4, 0, 0, 0]);
        let final_up = rig.packets[count + 1];
        assert_eq!(final_up.2, [b'k', 0xe1, 0, 0, 1]);
        assert!(rig.input.is_empty());
        let deadline = final_up.1 + Duration::from_millis(80);
        assert_eq!(paste.next_press_at, Some(deadline));
        rig.clock.set(deadline - Duration::from_nanos(1));
        rig.tick(&mut paste);
        assert_eq!(rig.packets.len(), count + 2);
        rig.clock.set(deadline);
        rig.tick(&mut paste);
        assert_eq!(rig.packets.last().unwrap().2, [b'k', 4, 0, 1, 0]);
        assert_eq!(rig.packets.last().unwrap().0, deadline);
    }

    #[test]
    fn repeated_usages_with_shift_changes_keep_configured_all_up_gaps() {
        for text in ["1!1!", "[{[{"] {
            for pause_ms in [50, 200, 500] {
                let base = Instant::now();
                let mut paste = Paste::new_at(PastePlan::new(text, false).unwrap(), pause_ms, base);
                let mut rig = PasteRig::new(base);
                let mut previous_up: Option<Instant> = None;
                for index in 0..4 {
                    rig.usb_delay = Duration::from_millis(13 + index * 31);
                    if let Some(up) = previous_up {
                        let deadline = up + Duration::from_millis(pause_ms);
                        rig.clock.set(deadline - Duration::from_nanos(1));
                        let count = rig.packets.len();
                        assert_eq!(rig.tick(&mut paste), PasteTick::Wait);
                        assert_eq!(rig.packets.len(), count);
                        rig.clock.set(deadline);
                    }
                    let first_down = rig.packets.len();
                    rig.tick(&mut paste);
                    assert_eq!(rig.packets[first_down].2[3], 1);
                    if let Some(up) = previous_up {
                        assert!(
                            rig.packets[first_down].0.duration_since(up)
                                >= Duration::from_millis(pause_ms)
                        );
                    }
                    let release_at = paste.held.as_ref().unwrap().release_at;
                    rig.clock.set(release_at + Duration::from_millis(700));
                    let first_up = rig.packets.len();
                    assert_eq!(
                        rig.tick(&mut paste),
                        if index == 3 {
                            PasteTick::Complete
                        } else {
                            PasteTick::Released
                        }
                    );
                    assert!(
                        rig.packets[first_up..]
                            .iter()
                            .all(|record| record.2[3] == 0)
                    );
                    assert!(rig.input.is_empty());
                    let final_up = rig.packets.last().unwrap();
                    assert_eq!(final_up.2[4], 1);
                    previous_up = Some(final_up.1);
                    assert_eq!(
                        paste.next_press_at,
                        Some(final_up.1 + Duration::from_millis(pause_ms))
                    );
                }
                assert_eq!(paste.done, 4);
                assert!(rig.packets.iter().all(|record| record.2[2] == 0));
                let ordinary_downs: Vec<_> = rig
                    .packets
                    .iter()
                    .filter(|record| record.2[1] != 0xe1 && record.2[3] == 1)
                    .collect();
                assert_eq!(ordinary_downs.len(), 4);
                assert!(
                    ordinary_downs
                        .iter()
                        .all(|record| record.2[1] == ordinary_downs[0].2[1])
                );
            }
        }
    }

    #[test]
    fn failed_shift_release_keeps_held_state_and_does_not_start_pause() {
        let base = Instant::now();
        let clock = std::cell::Cell::new(base);
        let mut paste = Paste::new_at(PastePlan::new("Aa", false).unwrap(), 100, base);
        let mut input = InputState::new();
        paste
            .advance(
                || clock.get(),
                |usage, down| {
                    transition_key(&mut input, usage, down, |_| Ok(()))?;
                    Ok(true)
                },
            )
            .unwrap();
        clock.set(paste.held.as_ref().unwrap().release_at);
        let result = paste.advance(
            || clock.get(),
            |usage, down| {
                transition_key(&mut input, usage, down, |_| {
                    if usage == 0xe1 && !down {
                        bail!("Shift-up write failed");
                    }
                    Ok(())
                })?;
                Ok(true)
            },
        );
        assert!(result.is_err());
        assert!(input.held_keys().contains(&0xe1));
        assert!(!input.held_keys().contains(&4));
        assert!(paste.held.is_some());
        assert!(paste.next_press_at.is_none());
        assert_eq!(paste.done, 0);
    }

    #[test]
    fn paste_is_prevalidated_and_delay_is_clamped() {
        assert!(PastePlan::new("valid prefix\u{20ac}", false).is_err());
        for delay in [0, 49, 50, 500, u64::MAX] {
            let mut paste = Paste::new(PastePlan::new("ab", false).unwrap(), delay);
            assert!(matches!(
                paste.scheduler.poll(Duration::ZERO),
                SchedulePoll::Ready(_)
            ));
            let expected = Duration::from_millis(delay.clamp(50, 500));
            assert_eq!(paste.pause, expected);
            assert_eq!(
                paste.scheduler.poll(Duration::ZERO),
                SchedulePoll::Wait(expected)
            );
        }
    }
}
