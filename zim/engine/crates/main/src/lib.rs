//! Engine composition root (library half).
//!
//! `run()` wires the concrete adapters — SQLite persistence, the
//! in-memory session/API-key managers, the M5 API server, the M6
//! supervision module over the Docker runtime, and the M11 frontend
//! loader — into the engine process: it opens (and migrates) the
//! database, bootstraps the first root administrator and the default
//! local node, connects the container runtime (falling back to the
//! null runtime in degraded API-only mode when no Docker is
//! reachable), reconciles persisted vs. observed instance state
//! (lifecycle invariants 1 and 10), loads the active frontend package
//! (reverting to the bundled default when the recorded one fails its
//! re-verification — invariant 16), loads or generates the loopback
//! TLS certificate, and serves the HTTPS API until shutdown.
//!
//! The binary half (`main.rs`) parses arguments/environment and calls
//! [`run`] — except the offline `migrate` subcommand (M10), which
//! dispatches straight to `zim_engine_migrate_pterodactyl::cli`
//! before any engine wiring exists.
//!
//! Shutdown (M27) arrives three ways — SIGTERM, Ctrl-C, or the
//! local control plane (`POST /api/v1/engine/control`, the desktop
//! shell's coordinated stop) — and all three drain identically.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod discovery;

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;

use zim_engine_api::control::{ControlPlane, ControlToken};
use zim_engine_api::{ApiConfig, AppState};
use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::{
    NullRuntimeFactory, RuntimeFactory, SupervisionPolicy, Supervisor,
};
use zim_engine_auth::{ApiKeyManager, SessionManager, TabTokenManager};
use zim_engine_backups::BackupService;
use zim_engine_domain::{Node, NodeCapabilities, NodeDeployment, NodeState, User};
use zim_engine_installs::InstallService;
use zim_engine_jobs::{JobPolicy, JobSystem};

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

/// The API generation string the frontend loader gates packages
/// against (kept in lockstep with `ApiConfig::default().api_version`).
fn state_api_version() -> String {
    ApiConfig::default().api_version.to_owned()
}

/// Record the §3.6 recovery event in the audit log (actor `system`).
fn record_recovery_audit(
    persistence: &Arc<dyn Persistence>,
    notice: &zim_engine_frontend_loader::RecoveryNotice,
) {
    let record = zim_engine_domain::AuditRecord {
        id: 0,
        timestamp: notice.at,
        actor: zim_engine_domain::AuditActor::System,
        actor_id: None,
        action: "frontend.recovery".to_owned(),
        instance_id: None,
        metadata: serde_json::json!({
            "reason": notice.reason,
            "from": notice.from,
        }),
        ip_address: None,
        user_agent: None,
    };
    if let Err(e) = persistence.append_audit(&record) {
        eprintln!("zim_engine: frontend-recovery audit append failed: {e}");
    }
}

/// Append an addon boot notice to the audit trail (best-effort, the
/// same discipline as the frontend recovery record: a lost audit row
/// must not fail the boot).
fn record_addon_boot_notice(persistence: &Arc<dyn Persistence>, notice: &str) {
    let record = zim_engine_domain::AuditRecord {
        id: 0,
        timestamp: chrono::Utc::now(),
        actor: zim_engine_domain::AuditActor::System,
        actor_id: None,
        action: "extension.boot_notice".to_owned(),
        instance_id: None,
        metadata: serde_json::json!({ "notice": notice }),
        ip_address: None,
        user_agent: None,
    };
    if let Err(e) = persistence.append_audit(&record) {
        eprintln!("zim_engine: addon boot-notice audit append failed: {e}");
    }
    eprintln!("zim_engine: addon boot notice — {notice}");
}

/// A booted engine: the bound loopback address plus the handles to
/// stop serving, supervision, and the job executor.
pub struct RunningEngine {
    /// The address the HTTPS listener is bound to.
    pub local_addr: std::net::SocketAddr,
    /// The engine's last reconciliation pass (observability).
    pub reconciled: zim_engine_application::supervision::ReconcileReport,
    /// The data dir (to remove the shell-discovery file on
    /// shutdown).
    data_dir: PathBuf,
    server: zim_engine_api::tls::RunningServer,
    supervisor: Arc<Supervisor>,
    jobs: Arc<JobSystem>,
    /// The control plane's shutdown side: handed to the API state at
    /// startup; the run loop selects on it alongside the OS signals
    /// so a coordinated stop and a service stop drain identically.
    control: ControlPlane,
}

impl RunningEngine {
    /// Stop accepting connections, drain in-flight requests, stop
    /// the supervision workers, wait (up to the job policy's
    /// grace) for running jobs (running containers are left running
    /// by design — the next boot's reconciliation re-adopts them),
    /// and remove the shell-discovery file.
    pub async fn shutdown(self) {
        self.jobs.shutdown().await;
        self.supervisor.shutdown().await;
        self.server.shutdown().await;
        discovery::remove(&self.data_dir);
        zim_engine_api::control::remove_token(&self.data_dir);
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
    bootstrap_local_node(&persistence)?;

    // ── Container runtime (M6) ────────────────────────────────────
    // Probe Docker; a missing/unreachable daemon degrades the engine
    // to API-only mode (metadata management works; lifecycle calls
    // answer 503 runtime_unavailable) rather than refusing to boot.
    // The install runner follows the same probe: the Docker adapter
    // (M8.3) when a daemon answered, the null runner in the degraded
    // engine — installs then fail fast with a structured error
    // instead of queueing work no runtime can execute.
    let docker_settings = zim_engine_runtime_docker::DockerSettings {
        data_root: options.data_dir.clone(),
        ..zim_engine_runtime_docker::DockerSettings::default()
    };
    use zim_engine_application::supervision::InstallRunner;
    let (runtime_factory, install_runner): (Arc<dyn RuntimeFactory>, Arc<dyn InstallRunner>) =
        match zim_engine_runtime_docker::DockerRuntimeFactory::connect(docker_settings).await {
            Ok(factory) => {
                eprintln!("zim_engine: container runtime connected (Docker, pterodactyl0)");
                let runner = Arc::new(
                    zim_engine_runtime_docker::DockerInstallRunner::from_factory(&factory),
                );
                (Arc::new(factory), runner)
            }
            Err(e) => {
                eprintln!(
                    "zim_engine: no container runtime available ({e}); \
                 lifecycle operations will answer 503 until one is"
                );
                (
                    Arc::new(NullRuntimeFactory),
                    Arc::new(zim_engine_application::supervision::NullInstallRunner),
                )
            }
        };

    // ── Supervision + restart reconciliation (invariants 1, 10) ──
    let supervisor = Supervisor::new(
        Arc::clone(&persistence),
        runtime_factory,
        SupervisionPolicy::default(),
        options.data_dir.clone(),
    );
    let reconciled = supervisor
        .boot()
        .await
        .map_err(|e| format!("instance supervision boot failed: {e}"))?;
    eprintln!(
        "zim_engine: supervision reconciled {} instance(s): {} adopted running, {} restarts queued, {} crash-looped, {} unknown",
        reconciled.supervised,
        reconciled.observed_running,
        reconciled.queued_restarts,
        reconciled.crash_loops,
        reconciled.unknown
    );

    // ── Durable jobs + backups (M8) ───────────────────────────────
    // The executor claims Pending jobs, runs the registered
    // handlers (backups today; transfers and installs in later M8
    // sub-milestones), and cleans up partial state after crashes.
    // Orphan reconciliation runs BEFORE start() so a dead process's
    // InProgress rows are already cleanup-pending when the queue
    // wakes up.
    let jobs = JobSystem::new(
        Arc::clone(&persistence),
        Some(supervisor.stream().clone()),
        JobPolicy::default(),
    );
    let orphaned = jobs
        .reconcile_orphans()
        .await
        .map_err(|e| format!("job reconciliation failed: {e}"))?;
    if orphaned > 0 {
        eprintln!(
            "zim_engine: reconciled {orphaned} orphaned job(s) from a previous run; \
             partial state will be cleaned up"
        );
    }
    let backups = BackupService::new(
        Arc::clone(&persistence),
        Arc::clone(&jobs),
        options.data_dir.clone(),
    );
    backups.register().await;
    // M8.2: the transfer service — the §10.9 transfer mutation
    // family over the same executor. Both halves of a transfer's
    // working tree live under the engine's data root in this
    // milestone (the node-runtime contract later replaces the
    // local ship step).
    let transfers = zim_engine_transfers::TransferService::new(
        Arc::clone(&persistence),
        Arc::clone(&jobs),
        Arc::clone(&supervisor),
        options.data_dir.clone(),
    );
    transfers.register().await;
    // M8.3: the install service — the §10.3 install mutation over
    // the same executor. The runner is the Docker adapter when a
    // daemon answered at boot; the null runner in the degraded
    // API-only engine (installs then fail fast with a structured
    // error instead of queueing work no runtime can execute).
    let installs = InstallService::new(
        Arc::clone(&persistence),
        Arc::clone(&jobs),
        Arc::clone(&supervisor),
        install_runner,
        options.data_dir.clone(),
    );
    installs.register().await;
    jobs.start();

    // ── Cron scheduler (M9) ─────────────────────────────────────
    // The per-instance schedule rows become executable here: the
    // driver loop fires due schedules through the supervision
    // module's power queue and console input.
    let scheduler =
        zim_engine_scheduler::Scheduler::new(Arc::clone(&persistence), Arc::clone(&supervisor));
    scheduler.start();

    // ── Credential managers (in-memory by design; see auth crate) ─
    let sessions = Arc::new(SessionManager::new(Default::default()));
    let api_keys = Arc::new(ApiKeyManager::new());
    // Per-tab capability tokens for the WebSocket stream (M7).
    let tab_tokens = Arc::new(TabTokenManager::new(Default::default()));

    // ── Frontend loader (M11) ────────────────────────────────
    // Loads the recorded active package — the bundled default on a
    // fresh data dir — running the full §3.5 gate on a custom one
    // and reverting to the default (with a recovery notice, §3.6)
    // when it fails. Page serving and the §10.14 endpoints hang off
    // this through AppState.
    let frontend = Arc::new(zim_engine_frontend_loader::FrontendService::new(
        options.data_dir.clone(),
        semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .expect("the workspace version is valid semver"),
        state_api_version(),
    ));
    let frontend_info = frontend.active_info();
    if let Some(notice) = frontend.take_recovery() {
        record_recovery_audit(&persistence, &notice);
        eprintln!(
            "zim_engine: frontend recovery — reverted to the default frontend package \
             ({}); see the audit log",
            notice.reason
        );
    }
    eprintln!(
        "zim_engine: frontend active — {} {} ({}), {} surface(s)",
        frontend_info.id,
        frontend_info.version,
        if frontend_info.is_default {
            "bundled"
        } else {
            "data-dir"
        },
        frontend_info.page_surfaces.len(),
    );

    // ── Addon system (M24) ───────────────────────────────────
    // Boots the installed-addon registry against disk (§4.10: a
    // corrupt package disables that addon with a notice, never the
    // engine), seeds the bundled defaults on a fresh data dir (§6),
    // and owns the `/zim/` addon fallback routing + the §10.12
    // endpoints through AppState. The theme registry (M23's deferred
    // upload surface) boots alongside it.
    let extensions = Arc::new(zim_engine_extensions::ExtensionsService::new(
        Arc::clone(&persistence),
        Arc::clone(&frontend),
        &options.data_dir,
        semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .expect("the workspace version is valid semver"),
        &state_api_version(),
    ));
    let addon_summaries = extensions.list();
    for notice in extensions.boot_notices() {
        record_addon_boot_notice(&persistence, &notice);
    }
    eprintln!(
        "zim_engine: addons — {} installed ({} bundled), {} notice(s)",
        addon_summaries.len(),
        addon_summaries.iter().filter(|a| a.bundled).count(),
        extensions.boot_notices().len(),
    );
    let themes = Arc::new(zim_engine_extensions::ThemeService::new(&options.data_dir));

    // ── API server ──────────────────────────────────────────────────
    // The local control plane (M27): a fresh per-boot token in
    // engine.token (0600) + the shutdown handle the
    // POST /engine/control endpoint fires. A token write failure
    // degrades exactly like the discovery file's — the endpoint
    // refuses every presented token; boot is never blocked.
    let control = ControlPlane::new(ControlToken::generate());
    if let Err(e) = zim_engine_api::control::write_token(&options.data_dir, &control) {
        eprintln!("zim_engine: cannot write the engine-control token: {e}");
    }
    let state = Arc::new(AppState::new(
        Arc::clone(&persistence),
        Arc::clone(&supervisor),
        Arc::clone(&sessions),
        Arc::clone(&api_keys),
        Arc::clone(&tab_tokens),
        Arc::clone(&jobs),
        Arc::clone(&backups),
        Arc::clone(&transfers),
        Arc::clone(&installs),
        Arc::clone(&scheduler),
        Arc::clone(&frontend),
        Arc::clone(&extensions),
        Arc::clone(&themes),
        control.clone(),
        ApiConfig {
            // M28: the file family operates under the engine's data
            // root ({data_dir}/instances/{uuid_short}), the same
            // directory the runtime bind-mounts and the backup
            // pipeline archives.
            data_dir: options.data_dir.clone(),
            ..ApiConfig::default()
        },
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
        "zim_engine: serving https://127.0.0.1:{} (API prefix /api/v1, frontend at /zim/, \
         data dir {})",
        server.local_addr.port(),
        options.data_dir.display()
    );

    // ── Shell discovery (M21) ────────────────────────────────────
    // The desktop shell reads this to find the engine's port and
    // PID without guessing. A write failure degrades (the shell
    // falls back to probing the default port), never blocks boot.
    if let Err(e) = discovery::write(
        &options.data_dir,
        &discovery::DiscoveryInfo::current(server.local_addr.port()),
    ) {
        eprintln!("zim_engine: cannot write the discovery file: {e}");
    }

    Ok(RunningEngine {
        local_addr: server.local_addr,
        reconciled,
        data_dir: options.data_dir,
        server,
        supervisor,
        jobs,
        control,
    })
}

/// Create the default local node on a node-less database.
///
/// Single-machine ZIM installs place every instance on the engine's
/// own node; requiring a manual node-creation step before the first
/// instance would be ceremony without function. Remote-node rows
/// (added through the API later) coexist: only a missing *local*
/// node triggers the bootstrap.
fn bootstrap_local_node(persistence: &Arc<dyn Persistence>) -> Result<(), String> {
    let nodes = persistence
        .list_nodes()
        .map_err(|e| format!("cannot read nodes: {e}"))?;
    if nodes
        .iter()
        .any(|node| node.deployment == NodeDeployment::LocalLinux)
    {
        return Ok(());
    }

    let node = Node {
        id: zim_engine_domain::NodeId::new(),
        name: "local".to_owned(),
        location_id: zim_engine_domain::LocationId::new(),
        fqdn: "localhost".to_owned(),
        scheme: zim_engine_domain::Scheme::Https,
        public_port: 8443,
        listen_port: 8444,
        capabilities: NodeCapabilities::default(),
        deployment: NodeDeployment::LocalLinux,
        last_heartbeat: None,
        last_seen_state: NodeState::Unknown,
    };
    node.validate()
        .map_err(|e| format!("bootstrap local node invalid: {e}"))?;
    persistence
        .put_node(&node)
        .map_err(|e| format!("cannot persist bootstrap local node: {e}"))?;
    eprintln!(
        "zim_engine: first run — default local node created (id {}); \
         instances created without an explicit node_id land here",
        node.id
    );
    Ok(())
}

/// Run the engine until shutdown, then drain.
///
/// Shutdown arrives three ways — SIGTERM, Ctrl-C, or the local
/// control plane's `POST /engine/control` (M27: the desktop
/// shell's coordinated stop for the Windows update path) — and all
/// three drain through the identical path.
///
/// Returns the address that was served.
pub async fn run(options: EngineOptions) -> Result<std::net::SocketAddr, String> {
    let engine = startup(options).await?;
    let addr = engine.local_addr;
    let control = engine.control.shutdown_handle();
    tokio::select! {
        _ = wait_for_shutdown() => {},
        _ = control.notified() => {
            eprintln!("zim_engine: engine-control shutdown requested; draining connections");
        },
    }
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

/// The five first-party workload integrations, linked into the
/// engine build by the `workloads-all` feature (M16, AD-15's
/// feature-gated inclusion path).
///
/// The engine's kind registry consumes them in M17+; until then
/// this module proves the feature actually links all five (an
/// optional dependency nothing references would not).
#[cfg(feature = "workloads-all")]
pub mod linked_integrations {
    use zim_engine_workloads_sdk::WorkloadIntegration;

    /// The linked integrations' manifest ids.
    #[allow(dead_code)]
    pub fn linked_ids() -> [&'static str; 5] {
        [
            zim_engine_workloads_minecraft::MinecraftIntegration
                .manifest()
                .id
                .as_str(),
            zim_engine_workloads_database::DatabaseIntegration
                .manifest()
                .id
                .as_str(),
            zim_engine_workloads_website::WebsiteIntegration
                .manifest()
                .id
                .as_str(),
            zim_engine_workloads_dev_process::DevProcessIntegration
                .manifest()
                .id
                .as_str(),
            zim_engine_workloads_generic_container::GenericContainerIntegration
                .manifest()
                .id
                .as_str(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_main");
    }

    #[cfg(feature = "workloads-all")]
    #[test]
    fn the_five_integrations_link_under_the_feature() {
        let ids = linked_integrations::linked_ids();
        assert_eq!(ids[0], "org.zaminmc.workload.minecraft");
        assert_eq!(ids[1], "org.zaminmc.workload.database");
        assert_eq!(ids[2], "org.zaminmc.workload.website");
        assert_eq!(ids[3], "org.zaminmc.workload.dev_process");
        assert_eq!(ids[4], "org.zaminmc.workload.generic_container");
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
