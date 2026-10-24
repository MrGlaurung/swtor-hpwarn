use crate::config::{Config, FontStyle};
use crate::state::SharedState;
use eframe::egui;
use egui_plot::{HLine, Line, Plot, PlotPoints};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The always-on dashboard: a configuration panel on the left, and an
/// HP-change history + curve graph on the right. This is what `hpwarn`
/// shows by default while the overlay (and the log watcher) keep running
/// in the background.
pub struct DashboardApp {
    config_path: PathBuf,
    config: Config,
    shared: Arc<SharedState>,
    saved_notice: Option<Instant>,
    /// Which font_file path (if any) has been loaded into this window's
    /// egui context already, so the preview actually renders in the
    /// selected custom font instead of always falling back to egui's
    /// built-in Monospace/Proportional fonts.
    custom_font_loaded: Option<String>,
}

impl DashboardApp {
    pub fn new(config_path: PathBuf, config: Config, shared: Arc<SharedState>) -> Self {
        DashboardApp {
            config_path,
            config,
            shared,
            saved_notice: None,
            custom_font_loaded: None,
        }
    }

    /// Mirrors `OverlayApp::apply_custom_font`: loads `config.font_file`
    /// into this window's own egui context (each window/viewport has its
    /// own `Context`, so this doesn't affect the overlay or vice versa),
    /// so the live preview renders in the actual selected font.
    fn apply_custom_font(&mut self, ctx: &egui::Context) {
        let Some(path) = self.config.font_file.clone() else {
            if self.custom_font_loaded.is_some() {
                // A custom font was cleared - restore egui's defaults.
                ctx.set_fonts(egui::FontDefinitions::default());
                self.custom_font_loaded = None;
            }
            return;
        };
        if self.custom_font_loaded.as_deref() == Some(path.as_str()) {
            return;
        }
        match std::fs::read(&path) {
            Ok(bytes) => {
                let mut fonts = egui::FontDefinitions::default();
                fonts
                    .font_data
                    .insert("custom".to_owned(), egui::FontData::from_owned(bytes).into());
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "custom".to_owned());
                fonts
                    .families
                    .entry(egui::FontFamily::Monospace)
                    .or_default()
                    .insert(0, "custom".to_owned());
                ctx.set_fonts(fonts);
                self.custom_font_loaded = Some(path);
            }
            Err(e) => {
                eprintln!("hpwarn: could not load font_file {path}: {e}");
                self.custom_font_loaded = Some(path); // don't retry every frame
            }
        }
    }

    fn save(&mut self) {
        // Push into the shared state immediately so the overlay/watcher
        // don't have to wait out their own config-file hot-reload poll.
        self.shared.set_threshold(self.config.threshold);
        self.shared.set_log_path(self.config.log_path.clone());
        if let Err(e) = self.config.save(&self.config_path) {
            eprintln!("hpwarn: failed to save config: {e}");
        } else {
            self.saved_notice = Some(Instant::now());
        }
    }

    fn config_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Configuration");
        ui.add_space(4.0);

        let mut changed = false;

        ui.label("SWTOR combat log file, or the CombatLogs folder to auto-follow the newest one:");
        changed |= ui.text_edit_singleline(&mut self.config.log_path).changed();
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label("Show HP when at or below:");
            let mut t = self.config.threshold as f32;
            if ui.add(egui::Slider::new(&mut t, 1.0..=99.0).suffix("%")).changed() {
                self.config.threshold = t.round() as u8;
                changed = true;
            }
        });
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label("Font:");
            changed |= ui
                .radio_value(&mut self.config.font_style, FontStyle::Monospace, "Monospace")
                .changed();
            changed |= ui
                .radio_value(&mut self.config.font_style, FontStyle::Proportional, "Proportional")
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label("Or a custom font file (.ttf/.otf), optional:");
            let mut text = self.config.font_file.clone().unwrap_or_default();
            if ui.text_edit_singleline(&mut text).changed() {
                self.config.font_file = if text.trim().is_empty() { None } else { Some(text) };
                changed = true;
            }
        });
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label("Font size:");
            changed |= ui
                .add(egui::Slider::new(&mut self.config.font_size, 12.0..=160.0))
                .changed();
        });
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label("Colour:");
            let mut c = self.config.color32();
            let mut rgb = [c.r(), c.g(), c.b()];
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                self.config.color = format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b());
                changed = true;
            }
        });
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label("Overlay position (also settable by dragging it, on X11):");
            changed |= ui.add(egui::DragValue::new(&mut self.config.pos_x).prefix("x: ")).changed();
            changed |= ui.add(egui::DragValue::new(&mut self.config.pos_y).prefix("y: ")).changed();
        });

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                self.save();
            }
            if let Some(t) = self.saved_notice {
                if t.elapsed().as_secs() < 2 {
                    ui.label("Saved.");
                }
            }
        });

        if changed {
            self.save();
        }

        ui.add_space(12.0);
        ui.separator();
        ui.label(format!(
            "Live preview, in your chosen font/colour/size (threshold {}%):",
            self.config.threshold
        ));
        let family = match self.config.font_style {
            FontStyle::Monospace => egui::FontFamily::Monospace,
            FontStyle::Proportional => egui::FontFamily::Proportional,
        };
        let live_pct = *self.shared.pct.lock().unwrap();
        let preview_text = match live_pct {
            Some(p) => format!("HITPOINTS: {p}%"),
            None => "HITPOINTS: --%".to_string(),
        };
        ui.label(
            egui::RichText::new(preview_text)
                .font(egui::FontId::new(self.config.font_size.min(64.0), family))
                .color(self.config.color32()),
        );

        ui.add_space(12.0);
        ui.separator();
        let status = self.shared.status.lock().unwrap().clone();
        let pct = *self.shared.pct.lock().unwrap();
        ui.label(format!("Watcher status: {status}"));
        match pct {
            Some(p) => ui.label(format!("Current HP: {p}%")),
            None => ui.label("Current HP: unknown yet"),
        };
    }

    fn history_panel(&self, ui: &mut egui::Ui) {
        ui.heading("History");
        ui.add_space(4.0);

        // Curve graph: HP% over "events" (not time) - a new point only
        // when HP actually changed, per the user's request.
        let points: Vec<[f64; 2]> = {
            let curve = self.shared.curve.lock().unwrap();
            curve
                .iter()
                .enumerate()
                .map(|(i, &pct)| [i as f64, pct as f64])
                .collect()
        };
        Plot::new("hp_curve")
            .height(180.0)
            .include_y(0.0)
            .include_y(100.0)
            .show_axes([false, true])
            .allow_scroll(false)
            .show(ui, |plot_ui| {
                // Fat reference lines at 0/50/100% so they're easy to spot
                // at a glance against the thinner HP curve.
                let gridline_color = egui::Color32::from_gray(110);
                for y in [0.0, 50.0, 100.0] {
                    plot_ui.hline(HLine::new(y).width(2.5_f32).color(gridline_color));
                }
                if !points.is_empty() {
                    plot_ui.line(Line::new(PlotPoints::from(points)).name("HP %"));
                }
            });

        ui.add_space(8.0);
        ui.separator();
        ui.label("HP changes (newest first):");

        let history = self.shared.history.lock().unwrap();
        egui::ScrollArea::vertical()
            .max_height(ui.available_height())
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if history.is_empty() {
                    ui.weak("No HP changes recorded yet.");
                }
                for entry in history.iter() {
                    let color = if entry.delta >= 0 {
                        egui::Color32::from_rgb(90, 200, 90)
                    } else {
                        egui::Color32::from_rgb(230, 90, 90)
                    };
                    ui.horizontal(|ui| {
                        ui.monospace(
                            egui::RichText::new(&entry.timestamp).color(egui::Color32::GRAY),
                        );
                        ui.colored_label(color, &entry.label);
                        ui.weak(format!("({} / {} - {}%)", entry.cur, entry.max, entry.pct));
                    });
                }
            });
    }
}

impl eframe::App for DashboardApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Repaint regularly even with no user input, since HP/history/curve
        // change from the background watcher thread, not from UI events.
        ctx.request_repaint_after(Duration::from_millis(200));

        self.apply_custom_font(ctx);

        egui::SidePanel::left("config_panel")
            .resizable(true)
            .default_width(380.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.config_panel(ui);
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.history_panel(ui);
        });
    }
}
