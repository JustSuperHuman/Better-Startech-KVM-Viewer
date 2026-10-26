//! The virtual USB drive, like the original Disk Control window: files are
//! copied onto a shared FAT32 drive and plugged into the target, or a disk
//! image is attached as-is. Files the target saved can be copied back.
use super::*;
use crate::{
    drive::{self, ImageFormat, Listing, Progress, SharedDrive},
    transport::DriveStatus,
};
use std::{
    sync::{atomic::Ordering, mpsc},
    thread::JoinHandle,
};

const GOOD: Color32 = Color32::from_rgb(153, 207, 176);
const WARN: Color32 = Color32::from_rgb(218, 188, 126);
const BAD: Color32 = Color32::from_rgb(240, 165, 157);
/// How long the helper may take to answer an attach or unplug.
const DRIVE_ANSWER: Duration = Duration::from_secs(6);

#[derive(Clone, Copy)]
pub(super) enum Picker {
    Files,
    Folder,
    Image,
    SaveTo,
}

enum Picked {
    Send(Vec<PathBuf>),
    Image(PathBuf),
    SaveTo(PathBuf),
}

#[derive(Clone)]
enum Plan {
    Send(Vec<PathBuf>),
    Attach(PathBuf, ImageFormat),
    Save(PathBuf),
    Unplug,
    Erase,
}

enum Done {
    Sent(u64),
    Saved(PathBuf, u64),
    Listed(Listing),
    Erased,
}

struct Job {
    progress: Arc<Progress>,
    handle: JoinHandle<Result<Done>>,
    /// Shown while it runs: "Copying…", "Saving…".
    verb: &'static str,
}

struct Plugged {
    name: String,
    shared: bool,
}

struct Notice {
    text: String,
    error: bool,
    folder: Option<PathBuf>,
}

#[derive(Default)]
pub(super) struct DriveState {
    shared_path: Option<PathBuf>,
    plugged: Option<Plugged>,
    listing: Option<Listing>,
    listing_failed: bool,
    job: Option<Job>,
    /// Work that runs once the helper confirms the drive is unplugged.
    pending: Option<Plan>,
    /// The helper's drive sequence when the last command was sent.
    awaiting: Option<(u16, Instant, Option<Plugged>)>,
    /// A plan that needs "Unplug anyway" because the target holds the drive.
    confirm: Option<Plan>,
    confirm_erase: bool,
    notice: Option<Notice>,
    picker: Option<mpsc::Receiver<Option<Picked>>>,
    pub(super) hovering: bool,
    pub(super) dropped: Vec<PathBuf>,
    was_attached: bool,
    rate_sample: Option<(Instant, u64)>,
    speed: f64,
    /// Demo mode shows a plugged-in drive without touching any file or device.
    demo: Option<DriveStatus>,
    /// Smoke tests can open the panel without moving the pointer.
    pub(super) open_once: bool,
    smoke_started: bool,
}

impl DriveState {
    pub(super) fn new(demo: bool) -> Self {
        if !demo {
            return Self {
                shared_path: SharedDrive::default_location(),
                ..Self::default()
            };
        }
        let entry = |name: &str, size: u64, is_dir: bool| drive::Entry {
            name: name.into(),
            size,
            is_dir,
        };
        Self {
            plugged: Some(Plugged {
                name: "Shared drive".into(),
                shared: true,
            }),
            listing: Some(Listing {
                entries: vec![
                    entry("Drivers", 41_943_040, true),
                    entry("firmware-update.bin", 8_388_608, false),
                    entry("network-config.txt", 2_310, false),
                    entry("setup-notes.pdf", 1_258_291, false),
                ],
                files: 9,
                used: 52_428_800,
                capacity: 803_209_216,
            }),
            demo: Some(DriveStatus {
                supported: true,
                attached: true,
                media: Some("disk".into()),
                size: 805_306_368,
                read: 50_331_648,
                written: 3_145_728,
                ..DriveStatus::default()
            }),
            was_attached: true,
            ..Self::default()
        }
    }
}

impl App {
    fn drive_status(&self) -> Option<&DriveStatus> {
        if self.options.demo {
            return self.drive.demo.as_ref();
        }
        self.helper
            .as_ref()
            .filter(|_| !self.helper_fault)
            .and(self.status.drive.as_ref())
    }

    fn drive_connected(&self) -> bool {
        if self.options.demo {
            return true;
        }
        (!self.options.smoke_test || self.options.smoke_attach.is_some())
            && self.drive_status().is_some_and(|drive| drive.supported)
            && matches!(self.status.state.as_deref(), Some("viewing" | "no_video"))
    }

    fn drive_busy(&self) -> bool {
        let drive = &self.drive;
        drive.job.is_some()
            || drive.pending.is_some()
            || drive.awaiting.is_some()
            || drive.picker.is_some()
    }

    fn shared(&self) -> Option<SharedDrive> {
        self.drive.shared_path.clone().map(SharedDrive::new)
    }

    pub(super) fn drive_attached(&self) -> bool {
        self.drive_status().is_some_and(|drive| drive.attached)
    }

    fn notify(&mut self, text: impl Into<String>, error: bool) {
        self.drive.notice = Some(Notice {
            text: text.into(),
            error,
            folder: None,
        });
    }

    pub(super) fn open_picker(&mut self, picker: Picker) {
        if self.drive.picker.is_some() {
            return;
        }
        let window = self.window.clone();
        let (sender, receiver) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("cart-file-dialog".into())
            .spawn(move || {
                let mut dialog = rfd::FileDialog::new();
                if let Some(window) = &window {
                    dialog = dialog.set_parent(&**window);
                }
                let picked = match picker {
                    Picker::Files => dialog
                        .set_title("Send files to the target")
                        .pick_files()
                        .map(Picked::Send),
                    Picker::Folder => dialog
                        .set_title("Send a folder to the target")
                        .pick_folder()
                        .map(|folder| Picked::Send(vec![folder])),
                    Picker::Image => dialog
                        .set_title("Plug in a disk image")
                        .add_filter(
                            "Disk images",
                            &["img", "iso", "vhd", "ima", "bin", "raw", "dd", "dsk"],
                        )
                        .add_filter("All files", &["*"])
                        .pick_file()
                        .map(Picked::Image),
                    Picker::SaveTo => dialog
                        .set_title("Save the drive's files into…")
                        .pick_folder()
                        .map(Picked::SaveTo),
                };
                let _ = sender.send(picked);
            });
        match spawned {
            Ok(_) => self.drive.picker = Some(receiver),
            Err(error) => self.notify(format!("The file picker could not open: {error}"), true),
        }
    }

    /// Entry point for every drive action; unplugs first when it must.
    fn request(&mut self, plan: Plan, confirmed: bool) {
        self.drive.confirm = None;
        self.drive.confirm_erase = false;
        if self.options.demo {
            self.notify("Demo mode: nothing is copied or sent to a target.", false);
            return;
        }
        let needs_connection = matches!(plan, Plan::Send(_) | Plan::Attach(..) | Plan::Unplug);
        if needs_connection && !self.drive_connected() {
            self.notify("Connect the adapter to plug in a drive.", true);
            return;
        }
        let attached = self.drive_attached();
        let shared_plugged = self
            .drive
            .plugged
            .as_ref()
            .is_some_and(|plugged| plugged.shared);
        let unplug_first = attached
            && match plan {
                Plan::Send(_) | Plan::Unplug => true,
                Plan::Save(_) | Plan::Erase => shared_plugged,
                Plan::Attach(..) => false,
            };
        let replaces = attached && matches!(plan, Plan::Attach(..));
        if (unplug_first || replaces)
            && !confirmed
            && self.drive_status().is_some_and(|drive| drive.locked)
        {
            self.drive.confirm = Some(plan);
            return;
        }
        if unplug_first {
            if self.send_drive(CommandMessage::DriveDetach, None) && !matches!(plan, Plan::Unplug) {
                self.drive.pending = Some(plan);
            }
            return;
        }
        self.run_plan(plan);
    }

    fn send_drive(&mut self, message: CommandMessage, plugged: Option<Plugged>) -> bool {
        let sequence = self.drive_status().map_or(0, |drive| drive.sequence);
        if self.send(message) {
            self.drive.awaiting = Some((sequence, Instant::now(), plugged));
            true
        } else {
            false
        }
    }

    fn run_plan(&mut self, plan: Plan) {
        let Some(shared) = self.shared() else {
            self.notify(
                "Windows did not provide a local app data folder for the shared drive.",
                true,
            );
            return;
        };
        match plan {
            Plan::Send(paths) => self.start_job("Copying", move |progress| {
                shared.add(&paths, progress).map(Done::Sent)
            }),
            Plan::Save(folder) => self.start_job("Saving", move |progress| {
                shared
                    .export(&folder, progress)
                    .map(|(folder, count)| Done::Saved(folder, count))
            }),
            Plan::Erase => {
                self.start_job("Erasing", move |_| shared.erase().map(|()| Done::Erased))
            }
            Plan::Attach(path, format) => {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                let shared = path == shared.path;
                let plugged = Plugged {
                    name: if shared { "Shared drive".into() } else { name },
                    shared,
                };
                self.send_drive(
                    CommandMessage::DriveAttach {
                        path: path.display().to_string(),
                        format,
                    },
                    Some(plugged),
                );
            }
            Plan::Unplug => {
                self.send_drive(CommandMessage::DriveDetach, None);
            }
        }
    }

    fn start_job(
        &mut self,
        verb: &'static str,
        work: impl FnOnce(&Progress) -> Result<Done> + Send + 'static,
    ) {
        let progress = Arc::new(Progress::default());
        let shared = progress.clone();
        match std::thread::Builder::new()
            .name("cart-drive".into())
            .spawn(move || work(&shared))
        {
            Ok(handle) => {
                // Reading the listing happens in the background; keep any message.
                if verb != "Reading" {
                    self.drive.notice = None;
                }
                self.drive.job = Some(Job {
                    progress,
                    handle,
                    verb,
                });
            }
            Err(error) => self.notify(format!("The copy could not start: {error}"), true),
        }
    }

    fn refresh_listing(&mut self) {
        if self.drive.job.is_some() || !self.shared().is_some_and(|shared| shared.exists()) {
            return;
        }
        if let Some(shared) = self.shared() {
            self.start_job("Reading", move |_| shared.list().map(Done::Listed));
        }
    }

    /// Called every frame: dialogs, drops, helper answers and finished copies.
    pub(super) fn poll_drive(&mut self) {
        if self.options.demo {
            self.drive.dropped.clear();
            return;
        }
        if let Some(receiver) = &self.drive.picker {
            match receiver.try_recv() {
                Ok(picked) => {
                    self.drive.picker = None;
                    match picked {
                        Some(Picked::Send(paths)) => self.request(Plan::Send(paths), false),
                        Some(Picked::Image(path)) => match drive::identify(&path) {
                            Ok(format) => self.request(Plan::Attach(path, format), false),
                            Err(error) => self.notify(format!("{error:#}"), true),
                        },
                        Some(Picked::SaveTo(folder)) => self.request(Plan::Save(folder), false),
                        None => {}
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.drive.picker = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.drive.dropped.is_empty() && !self.drive.hovering {
            let paths = std::mem::take(&mut self.drive.dropped);
            if self.drive_busy() {
                self.notify(
                    "Wait for the current drive task to finish, then drop again.",
                    true,
                );
            } else {
                self.request(Plan::Send(paths), false);
            }
        }
        if !self.drive.smoke_started
            && self.drive_connected()
            && !self.drive_busy()
            && let Some(path) = self.options.smoke_attach.clone()
        {
            self.drive.smoke_started = true;
            match drive::identify(&path) {
                Ok(format) => self.request(Plan::Attach(path, format), false),
                Err(error) => self.notify(format!("{error:#}"), true),
            }
        }
        let status = self.drive_status().cloned();
        if let Some((sequence, sent, _)) = &self.drive.awaiting {
            let answered = status
                .as_ref()
                .is_some_and(|drive| drive.sequence != *sequence);
            if answered {
                let (_, _, plugged) = self.drive.awaiting.take().unwrap();
                let drive = status.clone().unwrap_or_default();
                if let Some(error) = drive.error.filter(|error| !error.is_empty()) {
                    self.drive.pending = None;
                    self.notify(error, true);
                } else if let Some(plugged) = plugged.filter(|_| drive.attached) {
                    let kind = if drive.media.as_deref() == Some("cdrom") {
                        "CD-ROM"
                    } else {
                        "USB drive"
                    };
                    self.notify(
                        format!("{} is plugged in as a {kind}.", plugged.name),
                        false,
                    );
                    self.drive.plugged = Some(plugged);
                } else if !drive.attached {
                    self.drive.plugged = None;
                }
            } else if sent.elapsed() > DRIVE_ANSWER || self.helper.is_none() {
                self.drive.awaiting = None;
                self.drive.pending = None;
                self.notify(
                    "The adapter did not answer. Check the connection and try again.",
                    true,
                );
            }
        }
        let attached = status.as_ref().is_some_and(|drive| drive.attached);
        if self.drive.awaiting.is_none() && !attached {
            if self.drive.was_attached {
                if status.as_ref().is_some_and(|drive| drive.ejected) {
                    self.notify("The target ejected the drive.", false);
                }
                self.drive.plugged = None;
                self.drive.listing = None;
            }
            if let Some(plan) = self.drive.pending.take() {
                self.run_plan(plan);
            }
        }
        self.drive.was_attached = attached;
        if let Some(drive) = &status {
            let total = drive.read + drive.written;
            let now = Instant::now();
            match self.drive.rate_sample {
                Some((at, before)) if now.duration_since(at) >= Duration::from_millis(500) => {
                    let instant =
                        total.saturating_sub(before) as f64 / now.duration_since(at).as_secs_f64();
                    self.drive.speed = self.drive.speed * 0.5 + instant * 0.5;
                    self.drive.rate_sample = Some((now, total));
                }
                None => self.drive.rate_sample = Some((now, total)),
                _ => {}
            }
        }
        if self.helper.is_none() {
            self.drive.plugged = None;
            self.drive.pending = None;
            self.drive.awaiting = None;
            self.drive.confirm = None;
        }
        if self
            .drive
            .job
            .as_ref()
            .is_some_and(|job| job.handle.is_finished())
        {
            let job = self.drive.job.take().unwrap();
            let result = job
                .handle
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("The drive task stopped unexpectedly.")));
            if job.verb != "Reading" {
                self.drive.listing_failed = false;
            }
            match result {
                Ok(Done::Listed(listing)) => {
                    self.drive.listing = Some(listing);
                    self.drive.listing_failed = false;
                }
                Ok(Done::Sent(count)) => {
                    self.drive.listing = None;
                    if let Some(shared) = self.shared() {
                        self.notify(
                            format!("Copied {}. Plugging in…", plural(count, "file")),
                            false,
                        );
                        self.run_plan(Plan::Attach(shared.path, ImageFormat::Raw));
                    }
                }
                Ok(Done::Saved(folder, count)) => {
                    self.drive.notice = Some(Notice {
                        text: format!("Saved {} to {}.", plural(count, "file"), folder.display()),
                        error: false,
                        folder: Some(folder),
                    });
                }
                Ok(Done::Erased) => {
                    self.drive.listing = None;
                    self.notify("The shared drive is empty.", false);
                }
                Err(error) => {
                    if job.verb == "Reading" {
                        self.drive.listing_failed = true;
                    }
                    self.drive.listing = None;
                    self.notify(format!("{error:#}"), true);
                }
            }
        }
    }

    pub(super) fn drive_menu_label(&self) -> egui::Atoms<'static> {
        use egui::IntoAtoms;
        if !self.drive_attached() {
            return "USB drive".into_atoms();
        }
        let color = if self.drive_status().is_some_and(|drive| drive.locked) {
            WARN
        } else {
            GOOD
        };
        (RichText::new("●").size(10.0).color(color), "USB drive").into_atoms()
    }

    pub(super) fn drive_controls(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(340.0);
        ui.set_max_width(340.0);
        // Real buttons, not menu rows: undo egui's menu_style for this panel.
        let style = ui.style_mut();
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.item_spacing.y = 6.0;
        let widgets = &mut style.visuals.widgets;
        widgets.inactive.weak_bg_fill = Color32::from_rgb(26, 29, 35);
        for state in [
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
            &mut widgets.open,
        ] {
            state.bg_stroke = Stroke::new(1.0_f32, BORDER);
        }
        let status = self.drive_status().cloned();
        let connected = self.drive_connected();
        let busy = self.drive_busy();
        let attached = status.as_ref().is_some_and(|drive| drive.attached);

        ui.horizontal(|ui| {
            ui.label(RichText::new("Virtual USB drive").strong().size(15.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (text, color) = match &status {
                    Some(drive) if drive.attached && drive.locked => ("In use by target", WARN),
                    Some(drive) if drive.attached => ("Plugged in", GOOD),
                    _ if connected => ("Unplugged", MUTED),
                    _ => ("Offline", MUTED),
                };
                ui.label(RichText::new(text).color(color));
            });
        });
        if self.helper.is_none() && !self.options.demo {
            ui.label(
                RichText::new("Connect the adapter to plug a drive into the target.").color(MUTED),
            );
        } else if status.as_ref().is_some_and(|drive| !drive.supported) {
            ui.label(RichText::new("This adapter has no file-transfer connection.").color(MUTED));
        }

        if let Some(drive) = status.as_ref().filter(|drive| drive.attached) {
            egui::Frame::new()
                .fill(Color32::from_rgb(22, 27, 36))
                .stroke(Stroke::new(1.0_f32, BORDER))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 3.0;
                    let name = self
                        .drive
                        .plugged
                        .as_ref()
                        .map_or("Disk image".to_owned(), |plugged| plugged.name.clone());
                    let kind = if drive.media.as_deref() == Some("cdrom") {
                        "CD-ROM"
                    } else {
                        "USB flash drive"
                    };
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.add(
                                egui::Label::new(RichText::new(name).color(TEXT).size(14.5))
                                    .truncate(),
                            );
                            ui.label(
                                RichText::new(format!("{kind} · {}", bytes(drive.size)))
                                    .color(MUTED),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if self.drive.confirm.is_none()
                                && ui.add_enabled(!busy, egui::Button::new("Unplug")).clicked()
                            {
                                self.request(Plan::Unplug, false);
                            }
                        });
                    });
                    let mut activity = format!(
                        "↓ {} read   ↑ {} written",
                        bytes(drive.read),
                        bytes(drive.written)
                    );
                    if self.drive.speed >= 64.0 * 1024.0 {
                        activity.push_str(&format!("   {}/s", bytes(self.drive.speed as u64)));
                    }
                    ui.add_space(2.0);
                    ui.label(RichText::new(activity).color(MUTED).size(12.0));
                    if drive.locked {
                        ui.label(
                            RichText::new(
                                "The target is using it. Eject it there before unplugging.",
                            )
                            .color(WARN),
                        );
                    }
                });
        }

        if self.drive.confirm.is_some() {
            ui.label(
                RichText::new("The target is still using the drive. Unplugging now can lose its unsaved changes.")
                    .color(WARN),
            );
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new("Unplug anyway").color(BAD))
                    .clicked()
                    && let Some(plan) = self.drive.confirm.take()
                {
                    self.request(plan, true);
                }
                if ui.button("Keep plugged in").clicked() {
                    self.drive.confirm = None;
                }
            });
        }

        ui.add_enabled_ui(connected && !busy, |ui| {
            ui.horizontal(|ui| {
                let send = egui::Button::new(RichText::new("Send files…").color(Color32::WHITE))
                    .fill(Color32::from_rgb(53, 106, 202));
                if ui
                    .add(send)
                    .on_hover_text("Copy files onto the shared drive and plug it into the target. Dropping files on the window does the same.")
                    .clicked()
                {
                    self.open_picker(Picker::Files);
                }
                if ui.button("Send folder…").clicked() {
                    self.open_picker(Picker::Folder);
                }
                if ui
                    .button("Disk image…")
                    .on_hover_text("Plug in a .img or .vhd as a USB flash drive, or an .iso as a CD-ROM. The target can change .img and .vhd files.")
                    .clicked()
                {
                    self.open_picker(Picker::Image);
                }
            });
        });

        if let Some(job) = &self.drive.job {
            let total = job.progress.total.load(Ordering::Relaxed);
            let done = job.progress.done.load(Ordering::Relaxed);
            let current = job
                .progress
                .current
                .lock()
                .map(|name| name.clone())
                .unwrap_or_default();
            let label = if current.is_empty() {
                format!("{}…", job.verb)
            } else {
                format!("{} {current}", job.verb)
            };
            ui.add(egui::Label::new(RichText::new(label).color(MUTED)).truncate());
            if total > 0 {
                ui.add(
                    egui::ProgressBar::new(done as f32 / total as f32)
                        .desired_height(14.0)
                        .text(
                            RichText::new(format!("{} of {}", bytes(done), bytes(total)))
                                .size(11.0),
                        ),
                );
            }
            if matches!(job.verb, "Copying" | "Saving") && ui.button("Cancel").clicked() {
                job.progress.cancel.store(true, Ordering::Relaxed);
            }
            ui.ctx().request_repaint();
        } else if self.drive.awaiting.is_some() || self.drive.pending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Waiting for the adapter…").color(MUTED));
            });
        }

        let shared_exists =
            self.options.demo || self.shared().is_some_and(|shared| shared.exists());
        if shared_exists {
            if self.drive.listing.is_none() && !self.drive.listing_failed && !busy {
                self.refresh_listing();
            }
            ui.add_space(2.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Shared drive").strong());
                if let Some(listing) = &self.drive.listing {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} of {} used",
                                bytes(listing.used),
                                bytes(listing.capacity)
                            ))
                            .color(MUTED)
                            .size(12.0),
                        );
                    });
                }
            });
            match &self.drive.listing {
                Some(listing) if listing.entries.is_empty() => {
                    ui.label(RichText::new("Empty").color(MUTED));
                }
                Some(listing) => {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for entry in listing.entries.iter().take(8) {
                        ui.horizontal(|ui| {
                            let width = ui.available_width() - 74.0;
                            ui.allocate_ui_with_layout(
                                egui::vec2(width, 18.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    let icon = if entry.is_dir { "🗁" } else { "🗋" };
                                    ui.label(RichText::new(icon).color(MUTED));
                                    ui.add(
                                        egui::Label::new(RichText::new(&entry.name).color(TEXT))
                                            .truncate(),
                                    );
                                },
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(bytes(entry.size)).color(MUTED).size(12.0),
                                    );
                                },
                            );
                        });
                    }
                    if listing.entries.len() > 8 {
                        ui.label(
                            RichText::new(format!("and {} more", listing.entries.len() - 8))
                                .color(MUTED),
                        );
                    }
                    ui.spacing_mut().item_spacing.y = 6.0;
                }
                None if self.drive.listing_failed => {
                    ui.label(RichText::new("The shared drive could not be read.").color(BAD));
                }
                None => {
                    ui.label(RichText::new("Reading…").color(MUTED));
                }
            }
            let shared_plugged = attached
                && self
                    .drive
                    .plugged
                    .as_ref()
                    .is_some_and(|plugged| plugged.shared);
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    if !shared_plugged
                        && ui
                            .add_enabled(connected, egui::Button::new("Plug in"))
                            .on_hover_text("Plug the shared drive into the target as it is")
                            .clicked()
                        && let Some(shared) = self.shared()
                    {
                        self.request(Plan::Attach(shared.path, ImageFormat::Raw), false);
                    }
                    if ui
                        .button("Save all to PC…")
                        .on_hover_text("Copy everything on the shared drive, including what the target saved, into a new folder")
                        .clicked()
                    {
                        self.open_picker(Picker::SaveTo);
                    }
                    if self.drive.confirm_erase {
                        if ui.button(RichText::new("Erase everything").color(BAD)).clicked() {
                            self.drive.confirm_erase = false;
                            self.request(Plan::Erase, false);
                        }
                        if ui.button("Keep").clicked() {
                            self.drive.confirm_erase = false;
                        }
                    } else if ui.button("Erase…").on_hover_text("Delete the shared drive and everything on it").clicked() {
                        self.drive.confirm_erase = true;
                    }
                });
            });
        }

        if let Some(notice) = &self.drive.notice {
            ui.add_space(2.0);
            ui.label(RichText::new(&notice.text).color(if notice.error { BAD } else { GOOD }));
            if let Some(folder) = notice.folder.clone()
                && ui.button("Show in Explorer").clicked()
            {
                let _ = std::process::Command::new("explorer.exe")
                    .arg(folder)
                    .spawn();
            }
        }
    }

    /// Shown over the video while files are dragged onto the window.
    pub(super) fn drop_overlay(&self, ui: &egui::Ui, rect: egui::Rect) {
        if !self.drive.hovering {
            return;
        }
        ui.painter()
            .rect_filled(rect, 0.0, Color32::from_black_alpha(150));
        if self.drive_connected() && !self.drive_busy() {
            overlay(
                ui,
                rect,
                "Drop to send to the target",
                "The files are copied onto the shared USB drive, which is then plugged in.",
            );
        } else {
            overlay(
                ui,
                rect,
                "Can't send files right now",
                "Connect the adapter and wait for the current drive task to finish.",
            );
        }
    }

    /// One line for the status bar while a drive is plugged in.
    pub(super) fn drive_summary(&self) -> Option<(String, Color32)> {
        let drive = self.drive_status().filter(|drive| drive.attached)?;
        let name = self
            .drive
            .plugged
            .as_ref()
            .map_or("USB drive", |plugged| plugged.name.as_str());
        Some(if drive.locked {
            (format!("{name} · in use by target"), WARN)
        } else {
            (format!("{name} · plugged in"), GOOD)
        })
    }
}

fn plural(count: u64, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

/// Sizes the way Explorer shows them.
pub(super) fn bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    if value < 1024 {
        return if value == 1 {
            "1 byte".into()
        } else {
            format!("{value} bytes")
        };
    }
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if size >= 100.0 {
        format!("{size:.0} {}", UNITS[unit])
    } else if size >= 10.0 {
        format!("{size:.1} {}", UNITS[unit])
    } else {
        format!("{size:.2} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_explorer() {
        assert_eq!(bytes(0), "0 bytes");
        assert_eq!(bytes(1), "1 byte");
        assert_eq!(bytes(1536), "1.50 KB");
        assert_eq!(bytes(12 << 20), "12.0 MB");
        assert_eq!(bytes(768 << 20), "768 MB");
        assert_eq!(bytes(5 << 30), "5.00 GB");
        assert_eq!(plural(1, "file"), "1 file");
        assert_eq!(plural(3, "file"), "3 files");
    }
}
