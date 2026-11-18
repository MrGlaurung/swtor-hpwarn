//! Native Windows overlay, using a classic `WS_EX_LAYERED` window updated via
//! `UpdateLayeredWindow` - the same mechanism real Windows HUD/overlay
//! utilities (and plenty of game overlays) use to put per-pixel-transparent
//! text on screen.
//!
//! This exists because eframe/winit's own "transparent window" support
//! turned out not to work here at all, for a fairly deep reason worth
//! writing down (two earlier release rounds tried to fix this a different
//! way before landing on this module):
//!
//! winit's Windows backend makes a window "transparent" by calling
//! `DwmEnableBlurBehindWindow` with an empty blur region - a Windows
//! Vista/7-era DWM trick that only has any visible effect on a window using
//! the *older*, desktop-redirection-bitmap-based presentation path. Both
//! rendering backends eframe offers on Windows present through the *newer*
//! "flip model" path instead - OpenGL ("glow") via a modern driver's WGL
//! implementation, and `wgpu`'s DX12/Vulkan backends by design (flip-model
//! swap chains are mandatory for DX12). Flip-model presentation bypasses the
//! desktop redirection bitmap entirely for performance, which is exactly
//! what `DwmEnableBlurBehindWindow` needs to still be in the loop. So on
//! this system, neither backend's window ever actually became transparent -
//! "glow" showed a solid black box, and switching to "wgpu" only changed
//! *which* opaque fallback color showed (black, then white), never
//! producing real transparency. This is a known, still-open gap in what
//! winit/eframe can do on Windows, not something fixable by picking a
//! different Cargo feature.
//!
//! `UpdateLayeredWindow` sidesteps all of this: it's not a GPU presentation
//! path at all, just "hand the window this premultiplied-alpha bitmap and
//! the compositor will show it with real per-pixel alpha" - supported since
//! Windows 2000, and unaffected by which graphics backend (or none) a
//! process otherwise uses. The small overlay window built here never goes
//! through eframe/egui/winit at all: it's drawn with the same software text
//! rasterizer the Linux layer-shell overlay uses (`text_canvas.rs`), and
//! pushed to the screen with raw Win32 calls.

use crate::config::Config;
use crate::state::SharedState;
use crate::text_canvas::{draw_text, Canvas, DEFAULT_FONT_BYTES, HEIGHT, WIDTH};
use ab_glyph::FontVec;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetWindowLongPtrW,
    GetWindowRect, LoadCursorW, PeekMessageW, PostQuitMessage, RegisterClassW, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, UpdateLayeredWindow, GWLP_USERDATA, HWND_TOPMOST,
    IDC_ARROW, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE,
    SW_SHOWNOACTIVATE, ULW_ALPHA, WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_QUIT,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::Win32::Graphics::Gdi::{BLENDFUNCTION, AC_SRC_ALPHA, AC_SRC_OVER, HGDIOBJ};

#[derive(Debug)]
pub struct WinOverlayError(pub String);

impl fmt::Display for WinOverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for WinOverlayError {}

/// Per-window state, stashed in the window's `GWLP_USERDATA` slot so the
/// (plain C-ABI, environment-less) window procedure can get at it. Only
/// ever touched from this module's own thread - `wndproc` runs synchronously
/// on whichever thread is pumping this window's messages, which is always
/// the thread that calls `run()`.
struct DragState {
    dragging: bool,
    drag_start_cursor: POINT,
    drag_start_window: POINT,
    /// Set by `wndproc` when a drag ends; drained by the poll loop in `run`,
    /// which is what actually persists the new position to disk. Keeps all
    /// file I/O off the window procedure.
    drag_result: Option<(i32, i32)>,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DragState;

    match msg {
        WM_LBUTTONDOWN => {
            if let Some(state) = state_ptr.as_mut() {
                let mut cursor = POINT { x: 0, y: 0 };
                let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                if GetCursorPos(&mut cursor).is_ok() && GetWindowRect(hwnd, &mut rect).is_ok() {
                    state.dragging = true;
                    state.drag_start_cursor = cursor;
                    state.drag_start_window = POINT { x: rect.left, y: rect.top };
                }
            }
            let _ = SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(state) = state_ptr.as_mut() {
                if state.dragging {
                    let mut cursor = POINT { x: 0, y: 0 };
                    if GetCursorPos(&mut cursor).is_ok() {
                        let nx = state.drag_start_window.x + (cursor.x - state.drag_start_cursor.x);
                        let ny = state.drag_start_window.y + (cursor.y - state.drag_start_cursor.y);
                        let _ = SetWindowPos(
                            hwnd,
                            HWND(0),
                            nx,
                            ny,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(state) = state_ptr.as_mut() {
                if state.dragging {
                    state.dragging = false;
                    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                    if GetWindowRect(hwnd, &mut rect).is_ok() {
                        state.drag_result = Some((rect.left, rect.top));
                    }
                }
            }
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// A small, reusable `UpdateLayeredWindow` backing surface: a 32bpp,
/// top-down, premultiplied-alpha DIB section selected into a memory DC, the
/// same fixed `WIDTH` x `HEIGHT` for the lifetime of the overlay.
struct LayeredSurface {
    screen_dc: windows::Win32::Graphics::Gdi::HDC,
    mem_dc: windows::Win32::Graphics::Gdi::HDC,
    bitmap: HBITMAP,
    bits: *mut u8,
}

impl LayeredSurface {
    fn new() -> Result<Self, WinOverlayError> {
        unsafe {
            let screen_dc = GetDC(HWND(0));
            let mem_dc = CreateCompatibleDC(screen_dc);

            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: WIDTH as i32,
                    // Negative height asks for a top-down DIB (row 0 = top),
                    // matching Canvas's own row order - no manual flip needed.
                    biHeight: -(HEIGHT as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [windows::Win32::Graphics::Gdi::RGBQUAD {
                    rgbBlue: 0,
                    rgbGreen: 0,
                    rgbRed: 0,
                    rgbReserved: 0,
                }; 1],
            };

            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(
                screen_dc,
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                windows::Win32::Foundation::HANDLE(0),
                0,
            )
            .map_err(|e| WinOverlayError(format!("CreateDIBSection failed: {e}")))?;

            if bits.is_null() {
                return Err(WinOverlayError("CreateDIBSection returned no backing buffer".into()));
            }

            let previous: HGDIOBJ = SelectObject(mem_dc, bitmap);
            if !previous.is_invalid() {
                let _ = DeleteObject(previous);
            }

            Ok(LayeredSurface {
                screen_dc,
                mem_dc,
                bitmap,
                bits: bits as *mut u8,
            })
        }
    }

    /// Writes a premultiplied-alpha RGBA canvas into the DIB's backing
    /// buffer, converting to the BGRA byte order GDI bitmaps use.
    fn write(&mut self, canvas: &Canvas) {
        let len = (WIDTH * HEIGHT * 4) as usize;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, len) };
        for (i, chunk) in canvas.rgba.chunks(4).enumerate() {
            let o = i * 4;
            if o + 3 < dst.len() {
                dst[o] = chunk[2]; // B
                dst[o + 1] = chunk[1]; // G
                dst[o + 2] = chunk[0]; // R
                dst[o + 3] = chunk[3]; // A
            }
        }
    }

    /// Pushes the current buffer contents to the window at `(x, y)`.
    fn present(&self, hwnd: HWND, x: i32, y: i32) {
        let dst = POINT { x, y };
        let size = windows::Win32::Foundation::SIZE { cx: WIDTH as i32, cy: HEIGHT as i32 };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            let _ = UpdateLayeredWindow(
                hwnd,
                self.screen_dc,
                Some(&dst as *const POINT),
                Some(&size as *const windows::Win32::Foundation::SIZE),
                self.mem_dc,
                Some(&src as *const POINT),
                windows::Win32::Foundation::COLORREF(0),
                Some(&blend as *const BLENDFUNCTION),
                ULW_ALPHA,
            );
        }
    }
}

impl Drop for LayeredSurface {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.mem_dc);
            ReleaseDC(HWND(0), self.screen_dc);
        }
    }
}

/// Runs the native Windows overlay. Blocks until the window is destroyed
/// (which, in practice, is only on process exit - nothing in this app ever
/// asks it to close early).
pub fn run(shared: Arc<SharedState>, config_path: PathBuf, mut config: Config) -> Result<(), WinOverlayError> {
    unsafe {
        let instance: windows::Win32::Foundation::HINSTANCE =
            windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::PCWSTR::null())
                .map_err(|e| WinOverlayError(format!("GetModuleHandleW failed: {e}")))?
                .into();

        let class_name = w!("HpwarnOverlayWindow");
        let cursor = LoadCursorW(windows::Win32::Foundation::HINSTANCE(0), IDC_ARROW)
            .map_err(|e| WinOverlayError(format!("LoadCursorW failed: {e}")))?;

        let wc = WNDCLASSW {
            style: Default::default(),
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: Default::default(),
            hCursor: cursor,
            hbrBackground: Default::default(),
            lpszMenuName: windows::core::PCWSTR::null(),
            lpszClassName: class_name,
        };
        if RegisterClassW(&wc) == 0 {
            return Err(WinOverlayError("RegisterClassW failed".into()));
        }

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class_name,
            w!(""),
            WS_POPUP,
            config.pos_x as i32,
            config.pos_y as i32,
            WIDTH as i32,
            HEIGHT as i32,
            HWND(0),
            windows::Win32::UI::WindowsAndMessaging::HMENU(0),
            instance,
            None,
        );
        if hwnd.0 == 0 {
            return Err(WinOverlayError("CreateWindowExW failed".into()));
        }

        let drag_state = Box::into_raw(Box::new(DragState {
            dragging: false,
            drag_start_cursor: POINT { x: 0, y: 0 },
            drag_start_window: POINT { x: 0, y: 0 },
            drag_result: None,
        }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, drag_state as isize);

        let mut surface = LayeredSurface::new()?;
        let mut canvas = Canvas::new(WIDTH, HEIGHT);

        let mut font_bytes: Vec<u8> = DEFAULT_FONT_BYTES.to_vec();
        if let Some(path) = &config.font_file {
            if let Ok(bytes) = std::fs::read(path) {
                font_bytes = bytes;
            } else {
                eprintln!("hpwarn: could not read font_file {path}, using built-in font");
            }
        }
        let mut font = FontVec::try_from_vec(font_bytes.clone())
            .map_err(|e| WinOverlayError(format!("embedded font failed to parse: {e}")))?;

        eprintln!("hpwarn: using native Windows layered-window overlay, {WIDTH}x{HEIGHT}");

        let mut last_shown: Option<bool> = None;
        let mut last_pct: Option<u8> = None;
        let mut pos_x = config.pos_x;
        let mut pos_y = config.pos_y;
        let mut config_mtime = std::fs::metadata(&config_path).and_then(|m| m.modified()).ok();
        let mut last_config_check = Instant::now();
        let mut last_topmost_assert = Instant::now() - Duration::from_secs(10);

        loop {
            // Drain pending window messages without blocking the HP-poll cadence.
            let mut msg = MSG {
                hwnd: HWND(0),
                message: 0,
                wParam: WPARAM(0),
                lParam: LPARAM(0),
                time: 0,
                pt: POINT { x: 0, y: 0 },
            };
            while PeekMessageW(&mut msg, HWND(0), 0, 0, PM_REMOVE).into() {
                if msg.message == WM_QUIT {
                    let _ = Box::from_raw(drag_state);
                    return Ok(());
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            // Pick up a just-finished drag (see wndproc's WM_LBUTTONUP) and
            // persist it, same as the egui-based overlay does on release.
            if let Some((x, y)) = (*drag_state).drag_result.take() {
                pos_x = x as f32;
                pos_y = y as f32;
                config.pos_x = pos_x;
                config.pos_y = pos_y;
                let _ = config.save(&config_path);
                if let Ok(meta) = std::fs::metadata(&config_path) {
                    config_mtime = meta.modified().ok();
                }
            }

            // Hot-reload config roughly once a second, same as the other
            // overlay backends.
            if last_config_check.elapsed() >= Duration::from_millis(750) {
                last_config_check = Instant::now();
                if let Ok(meta) = std::fs::metadata(&config_path) {
                    if let Ok(modified) = meta.modified() {
                        if config_mtime != Some(modified) {
                            config_mtime = Some(modified);
                            let new_config = Config::load(&config_path);
                            if (new_config.pos_x - pos_x).abs() > 0.01 || (new_config.pos_y - pos_y).abs() > 0.01 {
                                pos_x = new_config.pos_x;
                                pos_y = new_config.pos_y;
                                let _ = SetWindowPos(
                                    hwnd,
                                    HWND(0),
                                    pos_x as i32,
                                    pos_y as i32,
                                    0,
                                    0,
                                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                                );
                            }
                            if new_config.font_file != config.font_file {
                                let loaded = new_config.font_file.as_ref().and_then(|p| std::fs::read(p).ok());
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

            // Some window managers / focus changes can drop "always on top";
            // re-assert it periodically, same reasoning as the egui overlay.
            if last_topmost_assert.elapsed() >= Duration::from_millis(1500) {
                last_topmost_assert = Instant::now();
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }

            let pct = *shared.pct.lock().unwrap();
            let show = shared.test_overlay_active() || matches!(pct, Some(p) if p <= config.threshold);

            if show != last_shown.unwrap_or(!show) {
                if show {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                } else {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }

            if show && (show != last_shown.unwrap_or(!show) || pct != last_pct) {
                canvas.clear();
                draw_text(
                    &mut canvas,
                    &font,
                    config.font_size,
                    config.color32(),
                    &format!("HITPOINTS: {}%", pct.unwrap_or(0)),
                );
                surface.write(&canvas);
                surface.present(hwnd, pos_x as i32, pos_y as i32);
            }

            last_shown = Some(show);
            last_pct = pct;

            std::thread::sleep(Duration::from_millis(80));
        }
    }
}
