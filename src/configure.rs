use crate::config::{Config, FontStyle};
use eframe::egui;
use std::path::PathBuf;

pub struct ConfigureApp {
    config_path: PathBuf,
    config: Config,
    saved_notice: Option<std::time::Instant>,
}

impl ConfigureApp {
    pub fn new(config_path: PathBuf) -> Self {
        let config = Config::load(&config_path);
        ConfigureApp {
            config_path,
            config,
            saved_notice: None,
        }
    }

    fn save(&mut self) {
        if let Err(e) = self.config.save(&self.config_path) {
            eprintln!("hpwarn: failed to save config: {e}");
        } else {
            self.saved_notice = Some(std::time::Instant::now());
        }
    }
}

impl eframe::App for ConfigureApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("hpwarn settings");
            ui.label("The overlay process reloads this file automatically within a second.");
            ui.separator();

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
                ui.label("Position (also settable by dragging the overlay itself):");
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
                "Preview, in your chosen font/colour/size (threshold {}%):",
                self.config.threshold
            ));
            let family = match self.config.font_style {
                FontStyle::Monospace => egui::FontFamily::Monospace,
                FontStyle::Proportional => egui::FontFamily::Proportional,
            };
            ui.label(
                egui::RichText::new("HITPOINTS: 42%")
                    .font(egui::FontId::new(self.config.font_size.min(64.0), family))
                    .color(self.config.color32()),
            );
        });
    }
}
