//! `/api/v1/instances/*` — instance CRUD and sub-resources (spec
//! §10.3, §10.4 allocations, §10.5 backup reads, §10.6 schedules,
//! §10.7 subusers, §10.9 transfer status), plus the 501 stubs for
//! everything that needs the runtime abstraction (M6), streaming
//! (M7), or the job system (M8).
//!
//! The `/api/v1/workloads` surface (spec §10.15) aliases these
//! handlers per the AD-19 back-compat rules in §10.17: same
//! handlers, same responses, shared id space.

use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::Utc;
use serde::Deserialize;

use crate::error::ApiError;
use crate::etag::{instance_etag, require_if_match};
use crate::handlers::{
    audit, check_grants, conflict_on_backend, load_instance_for_caller, parse_grants, parse_uuid,
    query_params, require_admin, require_json_content_type, require_user, subuser_json, ApiJson,
};
use crate::pagination::{page_envelope, PageParams};
use crate::state::{db_or_500, AppState, Caller};
use zim_engine_domain::{
    AllocationId, CommandTemplate, DockerImage, EggId, Instance, InstanceId, InstanceStatus,
    NodeId, Permission, PermissionGrant, PermissionSet, ResourceLimits, Schedule, ScheduleAction,
    ScheduleId, UserId,
};

// ── Listing ───────────────────────────────────────────────────────

/// Query shape for `GET /instances` / `GET /workloads`.
#[derive(Debug, Default, Deserialize)]
struct ListInstancesQuery {
    page: Option<u32>,
    per_page: Option<u32>,
    /// `filter[state]` — comma-separated snake_case statuses (OR).
    filter_state: Option<String>,
    /// `sort` — `created_at`, `-created_at`, `name`, `-name`.
    sort: Option<String>,
}

/// Load every instance the caller may see (admin: all; member:
/// owned ∪ permission rows), pre-sort and filter.
async fn visible_instances(state: &AppState, caller: &Caller) -> Result<Vec<Instance>, ApiError> {
    let user = require_user(caller)?;
    let instances = if caller.is_admin() {
        // All instances = per-node concatenation (there is no
        // list-all port method; every instance sits on a node).
        let nodes = db_or_500(state, |p| p.list_nodes()).await?;
        let mut all = Vec::new();
        for node in nodes {
            let on_node = db_or_500(state, {
                let id = node.id;
                move |p| p.list_instances_by_node(&id)
            })
            .await?;
            all.extend(on_node);
        }
        all
    } else {
        let owned = db_or_500(state, {
            let uid = user.id;
            move |p| p.list_instances_by_owner(&uid)
        })
        .await?;
        let rows = db_or_500(state, {
            let uid = user.id;
            move |p| p.list_permissions_by_user(&uid)
        })
        .await?;
        let mut seen = owned;
        for row in rows {
            let instance = db_or_500(state, {
                let id = row.instance_id;
                move |p| p.get_instance(&id)
            })
            .await?;
            if let Some(instance) = instance {
                if !seen.iter().any(|i| i.id == instance.id) {
                    seen.push(instance);
                }
            }
        }
        seen
    };
    Ok(instances)
}

/// `GET /api/v1/instances` — page-based, filterable by state.
pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    RawQuery(query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_user(&caller)?;
    let q: ListInstancesQuery = query_params(query.as_deref().unwrap_or(""))?;
    let page = PageParams::from_query(q.page, q.per_page)?;

    let mut instances = visible_instances(&state, &caller).await?;

    // filter[state]=running,starting — OR semantics (§7).
    if let Some(states) = q.filter_state.as_deref() {
        let wanted: Result<Vec<InstanceStatus>, _> = states
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                serde_json::from_value::<InstanceStatus>(serde_json::Value::String(
                    s.trim().to_owned(),
                ))
                .map_err(|_| {
                    ApiError::validation(
                        format!("unknown instance status {s:?} in filter[state]"),
                        "filter[state]",
                    )
                })
            })
            .collect();
        let wanted = wanted?;
        instances.retain(|i| wanted.contains(&i.status));
    }

    // sort (§7): `-field` descending.
    match q.sort.as_deref() {
        None | Some("created_at") => instances.sort_by_key(|i| i.created_at),
        Some("-created_at") => instances.sort_by_key(|i| std::cmp::Reverse(i.created_at)),
        Some("name") => instances.sort_by(|a, b| a.name.cmp(&b.name)),
        Some("-name") => instances.sort_by(|a, b| b.name.cmp(&a.name)),
        Some(other) => {
            return Err(ApiError::validation(
                format!("unsupported sort field {other:?} (created_at, name)"),
                "sort",
            ));
        }
    }

    let total = instances.len();
    let (start, end) = page.bounds(total);
    let data: Vec<_> = instances[start..end]
        .iter()
        .map(|i| instance_json(i, None))
        .collect();
    Ok(Json(page_envelope(page, total, data)))
}

/// The instance JSON shape (domain serialization plus the API's
/// `object` discriminator; the ETag rides the response header).
fn instance_json(instance: &Instance, etag: Option<&str>) -> serde_json::Value {
    let mut value = serde_json::to_value(instance).unwrap_or(serde_json::Value::Null);
    if let Some(map) = value.as_object_mut() {
        map.insert("object".to_owned(), serde_json::json!("instance"));
        if let Some(etag) = etag {
            map.insert("etag".to_owned(), serde_json::json!(etag));
        }
    }
    value
}

/// `GET /api/v1/instances/{id}` — with `ETag` response header (§8).
pub async fn get(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, _ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    let etag = instance_etag(&instance);
    let mut response = Json(instance_json(&instance, Some(&etag))).into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&etag) {
        response.headers_mut().insert(axum::http::header::ETAG, v);
    }
    Ok(response)
}

// ── Creation / mutation ───────────────────────────────────────────

/// `POST /api/v1/instances` (admin) / `POST /api/v1/workloads`
/// (kind defaults to `game_server`; other kinds are 501 per §10.15).
#[derive(Debug, Default, Deserialize)]
pub struct CreateInstanceRequest {
    /// Display name.
    pub name: String,
    /// Target node id.
    pub node_id: String,
    /// Owning user id (defaults to the calling admin).
    #[serde(default)]
    pub owner_id: Option<String>,
    /// Egg id (defaults to a generated placeholder; eggs land in M9).
    #[serde(default)]
    pub egg_id: Option<String>,
    /// Primary allocation id (defaults to the first free allocation
    /// on the node).
    #[serde(default)]
    pub allocation_id: Option<u64>,
    /// Resource limits (defaults: unlimited).
    #[serde(default)]
    pub limits: Option<ResourceLimits>,
    /// Startup command template.
    #[serde(default)]
    pub startup: Option<String>,
    /// Docker image reference.
    #[serde(default)]
    pub image: Option<String>,
    /// Free-form description.
    #[serde(default)]
    pub description: Option<String>,
    /// Legacy Pterodactyl external id.
    #[serde(default)]
    pub external_id: Option<String>,
    /// Self-assignable allocation budget (None = admin unlimited).
    #[serde(default)]
    pub allocation_limit: Option<u32>,
    /// Database budget (None = feature off).
    #[serde(default)]
    pub database_limit: Option<u32>,
    /// Backup retention budget (0 = disabled).
    #[serde(default)]
    pub backup_limit: Option<u32>,
    /// Crash-loop protection switch.
    #[serde(default)]
    pub crash_detection_enabled: Option<bool>,
    /// Workload kind marker for the /workloads alias (§10.15).
    #[serde(default)]
    pub kind: Option<String>,
}

/// `POST /api/v1/instances` — admin-only creation.
pub async fn create(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<CreateInstanceRequest>,
) -> Result<Response, ApiError> {
    let admin = require_admin(&caller)?;
    require_json_content_type(&headers)?;

    // The workloads alias accepts kind=game_server (the only
    // modeled kind in M5) or absent.
    if let Some(kind) = request.kind.as_deref() {
        if kind != "game_server" {
            return Err(ApiError::NotImplemented {
                feature: "workload kinds other than game_server",
                milestone: "M16 (workload integration SDK)",
            });
        }
    }

    let node_id = parse_uuid::<NodeId>(&request.node_id, "node_id")?;
    let node = db_or_500(&state, move |p| p.get_node(&node_id))
        .await?
        .ok_or_else(|| ApiError::not_found("node", request.node_id.clone()))?;

    let owner_id = match request.owner_id.as_deref() {
        None => admin.id,
        Some(raw) => parse_uuid::<UserId>(raw, "owner_id")?,
    };
    let owner = db_or_500(&state, move |p| p.get_user(&owner_id))
        .await?
        .ok_or_else(|| ApiError::not_found("user", owner_id.to_string()))?;

    // Allocation: explicit (must be free and on the node) or
    // auto-picked from the node's free pool.
    let allocations = db_or_500(&state, move |p| p.list_allocations_by_node(&node.id)).await?;
    let allocation = match request.allocation_id {
        Some(id) => {
            let found = allocations
                .iter()
                .find(|a| a.id == AllocationId(id))
                .ok_or_else(|| ApiError::not_found("allocation", id))?;
            if found.assigned_to.is_some() {
                return Err(ApiError::Conflict(format!(
                    "allocation {id} is already assigned to another instance"
                )));
            }
            if found.node_id != node.id {
                return Err(ApiError::Conflict(format!(
                    "allocation {id} belongs to a different node"
                )));
            }
            found.clone()
        }
        None => allocations
            .iter()
            .find(|a| a.assigned_to.is_none())
            .cloned()
            .ok_or_else(|| {
                ApiError::Conflict(format!(
                    "node {:?} has no free allocation; create one first (POST \
                     /nodes/{{id}}/allocations)",
                    node.name
                ))
            })?,
    };

    let image = match request.image.as_deref() {
        None => "ghcr.io/parkervcp/yolks:latest"
            .parse::<DockerImage>()
            .expect("static reference parses"),
        Some(raw) => raw
            .parse::<DockerImage>()
            .map_err(|e| ApiError::validation(format!("invalid image reference: {e}"), "image"))?,
    };
    let (uuid, uuid_short) = Instance::legacy_uuid_pair();
    let now = Utc::now();
    let instance = Instance {
        id: InstanceId::new(),
        external_id: request.external_id,
        uuid,
        uuid_short,
        node_id: node.id,
        name: request.name,
        description: request.description.unwrap_or_default(),
        status: InstanceStatus::Offline, // desired state; observed state needs the runtime (M6)
        skip_egg_scripts: false,
        owner_id: owner.id,
        limits: request.limits.unwrap_or_default(),
        allocation_id: allocation.id,
        egg_id: request
            .egg_id
            .as_deref()
            .map(|raw| parse_uuid::<EggId>(raw, "egg_id"))
            .transpose()?
            .unwrap_or_else(EggId::new),
        startup: CommandTemplate(request.startup.unwrap_or_else(|| "./run.sh".to_owned())),
        image,
        allocation_limit: request.allocation_limit,
        database_limit: request.database_limit,
        backup_limit: request.backup_limit.unwrap_or(0),
        crash_detection_enabled: request.crash_detection_enabled.unwrap_or(true),
        installed_at: None,
        created_at: now,
        updated_at: now,
    };
    instance
        .validate()
        .map_err(|e| ApiError::validation(e.to_string(), "instance"))?;

    // Store the instance, then claim the allocation (the allocation
    // row's assigned_to FK points at the instance, so the instance
    // must exist first).
    let mut claimed = allocation.clone();
    claimed.assigned_to = Some(instance.id);
    let row = instance.clone();
    db_or_500(&state, move |p| {
        p.put_instance(&row)?;
        p.put_allocation(&claimed)
    })
    .await?;

    audit(
        &state,
        Some(admin.id.to_string()),
        "instance.create",
        Some(instance.id),
        serde_json::json!({ "name": instance.name, "node": node.name }),
    )
    .await;

    let mut response = Json(instance_json(&instance, None)).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

/// `PATCH /api/v1/instances/{id}` — `AdminInstance` or
/// `settings.update` grants (spec §10.3), guarded by `If-Match` (§8).
#[derive(Debug, Default, Deserialize)]
pub struct PatchInstanceRequest {
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Free-form description.
    #[serde(default)]
    pub description: Option<String>,
    /// Resource limits.
    #[serde(default)]
    pub limits: Option<ResourceLimits>,
    /// Startup command template.
    #[serde(default)]
    pub startup: Option<String>,
    /// Docker image reference.
    #[serde(default)]
    pub image: Option<String>,
    /// Self-assignable allocation budget.
    #[serde(default)]
    pub allocation_limit: Option<u32>,
    /// Database budget.
    #[serde(default)]
    pub database_limit: Option<u32>,
    /// Backup retention budget.
    #[serde(default)]
    pub backup_limit: Option<u32>,
    /// Crash-loop protection switch.
    #[serde(default)]
    pub crash_detection_enabled: Option<bool>,
    /// Skip egg install scripts on next install.
    #[serde(default)]
    pub skip_egg_scripts: Option<bool>,
    /// Legacy external id.
    #[serde(default)]
    pub external_id: Option<String>,
}

/// `PATCH /api/v1/instances/{id}`.
pub async fn patch(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<PatchInstanceRequest>,
) -> Result<Response, ApiError> {
    require_json_content_type(&headers)?;
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    // Spec §10.3: "AdminInstance or SettingsUpdate" — either grant
    // suffices (an inclusive OR, not a joint requirement).
    check_grants(&ctx, &[PermissionGrant::AdminInstance])
        .or_else(|_| check_grants(&ctx, &[PermissionGrant::SettingsUpdate]))?;

    let etag = instance_etag(&instance);
    require_if_match(&headers, &etag)?;

    let mut updated = instance.clone();
    if let Some(name) = request.name {
        updated.name = name;
    }
    if let Some(description) = request.description {
        updated.description = description;
    }
    if let Some(limits) = request.limits {
        updated.limits = limits;
    }
    if let Some(startup) = request.startup {
        updated.startup = CommandTemplate(startup);
    }
    if let Some(image) = request.image {
        updated.image = image
            .parse::<DockerImage>()
            .map_err(|e| ApiError::validation(format!("invalid image reference: {e}"), "image"))?;
    }
    if let Some(allocation_limit) = request.allocation_limit {
        updated.allocation_limit = Some(allocation_limit);
    }
    if let Some(database_limit) = request.database_limit {
        updated.database_limit = Some(database_limit);
    }
    if let Some(backup_limit) = request.backup_limit {
        updated.backup_limit = backup_limit;
    }
    if let Some(flag) = request.crash_detection_enabled {
        updated.crash_detection_enabled = flag;
    }
    if let Some(flag) = request.skip_egg_scripts {
        updated.skip_egg_scripts = flag;
    }
    if let Some(external_id) = request.external_id {
        updated.external_id = Some(external_id);
    }
    updated
        .validate()
        .map_err(|e| ApiError::validation(e.to_string(), "instance"))?;
    updated.updated_at = Utc::now();

    let row = updated.clone();
    db_or_500(&state, move |p| p.put_instance(&row)).await?;
    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.update",
        Some(instance_id),
        serde_json::json!({ "name": updated.name }),
    )
    .await;

    let new_etag = instance_etag(&updated);
    let mut response = Json(instance_json(&updated, Some(&new_etag))).into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&new_etag) {
        response.headers_mut().insert(axum::http::header::ETAG, v);
    }
    Ok(response)
}

/// `DELETE /api/v1/instances/{id}` — `AdminInstance` grant (spec
/// §10.3), guarded by `If-Match`; unassigns the allocation first,
/// then deletes (child rows cascade in the schema).
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_json_content_type(&headers)?;
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::AdminInstance])?;

    let etag = instance_etag(&instance);
    require_if_match(&headers, &etag)?;

    // Release the primary allocation (plain FK, no cascade).
    let primary = instance.allocation_id;
    db_or_500(&state, move |p| {
        if let Some(found) = p.get_allocation(&primary)? {
            let mut freed = found;
            freed.assigned_to = None;
            p.put_allocation(&freed)?;
        }
        Ok(())
    })
    .await?;

    let row_id = instance.id;
    crate::state::db(&state, move |p| p.delete_instance(&row_id))
        .await
        .map_err(|e| {
            conflict_on_backend(
                e,
                "instance still has dependent rows; delete its schedules first",
            )
        })?;
    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.delete",
        Some(instance_id),
        serde_json::json!({ "name": instance.name }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/instances/{id}/state` — the small observed-state
/// response. Persisted desired status only in M5; observed liveness
/// arrives with the runtime (M6) per lifecycle invariant 1.
pub async fn state(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, _ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    Ok(Json(serde_json::json!({
        "id": instance.id,
        "status": instance.status,
        "updated_at": instance.updated_at,
        "observed_by": "persistence",
    })))
}

// ── Subusers (§10.7) ──────────────────────────────────────────────

/// `GET /api/v1/instances/{id}/subusers` — `user.read` grant.
pub async fn list_subusers(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::UserRead])?;
    let rows = db_or_500(&state, {
        let id = instance_id;
        move |p| p.list_permissions_by_instance(&id)
    })
    .await?;
    let mut data = Vec::with_capacity(rows.len());
    for row in rows {
        let user = db_or_500(&state, {
            let uid = row.user_id;
            move |p| p.get_user(&uid)
        })
        .await?;
        if let Some(user) = user {
            data.push(subuser_json(&row, &user));
        }
    }
    Ok(Json(serde_json::json!({ "data": data })))
}

/// `POST /api/v1/instances/{id}/subusers` — `user.create` grant.
#[derive(Debug, Default, Deserialize)]
pub struct CreateSubuserRequest {
    /// The user to add (must already exist).
    #[serde(default)]
    pub user_id: String,
    /// Grants to bestow (dotted names).
    #[serde(default)]
    pub grants: Vec<String>,
}

/// `POST /api/v1/instances/{id}/subusers`.
pub async fn add_subuser(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<CreateSubuserRequest>,
) -> Result<Response, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::UserCreate])?;

    let target = parse_uuid::<UserId>(&request.user_id, "user_id")?;
    let user = db_or_500(&state, move |p| p.get_user(&target))
        .await?
        .ok_or_else(|| ApiError::not_found("user", request.user_id.clone()))?;
    if user.id == ctx.user_id {
        return Err(ApiError::Conflict(
            "the instance owner/caller is already implicitly authorized".to_owned(),
        ));
    }

    let grants = parse_grants(&request.grants)?;
    let mut set = PermissionSet::new();
    for grant in grants {
        set.grant(grant);
    }
    let permission = Permission {
        user_id: user.id,
        instance_id,
        grants: set,
    };
    db_or_500(&state, {
        let permission = permission.clone();
        move |p| p.put_permission(&permission)
    })
    .await?;
    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.subuser.add",
        Some(instance_id),
        serde_json::json!({ "subuser": user.id.to_string() }),
    )
    .await;
    let mut response = Json(subuser_json(&permission, &user)).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

/// `PATCH /api/v1/instances/{id}/subusers/{uid}` — `user.update`
/// grant; replaces the grant set.
pub async fn update_subuser(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, uid)): Path<(String, String)>,
    ApiJson(request): ApiJson<CreateSubuserRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let target = parse_uuid::<UserId>(&uid, "uid")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::UserUpdate])?;

    let existing = db_or_500(&state, {
        let id = instance_id;
        move |p| p.get_permission(&target, &id)
    })
    .await?
    .ok_or_else(|| ApiError::not_found("subuser", uid.clone()))?;

    let grants = parse_grants(&request.grants)?;
    let mut set = PermissionSet::new();
    for grant in grants {
        set.grant(grant);
    }
    let permission = Permission {
        user_id: target,
        instance_id,
        grants: set,
    };
    db_or_500(&state, {
        let permission = permission.clone();
        move |p| p.put_permission(&permission)
    })
    .await?;
    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.subuser.update",
        Some(instance_id),
        serde_json::json!({
            "subuser": target.to_string(),
            "was": existing.grants.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        }),
    )
    .await;
    let user = db_or_500(&state, move |p| p.get_user(&target)).await?;
    Ok(Json(match user {
        Some(user) => subuser_json(&permission, &user),
        None => serde_json::to_value(&permission).unwrap_or_default(),
    }))
}

/// `DELETE /api/v1/instances/{id}/subusers/{uid}` — `user.delete`
/// grant.
pub async fn remove_subuser(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, uid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let target = parse_uuid::<UserId>(&uid, "uid")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::UserDelete])?;

    db_or_500(&state, {
        let id = instance_id;
        move |p| p.delete_permission(&target, &id)
    })
    .await?;
    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.subuser.remove",
        Some(instance_id),
        serde_json::json!({ "subuser": target.to_string() }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ── Instance allocations (§10.4) ───────────────────────────────────

/// `GET /api/v1/instances/{id}/allocations` — `allocation.read`.
pub async fn list_allocations(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::AllocationRead])?;
    let allocation = db_or_500(&state, {
        let id = instance.id;
        move |p| p.find_allocation_by_instance(&id)
    })
    .await?;
    Ok(Json(serde_json::json!({ "data": allocation })))
}

/// `POST /api/v1/instances/{id}/allocations` — `allocation.create`;
/// assigns a free allocation from the instance's node (M5: the
/// primary allocation swap — the multi-allocation model the
/// `allocation_limit` field hints at arrives with the runtime
/// milestone).
#[derive(Debug, Default, Deserialize)]
pub struct AssignAllocationRequest {
    /// Allocation to claim (must be free and on the instance's node).
    #[serde(default)]
    pub allocation_id: u64,
}

/// `POST /api/v1/instances/{id}/allocations`.
pub async fn assign_allocation(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<AssignAllocationRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::AllocationCreate])?;

    let wanted = AllocationId(request.allocation_id);
    let allocation = db_or_500(&state, move |p| p.get_allocation(&wanted))
        .await?
        .ok_or_else(|| ApiError::not_found("allocation", request.allocation_id))?;
    if allocation.assigned_to.is_some() {
        return Err(ApiError::Conflict(format!(
            "allocation {} is already assigned",
            request.allocation_id
        )));
    }
    if allocation.node_id != instance.node_id {
        return Err(ApiError::Conflict(
            "allocation belongs to a different node than the instance".to_owned(),
        ));
    }

    // Release the old primary, claim the new one, and point the
    // instance at it (the swap must update both sides atomically).
    let old_id = instance.allocation_id;
    let new_id = allocation.id;
    let mut row = instance.clone();
    row.allocation_id = new_id;
    row.updated_at = Utc::now();
    db_or_500(&state, move |p| {
        if let Some(previous) = p.get_allocation(&old_id)? {
            let mut freed = previous;
            freed.assigned_to = None;
            p.put_allocation(&freed)?;
        }
        let mut claimed = p.get_allocation(&new_id)?.ok_or_else(|| {
            zim_engine_application::ports::PersistenceError::backend("allocation vanished")
        })?;
        claimed.assigned_to = Some(row.id);
        p.put_allocation(&claimed)?;
        p.put_instance(&row)
    })
    .await?;
    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.allocation.assign",
        Some(instance_id),
        serde_json::json!({ "allocation": request.allocation_id }),
    )
    .await;
    Ok(Json(serde_json::json!({ "data": allocation })))
}

/// `DELETE /api/v1/instances/{id}/allocations/{aid}` —
/// `allocation.delete` (unassign; the primary slot stays occupied
/// because the schema requires one — swap first).
pub async fn unassign_allocation(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, aid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let allocation_id: u64 = aid
        .parse()
        .map_err(|_| ApiError::validation("allocation ids are integers", "aid"))?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::AllocationDelete])?;

    if instance.allocation_id == AllocationId(allocation_id) {
        return Err(ApiError::Conflict(
            "cannot unassign the instance's primary allocation; assign a replacement first"
                .to_owned(),
        ));
    }
    db_or_500(&state, {
        let id = AllocationId(allocation_id);
        move |p| {
            if let Some(mut allocation) = p.get_allocation(&id)? {
                allocation.assigned_to = None;
                p.put_allocation(&allocation)?;
            }
            Ok(())
        }
    })
    .await?;
    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.allocation.unassign",
        Some(instance_id),
        serde_json::json!({ "allocation": allocation_id }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ── Schedules (§10.6) ─────────────────────────────────────────────

/// Serialize a schedule (domain serialization + `object` marker).
fn schedule_json(schedule: &Schedule) -> serde_json::Value {
    let mut value = serde_json::to_value(schedule).unwrap_or_default();
    if let Some(map) = value.as_object_mut() {
        map.insert("object".to_owned(), serde_json::json!("schedule"));
    }
    value
}

/// `GET /api/v1/instances/{id}/schedules` — `schedule.read`.
pub async fn list_schedules(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::ScheduleRead])?;
    let schedules = db_or_500(&state, {
        let id = instance_id;
        move |p| p.list_schedules_by_instance(&id)
    })
    .await?;
    let data: Vec<_> = schedules.iter().map(schedule_json).collect();
    Ok(Json(serde_json::json!({ "data": data })))
}

/// Schedule request body (create and update share the shape).
#[derive(Debug, Default, Deserialize)]
pub struct ScheduleBody {
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// 5-field cron expression.
    #[serde(default)]
    pub cron: String,
    /// Whether the schedule is active.
    #[serde(default)]
    pub is_active: Option<bool>,
    /// Only fire while the instance is Running.
    #[serde(default)]
    pub only_when_online: Option<bool>,
    /// Actions to run when it fires.
    #[serde(default)]
    pub actions: Vec<ScheduleAction>,
}

/// `POST /api/v1/instances/{id}/schedules` — `schedule.create`.
/// `next_run_at` stays null: the scheduler computes it (M9).
pub async fn create_schedule(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ScheduleBody>,
) -> Result<Response, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::ScheduleCreate])?;

    let schedule = Schedule {
        id: ScheduleId::new(),
        instance_id,
        name: body.name,
        cron: body.cron,
        next_run_at: None,
        last_run_at: None,
        is_active: body.is_active.unwrap_or(true),
        only_when_online: body.only_when_online.unwrap_or(false),
        actions: body.actions,
    };
    schedule
        .validate()
        .map_err(|e| ApiError::validation(e.to_string(), "schedule"))?;
    db_or_500(&state, {
        let schedule = schedule.clone();
        move |p| p.put_schedule(&schedule)
    })
    .await?;
    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "schedule.create",
        Some(instance_id),
        serde_json::json!({ "name": schedule.name }),
    )
    .await;
    let mut response = Json(schedule_json(&schedule)).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

/// `GET /api/v1/instances/{id}/schedules/{sid}` — `schedule.read`.
pub async fn get_schedule(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, sid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let schedule_id = parse_uuid::<ScheduleId>(&sid, "sid")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::ScheduleRead])?;
    let schedule = db_or_500(&state, {
        let id = schedule_id;
        move |p| p.get_schedule(&id)
    })
    .await?
    .filter(|s| s.instance_id == instance_id)
    .ok_or_else(|| ApiError::not_found("schedule", sid.clone()))?;
    Ok(Json(schedule_json(&schedule)))
}

/// `PATCH /api/v1/instances/{id}/schedules/{sid}` —
/// `schedule.update`.
pub async fn patch_schedule(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, sid)): Path<(String, String)>,
    ApiJson(body): ApiJson<ScheduleBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let schedule_id = parse_uuid::<ScheduleId>(&sid, "sid")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::ScheduleUpdate])?;
    let existing = db_or_500(&state, {
        let id = schedule_id;
        move |p| p.get_schedule(&id)
    })
    .await?
    .filter(|s| s.instance_id == instance_id)
    .ok_or_else(|| ApiError::not_found("schedule", sid.clone()))?;

    let mut updated = existing.clone();
    if !body.name.is_empty() {
        updated.name = body.name;
    }
    if !body.cron.is_empty() {
        updated.cron = body.cron;
    }
    if let Some(flag) = body.is_active {
        updated.is_active = flag;
    }
    if let Some(flag) = body.only_when_online {
        updated.only_when_online = flag;
    }
    if !body.actions.is_empty() {
        updated.actions = body.actions;
    }
    updated
        .validate()
        .map_err(|e| ApiError::validation(e.to_string(), "schedule"))?;
    db_or_500(&state, {
        let updated = updated.clone();
        move |p| p.put_schedule(&updated)
    })
    .await?;
    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "schedule.update",
        Some(instance_id),
        serde_json::json!({ "schedule": sid }),
    )
    .await;
    Ok(Json(schedule_json(&updated)))
}

/// `DELETE /api/v1/instances/{id}/schedules/{sid}` —
/// `schedule.delete`.
pub async fn delete_schedule(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, sid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let schedule_id = parse_uuid::<ScheduleId>(&sid, "sid")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::ScheduleDelete])?;
    // 404 when it belongs to another instance (uniformity).
    let existing = db_or_500(&state, {
        let id = schedule_id;
        move |p| p.get_schedule(&id)
    })
    .await?;
    match existing {
        Some(schedule) if schedule.instance_id == instance_id => {
            db_or_500(&state, move |p| p.delete_schedule(&schedule_id)).await?;
            audit(
                &state,
                Some(ctx.user_id.to_string()),
                "schedule.delete",
                Some(instance_id),
                serde_json::json!({ "schedule": sid }),
            )
            .await;
            Ok(StatusCode::NO_CONTENT)
        }
        _ => Err(ApiError::not_found("schedule", sid.clone())),
    }
}

// ── Backup reads + transfer status (§10.5 / §10.9) ────────────────

/// `GET /api/v1/instances/{id}/backups` — `backup.read` (empty until
/// the job system creates archives in M8).
pub async fn list_backups(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::BackupRead])?;
    let backups = db_or_500(&state, {
        let id = instance_id;
        move |p| p.list_backups_by_instance(&id)
    })
    .await?;
    Ok(Json(serde_json::json!({ "data": backups })))
}

/// `GET /api/v1/instances/{id}/backups/{bid}` — `backup.read`.
pub async fn get_backup(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, bid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let backup_id = parse_uuid::<zim_engine_domain::BackupId>(&bid, "bid")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::BackupRead])?;
    let backup = db_or_500(&state, {
        let id = backup_id;
        move |p| p.get_backup(&id)
    })
    .await?
    .filter(|b| b.instance_id == instance_id)
    .ok_or_else(|| ApiError::not_found("backup", bid.clone()))?;
    Ok(Json(serde_json::to_value(&backup).unwrap_or_default()))
}

/// `GET /api/v1/instances/{id}/transfer` — current transfer status
/// (404 when none is in flight, Pterodactyl-style).
pub async fn transfer_status(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (_instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::Transfer])?;
    let transfers = db_or_500(&state, {
        let id = instance_id;
        move |p| p.list_transfers_by_instance(&id)
    })
    .await?;
    match transfers.first() {
        Some(transfer) => Ok(Json(serde_json::to_value(transfer).unwrap_or_default())),
        None => Err(ApiError::not_found("transfer", "none in progress")),
    }
}

// ── 501 stubs (later milestones) ──────────────────────────────────

/// `POST /instances/{id}/start` — M6 (runtime abstraction).
pub async fn start_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "instance power operations (start)",
        milestone: "M6 (runtime abstraction + Docker runtime)",
    })
}

/// `POST /instances/{id}/stop` — M6.
pub async fn stop_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "instance power operations (stop)",
        milestone: "M6 (runtime abstraction + Docker runtime)",
    })
}

/// `POST /instances/{id}/restart` — M6.
pub async fn restart_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "instance power operations (restart)",
        milestone: "M6 (runtime abstraction + Docker runtime)",
    })
}

/// `POST /instances/{id}/kill` — M6.
pub async fn kill_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "instance power operations (kill)",
        milestone: "M6 (runtime abstraction + Docker runtime)",
    })
}

/// `POST /instances/{id}/install` — M8 (jobs).
pub async fn install_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "instance install/reinstall",
        milestone: "M8 (jobs) + M9 (egg importer)",
    })
}

/// `POST /instances/{id}/suspend` — M6.
pub async fn suspend_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "instance suspend",
        milestone: "M6 (runtime abstraction)",
    })
}

/// `POST /instances/{id}/transfer` — M8.
pub async fn transfer_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "initiating a node transfer",
        milestone: "M8 (jobs) — the transfer two-phase commit",
    })
}

/// `DELETE /instances/{id}/transfer` — M8.
pub async fn cancel_transfer_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "cancelling a node transfer",
        milestone: "M8 (jobs)",
    })
}

/// `GET /instances/{id}/console` — M7 (streaming).
pub async fn console_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "console buffer reads",
        milestone: "M7 (streaming) + M6 (runtime)",
    })
}

/// `POST /instances/{id}/console/input` — M7.
pub async fn console_input_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "console input",
        milestone: "M7 (streaming) + M6 (runtime)",
    })
}

/// `GET /instances/{id}/metrics` — M7.
pub async fn metrics_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "metrics queries",
        milestone: "M7 (streaming + metrics)",
    })
}

/// The file-API family — M6 (`FilesystemCapable` runtime).
pub async fn files_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "the instance file API",
        milestone: "M6 (runtime filesystem capability)",
    })
}

/// Backup mutation family — M8 (job-driven).
pub async fn backup_write_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "backup creation / restore / deletion / locking",
        milestone: "M8 (jobs + backup archive handling)",
    })
}

/// `POST /instances/{id}/schedules/{sid}/run` — M9 (scheduler).
pub async fn schedule_run_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "triggering a schedule immediately",
        milestone: "M9 (scheduler)",
    })
}

/// Workload-kind-specific surface (§10.15) — M16+.
pub async fn workload_kind_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature:
            "workload-kind-specific operations (capabilities, endpoints, deploy, snapshot, migrate)",
        milestone: "M16 (workload integration SDK)",
    })
}
