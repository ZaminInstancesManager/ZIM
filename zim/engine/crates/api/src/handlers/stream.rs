//! `GET /api/v1/stream` — the WebSocket event stream (M7,
//! ZIM_API_SPEC.md §11).
//!
//! One connection = one subscriber on the engine's streaming hub,
//! filtered to one instance (`?instance_id={id}`) and optionally
//! replaying from a checkpoint (`?since_generation={n}`). The
//! §11.1 envelope is serialized straight from the streaming crate's
//! `StreamEvent`.
//!
//! # Handshake contract
//!
//! - Auth: the standard credential resolution (session cookie or
//!   `Authorization: Bearer` API key, via the auth middleware), or
//!   an `X-ZIM-Tab-Token` per-tab capability token (spec §3; engine
//!   side per M7). Anonymous connections are rejected before the
//!   upgrade with a structured `401`.
//! - Authorization: the caller must hold `WebsocketConnect` on the
//!   target instance (owner-implicit, subuser row, or admin — the
//!   same resolution as every other instance-scoped endpoint).
//! - Sub-protocol: `zim-events.v1` is the only supported value. A
//!   client that names sub-protocols must include it (the upgrade
//!   echoes it); a client that names none is accepted without one.
//! - Limits: the hub's per-instance subscriber cap (§13: 50)
//!   rejects with `429 rate_limited` once the instance's streams
//!   are full.
//!
//! # Delivery
//!
//! Events are written as they arrive, in per-instance generation
//! order; the engine pings every 30 s to keep intermediaries from
//! idling the connection out. Client→server text frames have no
//! v1 contract (console input is the HTTP endpoint) and are
//! ignored. The subscription ends when either side closes.
//!
//! # Deferred (documented per the M5 delivery-note style)
//!
//! The §13 per-tab limit (3 subscribers) is enforced by counting a
//! tab's live connections; with the browser-integration layer not
//! yet issuing tab tokens in production, M7 ships the per-instance
//! cap (hub-side) and leaves per-tab counting to the integration
//! milestone, where tabs exist.
//!
//! M22 note: tabs now exist (`zim://app`'s panes), but they
//! authenticate with the session cookie — a cookie is not
//! tab-scoped, so per-tab counting has nothing to count against.
//! Tab tokens become worth issuing when extension-authored panes
//! arrive (M24); until then the per-instance cap (50) is the
//! enforced bound and each pane's budget matches §9 by construction
//! (one events stream per server tab, held by the chrome).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap};
use axum::response::Response;
use axum::Extension;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use zim_engine_auth::TabTokenError;
use zim_engine_domain::{InstanceId, PermissionGrant};
use zim_engine_streaming::{EventKindSet, StreamFilter, StreamHandle, SubscribeError};

use crate::error::ApiError;
use crate::handlers::{check_grants, load_instance_for_caller, parse_uuid};
use crate::state::{db_or_500, AppState, Caller};

/// The header a tab presents its capability token in (spec §3/§11).
pub const TAB_TOKEN_HEADER: &str = "x-zim-tab-token";

/// How often the server sends a WebSocket ping (keepalive).
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// The parsed query string of `/api/v1/stream`.
///
/// Fields are strings, not typed: a malformed value must produce
/// the API's structured `validation_failed` body, not axum's
/// default query-rejection text.
#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    /// The instance to subscribe to (spec §11's
    /// `?instance_id={id}`; required — cross-instance streams are
    /// not part of the v1 contract).
    pub instance_id: Option<String>,
    /// Replay-from-checkpoint: the last event generation the client
    /// saw (spec §11.3's `?since_generation={n}`; optional).
    pub since_generation: Option<String>,
}

/// `GET /api/v1/stream` — the WebSocket upgrade.
pub async fn stream(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // ── Query parsing (structured errors, not extractor defaults) ──
    let raw_instance_id = query.instance_id.ok_or_else(|| {
        ApiError::validation(
            "the stream endpoint requires an instance_id query parameter",
            "instance_id",
        )
    })?;
    let instance_id = parse_uuid::<InstanceId>(&raw_instance_id, "instance_id")?;
    let since_generation = parse_since_generation(&query.since_generation)?;

    // ── Authorization (§11 auth, §10.3 WebsocketConnect grant) ────
    authorize(&state, &caller, &headers, &instance_id).await?;

    // ── Sub-protocol negotiation (§11: zim-events.v1 only) ────────
    let protocols = requested_subprotocols(&headers);
    if !protocols.is_empty()
        && !protocols
            .iter()
            .any(|protocol| protocol == zim_engine_streaming::WIRE_SUBPROTOCOL)
    {
        return Err(ApiError::bad_request(format!(
            "unsupported WebSocket sub-protocol(s) {}; this engine speaks {}",
            protocols.join(", "),
            zim_engine_streaming::WIRE_SUBPROTOCOL
        )));
    }

    // ── Subscribe before upgrading: subscriber-limit failures must ─
    // ── answer as structured HTTP errors, not as dead sockets.    ─
    let filter = StreamFilter {
        instance_id: Some(instance_id),
        kinds: EventKindSet::all(),
        since_generation,
    };
    let handle = state
        .supervisor
        .stream()
        .subscribe(&filter)
        .map_err(subscribe_error)?;

    Ok(ws
        .protocols([zim_engine_streaming::WIRE_SUBPROTOCOL])
        .on_upgrade(move |socket| async move {
            stream_session(socket, handle).await;
        }))
}

/// Parse `?since_generation` (absent = live-only).
fn parse_since_generation(raw: &Option<String>) -> Result<Option<u64>, ApiError> {
    match raw.as_deref() {
        None => Ok(None),
        Some(raw) => Ok(Some(raw.trim().parse::<u64>().map_err(|e| {
            ApiError::validation(
                format!("since_generation must be an unsigned integer: {e}"),
                "since_generation",
            )
        })?)),
    }
}

/// Resolve authorization for a stream connection.
///
/// Tab tokens take precedence when presented (spec §11: "or
/// `X-ZIM-Tab-Token` for tab tokens"); otherwise the request's
/// resolved caller (session cookie or API key) is authorized
/// against the instance like any other instance-scoped operation.
/// Either path must clear `WebsocketConnect` for the target
/// instance.
async fn authorize(
    state: &AppState,
    caller: &Caller,
    headers: &HeaderMap,
    instance_id: &InstanceId,
) -> Result<(), ApiError> {
    if let Some(token) = headers
        .get(TAB_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
    {
        authorize_with_tab_token(state, token, instance_id).await?;
        return Ok(());
    }

    let (_instance, ctx) = load_instance_for_caller(state, caller, instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::WebsocketConnect])?;
    Ok(())
}

/// The tab-token path: validate the token, enforce its instance
/// scope and `WebsocketConnect` capability, and confirm the user
/// it was issued for still exists.
async fn authorize_with_tab_token(
    state: &AppState,
    token: &str,
    instance_id: &InstanceId,
) -> Result<(), ApiError> {
    let view = state
        .tab_tokens
        .validate(token, chrono::Utc::now())
        .map_err(|e| match e {
            TabTokenError::Unknown => ApiError::unauthorized("tab token is unknown or revoked"),
            TabTokenError::Expired => {
                ApiError::unauthorized("tab token expired; refresh it via the integration layer")
            }
        })?;
    if view.instance_id != *instance_id {
        return Err(ApiError::PermissionDenied {
            message: format!(
                "the tab token is scoped to instance {} and cannot subscribe to {instance_id}",
                view.instance_id
            ),
            missing: vec![PermissionGrant::WebsocketConnect],
            user_id: view.user_id,
        });
    }
    if !view.grants.contains(PermissionGrant::WebsocketConnect) {
        return Err(ApiError::PermissionDenied {
            message: "the tab token does not carry the websocket.connect capability".to_owned(),
            missing: vec![PermissionGrant::WebsocketConnect],
            user_id: view.user_id,
        });
    }

    // The token's user must still exist (a deleted user's tab
    // tokens are as dead as their sessions).
    let user_id = view.user_id;
    let user = db_or_500(state, move |p| p.get_user(&user_id)).await?;
    if user.is_none() {
        return Err(ApiError::unauthorized(
            "the tab token names a user that no longer exists",
        ));
    }
    Ok(())
}

/// The `Sec-WebSocket-Protocol` values the client named (comma-
/// separated; possibly repeated headers).
fn requested_subprotocols(headers: &HeaderMap) -> Vec<String> {
    let mut protocols = Vec::new();
    for value in headers.get_all(header::SEC_WEBSOCKET_PROTOCOL) {
        if let Ok(raw) = value.to_str() {
            for protocol in raw.split(',') {
                let protocol = protocol.trim();
                if !protocol.is_empty() {
                    protocols.push(protocol.to_owned());
                }
            }
        }
    }
    protocols
}

/// Map a subscription rejection onto the API vocabulary.
///
/// The §9 headers (limit/reset/retry-after) are applied by the
/// error renderer; the offending instance is visible in the
/// request's own query string, so the body needs no extra context.
fn subscribe_error(error: SubscribeError) -> ApiError {
    match error {
        SubscribeError::TooManySubscribers { limit, .. } => ApiError::RateLimited {
            retry_after_seconds: 5,
            limit: limit as u64,
        },
    }
}

/// The live half of one stream connection: write envelopes as they
/// arrive, answer pings, ping on the keepalive cadence, and end
/// cleanly when either side goes away.
async fn stream_session(socket: WebSocket, handle: StreamHandle) {
    let (mut sender, mut receiver) = socket.split();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            maybe_event = handle.recv() => match maybe_event {
                Some(event) => {
                    let envelope = serde_json::to_string(&*event).unwrap_or_else(|_| {
                        // An event that cannot serialize is an engine
                        // bug; dropping it from one subscriber's view
                        // beats killing the connection.
                        "{\"type\":\"stream.dropped\",\"generation\":0,\"instance_id\":null,\"data\":{\"dropped_events\":1}}"
                            .to_owned()
                    });
                    if sender.send(Message::text(envelope)).await.is_err() {
                        break;
                    }
                }
                None => {
                    // The subscription ended (engine shutdown or hub
                    // drop): close the socket cleanly.
                    let _ = sender.send(Message::Close(None)).await;
                    break;
                }
            },
            maybe_message = receiver.next() => match maybe_message {
                None | Some(Ok(Message::Close(_))) => break,
                Some(Ok(Message::Ping(payload))) => {
                    let _ = sender.send(Message::Pong(payload)).await;
                }
                Some(Ok(_)) => {
                    // Text/binary client frames have no v1 contract;
                    // ignored per the module docs.
                }
                Some(Err(_)) => break,
            },
            _ = ping.tick() => {
                if sender.send(Message::Ping(Default::default())).await.is_err() {
                    break;
                }
            }
        }
    }
    handle.close();
}

#[cfg(test)]
mod tests {
    //! The handshake-level behaviors (auth, query validation,
    //! sub-protocol) are exercised end-to-end over a real socket in
    //! `tests/stream_ws.rs`; the pure helpers are covered here.

    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn ping_interval_is_sane() {
        assert!(PING_INTERVAL >= Duration::from_secs(1));
        assert!(PING_INTERVAL <= Duration::from_secs(60));
    }

    #[test]
    fn subprotocol_parsing_splits_and_trims() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("zim-events.v1, future.v2"),
        );
        headers.append(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("spaced.v3"),
        );
        let protocols = requested_subprotocols(&headers);
        assert_eq!(
            protocols,
            vec![
                "zim-events.v1".to_owned(),
                "future.v2".to_owned(),
                "spaced.v3".to_owned()
            ]
        );
    }

    #[test]
    fn no_subprotocol_means_empty_vec() {
        assert!(requested_subprotocols(&HeaderMap::new()).is_empty());
    }

    #[test]
    fn since_generation_parses_cleanly() {
        assert_eq!(parse_since_generation(&None).unwrap(), None);
        assert_eq!(
            parse_since_generation(&Some("42".to_owned())).unwrap(),
            Some(42)
        );
        assert_eq!(
            parse_since_generation(&Some(" 7 ".to_owned())).unwrap(),
            Some(7)
        );
        assert!(parse_since_generation(&Some("nope".to_owned())).is_err());
        assert!(parse_since_generation(&Some("-1".to_owned())).is_err());
    }
}
