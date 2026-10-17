//! The SFTP transport (M28) — one chrooted tree, one more doorway.
//!
//! ZIM's file surface has one tree per instance
//! (`{data_dir}/instances/{uuid_short}`) and, since M28's first
//! batch, one HTTP grammar over it. This crate is the second
//! doorway: a real SSH server (russh, pure Rust, `ring` backend)
//! whose SFTP subsystem serves the same tree through the same
//! [`zim_engine_files::Filesystem`] chroot and the same `file.*`
//! grant vocabulary, for the FileZilla / WinSCP / `sftp`-CLI world
//! Pterodactyl migrators already live in. Nothing here is a second
//! implementation of file policy: every operation resolves through
//! the files crate's §2.6 grammar, and every permission decision is
//! the §10.3 grant table.
//!
//! # The session model (AD-33)
//!
//! - **Authentication** is the engine's own user records: SSH
//!   "password" auth against the Argon2id hashes (username or email
//!   as the login name), bridged off the async workers exactly like
//!   the HTTP login route, with per-username throttling on top of
//!   russh's constant-time rejection. Publickey auth is refused
//!   (offered keys are rejected before signature verification).
//! - **The synthetic root**: after login, the session's `/` is a
//!   directory listing one entry per instance the user can see
//!   (owner, admin, or permission row — the same visibility rule as
//!   `load_instance_for_caller`, uniform invisibility for
//!   strangers). Each entry is the instance's sanitized display
//!   name; descending into it chroots into that instance's data
//!   directory. A move across instances is refused (a rename is one
//!   operation inside one chroot).
//! - **Grants per operation**: readdir/stat need `file.read`; byte
//!   reads need `file.read_content`; writes/truncates need
//!   `file.write`; mkdir needs `file.create`; remove/rmdir need
//!   `file.delete`; rename needs `file.write` — the HTTP table,
//!   operation for operation.
//! - **Durability**: uploads stream into real file handles
//!   (seek+write at the offsets the protocol carries), so an SFTP
//!   upload is NOT the HTTP route's atomic temp-then-rename write —
//!   a crashed transfer can leave a partial file, exactly like
//!   every stock SFTP server. The HTTP route stays atomic; the
//!   tradeoff is recorded in AD-33.
//! - **Audit**: logins audit as `auth.login` with
//!   `"transport": "sftp"`; a file written over SFTP audits one
//!   `instance.file.write` row at handle close (path, total bytes,
//!   `"transport": "sftp"`) — one row per file, not per 32 KB
//!   packet. A session that dies mid-transfer loses that row (the
//!   bytes themselves are still on disk); also AD-33.
//! - **The host key** is an Ed25519 key generated on first boot and
//!   persisted (`sftp_host_ed25519` in the data dir, 0600 on POSIX),
//!   re-generated on unreadable — the same lifecycle discipline as
//!   the loopback TLS certificate.
//! - **The listener** defaults to `127.0.0.1:2022` (Wings's own
//!   SFTP port), loopback-scoped like the HTTPS API; operators can
//!   widen it through configuration when they mean to.
//!
//! # What this crate deliberately does not do
//!
//! - No symlink creation or readlink through SFTP (§2.6's doctrine:
//!   links are the traversal vector; the chroot resolves them
//!   defensively but the engine does not mint new ones).
//! - No setstat/fsetstat (no chmod/chown/utimes over SFTP).
//! - No host-certificate or keyboard-interactive auth.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod auth;
mod session;
mod vfs;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use russh::server::Server as _;
use zim_engine_application::ports::Persistence;

/// The canonical crate name, useful for runtime identification in
/// logs and crash reports.
pub const CRATE_NAME: &str = "zim_engine_sftp";

/// The SFTP listener's configuration.
#[derive(Debug, Clone)]
pub struct SftpOptions {
    /// Whether the listener starts at all (`ZIM_SFTP_ENABLED=0`
    /// keeps the engine file-API-only).
    pub enabled: bool,
    /// The address to bind (loopback by default, like the HTTPS
    /// API; a wider bind is an operator's deliberate choice).
    pub bind: IpAddr,
    /// The port to bind — 2022 is Wings's own SFTP default, kept so
    /// a migrator's muscle memory keeps working.
    pub port: u16,
}

impl Default for SftpOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 2022,
        }
    }
}

impl SftpOptions {
    /// Fill from the environment: `ZIM_SFTP_ENABLED` (0/false/off
    /// disables), `ZIM_SFTP_PORT` (u16).
    pub fn from_env(base: Self) -> Self {
        let mut opts = base;
        if let Ok(value) = std::env::var("ZIM_SFTP_ENABLED") {
            let value = value.trim().to_ascii_lowercase();
            if matches!(value.as_str(), "0" | "false" | "off" | "no") {
                opts.enabled = false;
            }
        }
        if let Ok(port) = std::env::var("ZIM_SFTP_PORT") {
            if let Ok(port) = port.trim().parse::<u16>() {
                opts.port = port;
            }
        }
        opts
    }
}

/// Everything a connection needs, shared across the process: the
/// persistence port, the engine data root, and the login throttle.
pub(crate) struct SftpShared {
    /// Durable metadata store (sync port; bridged with
    /// spawn_blocking, the engine's standing discipline).
    pub persistence: Arc<dyn Persistence>,
    /// The engine data root — instance trees live under
    /// `{data_dir}/instances/{uuid_short}`.
    pub data_dir: PathBuf,
    /// Per-username failed-login throttle (auth.rs).
    pub throttle: auth::AttemptThrottle,
}

impl SftpShared {
    /// Assemble the shared state.
    pub fn new(persistence: Arc<dyn Persistence>, data_dir: PathBuf) -> Self {
        Self {
            persistence,
            data_dir,
            throttle: auth::AttemptThrottle::default(),
        }
    }
}

/// The host-key file name inside the data directory.
const HOST_KEY_FILE: &str = "sftp_host_ed25519";

/// Load the persisted Ed25519 host key, generating and persisting a
/// fresh one on first boot (or when the stored key is unreadable —
/// a truncated write regenerates, exactly like the loopback TLS
/// certificate's lifecycle).
fn load_or_generate_host_key(data_dir: &Path) -> Result<russh::keys::PrivateKey, String> {
    let key_path = data_dir.join(HOST_KEY_FILE);
    if key_path.exists() {
        let pem = std::fs::read_to_string(&key_path)
            .map_err(|e| format!("cannot read {}: {e}", key_path.display()))?;
        match russh::keys::decode_secret_key(&pem, None) {
            Ok(key) => return Ok(key),
            Err(e) => {
                eprintln!("zim_engine_sftp: host key unreadable ({e}); regenerating");
            }
        }
    }

    let key = russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
        .map_err(|e| format!("cannot generate the SFTP host key: {e}"))?;
    let pem = key
        .to_openssh(russh::keys::ssh_key::LineEnding::LF)
        .map_err(|e| format!("cannot encode the SFTP host key: {e}"))?;
    std::fs::write(&key_path, pem.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", key_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(key)
}

/// A running SFTP listener: the bound address plus the shutdown
/// path (russh's `RunningServerHandle` + the task awaiting its
/// drain).
pub struct RunningSftp {
    /// The address the listener bound (an ephemeral `0` port
    /// resolves here — the test suite's shape).
    pub local_addr: SocketAddr,
    shutdown: russh::server::RunningServerHandle,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl RunningSftp {
    /// Ask every connection to drain (russh sends
    /// SSH_MSG_DISCONNECT per client), then wait for the accept
    /// loop to finish. Running containers and file bytes are
    /// untouched — this is transport shutdown only.
    pub async fn shutdown(self) {
        self.shutdown.shutdown("engine shutdown".to_owned());
        if let Err(e) = self.task.await {
            eprintln!("zim_engine_sftp: accept loop ended poorly: {e}");
        }
    }
}

/// Start the SFTP listener (the composition root calls this after
/// the HTTPS API is up; a failure to bind is the caller's decision
/// to treat as fatal — the engine treats it as a boot failure
/// because the operator asked for the surface).
pub async fn start(
    persistence: Arc<dyn Persistence>,
    data_dir: PathBuf,
    options: &SftpOptions,
) -> Result<RunningSftp, String> {
    let shared = Arc::new(SftpShared::new(persistence, data_dir.clone()));
    let host_key = load_or_generate_host_key(&data_dir)?;

    let config = russh::server::Config {
        // The identification string clients see in their logs.
        server_id: russh::SshId::Standard(std::borrow::Cow::Borrowed("SSH-2.0-ZaminZIM")),
        // Password auth only — publickey is refused before signature
        // verification (session.rs), keyboard-interactive never
        // offered.
        methods: russh::MethodSet::from(&[russh::MethodKind::Password][..]),
        // Constant-time rejections (russh enforces the floor).
        auth_rejection_time: Duration::from_secs(3),
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![host_key],
        // Six password tries per connection, then disconnect — the
        // throttle (per-username, cross-connection) is the real
        // brute-force brake.
        max_auth_attempts: 6,
        // Idle sessions do not hold engine shutdown hostage forever.
        inactivity_timeout: Some(Duration::from_secs(600)),
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    };

    let listener = tokio::net::TcpListener::bind((options.bind, options.port))
        .await
        .map_err(|e| format!("cannot bind SFTP listener on {}: {e}", options.port))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| format!("cannot resolve the SFTP listener address: {e}"))?;

    // run_on_socket's future borrows the server and the listener, so
    // the accept loop must own them: spawn a block that moves both
    // in and hands the shutdown handle back through a oneshot.
    let (handle_tx, handle_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut server = session::ZimSshServer { shared };
        let running = server.run_on_socket(Arc::new(config), &listener);
        let _ = handle_tx.send(running.handle());
        running.await
    });
    let shutdown = handle_rx
        .await
        .map_err(|e| format!("the SFTP accept loop died at startup: {e}"))?;
    Ok(RunningSftp {
        local_addr,
        shutdown,
        task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_sftp");
    }

    #[test]
    fn defaults_are_loopback_and_wings_port() {
        let options = SftpOptions::default();
        assert!(options.enabled);
        assert_eq!(options.bind, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(options.port, 2022);
    }

    #[test]
    fn env_can_disable_and_retarget() {
        std::env::set_var("ZIM_SFTP_ENABLED", "0");
        std::env::set_var("ZIM_SFTP_PORT", "2222");
        let options = SftpOptions::from_env(SftpOptions::default());
        std::env::remove_var("ZIM_SFTP_ENABLED");
        std::env::remove_var("ZIM_SFTP_PORT");
        assert!(!options.enabled);
        assert_eq!(options.port, 2222);
    }

    #[test]
    fn host_key_round_trips_through_the_data_dir() {
        let dir = std::env::temp_dir().join(format!("zim-sftp-key-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = load_or_generate_host_key(&dir).unwrap();
        let hash = russh::keys::HashAlg::Sha256;
        // The persisted form loads back as the same key.
        let second = load_or_generate_host_key(&dir).unwrap();
        assert_eq!(
            first.public_key().fingerprint(hash),
            second.public_key().fingerprint(hash)
        );
        // A corrupt file regenerates (a new fingerprint, not an
        // error).
        std::fs::write(dir.join(HOST_KEY_FILE), b"not a key").unwrap();
        let third = load_or_generate_host_key(&dir).unwrap();
        assert_ne!(
            first.public_key().fingerprint(hash),
            third.public_key().fingerprint(hash)
        );
    }
}
