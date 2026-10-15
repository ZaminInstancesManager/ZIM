//! The shell-discovery file (`engine.json` in the data dir).
//!
//! M21: the desktop shell (`zim/shell/`) needs to find a running
//! engine — its loopback port, PID, and version — without scraping
//! logs or guessing ports. At boot the engine atomically writes a
//! small JSON descriptor next to `engine.db`; on graceful shutdown
//! it removes it. A stale file (crash, kill -9) is harmless: the
//! shell probes `/api/v1/health` on the recorded port and spawns a
//! fresh engine when nothing answers — the SQLite file lock is what
//! actually keeps two engines from ever double-serving one data dir.
//!
//! The file is public operational metadata (no secrets: no paths,
//! no credentials, no key material); it lives under the same
//! access rules as the rest of the data dir.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The discovery file name inside the data directory.
pub const DISCOVERY_FILE: &str = "engine.json";

/// The discovery schema version (bump on breaking shape changes;
/// readers that do not understand it ignore the file).
const DISCOVERY_SCHEMA: u8 = 1;

/// The descriptor the engine writes at boot and the shell reads to
/// attach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryInfo {
    /// The discovery schema version (`schema`).
    pub schema: u8,
    /// The engine process id (`pid`).
    pub pid: u32,
    /// The bound loopback port (`port`).
    pub port: u16,
    /// The engine version string (`engine_version`).
    pub engine_version: String,
    /// Boot time, seconds since the Unix epoch (`started_at_unix`).
    pub started_at_unix: u64,
}

impl DiscoveryInfo {
    /// The current process's descriptor for a bound port.
    pub fn current(port: u16) -> Self {
        Self {
            schema: DISCOVERY_SCHEMA,
            pid: std::process::id(),
            port,
            engine_version: env!("CARGO_PKG_VERSION").to_owned(),
            started_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }
}

/// Write the discovery file atomically (write a sibling `.tmp`,
/// then rename over the target — `std::fs::rename` replaces an
/// existing target on both Unix and Windows, so a stale file from
/// a crashed engine is overwritten, never merged).
pub fn write(data_dir: &Path, info: &DiscoveryInfo) -> std::io::Result<()> {
    let final_path = data_dir.join(DISCOVERY_FILE);
    let tmp_path = data_dir.join(format!("{DISCOVERY_FILE}.tmp"));
    let body = serde_json::to_string(info)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp_path, body)?;
    std::fs::rename(&tmp_path, &final_path)
}

/// Read the discovery file.
///
/// `None` when the file is absent, unreadable, malformed, or a
/// schema the reader does not understand — every caller treats
/// that identically: probe the fallback port, then consider
/// spawning an engine.
pub fn read(data_dir: &Path) -> Option<DiscoveryInfo> {
    let body = std::fs::read_to_string(data_dir.join(DISCOVERY_FILE)).ok()?;
    let info: DiscoveryInfo = serde_json::from_str(&body).ok()?;
    (info.schema == DISCOVERY_SCHEMA).then_some(info)
}

/// Remove the discovery file (graceful shutdown). Absence is fine —
/// a crashed engine leaves the file behind by design.
pub fn remove(data_dir: &Path) {
    let _ = std::fs::remove_file(data_dir.join(DISCOVERY_FILE));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private scratch dir under the system temp dir (the std-only
    /// pattern this crate's smoke test uses).
    fn scratch_dir(label: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "zim_discovery_{label}_{}_{}",
            std::process::id(),
            stamp
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn roundtrip_write_read_remove() {
        let dir = scratch_dir("roundtrip");
        assert!(read(&dir).is_none(), "no discovery before boot");

        write(&dir, &DiscoveryInfo::current(49152)).unwrap();
        let info = read(&dir).expect("discovery readable after write");
        assert_eq!(info.port, 49152);
        assert_eq!(info.schema, 1);
        assert!(info.pid != 0);
        assert!(!info.engine_version.is_empty());

        remove(&dir);
        assert!(read(&dir).is_none(), "discovery gone after shutdown");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_replaces_a_stale_file() {
        let dir = scratch_dir("stale");
        write(&dir, &DiscoveryInfo::current(1111)).unwrap();
        // A "crashed" engine's file is still there…
        assert_eq!(read(&dir).unwrap().port, 1111);
        // …and the next boot overwrites it wholesale.
        write(&dir, &DiscoveryInfo::current(2222)).unwrap();
        assert_eq!(read(&dir).unwrap().port, 2222);
        remove(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_or_future_files_are_ignored() {
        let dir = scratch_dir("malformed");
        std::fs::write(dir.join(DISCOVERY_FILE), "not json").unwrap();
        assert!(read(&dir).is_none(), "malformed file reads as absent");

        // A future schema is not this reader's business: ignore.
        let future = serde_json::json!({
            "schema": 99,
            "pid": 1,
            "port": 8443,
            "engine_version": "future",
            "started_at_unix": 0
        });
        std::fs::write(dir.join(DISCOVERY_FILE), future.to_string()).unwrap();
        assert!(read(&dir).is_none(), "future schema reads as absent");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
