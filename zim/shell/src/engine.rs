//! Engine attach: discovery, health probe, and detached spawn.
//!
//! The engine is an **independent OS process** — the architecture's
//! first desktop invariant (BROWSER_INTEGRATION.md §2): closing,
//! crashing, or quitting the shell never stops it, and any shell
//! instance may attach to the one already running. This module is
//! the whole of the shell's engine lifecycle surface:
//!
//! 1. **Discover** — read `engine.json` from the engine's data dir
//!    (written at boot, removed on graceful shutdown; a stale file
//!    from a crash is just a hint).
//! 2. **Probe** — `GET /api/v1/health` over loopback TLS with a
//!    bounded verifier (below). Healthy → attach.
//! 3. **Spawn** — no engine answering → start `zim_engine` as a
//!    *detached* process (new process group / detached console),
//!    its stdout/stderr appended to `engine.log` in the data dir,
//!    then poll for health.
//!
//! The shell never holds a handle it could drop-kill: the spawned
//! child's `Child` is dropped immediately, and std never kills a
//! dropped child.

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::state::EngineStatus;

/// The discovery-file mirror the shell understands (the engine owns
/// the writer; see `zim/engine/crates/main/src/discovery.rs`).
///
/// A schema the shell does not understand reads as absent — the
/// fallbacks (default-port probe, spawn) apply.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DiscoveryInfo {
    /// Discovery schema version (the shell understands `1`).
    pub schema: u8,
    /// The engine process id.
    pub pid: u32,
    /// The bound loopback port.
    pub port: u16,
    /// The engine version string.
    pub engine_version: String,
    /// Boot time, seconds since the Unix epoch.
    pub started_at_unix: u64,
}

/// The schema this shell reads.
const DISCOVERY_SCHEMA: u8 = 1;

/// The `/api/v1/health` payload (§misc: `status`, `service`,
/// `api_version`).
#[derive(Debug, Clone, Deserialize)]
struct Health {
    #[allow(dead_code)] // kept for diagnostics in debug logs
    status: String,
    #[allow(dead_code)]
    service: String,
    #[allow(dead_code)]
    api_version: String,
}

/// What one attach sequence produced.
#[derive(Debug, Clone)]
pub enum AttachOutcome {
    /// An engine was already serving; the shell attached.
    Attached {
        /// The healthy engine's loopback port.
        port: u16,
        /// The engine's version string when discovery knew it.
        version: Option<String>,
        /// The engine's PID when discovery knew it.
        pid: Option<u32>,
    },
    /// No engine answered; the shell spawned one and it came up.
    Spawned {
        /// The new engine's loopback port.
        port: u16,
        /// The engine's version string when discovery knew it.
        version: Option<String>,
    },
    /// The engine did not come up; the message is user-facing.
    Failed {
        /// The diagnostic (engine-log tail when available).
        message: String,
    },
}

/// Why a probe failed (diagnostics for the recovery page).
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// The TCP connect failed (nothing listening / refused).
    #[error("connect failed: {0}")]
    Connect(#[from] std::io::Error),
    /// The TLS handshake or HTTP exchange failed mid-way.
    #[error("tls/http exchange failed: {0}")]
    Tls(String),
    /// The response was not a usable HTTP/1.x success.
    #[error("bad response: {0}")]
    BadResponse(String),
}

/// Probe timeout: generous enough for a cold TLS handshake on a
/// busy machine, short enough that three probes still feel snappy.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for a freshly spawned engine to come healthy
/// (SQLite migrations on first boot can take a few seconds).
pub const SPAWN_WAIT: Duration = Duration::from_secs(30);

/// The poll cadence while waiting for a spawned engine.
const SPAWN_POLL: Duration = Duration::from_millis(250);

/// The engine's default loopback port (`EngineOptions::default`),
/// overridable via the environment for both sides at once.
pub fn default_port() -> u16 {
    std::env::var("ZIM_PORT")
        .ok()
        .and_then(|p| p.trim().parse::<u16>().ok())
        .unwrap_or(8443)
}

/// Read the discovery file (`None` when absent, malformed, or a
/// future schema).
pub fn discover(data_dir: &Path) -> Option<DiscoveryInfo> {
    let body = std::fs::read_to_string(data_dir.join("engine.json")).ok()?;
    let info: DiscoveryInfo = serde_json::from_str(&body).ok()?;
    (info.schema == DISCOVERY_SCHEMA).then_some(info)
}

/// Probe `/api/v1/health` on a loopback port.
///
/// Returns the engine's reported version info on `ok`.
pub fn probe(port: u16) -> Result<String, ProbeError> {
    let (status, _content_type, body) = https_get(port, "/api/v1/health")?;
    if status != 200 {
        return Err(ProbeError::BadResponse(format!("health answered {status}")));
    }
    let health: Health = serde_json::from_slice(&body)
        .map_err(|e| ProbeError::BadResponse(format!("health body not JSON: {e}")))?;
    if health.status != "ok" {
        return Err(ProbeError::BadResponse(format!(
            "health status {:?}",
            health.status
        )));
    }
    Ok(health.api_version)
}

/// One loopback HTTPS GET, the shell's whole HTTP client.
///
/// Hand-rolled HTTP/1.1 over rustls on purpose: the shell needs
/// exactly one GET shape, and a web-scale client (reqwest and its
/// tree) is weight the thin shell does not carry.
///
/// The TLS verification here is a **bounded probe verifier**: it
/// accepts any certificate, but only ever on connections this
/// process dialed to a literal loopback address. It exists to ask
/// "is that our engine in there?" — the webview's trust decision is
/// made separately by the OS trust store (see `trust.rs`), and the
/// API's real boundary is the credential-checked `/api/v1` surface.
pub fn https_get(port: u16, path: &str) -> Result<(u16, String, Vec<u8>), ProbeError> {
    use rustls::pki_types::ServerName;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| ProbeError::Tls(format!("protocol versions: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(LoopbackProbeVerifier))
        .with_no_client_auth();

    let server_name =
        ServerName::IpAddress(rustls::pki_types::IpAddr::from(std::net::IpAddr::from([
            127, 0, 0, 1,
        ])));
    let conn = rustls::ClientConnection::new(std::sync::Arc::new(config), server_name)
        .map_err(|e| ProbeError::Tls(format!("client setup: {e}")))?;

    let tcp = std::net::TcpStream::connect(("127.0.0.1", port))?;
    tcp.set_read_timeout(Some(PROBE_TIMEOUT))?;
    tcp.set_write_timeout(Some(PROBE_TIMEOUT))?;
    tcp.set_nodelay(true)?;

    let mut stream = rustls::StreamOwned::new(conn, tcp);
    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         User-Agent: zim_shell/{}\r\n\
         Accept: application/json, text/html\r\n\
         Connection: close\r\n\
         \r\n",
        env!("CARGO_PKG_VERSION")
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| ProbeError::Tls(format!("write: {e}")))?;
    stream
        .flush()
        .map_err(|e| ProbeError::Tls(format!("flush: {e}")))?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    // Read until EOF (Connection: close), a rustls "peer closed
    // without close_notify" EOF (which is how hyper closes), or a
    // complete Content-Length-delimited body — whichever comes
    // first, so a pathological server can never hang the probe.
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if response_complete(&raw) {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(ProbeError::Tls(format!("read: {e}"))),
        }
    }
    parse_http_response(&raw)
}

/// Whether `raw` already holds a full response (headers + the
/// Content-Length-delimited body; chunked responses just wait for
/// EOF — the probe never sends requests that would get one).
fn response_complete(raw: &[u8]) -> bool {
    let Some(header_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let header_block = String::from_utf8_lossy(&raw[..header_end]);
    let Some(length) = header_block.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())?
    }) else {
        return false;
    };
    raw.len() >= header_end + 4 + length
}

/// Parse a minimal HTTP/1.x response (status, content-type, body).
fn parse_http_response(raw: &[u8]) -> Result<(u16, String, Vec<u8>), ProbeError> {
    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| ProbeError::BadResponse("no header terminator".to_owned()))?;
    let header_block = String::from_utf8_lossy(&raw[..header_end]);
    let mut lines = header_block.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| ProbeError::BadResponse("empty response".to_owned()))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| {
            ProbeError::BadResponse(format!("unparseable status line: {status_line:?}"))
        })?;

    let content_type = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-type")
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default();

    Ok((status, content_type, raw[header_end + 4..].to_vec()))
}

/// Accepts any certificate — on the probe's loopback-dialed
/// connections only (see [`https_get`]).
#[derive(Debug)]
struct LoopbackProbeVerifier;

impl rustls::client::danger::ServerCertVerifier for LoopbackProbeVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme::*;
        vec![
            RSA_PSS_SHA256,
            RSA_PSS_SHA384,
            RSA_PSS_SHA512,
            ECDSA_NISTP256_SHA256,
            ECDSA_NISTP384_SHA384,
            ED25519,
        ]
    }
}

/// Spawn `zim_engine` as a detached process over the given data dir.
///
/// Detachment details per platform:
/// - Unix: `process_group(0)` — a new process group, so signals to
///   the shell's group (Ctrl-C in a terminal that launched the
///   shell) never reach the engine.
/// - Windows: `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` — no
///   console inheritance, same group isolation.
///
/// The child's stdout/stderr append to `engine.log` in the data
/// dir (the engine's boot diagnostics — the shell shows its tail
/// on the recovery page). The returned pid is informational; the
/// shell holds no kill handle, by design.
pub fn spawn_detached(engine_path: &Path, data_dir: &Path) -> std::io::Result<u32> {
    let log_path = data_dir.join("engine.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let mut command = std::process::Command::new(engine_path);
    command
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    Ok(command.spawn()?.id())
}

/// The tail of the engine log (for recovery diagnostics), capped so
/// a pathological log can never flood the recovery page.
pub fn engine_log_tail(data_dir: &Path, max_bytes: usize) -> Option<String> {
    let body = std::fs::read(data_dir.join("engine.log")).ok()?;
    let start = body.len().saturating_sub(max_bytes);
    Some(String::from_utf8_lossy(&body[start..]).into_owned())
}

/// The full attach sequence (probe-known engines, else spawn).
///
/// Pure logic over `engine.rs` primitives — the integration test
/// drives this against a real engine binary without any GUI.
pub fn attach(data_dir: &Path, engine_path: &Path) -> AttachOutcome {
    // 1) A discovery file's port — an engine that ran on an
    //    ephemeral port, or the default — probed for liveness.
    if let Some(info) = discover(data_dir) {
        if probe(info.port).is_ok() {
            return AttachOutcome::Attached {
                port: info.port,
                version: Some(info.engine_version),
                pid: Some(info.pid),
            };
        }
    }

    // 2) The default port — an engine started out-of-band with
    //    default options (or a stale discovery file pointing
    //    elsewhere while a default-port engine serves).
    let default = default_port();
    if probe(default).is_ok() {
        return AttachOutcome::Attached {
            port: default,
            version: None,
            pid: None,
        };
    }

    // 3) Nothing answered — spawn and wait for health.
    if let Err(e) = spawn_detached(engine_path, data_dir) {
        return AttachOutcome::Failed {
            message: format!("cannot start the engine ({}): {e}", engine_path.display()),
        };
    }
    let deadline = Instant::now() + SPAWN_WAIT;
    loop {
        if let Some(info) = discover(data_dir) {
            if probe(info.port).is_ok() {
                return AttachOutcome::Spawned {
                    port: info.port,
                    version: Some(info.engine_version),
                };
            }
        }
        if probe(default).is_ok() {
            return AttachOutcome::Spawned {
                port: default,
                version: None,
            };
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(SPAWN_POLL);
    }

    let tail = engine_log_tail(data_dir, 2 * 1024)
        .map(|tail| format!("\n\nengine.log (tail):\n{tail}"))
        .unwrap_or_default();
    AttachOutcome::Failed {
        message: format!(
            "the engine did not become healthy within {}s{tail}",
            SPAWN_WAIT.as_secs()
        ),
    }
}

/// Map an [`AttachOutcome`] to the UI status (helper for the
/// attach thread and the retry command).
pub fn outcome_status(outcome: &AttachOutcome) -> EngineStatus {
    use crate::state::Phase;
    match outcome {
        AttachOutcome::Attached { port, version, .. } => EngineStatus {
            phase: Phase::Ready,
            port: Some(*port),
            version: version.clone(),
            message: None,
        },
        AttachOutcome::Spawned { port, version } => EngineStatus {
            phase: Phase::Ready,
            port: Some(*port),
            version: version.clone(),
            message: None,
        },
        AttachOutcome::Failed { message } => EngineStatus {
            phase: Phase::Trouble,
            port: None,
            version: None,
            message: Some(message.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_schema_mismatch_reads_as_absent() {
        let dir = std::env::temp_dir().join(format!(
            "zim_shell_discovery_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // Absent.
        assert!(discover(&dir).is_none());

        // Malformed.
        std::fs::write(dir.join("engine.json"), "nope").unwrap();
        assert!(discover(&dir).is_none());

        // Future schema.
        let future = serde_json::json!({
            "schema": 2,
            "pid": 1,
            "port": 8443,
            "engine_version": "x",
            "started_at_unix": 0
        });
        std::fs::write(dir.join("engine.json"), future.to_string()).unwrap();
        assert!(discover(&dir).is_none());

        // Present, schema 1.
        let current = serde_json::json!({
            "schema": 1,
            "pid": 42,
            "port": 49152,
            "engine_version": "0.19.0",
            "started_at_unix": 1_700_000_000
        });
        std::fs::write(dir.join("engine.json"), current.to_string()).unwrap();
        let info = discover(&dir).expect("schema-1 discovery reads");
        assert_eq!(info.port, 49152);
        assert_eq!(info.pid, 42);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn http_response_parsing() {
        let raw = b"HTTP/1.1 200 OK\r\n\
                    Content-Type: application/json\r\n\
                    Content-Length: 2\r\n\
                    \r\n\
                    {}";
        let (status, content_type, body) = parse_http_response(raw).unwrap();
        assert_eq!(status, 200);
        assert_eq!(content_type, "application/json");
        assert_eq!(body, b"{}");

        assert!(parse_http_response(b"garbage").is_err());
        assert!(parse_http_response(b"HTTP/1.1 not-a-code\r\n\r\n").is_err());
    }

    #[test]
    fn probing_a_dead_port_fails_fast() {
        // 127.0.0.1:1 is never a listener in test environments; the
        // connect error is the point, not the specific errno.
        let result = probe(1);
        assert!(matches!(result, Err(ProbeError::Connect(_))));
    }
}
