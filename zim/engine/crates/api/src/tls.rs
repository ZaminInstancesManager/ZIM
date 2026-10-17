//! Loopback TLS: self-signed certificate bootstrap and the HTTPS
//! accept loop.
//!
//! Per `docs/api/ZIM_API_SPEC.md` §1 the engine speaks HTTPS (WSS
//! later) on loopback only. The certificate is self-signed for
//! `localhost` / `127.0.0.1` / `::1`; the browser integration layer
//! (M13) will teach Chromium to trust the engine's loopback
//! certificate. Until then, API clients (tests, the CLI) accept the
//! self-signed certificate explicitly.
//!
//! Serving is a manual accept loop: `TcpListener` →
//! `tokio_rustls::TlsAcceptor` → `hyper_util` auto connection
//! builder over the axum `Router`, with the peer address injected
//! into request extensions (`std::net::SocketAddr`) so handlers and
//! middleware can see the client IP (login rate limiting, audit
//! records).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnectionBuilder;
use hyper_util::service::TowerToHyperService;
use rcgen::generate_simple_self_signed;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tower::Service;

use crate::error::ApiError;

/// PEM material for the loopback certificate and key.
#[derive(Debug, Clone)]
pub struct TlsMaterial {
    /// The certificate chain in PEM form.
    pub cert_pem: String,
    /// The private key in PEM form.
    pub key_pem: String,
}

/// Subject-alt names the loopback certificate covers.
const LOOPBACK_SANS: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// Generate a fresh self-signed loopback certificate.
///
/// Validity and key material are rcgen defaults (ECDSA P-256, one
/// year) — the loopback certificate is regenerated on demand, so a
/// long validity horizon is unnecessary.
pub fn generate_loopback_certificate() -> Result<TlsMaterial, ApiError> {
    // One crypto provider per process (ring); repeated calls are
    // no-ops. Installing here keeps certificate building and
    // `TlsAcceptor` construction provider-consistent even when the
    // embedder (tests, the engine binary) never installs one.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certified = generate_simple_self_signed(
        LOOPBACK_SANS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    )
    .map_err(|e| ApiError::internal(format!("loopback certificate generation failed: {e}")))?;
    Ok(TlsMaterial {
        cert_pem: certified.cert.pem(),
        key_pem: certified.key_pair.serialize_pem(),
    })
}

/// Build a TLS acceptor from PEM material (generated fresh or
/// re-loaded from the engine's data directory).
pub fn tls_acceptor(material: &TlsMaterial) -> Result<TlsAcceptor, ApiError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certs: Vec<_> = rustls_pemfile::certs(&mut material.cert_pem.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|e| ApiError::internal(format!("loopback certificate PEM parse failed: {e}")))?;
    if certs.is_empty() {
        return Err(ApiError::internal(
            "loopback certificate PEM contains no certificates",
        ));
    }
    let key = rustls_pemfile::private_key(&mut material.key_pem.as_bytes())
        .map_err(|e| ApiError::internal(format!("loopback key PEM parse failed: {e}")))?
        .ok_or_else(|| ApiError::internal("loopback key PEM contains no private key"))?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| ApiError::internal(format!("TLS server configuration failed: {e}")))?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// A running API server handle.
#[derive(Debug)]
pub struct RunningServer {
    /// The loopback address the server is listening on (useful when
    /// binding port 0 for an ephemeral test port).
    pub local_addr: SocketAddr,
    /// Cancels the accept loop; the server drains in-flight
    /// connections on [`RunningServer::shutdown`].
    pub shutdown: CancellationToken,
    /// Connection task tracker (drained on shutdown).
    pub connections: TaskTracker,
    /// The accept-loop join handle.
    pub accept_task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl RunningServer {
    /// Trigger shutdown and drain connections (10 s graceful
    /// window, then abort).
    pub async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = self.accept_task.await;
        let drained =
            tokio::time::timeout(std::time::Duration::from_secs(10), self.connections.wait()).await;
        if drained.is_err() {
            self.connections.close();
        }
    }
}

/// Serve `router` over HTTPS on `addr`.
///
/// `addr` must be loopback; a non-loopback bind is refused (the
/// loopback-only transport rule is enforced at the socket, not just
/// the `Host` header).
pub async fn serve_https(
    router: axum::Router,
    addr: std::net::IpAddr,
    port: u16,
    tls: &TlsMaterial,
) -> Result<RunningServer, ApiError> {
    if !addr.is_loopback() {
        return Err(ApiError::internal(format!(
            "refusing to bind {addr}: the engine API is loopback-only"
        )));
    }
    let acceptor = tls_acceptor(tls)?;
    let listener = TcpListener::bind(SocketAddr::new(addr, port))
        .await
        .map_err(|e| ApiError::internal(format!("loopback bind failed: {e}")))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| ApiError::internal(format!("local_addr failed: {e}")))?;

    let shutdown = CancellationToken::new();
    let connections = TaskTracker::new();
    let shutdown_token = shutdown.clone();
    let tracker = connections.clone();
    let accept_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                () = shutdown_token.cancelled() => break,
                accepted = listener.accept() => {
                    let Ok((stream, peer)) = accepted else { continue };
                    let Ok(tls_stream) = acceptor.accept(stream).await else { continue };
                    let service = ConnectInfoService {
                        inner: router.clone(),
                        peer,
                    };
                    tracker.spawn(async move {
                        let hyper_stream = TokioIo::new(tls_stream);
                        let builder = ConnectionBuilder::new(TokioExecutor::new());
                        // HTTP/1 (and H2 over ALPN when negotiated). The
                        // tower -> hyper service adapter carries the
                        // peer-address wrapper.
                        //
                        // `serve_connection_with_upgrades` — the
                        // upgrades half is load-bearing: hyper-util's
                        // plain `serve_connection` completes the HTTP
                        // exchange but never hands the IO to the
                        // `Upgrade` extension, so a 101 WebSocket
                        // handshake succeeds and the connection is
                        // dropped immediately after (the browser sees
                        // 1006 and reconnect-loops forever — the live
                        // event stream never delivered a frame over
                        // the production TLS server until this was
                        // switched; found by M25's E2E browser suite,
                        // invisible to the plain-`axum::serve` API
                        // tests which enable upgrades by default).
                        let service = TowerToHyperService::new(service);
                        let _ = builder
                            .serve_connection_with_upgrades(hyper_stream, service)
                            .await;
                    });
                }
            }
        }
        tracker.close();
        Ok(())
    });

    Ok(RunningServer {
        local_addr,
        shutdown,
        connections,
        accept_task,
    })
}

/// Tower service wrapper that stamps the peer address into every
/// request's extensions (the manual accept loop's replacement for
/// axum's `into_make_service_with_connect_info`).
#[derive(Debug, Clone)]
struct ConnectInfoService<S> {
    inner: S,
    peer: SocketAddr,
}

impl<S, B> Service<Request<B>> for ConnectInfoService<S>
where
    S: Service<Request<B>, Response = Response> + Send + 'static,
    S::Future: Send,
    B: axum::body::HttpBody + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        req.extensions_mut().insert(self.peer);
        self.inner.call(req)
    }
}

/// Marker re-export so dependents can reference the body type used
/// by the accept loop without naming hyper directly.
pub type ServingBody = Body;
