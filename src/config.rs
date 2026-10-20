use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FontStyle {
    Monospace,
    Proportional,
}

impl Default for FontStyle {
    fn default() -> Self {
        FontStyle::Monospace
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Path to the SWTOR CombatLogs folder, or a specific .txt log file.
    /// If a folder is given, the newest `combat_*.txt` inside it is followed,
    /// and hpwarn automatically switches to a newer file if the game starts one.
    pub log_path: String,

    /// Show the HP readout whenever current HP% is at or below this value.
    pub threshold: u8,

    /// Built-in font choice. For full control, set `font_file` to a .ttf/.otf path instead.
    pub font_style: FontStyle,

    /// Optional path to a custom .ttf/.otf font file. Overrides `font_style` when set.
    pub font_file: Option<String>,

    pub font_size: f32,

    /// Text colour as "#RRGGBB".
    pub color: String,

    /// Top-left position of the overlay window, in screen pixels.
    pub pos_x: f32,
    pub pos_y: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            log_path: default_log_dir(),
            threshold: 50,
            font_style: FontStyle::Monospace,
            font_file: None,
            font_size: 48.0,
            color: "#FF3B30".to_string(),
            pos_x: 100.0,
            pos_y: 100.0,
        }
    }
}

/// Best-effort default guess for where SWTOR writes its CombatLogs, covering a
/// normal Steam/Proton install. The user can always override this in --configure.
fn default_log_dir() -> String {
    if let Some(home) = dirs::home_dir() {
        let candidates = [
            home.join(".local/share/Steam/steamapps/compatdata")
                .join("1286830/pfx/drive_c/users/steamuser/Documents/Star Wars - The Old Republic/CombatLogs"),
            home.join("Documents/Star Wars - The Old Republic/CombatLogs"),
        ];
        for c in candidates {
            if c.exists() {
                return c.to_string_lossy().to_string();
            }
        }
    }
    String::new()
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hpwarn")
}

pub fn default_config_path() -> PathBuf {
    config_dir().join("config.toml")
}

impl Config {
    pub fn load(path: &PathBuf) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self, path: &PathBuf) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    pub fn color32(&self) -> egui::Color32 {
        parse_hex_color(&self.color).unwrap_or(egui::Color32::from_rgb(255, 59, 48))
    }
}

pub fn parse_hex_color(s: &str) -> Option<egui::Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(egui::Color32::from_rgb(r, g, b))
}
