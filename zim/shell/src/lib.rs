//! ZIM desktop shell — application composition (M21).
//!
//! The flow, one thread wide:
//!
//! 1. Parse arguments ([`cli`]) and build the Tauri app with the
//!    bundled pre-engine pages (`ui/boot.html`).
//! 2. In `setup`: attach the tray, then start the attach sequence
//!    ([`engine::attach`]) — discover, probe, spawn-detached.
//! 3. On a healthy engine: stage trust for the engine's persistent
//!    loopback certificate ([`trust`]) — never disabling validation —
//!    then navigate the window to the engine origin via the
//!    `zim://` mapping ([`zim_url`], AD-23).
//! 4. On failure: the recovery page (`ui/down.html`) with the
//!    diagnostic and a Retry that re-runs the sequence.
//!
//! The shell is disposable: quitting it (tray or window) never
//! stops the engine — the engine is a detached, independently
//! supervised process.

pub mod cli;
pub mod commands;
pub mod engine;
pub mod state;
pub mod tray;
pub mod trust;
pub mod zim_url;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};

use crate::state::{EngineStatus, Phase, Shared};

/// The event the bundled pages listen for (`engine-status`).
const STATUS_EVENT: &str = "engine-status";

/// The window label (M21 keeps exactly one).
const MAIN_WINDOW: &str = "main";

/// Entry: parse, build, run.
pub fn run() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let options = cli::parse(&argv).unwrap_or_else(|message| {
        eprintln!("zim_shell: {message}");
        std::process::exit(2);
    });

    tauri::Builder::default()
        .manage(Arc::new(Shared::new(options)))
        .invoke_handler(tauri::generate_handler![
            commands::engine_status,
            commands::engine_retry
        ])
        .setup(|app| {
            tray::init(app.handle())?;
            spawn_attach_thread(
                app.handle().clone(),
                app.state::<Arc<Shared>>().inner().clone(),
            );
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("running the ZIM shell");
}

/// Start the attach sequence on its own thread (one flight at a
/// time; a retry while one runs is a no-op that lets the running
/// attempt's events speak).
pub fn spawn_attach_thread(app: AppHandle, shared: Arc<Shared>) {
    if shared.attaching.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        run_attach(&app, &shared);
        shared.attaching.store(false, Ordering::SeqCst);
    });
}

/// The attach sequence body (also the retry body).
fn run_attach(app: &AppHandle, shared: &Arc<Shared>) {
    let data_dir = shared
        .options
        .data_dir
        .clone()
        .unwrap_or_else(|| default_engine_data_dir(app));
    let engine_path = cli::resolve_engine_path(&shared.options.engine_path);

    emit(shared.set(attaching_status()), app);

    let outcome = engine::attach(&data_dir, &engine_path);
    let mut status = engine::outcome_status(&outcome);

    // Before first navigation: the webview must trust the engine's
    // certificate through the OS trust store (AD-23) — never by
    // disabling validation.
    if status.phase == Phase::Ready {
        match stage_trust(&data_dir, app) {
            Ok(()) => {}
            Err(message) => {
                status = EngineStatus {
                    phase: Phase::Trouble,
                    port: status.port,
                    version: status.version,
                    message: Some(message),
                };
            }
        }
    }

    emit(shared.set(status.clone()), app);

    match status.phase {
        Phase::Ready => {
            let port = status.port.expect("ready status always carries the port");
            let origin = format!("https://127.0.0.1:{port}");
            // M22: the default target is the application chrome
            // (`zim://app`: tab strip + omnibox hosting the tab
            // panes); `--open` overrides it for deep links.
            let target = shared
                .options
                .open
                .clone()
                .unwrap_or_else(|| "zim://app".to_owned());
            match zim_url::resolve(&target, &origin) {
                Ok(url) => {
                    if let Err(e) = navigate_to_engine_url(app, &url) {
                        eprintln!("zim_shell: navigation failed: {e}");
                    } else if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                        let _ = window.set_title(&format!("ZIM — 127.0.0.1:{port}"));
                    }
                    tray::set_engine_tooltip(app, &format!("ZIM — engine 127.0.0.1:{port}"));
                }
                Err(e) => {
                    let status = EngineStatus {
                        phase: Phase::Trouble,
                        port: Some(port),
                        version: status.version,
                        message: Some(format!("cannot open {target:?}: {e}")),
                    };
                    emit(shared.set(status), app);
                    navigate_to_page(app, "down.html");
                }
            }
        }
        Phase::Trouble => {
            navigate_to_page(app, "down.html");
            tray::set_engine_tooltip(app, "ZIM — engine unreachable");
        }
        _ => {}
    }
}

/// The engine data dir default: the shell's per-OS app-data dir +
/// `engine/`. Falls back to the engine's own relative default when
/// the platform path is unavailable (embedded/test contexts).
fn default_engine_data_dir(app: &AppHandle) -> std::path::PathBuf {
    match app.path().app_data_dir() {
        Ok(dir) => dir.join("engine"),
        Err(e) => {
            eprintln!("zim_shell: no app-data dir ({e}); using ./zim_data");
            std::path::PathBuf::from("zim_data")
        }
    }
}

/// Read the engine's persistent certificate and ensure the OS
/// trusts it (`trusted`/`installed` are both success).
fn stage_trust(data_dir: &std::path::Path, app: &AppHandle) -> Result<(), String> {
    let cert_path = data_dir.join("loopback-cert.pem");
    let pem = std::fs::read_to_string(&cert_path).map_err(|e| {
        format!(
            "the engine's certificate is missing ({}): {e}",
            cert_path.display()
        )
    })?;
    let state_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no shell state dir for trust bookkeeping: {e}"))?;
    match trust::ensure_trusted(&pem, &state_dir) {
        trust::TrustState::Trusted | trust::TrustState::InstalledNow => Ok(()),
        trust::TrustState::NeedsManualStep(message) | trust::TrustState::Failed(message) => {
            Err(message)
        }
    }
}

/// Navigate the main window to an engine-origin URL.
pub fn navigate_to_engine_url(app: &AppHandle, url: &str) -> Result<(), String> {
    let window = app
        .get_webview_window(MAIN_WINDOW)
        .ok_or_else(|| "the main window is gone".to_owned())?;
    let parsed = tauri::Url::parse(url).map_err(|e| format!("bad URL {url:?}: {e}"))?;
    window
        .navigate(parsed)
        .map_err(|e| format!("navigate: {e}"))
}

/// Navigate to one of the bundled shell pages (boot/down) by its
/// Tauri asset URL — the scheme differs per platform webview.
fn navigate_to_page(app: &AppHandle, page: &str) {
    let url = if cfg!(windows) {
        format!("http://tauri.localhost/{page}")
    } else {
        format!("tauri://localhost/{page}")
    };
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        match tauri::Url::parse(&url) {
            Ok(parsed) => {
                if let Err(e) = window.navigate(parsed) {
                    eprintln!("zim_shell: cannot navigate to {page}: {e}");
                }
            }
            Err(e) => eprintln!("zim_shell: bad page URL {url:?}: {e}"),
        }
    }
}

/// The tray's "New tab" (M22): open the application chrome with a
/// fresh tab. `zim_url::resolve` deliberately rejects query strings
/// (the identifier grammar, AD-23), so this builds the loadable HTTP
/// form directly — the chrome reads `?newtab=1` on load and restores
/// the session plus one new tab. Reloading the chrome page is the
/// honest cost here: session restore re-opens every tab as a view,
/// and the panes re-fetch their own data (console buffers included);
/// an in-place event channel arrives with the M23 window work.
pub fn open_new_tab(app: &AppHandle) -> Result<(), String> {
    let shared = app.state::<Arc<Shared>>();
    let port = shared
        .get()
        .port
        .ok_or_else(|| "the engine is not attached yet".to_owned())?;
    navigate_to_engine_url(app, &format!("https://127.0.0.1:{port}/zim/app?newtab=1"))
}

/// Emit a status snapshot to the bundled pages.
fn emit(status: EngineStatus, app: &AppHandle) {
    if let Err(e) = app.emit(STATUS_EVENT, status) {
        eprintln!("zim_shell: status emission failed: {e}");
    }
}

/// The transient attaching status.
fn attaching_status() -> EngineStatus {
    EngineStatus {
        phase: Phase::Attaching,
        port: None,
        version: None,
        message: None,
    }
}
