//! The managed-database family (M31, spec §10.21): the per-instance
//! databases (`/instances/{id}/databases`) and the admin database
//! hosts (`/hosts`).
//!
//! # The surface in one paragraph
//!
//! A database host is the connection record for an external DB
//! server (name, address, port, an administrative credential the
//! provisioning side would use, an optional node tie). A managed
//! database is a row provisioned for one instance on one host: a
//! namespaced name (`s{short}_{name}`), a generated username, a
//! generated password, an allowed connection source, and a
//! per-user connection cap. The engine stores and manages these
//! rows — create/list/rotate/delete, the limit gates, the
//! connection info — and does not connect to the host (the honest
//! boundary recorded in the domain module; provisioning is the
//! workloads/database integration's milestone).
//!
//! # Credential discipline (the `view_password` grant's own gate)
//!
//! Passwords appear in exactly three places: the create response
//! (the caller just made the row — `database.create` is the gate,
//! and the operator needs the initial credential), the
//! rotate-password response (same argument for `database.update`),
//! and the dedicated password endpoint gated by
//! `database.view_password` — the Panel's own distinction (its
//! `Permission.php` keeps `view_password` separate from `read`;
//! ZIM preserved the grant verbatim in AD-12, and this surface is
//! where it earns its keep). List and detail responses never carry
//! passwords; host credentials are write-only through this API
//! entirely (the admin supplies them, nothing reads them back).
//!
//! # The limit gates
//!
//! The instance row's `database_limit` is the feature switch AND
//! the cap (None = feature off — the desktop model's one-knob
//! posture; a number = at most that many rows). The host's
//! `max_databases` is a second cap (None = uncapped). Both refuse
//! with a structured 409 that names the limit; grammar failures
//! are the house `400 validation_failed`. Never a silent truncate,
//! never a 500.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Extension;
use axum::Json;
use serde::Deserialize;

use crate::error::ApiError;
use crate::handlers::{
    audit, check_grants, db_or_500, load_instance_for_caller, parse_uuid, require_admin, ApiJson,
};
use crate::state::{AppState, Caller};
use chrono::Utc;
use zim_engine_domain::{
    DatabaseHost, DatabaseHostId, InstanceId, ManagedDatabase, ManagedDatabaseId, NodeId,
    PermissionGrant,
};

// ── Wire shapes ────────────────────────────────────────────────────────

/// A host on the wire: NEVER the password (write-only credential).
fn host_json(host: &DatabaseHost, databases_count: u64) -> serde_json::Value {
    serde_json::json!({
        "object": "database_host",
        "id": host.id,
        "name": host.name,
        "host": host.host,
        "port": host.port,
        "username": host.username,
        "max_databases": host.max_databases,
        "node_id": host.node_id,
        "databases_count": databases_count,
        "created_at": host.created_at,
        "updated_at": host.updated_at,
    })
}

/// A database on the wire: the connection info the operator (or the
/// workload's config template) needs, and NEVER the password — the
/// create/rotate responses and the gated password endpoint are the
/// only places it appears.
fn database_json(
    database: &ManagedDatabase,
    host: &DatabaseHost,
    password: Option<&str>,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "object": "server_database",
        "id": database.id,
        "instance_id": database.instance_id,
        "host_id": database.host_id,
        "name": database.name,
        "username": database.username,
        "remote": database.remote,
        "max_connections": database.max_connections,
        "connections": {
            "host": host.host,
            "port": host.port,
        },
        "created_at": database.created_at,
        "updated_at": database.updated_at,
    });
    if let Some(password) = password {
        value
            .as_object_mut()
            .expect("the json! literal is an object")
            .insert("password".to_owned(), serde_json::json!(password));
    }
    value
}

// ── The admin host family ──────────────────────────────────────────────

/// `GET /api/v1/hosts` — admin: every database host, with each
/// host's live database count (the delete gate's number, surfaced
/// so the operator never has to discover it by trying).
pub async fn list_hosts(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&caller)?;
    let hosts = db_or_500(&state, move |p| p.list_database_hosts()).await?;
    let mut data = Vec::with_capacity(hosts.len());
    for host in &hosts {
        let count = db_or_500(&state, {
            let id = host.id;
            move |p| p.count_databases_by_host(&id)
        })
        .await?;
        data.push(host_json(host, count));
    }
    Ok(Json(serde_json::json!({ "data": data })))
}

/// The create/update body for a host.
#[derive(Debug, Default, Deserialize)]
pub struct HostBody {
    /// Display name (unique).
    #[serde(default)]
    pub name: Option<String>,
    /// The host's address (hostname or IP; no scheme, no port).
    #[serde(default)]
    pub host: Option<String>,
    /// The DB server port.
    #[serde(default)]
    pub port: Option<u16>,
    /// The administrative connection username.
    #[serde(default)]
    pub username: Option<String>,
    /// The administrative connection password (write-only).
    #[serde(default)]
    pub password: Option<String>,
    /// How many databases this host may carry (None = uncapped).
    #[serde(default)]
    pub max_databases: Option<u32>,
    /// The node this host serves, when tied to one.
    #[serde(default)]
    pub node_id: Option<String>,
}

/// `POST /api/v1/hosts` — admin: register a database host.
pub async fn create_host(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    ApiJson(body): ApiJson<HostBody>,
) -> Result<Response, ApiError> {
    let admin = require_admin(&caller)?;

    let name = body.name.unwrap_or_default();
    // Uniqueness up front: a duplicate host name is a 409, not a 500.
    let probe = name.clone();
    let existing = db_or_500(&state, move |p| p.get_database_host_by_name(&probe)).await?;
    if existing.is_some() {
        return Err(ApiError::Conflict(format!(
            "a database host named {:?} already exists",
            name
        )));
    }

    let node_id = resolve_node_id(&state, body.node_id.as_deref()).await?;
    let host = DatabaseHost {
        id: DatabaseHostId::new(),
        name,
        host: body.host.unwrap_or_default(),
        port: body.port.unwrap_or(0),
        username: body.username.unwrap_or_default(),
        // The credential is required on create; write-only forever
        // after (nothing reads it back through this API).
        password: body.password.unwrap_or_default(),
        max_databases: body.max_databases,
        node_id,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    host.validate()
        .map_err(|e| ApiError::validation(e.to_string(), "host"))?;
    db_or_500(&state, {
        let host = host.clone();
        move |p| p.put_database_host(&host)
    })
    .await?;
    audit(
        &state,
        Some(admin.id.to_string()),
        "database.host.create",
        None,
        serde_json::json!({ "name": host.name, "host": format!("{}:{}", host.host, host.port) }),
    )
    .await;
    let mut response = Json(host_json(&host, 0)).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

/// `GET /api/v1/hosts/{hid}` — admin: one host's detail.
pub async fn get_host(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(hid): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&caller)?;
    let host_id = parse_uuid::<DatabaseHostId>(&hid, "hid")?;
    let host = db_or_500(&state, move |p| p.get_database_host(&host_id))
        .await?
        .ok_or_else(|| ApiError::not_found("database host", hid.clone()))?;
    let count = db_or_500(&state, {
        let id = host.id;
        move |p| p.count_databases_by_host(&id)
    })
    .await?;
    Ok(Json(host_json(&host, count)))
}

/// `PATCH /api/v1/hosts/{hid}` — admin: update a host. The password
/// updates only when supplied (a PATCH that omits it keeps the
/// stored credential — the write-only discipline).
pub async fn update_host(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(hid): Path<String>,
    ApiJson(body): ApiJson<HostBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let admin = require_admin(&caller)?;
    let host_id = parse_uuid::<DatabaseHostId>(&hid, "hid")?;
    let existing = db_or_500(&state, move |p| p.get_database_host(&host_id))
        .await?
        .ok_or_else(|| ApiError::not_found("database host", hid.clone()))?;

    let mut host = existing.clone();
    if let Some(name) = body.name {
        // A rename may collide with another host's name: 409.
        if name != existing.name {
            let probe = name.clone();
            let clash = db_or_500(&state, move |p| p.get_database_host_by_name(&probe)).await?;
            if clash.is_some() {
                return Err(ApiError::Conflict(format!(
                    "a database host named {:?} already exists",
                    name
                )));
            }
        }
        host.name = name;
    }
    if let Some(address) = body.host {
        host.host = address;
    }
    if let Some(port) = body.port {
        host.port = port;
    }
    if let Some(username) = body.username {
        host.username = username;
    }
    if let Some(password) = body.password {
        host.password = password;
    }
    if let Some(max) = body.max_databases {
        host.max_databases = Some(max);
    }
    if body.node_id.is_some() {
        host.node_id = resolve_node_id(&state, body.node_id.as_deref()).await?;
    }
    host.updated_at = Utc::now();
    host.validate()
        .map_err(|e| ApiError::validation(e.to_string(), "host"))?;
    db_or_500(&state, {
        let host = host.clone();
        move |p| p.put_database_host(&host)
    })
    .await?;
    audit(
        &state,
        Some(admin.id.to_string()),
        "database.host.update",
        None,
        serde_json::json!({ "name": host.name }),
    )
    .await;
    let count = db_or_500(&state, {
        let id = host.id;
        move |p| p.count_databases_by_host(&id)
    })
    .await?;
    Ok(Json(host_json(&host, count)))
}

/// `DELETE /api/v1/hosts/{hid}` — admin: remove a host. Refused
/// with a structured 409 (naming the live count) while databases
/// reference it — the store's plain FK is the backstop, and the
/// check-first refusal is the operator-facing contract (credentials
/// and connection records are never silently cascaded away).
pub async fn delete_host(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(hid): Path<String>,
) -> Result<StatusCode, ApiError> {
    let admin = require_admin(&caller)?;
    let host_id = parse_uuid::<DatabaseHostId>(&hid, "hid")?;
    let existing = db_or_500(&state, move |p| p.get_database_host(&host_id))
        .await?
        .ok_or_else(|| ApiError::not_found("database host", hid.clone()))?;

    let count = db_or_500(&state, {
        let id = existing.id;
        move |p| p.count_databases_by_host(&id)
    })
    .await?;
    if count > 0 {
        return Err(ApiError::Conflict(format!(
            "database host {:?} still carries {} database(s); delete them first",
            existing.name, count
        )));
    }
    db_or_500(&state, move |p| p.delete_database_host(&host_id)).await?;
    audit(
        &state,
        Some(admin.id.to_string()),
        "database.host.delete",
        None,
        serde_json::json!({ "name": existing.name }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ── The per-instance database family ───────────────────────────────────

/// `GET /api/v1/instances/{id}/databases` — `database.read`: the
/// instance's rows with connection info, never passwords.
pub async fn list_databases(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::DatabaseRead])?;
    let databases = db_or_500(&state, {
        let id = instance_id;
        move |p| p.list_managed_databases_by_instance(&id)
    })
    .await?;
    let mut data = Vec::with_capacity(databases.len());
    for database in &databases {
        let host = db_or_500(&state, {
            let id = database.host_id;
            move |p| p.get_database_host(&id)
        })
        .await?;
        // A host deleted between the list and the join is a hole in
        // the connection info, not a failed list: the row's own
        // fields are complete; the connections object degrades to
        // the host id alone (the store's FK keeps this impossible in
        // practice — the refusal above is the only delete path).
        let host = host.unwrap_or_else(|| DatabaseHost {
            id: database.host_id,
            name: String::new(),
            host: String::new(),
            port: 0,
            username: String::new(),
            password: String::new(),
            max_databases: None,
            node_id: None,
            created_at: database.created_at,
            updated_at: database.updated_at,
        });
        data.push(database_json(database, &host, None));
    }
    Ok(Json(serde_json::json!({ "data": data })))
}

/// The create body: what the caller chooses. Everything else — the
/// namespace prefix, the username, the password — is derived
/// server-side, exactly as the Panel's create path derives it.
#[derive(Debug, Default, Deserialize)]
pub struct DatabaseBody {
    /// The user-chosen part of the database name (3-38 chars of
    /// alpha_dash; the `s{short}_` prefix is added server-side).
    #[serde(default)]
    pub name: String,
    /// The allowed connection source (default `%` = any).
    #[serde(default)]
    pub remote: Option<String>,
    /// The host to provision on (default: the node-tied host, else
    /// any host — the Panel's DeployServerDatabaseService rule).
    #[serde(default)]
    pub host_id: Option<String>,
    /// The per-user connection cap (0 = unlimited).
    #[serde(default)]
    pub max_connections: Option<u32>,
}

/// `POST /api/v1/instances/{id}/databases` — `database.create`.
///
/// The gates, in order: the feature/cap limit on the instance row,
/// host selection (explicit or auto), the host's own cap, the
/// derived name's global uniqueness, then the row's validation.
/// Each refusal is structured and names its reason.
pub async fn create_database(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<DatabaseBody>,
) -> Result<Response, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::DatabaseCreate])?;

    // The instance's limit is the feature switch AND the cap (None =
    // feature off — the desktop model's one-knob posture).
    let limit = instance.database_limit.ok_or_else(|| {
        ApiError::Conflict(
            "managed databases are not enabled for this instance (its database limit is unset)"
                .to_owned(),
        )
    })?;
    let count = db_or_500(&state, {
        let id = instance_id;
        move |p| p.count_managed_databases_by_instance(&id)
    })
    .await?;
    if count >= u64::from(limit) {
        return Err(ApiError::Conflict(format!(
            "the instance's database limit ({limit}) is reached ({count} of {limit})"
        )));
    }

    // Host selection: the explicit host, else the node-tied host,
    // else any host (the Panel's rule, one host is the alpha shape).
    let host = select_host(&state, body.host_id.as_deref(), instance.node_id).await?;

    // The host's own cap (None = uncapped).
    if let Some(cap) = host.max_databases {
        let host_count = db_or_500(&state, {
            let id = host.id;
            move |p| p.count_databases_by_host(&id)
        })
        .await?;
        if host_count >= u64::from(cap) {
            return Err(ApiError::Conflict(format!(
                "database host {:?} is at its capacity ({host_count} of {cap})",
                host.name
            )));
        }
    }

    // Derive the row: the Panel's conventions on ZIM's UUID rows.
    let name = zim_engine_domain::database::derive_database_name(&instance, &body.name);
    let clash = db_or_500(&state, {
        let name = name.clone();
        move |p| p.get_managed_database_by_name(&name)
    })
    .await?;
    if clash.is_some() {
        return Err(ApiError::Conflict(format!(
            "a database named {:?} already exists (names are unique across every host)",
            name
        )));
    }

    let database = ManagedDatabase {
        id: ManagedDatabaseId::new(),
        instance_id,
        host_id: host.id,
        name,
        username: zim_engine_domain::database::derive_username(&instance),
        password: zim_engine_domain::database::generate_password(),
        remote: body.remote.unwrap_or_else(|| "%".to_owned()),
        max_connections: body.max_connections.unwrap_or(0),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    database
        .validate()
        .map_err(|e| ApiError::validation(e.to_string(), "database"))?;
    db_or_500(&state, {
        let database = database.clone();
        move |p| p.put_managed_database(&database)
    })
    .await?;
    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "database.create",
        Some(instance_id),
        serde_json::json!({ "name": database.name, "host": host.name }),
    )
    .await;

    // The ONE response that carries the password: the caller just
    // created the credential (`database.create` is the gate) and
    // needs the initial value to configure the workload.
    let mut response =
        Json(database_json(&database, &host, Some(&database.password))).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

/// `GET /api/v1/instances/{id}/databases/{dbid}` — `database.read`:
/// the row with connection info, never the password (the gated
/// password endpoint is the only read path for it).
pub async fn get_database(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, dbid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let database_id = parse_uuid::<ManagedDatabaseId>(&dbid, "dbid")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::DatabaseRead])?;
    let (database, host) =
        load_database_and_host(&state, &instance_id, &database_id, &dbid).await?;
    Ok(Json(database_json(&database, &host, None)))
}

/// `GET /api/v1/instances/{id}/databases/{dbid}/password` — the
/// `database.view_password` grant's own endpoint (the Panel keeps
/// the grant separate from `database.read` for exactly this read;
/// ZIM preserved the vocabulary and this is where it is enforced).
pub async fn get_database_password(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, dbid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let database_id = parse_uuid::<ManagedDatabaseId>(&dbid, "dbid")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::DatabaseViewPassword])?;
    let (database, _host) =
        load_database_and_host(&state, &instance_id, &database_id, &dbid).await?;
    Ok(Json(serde_json::json!({ "password": database.password })))
}

/// `POST /api/v1/instances/{id}/databases/{dbid}/rotate-password` —
/// `database.update`: a fresh generated password; the response
/// carries it once (the rotator just reset the credential and needs
/// the new value — the same one-time-reveal argument as create).
pub async fn rotate_database_password(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, dbid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let database_id = parse_uuid::<ManagedDatabaseId>(&dbid, "dbid")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::DatabaseUpdate])?;
    let (mut database, host) =
        load_database_and_host(&state, &instance_id, &database_id, &dbid).await?;

    database.password = zim_engine_domain::database::generate_password();
    database.updated_at = Utc::now();
    db_or_500(&state, {
        let database = database.clone();
        move |p| p.put_managed_database(&database)
    })
    .await?;
    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "database.rotate_password",
        Some(instance_id),
        serde_json::json!({ "name": database.name }),
    )
    .await;
    Ok(Json(database_json(
        &database,
        &host,
        Some(&database.password),
    )))
}

/// `DELETE /api/v1/instances/{id}/databases/{dbid}` —
/// `database.delete`.
pub async fn delete_database(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, dbid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let database_id = parse_uuid::<ManagedDatabaseId>(&dbid, "dbid")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::DatabaseDelete])?;
    let (database, _host) =
        load_database_and_host(&state, &instance_id, &database_id, &dbid).await?;
    db_or_500(&state, move |p| p.delete_managed_database(&database_id)).await?;
    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "database.delete",
        Some(instance_id),
        serde_json::json!({ "name": database.name }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ── Shared helpers ─────────────────────────────────────────────────────

/// Resolve an optional node reference: `None` in the body means
/// "no tie" (not "keep the old one" — PATCH sets the tie only when
/// the field is present, and `null`/absence at create means
/// untied). A present-but-malformed id is a 422; a well-formed id
/// that matches no node is a 404.
async fn resolve_node_id(state: &AppState, raw: Option<&str>) -> Result<Option<NodeId>, ApiError> {
    match raw {
        None => Ok(None),
        Some(text) => {
            let node_id = parse_uuid::<NodeId>(text, "node_id")?;
            let found = db_or_500(state, move |p| p.get_node(&node_id)).await?;
            if found.is_none() {
                return Err(ApiError::not_found("node", text.to_owned()));
            }
            Ok(Some(node_id))
        }
    }
}

/// Host selection for a create: the explicit host when the caller
/// named one (404 when it does not exist), else a node-tied host
/// first, else any host (the Panel's DeployServerDatabaseService
/// rule; with one host in the alpha shape the rules collapse).
/// No hosts at all is the panel's NoSuitableDatabaseHostException —
/// a 409 that names the condition.
async fn select_host(
    state: &AppState,
    explicit: Option<&str>,
    node: NodeId,
) -> Result<DatabaseHost, ApiError> {
    if let Some(text) = explicit {
        let host_id = parse_uuid::<DatabaseHostId>(text, "host_id")?;
        return db_or_500(state, move |p| p.get_database_host(&host_id))
            .await?
            .ok_or_else(|| ApiError::not_found("database host", text.to_owned()));
    }
    let hosts = db_or_500(state, move |p| p.list_database_hosts()).await?;
    if hosts.is_empty() {
        return Err(ApiError::Conflict(
            "no database hosts are registered; an admin must add one first (POST /api/v1/hosts)"
                .to_owned(),
        ));
    }
    // Prefer a host tied to the instance's node; fall back to the
    // first untied host (node-tied hosts serve that node's
    // instances; an untied host serves any).
    let node_tied = hosts.iter().find(|host| host.node_id == Some(node));
    Ok(node_tied.cloned().unwrap_or_else(|| {
        hosts
            .iter()
            .find(|host| host.node_id.is_none())
            .cloned()
            .unwrap_or_else(|| hosts[0].clone())
    }))
}

/// Load one database scoped to one instance (the uniform-404 rule:
/// another instance's database id is indistinguishable from a
/// missing one), joined with its host for the connection info.
async fn load_database_and_host(
    state: &AppState,
    instance_id: &InstanceId,
    database_id: &ManagedDatabaseId,
    dbid: &str,
) -> Result<(ManagedDatabase, DatabaseHost), ApiError> {
    let database = db_or_500(state, {
        let id = *database_id;
        move |p| p.get_managed_database(&id)
    })
    .await?
    .filter(|database| database.instance_id == *instance_id)
    .ok_or_else(|| ApiError::not_found("database", dbid.to_owned()))?;
    let host = db_or_500(state, {
        let id = database.host_id;
        move |p| p.get_database_host(&id)
    })
    .await?
    .ok_or_else(|| {
        ApiError::internal(format!(
            "database {} references host {} which does not exist (the store's FK refuses this)",
            database.id, database.host_id
        ))
    })?;
    Ok((database, host))
}
