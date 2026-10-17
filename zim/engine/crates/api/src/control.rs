//! The local engine-control plane (M27, UPDATES.md §3's durable fix).
//!
//! The desktop shell owns the update channel's granularity decision:
//! the bundle (shell + engine bytes) updates together, but the
//! *running* engine is a detached, independently supervised process
//! that updates only when it next restarts. On POSIX that is purely
//! informational; on Windows the NSIS installer cannot replace a
//! running executable, so an update while the sidecar engine runs
//! from the install directory fails cleanly. The durable fix —
//! recorded in UPDATES.md §3 since M25 — is a **coordinated engine
//! shutdown as part of the update's pre-install step**, and this
//! module is that mechanism's engine half.
//!
//! # The design
//!
//! - At boot the engine generates a fresh 256-bit random token and
//!   writes it to `engine.token` in the data dir (mode 0600 on
//!   POSIX; on Windows the user-profile ACL carries the same
//!   intent). The discovery file stays **public operational
//!   metadata with no secrets** (its own documented contract); the
//!   token is deliberately a *separate* file.
//! - `POST /api/v1/engine/control` (see `handlers::control`) demands
//!   the token in the `X-ZIM-Engine-Token` header, compared in
//!   constant time. Possession of the token == "a process that can
//!   read the engine's data dir" == the machine-local operator
//!   context the shell already occupies. It is NOT a user
//!   credential: user authz never flows through this endpoint, and
//!   the token grants exactly one power — asking the engine to stop
//!   itself the same way SIGTERM would.
//! - The handler fires a [`ShutdownHandle`]; `run()` selects on the
//!   OS signals *or* that handle, so a coordinated stop and a
//!   service stop drain through the identical path (jobs quiesce,
//!   supervision stops, connections drain, discovery + token files
//!   are removed).
//!
//! # Threat posture (THREAT_MODEL.md §"local control plane")
//!
//! An attacker who can read `engine.token` can already read
//! `engine.db` (the same directory, same or weaker protection) —
//! at which point they hold the data directly and stopping the
//! engine adds nothing. The token's job is to keep *remote* callers
//! (anything not on the loopback origin the host guard already
//! enforces) and *other local processes without data-dir access*
//! from triggering shutdowns, and to make the shell's coordination
//! an authenticated, auditable act rather than a raw process kill.

use std::path::Path;

use rand_core::{OsRng, RngCore};
use tokio::sync::Notify;

/// The token file's name inside the data directory.
pub const TOKEN_FILE: &str = "engine.token";

/// The header carrying the control token.
pub const TOKEN_HEADER: &str = "x-zim-engine-token";

/// The number of random bytes in a control token.
const TOKEN_BYTES: usize = 32;

/// A freshly generated per-boot control token.
///
/// Held as raw bytes; the wire form is standard base64 (no padding
/// concerns — 32 bytes encode to 44 characters) so the shell's
/// client side never has to think about encodings.
#[derive(Debug, Clone)]
pub struct ControlToken {
    bytes: [u8; TOKEN_BYTES],
}

impl ControlToken {
    /// Generate a new token from the OS RNG.
    pub fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_BYTES];
        OsRng.fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// The wire form (standard base64).
    pub fn as_base64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(self.bytes)
    }

    /// Parse the wire form; `None` on any malformed input.
    pub fn from_base64(raw: &str) -> Option<Self> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(raw.trim())
            .ok()?;
        if bytes.len() != TOKEN_BYTES {
            return None;
        }
        let mut out = [0u8; TOKEN_BYTES];
        out.copy_from_slice(&bytes);
        Some(Self { bytes: out })
    }

    /// Constant-time equality (this is not a secrecy-critical
    /// HotPath, but a data-dependent early return would turn the
    /// token check into a byte-by-byte timing oracle for no gain).
    pub fn matches(&self, other: &ControlToken) -> bool {
        let mut diff = 0u8;
        for (a, b) in self.bytes.iter().zip(other.bytes.iter()) {
            diff |= a ^ b;
        }
        diff == 0
    }
}

impl PartialEq for ControlToken {
    fn eq(&self, other: &Self) -> bool {
        self.matches(other)
    }
}

/// The shutdown side of the control plane: many holders, one
/// notification, exactly one drain.
///
/// `trigger()` wakes every waiter registered at trigger time and
/// stores one permit — the guarantee that matters for the run loop:
/// the API server starts serving *inside* `startup()`, before
/// `run()`'s select registers, so a control request that lands in
/// that window must still wake the eventual waiter. Exactly one
/// such late waiter is guaranteed (one permit); the engine has
/// exactly one (the run loop's select).
#[derive(Debug, Clone, Default)]
pub struct ShutdownHandle {
    inner: std::sync::Arc<Notify>,
}

impl ShutdownHandle {
    /// A handle that has not been triggered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request the graceful drain (idempotent).
    pub fn trigger(&self) {
        self.inner.notify_waiters();
        // A second notify ensures late `notified()` futures (created
        // after the first notify_waiters but before the drain
        // completes) also resolve: Notify permits are not stored by
        // notify_waiters, so we also store one permit.
        self.inner.notify_one();
    }

    /// Resolve once a drain has been requested.
    pub async fn notified(&self) {
        self.inner.notified().await;
    }
}

/// The assembled control plane the API state carries.
#[derive(Debug, Clone)]
pub struct ControlPlane {
    token: ControlToken,
    shutdown: ShutdownHandle,
}

impl ControlPlane {
    /// Assemble from a generated token.
    pub fn new(token: ControlToken) -> Self {
        Self {
            token,
            shutdown: ShutdownHandle::new(),
        }
    }

    /// A plane for embedders/tests that never expose the endpoint's
    /// secret (any presented token is simply wrong).
    pub fn for_tests() -> Self {
        Self::new(ControlToken::generate())
    }

    /// The shutdown side (handed to the engine's run loop).
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        self.shutdown.clone()
    }

    /// Verify a presented wire token.
    pub fn verify(&self, presented: &str) -> bool {
        match ControlToken::from_base64(presented) {
            Some(candidate) => self.token.matches(&candidate),
            None => false,
        }
    }

    /// The token's wire form (the boot code writes it to disk).
    pub fn token_base64(&self) -> String {
        self.token.as_base64()
    }
}

/// Persist the token file (write + 0600 on POSIX). Like the
/// discovery file, a write failure degrades — the endpoint simply
/// refuses every presented token — never blocks boot.
pub fn write_token(data_dir: &Path, plane: &ControlPlane) -> std::io::Result<()> {
    let path = data_dir.join(TOKEN_FILE);
    std::fs::write(&path, plane.token_base64())?;
    restrict_permissions(&path);
    Ok(())
}

/// Read the token's wire form back (the shell's client side).
pub fn read_token(data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(data_dir.join(TOKEN_FILE))
        .ok()
        .map(|raw| raw.trim().to_owned())
        .filter(|raw| !raw.is_empty())
}

/// Remove the token file (graceful shutdown). Absence is fine — a
/// crashed engine leaves it behind by design, and the next boot
/// overwrites it with fresh material.
pub fn remove_token(data_dir: &Path) {
    let _ = std::fs::remove_file(data_dir.join(TOKEN_FILE));
}

/// Best-effort 0600 on POSIX; Windows relies on the profile ACL.
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(mut perms) = std::fs::metadata(path).map(|m| m.permissions()) {
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_roundtrips_through_base64() {
        let token = ControlToken::generate();
        let wire = token.as_base64();
        assert_eq!(wire.len(), 44);
        let back = ControlToken::from_base64(&wire).expect("parses");
        assert!(token.matches(&back));
    }

    #[test]
    fn malformed_tokens_are_rejected_not_panicked() {
        assert!(ControlToken::from_base64("").is_none());
        assert!(ControlToken::from_base64("!!!not-base64!!!").is_none());
        assert!(ControlToken::from_base64(&"A".repeat(43)).is_none());
        // Right alphabet, wrong length: decodes but is not 32 bytes.
        assert!(ControlToken::from_base64(&"A".repeat(44)).is_none());
    }

    #[test]
    fn generated_tokens_differ() {
        assert_ne!(ControlToken::generate(), ControlToken::generate());
    }

    #[test]
    fn plane_verifies_only_the_right_token() {
        let plane = ControlPlane::new(ControlToken::generate());
        assert!(plane.verify(&plane.token_base64()));
        assert!(!plane.verify(&ControlToken::generate().as_base64()));
        assert!(!plane.verify("garbage"));
    }

    #[tokio::test]
    async fn shutdown_handle_resolves_a_preregistered_waiter() {
        let plane = ControlPlane::new(ControlToken::generate());
        let handle = plane.shutdown_handle();
        let first = tokio::spawn(async move {
            handle.notified().await;
        });
        // Let the waiter register before triggering.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        plane.shutdown_handle().trigger();
        plane.shutdown_handle().trigger(); // idempotent
        tokio::time::timeout(std::time::Duration::from_secs(1), first)
            .await
            .expect("preregistered waiter resolves")
            .expect("waiter task joins");
    }

    #[tokio::test]
    async fn shutdown_handle_covers_the_run_loop_race() {
        // The API server starts serving inside startup(), BEFORE
        // run()'s select registers its waiter: a trigger that fires
        // in that window must still wake the eventual waiter (the
        // stored permit is the guarantee).
        let plane = ControlPlane::new(ControlToken::generate());
        let handle = plane.shutdown_handle();
        plane.shutdown_handle().trigger();
        tokio::time::timeout(std::time::Duration::from_secs(1), handle.notified())
            .await
            .expect("the late waiter resolves via the stored permit");
    }
}
