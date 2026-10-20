use crate::config::Config;
use crate::state::SharedState;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct OverlayApp {
    shared: Arc<SharedState>,
    config_path: PathBuf,
    config: Config,
    config_mtime: Option<std::time::SystemTime>,
    last_config_check: Instant,
    custom_font_loaded: Option<String>,
}

impl OverlayApp {
    pub fn new(shared: Arc<SharedState>, config_path: PathBuf, config: Config) -> Self {
        OverlayApp {
            shared,
            config_path,
            config,
            config_mtime: None,
            last_config_check: Instant::now() - Duration::from_secs(10),
            custom_font_loaded: None,
        }
    }

    fn maybe_reload_config(&mut self) {
        if self.last_config_check.elapsed() < Duration::from_millis(750) {
            return;
        }
        self.last_config_check = Instant::now();

        let Ok(meta) = std::fs::metadata(&self.config_path) else { return };
        let Ok(modified) = meta.modified() else { return };
        if self.config_mtime == Some(modified) {
            return;
        }
        self.config_mtime = Some(modified);
        self.config = Config::load(&self.config_path);
        self.shared.set_threshold(self.config.threshold);
        self.shared.set_log_path(self.config.log_path.clone());
    }

    fn apply_custom_font(&mut self, ctx: &egui::Context) {
        let Some(path) = self.config.font_file.clone() else { return };
        if self.custom_font_loaded.as_deref() == Some(path.as_str()) {
            return;
        }
        match std::fs::read(&path) {
            Ok(bytes) => {
                let mut fonts = egui::FontDefinitions::default();
                fonts.font_data.insert(
                    "custom".to_owned(),
                    egui::FontData::from_owned(bytes).into(),
                );
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
}

impl eframe::App for OverlayApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // fully transparent background
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.maybe_reload_config();
        self.apply_custom_font(ctx);

        // Some window managers only honour "always on top" once, at window
        // creation, and silently drop it the first time another window gets
        // focus. Re-asserting it every frame is cheap and keeps the overlay
        // stacked above the game even after alt-tabbing.
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::AlwaysOnTop,
        ));

        let pct = *self.shared.pct.lock().unwrap();
        let threshold = self.config.threshold;
        let show = matches!(pct, Some(p) if p <= threshold);

        let panel_frame = egui::Frame::none().fill(egui::Color32::TRANSPARENT);
        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let drag_id = egui::Id::new("hpwarn_drag_area");
                let response = ui.interact(rect, drag_id, egui::Sense::click_and_drag());

                if response.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if response.drag_stopped() {
                    let outer_rect = ctx.input(|i| i.viewport().outer_rect);
                    if let Some(rect) = outer_rect {
                        self.config.pos_x = rect.left();
                        self.config.pos_y = rect.top();
                        let _ = self.config.save(&self.config_path);
                        if let Ok(meta) = std::fs::metadata(&self.config_path) {
                            self.config_mtime = meta.modified().ok();
                        }
                    }
                }

                if show {
                    let family = match self.config.font_style {
                        crate::config::FontStyle::Monospace => egui::FontFamily::Monospace,
                        crate::config::FontStyle::Proportional => egui::FontFamily::Proportional,
                    };
                    let text = format!("HITPOINTS: {}%", pct.unwrap_or(0));
                    ui.painter().text(
                        rect.left_top() + egui::vec2(6.0, 4.0),
                        egui::Align2::LEFT_TOP,
                        text,
                        egui::FontId::new(self.config.font_size, family),
                        self.config.color32(),
                    );
                }
            });

        // Light refresh rate: plenty responsive for a text readout, cheap on CPU.
        ctx.request_repaint_after(Duration::from_millis(80));
    }
}
