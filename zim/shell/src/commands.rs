//! The bundled UI pages' command surface (the pages are shell-owned
//! — they talk to the shell, never directly to the engine).
//!
//! The pre-engine pages need the attach status (`engine_status`)
//! and a retry (`engine_retry`). The engine-origin chrome page (M23)
//! adds the window commands: tear-off (`zim_tear_off`), merge-back
//! (`zim_merge_to_main`), and the window label probe
//! (`zim_window_label`). The chrome is engine-origin content, so
//! these commands are how *it* reaches the shell — the same
//! direction of trust as the bundled pages, reached over the
//! `withGlobalTauri` bridge.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::Url;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow};

use crate::state::{EngineStatus, Shared};
use crate::windows::next_label;

/// The current attach status (the `engine-status` event payload).
#[tauri::command]
pub fn engine_status(shared: State<'_, Arc<Shared>>) -> EngineStatus {
    shared.get()
}

/// Re-run the attach sequence (recovery page's Retry button).
///
/// One flight at a time: while an attempt is running this returns
/// immediately and the in-flight attempt's events are the truth.
#[tauri::command]
pub fn engine_retry(app: AppHandle, shared: State<'_, Arc<Shared>>) {
    crate::spawn_attach_thread(app, shared.inner().clone());
}

/// The calling window's label (the chrome normally knows it from
/// its `?window=` parameter; this is the authoritative answer).
#[tauri::command]
pub fn zim_window_label(window: WebviewWindow) -> String {
    window.label().to_owned()
}

/// The shell's own build version (M27's About surface: the app
/// version is the bundle's — the shell and engine bytes ship
/// together).
#[tauri::command]
pub fn zim_shell_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// The installed engine's version — the sidecar binary's own
/// `--version` answer (M27's About surface: the bytes that run on
/// the next engine restart; can lag the RUNNING engine right after
/// an update, per UPDATES.md §3 — the About page shows both).
#[tauri::command]
pub fn zim_installed_engine_version(shared: State<'_, Arc<Shared>>) -> Option<String> {
    let engine_path = crate::cli::resolve_engine_path(&shared.options.engine_path);
    crate::engine::installed_engine_version(&engine_path)
}

/// The running engine's version, straight from the health surface
/// (M27's About surface: the process actually supervising servers
/// right now).
#[tauri::command]
pub fn zim_running_engine_version(shared: State<'_, Arc<Shared>>) -> Option<String> {
    let port = shared.get().port?;
    crate::engine::running_engine_version(port).ok()
}

/// The update channel's check, invoked from the About surface (the
/// same consent-shaped cycle the tray item runs — the tray stays
/// the flash surface for label-state).
#[tauri::command]
pub fn zim_check_updates(app: AppHandle) {
    crate::updater::check_and_apply(app);
}

/// Tear-off (M23, AD-26): open the dragged-out tab in its own
/// chrome window. `paths` carries the tab's `zim://` URLs — the
/// primary pane first, the split's secondary pane second when the
/// tab was split — plus the split ratio. The new window loads
/// `/zim/app?tab=…&split=…&ratio=…&window={label}` and restores
/// exactly that one tab (Chrome's "opened with a URL" semantics);
/// its further tab set is its own, persisted per-window.
#[tauri::command]
pub fn zim_tear_off(
    app: AppHandle,
    shared: State<'_, Arc<Shared>>,
    paths: Vec<String>,
    ratio: Option<f64>,
) -> Result<String, String> {
    if paths.is_empty() || paths.len() > 2 {
        return Err("tear-off carries one tab (and its optional split pane)".to_owned());
    }
    let port = shared
        .get()
        .port
        .ok_or_else(|| "the engine is not attached yet".to_owned())?;
    let origin = format!("https://127.0.0.1:{port}");

    // Every path is resolved through the zim:// grammar — the
    // identifier rules (AD-23) are enforced here too, so a hostile
    // page cannot aim a window at an arbitrary URL.
    let mut url = String::new();
    for (index, path) in paths.iter().enumerate() {
        let resolved = crate::zim_url::resolve(path, &origin)
            .map_err(|e| format!("cannot open {path:?}: {e}"))?;
        let separator = if index == 0 { '?' } else { '&' };
        let key = if index == 0 { "tab" } else { "split" };
        url.push(separator);
        url.push_str(key);
        url.push('=');
        url.push_str(&resolved);
    }
    let label = next_label(shared.window_counter.fetch_add(1, Ordering::SeqCst));
    url.push_str(&format!("&window={label}"));
    if paths.len() > 1 {
        let ratio = ratio.unwrap_or(0.5).clamp(0.2, 0.8);
        url.push_str(&format!("&ratio={ratio}"));
    }

    create_chrome_window(&app, &label, &url)?;
    shared.windows.add(&label);
    // The caller (the chrome page) closes its torn-out tab when
    // this returns Ok — the window on this side already exists,
    // and it takes the focus (the torn-out tab is where the eye is).
    if let Some(new_window) = app.get_webview_window(&label) {
        let _ = new_window.set_focus();
    }
    Ok(label)
}

/// Merge-back (M23): hand a torn-off window's whole tab set to the
/// main window. The session JSON is relayed verbatim as a
/// `zim:merge-session` event (the main chrome validates it with
/// the same restore discipline as its own storage), then the
/// calling window closes — the shell forgets it.
#[tauri::command]
pub fn zim_merge_to_main(
    app: AppHandle,
    shared: State<'_, Arc<Shared>>,
    window: WebviewWindow,
    session: String,
) -> Result<(), String> {
    if session.len() > 256 * 1024 {
        return Err("that session is too large to merge".to_owned());
    }
    let payload = serde_json::json!({ "session": session });
    app.emit_to("main", "zim:merge-session", payload)
        .map_err(|e| format!("merge event failed: {e}"))?;
    shared.windows.remove(window.label());
    window
        .close()
        .map_err(|e| format!("closing the merged window failed: {e}"))
}

/// Open a fresh chrome window (the tray's "New window"): the plain
/// chrome, restoring whatever its per-window session key holds.
#[tauri::command]
pub fn zim_new_window(app: AppHandle, shared: State<'_, Arc<Shared>>) -> Result<String, String> {
    let port = shared
        .get()
        .port
        .ok_or_else(|| "the engine is not attached yet".to_owned())?;
    let label = next_label(shared.window_counter.fetch_add(1, Ordering::SeqCst));
    let url = format!("https://127.0.0.1:{port}/zim/app?window={label}");
    create_chrome_window(&app, &label, &url)?;
    shared.windows.add(&label);
    Ok(label)
}

/// Build one chrome window at an engine-origin URL.
///
/// `WebviewUrl::External` navigates at build time; the window is
/// sized like the main window and centered on the active screen
/// (Chrome tears off near-sized windows; exact pinning is polish).
/// A per-window close listener forgets the window in the registry —
/// app-level window events carry no label, so each chrome window
/// brings its own bookkeeping.
pub(crate) fn create_chrome_window(app: &AppHandle, label: &str, url: &str) -> Result<(), String> {
    let parsed = Url::parse(url).map_err(|e| format!("bad URL {url:?}: {e}"))?;
    let window = WebviewWindow::builder(app, label, WebviewUrl::External(parsed))
        .title("ZIM")
        .inner_size(1100.0, 720.0)
        .min_inner_size(520.0, 360.0)
        .build()
        .map_err(|e| format!("creating the window failed: {e}"))?;
    let app_for_close = app.clone();
    let label_for_close = label.to_owned();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::CloseRequested { .. })
            || matches!(event, tauri::WindowEvent::Destroyed)
        {
            let shared = app_for_close.state::<Arc<Shared>>();
            shared.windows.remove(&label_for_close);
        }
    });
    Ok(())
}
