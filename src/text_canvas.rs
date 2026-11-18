//! Shared software text rendering for the overlay backends that paint their
//! own pixels directly instead of going through eframe/egui's own GPU
//! rendering at all: the Linux layer-shell overlay (`wayland_overlay.rs`)
//! and the Windows native layered-window overlay (`win_overlay.rs`). Both
//! need the exact same thing - rasterize "HITPOINTS: NN%" as premultiplied
//! RGBA onto a small canvas - so it lives here once instead of twice.
//!
//! This module has no platform-specific code at all (just `ab_glyph` doing
//! CPU font rasterization), so it's cheap to always compile, even on
//! platforms that don't currently use it.

use ab_glyph::{Font, FontVec, Glyph, PxScale, ScaleFont};

/// Canvas size used by both native overlay backends.
pub const WIDTH: u32 = 520;
pub const HEIGHT: u32 = 100;

pub static DEFAULT_FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/DejaVuSansMono.ttf");

/// A plain CPU RGBA canvas, kept free of any windowing-system types so the
/// drawing logic (and the font rasterization it drives) can be unit-tested
/// without a live compositor/window to check it against.
///
/// Pixels end up premultiplied by alpha: since every pixel starts fully
/// transparent `(0, 0, 0, 0)` and glyphs essentially never overlap each
/// other, the first (and typically only) blend onto any given pixel leaves
/// `rgba = true_color * coverage`, `a = coverage * 255` - i.e. already
/// premultiplied. That happens to be exactly the format both consumers
/// need: Wayland's `wl_shm` ARGB8888 format is premultiplied by protocol
/// convention, and Windows' `UpdateLayeredWindow` (with `AC_SRC_ALPHA`)
/// requires premultiplied input explicitly.
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        Canvas {
            width,
            height,
            rgba: vec![0u8; (width * height * 4) as usize],
        }
    }

    pub fn clear(&mut self) {
        self.rgba.fill(0);
    }

    /// Blend one glyph coverage sample (straight alpha) onto the RGBA buffer.
    fn blend(&mut self, x: i32, y: i32, color: Rgb, coverage: f32) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let idx = ((y as u32 * self.width + x as u32) * 4) as usize;
        let a = coverage.clamp(0.0, 1.0);
        let inv = 1.0 - a;
        self.rgba[idx] = (color.0 as f32 * a + self.rgba[idx] as f32 * inv) as u8;
        self.rgba[idx + 1] = (color.1 as f32 * a + self.rgba[idx + 1] as f32 * inv) as u8;
        self.rgba[idx + 2] = (color.2 as f32 * a + self.rgba[idx + 2] as f32 * inv) as u8;
        let existing_a = self.rgba[idx + 3] as f32;
        self.rgba[idx + 3] = (a * 255.0 + existing_a * inv).min(255.0) as u8;
    }
}

/// Tiny standalone RGB tuple so this module doesn't need to depend on egui
/// for anything beyond the one color type `draw_text` accepts.
#[derive(Clone, Copy)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub fn draw_text(buf: &mut Canvas, font: &FontVec, size: f32, color: egui::Color32, text: &str) {
    let color = Rgb(color.r(), color.g(), color.b());
    let scale = PxScale::from(size);
    let scaled = font.as_scaled(scale);

    let margin_x = 6.0f32;
    let margin_y = 4.0f32;
    let mut caret_x = margin_x;
    let baseline_y = margin_y + scaled.ascent();
    let mut last_glyph: Option<ab_glyph::GlyphId> = None;

    for c in text.chars() {
        let glyph_id = font.glyph_id(c);
        if let Some(last) = last_glyph {
            caret_x += scaled.kern(last, glyph_id);
        }
        let glyph: Glyph = glyph_id.with_scale_and_position(scale, ab_glyph::point(caret_x, baseline_y));
        let advance = scaled.h_advance(glyph_id);
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, coverage| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                buf.blend(px, py, color, coverage);
            });
        }
        caret_x += advance;
        last_glyph = Some(glyph_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders the exact same text-drawing path the live overlays use, with
    /// the embedded default font, and dumps it to a PNG. There's no live
    /// compositor/window in CI/sandboxes to verify the real overlay against,
    /// so this is the practical way to confirm the glyph rasterization and
    /// color/alpha blending actually look right, by eye, from the saved
    /// file.
    #[test]
    fn renders_hitpoints_text_to_png() {
        let font = FontVec::try_from_vec(DEFAULT_FONT_BYTES.to_vec()).expect("embedded font parses");

        let mut canvas = Canvas::new(WIDTH, HEIGHT);
        draw_text(
            &mut canvas,
            &font,
            48.0,
            egui::Color32::from_rgb(0xFF, 0x3B, 0x30),
            "HITPOINTS: 42%",
        );

        // Composite onto an opaque dark background so alpha is visible in a
        // plain viewer, same as it would look over a game window.
        let mut rgb = vec![0u8; (WIDTH * HEIGHT * 3) as usize];
        for i in 0..(WIDTH * HEIGHT) as usize {
            let a = canvas.rgba[i * 4 + 3] as f32 / 255.0;
            for c in 0..3 {
                let fg = canvas.rgba[i * 4 + c] as f32;
                let bg = 20.0; // near-black backdrop
                rgb[i * 3 + c] = (fg * a + bg * (1.0 - a)) as u8;
            }
        }

        let out_path = std::env::temp_dir().join("hpwarn_text_canvas_test.png");
        image::save_buffer(&out_path, &rgb, WIDTH, HEIGHT, image::ColorType::Rgb8)
            .expect("failed to save test PNG");
        println!("wrote {}", out_path.display());

        // Sanity check: some pixels should actually have been drawn on
        // (non-zero alpha), i.e. the font rasterized something.
        let any_ink = canvas.rgba.chunks(4).any(|p| p[3] > 0);
        assert!(any_ink, "expected the rendered text to produce visible pixels");
    }
}
