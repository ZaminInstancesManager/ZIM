//! Engine-attach state shared between the Rust shell, the tray, and
//! the bundled UI pages.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Mutex;

use serde::Serialize;

use crate::windows::WindowRegistry;

/// The stored handle type for the tray's update-check item (M25).
pub type UpdateMenuItem = Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>>;

/// The attach sequence's phases, in order.
///
/// Serialized snake_case to the UI pages (`engine-status` events
/// and the `engine_status` command).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Before the first attach attempt has reported anything.
    Waiting,
    /// Probing a discovered/default port for a running engine.
    Attaching,
    /// A detached engine process was spawned; waiting for health.
    Spawning,
    /// Engine healthy; the window is (or is about to be) on the
    /// engine origin.
    Ready,
    /// The engine did not come up (or the cert could not be
    /// trusted); the recovery page carries `message`.
    Trouble,
}

/// The attach status snapshot the UI sees.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct EngineStatus {
    /// The current phase.
    pub phase: Phase,
    /// The engine's loopback port once known.
    pub port: Option<u16>,
    /// The engine's version string once known.
    pub version: Option<String>,
    /// The diagnostic for the `trouble` phase.
    pub message: Option<String>,
}

impl EngineStatus {
    /// A fresh pre-attach status.
    pub fn waiting() -> Self {
        Self {
            phase: Phase::Waiting,
            port: None,
            version: None,
            message: None,
        }
    }
}

/// Shared shell state: the frozen CLI options, the attach status,
/// the one-flight guard for retry attempts, and (M23) the window
/// registry plus the chrome-window label counter.
pub struct Shared {
    /// The parsed options (frozen at boot; retries reuse them).
    pub options: crate::cli::Options,
    status: Mutex<EngineStatus>,
    /// True while an attach sequence is running (prevents retry
    /// dogpiles from the recovery page).
    pub attaching: AtomicBool,
    /// The chrome-window label mint (M23 multi-window).
    pub window_counter: AtomicU64,
    /// The chrome windows the shell knows about (M23).
    pub windows: WindowRegistry,
    /// The tray's "Check for updates…" item (M25, AD-31): stored
    /// by `tray::init` so the updater can retitle it as a check
    /// progresses — the item's own label is the alpha channel's
    /// progress surface. `None` until the tray is built.
    pub update_menu_item: UpdateMenuItem,
}

impl Shared {
    /// New state for a boot with the given options.
    pub fn new(options: crate::cli::Options) -> Self {
        Self {
            options,
            status: Mutex::new(EngineStatus::waiting()),
            attaching: AtomicBool::new(false),
            window_counter: AtomicU64::new(1),
            windows: WindowRegistry::new(),
            update_menu_item: Mutex::new(None),
        }
    }

    /// Replace the status and return it for emission.
    pub fn set(&self, status: EngineStatus) -> EngineStatus {
        *self
            .status
            .lock()
            .expect("status mutex poisoned by a panicked attach thread") = status.clone();
        status
    }

    /// The current status snapshot.
    pub fn get(&self) -> EngineStatus {
        self.status
            .lock()
            .expect("status mutex poisoned by a panicked attach thread")
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serializes_snake_case_for_the_ui() {
        let json = serde_json::to_string(&EngineStatus::waiting()).unwrap();
        assert!(json.contains("\"phase\":\"waiting\""));

        let ready = EngineStatus {
            phase: Phase::Ready,
            port: Some(8443),
            version: Some("0.19.0".to_owned()),
            message: None,
        };
        let json = serde_json::to_string(&ready).unwrap();
        assert!(json.contains("\"phase\":\"ready\""));
        assert!(json.contains("\"port\":8443"));
    }

    #[test]
    fn set_then_get_roundtrips() {
        let shared = Shared::new(crate::cli::Options::default());
        let trouble = EngineStatus {
            phase: Phase::Trouble,
            port: None,
            version: None,
            message: Some("engine.log: bind failed".to_owned()),
        };
        shared.set(trouble);
        assert_eq!(shared.get().phase, Phase::Trouble);
        assert_eq!(
            shared.get().message.as_deref(),
            Some("engine.log: bind failed")
        );
    }
}
