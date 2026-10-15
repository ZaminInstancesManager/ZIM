//! The shared health-check vocabulary (M19).
//!
//! [`HealthCheckSpec`] is the spec-shaped form of the §4
//! `HealthCheckCapable` contract: what to probe, on which logical
//! port, how long to wait. Both first-party consumers — the
//! Website integration (its spec's `health_check`) and the
//! GenericContainer integration (the same) — parse it from their
//! workload rows, and the executor here turns one into a
//! [`Health`] verdict over a placed host binding.
//!
//! Two probe kinds ship (the roadmap's §M19 "HTTP/TCP health
//! checks"):
//!
//! - [`HealthCheckKind::Tcp`] — the listener accepts a connection.
//!   Process presence says nothing about the listener; a TCP accept
//!   says the service bound its port.
//! - [`HealthCheckKind::Http`] — a minimal HTTP/1.1 `GET` over the
//!   same connection, classified against the expected status (any
//!   2xx by default). The probe speaks plain HTTP: an HTTPS
//!   endpoint is probed at the TCP level (the handshake proves the
//!   listener; response-level TLS probing arrives with the engine's
//!   own TLS client surface).
//!
//! The executor is deliberately small — a hand-rolled request
//! line, header block, and status-line parse over [`tokio`]'s TCP
//! — because a health probe needs exactly one request and one
//! status line, not an HTTP client stack.

#![forbid(unsafe_code)]

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::errors::SpecError;
use crate::traits::Health;

/// The default probe timeout (the same order the engine's probe
/// surfaces use; a healthy local listener answers in milliseconds).
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The maximum request-path length accepted in a spec (the probe
/// writes the path verbatim into a request line; a bound keeps a
/// pathological spec from turning the probe into a firehose).
pub const MAX_HTTP_PATH_LEN: usize = 2048;

/// What the probe does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HealthCheckKind {
    /// The listener accepts a TCP connection.
    Tcp,
    /// An HTTP/1.1 `GET` answered with an acceptable status.
    Http {
        /// The request path (default `/`).
        #[serde(default = "default_http_path")]
        path: String,
        /// The acceptable status code; `None` accepts any 2xx.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_status: Option<u16>,
    },
}

/// The default HTTP probe path.
fn default_http_path() -> String {
    "/".to_owned()
}

impl fmt::Display for HealthCheckKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp => write!(f, "tcp"),
            Self::Http { path, .. } => write!(f, "http:{path}"),
        }
    }
}

/// One health check, spec-shaped.
///
/// `port` names the *logical* endpoint carrying the check (e.g.
/// `"http"`); the handle resolves it to the placed host binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheckSpec {
    /// What the probe does.
    pub kind: HealthCheckKind,
    /// The logical endpoint name the probe targets (the workload's
    /// primary endpoint when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    /// The probe timeout.
    #[serde(default = "default_probe_timeout_ms")]
    pub timeout_ms: u64,
}

/// The default timeout, in the serde-facing milliseconds form.
const fn default_probe_timeout_ms() -> u64 {
    DEFAULT_PROBE_TIMEOUT.as_millis() as u64
}

impl HealthCheckSpec {
    /// Build a TCP check against the named logical port.
    pub fn tcp(port: &str) -> Self {
        Self {
            kind: HealthCheckKind::Tcp,
            port: Some(port.to_owned()),
            timeout_ms: default_probe_timeout_ms(),
        }
    }

    /// Build an HTTP check for `path` against the named logical
    /// port, accepting any 2xx.
    pub fn http(port: &str, path: &str) -> Self {
        Self {
            kind: HealthCheckKind::Http {
                path: path.to_owned(),
                expected_status: None,
            },
            port: Some(port.to_owned()),
            timeout_ms: default_probe_timeout_ms(),
        }
    }

    /// The probe's timeout.
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    /// The spec's structural invariants.
    ///
    /// Strict where the probe would otherwise misbehave (an empty
    /// or newline-carrying HTTP path — the request line is written
    /// verbatim; a zero timeout — every probe would time out); the
    /// expected status is validated to be a plausible HTTP status
    /// class rather than a specific code, because a health check
    /// that accepts `503` is a legitimate configuration.
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.timeout_ms == 0 {
            return Err(SpecError::Invalid {
                field: "health_check.timeout_ms".to_owned(),
                message: "the probe timeout must be positive".to_owned(),
            });
        }
        match &self.kind {
            HealthCheckKind::Tcp => Ok(()),
            HealthCheckKind::Http { path, expected_status } => {
                if path.is_empty() {
                    return Err(SpecError::Invalid {
                        field: "health_check.path".to_owned(),
                        message: "the HTTP probe path must not be empty".to_owned(),
                    });
                }
                if path.len() > MAX_HTTP_PATH_LEN {
                    return Err(SpecError::Invalid {
                        field: "health_check.path".to_owned(),
                        message: format!(
                            "the HTTP probe path exceeds {MAX_HTTP_PATH_LEN} bytes"
                        ),
                    });
                }
                if path.contains(['\r', '\n', ' ']) {
                    return Err(SpecError::Invalid {
                        field: "health_check.path".to_owned(),
                        message: "the HTTP probe path must not carry control characters \
                             or spaces (it is written verbatim into the request line)"
                            .to_owned(),
                    });
                }
                if let Some(status) = expected_status {
                    if !(100..=599).contains(status) {
                        return Err(SpecError::Invalid {
                            field: "health_check.expected_status".to_owned(),
                            message: format!("{status} is not an HTTP status code"),
                        });
                    }
                }
                Ok(())
            }
        }
    }
}

/// Run one health probe against a placed host binding.
///
/// `host`/`port` are the *host-side* binding (the published port
/// of the workload's container). The verdict's reasons carry the
/// probe's own vocabulary — refused, timeout, status mismatch —
/// so the surfaced `Unhealthy` explains itself.
pub async fn run_health_probe(
    spec: &HealthCheckSpec,
    host: IpAddr,
    port: u16,
) -> Health {
    let timeout = spec.timeout();
    let attempt = match &spec.kind {
        HealthCheckKind::Tcp => probe_tcp(host, port, timeout).await,
        HealthCheckKind::Http { path, expected_status } => {
            probe_http(host, port, path, *expected_status, timeout).await
        }
    };
    match attempt {
        Ok(()) => Health::Healthy,
        Err(reason) => Health::Unhealthy { reason },
    }
}

/// One probe failure's reason.
type ProbeFailure = String;

/// The TCP probe: connect within the timeout.
async fn probe_tcp(host: IpAddr, port: u16, timeout: Duration) -> Result<(), ProbeFailure> {
    let connect = tokio::time::timeout(timeout, tokio::net::TcpStream::connect((host, port)))
        .await
        .map_err(|_| format!("the tcp probe timed out after {timeout:?}"))?
        .map_err(|err| format!("the tcp probe could not connect: {err}"))?;
    drop(connect);
    Ok(())
}

/// The HTTP probe: connect, send one `GET`, parse the status line.
async fn probe_http(
    host: IpAddr,
    port: u16,
    path: &str,
    expected: Option<u16>,
    timeout: Duration,
) -> Result<(), ProbeFailure> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::time::timeout(
        timeout,
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    .map_err(|_| format!("the http probe timed out after {timeout:?}"))?
    .map_err(|err| format!("the http probe could not connect: {err}"))?;

    let host_header = match host {
        IpAddr::V4(addr) => addr.to_string(),
        IpAddr::V6(addr) => format!("[{addr}]"),
    };
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: zim-health-probe\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    tokio::time::timeout(timeout, stream.write_all(request.as_bytes()))
        .await
        .map_err(|_| format!("the http probe timed out after {timeout:?}"))?
        .map_err(|err| format!("the http probe could not send: {err}"))?;

    // The status line arrives first; a small bound suffices (the
    // headers may follow, but the verdict is decided already).
    let mut head = [0u8; 256];
    let read = tokio::time::timeout(timeout, stream.read(&mut head))
        .await
        .map_err(|_| format!("the http probe timed out after {timeout:?}"))?
        .map_err(|err| format!("the http probe could not read: {err}"))?;
    if read == 0 {
        return Err("the http probe closed the connection before answering".to_owned());
    }
    let status_line_end = head[..read]
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(read);
    let status_line = String::from_utf8_lossy(&head[..status_line_end]);
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| {
            format!("the http probe could not parse a status line from {status_line:?}")
        })?;
    match expected {
        Some(want) if want != status => Err(format!(
            "the http probe expected status {want} but the workload answered {status}"
        )),
        _ if (200..300).contains(&status) => Ok(()),
        _ => Err(format!(
            "the http probe expected a 2xx status but the workload answered {status}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kinds_display_compactly() {
        assert_eq!(HealthCheckKind::Tcp.to_string(), "tcp");
        let http = HealthCheckKind::Http {
            path: "/healthz".to_owned(),
            expected_status: None,
        };
        assert_eq!(http.to_string(), "http:/healthz");
    }

    #[test]
    fn the_specs_round_trip_through_serde() {
        let spec = HealthCheckSpec::http("http", "/healthz");
        let json = serde_json::to_value(&spec).unwrap();
        let back: HealthCheckSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);

        let json = serde_json::json!({
            "kind": { "type": "http", "path": "/" },
            "port": "https",
            "timeout_ms": 2500
        });
        let spec: HealthCheckSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec.timeout(), Duration::from_millis(2500));
        assert!(matches!(
            spec.kind,
            HealthCheckKind::Http {
                expected_status: None,
                ..
            }
        ));
    }

    #[test]
    fn a_minimal_json_check_defaults_to_the_root_path() {
        let json = serde_json::json!({ "kind": { "type": "http" } });
        let spec: HealthCheckSpec = serde_json::from_value(json).unwrap();
        assert!(spec.validate().is_ok());
        assert_eq!(spec.timeout(), DEFAULT_PROBE_TIMEOUT);
    }

    #[test]
    fn malformed_checks_are_refused() {
        let mut spec = HealthCheckSpec::http("http", "/");
        spec.timeout_ms = 0;
        assert!(spec.validate().is_err());

        let mut spec = HealthCheckSpec {
            kind: HealthCheckKind::Http {
                path: "/a b".to_owned(),
                expected_status: None,
            },
            port: None,
            timeout_ms: 1000,
        };
        assert!(spec.validate().is_err());

        spec.kind = HealthCheckKind::Http {
            path: "/ok".to_owned(),
            expected_status: Some(999),
        };
        assert!(spec.validate().is_err());

        // An unusual-but-real status is a legitimate configuration.
        spec.kind = HealthCheckKind::Http {
            path: "/ok".to_owned(),
            expected_status: Some(503),
        };
        assert!(spec.validate().is_ok());

        spec.kind = HealthCheckKind::Http {
            path: String::new(),
            expected_status: None,
        };
        assert!(spec.validate().is_err());
    }

    #[tokio::test]
    async fn a_refused_listener_is_unhealthy_with_its_reason() {
        // Port 1 on loopback: nothing listens there in a test
        // environment (the privileged-services range is closed by
        // default); the probe must report Unhealthy, not hang.
        let spec = HealthCheckSpec {
            kind: HealthCheckKind::Tcp,
            port: Some("http".to_owned()),
            timeout_ms: 750,
        };
        let verdict = run_health_probe(&spec, "127.0.0.1".parse().unwrap(), 1).await;
        assert!(matches!(verdict, Health::Unhealthy { .. }));
    }

    #[tokio::test]
    async fn a_live_http_listener_answers_healthy() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut scratch = [0u8; 512];
            let read = socket.read(&mut scratch).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&scratch[..read]).into_owned();
            assert!(request.starts_with("GET /healthz "));
            socket
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let spec = HealthCheckSpec::http("http", "/healthz");
        let verdict = run_health_probe(&spec, "127.0.0.1".parse().unwrap(), port).await;
        server.await.unwrap();
        assert_eq!(verdict, Health::Healthy);
    }

    #[tokio::test]
    async fn an_unacceptable_status_is_unhealthy() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut scratch = [0u8; 512];
            let _ = socket.read(&mut scratch).await;
            socket
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let spec = HealthCheckSpec::http("http", "/healthz");
        let verdict = run_health_probe(&spec, "127.0.0.1".parse().unwrap(), port).await;
        server.await.unwrap();
        match verdict {
            Health::Unhealthy { reason } => {
                assert!(reason.contains("503"), "{reason}");
            }
            other => panic!("expected Unhealthy, got {other:?}"),
        }
    }
}
