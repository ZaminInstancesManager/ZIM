//! Engine composition root (library half).
//!
//! `run()` wires the concrete adapters — SQLite persistence, the
//! in-memory session/API-key managers, the M5 API server — into the
//! engine process: it opens (and migrates) the database, bootstraps
//! the first root administrator, loads or generates the loopback
//! TLS certificate, and serves the HTTPS API until shutdown.
//!
//! The binary half (`main.rs`) parses arguments/environment and calls
//! [`run`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;

use zim_engine_api::{ApiConfig, AppState};
use zim_engine_application::ports::Persistence;
use zim_engine_auth::{ApiKeyManager, SessionManager};
use zim_engine_domain::User;

/// Process-wide engine options (CLI + environment).
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Directory holding `engine.db` and the TLS PEM files.
    pub data_dir: PathBuf,
    /// Loopback port to serve on (0 = ephemeral, printed at startup).
    pub port: u16,
    /// Bootstrap admin email (first run only; env
    /// `ZIM_BOOTSTRAP_ADMIN_EMAIL`, default `admin@zim.local`).
    pub bootstrap_admin_email: String,
    /// Bootstrap admin password (first run only; env
    /// `ZIM_BOOTSTRAP_ADMIN_PASSWORD`; generated and printed once
    /// when unset).
    pub bootstrap_admin_password: Option<String>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("zim_data"),
            port: 8443,
            bootstrap_admin_email: "admin@zim.local".to_owned(),
            bootstrap_admin_password: None,
        }
    }
}

impl EngineOptions {
    /// Fill defaults from the environment (`ZIM_DATA_DIR`,
    /// `ZIM_PORT`, `ZIM_BOOTSTRAP_ADMIN_EMAIL`,
    /// `ZIM_BOOTSTRAP_ADMIN_PASSWORD`).
    pub fn from_env(base: Self) -> Self {
        let mut opts = base;
        if let Ok(dir) = std::env::var("ZIM_DATA_DIR") {
            if !dir.is_empty() {
                opts.data_dir = PathBuf::from(dir);
            }
        }
        if let Ok(port) = std::env::var("ZIM_PORT") {
            if let Ok(port) = port.trim().parse::<u16>() {
                opts.port = port;
            }
        }
        if let Ok(email) = std::env::var("ZIM_BOOTSTRAP_ADMIN_EMAIL") {
            if !email.is_empty() {
                opts.bootstrap_admin_email = email;
            }
        }
        if let Ok(password) = std::env::var("ZIM_BOOTSTRAP_ADMIN_PASSWORD") {
            if !password.is_empty() {
                opts.bootstrap_admin_password = Some(password);
            }
        }
        opts
    }
}

/// The file names inside the data directory.
const DB_FILE: &str = "engine.db";
const TLS_CERT_FILE: &str = "loopback-cert.pem";
const TLS_KEY_FILE: &str = "loopback-key.pem";

/// A booted engine: the bound loopback address plus the handle to
/// stop serving.
pub struct RunningEngine {
    /// The address the HTTPS listener is bound to.
    pub local_addr: std::net::SocketAddr,
    server: zim_engine_api::tls::RunningServer,
}

impl RunningEngine {
    /// Stop accepting connections and drain in-flight requests.
    pub async fn shutdown(self) {
        self.server.shutdown().await;
    }
}

/// Boot the engine (persistence, bootstrap, TLS, listener) without
/// blocking on shutdown — composition for embedders and tests.
pub async fn startup(options: EngineOptions) -> Result<RunningEngine, String> {
    std::fs::create_dir_all(&options.data_dir)
        .map_err(|e| format!("cannot create data dir {}: {e}", options.data_dir.display()))?;

    // ── Persistence ───────────────────────────────────────────────
    let db_path = options.data_dir.join(DB_FILE);
    let persistence = zim_engine_persistence_sqlite::SqlitePersistence::open(&db_path)
        .map_err(|e| format!("cannot open {}: {e}", db_path.display()))?;
    persistence
        .migrate()
        .map_err(|e| format!("migrations failed: {e}"))?;
    let persistence: Arc<dyn Persistence> = Arc::new(persistence);

    // ── First-run bootstrap ───────────────────────────────────────
    bootstrap_admin(&persistence, &options)?;

    // ── Credential managers (in-memory by design; see auth crate) ─
    let sessions = Arc::new(SessionManager::new(Default::default()));
    let api_keys = Arc::new(ApiKeyManager::new());

    // ── API server ────────────────────────────────────────────────
    let state = Arc::new(AppState::new(
        Arc::clone(&persistence),
        Arc::clone(&sessions),
        Arc::clone(&api_keys),
        ApiConfig::default(),
    ));
    let router = zim_engine_api::router(Arc::clone(&state));

    let tls = load_or_generate_tls(&options)?;
    let server = zim_engine_api::tls::serve_https(
        router,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        options.port,
        &tls,
    )
    .await
    .map_err(|e| e.to_string())?;

    eprintln!(
        "zim_engine: serving https://127.0.0.1:{} (API prefix /api/v1, data dir {})",
        server.local_addr.port(),
        options.data_dir.display()
    );

    Ok(RunningEngine {
        local_addr: server.local_addr,
        server,
    })
}

/// Run the engine until shutdown (SIGTERM / Ctrl-C), then drain.
///
/// Returns the address that was served.
pub async fn run(options: EngineOptions) -> Result<std::net::SocketAddr, String> {
    let engine = startup(options).await?;
    let addr = engine.local_addr;
    wait_for_shutdown().await;
    engine.shutdown().await;
    Ok(addr)
}

/// Create the root administrator on an empty user table.
fn bootstrap_admin(
    persistence: &Arc<dyn Persistence>,
    options: &EngineOptions,
) -> Result<(), String> {
    let users = persistence
        .list_users()
        .map_err(|e| format!("cannot read users: {e}"))?;
    if !users.is_empty() {
        return Ok(());
    }

    let password = match &options.bootstrap_admin_password {
        Some(given) => given.clone(),
        None => generate_password(),
    };
    let policy = zim_engine_auth::PasswordPolicy::default();
    let hash = zim_engine_auth::password::hash_password(&policy, &password)
        .map_err(|e| format!("bootstrap password rejected: {e}"))?;

    let admin = User {
        id: zim_engine_domain::UserId::new(),
        email: options.bootstrap_admin_email.trim().to_lowercase(),
        username: "admin".to_owned(),
        external_id: None,
        password_hash: Some(hash),
        oauth_subject: None,
        is_admin: true,
        root_admin: true,
    };
    admin
        .validate()
        .map_err(|e| format!("bootstrap admin invalid: {e}"))?;
    persistence
        .put_user(&admin)
        .map_err(|e| format!("cannot persist bootstrap admin: {e}"))?;

    eprintln!(
        "zim_engine: first run — bootstrap root administrator created\n  \
         email:    {}\n  \
         username: admin\n  \
         password:  {}\n  \
         (This password is not shown again; set ZIM_BOOTSTRAP_ADMIN_PASSWORD to choose your own.)",
        admin.email, password
    );
    Ok(())
}

/// A generated bootstrap password: 24 chars of non-ambiguous
/// base58-ish alphabet.
fn generate_password() -> String {
    use rand_core::{OsRng, RngCore};
    const ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut bytes = [0u8; 24];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

/// Load the persisted loopback certificate, generating and persisting
/// it on first run.
fn load_or_generate_tls(
    options: &EngineOptions,
) -> Result<zim_engine_api::tls::TlsMaterial, String> {
    let cert_path = options.data_dir.join(TLS_CERT_FILE);
    let key_path = options.data_dir.join(TLS_KEY_FILE);
    if cert_path.exists() && key_path.exists() {
        let cert = std::fs::read_to_string(&cert_path)
            .map_err(|e| format!("cannot read {}: {e}", cert_path.display()))?;
        let key = std::fs::read_to_string(&key_path)
            .map_err(|e| format!("cannot read {}: {e}", key_path.display()))?;
        // A malformed pair (truncated write, manual edit) regenerates.
        if zim_engine_api::tls::tls_acceptor(&zim_engine_api::tls::TlsMaterial {
            cert_pem: cert.clone(),
            key_pem: key.clone(),
        })
        .is_ok()
        {
            return Ok(zim_engine_api::tls::TlsMaterial {
                cert_pem: cert,
                key_pem: key,
            });
        }
        eprintln!("zim_engine: loopback certificate unreadable; regenerating");
    }
    let material =
        zim_engine_api::tls::generate_loopback_certificate().map_err(|e| e.to_string())?;
    std::fs::write(&cert_path, &material.cert_pem)
        .map_err(|e| format!("cannot write {}: {e}", cert_path.display()))?;
    std::fs::write(&key_path, &material.key_pem)
        .map_err(|e| format!("cannot write {}: {e}", key_path.display()))?;
    Ok(material)
}

/// Block until SIGTERM (Unix) or Ctrl-C.
async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("installing SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    eprintln!("zim_engine: shutdown signal received; draining connections");
}

/// The canonical crate name (the binary uses it in banners).
pub const CRATE_NAME: &str = "zim_engine_main";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_main");
    }

    #[test]
    fn generated_passwords_have_the_right_shape() {
        let password = generate_password();
        assert_eq!(password.len(), 24);
        assert!(password.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn options_defaults_are_sane() {
        let options = EngineOptions::default();
        assert_eq!(options.port, 8443);
        assert_eq!(options.bootstrap_admin_email, "admin@zim.local");
        assert!(options.bootstrap_admin_password.is_none());
    }
}
