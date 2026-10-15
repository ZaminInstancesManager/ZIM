//! The per-workload Website handle (M19): the §4 capability
//! implementations over a live [`Runtime`].
//!
//! [`WebsiteWorkload`] fuses one workload's runtime handle, its
//! instance configuration, its file root, and its spec into the
//! typed surface the SDK's capability traits describe:
//!
//! - [`HealthCheckCapable`] — the spec's health check, run over
//!   the placed host binding (the SDK's shared probe executor);
//! - [`FilesystemCapable`] — the Wings file API over the
//!   instance's data directory (the site tree, the releases, the
//!   configuration — one root);
//! - [`NetworkCapable`] — the multi-endpoint surface (the
//!   http/https logical names over the placed bindings; live
//!   re-plumbing applies at the next start, the Docker
//!   constraint — the same posture the Database handle takes);
//! - [`MetricsCapable`] — the Docker-stats resource sample;
//! - the deploy family — [`install_async`][Self::install_async],
//!   [`deploy_async`][Self::deploy_async],
//!   [`rollback_async`][Self::rollback_async]: the release
//!   directory layout, the atomic current-symlink switch, and
//!   the bounded retention.
//!
//! # The sync↔async bridge
//!
//! Same shape as the Minecraft and Database handles (M17/M18):
//! the SDK's capability traits are synchronous; the engine's
//! [`Runtime`] is async. The handle carries the engine runtime's
//! [`tokio::runtime::Handle`] and the trait implementations
//! `block_on` their async twins through it. **Calling the sync
//! trait methods from inside an async context on the same
//! runtime deadlocks** — they are for the job-executor's worker
//! threads and `spawn_blocking` contexts. Async callers use the
//! inherent `*_async` methods.
//!
//! # The atomic switch
//!
//! The deploy path's switch is `symlink(new, tmp)` +
//! `rename(tmp, current)`: the rename replaces the link in one
//! step, so a request resolving `current` sees either the old
//! release or the new one — never a missing link. The release
//! directories themselves are engine-controlled names (UUIDs),
//! so user-supplied paths never reach the symlink dance; the
//! file writes inside a release go through the [`Filesystem`]
//! surface's validated paths.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use zim_engine_application::supervision::{
    InstanceConfig, ResourceUsage as RuntimeUsage, Runtime, RuntimeError,
};
use zim_engine_files::{FileError, Filesystem};
use zim_engine_workloads_sdk::errors::IntegrationError;
use zim_engine_workloads_sdk::traits::{
    ArchiveFormat as SdkFormat, ArchiveRef, Endpoint, FilesystemCapable, Health,
    HealthCheckCapable, MetricsCapable, NetworkCapable, PortSpec, ResourceUsage as SdkUsage,
};

use crate::deploy;
use crate::install;
use crate::lifecycle::DEFAULT_STOP_DEADLINE;
use crate::spec::WebsiteSpec;

/// How often the deploy's verify step re-probes health while
/// waiting for the listener to answer.
const VERIFY_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The first host port the handle's expose path allocates from
/// (the same posture the Database handle takes — above the
/// ephemeral range's usual users).
const FIRST_EXPOSED_HOST_PORT: u16 = 24600;

/// The outcome of one deploy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOutcome {
    /// The release id the deploy wrote and switched to.
    pub release_id: String,
    /// The release id the switch replaced (the rollback target).
    pub previous_release_id: Option<String>,
}

/// The per-workload Website capability handle.
#[derive(Clone)]
pub struct WebsiteWorkload {
    runtime: Arc<dyn Runtime>,
    cfg: Arc<RwLock<InstanceConfig>>,
    io: tokio::runtime::Handle,
    files: Filesystem,
    spec: WebsiteSpec,
    next_host_port: Arc<AtomicU16>,
    stop_deadline: Duration,
    probe_timeout: Duration,
    verify_deadline: Duration,
}

impl WebsiteWorkload {
    /// Build the handle for one placed workload.
    pub fn new(
        runtime: Arc<dyn Runtime>,
        cfg: Arc<InstanceConfig>,
        io: tokio::runtime::Handle,
        spec: WebsiteSpec,
    ) -> Self {
        let base = cfg
            .port_bindings
            .iter()
            .map(|binding| binding.host_port)
            .max()
            .map(|port| port.saturating_add(1))
            .filter(|port| *port >= FIRST_EXPOSED_HOST_PORT)
            .unwrap_or(FIRST_EXPOSED_HOST_PORT);
        Self {
            files: Filesystem::new(cfg.data_dir.clone()),
            spec,
            runtime,
            cfg: Arc::new(RwLock::new((*cfg).clone())),
            io,
            next_host_port: Arc::new(AtomicU16::new(base)),
            stop_deadline: DEFAULT_STOP_DEADLINE,
            probe_timeout: Duration::from_secs(10),
            verify_deadline: Duration::from_secs(30),
        }
    }

    /// Override the graceful-stop deadline.
    pub fn with_stop_deadline(mut self, deadline: Duration) -> Self {
        self.stop_deadline = deadline;
        self
    }

    /// Override the health probe's timeout.
    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    /// Override the deploy verify step's deadline (how long the
    /// listener may take to answer the health check after a
    /// switch).
    pub fn with_verify_deadline(mut self, deadline: Duration) -> Self {
        self.verify_deadline = deadline;
        self
    }

    /// The handle's spec.
    pub fn spec(&self) -> &WebsiteSpec {
        &self.spec
    }

    /// The handle's file API.
    pub fn files(&self) -> &Filesystem {
        &self.files
    }

    /// A snapshot of the carried instance configuration.
    pub fn current_config(&self) -> InstanceConfig {
        self.cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .clone()
    }

    // ── The async twins ─────────────────────────────────────────

    /// (Re)create and start the workload's container from the
    /// carried configuration.
    pub async fn start_async(&self) -> Result<(), IntegrationError> {
        let cfg = self
            .cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .clone();
        self.runtime.start(&cfg).await.map_err(runtime_err)
    }

    /// Stop the workload the web-server way: the runtime's
    /// graceful-then-forcible signal stop (see
    /// [`crate::lifecycle`]).
    pub async fn stop_async(&self) -> Result<(), IntegrationError> {
        self.runtime
            .stop(self.stop_deadline)
            .await
            .map_err(runtime_err)
    }

    /// One resource-usage sample.
    pub async fn metrics_async(&self) -> Result<SdkUsage, IntegrationError> {
        let usage = self.runtime.stats().await.map_err(runtime_err)?;
        Ok(to_sdk_usage(&usage))
    }

    /// The health check over the placed host binding.
    pub async fn health_async(&self) -> Result<Health, IntegrationError> {
        let check = self.spec.effective_health_check();
        let port = self.probe_host_port(&check).ok_or_else(|| {
            IntegrationError::Runtime(format!(
                "the health check's endpoint {:?} is not placed (no host binding carries it)",
                check
                    .port
                    .as_deref()
                    .unwrap_or(crate::server::HTTP_ENDPOINT)
            ))
        })?;
        let host = self.probe_host_ip();
        let bounded = HealthCheckSpecBounded {
            spec: &check,
            timeout: self.probe_timeout,
        };
        Ok(run_bounded_probe(&bounded, host, port).await)
    }

    /// The host port the health check's logical endpoint placed.
    fn probe_host_port(&self, check: &zim_engine_workloads_sdk::HealthCheckSpec) -> Option<u16> {
        let cfg = self
            .cfg
            .read()
            .expect("the instance-config lock is only held briefly");
        let endpoints = self.spec.effective_endpoints();
        // The named endpoint resolves to its container port; the
        // unnamed check targets the primary endpoint.
        let container_port = match check.port.as_deref() {
            Some(name) => {
                endpoints
                    .iter()
                    .find(|endpoint| endpoint.name == name)?
                    .container_port
            }
            None => endpoints.first()?.container_port,
        };
        cfg.port_bindings
            .iter()
            .find(|binding| {
                binding.container_port == container_port
                    && matches!(
                        binding.protocol,
                        zim_engine_application::supervision::PortProtocol::Tcp
                    )
            })
            .map(|binding| binding.host_port)
    }

    /// The host IP the probe connects through (the first
    /// binding's — the placement's own loopback posture).
    fn probe_host_ip(&self) -> IpAddr {
        self.cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .port_bindings
            .first()
            .map(|binding| binding.host_ip)
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    // ── The install family ──────────────────────────────────────

    /// Bootstrap the site's first run: the static carrier's
    /// configuration, the first release (the placeholder page),
    /// and the `current` symlink.
    ///
    /// Idempotent: a re-run rewrites the configuration and
    /// re-points `current` at a fresh first release (the same
    /// posture an install-after-delete takes).
    pub async fn install_async(&self) -> Result<(), IntegrationError> {
        // 1. The static carrier's configuration.
        if matches!(self.spec.server, crate::server::WebServer::Static) {
            self.files
                .write_file(
                    self.spec.server.config_relative_path(),
                    install::nginx_conf(&self.spec).as_bytes(),
                )
                .map_err(file_err)?;
        }
        // 2. The first release.
        let release_id = new_release_id();
        let index_path = format!(
            "{}/{}/{}/index.html",
            deploy::RELEASES_DIR,
            release_id,
            deploy::SITE_SUBDIR
        );
        self.files
            .write_file(&index_path, install::PLACEHOLDER_INDEX.as_bytes())
            .map_err(file_err)?;
        // 3. The switch.
        self.switch_current(&release_id)?;
        Ok(())
    }

    // ── The deploy family ───────────────────────────────────────

    /// Deploy one new release: write the files, switch
    /// `current`, verify the live endpoint, prune the retention.
    ///
    /// `files` are root-relative paths *inside the release*
    /// (e.g. `"site/index.html"`, `"site/assets/app.css"`,
    /// `"server.js"`); the write goes through the file API's
    /// validated paths.
    pub async fn deploy_async(
        &self,
        files: &[(String, Vec<u8>)],
    ) -> Result<DeployOutcome, IntegrationError> {
        if files.is_empty() {
            return Err(IntegrationError::Spec(
                zim_engine_workloads_sdk::errors::SpecError::Invalid {
                    field: "files".to_owned(),
                    message: "a deploy must carry at least one file".to_owned(),
                },
            ));
        }
        let previous = self.current_release()?.as_deref().map(str::to_owned);
        let release_id = new_release_id();
        // 1-2. Upload (alongside: the write is invisible — the
        // document root resolves `current` per request).
        for (path, content) in files {
            if path.trim().is_empty() {
                return Err(IntegrationError::Spec(
                    zim_engine_workloads_sdk::errors::SpecError::Invalid {
                        field: "files".to_owned(),
                        message: "a deploy file has an empty path".to_owned(),
                    },
                ));
            }
            let in_release = format!("{}/{}/{path}", deploy::RELEASES_DIR, release_id);
            self.files
                .write_file(&in_release, content)
                .map_err(file_err)?;
        }
        // 3. The switch.
        self.switch_current(&release_id)?;
        // 4. The drain step: the static carrier needs nothing
        // (in-flight requests hold their own handles); the
        // application servers relaunch to load the new code.
        let running = matches!(
            self.runtime.state().await.map_err(runtime_err)?,
            zim_engine_application::supervision::ObservedState::Running { .. }
        );
        if running && self.spec.server.release_entrypoint().is_some() {
            self.stop_async().await?;
            self.start_async().await?;
        }
        // 5. Verify: the live endpoint must answer healthy within
        // the deadline (a failed verdict fails the deploy, and
        // the rollback window stays open). A deploy to a stopped
        // site skips the verification — the switch is the deploy,
        // and the next start serves it.
        if running {
            let verify_started = tokio::time::Instant::now();
            loop {
                match self.health_async().await {
                    Ok(Health::Healthy) => break,
                    Ok(unhealthy @ Health::Unhealthy { .. }) => {
                        if tokio::time::Instant::now() - verify_started >= self.verify_deadline {
                            return Err(IntegrationError::Runtime(format!(
                                "the deployed release did not answer healthy within {:?}: {unhealthy:?}",
                                self.verify_deadline
                            )));
                        }
                    }
                    Ok(degraded @ Health::Degraded { .. }) => {
                        if tokio::time::Instant::now() - verify_started >= self.verify_deadline {
                            return Err(IntegrationError::Runtime(format!(
                                "the deployed release answered degraded within {:?}: {degraded:?}",
                                self.verify_deadline
                            )));
                        }
                    }
                    Err(err) => return Err(err),
                }
                tokio::time::sleep(VERIFY_POLL_INTERVAL).await;
            }
        }
        // 6. Retain.
        self.prune_retention()?;
        Ok(DeployOutcome {
            release_id,
            previous_release_id: previous,
        })
    }

    /// Roll back to the previous release (the symlink flip).
    ///
    /// Answers `Ok(None)` when there is no previous release to
    /// roll back to (a never-deployed install).
    pub async fn rollback_async(&self) -> Result<Option<String>, IntegrationError> {
        let releases = self.releases()?;
        // The list is oldest-first; the previous release is the
        // one before the current, whatever the current is.
        let current = self.current_release()?;
        let current_index = releases
            .iter()
            .position(|release| Some(release.as_str()) == current.as_deref())
            .unwrap_or(releases.len());
        if current_index == 0 {
            return Ok(None);
        }
        let previous = releases[current_index - 1].clone();
        self.switch_current(&previous)?;
        if self.spec.server.release_entrypoint().is_some() {
            let running = matches!(
                self.runtime.state().await.map_err(runtime_err)?,
                zim_engine_application::supervision::ObservedState::Running { .. }
            );
            if running {
                self.stop_async().await?;
                self.start_async().await?;
            }
        }
        Ok(Some(previous))
    }

    /// The release ids on disk, oldest-first (the creation-order
    /// the retention prunes from).
    pub fn releases(&self) -> Result<Vec<String>, IntegrationError> {
        let entries = self
            .files
            .list_dir(deploy::RELEASES_DIR)
            .map_err(|err| match err {
                FileError::NotFound(_) => IntegrationError::Runtime(
                    "the releases directory is absent (the workload is not installed)".to_owned(),
                ),
                other => file_err(other),
            })?;
        let mut releases: Vec<String> = entries
            .into_iter()
            .filter(|entry| entry.is_dir)
            .map(|entry| entry.name)
            .collect();
        releases.sort();
        Ok(releases)
    }

    /// The release id `current` points at (`None` when the
    /// workload is not installed).
    pub fn current_release(&self) -> Result<Option<String>, IntegrationError> {
        let current = self.data_root().join(deploy::CURRENT_LINK);
        match std::fs::read_link(&current) {
            Ok(target) => {
                let id = target
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        IntegrationError::Runtime(format!(
                            "the current link points outside the releases directory: {target:?}"
                        ))
                    })?
                    .to_owned();
                Ok(Some(id))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(IntegrationError::Runtime(format!(
                "the current link could not be read: {err}"
            ))),
        }
    }

    /// Point `current` at one release, atomically.
    ///
    /// The link's target is *relative* — `releases/<id>` from the
    /// link's own directory — so it resolves identically host-side
    /// (where the deploy family writes) and container-side (where
    /// the server's document root follows it through the bind
    /// mount). An absolute host path would dangle inside the
    /// container's mount namespace.
    ///
    /// The engine-controlled names (the release id, the link
    /// name) are the only inputs to the symlink dance; user
    /// paths never reach it.
    fn switch_current(&self, release_id: &str) -> Result<(), IntegrationError> {
        let root = self.data_root();
        let releases = root.join(deploy::RELEASES_DIR);
        std::fs::create_dir_all(&releases).map_err(io_err)?;
        let target = releases.join(release_id);
        if !target.is_dir() {
            return Err(IntegrationError::Runtime(format!(
                "the release {release_id:?} does not exist under the releases directory"
            )));
        }
        let relative_target = format!("{}/{}", deploy::RELEASES_DIR, release_id);
        atomic_symlink_switch(&root, deploy::CURRENT_LINK, &relative_target)
    }

    /// Delete the oldest releases beyond the retention count.
    fn prune_retention(&self) -> Result<(), IntegrationError> {
        let retain = self.spec.retained_releases();
        let releases = self.releases()?;
        if releases.len() <= retain {
            return Ok(());
        }
        for release in &releases[..releases.len() - retain] {
            self.files
                .delete(&format!("{}/{}", deploy::RELEASES_DIR, release))
                .map_err(file_err)?;
        }
        Ok(())
    }

    /// The data directory's host-side root.
    fn data_root(&self) -> PathBuf {
        self.current_config().data_dir
    }
}

impl HealthCheckCapable for WebsiteWorkload {
    fn health(&self) -> Result<Health, IntegrationError> {
        // Sync bridge: see the module docs.
        self.io.block_on(self.health_async())
    }
}

impl FilesystemCapable for WebsiteWorkload {
    fn list_dir(&self, path: &str) -> Result<Vec<String>, IntegrationError> {
        self.files
            .list_dir(path)
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| {
                        if entry.is_dir {
                            format!("{}/", entry.name)
                        } else {
                            entry.name
                        }
                    })
                    .collect()
            })
            .map_err(file_err)
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, IntegrationError> {
        self.files.read_file(path).map_err(file_err)
    }

    fn write_file(&self, path: &str, content: &[u8]) -> Result<(), IntegrationError> {
        self.files.write_file(path, content).map_err(file_err)
    }

    fn delete(&self, path: &str) -> Result<(), IntegrationError> {
        self.files.delete(path).map_err(file_err)
    }

    fn archive(
        &self,
        dest: &str,
        paths: &[&str],
        format: SdkFormat,
    ) -> Result<ArchiveRef, IntegrationError> {
        let bytes = self
            .files
            .archive(dest, paths, to_files_format(format))
            .map_err(file_err)?;
        Ok(ArchiveRef {
            path: dest.to_owned(),
            bytes,
        })
    }

    fn extract(&self, archive_path: &str, dest: &str) -> Result<(), IntegrationError> {
        self.files
            .extract(archive_path, dest)
            .map(|_| ())
            .map_err(file_err)
    }
}

impl NetworkCapable for WebsiteWorkload {
    fn endpoints(&self) -> Result<Vec<Endpoint>, IntegrationError> {
        let cfg = self
            .cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .clone();
        let placed: Vec<Endpoint> = self
            .spec
            .effective_endpoints()
            .into_iter()
            .filter_map(|endpoint| {
                let binding = cfg.port_bindings.iter().find(|binding| {
                    binding.container_port == endpoint.container_port
                        && matches!(
                            binding.protocol,
                            zim_engine_application::supervision::PortProtocol::Tcp
                        )
                })?;
                Some(Endpoint {
                    name: endpoint.name,
                    port: binding.host_port,
                })
            })
            .collect();
        let mut all = placed;
        for binding in &cfg.port_bindings {
            if matches!(
                binding.protocol,
                zim_engine_application::supervision::PortProtocol::Udp
            ) {
                continue;
            }
            let known = all
                .iter()
                .any(|endpoint| endpoint.port == binding.host_port);
            if !known {
                all.push(Endpoint {
                    name: format!("port-{}", binding.container_port),
                    port: binding.host_port,
                });
            }
        }
        Ok(all)
    }

    fn expose_port(&self, spec: PortSpec) -> Result<Endpoint, IntegrationError> {
        let protocol = port_protocol(spec.protocol);
        let mut cfg = self
            .cfg
            .write()
            .expect("the instance-config lock is only held briefly");
        if let Some(existing) = cfg.port_bindings.iter().find(|binding| {
            binding.container_port == spec.container_port
                && std::mem::discriminant(&binding.protocol) == std::mem::discriminant(&protocol)
        }) {
            return Ok(Endpoint {
                name: spec.name,
                port: existing.host_port,
            });
        }
        let host_port = self.next_host_port.fetch_add(1, Ordering::Relaxed);
        let host_ip = cfg
            .port_bindings
            .first()
            .map(|binding| binding.host_ip)
            .unwrap_or_else(|| "127.0.0.1".parse().expect("a valid loopback IP"));
        cfg.port_bindings
            .push(zim_engine_application::supervision::PortBinding {
                host_ip,
                host_port,
                container_port: spec.container_port,
                protocol,
            });
        Ok(Endpoint {
            name: spec.name,
            port: host_port,
        })
    }

    fn unexpose_port(&self, endpoint: &Endpoint) -> Result<(), IntegrationError> {
        let mut cfg = self
            .cfg
            .write()
            .expect("the instance-config lock is only held briefly");
        let before = cfg.port_bindings.len();
        cfg.port_bindings
            .retain(|binding| binding.host_port != endpoint.port);
        if cfg.port_bindings.len() == before {
            return Err(IntegrationError::Runtime(format!(
                "the endpoint {:?} (port {}) is not placed",
                endpoint.name, endpoint.port
            )));
        }
        Ok(())
    }
}

impl MetricsCapable for WebsiteWorkload {
    fn metrics(&self) -> Result<SdkUsage, IntegrationError> {
        // Sync bridge: see the module docs.
        self.io.block_on(self.metrics_async())
    }
}

/// One health check with the handle's own timeout bound applied.
struct HealthCheckSpecBounded<'a> {
    spec: &'a zim_engine_workloads_sdk::HealthCheckSpec,
    timeout: Duration,
}

impl<'a> HealthCheckSpecBounded<'a> {
    /// The check's effective timeout (the handle's bound, capped
    /// by the spec's own).
    fn effective(&self) -> zim_engine_workloads_sdk::HealthCheckSpec {
        let mut spec = self.spec.clone();
        spec.timeout_ms = self.timeout.min(spec.timeout()).as_millis() as u64;
        spec
    }
}

/// Run one bounded health probe (the handle's async twin's
/// helper).
async fn run_bounded_probe(
    bounded: &HealthCheckSpecBounded<'_>,
    host: IpAddr,
    port: u16,
) -> Health {
    zim_engine_workloads_sdk::run_health_probe(&bounded.effective(), host, port).await
}

/// A new release id: a zero-padded creation timestamp plus a
/// UUID — the engine-controlled name the symlink dance uses,
/// lexically ordered by creation (the retention and rollback
/// families' "previous release" is the id order, so the order
/// must be the creation order, not a random one).
fn new_release_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or_default();
    format!("{millis:016x}-{}", uuid::Uuid::new_v4().simple())
}

/// The atomic symlink switch: point `link_name` at
/// `link_target` (a path *relative to the link's own directory*)
/// through a temp link + `rename(2)` — the rename replaces the
/// link in one step, so a reader sees either the old target or
/// the new one, never a missing link.
///
/// Unix-only in M19: the parity suites and the engine's own
/// deployments run the container workloads on Linux hosts, and
/// the Windows fallback (a `symlink_file` dance with its own
/// privilege posture) arrives with the engine's cross-platform
/// packaging milestone (M14).
#[cfg(unix)]
fn atomic_symlink_switch(
    root: &std::path::Path,
    link_name: &str,
    link_target: &str,
) -> Result<(), IntegrationError> {
    let link = root.join(link_name);
    let temp = root.join(format!(".{link_name}.switching"));
    let _ = std::fs::remove_file(&temp);
    std::os::unix::fs::symlink(link_target, &temp).map_err(io_err)?;
    std::fs::rename(&temp, &link).map_err(io_err)?;
    Ok(())
}

/// The non-unix answer: the atomic switch's own posture is
/// documented (M19 is the Linux-hosted parity surface; the
/// Windows symlink family arrives with M14's packaging).
#[cfg(not(unix))]
fn atomic_symlink_switch(
    _root: &std::path::Path,
    _link_name: &str,
    _link_target: &str,
) -> Result<(), IntegrationError> {
    Err(IntegrationError::Runtime(
        "the release-switch symlink family is unix-only in M19 (the \
         cross-platform switch arrives with the packaging milestone)"
            .to_owned(),
    ))
}

/// Map a runtime error into the integration vocabulary.
fn runtime_err(error: RuntimeError) -> IntegrationError {
    match error {
        RuntimeError::NotRunning(reason) => IntegrationError::NotRunning {
            operation: "runtime call",
            reason,
        },
        other => IntegrationError::Runtime(other.to_string()),
    }
}

/// Map a file-API error into the integration vocabulary.
fn file_err(error: FileError) -> IntegrationError {
    IntegrationError::File(error.to_string())
}

/// Map an IO error into the integration vocabulary.
fn io_err(error: std::io::Error) -> IntegrationError {
    IntegrationError::Runtime(format!("the deploy path's host-side IO failed: {error}"))
}

/// Map the SDK's archive format onto the file API's.
fn to_files_format(format: SdkFormat) -> zim_engine_files::ArchiveFormat {
    match format {
        SdkFormat::Tar => zim_engine_files::ArchiveFormat::Tar,
        SdkFormat::TarGz => zim_engine_files::ArchiveFormat::TarGz,
        SdkFormat::Zip => zim_engine_files::ArchiveFormat::Zip,
    }
}

/// Resolve the SDK's protocol vocabulary onto the runtime's.
fn port_protocol(protocol: &str) -> zim_engine_application::supervision::PortProtocol {
    match protocol {
        "udp" => zim_engine_application::supervision::PortProtocol::Udp,
        _ => zim_engine_application::supervision::PortProtocol::Tcp,
    }
}

/// Map the runtime's resource sample onto the SDK's §4 shape.
fn to_sdk_usage(usage: &RuntimeUsage) -> SdkUsage {
    let cpu = match (
        usage.cpu_usage_total_ns,
        usage.system_cpu_total_ns,
        usage.online_cpus,
    ) {
        (Some(process), Some(system), Some(cpus)) if system > 0 && cpus > 0 => {
            let fraction = process as f64 / system as f64;
            let cores = cpus as f64;
            (fraction * cores).clamp(0.0, cores) as f32
        }
        _ => 0.0,
    };
    let network = usage
        .network_rx_bytes
        .unwrap_or(0)
        .saturating_add(usage.network_tx_bytes.unwrap_or(0));
    SdkUsage {
        cpu,
        memory_bytes: usage.memory_usage_bytes.unwrap_or(0),
        network_bytes: network,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_probes_cap_the_spec_timeout() {
        let spec = zim_engine_workloads_sdk::HealthCheckSpec {
            kind: zim_engine_workloads_sdk::HealthCheckKind::Tcp,
            port: None,
            timeout_ms: 60_000,
        };
        let bounded = HealthCheckSpecBounded {
            spec: &spec,
            timeout: Duration::from_secs(2),
        };
        assert_eq!(bounded.effective().timeout(), Duration::from_secs(2));
        let bounded = HealthCheckSpecBounded {
            spec: &spec,
            timeout: Duration::from_secs(120),
        };
        assert_eq!(bounded.effective().timeout(), Duration::from_secs(60));
    }

    #[test]
    fn release_ids_are_engine_controlled_and_creation_ordered() {
        let first = new_release_id();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = new_release_id();
        // The id is the timestamp prefix plus the uuid suffix.
        assert_eq!(first.len(), 16 + 1 + 32);
        assert!(first.chars().take(16).all(|c| c.is_ascii_hexdigit()));
        // Creation order is lexical order (the rollback family's
        // "previous" depends on it).
        assert!(first < second, "{first} should sort before {second}");
    }

    #[test]
    fn the_current_link_is_relative_so_it_resolves_on_both_sides() {
        // The link must resolve host-side AND container-side: a
        // relative target (releases/<id>) does — an absolute host
        // path would dangle inside the container's mount
        // namespace.
        let root =
            std::env::temp_dir().join(format!("zim-web-link-{}", uuid::Uuid::new_v4().simple()));
        let releases = root.join(deploy::RELEASES_DIR);
        std::fs::create_dir_all(releases.join("abc")).expect("create the release");
        atomic_symlink_switch(&root, deploy::CURRENT_LINK, "releases/abc").expect("switch");
        let link = root.join(deploy::CURRENT_LINK);
        let target = std::fs::read_link(&link).expect("the link reads");
        assert_eq!(target, std::path::Path::new("releases/abc"));
        // And it resolves: the directory exists through the link.
        assert!(link.join("anything").parent().is_some());
        let resolved = std::fs::canonicalize(&link).expect("the link resolves");
        assert!(resolved.ends_with("releases/abc"));
        std::fs::remove_dir_all(&root).ok();
    }
}
