//! What helps find out why something goes wrong on one computer and not
//! another: a log file (each start keeps the last run's beside it), the
//! graphics in use, graphics errors written down instead of ending the app,
//! and the Diagnostics window that shows it all and copies it.
//!
//! The log has no passwords: nothing PrzmTerm logs carries one.

use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui;
use eframe::egui_wgpu;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, Registry, fmt, reload};

/// The log's level, for PrzmTerm's crates and the rest.
const NORMAL: &str = "warn,przmterm=info,przmterm_app=info,przmterm_session=info,\
                      przmterm_connect=info,przmterm_transport=info,przmterm_term=info,\
                      przmterm_script=info";
/// With *Detailed log* on: PrzmTerm's own steps, and the graphics library's.
const DETAILED: &str = "warn,przmterm=debug,przmterm_app=debug,przmterm_session=debug,\
                        przmterm_connect=debug,przmterm_transport=debug,przmterm_term=debug,\
                        przmterm_script=debug,russh=info,wgpu_core=info,wgpu_hal=info";

const LOG_NAME: &str = "przmterm.log";
const PREVIOUS_NAME: &str = "przmterm.previous.log";

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static FILTER: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();
static GRAPHICS: OnceLock<Graphics> = OnceLock::new();
/// Graphics errors seen; only the first ones are written down.
static GPU_ERRORS: AtomicUsize = AtomicUsize::new(0);
const GPU_ERRORS_LOGGED: usize = 20;

/// Where the logs go: not the settings folder, which may be synced.
pub fn log_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return dirs::home_dir().map(|h| h.join("Library/Logs/PrzmTerm"));
    }
    if cfg!(windows) {
        return dirs::data_local_dir().map(|d| d.join("PrzmTerm").join("logs"));
    }
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|d| d.join("przmterm").join("logs"))
}

/// The log file of this run, once open.
pub fn log_path() -> Option<&'static PathBuf> {
    LOG_PATH.get()
}

/// Starts logging to stderr and to the log file (the last run's file is
/// kept as `przmterm.previous.log`). `RUST_LOG` overrides the level.
pub fn init(detailed: bool) {
    let filter = std::env::var("RUST_LOG")
        .ok()
        .and_then(|f| EnvFilter::try_new(f).ok())
        .unwrap_or_else(|| level(detailed));
    let (filter, handle) = reload::Layer::new(filter);
    let file = open_log().map(|f| fmt::layer().with_ansi(false).with_writer(Mutex::new(f)));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(file)
        .init();
    let _ = FILTER.set(handle);

    // A crash leaves its reason in the log, not only on a console nobody
    // sees (Windows has none).
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("crash: {info}");
        default(info);
    }));

    tracing::info!(
        "PrzmTerm {} on {} {}{}",
        przmterm_core::VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
        if detailed { " (detailed log)" } else { "" }
    );
}

/// The detailed log on or off, from now on.
pub fn set_detailed(on: bool) {
    if std::env::var_os("RUST_LOG").is_some() {
        return;
    }
    if let Some(handle) = FILTER.get() {
        let _ = handle.modify(|f| *f = level(on));
    }
    tracing::info!("detailed log {}", if on { "on" } else { "off" });
}

fn level(detailed: bool) -> EnvFilter {
    EnvFilter::new(if detailed { DETAILED } else { NORMAL })
}

fn open_log() -> Option<File> {
    let dir = log_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(LOG_NAME);
    if path.exists() {
        let _ = std::fs::rename(&path, dir.join(PREVIOUS_NAME));
    }
    let file = File::create(&path).ok()?;
    let _ = LOG_PATH.set(path);
    Some(file)
}

/// The graphics adapter and how it draws.
#[derive(Debug, Clone)]
pub struct Graphics {
    pub adapter: String,
    pub backend: String,
    pub kind: String,
    pub driver: String,
    pub ids: String,
    pub format: String,
    /// Drawn without a graphics card (Windows' WARP, llvmpipe…).
    pub software: bool,
}

/// Notes the graphics eframe chose, and writes graphics errors to the log
/// instead of ending the app (wgpu's default).
pub fn graphics_found(state: &egui_wgpu::RenderState) {
    let info = state.adapter.get_info();
    let software = info.device_type == egui_wgpu::wgpu::DeviceType::Cpu;
    let graphics = Graphics {
        adapter: info.name.clone(),
        backend: format!("{:?}", info.backend),
        kind: format!("{:?}", info.device_type),
        driver: [info.driver.as_str(), info.driver_info.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" "),
        ids: format!("vendor {:04x}, device {:04x}", info.vendor, info.device),
        format: format!("{:?}", state.target_format),
        software,
    };
    tracing::info!(
        "graphics: {} ({}, {}), driver {}, {}, target {}",
        graphics.adapter,
        graphics.backend,
        graphics.kind,
        if graphics.driver.is_empty() {
            "?"
        } else {
            &graphics.driver
        },
        graphics.ids,
        graphics.format
    );
    if software {
        tracing::warn!("graphics drawn in software: no graphics card driver in use");
    }
    for other in &state.available_adapters {
        let o = other.get_info();
        tracing::debug!(
            "adapter available: {} ({:?}, {:?})",
            o.name,
            o.backend,
            o.device_type
        );
    }
    let _ = GRAPHICS.set(graphics);
    state.device.on_uncaptured_error(Arc::new(|error| {
        let n = GPU_ERRORS.fetch_add(1, Ordering::Relaxed);
        if n < GPU_ERRORS_LOGGED {
            tracing::error!("graphics error: {error}");
        } else if n == GPU_ERRORS_LOGGED {
            tracing::error!("more graphics errors; the rest are not written down");
        }
    }));
}

/// What the Diagnostics window shows, and copies.
pub fn report(ctx: &egui::Context, config_dir: Option<&std::path::Path>, detailed: bool) -> String {
    let mut lines = vec![
        format!("PrzmTerm {}", przmterm_core::VERSION),
        format!(
            "System: {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    ];
    match GRAPHICS.get() {
        Some(g) => {
            lines.push(format!("Graphics: {}", g.adapter));
            lines.push(format!("  {} · {} · {}", g.backend, g.kind, g.ids));
            if !g.driver.is_empty() {
                lines.push(format!("  Driver: {}", g.driver));
            }
            lines.push(format!("  Target: {}", g.format));
            if g.software {
                lines.push("  Drawn in software (no graphics card driver in use)".into());
            }
        }
        None => lines.push("Graphics: not wgpu".into()),
    }
    let grid = if crate::grid::enabled() {
        "graphics card (wgpu)"
    } else if std::env::var("PRZMTERM_RENDERER").is_ok_and(|v| v.eq_ignore_ascii_case("egui")) {
        "egui (PRZMTERM_RENDERER=egui)"
    } else {
        "egui"
    };
    lines.push(format!("Terminal drawn by: {grid}"));
    lines.push(format!(
        "Graphics errors: {}",
        GPU_ERRORS.load(Ordering::Relaxed)
    ));
    lines.push(format!("Scale: {:.2}", ctx.pixels_per_point()));
    for var in [
        "PRZMTERM_RENDERER",
        "WGPU_BACKEND",
        "WGPU_POWER_PREF",
        "RUST_LOG",
    ] {
        if let Ok(v) = std::env::var(var) {
            lines.push(format!("{var}={v}"));
        }
    }
    if let Some(dir) = config_dir {
        lines.push(format!("Settings folder: {}", dir.display()));
    }
    lines.push(format!(
        "Log: {}{}",
        log_path().map_or("none".into(), |p| p.display().to_string()),
        if detailed { " (detailed)" } else { "" }
    ));
    lines.join("\n")
}

/// The previous run's log, beside this one's.
pub fn previous_log() -> Option<PathBuf> {
    let path = log_path()?.with_file_name(PREVIOUS_NAME);
    path.exists().then_some(path)
}
