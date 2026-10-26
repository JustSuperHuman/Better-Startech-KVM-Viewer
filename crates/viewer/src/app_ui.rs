use super::*;

impl App {
    pub(super) fn ui(&mut self) {
        let context = self.context.clone();
        egui::TopBottomPanel::top("controls")
            .exact_height(52.0)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin::symmetric(12, 9)))
            .show(&context, |ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                ui.spacing_mut().button_padding = egui::vec2(8.0, 5.0);
                ui.spacing_mut().interact_size.y = 30.0;
                let wide = ui.available_width() >= 1010.0;
                let drive_visible = ui.available_width() >= 905.0;
                let medium = ui.available_width() >= 805.0;
                ui.horizontal(|ui| {
                    self.connection_control(ui);
                    ui.separator();
                    for (mode, text) in [(InteractionMode::Control, "Control"), (InteractionMode::Navigate, "Navigate")] {
                        if ui.add(egui::Button::new(text).selected(self.interaction == mode))
                            .on_hover_text(if mode == InteractionMode::Control {
                                "Automatically controls the target while the pointer is over video. F12 switches modes."
                            } else {
                                "Keep input local. Ctrl+scroll zooms; drag pans. F12 returns to Control."
                            }).clicked() {
                            self.set_interaction(mode);
                        }
                    }
                    ui.separator();
                    for (mode, text) in [(ScaleMode::Fit, "Fit"), (ScaleMode::ActualPixels, "1:1")] {
                        if ui.selectable_label(self.navigation.is_base_view(mode), text)
                            .on_hover_text(if mode == ScaleMode::Fit { "Show the entire captured frame" } else { "One source pixel per physical display pixel" })
                            .clicked() {
                            self.reset_view(mode);
                        }
                    }
                    if ui.small_button("−").on_hover_text("Zoom out").clicked()
                        && let Some(view) = self.viewport {
                        self.navigation.zoom_center(view, false);
                    }
                    let percentage = self.viewport.map_or(100.0, |view| view.scale() * 100.0);
                    ui.add_sized([46.0, 28.0], egui::Label::new(RichText::new(format!("{percentage:.0}%")).monospace().color(TEXT)));
                    if ui.small_button("+").on_hover_text("Zoom in").clicked()
                        && let Some(view) = self.viewport {
                        self.navigation.zoom_center(view, true);
                    }
                    ui.separator();
                    if wide && ui.add_enabled(self.input_ready(), egui::Button::new("Paste…")).clicked() {
                        self.read_clipboard();
                    }
                    let stay_open = egui::containers::menu::MenuConfig::new()
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
                    if drive_visible {
                        let (button, _) =
                            egui::containers::menu::MenuButton::new(self.drive_menu_label())
                                .config(stay_open.clone())
                                .ui(ui, |ui| self.drive_controls(ui));
                        if std::mem::take(&mut self.drive.open_once) {
                            egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&button));
                        }
                    }
                    if medium {
                        egui::containers::menu::MenuButton::new("Display")
                            .config(stay_open.clone())
                            .ui(ui, |ui| self.display_controls(ui));
                    }
                    egui::containers::menu::MenuButton::new("More").config(stay_open).ui(ui, |ui| {
                        ui.set_min_width(200.0);
                        if !wide && ui.add_enabled(self.input_ready(), egui::Button::new("Paste clipboard…")).clicked() {
                            ui.close();
                            self.read_clipboard();
                        }
                        if !drive_visible {
                            ui.menu_button(self.drive_menu_label(), |ui| self.drive_controls(ui));
                        }
                        if !medium {
                            ui.menu_button("Display", |ui| self.display_controls(ui));
                        }
                        self.special_controls(ui);
                        self.input_controls(ui);
                        ui.separator();
                        if ui.button("Reset view").clicked() {
                            self.reset_view(self.navigation.mode);
                            ui.close();
                        }
                        if ui.button(if self.fullscreen { "Exit fullscreen" } else { "Fullscreen" }).clicked() {
                            self.toggle_fullscreen();
                            ui.close();
                        }
                    });
                    if ui.button(if self.fullscreen { "↙" } else { "⛶" }).on_hover_text("Toggle fullscreen").clicked() {
                        self.toggle_fullscreen();
                    }
                });
            });

        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(&context, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let (label, color) = self.connection_label();
                    ui.label(RichText::new("●").size(10.0).color(color));
                    ui.label(RichText::new(label).color(color));
                    if let Some(source) = self.source {
                        ui.separator();
                        ui.label(
                            RichText::new(format!("{} × {}", source.width, source.height))
                                .color(MUTED),
                        );
                    }
                    if let Some((summary, color)) = self.drive_summary() {
                        ui.separator();
                        ui.label(RichText::new(summary).color(color));
                    }
                    ui.separator();
                    if self.is_pasting() {
                        ui.label(format!(
                            "Typing {} / {}",
                            self.status.paste_done.unwrap_or(0),
                            self.status.paste_total.unwrap_or(0)
                        ));
                        if ui.button("Cancel paste").clicked() {
                            self.release_input(true);
                        }
                    } else {
                        let text = if self.options.smoke_test && !self.options.demo {
                            "Video check · target input disabled"
                        } else if self.middle_pan.owns_release()
                            && !self.middle_pan.active()
                            && !self
                                .host_buttons
                                .contains(&winit::event::MouseButton::Middle)
                        {
                            "Click the middle button once to resume control"
                        } else if self.interaction == InteractionMode::Navigate {
                            "Ctrl+scroll to zoom · Drag or middle-drag to pan · F12 to control"
                        } else if self.captured && self.remote_middle {
                            "Controlling target · Middle button goes to target · F12 to navigate"
                        } else if self.captured {
                            "Controlling target · Middle-drag to pan · F12 to navigate"
                        } else if self.input_ready() {
                            "Move over video to control · Middle-drag to pan · F12 to navigate"
                        } else {
                            "Target input paused"
                        };
                        ui.label(RichText::new(text).color(if self.captured {
                            TEXT
                        } else {
                            MUTED
                        }));
                    }
                });
                if let Some(message) = self.error.clone().or_else(|| {
                    (self.status.state.as_deref() == Some("error"))
                        .then(|| self.status.message.clone())
                        .flatten()
                }) {
                    ui.add_space(5.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(message).color(Color32::from_rgb(240, 165, 157)));
                        if !self.helper_fault && ui.small_button("Dismiss").clicked() {
                            self.error = None;
                        }
                    });
                }
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::from_rgb(9, 12, 17)))
            .show(&context, |ui| {
                let rect = ui.max_rect();
                let ppp = f64::from(context.pixels_per_point());
                let can_pan = self.interaction == InteractionMode::Navigate
                    && !self.is_pasting() && self.preview.is_none()
                    && !self.middle_pan.owns_release()
                    && !egui::Popup::is_any_open(&context);
                let response = ui.interact(rect, ui.id().with("video"), if can_pan { egui::Sense::drag() } else { egui::Sense::hover() });
                if can_pan && response.dragged_by(egui::PointerButton::Primary) {
                    let delta = response.drag_delta();
                    self.navigation.pan_by(PhysicalOffset { x: f64::from(delta.x) * ppp, y: f64::from(delta.y) * ppp }, self.viewport);
                }
                if let Some(elapsed) = self.navigation_delta.take() {
                    self.navigation.advance(elapsed);
                }
                let bounds = PhysicalRect {
                    x: f64::from(rect.min.x) * ppp, y: f64::from(rect.min.y) * ppp,
                    width: f64::from(rect.width()) * ppp, height: f64::from(rect.height()) * ppp,
                };
                self.viewport = self.source.and_then(|source| self.navigation.viewport(source, bounds));
                if let Some(view) = self.viewport
                    && let Some(id) = self.graphics.as_ref().and_then(Graphics::texture_id) {
                    let image = view.image_rect();
                    let image_rect = egui::Rect::from_min_size(
                        egui::pos2((image.x / ppp) as f32, (image.y / ppp) as f32),
                        egui::vec2((image.width / ppp) as f32, (image.height / ppp) as f32),
                    );
                    ui.painter().with_clip_rect(rect).image(id, image_rect,
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
                    if !self.options.demo {
                        if self.status.state.as_deref() == Some("no_video") {
                            overlay(ui, rect, "No video signal", "Check the target's VGA output, or wake a sleeping display.");
                            self.wake_button(ui, rect);
                        } else if self.helper_fault || self.status.state.as_deref() != Some("viewing") {
                            overlay(ui, rect, "Video paused", "The last received frame is shown. Check the adapter connection.");
                        } else if self.last_frame.is_some_and(|time| time.elapsed() > Duration::from_secs(3)) {
                            overlay(ui, rect, "Waiting for video", "The adapter has not supplied a new frame for more than three seconds.");
                        }
                    }
                } else {
                    let no_video = self.status.state.as_deref() == Some("no_video");
                    let (title, subtitle) = if no_video {
                        ("No video signal", "Check the target's VGA output, or wake a sleeping display.")
                    } else if self.helper.is_some() {
                        ("Connecting to adapter", "Video and keyboard status will appear here when the target is ready.")
                    } else {
                        ("Connect an adapter", "Choose Connect to view and control your target.")
                    };
                    overlay(ui, rect, title, subtitle);
                    if no_video {
                        self.wake_button(ui, rect);
                    }
                }
                self.drop_overlay(ui, rect);
            });
        self.paste_dialog(&context);
    }

    fn connection_label(&self) -> (&'static str, Color32) {
        if self.options.demo {
            ("Demo · no device", Color32::from_rgb(218, 188, 126))
        } else if self.error.is_some()
            || self.helper_fault
            || matches!(self.status.state.as_deref(), Some("error" | "closed"))
        {
            ("Needs attention", Color32::from_rgb(240, 165, 157))
        } else if self.status.state.as_deref() == Some("no_video") {
            ("No video signal", Color32::from_rgb(218, 188, 126))
        } else if self.viewing() {
            ("Video active", Color32::from_rgb(153, 207, 176))
        } else if self.helper.is_some() {
            ("Connecting", MUTED)
        } else {
            ("Disconnected", MUTED)
        }
    }

    fn connection_control(&mut self, ui: &mut egui::Ui) {
        if self.options.demo {
            ui.label(
                RichText::new("DEMO")
                    .strong()
                    .color(Color32::from_rgb(218, 188, 126)),
            );
        } else if self.helper.is_some() {
            let (text, _) = self.connection_label();
            ui.menu_button(text, |ui| {
                if ui.button("Disconnect adapter").clicked() {
                    ui.close();
                    self.disconnect();
                }
            });
        } else if ui.button("Connect").clicked() {
            self.start_helper();
        }
    }

    fn display_controls(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(220.0);
        ui.label(RichText::new("Image sampling").strong());
        ui.label(RichText::new("Filtered downscaling is always enabled.").color(MUTED));
        ui.checkbox(&mut self.nearest, "Crisp enlargement")
            .on_hover_text(
                "Keep pixels distinct above native size. Reduced images remain smoothly filtered.",
            );
        ui.separator();
        if ui.button("Fit entire frame").clicked() {
            self.reset_view(ScaleMode::Fit);
            ui.close();
        }
        if ui.button("Actual pixels · 1:1").clicked() {
            self.reset_view(ScaleMode::ActualPixels);
            ui.close();
        }
        ui.separator();
        self.position_controls(ui);
    }

    /// The original Video Settings position arrows, Auto and Default, with the
    /// chosen position remembered per video mode on this computer.
    fn position_controls(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Picture position").strong());
        let Some(mode) = self.position_mode() else {
            ui.label(RichText::new("Available while video is active.").color(MUTED));
            return;
        };
        let saved = self.settings.position(mode);
        let (x, y) = saved.unwrap_or((
            self.status.position_x.unwrap_or(0),
            self.status.position_y.unwrap_or(0),
        ));
        let busy = self.position_busy();
        ui.add_enabled_ui(!busy, |ui| {
            egui::Grid::new("picture_position").num_columns(4).spacing([6.0, 6.0]).show(ui, |ui| {
                ui.label("Horizontal");
                if ui.small_button("◀").on_hover_text("Move the picture left").clicked() {
                    self.step_position(1, 0);
                }
                ui.add_sized([58.0, 22.0], egui::Label::new(RichText::new(format!("{x:+} px")).monospace()));
                if ui.small_button("▶").on_hover_text("Move the picture right").clicked() {
                    self.step_position(-1, 0);
                }
                ui.end_row();
                ui.label("Vertical");
                if ui.small_button("▲").on_hover_text("Move the picture up").clicked() {
                    self.step_position(0, 1);
                }
                ui.add_sized([58.0, 22.0], egui::Label::new(RichText::new(format!("{y:+} px")).monospace()));
                if ui.small_button("▼").on_hover_text("Move the picture down").clicked() {
                    self.step_position(0, -1);
                }
                ui.end_row();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Step").color(MUTED));
                for step in [1, 8] {
                    if ui.selectable_label(self.position_step == step, format!("{step} px")).clicked() {
                        self.position_step = step;
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Auto align").on_hover_text("The original adapter's automatic alignment. Works best with content reaching every edge.").clicked() {
                    self.default_position(true);
                }
                if ui.button("Reset").on_hover_text("Return to the adapter's default position for this mode").clicked() {
                    self.default_position(false);
                }
            });
        });
        let (width, height, hz) = mode;
        let note = if self.status.aligning == Some(true) {
            "Aligning picture…".to_owned()
        } else if saved.is_some() {
            format!("Remembered for {width} × {height} at {hz} Hz")
        } else {
            format!("Adapter default for {width} × {height} at {hz} Hz")
        };
        ui.label(RichText::new(note).color(MUTED));
    }

    fn input_controls(&mut self, ui: &mut egui::Ui) {
        let mut remote = self.remote_middle;
        let changeable = !self.middle_pan.owns_release()
            && !self
                .host_buttons
                .contains(&winit::event::MouseButton::Middle);
        if ui
            .add_enabled(changeable, egui::Checkbox::new(&mut remote, "Send middle button to target"))
            .on_hover_text("In Control mode, the middle button clicks and drags on the target instead of panning. Navigate mode always pans.")
            .changed()
        {
            self.set_remote_middle(remote);
        }
        if ui
            .add_enabled(self.can_wake(), egui::Button::new("Wake display · press Shift"))
            .on_hover_text("Sends one Shift key press. Works without video, so a sleeping target display can wake.")
            .clicked()
        {
            self.wake_target();
            ui.close();
        }
    }

    fn wake_button(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if !self.can_wake() {
            return;
        }
        let button = egui::Rect::from_center_size(
            rect.center() + egui::vec2(0.0, 112.0),
            egui::vec2(220.0, 34.0),
        );
        if ui
            .put(button, egui::Button::new("Wake display · press Shift"))
            .on_hover_text("Sends one Shift key press to the target")
            .clicked()
        {
            self.wake_target();
        }
    }

    fn special_controls(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(self.input_ready(), |ui| {
            ui.menu_button("Special keys", |ui| {
                for (label, keys) in [
                    ("Send F12 to target", &[0x45][..]),
                    ("Ctrl + Alt + Delete", &[0xe0, 0xe2, 0x4c][..]),
                    ("Ctrl + Alt + Backspace", &[0xe0, 0xe2, 0x2a][..]),
                    ("Print Screen", &[0x46][..]),
                    ("Pause / Break", &[0x48][..]),
                ] {
                    if ui.button(label).clicked() {
                        self.special_key(keys);
                        ui.close();
                    }
                }
            });
        });
    }

    fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        self.middle_pan.cancel();
        self.navigation.cancel(self.viewport);
        self.viewport = None;
        if let Some(window) = &self.window {
            window.set_fullscreen(self.fullscreen.then_some(Fullscreen::Borderless(None)));
        }
    }
}
