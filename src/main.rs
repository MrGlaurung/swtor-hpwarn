mod audio;
mod config;
mod configure;
mod dashboard;
mod overlay;
mod parser;
mod state;
mod watcher;
mod wayland_overlay;

use clap::Parser;
use config::Config;
use eframe::egui;
use state::SharedState;
use std::path::PathBuf;
use std::sync::Arc;

/// hpwarn: shows your SWTOR HP% when it drops at/below a threshold, beeps
/// once each time you cross from above it to at-or-below it, and keeps a
/// dashboard with your HP history and curve.
#[derive(Parser, Debug)]
#[command(name = "hpwarn")]
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
    {
        let shared = shared.clone();
        std::thread::spawn(move || watcher::run(shared));
    }

    // The overlay runs on its own background thread so the dashboard (if
    // shown) can own the main thread. On Wayland this is a plain Wayland
    // client with no GUI toolkit event loop of its own, so it's always
    // thread-safe to run this way. On X11 it's a second eframe/winit
    // window; winit's "must run on the main thread" restriction is a macOS
    // requirement, not an X11 one, so a background thread works fine here.
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
                        // Fall through to the eframe path below, reusing the
                        // same watcher thread / shared state rather than
                        // starting a second one.
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
    Ok(())
}

fn run_configure(config_path: PathBuf) -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 480.0])
            .with_title("hpwarn settings"),
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
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0])
            .with_title("hpwarn"),
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
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 90.0])
            .with_position([config.pos_x, config.pos_y])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(false)
            .with_title("hpwarn")
            .with_app_id("hpwarn")
            .with_taskbar(false),
        ..Default::default()
    };

    eframe::run_native(
        "hpwarn",
        native_options,
        Box::new(move |_cc| Ok(Box::new(overlay::OverlayApp::new(shared, config_path, config)))),
    )
}
