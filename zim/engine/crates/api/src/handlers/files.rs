//! The instance file-API family (§10.3, M28) — the real handlers.
//!
//! The declared-but-stubbed surface from M5 turns real here, over
//! the same chrooted [`zim_engine_files::Filesystem`] the backup
//! and install pipelines already use: every operation is
//! root-relative, §2.6 path-validated (traversal, absolute paths,
//! and escaping symlinks refused), and §2.7-capped on extraction.
//! The handlers are transport only — they translate the HTTP
//! grammar onto the filesystem and map [`FileError`] onto the §6.1
//! vocabulary; every policy decision lives in the files crate.
//!
//! # Grant mapping (spec §10.3 table)
//!
//! | Operation                              | Grants          |
//! |----------------------------------------|-----------------|
//! | `GET  /instances/{id}/files`           | `file.read`     |
//! | `GET  /instances/{id}/files/content`   | `file.read_content` |
//! | `PUT  /instances/{id}/files/content`   | `file.write`   |
//! | `POST /instances/{id}/files/create`    | `file.create`  |
//! | `DELETE /instances/{id}/files`         | `file.delete`  |
//! | `POST /instances/{id}/files/rename`    | `file.write`   |
//! | `POST /instances/{id}/files/archive`   | `file.archive` |
//! | `POST /instances/{id}/files/extract`   | `file.archive` + `file.write` |
//!
//! # Concurrency and durability notes
//!
//! - Uploads (`PUT .../files/content`) ride the atomic
//!   write-to-temp-then-rename discipline of
//!   [`zim_engine_files::Filesystem::write_file`]: a crashed upload
//!   leaves the old or the new file, never a torn mix.
//! - `POST .../files/rename` is one `rename(2)` inside the root
//!   (destination parents created as needed) — atomic on POSIX.
//! - The §8 `If-Match` family does not apply here (spec §8: file
//!   sub-resources are documented as unconstrained; the last write
//!   wins, exactly like Wings).
//! - The upload route carries its own §13 body-limit channel
//!   (5 GB) layered *inside* the router-wide 10 MB JSON limit.
//!
//! # Audit
//!
//! Every mutation appends an `instance.file.*` audit record with
//! the affected path (and the operation's outcome facts); pure
//! reads are not audited, matching the rest of the surface.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use zim_engine_domain::{Instance, InstanceId, PermissionGrant};
use zim_engine_files::{ArchiveFormat, DirEntry, FileError, Filesystem};

use crate::error::ApiError;
use crate::handlers::{audit, check_grants, load_instance_for_caller, parse_uuid, ApiJson};
use crate::state::{AppState, Caller};

/// The §13 upload channel: `PUT .../files/content` accepts bodies up
/// to 5 GB (the router-wide 10 MB limit governs JSON routes).
pub const MAX_UPLOAD_BYTES: usize = 5 * 1024 * 1024 * 1024;

/// How long one blocking file operation may run before the request
/// is failed (§13's generous ceiling for multi-gigabyte uploads and
/// archives over slow disks; the §2.7 caps bound extraction work
/// independently).
const FILE_HOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

// ── Shared plumbing ──────────────────────────────────────────────

/// The per-instance filesystem rooted at
/// `{data_dir}/instances/{uuid_short}` — the same directory the
/// runtime bind-mounts and the backup pipeline archives, so the
/// three surfaces can never disagree about where a file lives.
///
/// The directory is created on first touch (the engine owns it; an
/// instance that has never been started still has a listable, empty
/// root — `GET .../files` on a fresh instance answers `[]`, not a
/// 500).
fn instance_fs(state: &AppState, instance: &Instance) -> Result<Filesystem, ApiError> {
    let root = state
        .config
        .data_dir
        .join("instances")
        .join(&instance.uuid_short);
    std::fs::create_dir_all(&root).map_err(|e| {
        ApiError::internal(format!(
            "cannot create the instance data dir {}: {e}",
            root.display()
        ))
    })?;
    Ok(Filesystem::new(root))
}

/// Bridge one blocking filesystem operation onto the async runtime
/// (the same discipline as the persistence hop: std::fs never runs
/// on the async workers) with [`FILE_HOP_TIMEOUT`] as the ceiling.
async fn fs_hop<T>(
    filesystem: Filesystem,
    op: impl FnOnce(&Filesystem) -> Result<T, FileError> + Send + 'static,
) -> Result<T, ApiError>
where
    T: Send + 'static,
{
    match tokio::time::timeout(
        FILE_HOP_TIMEOUT,
        tokio::task::spawn_blocking(move || op(&filesystem)),
    )
    .await
    {
        Ok(Ok(result)) => result.map_err(file_error),
        Ok(Err(join)) => Err(ApiError::internal(format!(
            "file operation task failed: {join}"
        ))),
        Err(_) => Err(ApiError::internal(
            "file operation timed out (600s ceiling reached)",
        )),
    }
}

/// Map the files crate's vocabulary onto the §6.1 table.
///
/// - `NotFound` → `404 not_found` (the requested path rides along).
/// - Path grammar failures (`EmptyPath`/`Absolute`/`Traversal`/
///   `EscapesRoot`) → `400 validation_failed` on `path`.
/// - Kind/policy refusals (`NotAFile`, `Limit`, `Overlap`,
///   `LinkEntry`, `UnsupportedFormat`) → `400 bad_request` — the
///   request described an operation the engine will not perform.
/// - `Io` → `500 internal_error`.
fn file_error(error: FileError) -> ApiError {
    match error {
        FileError::NotFound(path) => ApiError::not_found("file", path),
        FileError::EmptyPath
        | FileError::Absolute(_)
        | FileError::Traversal(_)
        | FileError::EscapesRoot(_) => ApiError::validation(error.to_string(), "path"),
        other @ (FileError::NotAFile(_)
        | FileError::Limit(_)
        | FileError::Overlap(_)
        | FileError::LinkEntry(_)
        | FileError::UnsupportedFormat(_)) => ApiError::bad_request(other.to_string()),
        FileError::Io { context, source } => {
            ApiError::internal(format!("file operation failed ({context}): {source}"))
        }
    }
}

/// One listed entry in the wire shape (spec §10.3 files family).
fn entry_json(entry: &DirEntry) -> serde_json::Value {
    json!({
        "name": entry.name,
        "is_dir": entry.is_dir,
        "size": entry.size,
        "modified": entry.modified.map(|t| t.to_rfc3339()),
    })
}

/// The `?path=` query shared by the content/delete routes: present,
/// non-empty (the listing route alone defaults to the root).
#[derive(Debug, Deserialize)]
pub struct PathQuery {
    /// The root-relative path to operate on.
    pub path: Option<String>,
}

/// Extract `?path=`, rejecting absence and emptiness with a
/// `validation_failed` (these routes have no meaningful default).
fn require_path(query: &PathQuery) -> Result<String, ApiError> {
    let path = query.path.as_deref().map(str::trim).unwrap_or("");
    if path.is_empty() {
        return Err(ApiError::validation(
            "this operation requires a non-empty ?path= query parameter",
            "path",
        ));
    }
    Ok(path.to_owned())
}

/// A filename safe for the `Content-Disposition` header (the wire
/// requires quoted-string safety: quotes and control bytes drop).
fn disposition_filename(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or("file");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_control() || c == '"' {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.is_empty() {
        "file".to_owned()
    } else {
        cleaned
    }
}

// ── Route handlers ───────────────────────────────────────────────

/// `GET /api/v1/instances/{id}/files?path={dir}` — list one
/// directory (defaults to the instance root). Requires `file.read`.
///
/// Answers `200` with `{"instance_id", "path", "entries": [...]}`;
/// each entry carries `name`, `is_dir`, `size`, and `modified`
/// (RFC 3339, `null` when the platform cannot read it).
pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::FileRead])?;

    let path = query.path.clone().unwrap_or_default();
    let path = if path.trim().is_empty() {
        ".".to_owned()
    } else {
        path
    };
    let filesystem = instance_fs(&state, &instance)?;
    let hop_path = path.clone();
    let entries = fs_hop(filesystem, move |fs| fs.list_dir(&hop_path)).await?;

    Ok(Json(json!({
        "instance_id": instance_id,
        "path": path,
        "entries": entries.iter().map(entry_json).collect::<Vec<_>>(),
    })))
}

/// `GET /api/v1/instances/{id}/files/content?path={file}` —
/// download one file's bytes. Requires `file.read_content`.
///
/// Answers `200` with the raw bytes (`application/octet-stream`,
/// `Content-Disposition: attachment`); a directory where a file was
/// required answers `400`.
pub async fn download(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Response, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::FileReadContent])?;
    let path = require_path(&query)?;

    let filesystem = instance_fs(&state, &instance)?;
    let hop_path = path.clone();
    let content = fs_hop(filesystem, move |fs| fs.read_file(&hop_path)).await?;

    let filename = disposition_filename(query.path.as_deref().unwrap_or("file"));
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(value) =
        header::HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
    {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    Ok((StatusCode::OK, headers, content).into_response())
}

/// `PUT /api/v1/instances/{id}/files/content?path={file}` — write
/// one file from the raw request body. Requires `file.write`.
///
/// The write is atomic (temp-then-rename; parent directories appear
/// as needed), so a crashed upload never leaves a torn file. The
/// route's body limit is the §13 upload channel (5 GB), layered
/// inside the router-wide 10 MB JSON limit at the route level in
/// `routes.rs`. Answers `200` with the path and the stored size.
pub async fn upload(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::FileWrite])?;
    let path = require_path(&query)?;

    let filesystem = instance_fs(&state, &instance)?;
    let size = body.len();
    let content = body.to_vec();
    let hop_path = path.clone();
    fs_hop(filesystem, move |fs| fs.write_file(&hop_path, &content)).await?;

    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.file.write",
        Some(instance_id),
        json!({ "path": path, "bytes": size, "transport": "http" }),
    )
    .await;
    Ok(Json(json!({ "path": path, "size": size })))
}

/// The body of `POST .../files/create` (spec §10.3).
#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    /// The root-relative path to create.
    pub path: String,
    /// Create a directory (with parents) instead of an empty file.
    #[serde(default)]
    pub is_dir: bool,
}

/// `POST /api/v1/instances/{id}/files/create` — create one file or
/// directory. Requires `file.create`.
///
/// Directories are created with parents; files are created empty
/// (content arrives via `PUT .../files/content`). Answers `201`.
pub async fn create(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<CreateRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::FileCreate])?;
    let path = request.path.trim().to_owned();
    if path.is_empty() {
        return Err(ApiError::validation("path must not be empty", "path"));
    }

    let filesystem = instance_fs(&state, &instance)?;
    let is_dir = request.is_dir;
    let hop_path = path.clone();
    fs_hop(filesystem, move |fs| {
        if is_dir {
            fs.mkdir(&hop_path)
        } else {
            fs.write_file(&hop_path, &[])
        }
    })
    .await?;

    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.file.create",
        Some(instance_id),
        json!({ "path": path, "is_dir": is_dir, "transport": "http" }),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "path": path, "is_dir": is_dir })),
    ))
}

/// `DELETE /api/v1/instances/{id}/files?path={path}` — delete one
/// file or directory (directories recursively). Requires
/// `file.delete`.
///
/// The root itself (`path=.`) is structurally refused (the §2.6
/// grammar has no empty relative form), so the data directory
/// cannot be deleted through this route. Answers `204`.
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<StatusCode, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::FileDelete])?;
    let path = require_path(&query)?;

    let filesystem = instance_fs(&state, &instance)?;
    let hop_path = path.clone();
    fs_hop(filesystem, move |fs| fs.delete(&hop_path)).await?;

    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.file.delete",
        Some(instance_id),
        json!({ "path": path, "transport": "http" }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// The body of `POST .../files/rename` (the M28 addition to the
/// §10.3 family: one `rename(2)` where Wings has `Rename`).
#[derive(Debug, Deserialize)]
pub struct RenameRequest {
    /// The existing root-relative path.
    pub old_path: String,
    /// The new root-relative path (parents created as needed).
    pub new_path: String,
}

/// `POST /api/v1/instances/{id}/files/rename` — rename or move one
/// path inside the instance root. Requires `file.write`.
///
/// Both ends are §2.6-validated; a directory cannot move inside
/// itself; on POSIX the move is atomic. Answers `200` with both
/// paths.
pub async fn rename(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<RenameRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::FileWrite])?;
    let old_path = request.old_path.trim().to_owned();
    let new_path = request.new_path.trim().to_owned();
    if old_path.is_empty() || new_path.is_empty() {
        return Err(ApiError::validation(
            "old_path and new_path must both be non-empty",
            "old_path",
        ));
    }

    let filesystem = instance_fs(&state, &instance)?;
    let (from, to) = (old_path.clone(), new_path.clone());
    fs_hop(filesystem, move |fs| fs.rename(&from, &to)).await?;

    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.file.rename",
        Some(instance_id),
        json!({ "from": old_path, "to": new_path, "transport": "http" }),
    )
    .await;
    Ok(Json(json!({ "old_path": old_path, "new_path": new_path })))
}

/// The body of `POST .../files/archive` (spec §10.3; Wings's
/// `CompressFiles`).
#[derive(Debug, Deserialize)]
pub struct ArchiveRequest {
    /// The root-relative paths to compress (non-empty allow-list).
    pub paths: Vec<String>,
    /// Where the archive lands, root-relative.
    pub destination: String,
    /// `tar`, `tar.gz` (the default, Wings's own shape), or `zip`.
    pub format: Option<String>,
}

/// `POST /api/v1/instances/{id}/files/archive` — compress an
/// allow-list of paths into one archive inside the root. Requires
/// `file.archive`.
///
/// Answers `200` with the destination, format, entry count, and
/// archive size.
pub async fn archive(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<ArchiveRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(&ctx, &[PermissionGrant::FileArchive])?;

    if request.paths.is_empty() {
        return Err(ApiError::validation(
            "paths must list at least one entry to archive",
            "paths",
        ));
    }
    let format = match request.format.as_deref().map(str::trim) {
        None | Some("") | Some("tar.gz") => ArchiveFormat::TarGz,
        Some("tar") => ArchiveFormat::Tar,
        Some("zip") => ArchiveFormat::Zip,
        Some(other) => {
            return Err(ApiError::validation(
                format!("unknown archive format {other:?} (tar, tar.gz, zip)"),
                "format",
            ));
        }
    };

    let destination = request.destination.trim().to_owned();
    if destination.is_empty() {
        return Err(ApiError::validation(
            "destination must not be empty",
            "destination",
        ));
    }
    let paths: Vec<String> = request.paths;
    let paths_count = paths.len();

    let filesystem = instance_fs(&state, &instance)?;
    let dest = destination.clone();
    let bytes = fs_hop(filesystem, move |fs| {
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        fs.archive(&dest, &refs, format)
    })
    .await?;

    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.file.archive",
        Some(instance_id),
        json!({
            "destination": destination,
            "format": format.extension(),
            "entries": paths_count,
            "bytes": bytes,
            "transport": "http",
        }),
    )
    .await;
    Ok(Json(json!({
        "destination": destination,
        "format": format.extension(),
        "entries": paths_count,
        "bytes": bytes,
    })))
}

/// The body of `POST .../files/extract` (spec §10.3; Wings's
/// `DecompressFile` family).
#[derive(Debug, Deserialize)]
pub struct ExtractRequest {
    /// The root-relative archive to read.
    pub archive_path: String,
    /// Where the entries land, root-relative.
    pub destination: String,
}

/// `POST /api/v1/instances/{id}/files/extract` — extract one
/// archive under a destination inside the root. Requires
/// `file.archive` + `file.write`.
///
/// The §2.7 caps (uncompressed size, entry count, nesting depth,
/// link refusal, overlap refusal) are enforced by the files crate.
/// Answers `200` with the extraction stats.
pub async fn extract(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<ExtractRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let instance_id = parse_uuid::<InstanceId>(&id, "id")?;
    let (instance, ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    check_grants(
        &ctx,
        &[PermissionGrant::FileArchive, PermissionGrant::FileWrite],
    )?;

    let archive_path = request.archive_path.trim().to_owned();
    let destination = request.destination.trim().to_owned();
    if archive_path.is_empty() || destination.is_empty() {
        return Err(ApiError::validation(
            "archive_path and destination must both be non-empty",
            "archive_path",
        ));
    }

    let filesystem = instance_fs(&state, &instance)?;
    let (src, dest) = (archive_path.clone(), destination.clone());
    let stats = fs_hop(filesystem, move |fs| fs.extract(&src, &dest)).await?;

    audit(
        &state,
        Some(ctx.user_id.to_string()),
        "instance.file.extract",
        Some(instance_id),
        json!({
            "archive_path": archive_path,
            "destination": destination,
            "entries": stats.entries,
            "bytes": stats.uncompressed_bytes,
            "nested_archives": stats.nested_archives,
            "transport": "http",
        }),
    )
    .await;
    Ok(Json(json!({
        "archive_path": archive_path,
        "destination": destination,
        "entries": stats.entries,
        "bytes": stats.uncompressed_bytes,
        "nested_archives": stats.nested_archives,
    })))
}
