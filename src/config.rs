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

    /// Optional second, lower-HP "panic" beep. Off by default. Always kept
    /// strictly below `threshold` (never equal to or above the first beep's
    /// threshold) - see `max_second_threshold`/`clamp_second_beep`.
    #[serde(default)]
    pub second_beep_enabled: bool,

    /// Threshold for the optional second beep. Only takes effect while
    /// `second_beep_enabled` is true; always <= `threshold - 1`.
    #[serde(default = "default_second_threshold")]
    pub second_threshold: u8,
}

fn default_second_threshold() -> u8 {
    1
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
            second_beep_enabled: false,
            second_threshold: default_second_threshold(),
        }
    }
}

/// The highest value the second beep's threshold may ever take, given the
/// first beep's threshold: `first - 1`, floored at 0. When this is 0, there
/// is no valid value for the second threshold, so the second beep must be
/// impossible to enable.
pub fn max_second_threshold(first_threshold: u8) -> u8 {
    first_threshold.saturating_sub(1)
}

/// Whether the second beep can be enabled at all for a given first threshold.
pub fn second_beep_possible(first_threshold: u8) -> bool {
    max_second_threshold(first_threshold) >= 1
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

    /// Keeps `second_threshold`/`second_beep_enabled` consistent with
    /// `threshold`: clamps the second threshold to `max_second_threshold`,
    /// and forces the second beep off if that max is 0 (i.e. `threshold`
    /// is 1, leaving no room below it for a second beep).
    pub fn clamp_second_beep(&mut self) {
        let max = max_second_threshold(self.threshold);
        if max == 0 {
            self.second_beep_enabled = false;
            self.second_threshold = 0;
        } else if self.second_threshold > max {
            self.second_threshold = max;
        } else if self.second_threshold == 0 {
            self.second_threshold = 1;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_second_threshold_is_first_minus_one() {
        assert_eq!(max_second_threshold(50), 49);
        assert_eq!(max_second_threshold(2), 1);
    }

    #[test]
    fn second_beep_impossible_when_first_threshold_is_one() {
        assert_eq!(max_second_threshold(1), 0);
        assert!(!second_beep_possible(1));
        assert!(second_beep_possible(2));
    }

    #[test]
    fn clamp_second_beep_pulls_second_threshold_down_below_first() {
        let mut c = Config::default();
        c.threshold = 50;
        c.second_beep_enabled = true;
        c.second_threshold = 70;
        c.clamp_second_beep();
        assert_eq!(c.second_threshold, 49);
        assert!(c.second_beep_enabled);
    }

    #[test]
    fn clamp_second_beep_disables_when_first_threshold_is_one() {
        let mut c = Config::default();
        c.threshold = 1;
        c.second_beep_enabled = true;
        c.second_threshold = 1;
        c.clamp_second_beep();
        assert!(!c.second_beep_enabled);
        assert_eq!(c.second_threshold, 0);
    }

    #[test]
    fn old_config_without_second_beep_fields_still_loads() {
        // Simulates an existing user's config.toml saved before v1.8, which
        // has no second_beep_enabled/second_threshold keys at all. This must
        // NOT fall back to Config::default() for the whole struct - only the
        // missing fields should take their #[serde(default)] values.
        let text = r##"
            log_path = "/some/custom/path"
            threshold = 37
            font_style = "Proportional"
            font_size = 72.0
            color = "#112233"
            pos_x = 10.0
            pos_y = 20.0
        "##;
        let c: Config = toml::from_str(text).expect("old config without v1.8 fields should parse");
        assert_eq!(c.log_path, "/some/custom/path");
        assert_eq!(c.threshold, 37);
        assert_eq!(c.font_size, 72.0);
        assert_eq!(c.color, "#112233");
        assert!(!c.second_beep_enabled);
        assert_eq!(c.second_threshold, 1);
    }
}
