//! The update channel mechanics (M25, AD-31): one manual,
//! consent-shaped "Check for updates…" tray action that queries
//! the channel, applies what it finds, and relaunches.
//!
//! Scope discipline: this module is *mechanics*, not UX. The
//! alpha channel surfaces its state where the consent happened —
//! the tray menu item's own label (the handle lives in
//! [`crate::state::Shared`]) — plus stderr; an in-app update page
//! is release polish (M26). There is deliberately no automatic
//! check-on-boot: updates install only when the user asks for
//! one, and the signature gate (minisign, the public key baked
//! into this binary via tauri.conf.json) is what makes that safe.
//! The channel's contract — endpoint, manifest, granularity,
//! rollback — is documented in docs/architecture/UPDATES.md.

use std::sync::Arc;

use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::state::Shared;

/// The tray item's resting label (restored after any check).
const RESTING_LABEL: &str = "Check for updates…";

/// Run one check-then-apply cycle (spawned from the tray menu).
pub fn check_and_apply(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        set_label(&app, "Checking for updates…");
        let check = async {
            let updater = app
                .updater_builder()
                .build()
                .map_err(|error| format!("updater construction failed: {error}"))?;
            updater
                .check()
                .await
                .map_err(|error| format!("channel check failed: {error}"))
        };
        match check.await {
            Ok(Some(update)) => install(app, update).await,
            Ok(None) => {
                set_label(&app, "Up to date");
                eprintln!("zim_shell: updater: up to date");
                restore_label_later(app);
            }
            Err(message) => {
                set_label(&app, "Update check failed");
                eprintln!("zim_shell: updater: {message}");
                restore_label_later(app);
            }
        }
    });
}

/// Download, verify, coordinate, install, and relaunch. The consent
/// already happened — the user clicked the item knowing an update
/// may install — but the signature must still verify, or nothing
/// installs.
///
/// The M27 sequencing (Windows): the NSIS installer cannot replace a
/// running executable, and the sidecar engine executes from the
/// install directory — so the download completes FIRST (a failed
/// download must never have stopped anybody's engine), then the
/// engine is asked to drain itself through the control plane
/// (`crate::control`), and only then does the installer run. On
/// POSIX the file is replaced under the running engine without
/// issue (UPDATES.md §3's granularity model) and the engine keeps
/// running through the update.
async fn install(app: AppHandle, update: Update) {
    let version = update.version.clone();
    set_label(&app, &format!("Downloading v{version}…"));
    eprintln!("zim_shell: updater: v{version} available, downloading");
    let mut downloaded: u64 = 0;
    let download = update
        .download(|chunk, total| {
            downloaded += chunk as u64;
            if let Some(total) = total {
                let percent = (downloaded as f64 / total as f64 * 100.0).round() as u64;
                set_label(&app, &format!("Downloading v{version}… {percent}%"));
            }
        })
        .await;
    if let Err(error) = download {
        set_label(&app, "Update failed");
        eprintln!("zim_shell: updater: download failed: {error}");
        restore_label_later(app);
        return;
    }

    // Windows only: stop the engine BEFORE the installer replaces
    // its files. Servers keep running (the engine's drain leaves
    // containers running by design; the next boot re-adopts them).
    #[cfg(target_os = "windows")]
    {
        set_label(&app, "Stopping the engine…");
        match crate::control::stop_engine(&app).await {
            Ok(()) => {}
            Err(message) => {
                set_label(&app, "Update failed — engine still running");
                eprintln!(
                    "zim_shell: updater: cannot stop the engine for the install: {message}"
                );
                restore_label_later(app);
                return;
            }
        }
    }

    set_label(&app, &format!("Installing v{version}…"));
    match update.install().await {
        Ok(()) => {
            eprintln!("zim_shell: updater: v{version} installed; relaunching");
            // The engine (already drained on Windows; untouched
            // elsewhere) is a separate, detached process: the
            // relaunch replaces the shell only, and the attach
            // sequence probes for a healthy engine before ever
            // spawning one. See UPDATES.md §granularity for what
            // updates when.
            app.restart();
        }
        Err(error) => {
            set_label(&app, "Update failed");
            eprintln!("zim_shell: updater: install failed: {error}");
            restore_label_later(app);
        }
    }
}

/// Point the tray item's label at `text` (the item handle was
/// stored by `tray::init`; a missing tray is a no-op, not an error).
fn set_label(app: &AppHandle, text: &str) {
    let shared = app.state::<Arc<Shared>>();
    // Clone the handle out of the guard so the mutex's borrow ends
    // inside this statement (a `MenuItem` is an owned, cheap native
    // handle — cloning it is the menu API's own pattern).
    let item = shared
        .update_menu_item
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    if let Some(item) = item {
        let _ = item.set_text(text);
    }
}

/// Reset the tray item's label after the state has been read —
/// the label is a flash surface, not a log. A plain thread: the
/// async runtime has no sleep of its own, and this is a one-shot.
fn restore_label_later(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(10));
        set_label(&app, RESTING_LABEL);
    });
}
