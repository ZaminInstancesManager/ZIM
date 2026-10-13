//! `/api/v1/nodes/*` — node CRUD and node-level allocation management
//! (spec §10.4's node surface). All admin-gated per §10.10.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;

use crate::error::ApiError;
use crate::handlers::{
    audit, conflict_on_backend, parse_uuid, query_params, require_admin, require_json_content_type,
    ApiJson,
};
use crate::pagination::{page_envelope, PageParams};
use crate::state::{db_or_500, AppState, Caller};
use zim_engine_domain::{
    Allocation, LocationId, Node, NodeCapabilities, NodeDeployment, NodeId, Scheme,
};

/// Node JSON shape (domain serialization + `object` marker).
fn node_json(node: &Node) -> serde_json::Value {
    let mut value = serde_json::to_value(node).unwrap_or_default();
    if let Some(map) = value.as_object_mut() {
        map.insert("object".to_owned(), serde_json::json!("node"));
    }
    value
}

/// `GET /api/v1/nodes` — admin, page-based.
#[derive(Debug, Default, Deserialize)]
struct ListNodesQuery {
    page: Option<u32>,
    per_page: Option<u32>,
}

/// `GET /api/v1/nodes`.
pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    RawQuery(query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&caller)?;
    let q: ListNodesQuery = query_params(query.as_deref().unwrap_or(""))?;
    let page = PageParams::from_query(q.page, q.per_page)?;
    let mut nodes = db_or_500(&state, |p| p.list_nodes()).await?;
    nodes.sort_by_key(|n| std::cmp::Reverse(n.name.clone()));
    let total = nodes.len();
    let (start, end) = page.bounds(total);
    let data: Vec<_> = nodes[start..end].iter().map(node_json).collect();
    Ok(Json(page_envelope(page, total, data)))
}

/// Node creation/patch body (partial for PATCH).
#[derive(Debug, Default, Deserialize)]
pub struct NodeBody {
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Location grouping (defaults to a generated location).
    #[serde(default)]
    pub location_id: Option<String>,
    /// FQDN advertised by the remote-node contract.
    #[serde(default)]
    pub fqdn: String,
    /// `http` or `https` (default https).
    #[serde(default)]
    pub scheme: Option<String>,
    /// Advertised client port (SFTP legacy surface).
    #[serde(default)]
    pub public_port: Option<u16>,
    /// Node-contract listen port.
    #[serde(default)]
    pub listen_port: Option<u16>,
    /// Capability family switches (defaults: console + filesystem).
    #[serde(default)]
    pub capabilities: Option<PartialCapabilities>,
    /// `local_linux` (default) or `remote_linux`.
    #[serde(default)]
    pub deployment: Option<String>,
}

/// Capability switches a client may set at creation time.
#[derive(Debug, Default, Deserialize)]
pub struct PartialCapabilities {
    /// Container runtime present.
    #[serde(default)]
    pub container_runtime: Option<bool>,
    /// ConsoleCapable.
    #[serde(default)]
    pub console: Option<bool>,
    /// ReadyProbeCapable.
    #[serde(default)]
    pub ready_probe: Option<bool>,
    /// HealthCheckCapable.
    #[serde(default)]
    pub health_check: Option<bool>,
    /// FilesystemCapable.
    #[serde(default)]
    pub filesystem: Option<bool>,
    /// NetworkCapable.
    #[serde(default)]
    pub network: Option<bool>,
    /// SnapshotCapable.
    #[serde(default)]
    pub snapshot: Option<bool>,
}

fn default_capabilities() -> NodeCapabilities {
    // A standard local Docker node: console + filesystem on; the
    // exotic capability families are opt-in.
    NodeCapabilities {
        container_runtime: true,
        console: true,
        ready_probe: false,
        health_check: false,
        filesystem: true,
        network: false,
        snapshot: false,
    }
}

fn merge_capabilities(base: NodeCapabilities, partial: &PartialCapabilities) -> NodeCapabilities {
    NodeCapabilities {
        container_runtime: partial.container_runtime.unwrap_or(base.container_runtime),
        console: partial.console.unwrap_or(base.console),
        ready_probe: partial.ready_probe.unwrap_or(base.ready_probe),
        health_check: partial.health_check.unwrap_or(base.health_check),
        filesystem: partial.filesystem.unwrap_or(base.filesystem),
        network: partial.network.unwrap_or(base.network),
        snapshot: partial.snapshot.unwrap_or(base.snapshot),
    }
}

/// `POST /api/v1/nodes` — admin.
pub async fn create(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<NodeBody>,
) -> Result<Response, ApiError> {
    let admin = require_admin(&caller)?;
    require_json_content_type(&headers)?;
    let scheme = match body.scheme.as_deref() {
        None => Scheme::Https,
        Some("https") => Scheme::Https,
        Some("http") => Scheme::Http,
        Some(other) => {
            return Err(ApiError::validation(
                format!("unknown scheme {other:?} (http, https)"),
                "scheme",
            ));
        }
    };
    let deployment = match body.deployment.as_deref() {
        None => NodeDeployment::LocalLinux,
        Some("local_linux") => NodeDeployment::LocalLinux,
        Some("remote_linux") => NodeDeployment::RemoteLinux,
        Some(other) => {
            return Err(ApiError::validation(
                format!("unknown deployment {other:?} (local_linux, remote_linux)"),
                "deployment",
            ));
        }
    };
    let node = Node {
        id: NodeId::new(),
        name: body.name,
        location_id: body
            .location_id
            .as_deref()
            .map(|raw| parse_uuid::<LocationId>(raw, "location_id"))
            .transpose()?
            .unwrap_or_else(LocationId::new),
        fqdn: if body.fqdn.is_empty() {
            "localhost".to_owned()
        } else {
            body.fqdn
        },
        scheme,
        public_port: body.public_port.unwrap_or(2022),
        listen_port: body.listen_port.unwrap_or(8081),
        capabilities: body
            .capabilities
            .as_ref()
            .map(|c| merge_capabilities(default_capabilities(), c))
            .unwrap_or_else(default_capabilities),
        deployment,
        last_heartbeat: None,
        last_seen_state: Default::default(),
    };
    node.validate()
        .map_err(|e| ApiError::validation(e.to_string(), "node"))?;
    db_or_500(&state, {
        let node = node.clone();
        move |p| p.put_node(&node)
    })
    .await?;
    audit(
        &state,
        Some(admin.id.to_string()),
        "node.create",
        None,
        serde_json::json!({ "name": node.name }),
    )
    .await;
    let mut response = Json(node_json(&node)).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

/// `GET /api/v1/nodes/{id}` — admin.
pub async fn get(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&caller)?;
    let node_id = parse_uuid::<NodeId>(&id, "id")?;
    let node = db_or_500(&state, move |p| p.get_node(&node_id))
        .await?
        .ok_or_else(|| ApiError::not_found("node", id.clone()))?;
    Ok(Json(node_json(&node)))
}

/// `PATCH /api/v1/nodes/{id}` — admin.
pub async fn patch(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<NodeBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let admin = require_admin(&caller)?;
    require_json_content_type(&headers)?;
    let node_id = parse_uuid::<NodeId>(&id, "id")?;
    let existing = db_or_500(&state, move |p| p.get_node(&node_id))
        .await?
        .ok_or_else(|| ApiError::not_found("node", id.clone()))?;
    let mut updated = existing.clone();
    if !body.name.is_empty() {
        updated.name = body.name;
    }
    if !body.fqdn.is_empty() {
        updated.fqdn = body.fqdn;
    }
    if let Some(scheme) = body.scheme.as_deref() {
        updated.scheme = match scheme {
            "https" => Scheme::Https,
            "http" => Scheme::Http,
            other => {
                return Err(ApiError::validation(
                    format!("unknown scheme {other:?} (http, https)"),
                    "scheme",
                ));
            }
        };
    }
    if let Some(port) = body.public_port {
        updated.public_port = port;
    }
    if let Some(port) = body.listen_port {
        updated.listen_port = port;
    }
    if let Some(caps) = body.capabilities.as_ref() {
        updated.capabilities = merge_capabilities(updated.capabilities, caps);
    }
    if let Some(deployment) = body.deployment.as_deref() {
        updated.deployment = match deployment {
            "local_linux" => NodeDeployment::LocalLinux,
            "remote_linux" => NodeDeployment::RemoteLinux,
            other => {
                return Err(ApiError::validation(
                    format!("unknown deployment {other:?} (local_linux, remote_linux)"),
                    "deployment",
                ));
            }
        };
    }
    updated
        .validate()
        .map_err(|e| ApiError::validation(e.to_string(), "node"))?;
    db_or_500(&state, {
        let updated = updated.clone();
        move |p| p.put_node(&updated)
    })
    .await?;
    audit(
        &state,
        Some(admin.id.to_string()),
        "node.update",
        None,
        serde_json::json!({ "node": id }),
    )
    .await;
    Ok(Json(node_json(&updated)))
}

/// `DELETE /api/v1/nodes/{id}` — admin; must be empty of instances
/// (FK guard → 409).
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let admin = require_admin(&caller)?;
    let node_id = parse_uuid::<NodeId>(&id, "id")?;
    crate::state::db(&state, move |p| p.delete_node(&node_id))
        .await
        .map_err(|e| {
            conflict_on_backend(e, "node still hosts instances; move or delete them first")
        })?;
    audit(
        &state,
        Some(admin.id.to_string()),
        "node.delete",
        None,
        serde_json::json!({ "node": id }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/nodes/{id}/state` — heartbeat/observed state view.
pub async fn state(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&caller)?;
    let node_id = parse_uuid::<NodeId>(&id, "id")?;
    let node = db_or_500(&state, move |p| p.get_node(&node_id))
        .await?
        .ok_or_else(|| ApiError::not_found("node", id.clone()))?;
    Ok(Json(serde_json::json!({
        "id": node.id,
        "last_heartbeat": node.last_heartbeat,
        "last_seen_state": node.last_seen_state,
        "observed_by": "persistence",
    })))
}

// ── Node allocations (§10.4) ──────────────────────────────────────

/// `GET /api/v1/nodes/{id}/allocations` — admin.
pub async fn list_allocations(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&caller)?;
    let node_id = parse_uuid::<NodeId>(&id, "id")?;
    let node = db_or_500(&state, move |p| p.get_node(&node_id))
        .await?
        .ok_or_else(|| ApiError::not_found("node", id.clone()))?;
    let allocations = db_or_500(&state, move |p| {
        let id = node.id;
        p.list_allocations_by_node(&id)
    })
    .await?;
    Ok(Json(serde_json::json!({ "data": allocations })))
}

/// `POST /api/v1/nodes/{id}/allocations` — admin; creates a free
/// allocation in the node's address space.
#[derive(Debug, Default, Deserialize)]
pub struct CreateAllocationBody {
    /// IP address (defaults to 127.0.0.1 for the local node).
    #[serde(default)]
    pub ip: Option<String>,
    /// Port (1–65535).
    #[serde(default)]
    pub port: u16,
    /// Operator note.
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /api/v1/nodes/{id}/allocations`.
pub async fn create_allocation(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<CreateAllocationBody>,
) -> Result<Response, ApiError> {
    let admin = require_admin(&caller)?;
    let node_id = parse_uuid::<NodeId>(&id, "id")?;
    let node = db_or_500(&state, move |p| p.get_node(&node_id))
        .await?
        .ok_or_else(|| ApiError::not_found("node", id.clone()))?;
    let ip: IpAddr = match body.ip.as_deref() {
        None => "127.0.0.1".parse().expect("static ip parses"),
        Some(raw) => raw
            .parse()
            .map_err(|_| ApiError::validation(format!("invalid IP address {raw:?}"), "ip"))?,
    };
    if body.port == 0 {
        return Err(ApiError::validation("port must be 1-65535", "port"));
    }

    // Duplicate (ip, port) on the node → 409.
    let existing = db_or_500(&state, {
        let id = node.id;
        move |p| p.list_allocations_by_node(&id)
    })
    .await?;
    if existing.iter().any(|a| a.ip == ip && a.port == body.port) {
        return Err(ApiError::Conflict(format!(
            "allocation {ip}:{} already exists on this node",
            body.port
        )));
    }

    // The store assigns the auto-increment id.
    let note = body.note.clone();
    let created_id = db_or_500(&state, {
        let node = node.clone();
        let port = body.port;
        let note = note.clone();
        move |p| p.create_allocation(&node.id, ip, port, note.as_deref())
    })
    .await?;
    let allocation = Allocation {
        id: created_id,
        node_id: node.id,
        ip,
        port: body.port,
        assigned_to: None,
        note: body.note,
    };
    audit(
        &state,
        Some(admin.id.to_string()),
        "node.allocation.create",
        None,
        serde_json::json!({ "node": id, "allocation": format!("{ip}:{}", body.port) }),
    )
    .await;
    let mut response = Json(serde_json::to_value(&allocation).unwrap_or_default()).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}
