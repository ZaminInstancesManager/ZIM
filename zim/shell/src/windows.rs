//! The shell's window registry (M23): which chrome windows exist.
//!
//! The shell owns OS windows; each chrome window owns its own tab
//! set (persisted under a per-window session key in the webview's
//! own storage, on the engine origin). What the shell persists is
//! only the *window set* — a list of labels — in `windows.json`
//! under the app-data dir; on a fresh boot, after the engine
//! attaches, each recorded window is recreated at
//! `/zim/app?window={label}` and the page restores its own session.
//! Nothing about tab state crosses the IPC boundary: sessions stay
//! view-only data in the frontend, where they already live.
//!
//! The main window (`"main"`) is never part of the registry: it is
//! the always-present chrome the tray recreates on demand
//! (M21's disposable-window rule). Labels are `chrome-{n}`, issued
//! monotonically per boot and recorded across boots.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The file the registry persists to (inside the shell's data dir).
const REGISTRY_FILE: &str = "windows.json";

/// The persisted shape (`{ "windows": ["chrome-1", ...] }`).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RegistryFile {
    windows: Vec<String>,
}

/// The in-memory + on-disk window registry.
pub struct WindowRegistry {
    labels: Mutex<BTreeSet<String>>,
    path: Mutex<Option<PathBuf>>,
}

impl WindowRegistry {
    /// A fresh registry; call [`WindowRegistry::load`] once the
    /// app-data dir exists (setup time).
    pub fn new() -> Self {
        Self {
            labels: Mutex::new(BTreeSet::new()),
            path: Mutex::new(None),
        }
    }

    /// Bind the registry to its on-disk location and load the
    /// previously persisted window set. A missing or corrupt file
    /// is an empty registry — losing remembered windows is a
    /// nicety, never a fault (the same discipline the chrome's
    /// session format uses).
    pub fn load(&self, data_dir: &Path) {
        let path = data_dir.join(REGISTRY_FILE);
        let parsed: Option<RegistryFile> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        let mut labels = self.labels.lock().expect("window registry mutex poisoned");
        if let Some(file) = parsed {
            for label in file.windows {
                if is_chrome_label(&label) {
                    labels.insert(label);
                }
            }
        }
        *self.path.lock().expect("window registry mutex poisoned") = Some(path);
    }

    /// The recorded window labels, in label order.
    pub fn labels(&self) -> Vec<String> {
        self.labels
            .lock()
            .expect("window registry mutex poisoned")
            .iter()
            .cloned()
            .collect()
    }

    /// Record a window (creating it via tear-off / the tray) and
    /// persist the set. Failures to persist are logged, not fatal.
    pub fn add(&self, label: &str) {
        if !is_chrome_label(label) {
            return;
        }
        let mut labels = self.labels.lock().expect("window registry mutex poisoned");
        labels.insert(label.to_owned());
        self.persist_locked(&labels);
    }

    /// Forget a window (closed by the user or merged away).
    pub fn remove(&self, label: &str) {
        let mut labels = self.labels.lock().expect("window registry mutex poisoned");
        labels.remove(label);
        self.persist_locked(&labels);
    }

    /// Whether a label belongs to a chrome window the registry
    /// tracks (anything but the main window).
    pub fn is_tracked(&self, label: &str) -> bool {
        label != "main"
    }

    /// Persist the current set (caller holds the lock).
    fn persist_locked(&self, labels: &BTreeSet<String>) {
        let path = self
            .path
            .lock()
            .expect("window registry mutex poisoned")
            .clone();
        let Some(path) = path else {
            return; // Not bound to disk yet — in-memory only.
        };
        let file = RegistryFile {
            windows: labels.iter().cloned().collect(),
        };
        match serde_json::to_string_pretty(&file) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    eprintln!(
                        "zim_shell: window registry write failed ({}): {e}",
                        path.display()
                    );
                }
            }
            Err(e) => eprintln!("zim_shell: window registry serialize failed: {e}"),
        }
    }
}

impl Default for WindowRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Chrome window labels the shell issues: `chrome-{n}`.
fn is_chrome_label(label: &str) -> bool {
    match label.strip_prefix("chrome-") {
        Some(digits) => !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// Mint the next window label from a monotonically increasing
/// counter (shared with [`WindowRegistry::add`]'s caller).
pub fn next_label(counter: u64) -> String {
    format!("chrome-{counter}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_roundtrip_through_disk() {
        let dir = std::env::temp_dir().join(format!("zim-window-registry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = WindowRegistry::new();
        registry.load(&dir);
        assert!(registry.labels().is_empty());

        registry.add("chrome-1");
        registry.add("chrome-2");
        registry.add("main"); // not a tracked chrome label
        registry.add("not-a-label");
        assert_eq!(
            registry.labels(),
            vec!["chrome-1".to_owned(), "chrome-2".to_owned()]
        );

        // A fresh registry reads the same set back.
        let reloaded = WindowRegistry::new();
        reloaded.load(&dir);
        assert_eq!(
            reloaded.labels(),
            vec!["chrome-1".to_owned(), "chrome-2".to_owned()]
        );

        reloaded.remove("chrome-1");
        assert_eq!(reloaded.labels(), vec!["chrome-2".to_owned()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_registry_file_is_an_empty_set() {
        let dir = std::env::temp_dir().join(format!("zim-window-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(REGISTRY_FILE), "{not json").unwrap();
        let registry = WindowRegistry::new();
        registry.load(&dir);
        assert!(registry.labels().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_main_window_is_untracked() {
        let registry = WindowRegistry::new();
        assert!(!registry.is_tracked("main"));
        assert!(registry.is_tracked("chrome-1"));
        assert!(registry.is_tracked("anything-else"));
    }

    #[test]
    fn labels_mint_from_the_counter() {
        assert_eq!(next_label(1), "chrome-1");
        assert_eq!(next_label(41), "chrome-41");
    }
}
