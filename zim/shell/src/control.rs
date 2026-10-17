//! The shell's engine-control client (M27): the coordinated stop.
//!
//! Two callers share one flow:
//!
//! - the **updater** (Windows): between download and install, the
//!   NSIS installer cannot replace the running sidecar engine's
//!   executable — the engine is asked to drain itself first (the
//!   durable fix UPDATES.md §3 recorded as M26 scope since M25);
//! - the **tray** ("Stop engine — servers keep running"): the
//!   operator-facing explicit stop; running containers are left
//!   running by the engine's own drain design (the next boot's
//!   reconciliation re-adopts them), and the shell's recovery page
//!   offers the re-start.
//!
//! The stop is a *request*: the engine-control endpoint answers 202
//! and drains on its side (jobs quiesce, supervision stops,
//! connections drain). This module then watches the port go quiet —
//! acceptance, not assumption — with the drain's own grace as the
//! ceiling.

use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::state::{EngineStatus, Phase, Shared};

/// How long the engine's drain may take before the caller gives up
/// waiting (mirrors the job system's shutdown grace; the engine's
/// own drain path is bounded by the same budget).
const DRAIN_WAIT: Duration = Duration::from_secs(35);

/// The poll cadence while watching the drain.
const DRAIN_POLL: Duration = Duration::from_millis(250);

/// Ask the running engine to shut itself down and watch it drain.
///
/// `Ok(())` means the engine accepted the request AND its port went
/// quiet — the process is stopped (or stopping into its final
/// seconds). `Err` carries the honest reason and MUST be surfaced
/// by the caller (the update refuses to proceed; the tray shows
/// the failure) rather than retried blindly.
///
/// No engine attached (nothing on the recorded port) is a success:
/// there is nothing to stop, which is exactly what the Windows
/// update path wants.
pub async fn stop_engine(app: &AppHandle) -> Result<(), String> {
    let shared = app.state::<std::sync::Arc<Shared>>();
    let status = shared.get();
    let Some(port) = status.port else {
        return Ok(()); // nothing is attached; nothing to stop
    };
    let data_dir = shared
        .options
        .data_dir
        .clone()
        .unwrap_or_else(|| default_engine_data_dir(app));

    // The request itself is a short, bounded blocking call (the
    // shell's hand-rolled HTTPS client) — keep it off the async
    // runtime's worker.
    tokio::task::spawn_blocking(move || crate::engine::request_engine_shutdown(&data_dir, port))
        .await
        .map_err(|e| format!("the engine-control task panicked: {e}"))??;

    // Watch the drain: the health probe stops answering.
    let deadline = tokio::time::Instant::now() + DRAIN_WAIT;
    loop {
        let quiet = tokio::task::spawn_blocking(move || crate::engine::probe(port).is_err())
            .await
            .unwrap_or(true);
        if quiet {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "the engine accepted the shutdown but is still draining after {}s \
                 (jobs quiescing); wait a moment and retry",
                DRAIN_WAIT.as_secs()
            ));
        }
        tokio::time::sleep(DRAIN_POLL).await;
    }
}

/// Reflect a successful user-facing stop in the shell's surfaces:
/// the recovery page with a re-start (its Retry re-runs the attach
/// sequence, which spawns a fresh engine) and the tray tooltip.
pub fn engine_stopped_surfaces(app: &AppHandle) {
    let shared = app.state::<std::sync::Arc<Shared>>();
    let stopped = EngineStatus {
        phase: Phase::Trouble,
        port: None,
        version: None,
        message: Some(
            "the engine was stopped (your servers keep running; the next start \
             re-adopts them)"
                .to_owned(),
        ),
    };
    let _ = shared.set(stopped);
    crate::navigate_to_page(app, "down.html");
    crate::tray::set_engine_tooltip(app, "ZIM — engine stopped (servers keep running)");
}

/// The engine data dir default, resolved exactly as the attach
/// sequence resolves it (one source of truth for both).
fn default_engine_data_dir(app: &AppHandle) -> std::path::PathBuf {
    crate::default_engine_data_dir(app)
}
