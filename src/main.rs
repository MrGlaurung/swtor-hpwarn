mod audio;
mod config;
mod configure;
mod dashboard;
mod overlay;
mod parser;
mod state;
mod watcher;
// The native Wayland layer-shell overlay only exists to route around a
// Linux-compositor-specific problem (see overlay.rs/README): compositors
// routinely stop compositing above any window that *looks* fullscreen, so a
// normal "always on top" window can vanish behind the game. Windows (and
// macOS) don't have that problem - a plain always-on-top window just stays
// on top - so this module, and the Wayland-protocol crates it needs, only
// need to exist (and only need to compile) on Linux.
#[cfg(target_os = "linux")]
mod wayland_overlay;

use clap::Parser;
use config::Config;
use eframe::egui;
use state::SharedState;
use std::path::PathBuf;
use std::sync::Arc;

/// The app icon (a sci-fi HUD badge), embedded so every window shows it
/// without relying on an external file being present at runtime. This sets
/// the window icon directly (honoured by X11/most window managers); on
/// Wayland, the taskbar icon instead comes from a matching .desktop file's
/// `Icon=` entry (see `assets/hpwarn.desktop` and `install-icon.sh`), since
/// Wayland has no protocol for a window to hand the compositor pixel data
/// for its icon.
static ICON_PNG: &[u8] = include_bytes!("../assets/icons/hpwarn-256.png");

fn app_icon() -> Option<Arc<egui::IconData>> {
    match eframe::icon_data::from_png_bytes(ICON_PNG) {
        Ok(icon) => Some(Arc::new(icon)),
        Err(e) => {
            eprintln!("hpwarn: failed to load embedded icon: {e}");
            None
        }
    }
}

/// hpwarn's own version, read from Cargo.toml at build time. Shown in the
/// dashboard/settings window titles and bodies, and via `hpwarn --version`,
/// so it's easy to tell at a glance whether you're running the build you
/// think you are.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// hpwarn: shows your SWTOR HP% when it drops at/below a threshold, beeps
/// once each time you cross from above it to at-or-below it, and keeps a
/// dashboard with your HP history and curve.
#[derive(Parser, Debug)]
#[command(name = "hpwarn", version)]
struct Args {
    /// Open only the lightweight settings window (no overlay, no history
    /// dashboard, no log watcher running). Mainly for editing the config
    /// without the game running.
    #[arg(long)]
    configure: bool,

    /// Use a config file at this path instead of the default
    /// (~/.config/hpwarn/config.toml).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Force the plain windowed overlay even on Wayland (skips the
    /// layer-shell overlay). Mainly useful for debugging.
    #[arg(long)]
    no_layer_shell: bool,

    /// Don't open the dashboard window; just run the HP overlay (the old
    /// default behaviour).
    #[arg(long)]
    no_dashboard: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config_path = args.config.unwrap_or_else(config::default_config_path);

    if !config_path.exists() {
        let _ = Config::default().save(&config_path);
    }

    if args.configure {
        run_configure(config_path).map_err(|e| anyhow::anyhow!("{e}"))?;
        return Ok(());
    }

    let config = Config::load(&config_path);
    let shared = Arc::new(SharedState::new(config.threshold, config.log_path.clone()));
    // SharedState::new only seeds threshold/log_path; the watcher thread
    // reads every other live-tunable knob straight from `shared`, so the
    // rest of a freshly-loaded config (in particular the second beep's
    // enabled flag/threshold) must be pushed in here too. Without this, a
    // saved "second beep: on" from a previous run would show correctly in
    // the dashboard's checkbox (it reads `config` directly) but the watcher
    // would silently keep treating it as off until the dashboard's own UI
    // happened to re-save it - i.e. the second beep just wouldn't fire,
    // with no visible sign anything was wrong.
    shared.set_second_beep_enabled(config.second_beep_enabled);
    shared.set_second_threshold(config.second_threshold);
    {
        let shared = shared.clone();
        std::thread::spawn(move || watcher::run(shared));
    }

    // Linux: the overlay runs on its own background thread so the dashboard
    // (if shown) can own the main thread. On Wayland this is a plain
    // Wayland client with no GUI toolkit event loop of its own, so it's
    // always thread-safe to run this way. On the X11 fallback it's a second
    // eframe/winit window on a second thread - this works on Linux's winit
    // backend (unlike Windows, see run_combined below).
    #[cfg(target_os = "linux")]
    {
        {
            let shared = shared.clone();
            let config_path = config_path.clone();
            let config = config.clone();
            let no_layer_shell = args.no_layer_shell;
            std::thread::spawn(move || {
                let is_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
                if is_wayland && !no_layer_shell {
                    match wayland_overlay::run(shared.clone(), config_path.clone(), config.clone()) {
                        Ok(()) => return,
                        Err(e) => {
                            eprintln!(
                                "hpwarn: native Wayland overlay unavailable ({e}); \
                                 falling back to a plain window (it may not stay above a fullscreen game)."
                            );
                            // Fall through to the eframe path below, reusing
                            // the same watcher thread / shared state rather
                            // than starting a second one.
                        }
                    }
                }
                if let Err(e) = run_overlay(shared, config_path, config) {
                    eprintln!("hpwarn: overlay window failed: {e}");
                }
            });
        }

        if args.no_dashboard {
            // Nothing left to do on the main thread; park it so the process
            // doesn't exit while the background threads are still running.
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }

        run_dashboard(shared, config_path, config).map_err(|e| anyhow::anyhow!("{e}"))?;
        return Ok(());
    }

    // Windows (and macOS): winit only ever allows ONE event loop for the
    // whole process - not one-at-a-time, but ever, even sequentially across
    // threads - so the Linux approach above (two separate `eframe::run_native`
    // calls, each creating its own event loop) fails there with exactly the
    // error this was built to fix: "EventLoop can't be recreated". Instead,
    // run a single eframe app on the main thread that hosts the dashboard as
    // its root window and the overlay as a second, immediate-mode native
    // viewport alongside it - one process-wide event loop, still two real,
    // independently positioned OS windows.
    #[cfg(not(target_os = "linux"))]
    {
        if args.no_dashboard {
            return run_overlay(shared, config_path, config).map_err(|e| anyhow::anyhow!("{e}"));
        }
        return run_combined(shared, config_path, config).map_err(|e| anyhow::anyhow!("{e}"));
    }
}

fn run_configure(config_path: PathBuf) -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([520.0, 480.0])
        .with_title(format!("hpwarn settings - v{VERSION}"))
        // Its own app_id, distinct from "hpwarn" (the overlay) and
        // "hpwarn-dashboard" (see run_dashboard for why this matters).
        .with_app_id("hpwarn-settings")
        .with_window_level(egui::WindowLevel::Normal);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "hpwarn-settings",
        native_options,
        Box::new(|_cc| Ok(Box::new(configure::ConfigureApp::new(config_path)))),
    )
}

/// The always-on dashboard: configuration on the left, HP history + curve
/// on the right. This is what `hpwarn` shows by default.
fn run_dashboard(shared: Arc<SharedState>, config_path: PathBuf, config: Config) -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([900.0, 600.0])
        .with_title(format!("hpwarn dashboard - v{VERSION}"))
        // Give this window its own app_id (Wayland) / window class,
        // distinct from the overlay's "hpwarn". The overlay used to be
        // a plain window kept on top via a desktop-environment window
        // rule (e.g. KDE's "Keep above" matched to app_id "hpwarn")
        // before it became a native Wayland layer-shell surface; if
        // this window reused that same id/class, a rule like that
        // would also catch the dashboard and keep *it* on top too,
        // which isn't wanted - only the HP warning itself should float
        // above everything.
        .with_app_id("hpwarn-dashboard")
        // Explicit, in case anything upstream of this (session/compositor
        // defaults, a leftover window rule matched some other way) would
        // otherwise make it come up "always on top".
        .with_window_level(egui::WindowLevel::Normal);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "hpwarn-dashboard",
        native_options,
        Box::new(move |_cc| Ok(Box::new(dashboard::DashboardApp::new(config_path, config, shared)))),
    )
}

/// The plain eframe/winit overlay: used on X11, and as a fallback on
/// Wayland compositors that don't support wlr-layer-shell.
fn run_overlay(shared: Arc<SharedState>, config_path: PathBuf, config: Config) -> eframe::Result<()> {
    let mut viewport = overlay_viewport_builder(&config);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "hpwarn",
        native_options,
        Box::new(move |_cc| Ok(Box::new(overlay::OverlayApp::new(shared, config_path, config)))),
    )
}

/// The overlay window's properties - shared between `run_overlay` (its own
/// standalone `eframe::run_native` call, used on Linux) and `run_combined`'s
/// second, immediate-mode viewport (used on Windows/macOS).
fn overlay_viewport_builder(config: &Config) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_inner_size([420.0, 90.0])
        .with_position([config.pos_x, config.pos_y])
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_resizable(false)
        .with_title("hpwarn")
        .with_app_id("hpwarn")
        .with_taskbar(false)
}

/// Windows/macOS only: a single `eframe::run_native` call hosting both the
/// dashboard (root viewport) and the overlay (a second, immediate-mode
/// viewport) in one process-wide winit event loop. See the comment at the
/// call site in `main` for why this exists - Linux keeps the simpler
/// separate-window-per-thread approach (`run_dashboard` + `run_overlay`),
/// which doesn't work on Windows.
#[cfg(not(target_os = "linux"))]
fn run_combined(shared: Arc<SharedState>, config_path: PathBuf, config: Config) -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([900.0, 600.0])
        .with_title(format!("hpwarn dashboard - v{VERSION}"))
        .with_app_id("hpwarn-dashboard")
        .with_window_level(egui::WindowLevel::Normal)
        // On Windows/macOS, all of this run's windows share ONE GL context,
        // and that context's pixel format is chosen once, up front, from
        // this (the root/dashboard) viewport's own `transparent` flag - see
        // eframe's `GlutinWindowContext::new`, which builds the shared
        // `ConfigTemplateBuilder` with
        // `.with_transparency(native_options.viewport.transparent...)`.
        // Every other viewport (including the overlay, below) is then a
        // window created against that same shared, already-fixed pixel
        // format, so if THIS viewport doesn't ask for transparency, no
        // viewport in the whole process can ever get it, no matter what it
        // asks for itself - which is exactly why the overlay rendered as an
        // opaque black box on Windows before this flag was added here. The
        // dashboard window still looks fully solid despite this: a pixel
        // format merely being alpha-*capable* doesn't make anything see-through
        // on its own, only actually painting with alpha < 1.0 does (see the
        // `clear_color` override below, which keeps the dashboard's own
        // alpha pinned to fully opaque).
        .with_transparent(true);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let mut overlay_viewport = overlay_viewport_builder(&config);
    if let Some(icon) = app_icon() {
        overlay_viewport = overlay_viewport.with_icon(icon);
    }

    eframe::run_native(
        "hpwarn-dashboard",
        native_options,
        Box::new(move |_cc| {
            let dashboard = dashboard::DashboardApp::new(config_path.clone(), config.clone(), shared.clone());
            let overlay = overlay::OverlayApp::new(shared, config_path, config);
            Ok(Box::new(CombinedApp {
                dashboard,
                overlay,
                overlay_viewport_id: egui::ViewportId::from_hash_of("hpwarn-overlay"),
                overlay_viewport,
                painting_overlay: std::cell::Cell::new(false),
            }))
        }),
    )
}

/// Hosts the dashboard (root viewport) and the overlay (a second,
/// immediate-mode viewport drawn from inside the root's own `update`) as one
/// combined `eframe::App`. Only used on Windows/macOS - see `run_combined`.
#[cfg(not(target_os = "linux"))]
struct CombinedApp {
    dashboard: dashboard::DashboardApp,
    overlay: overlay::OverlayApp,
    overlay_viewport_id: egui::ViewportId,
    overlay_viewport: egui::ViewportBuilder,
    /// `eframe::App::clear_color` has no way to know which viewport is
    /// currently being painted (the dashboard needs its normal background,
    /// the overlay needs full transparency), so this is toggled around the
    /// `show_viewport_immediate` call below: true while the overlay's own
    /// paint pass is in progress (which queries `clear_color` before
    /// `overlay.render` runs), false the rest of the time (covering the
    /// dashboard/root's own paint pass, before and after).
    painting_overlay: std::cell::Cell<bool>,
}

#[cfg(not(target_os = "linux"))]
impl eframe::App for CombinedApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.painting_overlay.get() {
            [0.0, 0.0, 0.0, 0.0] // fully transparent, for the overlay
        } else {
            // eframe's own default clear colour (see eframe::App::clear_color)
            // is actually only ~70% opaque (alpha 180/255) - normally
            // invisible, because a normal eframe window's pixel format has
            // no alpha channel at all, so the alpha is simply discarded and
            // the window looks fully solid no matter what. Here, though,
            // the dashboard's own viewport had to be marked `transparent`
            // (see `run_combined`) purely so the *overlay* viewport - which
            // shares the same, process-wide GL pixel format - can actually
            // get one. That makes the dashboard's own alpha channel real,
            // so using the normal ~70%-opacity default here would make the
            // dashboard genuinely see-through on Windows/macOS. Pin it to
            // fully opaque instead, so it keeps looking exactly like it
            // does on Linux (where the dashboard's own window never asks
            // for transparency, and this override doesn't apply at all).
            egui::Color32::from_rgba_unmultiplied(12, 12, 12, 255).to_normalized_gamma_f32()
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.dashboard.render(ctx);

        // Immediate viewports only repaint when the root/parent viewport
        // also repaints, so keep the root's own cadence at least as tight
        // as the overlay wants (80ms) rather than the dashboard's more
        // relaxed 200ms, to keep the overlay responsive during a fight.
        ctx.request_repaint_after(std::time::Duration::from_millis(80));

        self.painting_overlay.set(true);
        ctx.show_viewport_immediate(self.overlay_viewport_id, self.overlay_viewport.clone(), |overlay_ctx, _class| {
            self.overlay.render(overlay_ctx);
        });
        self.painting_overlay.set(false);
    }
}
