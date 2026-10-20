//! Native Wayland overlay, using the wlr-layer-shell protocol.
//!
//! On Wayland there is no protocol for an ordinary application window to
//! demand "always on top" the way X11 window managers allow. Desktop-level
//! workarounds (e.g. KDE's Window Rules "Keep above") only affect *normal*
//! windows, and compositors commonly stop compositing entirely for a window
//! that looks fullscreen to save GPU work (SWTOR's window under Wine/Proton
//! does this even in "Borderless Windowed" mode) — at which point nothing,
//! from any app, can draw above it.
//!
//! The real mechanism panels, docks, and on-screen-display tools use is a
//! *layer-shell surface* (`zwlr_layer_shell_v1`) placed on the `Overlay`
//! layer, which by protocol definition composites above everything,
//! including fullscreen content. This is the same approach the Baras DPS
//! meter's own Linux overlay uses. We draw our one line of text ourselves,
//! directly into a shared-memory buffer — no GPU, no toolkit needed.

use crate::config::Config;
use crate::state::SharedState;
use ab_glyph::{Font, FontVec, Glyph, PxScale, ScaleFont};
use rustix::fs::{memfd_create, MemfdFlags};
use rustix::mm::{mmap, munmap, MapFlags, ProtFlags};
use std::fmt;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry;
use wayland_client::protocol::wl_shm::{Format, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{globals::registry_queue_init, globals::GlobalListContents};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    Layer, ZwlrLayerShellV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1,
};

const WIDTH: u32 = 520;
const HEIGHT: u32 = 100;

static DEFAULT_FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/DejaVuSansMono.ttf");

#[derive(Debug)]
pub struct LayerShellError(pub String);

impl fmt::Display for LayerShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for LayerShellError {}

macro_rules! empty_dispatch {
    ($proxy:ty, $data:ty) => {
        impl Dispatch<$proxy, $data> for AppState {
            fn event(
                _: &mut Self,
                _: &$proxy,
                _: <$proxy as wayland_client::Proxy>::Event,
                _: &$data,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

struct ShmMapping {
    ptr: *mut u8,
    size: usize,
}
// SAFETY: only touched from this module's single-threaded event loop.
unsafe impl Send for ShmMapping {}

struct AppState {
    configured: bool,
    closed: bool,
    width: u32,
    height: u32,
}

empty_dispatch!(wl_registry::WlRegistry, GlobalListContents);
empty_dispatch!(WlCompositor, ());
empty_dispatch!(WlSurface, ());
empty_dispatch!(WlRegion, ());
empty_dispatch!(WlShm, ());
empty_dispatch!(WlShmPool, ());
empty_dispatch!(WlBuffer, ());
empty_dispatch!(ZwlrLayerShellV1, ());

impl Dispatch<ZwlrLayerSurfaceV1, ()> for AppState {
    fn event(
        state: &mut Self,
        proxy: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                proxy.ack_configure(serial);
                if width > 0 && height > 0 {
                    state.width = width;
                    state.height = height;
                }
                state.configured = true;
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.closed = true;
            }
            _ => {}
        }
    }
}

/// Runs the Wayland-native overlay. Blocks until the process is killed.
/// Returns an error (without having drawn anything) if this compositor
/// doesn't support wlr-layer-shell, so the caller can fall back.
pub fn run(
    shared: Arc<SharedState>,
    config_path: PathBuf,
    mut config: Config,
) -> Result<(), LayerShellError> {
    let connection =
        Connection::connect_to_env().map_err(|e| LayerShellError(format!("no Wayland connection: {e}")))?;

    let (globals, mut event_queue) = registry_queue_init::<AppState>(&connection)
        .map_err(|e| LayerShellError(format!("registry init failed: {e}")))?;
    let qh = event_queue.handle();

    let compositor: WlCompositor = globals
        .bind(&qh, 4..=6, ())
        .map_err(|_| LayerShellError("compositor missing wl_compositor".into()))?;
    let layer_shell: ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).map_err(|_| {
        LayerShellError(
            "compositor does not support wlr-layer-shell (zwlr_layer_shell_v1) \u{2014} \
             this overlay technique needs it; GNOME/Mutter notably doesn't implement it"
                .into(),
        )
    })?;
    let shm: WlShm = globals
        .bind(&qh, 1..=1, ())
        .map_err(|_| LayerShellError("compositor missing wl_shm".into()))?;

    let surface = compositor.create_surface(&qh, ());

    // Fully click-through: the overlay never intercepts mouse input, so it
    // can never block a click in the game underneath it.
    let empty_region = compositor.create_region(&qh, ());
    surface.set_input_region(Some(&empty_region));
    empty_region.destroy();

    let layer_surface = layer_shell.get_layer_surface(
        &surface,
        None, // let the compositor pick the active output
        Layer::Overlay,
        "hpwarn".to_string(),
        &qh,
        (),
    );
    layer_surface.set_anchor(Anchor::Top | Anchor::Left);
    layer_surface.set_margin(config.pos_y as i32, 0, 0, config.pos_x as i32);
    layer_surface.set_exclusive_zone(-1);
    layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer_surface.set_size(WIDTH, HEIGHT);
    surface.commit();

    let mut state = AppState {
        configured: false,
        closed: false,
        width: WIDTH,
        height: HEIGHT,
    };

    while !state.configured && !state.closed {
        event_queue
            .blocking_dispatch(&mut state)
            .map_err(|e| LayerShellError(format!("initial dispatch failed: {e}")))?;
    }
    if state.closed {
        return Err(LayerShellError("layer surface closed before it was ready".into()));
    }

    eprintln!(
        "hpwarn: using native Wayland overlay (layer-shell), {}x{}",
        state.width, state.height
    );

    let mut buf = PixelBuffer::new(state.width, state.height, &shm, &qh);
    let mut font_bytes: Vec<u8> = DEFAULT_FONT_BYTES.to_vec();
    if let Some(path) = &config.font_file {
        if let Ok(bytes) = std::fs::read(path) {
            font_bytes = bytes;
        } else {
            eprintln!("hpwarn: could not read font_file {path}, using built-in font");
        }
    }
    let mut font = FontVec::try_from_vec(font_bytes.clone())
        .map_err(|e| LayerShellError(format!("embedded font failed to parse: {e}")))?;

    let mut last_shown: Option<bool> = None;
    let mut last_pct: Option<u8> = None;
    let mut config_mtime = std::fs::metadata(&config_path).and_then(|m| m.modified()).ok();
    let mut last_config_check = Instant::now();

    loop {
        // Service Wayland events without blocking the HP-poll cadence.
        let _ = connection.flush();
        match event_queue.dispatch_pending(&mut state) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("hpwarn: Wayland dispatch error, exiting: {e}");
                break;
            }
        }
        if state.closed {
            break;
        }

        // Hot-reload config roughly once a second, same as the X11 overlay.
        if last_config_check.elapsed() >= Duration::from_millis(750) {
            last_config_check = Instant::now();
            if let Ok(meta) = std::fs::metadata(&config_path) {
                if let Ok(modified) = meta.modified() {
                    if config_mtime != Some(modified) {
                        config_mtime = Some(modified);
                        let new_config = Config::load(&config_path);
                        if (new_config.pos_x - config.pos_x).abs() > 0.01
                            || (new_config.pos_y - config.pos_y).abs() > 0.01
                        {
                            layer_surface.set_margin(
                                new_config.pos_y as i32,
                                0,
                                0,
                                new_config.pos_x as i32,
                            );
                            surface.commit();
                        }
                        if new_config.font_file != config.font_file {
                            let loaded = new_config
                                .font_file
                                .as_ref()
                                .and_then(|p| std::fs::read(p).ok());
                            font_bytes = loaded.unwrap_or_else(|| DEFAULT_FONT_BYTES.to_vec());
                            match FontVec::try_from_vec(font_bytes.clone()) {
                                Ok(f) => font = f,
                                Err(e) => eprintln!("hpwarn: new font_file invalid ({e}), keeping previous font"),
                            }
                        }
                        config = new_config;
                        shared.set_threshold(config.threshold);
                        shared.set_log_path(config.log_path.clone());
                        // Force a redraw since appearance settings may have changed.
                        last_shown = None;
                    }
                }
            }
        }

        let pct = *shared.pct.lock().unwrap();
        let show = matches!(pct, Some(p) if p <= config.threshold);

        if show != last_shown.unwrap_or(!show) || (show && pct != last_pct) {
            buf.clear();
            if show {
                draw_text(
                    &mut buf.canvas,
                    &font,
                    config.font_size,
                    config.color32(),
                    &format!("HITPOINTS: {}%", pct.unwrap_or(0)),
                );
            }
            buf.commit(&surface);
            let _ = connection.flush();
            last_shown = Some(show);
            last_pct = pct;
        }

        std::thread::sleep(Duration::from_millis(80));
    }

    Ok(())
}

/// A plain CPU RGBA canvas. Kept free of any Wayland types so the drawing
/// logic (and the font rasterization it drives) can be unit-tested without a
/// live compositor connection.
struct Canvas {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        Canvas {
            width,
            height,
            rgba: vec![0u8; (width * height * 4) as usize],
        }
    }

    fn clear(&mut self) {
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

/// Tiny standalone RGB tuple so the drawing code doesn't need to depend on egui.
#[derive(Clone, Copy)]
struct Rgb(pub u8, pub u8, pub u8);

struct PixelBuffer {
    canvas: Canvas,
    mapping: ShmMapping,
    wl_buffer: WlBuffer,
}

impl PixelBuffer {
    fn new(width: u32, height: u32, shm: &WlShm, qh: &QueueHandle<AppState>) -> Self {
        let stride = width * 4;
        let size = (stride * height) as usize;

        let fd = memfd_create(c"hpwarn-overlay", MemfdFlags::CLOEXEC).expect("memfd_create failed");
        rustix::fs::ftruncate(&fd, size as u64).expect("ftruncate failed");
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                size,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                fd.as_fd(),
                0,
            )
            .expect("mmap failed")
        } as *mut u8;

        let pool = shm.create_pool(fd.as_fd(), size as i32, qh, ());
        let wl_buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            Format::Argb8888,
            qh,
            (),
        );
        pool.destroy();

        PixelBuffer {
            canvas: Canvas::new(width, height),
            mapping: ShmMapping { ptr, size },
            wl_buffer,
        }
    }

    fn clear(&mut self) {
        self.canvas.clear();
    }

    fn commit(&mut self, surface: &WlSurface) {
        // wl_shm Argb8888 is little-endian 0xAARRGGBB, i.e. bytes B,G,R,A.
        let shm_slice = unsafe { std::slice::from_raw_parts_mut(self.mapping.ptr, self.mapping.size) };
        for (i, chunk) in self.canvas.rgba.chunks(4).enumerate() {
            let o = i * 4;
            if o + 3 < shm_slice.len() {
                shm_slice[o] = chunk[2];
                shm_slice[o + 1] = chunk[1];
                shm_slice[o + 2] = chunk[0];
                shm_slice[o + 3] = chunk[3];
            }
        }
        surface.attach(Some(&self.wl_buffer), 0, 0);
        surface.damage_buffer(0, 0, self.canvas.width as i32, self.canvas.height as i32);
        surface.commit();
    }
}

impl Drop for PixelBuffer {
    fn drop(&mut self) {
        unsafe {
            let _ = munmap(self.mapping.ptr as *mut _, self.mapping.size);
        }
    }
}

fn draw_text(buf: &mut Canvas, font: &FontVec, size: f32, color: egui::Color32, text: &str) {
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

// Pulled in only for Color32 in draw_text's signature; the rest of this
// module intentionally avoids egui/eframe so it has no GPU/windowing deps.
use eframe::egui;

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders the exact same text-drawing path the live overlay uses, with
    /// the embedded default font, and dumps it to a PNG. There's no Wayland
    /// compositor in CI/sandboxes to verify the real overlay against, so
    /// this is the practical way to confirm the glyph rasterization and
    /// color/alpha blending actually look right, by eye, from the saved file.
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

        let out_path = std::env::temp_dir().join("hpwarn_wayland_overlay_test.png");
        image::save_buffer(&out_path, &rgb, WIDTH, HEIGHT, image::ColorType::Rgb8)
            .expect("failed to save test PNG");
        println!("wrote {}", out_path.display());

        // Sanity check: some pixels should actually have been drawn on
        // (non-zero alpha), i.e. the font rasterized something.
        let any_ink = canvas.rgba.chunks(4).any(|p| p[3] > 0);
        assert!(any_ink, "expected the rendered text to produce visible pixels");
    }
}
