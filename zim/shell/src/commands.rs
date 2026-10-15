//! The bundled UI pages' command surface (the pages are shell-owned
//! — they talk to the shell, never directly to the engine).
//!
//! Two commands, kept deliberately tiny: the pre-engine pages need
//! to know where the attach sequence stands (`engine_status`) and
//! ask for another attempt (`engine_retry`). Nothing else.

use std::sync::Arc;

use tauri::AppHandle;
use tauri::State;

use crate::state::{EngineStatus, Shared};

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
