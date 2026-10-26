#[path = "app_ui.rs"]
mod ui;
#[path = "app_drive.rs"]
mod usb;

use crate::{
    Wake,
    graphics::Graphics,
    keyboard::{KeyRoute, PhysicalKeys, route_key},
    navigation::{
        CaptureAction, CaptureConditions, InteractionMode, MiddlePan, Navigation, capture_action,
        wheel_lines,
    },
    settings::{Mode, Settings},
    transport::{self, CommandMessage, Frame, Helper, Status},
};
use anyhow::Result;
use cart_core::{
    geometry::{
        PhysicalOffset, PhysicalPoint, PhysicalRect, ScaleMode, SourcePixel, SourceSize, Viewport,
    },
    paste::PastePlan,
};
use egui::{Color32, FontFamily, FontId, RichText, Stroke};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition},
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::{Fullscreen, Window, WindowId},
};

const TEXT: Color32 = Color32::from_rgb(228, 230, 235);
const MUTED: Color32 = Color32::from_rgb(145, 151, 163);
const PANEL: Color32 = Color32::from_rgb(14, 16, 21);
const BORDER: Color32 = Color32::from_rgb(44, 49, 59);

#[derive(Default)]
pub struct Options {
    pub demo: bool,
    pub smoke_test: bool,
    pub smoke_seconds: Option<u64>,
    pub connect: bool,
    pub helper: Option<PathBuf>,
    pub screenshot: Option<PathBuf>,
    /// Smoke tests only: open the USB drive panel for a screenshot.
    pub show_drive: bool,
    /// Connected smoke tests only: plug this image into the target.
    pub smoke_attach: Option<PathBuf>,
}

struct PastePreview {
    text: String,
    strokes: usize,
    caps_lock: bool,
    delay_ms: u32,
    allow_control_keys: bool,
}

pub struct App {
    options: Options,
    wake: EventLoopProxy<Wake>,
    window: Option<Arc<Window>>,
    graphics: Option<Graphics>,
    egui_state: Option<egui_winit::State>,
    context: egui::Context,
    helper: Option<Helper>,
    status: Status,
    error: Option<String>,
    helper_fault: bool,
    captured: bool,
    interaction: InteractionMode,
    focused: bool,
    focus_pending: bool,
    pointer_inside: bool,
    pointer_valid: bool,
    host_keys: HashSet<KeyCode>,
    host_buttons: HashSet<MouseButton>,
    host_modifiers: ModifiersState,
    quarantined_keys: HashSet<KeyCode>,
    quarantined_buttons: HashSet<MouseButton>,
    keys: PhysicalKeys,
    pointer: PhysicalPosition<f64>,
    buttons: u8,
    last_source: Option<SourcePixel>,
    mouse_dirty: bool,
    wheel_remainder: f64,
    viewport: Option<Viewport>,
    source: Option<SourceSize>,
    navigation: Navigation,
    navigation_time: Instant,
    navigation_delta: Option<Duration>,
    middle_pan: MiddlePan,
    /// Control mode only: send the middle button to the target instead of panning.
    remote_middle: bool,
    settings: Settings,
    settings_path: Option<PathBuf>,
    position_step: i8,
    position_busy_until: Option<Instant>,
    wake_sent: Option<Instant>,
    nearest: bool,
    fullscreen: bool,
    /// The virtual USB drive: copies, attached image, dialogs, drops.
    drive: usb::DriveState,
    preview: Option<PastePreview>,
    special_release: Option<(Instant, Vec<u8>)>,
    paste_pending: bool,
    paste_revision: u64,
    paste_baseline: u64,
    last_frame: Option<Instant>,
    next_redraw: Instant,
    smoke_deadline: Option<Instant>,
    rendered: u32,
    pub failed: bool,
}

impl App {
    pub fn new(options: Options, wake: EventLoopProxy<Wake>) -> Self {
        let context = egui::Context::default();
        configure_style(&context);
        context.options_mut(|options| options.zoom_with_keyboard = false);
        let interaction = if options.demo || options.smoke_test {
            InteractionMode::Navigate
        } else {
            InteractionMode::Control
        };
        let mut drive = usb::DriveState::new(options.demo);
        drive.open_once = options.show_drive;
        Self {
            options,
            wake,
            window: None,
            graphics: None,
            egui_state: None,
            context,
            helper: None,
            status: Status::default(),
            error: None,
            helper_fault: false,
            captured: false,
            interaction,
            focused: false,
            focus_pending: false,
            pointer_inside: false,
            pointer_valid: false,
            host_keys: HashSet::new(),
            host_buttons: HashSet::new(),
            host_modifiers: ModifiersState::empty(),
            quarantined_keys: HashSet::new(),
            quarantined_buttons: HashSet::new(),
            keys: PhysicalKeys::default(),
            pointer: PhysicalPosition::new(0.0, 0.0),
            buttons: 0,
            last_source: None,
            mouse_dirty: false,
            wheel_remainder: 0.0,
            viewport: None,
            source: None,
            navigation: Navigation::default(),
            navigation_time: Instant::now(),
            navigation_delta: None,
            middle_pan: MiddlePan::default(),
            remote_middle: false,
            settings: Settings::default(),
            settings_path: None,
            position_step: 1,
            position_busy_until: None,
            wake_sent: None,
            nearest: false,
            fullscreen: false,
            drive,
            preview: None,
            special_release: None,
            paste_pending: false,
            paste_revision: 0,
            paste_baseline: 0,
            last_frame: None,
            next_redraw: Instant::now(),
            smoke_deadline: None,
            rendered: 0,
            failed: false,
        }
    }

    fn load_settings(&mut self) {
        if self.options.demo {
            return;
        }
        if let Some(path) = Settings::path() {
            self.settings = Settings::load(&path);
            // Smoke tests use the remembered positions but never write them.
            if !self.options.smoke_test {
                self.settings_path = Some(path);
            }
        }
    }

    fn save_settings(&mut self) {
        if let Some(path) = &self.settings_path
            && let Err(error) = self.settings.save(path)
        {
            self.error = Some(format!("Could not save the picture position: {error:#}"));
        }
        let positions = self.settings.positions.clone();
        self.send(CommandMessage::Positions { positions });
    }

    /// The video mode whose picture position the controls adjust.
    pub(super) fn position_mode(&self) -> Option<Mode> {
        (!self.options.demo && self.viewing())
            .then(|| self.status.mode())
            .flatten()
    }

    /// Original Video Settings arrows: Left is X+1, Right X-1, Up Y+1, Down Y-1.
    pub(super) fn step_position(&mut self, dx: i8, dy: i8) {
        let Some(mode) = self.position_mode() else {
            return;
        };
        let (x, y) = self.settings.position(mode).unwrap_or((
            self.status.position_x.unwrap_or(0),
            self.status.position_y.unwrap_or(0),
        ));
        let clamp =
            |value: i8, delta: i8| (i16::from(value) + i16::from(delta)).clamp(-127, 127) as i8;
        self.settings.set_position(
            mode,
            clamp(x, dx.saturating_mul(self.position_step)),
            clamp(y, dy.saturating_mul(self.position_step)),
        );
        self.save_settings();
    }

    pub(super) fn default_position(&mut self, automatic: bool) {
        let Some(mode) = self.position_mode() else {
            return;
        };
        self.settings.clear_position(mode);
        self.save_settings();
        self.send(if automatic {
            CommandMessage::AlignAuto
        } else {
            CommandMessage::AlignReset
        });
        self.position_busy_until = Some(Instant::now() + Duration::from_millis(1500));
    }

    pub(super) fn position_busy(&self) -> bool {
        self.status.aligning == Some(true)
            || self
                .position_busy_until
                .is_some_and(|until| Instant::now() < until)
    }

    /// One Shift tap that may wake a sleeping display. The helper checks fresh
    /// keyboard status; it never enables other input without video.
    pub(super) fn can_wake(&self) -> bool {
        !self.options.smoke_test
            && self.helper.is_some()
            && !self.helper_fault
            && self.status.keyboard_link == Some(true)
            && !self.is_pasting()
            && self.special_release.is_none()
            && !self
                .wake_sent
                .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
    }

    pub(super) fn wake_target(&mut self) {
        if self.can_wake() {
            self.release_input(false);
            if self.send(CommandMessage::Wake) {
                self.wake_sent = Some(Instant::now());
            }
        }
    }

    pub(super) fn reset_view(&mut self, mode: ScaleMode) {
        self.middle_pan.cancel();
        self.navigation.reset(mode);
    }

    pub(super) fn set_remote_middle(&mut self, enabled: bool) {
        // Change ownership only between gestures, never mid-press.
        if !self.middle_pan.owns_release() && !self.host_buttons.contains(&MouseButton::Middle) {
            self.release_input(true);
            self.remote_middle = enabled;
        }
    }

    fn start_helper(&mut self) {
        if self.helper.is_some() || self.options.demo {
            return;
        }
        self.error = None;
        self.helper_fault = false;
        self.status = Status {
            state: Some("connecting".into()),
            message: Some("Opening the adapter…".into()),
            ..Default::default()
        };
        self.source = None;
        self.last_frame = None;
        self.paste_revision = 0;
        self.paste_pending = false;
        if let Some(graphics) = &mut self.graphics {
            graphics.clear_video();
        }
        let path = transport::helper_path(self.options.helper.as_deref());
        match Helper::spawn(&path, self.wake.clone()) {
            Ok(helper) => {
                self.helper = Some(helper);
                // Applied by the helper whenever a matching mode appears.
                let positions = self.settings.positions.clone();
                self.send(CommandMessage::Positions { positions });
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                self.status.state = Some("error".into());
            }
        }
    }

    fn disconnect(&mut self) {
        self.release_input(true);
        self.middle_pan.cancel();
        self.remote_middle = false;
        self.preview = None;
        if let Some(mut helper) = self.helper.take()
            && let Err(error) = helper.shutdown()
        {
            self.error = Some(error.to_string());
            eprintln!("{error}");
        }
        self.status = Status::default();
        self.source = None;
        self.viewport = None;
        self.paste_pending = false;
        self.last_frame = None;
        if let Some(graphics) = &mut self.graphics {
            graphics.clear_video();
        }
    }

    fn send(&mut self, message: CommandMessage) -> bool {
        if let Some(helper) = &self.helper {
            if let Err(error) = helper.send(message) {
                self.error = Some(error.to_string());
                self.helper_fault = true;
                self.release_input(true);
                return false;
            }
            true
        } else {
            false
        }
    }

    fn release_input(&mut self, cancel: bool) {
        self.captured = false;
        self.special_release = None;
        self.keys.clear();
        self.buttons = 0;
        self.mouse_dirty = false;
        self.wheel_remainder = 0.0;
        if let Some(helper) = &self.helper {
            if cancel {
                helper.cancel_release();
            } else {
                let _ = helper.send(CommandMessage::Release);
            }
        }
    }

    fn is_pasting(&self) -> bool {
        self.paste_pending || self.status.pasting == Some(true)
    }
    fn viewing(&self) -> bool {
        self.helper.is_some()
            && !self.helper_fault
            && self.source.is_some()
            && self.status.state.as_deref() == Some("viewing")
    }
    fn input_ready(&self) -> bool {
        !self.options.smoke_test
            && self.viewing()
            && self.status.keyboard_ready == Some(true)
            && self
                .last_frame
                .is_some_and(|time| time.elapsed() <= Duration::from_secs(3))
            && !self.is_pasting()
            && self.preview.is_none()
            && self.special_release.is_none()
    }

    fn set_interaction(&mut self, mode: InteractionMode) {
        if self.interaction == mode && !self.is_pasting() {
            return;
        }
        self.release_input(true);
        self.middle_pan.cancel();
        self.navigation.cancel(self.viewport);
        self.navigation_time = Instant::now();
        self.interaction = mode;
    }

    fn toggle_capture(&mut self) {
        let mode = if self.is_pasting() || self.interaction == InteractionMode::Control {
            InteractionMode::Navigate
        } else {
            InteractionMode::Control
        };
        self.set_interaction(mode);
    }

    fn reconcile_capture(&mut self) {
        let view_valid = self.viewport.is_some_and(|view| {
            self.source == Some(view.source())
                && self.status.width == Some(view.source().width)
                && self.status.height == Some(view.source().height)
        });
        let conditions = CaptureConditions {
            focused: self.focused,
            pointer_in_window: self.pointer_inside && self.pointer_valid,
            pointer_in_image: self.pointer_source(false).is_some(),
            ready: !self.options.demo && self.input_ready() && view_valid,
            blocked: self.focus_pending
                || self.middle_pan.owns_release()
                || egui::Popup::is_any_open(&self.context)
                || local_keyboard_editing(&self.context),
            host_neutral: self.host_keys.is_empty()
                && self.host_buttons.is_empty()
                && self.host_modifiers.is_empty(),
            dragging: self.buttons != 0,
        };
        match capture_action(self.interaction, self.captured, conditions) {
            CaptureAction::Acquire => {
                self.navigation.cancel(self.viewport);
                self.keys.clear();
                if self.send(CommandMessage::Release) {
                    self.captured = true;
                }
            }
            CaptureAction::Release => self.release_input(true),
            CaptureAction::None => {}
        }
    }

    fn poll_helper(&mut self) {
        let Some(helper) = &self.helper else {
            return;
        };
        let (frame, status, error, revision) = {
            let mut inbox = helper.inbox.lock().unwrap();
            (
                inbox.frame.take(),
                inbox.status.clone(),
                inbox.error.take(),
                inbox.paste_revision,
            )
        };
        self.paste_revision = revision;
        if revision > self.paste_baseline && status.pasting == Some(false) {
            self.paste_pending = false;
        }
        self.status = status;
        if let Some(error) = error {
            self.error = Some(error);
            self.helper_fault = true;
        }
        if self.helper_fault || matches!(self.status.state.as_deref(), Some("error" | "closed")) {
            if self.captured || self.is_pasting() || self.special_release.is_some() {
                self.release_input(true);
            }
            self.preview = None;
            self.paste_pending = false;
        } else if self.status.keyboard_ready == Some(false) && self.captured {
            self.release_input(true);
        }
        if let Some(frame) = frame {
            let size = SourceSize {
                width: frame.width,
                height: frame.height,
            };
            if self.source != Some(size) {
                self.release_input(true);
                self.middle_pan.cancel();
                self.navigation.reset(self.navigation.mode);
                self.viewport = None;
            }
            self.source = Some(size);
            self.last_frame = Some(Instant::now());
            if let Some(graphics) = &mut self.graphics {
                graphics.upload(frame);
            }
        }
    }

    /// Target events are routed before egui; a local press keeps its local release.
    fn physical_event(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::Focused(focused) => {
                self.focused = *focused;
                self.focus_pending = *focused;
                if !focused {
                    self.release_input(true);
                    self.preview = None;
                    self.host_keys.clear();
                    self.host_buttons.clear();
                    self.host_modifiers = ModifiersState::empty();
                    self.middle_pan.cancel();
                    self.navigation.cancel(self.viewport);
                }
                false
            }
            WindowEvent::CursorEntered { .. } => {
                self.pointer_inside = true;
                false
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer_inside = false;
                self.pointer_valid = false;
                self.middle_pan.cancel();
                if self.captured {
                    self.release_input(true);
                }
                false
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                let code = match event.physical_key {
                    PhysicalKey::Code(code) => Some(code),
                    _ => None,
                };
                let down = event.state == ElementState::Pressed;
                if let Some(code) = code {
                    if down {
                        self.host_keys.insert(code);
                    } else {
                        self.host_keys.remove(&code);
                    }
                }
                if *is_synthetic {
                    if let Some(code) = code {
                        if down {
                            self.quarantined_keys.insert(code);
                        } else {
                            self.quarantined_keys.remove(&code);
                        }
                    }
                    return true;
                }
                let owned = !down && code.is_some_and(|code| self.quarantined_keys.remove(&code));
                if code == Some(KeyCode::F12) {
                    if down && !event.repeat {
                        self.toggle_capture();
                    }
                    return true;
                }
                if owned && !self.captured {
                    return true;
                }
                if down && self.captured && self.pointer_source(false).is_none() {
                    if let Some(code) = code {
                        self.quarantined_keys.insert(code);
                    }
                    return true;
                }
                match route_key(
                    self.captured,
                    self.is_pasting() || self.special_release.is_some(),
                    code,
                    down,
                    event.repeat,
                    false,
                ) {
                    KeyRoute::Local => false,
                    KeyRoute::Consume => {
                        if down && let Some(code) = code {
                            self.quarantined_keys.insert(code);
                        }
                        true
                    }
                    KeyRoute::ToggleCapture => unreachable!(),
                    KeyRoute::Remote { usage, down } => {
                        if down && let Some(code) = code {
                            self.quarantined_keys.insert(code);
                        }
                        if self.keys.transition(usage, down) {
                            self.send(CommandMessage::Key { usage, down });
                        }
                        true
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.host_modifiers = modifiers.state();
                self.captured || self.is_pasting()
            }
            WindowEvent::Ime(_) => self.captured || self.is_pasting(),
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = *position;
                self.pointer_valid = position.x.is_finite() && position.y.is_finite();
                // Native mouse capture during a held button reports positions
                // outside the client area; those are not "in the window".
                self.pointer_inside = self.pointer_valid
                    && self.window.as_ref().is_none_or(|window| {
                        let size = window.inner_size();
                        position.x >= 0.0
                            && position.y >= 0.0
                            && position.x < f64::from(size.width)
                            && position.y < f64::from(size.height)
                    });
                if self.middle_pan.active() {
                    if self.pointer_inside {
                        let point = PhysicalPoint {
                            x: position.x,
                            y: position.y,
                        };
                        if let Some(delta) = self.middle_pan.moved(point) {
                            self.navigation.pan_by(delta, self.viewport);
                        }
                    } else {
                        self.middle_pan.cancel();
                    }
                    return true;
                }
                if self.captured
                    && let Some(pixel) = self.pointer_source(self.buttons != 0)
                {
                    self.last_source = Some(pixel);
                    self.mouse_dirty = true;
                }
                false
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let down = *state == ElementState::Pressed;
                if down {
                    self.host_buttons.insert(*button);
                } else {
                    self.host_buttons.remove(button);
                }
                if *button == MouseButton::Middle {
                    if !down && self.middle_pan.release() {
                        return true;
                    }
                    if down {
                        let local = self.middle_pan_eligible();
                        let point = PhysicalPoint {
                            x: self.pointer.x,
                            y: self.pointer.y,
                        };
                        if self.middle_pan.press(point, local) {
                            // Release any target input once; the selected mode stays.
                            if self.captured {
                                self.release_input(true);
                            }
                            self.navigation.cancel(self.viewport);
                            return true;
                        }
                    }
                }
                if self.middle_pan.owns_release() {
                    // Other buttons during a pan stay local through their release.
                    if down {
                        self.quarantined_buttons.insert(*button);
                    } else {
                        self.quarantined_buttons.remove(button);
                    }
                    return true;
                }
                let owned = !down && self.quarantined_buttons.remove(button);
                if owned && !self.captured {
                    return true;
                }
                if self.is_pasting() || !self.captured {
                    return false;
                }
                let mask = match button {
                    MouseButton::Left => 1,
                    MouseButton::Right => 2,
                    MouseButton::Middle => 4,
                    _ => return true,
                };
                let pixel = self.pointer_source(!down);
                if down && pixel.is_none() {
                    if self.buttons != 0 {
                        self.quarantined_buttons.insert(*button);
                        return true;
                    }
                    self.release_input(true);
                    return false;
                }
                if let Some(pixel) = pixel {
                    self.last_source = Some(pixel);
                }
                if down {
                    self.quarantined_buttons.insert(*button);
                    self.buttons |= mask;
                } else {
                    self.buttons &= !mask;
                }
                self.flush_mouse(0);
                true
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.options.smoke_test && std::env::var_os("CART_TRACE_NAVIGATION").is_some() {
                    eprintln!(
                        "Navigation wheel: mode={:?}, focused={}, pointer_valid={}, pointer_inside={}, ctrl={}, physical_ctrl={}, popup={}, editing={}",
                        self.interaction,
                        self.focused,
                        self.pointer_valid,
                        self.pointer_inside,
                        self.host_modifiers.control_key(),
                        self.host_keys.contains(&KeyCode::ControlLeft)
                            || self.host_keys.contains(&KeyCode::ControlRight),
                        egui::Popup::is_any_open(&self.context),
                        local_keyboard_editing(&self.context)
                    );
                }
                if self.middle_pan.owns_release() {
                    return true;
                }
                if self.interaction == InteractionMode::Navigate
                    && self.focused
                    && self.pointer_valid
                    && self.pointer_inside
                    && (self.host_modifiers.control_key()
                        || self.host_keys.contains(&KeyCode::ControlLeft)
                        || self.host_keys.contains(&KeyCode::ControlRight))
                    && !self.is_pasting()
                    && self.preview.is_none()
                    && !egui::Popup::is_any_open(&self.context)
                    && !local_keyboard_editing(&self.context)
                    && let Some(view) = self.viewport
                    && view.bounds().contains(PhysicalPoint {
                        x: self.pointer.x,
                        y: self.pointer.y,
                    })
                {
                    let (value, pixels) = match delta {
                        MouseScrollDelta::LineDelta(_, y) => (f64::from(*y), false),
                        MouseScrollDelta::PixelDelta(p) => (
                            p.y / self.window.as_ref().map_or(1.0, |w| w.scale_factor()),
                            true,
                        ),
                    };
                    if let Some(lines) = wheel_lines(value, pixels) {
                        self.navigation.zoom_at(
                            view,
                            PhysicalPoint {
                                x: self.pointer.x,
                                y: self.pointer.y,
                            },
                            lines,
                        );
                    }
                    return true;
                }
                if self.captured {
                    if let Some(pixel) = self.pointer_source(false) {
                        self.last_source = Some(pixel);
                        let lines = match delta {
                            MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                            MouseScrollDelta::PixelDelta(p) => p.y / 40.0,
                        };
                        if lines.is_finite() {
                            self.wheel_remainder += lines;
                            let wheel = self.wheel_remainder.trunc().clamp(-127.0, 127.0) as i8;
                            self.wheel_remainder -= f64::from(wheel);
                            if wheel != 0 {
                                self.flush_mouse(wheel);
                            }
                        }
                    }
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Middle-drag pans locally in Navigate, and in Control unless the user
    /// chose to send the middle button to the target.
    fn middle_pan_eligible(&self) -> bool {
        (self.interaction == InteractionMode::Navigate || !self.remote_middle)
            && self.focused
            && self.pointer_valid
            && self.pointer_inside
            && !self.is_pasting()
            && self.preview.is_none()
            && self.special_release.is_none()
            && !egui::Popup::is_any_open(&self.context)
            && !local_keyboard_editing(&self.context)
            && self.viewport.is_some_and(|view| {
                view.bounds().contains(PhysicalPoint {
                    x: self.pointer.x,
                    y: self.pointer.y,
                })
            })
    }

    fn pointer_source(&self, clamp: bool) -> Option<SourcePixel> {
        let view = self.viewport?;
        let point = PhysicalPoint {
            x: self.pointer.x,
            y: self.pointer.y,
        };
        if let Some(pixel) = view.screen_to_source(point) {
            return Some(pixel);
        }
        if !clamp {
            return None;
        }
        // Continue remote drags at the visible image edge. Never map letterbox
        // clicks into the source; this path only serves a held-button drag/up.
        let image = view.image_rect();
        let bounds = view.bounds();
        let left = image.x.max(bounds.x);
        let top = image.y.max(bounds.y);
        let right = (image.x + image.width).min(bounds.x + bounds.width);
        let bottom = (image.y + image.height).min(bounds.y + bounds.height);
        if right <= left || bottom <= top {
            return self.last_source;
        }
        view.screen_to_source(PhysicalPoint {
            x: point.x.clamp(left, (right - 0.001).max(left)),
            y: point.y.clamp(top, (bottom - 0.001).max(top)),
        })
    }

    fn flush_mouse(&mut self, wheel: i8) {
        if !self.captured || self.is_pasting() {
            return;
        }
        if let Some(pixel) = self.last_source {
            self.send(CommandMessage::Mouse {
                x: pixel.x,
                y: pixel.y,
                buttons: self.buttons,
                wheel,
            });
        }
        self.mouse_dirty = false;
    }

    fn read_clipboard(&mut self) {
        if !self.input_ready() {
            return;
        }
        self.release_input(false);
        let text = match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
            Ok(text) => text,
            Err(error) => {
                self.error = Some(format!("Could not read clipboard text: {error}"));
                return;
            }
        };
        if text.len() > transport::MAX_PASTE_BYTES {
            self.error = Some(
                "Clipboard text exceeds the 16 KiB paste limit. Copy a smaller section.".into(),
            );
            return;
        }
        let Some(caps_lock) = self.status.caps_lock else {
            self.error = Some(
                "Target Caps Lock state is unknown. Wait for keyboard status before pasting."
                    .into(),
            );
            return;
        };
        match PastePlan::new(&text, caps_lock) {
            Ok(plan) if !plan.is_empty() => {
                self.preview = Some(PastePreview {
                    strokes: plan.len(),
                    text,
                    caps_lock,
                    delay_ms: 150,
                    allow_control_keys: false,
                })
            }
            Ok(_) => {
                self.error = Some("The clipboard is empty. Copy plain text, then try again.".into())
            }
            Err(error) => {
                self.error = Some(format!(
                    "Nothing was sent. Paste supports the US keyboard layout and printable ASCII, Tab and line breaks; {error}."
                ))
            }
        }
    }

    fn special_key(&mut self, keys: &[u8]) {
        if !self.input_ready() {
            return;
        }
        self.release_input(false);
        for &usage in keys {
            if !self.send(CommandMessage::Key { usage, down: true }) {
                return;
            }
        }
        // Hold long enough for the target's HID polling without blocking the UI.
        self.special_release = Some((Instant::now() + Duration::from_millis(50), keys.to_vec()));
    }

    fn finish_special_key(&mut self) {
        if self
            .special_release
            .as_ref()
            .is_some_and(|(at, _)| Instant::now() >= *at)
        {
            let (_, keys) = self.special_release.take().unwrap();
            for &usage in keys.iter().rev() {
                if !self.send(CommandMessage::Key { usage, down: false }) {
                    break;
                }
            }
        }
    }

    fn paste_dialog(&mut self, context: &egui::Context) {
        let Some(mut preview) = self.preview.take() else {
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("paste_preview")).show(context, |ui| {
            ui.set_width(570.0);
            ui.heading("Type clipboard on the target?");
            ui.add_space(7.0);
            ui.label("This sends keystrokes, not a clipboard transfer. Check the target cursor before continuing.");
            ui.add_space(10.0);
            egui::Frame::new().fill(Color32::from_rgb(13, 17, 23)).stroke(Stroke::new(1.0_f32, BORDER)).corner_radius(7).inner_margin(12).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(250.0).show(ui, |ui| {
                    // A read-only selectable preview. No extra clipboard reads.
                    let display: String = preview.text.chars().take(12_000).collect();
                    ui.add(egui::Label::new(display).selectable(true).wrap());
                    if preview.text.chars().count() > 12_000 { ui.label(RichText::new("Preview shortened. The full validated clipboard will be typed.").color(MUTED)); }
                });
            });
            ui.add_space(9.0);
            ui.label(RichText::new("Line breaks press Enter and can execute commands. Tabs can trigger completion or move focus.").color(Color32::from_rgb(226, 195, 137)));
            ui.label("US keyboard layout only. No extra Enter is added. Unsupported characters are rejected before anything is sent.");
            ui.label(RichText::new("Experimental paste: this adapter has repeated characters in testing. Verify a short sample before sending commands.").color(Color32::from_rgb(226, 195, 137)));
            let has_control_keys = preview.text.bytes().any(|byte| matches!(byte, b'\t' | b'\r' | b'\n'));
            if has_control_keys {
                ui.checkbox(&mut preview.allow_control_keys, "Allow Enter and Tab keys for this paste");
            }
            ui.add_space(9.0);
            ui.horizontal(|ui| {
                ui.label("Pause between keys");
                ui.add(egui::DragValue::new(&mut preview.delay_ms).range(50..=500).suffix(" ms"));
                let seconds = preview.strokes as f64 * f64::from(preview.delay_ms + 50) / 1000.0;
                ui.label(RichText::new(format!("{} keystrokes · about {:.1} s", preview.strokes, seconds)).color(MUTED));
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() { cancel = true; }
                let valid = self.viewing() && self.status.keyboard_ready == Some(true) && self.status.caps_lock == Some(preview.caps_lock) && !self.is_pasting() && (!has_control_keys || preview.allow_control_keys);
                if ui.add_enabled(valid, egui::Button::new("Type on target")).clicked() { confirm = true; }
                if self.status.caps_lock != Some(preview.caps_lock) { ui.label("Caps Lock changed. Cancel and reopen the preview."); }
            });
            if ui.input(|input| input.key_pressed(egui::Key::Escape)) { cancel = true; }
        });
        if confirm {
            // Revalidate at the final boundary, including a fresh Caps Lock check.
            if let Some(caps) = self.status.caps_lock
                && caps == preview.caps_lock
                && PastePlan::new(&preview.text, caps).is_ok()
            {
                self.paste_baseline = self.paste_revision;
                if self.send(CommandMessage::Paste {
                    text: preview.text,
                    delay_ms: preview.delay_ms,
                }) {
                    self.paste_pending = true;
                }
            }
        } else if !cancel {
            self.preview = Some(preview);
        }
    }

    fn redraw(&mut self) -> Result<()> {
        let now = Instant::now();
        self.navigation_delta = Some(now.saturating_duration_since(self.navigation_time));
        self.navigation_time = now;
        self.poll_helper();
        self.poll_drive();
        if self.mouse_dirty {
            self.flush_mouse(0);
        }
        let Some(window) = self.window.clone() else {
            return Ok(());
        };
        if window.inner_size().width == 0 || window.inner_size().height == 0 {
            return Ok(());
        }
        let raw = self.egui_state.as_mut().unwrap().take_egui_input(&window);
        let context = self.context.clone();
        let output = context.run(raw, |_| self.ui());
        self.focus_pending = false;
        if self.middle_pan.active()
            && (egui::Popup::is_any_open(&context) || self.preview.is_some() || self.is_pasting())
        {
            self.middle_pan.cancel();
        }
        self.reconcile_capture();
        self.egui_state
            .as_mut()
            .unwrap()
            .handle_platform_output(&window, output.platform_output.clone());
        let graphics = self.graphics.as_mut().unwrap();
        graphics.set_nearest(self.nearest);
        let capture_now = self
            .smoke_deadline
            .is_some_and(|deadline| Instant::now() + Duration::from_millis(300) >= deadline);
        let screenshot = if capture_now {
            self.options.screenshot.as_deref()
        } else {
            None
        };
        if graphics.draw(&context, output, screenshot)? {
            self.options.screenshot = None;
        }
        self.rendered += 1;
        Ok(())
    }
}

impl ApplicationHandler<Wake> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let result = (|| -> Result<()> {
            let attributes = Window::default_attributes()
                .with_title(if self.options.demo {
                    "USB Crash Cart · DEMO (no device)"
                } else {
                    "USB Crash Cart"
                })
                .with_visible(false)
                .with_inner_size(LogicalSize::new(1360.0, 900.0))
                .with_min_inner_size(LogicalSize::new(760.0, 520.0));
            let attributes = crate::chrome::attributes(
                attributes,
                [PANEL.r(), PANEL.g(), PANEL.b()],
                [TEXT.r(), TEXT.g(), TEXT.b()],
            )?;
            let window = Arc::new(event_loop.create_window(attributes)?);
            window.set_ime_allowed(false);
            let graphics = pollster::block_on(Graphics::new(window.clone()))?;
            let egui_state = egui_winit::State::new(
                self.context.clone(),
                egui::ViewportId::ROOT,
                &*window,
                Some(window.scale_factor() as f32),
                Some(winit::window::Theme::Dark),
                None,
            );
            self.window = Some(window);
            self.graphics = Some(graphics);
            self.egui_state = Some(egui_state);
            self.load_settings();
            if self.options.demo {
                let frame = demo_frame();
                self.source = Some(SourceSize {
                    width: frame.width,
                    height: frame.height,
                });
                self.graphics.as_mut().unwrap().upload(frame);
            } else if self.options.connect {
                self.start_helper();
            }
            if self.options.smoke_test {
                self.smoke_deadline = Some(
                    Instant::now() + Duration::from_secs(self.options.smoke_seconds.unwrap_or(2)),
                );
            }
            if let Some(window) = &self.window {
                window.set_visible(true);
                window.request_redraw();
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("Viewer startup failed: {error:#}");
            self.failed = true;
            event_loop.exit();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: Wake) {
        // Coalesce helper notifications into the next scheduled redraw.
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if window.id() != window_id {
            return;
        }
        if matches!(event, WindowEvent::CloseRequested) {
            self.disconnect();
            event_loop.exit();
            return;
        }
        if let WindowEvent::Resized(size) = &event {
            if let Some(graphics) = &mut self.graphics {
                graphics.resize(*size);
            }
            self.middle_pan.cancel();
            self.navigation.cancel(self.viewport);
            self.viewport = None;
        }
        match &event {
            // Files dragged onto the window go to the shared USB drive.
            WindowEvent::HoveredFile(_) => self.drive.hovering = true,
            WindowEvent::HoveredFileCancelled => {
                self.drive.hovering = false;
                self.drive.dropped.clear();
            }
            WindowEvent::DroppedFile(path) => {
                self.drive.hovering = false;
                self.drive.dropped.push(path.clone());
            }
            _ => {}
        }
        if matches!(event, WindowEvent::ScaleFactorChanged { .. }) {
            self.middle_pan.cancel();
            self.navigation.cancel(self.viewport);
            self.viewport = None;
        }
        let consumed = self.physical_event(&event);
        if !consumed {
            let _ = self
                .egui_state
                .as_mut()
                .unwrap()
                .on_window_event(&window, &event);
        }
        if matches!(event, WindowEvent::RedrawRequested)
            && let Err(error) = self.redraw()
        {
            eprintln!("Viewer rendering failed: {error:#}");
            self.failed = true;
            self.disconnect();
            event_loop.exit();
        }
        self.reconcile_capture();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.finish_special_key();
        if self
            .smoke_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.smoke_deadline = None;
            if self.rendered == 0 || self.options.screenshot.is_some() {
                self.failed = true;
                eprintln!(
                    "Smoke test failed: no frames rendered or requested screenshot was not captured"
                );
            } else if self.options.connect
                && (!self.viewing()
                    || !self
                        .last_frame
                        .is_some_and(|time| time.elapsed() <= Duration::from_secs(3)))
            {
                self.failed = true;
                eprintln!(
                    "Smoke test failed: the connected helper did not provide fresh real video"
                );
            } else {
                let drive = self.status.drive.clone().unwrap_or_default();
                println!(
                    "Viewer smoke test passed: {} GPU frames; demo={}, live_video={}, input_captured={}, scale={:.4}, pan={:.1},{:.1}, position={},{}, drive_attached={}, drive_read={}, drive_locked={}",
                    self.rendered,
                    self.options.demo,
                    self.viewing(),
                    self.captured,
                    self.viewport.map_or(0.0, |view| view.scale()),
                    self.navigation.pan.x,
                    self.navigation.pan.y,
                    self.status.position_x.map_or("-".into(), |x| x.to_string()),
                    self.status.position_y.map_or("-".into(), |y| y.to_string()),
                    drive.attached,
                    drive.read,
                    drive.locked,
                );
            }
            self.disconnect();
            event_loop.exit();
            return;
        }
        if Instant::now() >= self.next_redraw {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            self.next_redraw = Instant::now() + Duration::from_millis(16);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_redraw));
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.disconnect();
    }
}

#[cfg(test)]
fn ui_viewport(
    source: SourceSize,
    rect: egui::Rect,
    ppp: f32,
    mode: ScaleMode,
    zoom: f64,
    pan: PhysicalOffset,
) -> Option<Viewport> {
    let ppp = f64::from(ppp);
    Viewport::new(
        source,
        PhysicalRect {
            x: f64::from(rect.min.x) * ppp,
            y: f64::from(rect.min.y) * ppp,
            width: f64::from(rect.width()) * ppp,
            height: f64::from(rect.height()) * ppp,
        },
        mode,
        zoom,
        pan,
    )
    .ok()
}

fn overlay(ui: &egui::Ui, rect: egui::Rect, title: &str, subtitle: &str) {
    let center = rect.center();
    let width = (rect.width() - 64.0).clamp(100.0, 620.0);
    let box_rect = egui::Rect::from_center_size(center, egui::vec2(width, 152.0));
    ui.painter()
        .rect_filled(box_rect, 12.0, Color32::from_rgb(19, 25, 34));
    ui.painter().rect_stroke(
        box_rect,
        12.0,
        Stroke::new(1.0_f32, BORDER),
        egui::StrokeKind::Inside,
    );
    let title_galley =
        ui.painter()
            .layout(title.into(), FontId::proportional(22.0), TEXT, width - 44.0);
    let subtitle_galley = ui.painter().layout(
        subtitle.into(),
        FontId::proportional(14.0),
        MUTED,
        width - 44.0,
    );
    ui.painter().galley(
        egui::pos2(box_rect.left() + 22.0, box_rect.top() + 27.0),
        title_galley,
        TEXT,
    );
    ui.painter().galley(
        egui::pos2(box_rect.left() + 22.0, box_rect.top() + 70.0),
        subtitle_galley,
        MUTED,
    );
}

fn local_keyboard_editing(context: &egui::Context) -> bool {
    context
        .memory(|memory| memory.focused())
        .is_some_and(|id| egui::TextEdit::load_state(context, id).is_some())
}

fn configure_style(context: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Inter".into(),
        egui::FontData::from_static(include_bytes!(
            "../../../assets/fonts/inter/InterVariable.ttf"
        ))
        .into(),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "Inter".into());
    context.set_fonts(fonts);
    let mut style = (*context.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.panel_fill = PANEL;
    style.visuals.window_fill = PANEL;
    style.visuals.window_stroke = Stroke::new(1.0_f32, BORDER);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(26, 29, 35);
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(26, 29, 35);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(42, 47, 57);
    style.visuals.widgets.active.weak_bg_fill = Color32::from_rgb(56, 68, 87);
    style.visuals.selection.bg_fill = Color32::from_rgb(57, 77, 101);
    style.visuals.selection.stroke = Stroke::new(1.0_f32, TEXT);
    for visuals in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        visuals.corner_radius = egui::CornerRadius::same(6);
        visuals.bg_stroke = Stroke::new(1.0_f32, BORDER);
    }
    style.spacing.item_spacing = egui::vec2(9.0, 8.0);
    style.spacing.button_padding = egui::vec2(11.0, 7.0);
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, FontId::proportional(21.0));
    context.set_style(style);
}

fn demo_frame() -> Frame {
    crate::demo::frame()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::clamp_pan;
    #[test]
    fn toolbar_focus_does_not_block_hover_capture_but_text_editing_does() {
        let context = egui::Context::default();
        let button = egui::Id::new("control_button");
        let edit = egui::Id::new("local_editor");
        let mut text = String::from("150");
        let _ = context.run(Default::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                ui.interact(ui.max_rect(), button, egui::Sense::click());
                ui.add(egui::TextEdit::singleline(&mut text).id(edit));
            });
        });
        context.memory_mut(|memory| memory.request_focus(button));
        assert!(!local_keyboard_editing(&context));
        context.memory_mut(|memory| memory.request_focus(edit));
        assert!(local_keyboard_editing(&context));
    }

    #[test]
    fn egui_units_convert_to_physical_exactly_once() {
        for ppp in [1.0, 1.25, 1.5, 2.0] {
            let source = SourceSize {
                width: 1920,
                height: 1080,
            };
            let rect = egui::Rect::from_min_size(egui::pos2(20.0, 80.0), egui::vec2(1000.0, 700.0));
            let view = ui_viewport(
                source,
                rect,
                ppp,
                ScaleMode::ActualPixels,
                1.0,
                PhysicalOffset::default(),
            )
            .unwrap();
            assert_eq!(view.scale(), 1.0);
            let physical = PhysicalPoint {
                x: 520.0 * f64::from(ppp),
                y: 430.0 * f64::from(ppp),
            };
            assert_eq!(
                view.screen_to_source(physical),
                Some(SourcePixel { x: 960, y: 540 })
            );
            assert_eq!(view.image_rect().width, 1920.0);
        }
    }
    #[test]
    fn panning_keeps_image_covering_viewport_when_cropped() {
        let source = SourceSize {
            width: 1920,
            height: 1080,
        };
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0));
        let view = ui_viewport(
            source,
            rect,
            1.0,
            ScaleMode::ActualPixels,
            1.0,
            PhysicalOffset::default(),
        )
        .unwrap();
        let mut pan = PhysicalOffset {
            x: 10_000.0,
            y: -10_000.0,
        };
        clamp_pan(&mut pan, view);
        assert_eq!(
            pan,
            PhysicalOffset {
                x: 460.0,
                y: -190.0
            }
        );
    }
}
