//! The setup window: one page to confirm, live progress, then the result.
use crate::{
    install::{self, INSTALL_STEPS, Options, Outcome, REMOVE_STEPS, State, System},
    system::{self, Windows},
};
use anyhow::{Result, bail};
use egui::{Color32, FontFamily, FontId, RichText, Stroke, vec2};
use std::{
    io::Cursor,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

const TEXT: Color32 = Color32::from_rgb(228, 230, 235);
const MUTED: Color32 = Color32::from_rgb(145, 151, 163);
const PANEL: Color32 = Color32::from_rgb(14, 16, 21);
const CARD: Color32 = Color32::from_rgb(20, 23, 30);
const BORDER: Color32 = Color32::from_rgb(44, 49, 59);
const ACCENT: Color32 = Color32::from_rgb(53, 106, 202);
const GOOD: Color32 = Color32::from_rgb(153, 207, 176);
const BAD: Color32 = Color32::from_rgb(240, 165, 157);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Install,
    Remove,
}

enum Page {
    Confirm,
    Working,
    Installed(Outcome),
    Removed,
    Failed(String),
}

enum Message {
    Step(usize, State, String),
    Installed(Result<Outcome, String>),
    Removed(Result<bool, String>),
}

struct Setup {
    mode: Mode,
    dir: PathBuf,
    page: Page,
    desktop: bool,
    accepted: bool,
    reading_license: bool,
    runtime_present: bool,
    steps: Vec<(&'static str, State, String)>,
    worker: Option<mpsc::Receiver<Message>>,
    delete_self: bool,
    window: Option<Arc<Window>>,
    render: Option<crate::render::Render>,
    egui_state: Option<egui_winit::State>,
    context: egui::Context,
    icon: Option<egui::TextureHandle>,
    error: Option<String>,
}

pub fn run(mode: Mode, dir: PathBuf) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let context = egui::Context::default();
    style(&context);
    let mut setup = Setup {
        mode,
        dir,
        page: Page::Confirm,
        desktop: false,
        accepted: false,
        reading_license: false,
        runtime_present: Windows.vc_runtime_present(),
        steps: Vec::new(),
        worker: None,
        delete_self: false,
        window: None,
        render: None,
        egui_state: None,
        context,
        icon: None,
        error: None,
    };
    event_loop.run_app(&mut setup)?;
    if let Some(error) = setup.error {
        bail!(error);
    }
    if setup.delete_self {
        system::delete_after_exit(&std::env::current_exe()?)?;
    }
    Ok(())
}

fn style(context: &egui::Context) {
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
    style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(26, 29, 35);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(42, 47, 57);
    style.visuals.widgets.active.weak_bg_fill = Color32::from_rgb(56, 68, 87);
    style.visuals.selection.bg_fill = ACCENT;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, TEXT);
    for visuals in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        visuals.corner_radius = egui::CornerRadius::same(5);
        visuals.bg_stroke = Stroke::new(1.0_f32, BORDER);
    }
    style.spacing.item_spacing = vec2(9.0, 8.0);
    style.spacing.button_padding = vec2(14.0, 8.0);
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(14.0));
    context.set_style(style);
}

fn icon_image() -> Option<egui::ColorImage> {
    let bytes = include_bytes!("../../../assets/icons/128/cart.png");
    let mut reader = png::Decoder::new(Cursor::new(bytes)).read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).ok()?;
    (info.color_type == png::ColorType::Rgba).then(|| {
        egui::ColorImage::from_rgba_unmultiplied(
            [info.width as usize, info.height as usize],
            &pixels[..info.buffer_size()],
        )
    })
}

fn primary(text: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
        .fill(ACCENT)
        .stroke(Stroke::NONE)
        .min_size(vec2(132.0, 36.0))
}

fn secondary(text: &str) -> egui::Button<'_> {
    egui::Button::new(text).min_size(vec2(96.0, 36.0))
}

impl Setup {
    fn start(&mut self) {
        let (sender, receiver) = mpsc::channel();
        let labels: &[&'static str] = match self.mode {
            Mode::Install => &INSTALL_STEPS,
            Mode::Remove => &REMOVE_STEPS,
        };
        self.steps = labels
            .iter()
            .map(|label| (*label, State::Pending, String::new()))
            .collect();
        self.page = Page::Working;
        let mode = self.mode;
        let options = Options {
            dir: self.dir.clone(),
            desktop: self.desktop,
        };
        std::thread::spawn(move || {
            let progress = sender.clone();
            let mut report = |index, state, detail| {
                let _ = progress.send(Message::Step(index, state, detail));
            };
            let message = match mode {
                Mode::Install => Message::Installed(
                    install::install(&Windows, &options, &mut report)
                        .map_err(|error| format!("{error:#}")),
                ),
                Mode::Remove => Message::Removed(
                    install::uninstall(&Windows, &options.dir, &mut report)
                        .map_err(|error| format!("{error:#}")),
                ),
            };
            let _ = sender.send(message);
        });
        self.worker = Some(receiver);
    }

    fn poll(&mut self) {
        let Some(worker) = &self.worker else {
            return;
        };
        while let Ok(message) = worker.try_recv() {
            match message {
                Message::Step(index, state, detail) => {
                    if let Some(step) = self.steps.get_mut(index) {
                        step.1 = state;
                        step.2 = detail;
                    }
                }
                Message::Installed(result) => {
                    self.page = result.map_or_else(Page::Failed, Page::Installed);
                }
                Message::Removed(result) => match result {
                    Ok(deferred) => {
                        self.delete_self = deferred;
                        self.page = Page::Removed;
                    }
                    Err(error) => self.page = Page::Failed(error),
                },
            }
        }
        if !matches!(self.page, Page::Working) {
            self.worker = None;
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        if self.icon.is_none()
            && let Some(image) = icon_image()
        {
            self.icon = Some(
                ui.ctx()
                    .load_texture("setup-icon", image, Default::default()),
            );
        }
        ui.horizontal(|ui| {
            if let Some(icon) = &self.icon {
                ui.add(egui::Image::new(egui::load::SizedTexture::new(
                    icon.id(),
                    vec2(46.0, 46.0),
                )));
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.add_space(3.0);
                ui.label(RichText::new("USB Crash Cart").size(21.0).strong());
                ui.label(
                    RichText::new(format!("Setup · version {}", install::VERSION)).color(MUTED),
                );
            });
        });
        ui.add_space(18.0);
    }

    fn item(ui: &mut egui::Ui, title: &str, detail: &str, kept: bool) {
        ui.horizontal(|ui| {
            let (glyph, color) = if kept { ("–", MUTED) } else { ("✓", GOOD) };
            ui.add_sized(
                [16.0, 18.0],
                egui::Label::new(RichText::new(glyph).color(color).size(15.0)),
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(title).strong());
                ui.label(RichText::new(detail).color(MUTED).size(13.0));
            });
        });
    }

    fn card(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
        egui::Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 12.0;
                content(ui);
            });
    }

    fn confirm(&mut self, ui: &mut egui::Ui) {
        let install = self.mode == Mode::Install;
        ui.label(
            RichText::new(if install {
                "Install USB Crash Cart"
            } else {
                "Remove USB Crash Cart"
            })
            .size(19.0)
            .strong(),
        );
        ui.label(
            RichText::new(if install {
                "Everything the adapter needs on this PC, set up in one pass."
            } else {
                "Takes the viewer off this PC. The adapter keeps working with other software."
            })
            .color(MUTED),
        );
        ui.add_space(10.0);
        let runtime = if self.runtime_present {
            "Already on this PC, so it is left as is"
        } else {
            "Microsoft runtime the adapter components rely on"
        };
        Self::card(ui, |ui| {
            if install {
                Self::item(
                    ui,
                    "USB Crash Cart Viewer",
                    "Sharp video, smooth zoom and full keyboard and mouse control",
                    false,
                );
                Self::item(
                    ui,
                    "Adapter driver",
                    "Digital Multitools ULC driver 2.80 for the StarTech adapter",
                    false,
                );
                Self::item(
                    ui,
                    "Adapter runtime",
                    "StarTech decoder and transport, private to the viewer",
                    false,
                );
                Self::item(ui, "Visual C++ 2008 runtime", runtime, self.runtime_present);
            } else {
                Self::item(
                    ui,
                    "Viewer and adapter runtime",
                    &self.dir.display().to_string(),
                    false,
                );
                Self::item(
                    ui,
                    "Shortcuts and Apps & features entry",
                    "Start menu and desktop",
                    false,
                );
                Self::item(
                    ui,
                    "Adapter driver stays installed",
                    "The original StarTech software keeps working",
                    true,
                );
                Self::item(
                    ui,
                    "Picture positions stay saved",
                    "Ready if you install the viewer again",
                    true,
                );
            }
        });
        ui.add_space(12.0);
        if install {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Location").color(MUTED));
                ui.label(
                    RichText::new(self.dir.display().to_string())
                        .monospace()
                        .size(12.5),
                );
            });
            ui.checkbox(&mut self.desktop, "Add a desktop shortcut");
            ui.horizontal(|ui| {
                ui.checkbox(
                    &mut self.accepted,
                    "I accept the StarTech license for the adapter components",
                );
                if ui.link("Read it").clicked() {
                    self.reading_license = true;
                }
            });
        }
        self.footer(ui, |setup, ui| {
            let ready = !install || (setup.accepted && install::license_text().is_some());
            if ui
                .add_enabled(ready, primary(if install { "Install" } else { "Remove" }))
                .clicked()
            {
                setup.start();
            }
            if ui.add(secondary("Cancel")).clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }

    fn progress(&self, ui: &mut egui::Ui) {
        Self::card(ui, |ui| {
            for (label, state, detail) in &self.steps {
                ui.horizontal(|ui| {
                    ui.add_sized([18.0, 18.0], |ui: &mut egui::Ui| match state {
                        State::Running => ui.add(egui::Spinner::new().size(14.0)),
                        State::Done => ui.label(RichText::new("✓").color(GOOD)),
                        State::Failed => ui.label(RichText::new("✕").color(BAD)),
                        State::Skipped => ui.label(RichText::new("–").color(MUTED)),
                        State::Pending => ui.label(RichText::new("○").color(BORDER)),
                    });
                    let color = if *state == State::Pending {
                        MUTED
                    } else {
                        TEXT
                    };
                    ui.label(RichText::new(*label).color(color));
                    if !detail.is_empty() && *state != State::Failed {
                        ui.label(RichText::new(detail).color(MUTED).size(13.0));
                    }
                });
            }
        });
    }

    fn footer(&mut self, ui: &mut egui::Ui, buttons: impl FnOnce(&mut Self, &mut egui::Ui)) {
        let bottom = ui.max_rect().bottom();
        let rect = egui::Rect::from_min_max(
            egui::pos2(ui.max_rect().left(), bottom - 40.0),
            ui.max_rect().right_bottom(),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                buttons(self, ui)
            });
        });
    }

    fn page(&mut self, ui: &mut egui::Ui) {
        self.header(ui);
        match &self.page {
            Page::Confirm => self.confirm(ui),
            Page::Working => {
                ui.label(
                    RichText::new(if self.mode == Mode::Install {
                        "Installing…"
                    } else {
                        "Removing…"
                    })
                    .size(19.0)
                    .strong(),
                );
                ui.label(
                    RichText::new("This takes less than a minute. Keep this window open.")
                        .color(MUTED),
                );
                ui.add_space(10.0);
                self.progress(ui);
            }
            Page::Installed(outcome) => {
                let restart = outcome.restart_required;
                ui.label(RichText::new("USB Crash Cart is ready").size(19.0).strong());
                ui.label(
                    RichText::new("Connect the adapter. If it was already plugged in, unplug it and plug it back in so Windows loads the driver.")
                        .color(MUTED),
                );
                if restart {
                    ui.label(
                        RichText::new("Restart Windows to finish the driver installation.")
                            .color(Color32::from_rgb(226, 195, 137)),
                    );
                }
                ui.add_space(10.0);
                self.progress(ui);
                self.footer(ui, |setup, ui| {
                    if ui.add(primary("Open USB Crash Cart")).clicked() {
                        launch(&setup.dir);
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.add(secondary("Close")).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            }
            Page::Removed => {
                ui.label(
                    RichText::new("USB Crash Cart was removed")
                        .size(19.0)
                        .strong(),
                );
                ui.label(
                    RichText::new("The adapter driver is still installed for other software.")
                        .color(MUTED),
                );
                ui.add_space(10.0);
                self.progress(ui);
                self.footer(ui, |_, ui| {
                    if ui.add(primary("Close")).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            }
            Page::Failed(error) => {
                let error = error.clone();
                ui.label(RichText::new("Setup stopped").size(19.0).strong());
                ui.label(
                    RichText::new("Steps after the one marked ✕ were not started.").color(MUTED),
                );
                ui.add_space(10.0);
                self.progress(ui);
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(90.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(error).color(BAD))
                                .selectable(true)
                                .wrap(),
                        );
                    });
                self.footer(ui, |setup, ui| {
                    if ui.add(primary("Try again")).clicked() {
                        setup.start();
                    }
                    if ui.add(secondary("Close")).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            }
        }
        if self.reading_license {
            let mut open = true;
            egui::Modal::new(egui::Id::new("license")).show(ui.ctx(), |ui| {
                ui.set_width(520.0);
                ui.label(
                    RichText::new("StarTech USB Crash Cart Adapter license")
                        .strong()
                        .size(16.0),
                );
                ui.label(
                    RichText::new("Covers the adapter driver and runtime included in this setup.")
                        .color(MUTED),
                );
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .max_height(330.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(
                                    install::license_text()
                                        .unwrap_or("License text missing from this setup."),
                                )
                                .monospace()
                                .size(11.5),
                            )
                            .selectable(true)
                            .wrap(),
                        );
                    });
                ui.add_space(8.0);
                if ui.add(secondary("Close")).clicked()
                    || ui.input(|input| input.key_pressed(egui::Key::Escape))
                {
                    open = false;
                }
            });
            self.reading_license = open;
        }
    }
}

/// Starts the viewer through Explorer so it runs without administrator rights.
fn launch(dir: &std::path::Path) {
    let link = Windows
        .shortcut_dirs(false)
        .into_iter()
        .map(|folder| folder.join("USB Crash Cart.lnk"))
        .find(|link| link.is_file());
    let target = link.unwrap_or_else(|| dir.join("cart-viewer.exe"));
    let _ = std::process::Command::new("explorer.exe")
        .arg(target)
        .spawn();
}

impl ApplicationHandler for Setup {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let result = (|| -> Result<()> {
            let attributes = Window::default_attributes()
                .with_title(match self.mode {
                    Mode::Install => "USB Crash Cart Setup",
                    Mode::Remove => "Remove USB Crash Cart",
                })
                .with_inner_size(LogicalSize::new(620.0, 600.0))
                .with_resizable(false)
                .with_enabled_buttons(
                    winit::window::WindowButtons::CLOSE | winit::window::WindowButtons::MINIMIZE,
                )
                .with_visible(false);
            let attributes = crate::chrome::attributes(
                attributes,
                [PANEL.r(), PANEL.g(), PANEL.b()],
                [TEXT.r(), TEXT.g(), TEXT.b()],
            )?;
            let window = Arc::new(event_loop.create_window(attributes)?);
            let render = pollster::block_on(crate::render::Render::new(window.clone()))?;
            self.egui_state = Some(egui_winit::State::new(
                self.context.clone(),
                egui::ViewportId::ROOT,
                &*window,
                Some(window.scale_factor() as f32),
                Some(winit::window::Theme::Dark),
                None,
            ));
            self.render = Some(render);
            window.set_visible(true);
            window.request_redraw();
            self.window = Some(window);
            Ok(())
        })();
        if let Err(error) = result {
            self.error = Some(format!("{error:#}"));
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };
        match &event {
            // Never leave a half-finished install behind.
            WindowEvent::CloseRequested if self.worker.is_none() => {
                event_loop.exit();
                return;
            }
            WindowEvent::Resized(size) => {
                if let Some(render) = &mut self.render {
                    render.resize(*size);
                }
            }
            WindowEvent::RedrawRequested => {
                self.poll();
                let raw = self.egui_state.as_mut().unwrap().take_egui_input(&window);
                let context = self.context.clone();
                let output = context.run(raw, |context| {
                    egui::CentralPanel::default()
                        .frame(
                            egui::Frame::new()
                                .fill(PANEL)
                                .inner_margin(egui::Margin::symmetric(28, 24)),
                        )
                        .show(context, |ui| self.page(ui));
                });
                let close = output
                    .viewport_output
                    .values()
                    .any(|viewport| viewport.commands.contains(&egui::ViewportCommand::Close));
                self.egui_state
                    .as_mut()
                    .unwrap()
                    .handle_platform_output(&window, output.platform_output.clone());
                if let Err(error) = self.render.as_mut().unwrap().draw(&context, output) {
                    self.error = Some(format!("{error:#}"));
                    event_loop.exit();
                }
                if close && self.worker.is_none() {
                    event_loop.exit();
                }
                return;
            }
            _ => {}
        }
        let response = self
            .egui_state
            .as_mut()
            .unwrap()
            .on_window_event(&window, &event);
        if response.repaint {
            window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.worker.is_some() {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(33),
            ));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}
