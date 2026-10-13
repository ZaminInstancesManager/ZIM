//! The `Persistence` implementation for [`SqlitePersistence`].
//!
//! Every method follows the same shape: lock the connection,
//! execute one statement (upsert / select / delete / update), map
//! rusqlite errors into
//! [`PersistenceError::Backend`](zim_engine_application::ports::PersistenceError::Backend)
//! and (de)serialization failures into
//! [`PersistenceError::Serialization`](zim_engine_application::ports::PersistenceError::Serialization).
//!
//! Upserts use `INSERT ... ON CONFLICT DO UPDATE` rather than
//! `INSERT OR REPLACE` — REPLACE issues an implicit DELETE, which
//! trips foreign keys when the row is a parent (see the crate docs).

use crate::migrations;
use rusqlite::params;
use zim_engine_application::ports::{Persistence, PersistenceError};
use zim_engine_domain::{
    Allocation, AllocationId, AuditRecord, Backup, BackupId, CommandTemplate, ExtensionId,
    ExtensionRegistration, ExtensionSignature, Instance, InstanceId, InstanceStatus, Job, JobId,
    JobState, Node, NodeId, Permission, PermissionSet, Schedule, ScheduleId, Transfer, TransferId,
    User, UserId,
};

// ─── Error & conversion helpers ───────────────────────────────────────────

fn db_err(e: rusqlite::Error) -> PersistenceError {
    PersistenceError::backend(e.to_string())
}

fn ser<T: serde::Serialize>(value: &T) -> Result<String, PersistenceError> {
    serde_json::to_string(value).map_err(|e| PersistenceError::serialization(e.to_string()))
}

fn deser<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, PersistenceError> {
    serde_json::from_str(text).map_err(|e| PersistenceError::serialization(e.to_string()))
}

/// Serializes a serde enum to its `snake_case` string form (as used
/// by the schema's TEXT enum columns).
fn enum_str<T: serde::Serialize>(value: &T) -> Result<String, PersistenceError> {
    let json =
        serde_json::to_value(value).map_err(|e| PersistenceError::serialization(e.to_string()))?;
    json.as_str()
        .map(str::to_owned)
        .ok_or_else(|| PersistenceError::serialization("enum did not serialize to a string"))
}

/// Deserializes a serde enum from its `snake_case` string form.
fn enum_from<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, PersistenceError> {
    deser(&serde_json::Value::String(text.to_owned()).to_string())
}

fn ts(dt: &chrono::DateTime<chrono::Utc>) -> String {
    dt.to_rfc3339()
}

fn ts_opt(dt: &Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    dt.as_ref().map(ts)
}

fn parse_ts(text: &str) -> Result<chrono::DateTime<chrono::Utc>, PersistenceError> {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| PersistenceError::serialization(format!("bad timestamp {text:?}: {e}")))
}

fn parse_ts_opt(
    text: Option<&str>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, PersistenceError> {
    text.map(parse_ts).transpose()
}

/// Renders any strongly-typed UUID id (or a raw `Uuid`) as the
/// canonical hyphenated TEXT form. All domain ID newtypes implement
/// `Display` with exactly that form.
fn uuid_to_text(id: impl std::fmt::Display) -> String {
    id.to_string()
}

fn uuid_from_text(text: &str, what: &str) -> Result<uuid::Uuid, PersistenceError> {
    text.parse()
        .map_err(|e| PersistenceError::serialization(format!("bad {what} uuid {text:?}: {e}")))
}

/// Iterates a `Rows` cursor through a domain row mapper (which
/// produces `PersistenceError`s rather than rusqlite errors).
fn collect_rows<T>(
    rows: &mut rusqlite::Rows<'_>,
    map: impl Fn(&rusqlite::Row<'_>) -> Result<T, PersistenceError>,
) -> Result<Vec<T>, PersistenceError> {
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(db_err)? {
        out.push(map(row)?);
    }
    Ok(out)
}

// ─── The trait implementation ─────────────────────────────────────────────

impl Persistence for crate::SqlitePersistence {
    // ── Schema lifecycle ────────────────────────────────────────────

    fn migrate(&self) -> Result<(), PersistenceError> {
        self.with_conn(|conn| migrations::apply_pending(conn).map(|_| ()))
    }

    fn schema_version(&self) -> Result<u64, PersistenceError> {
        self.with_conn(migrations::current_version)
    }

    // ── Instances ───────────────────────────────────────────────────

    fn put_instance(&self, instance: &Instance) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO instances (
                    id, external_id, uuid, uuid_short, node_id, name, description,
                    status, skip_egg_scripts, owner_id, limits, allocation_id, egg_id,
                    startup, image, allocation_limit, database_limit, backup_limit,
                    crash_detection_enabled, installed_at, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                          ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)
                ON CONFLICT (id) DO UPDATE SET
                    external_id = excluded.external_id,
                    uuid = excluded.uuid,
                    uuid_short = excluded.uuid_short,
                    node_id = excluded.node_id,
                    name = excluded.name,
                    description = excluded.description,
                    status = excluded.status,
                    skip_egg_scripts = excluded.skip_egg_scripts,
                    owner_id = excluded.owner_id,
                    limits = excluded.limits,
                    allocation_id = excluded.allocation_id,
                    egg_id = excluded.egg_id,
                    startup = excluded.startup,
                    image = excluded.image,
                    allocation_limit = excluded.allocation_limit,
                    database_limit = excluded.database_limit,
                    backup_limit = excluded.backup_limit,
                    crash_detection_enabled = excluded.crash_detection_enabled,
                    installed_at = excluded.installed_at,
                    created_at = excluded.created_at,
                    updated_at = excluded.updated_at",
                params![
                    uuid_to_text(instance.id),
                    instance.external_id,
                    instance.uuid,
                    instance.uuid_short,
                    uuid_to_text(instance.node_id),
                    instance.name,
                    instance.description,
                    enum_str(&instance.status)?,
                    instance.skip_egg_scripts,
                    uuid_to_text(instance.owner_id),
                    ser(&instance.limits)?,
                    instance.allocation_id.0 as i64,
                    uuid_to_text(instance.egg_id),
                    instance.startup.as_str(),
                    instance.image.to_string(),
                    instance.allocation_limit.map(|v| v as i64),
                    instance.database_limit.map(|v| v as i64),
                    instance.backup_limit as i64,
                    instance.crash_detection_enabled,
                    ts_opt(&instance.installed_at),
                    ts(&instance.created_at),
                    ts(&instance.updated_at),
                ],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn get_instance(&self, id: &InstanceId) -> Result<Option<Instance>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, external_id, uuid, uuid_short, node_id, name, description,
                            status, skip_egg_scripts, owner_id, limits, allocation_id, egg_id,
                            startup, image, allocation_limit, database_limit, backup_limit,
                            crash_detection_enabled, installed_at, created_at, updated_at
                     FROM instances WHERE id = ?1",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![uuid_to_text(id)]).map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Ok(Some(row_to_instance(row)?)),
                None => Ok(None),
            }
        })
    }

    fn delete_instance(&self, id: &InstanceId) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM instances WHERE id = ?1",
                params![uuid_to_text(id)],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn list_instances_by_node(&self, node: &NodeId) -> Result<Vec<Instance>, PersistenceError> {
        self.list_instances_where("WHERE node_id = ?1", params![uuid_to_text(node)])
    }

    fn list_instances_by_owner(&self, owner: &UserId) -> Result<Vec<Instance>, PersistenceError> {
        self.list_instances_where("WHERE owner_id = ?1", params![uuid_to_text(owner)])
    }

    fn update_instance_status(
        &self,
        id: &InstanceId,
        status: InstanceStatus,
    ) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE instances SET status = ?2, updated_at = ?3 WHERE id = ?1",
                params![
                    uuid_to_text(id),
                    enum_str(&status)?,
                    ts(&chrono::Utc::now())
                ],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    // ── Nodes ───────────────────────────────────────────────────────

    fn put_node(&self, node: &Node) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO nodes (
                    id, name, location_id, fqdn, scheme, public_port, listen_port,
                    capabilities, deployment, last_heartbeat, last_seen_state
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT (id) DO UPDATE SET
                    name = excluded.name,
                    location_id = excluded.location_id,
                    fqdn = excluded.fqdn,
                    scheme = excluded.scheme,
                    public_port = excluded.public_port,
                    listen_port = excluded.listen_port,
                    capabilities = excluded.capabilities,
                    deployment = excluded.deployment,
                    last_heartbeat = excluded.last_heartbeat,
                    last_seen_state = excluded.last_seen_state",
                params![
                    uuid_to_text(node.id),
                    node.name,
                    uuid_to_text(node.location_id),
                    node.fqdn,
                    enum_str(&node.scheme)?,
                    node.public_port as i64,
                    node.listen_port as i64,
                    ser(&node.capabilities)?,
                    enum_str(&node.deployment)?,
                    ts_opt(&node.last_heartbeat),
                    enum_str(&node.last_seen_state)?,
                ],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn get_node(&self, id: &NodeId) -> Result<Option<Node>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, name, location_id, fqdn, scheme, public_port, listen_port,
                            capabilities, deployment, last_heartbeat, last_seen_state
                     FROM nodes WHERE id = ?1",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![uuid_to_text(id)]).map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Ok(Some(row_to_node(row)?)),
                None => Ok(None),
            }
        })
    }

    fn delete_node(&self, id: &NodeId) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM nodes WHERE id = ?1", params![uuid_to_text(id)])
                .map(|_| ())
                .map_err(db_err)
        })
    }

    fn list_nodes(&self) -> Result<Vec<Node>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, name, location_id, fqdn, scheme, public_port, listen_port,
                            capabilities, deployment, last_heartbeat, last_seen_state
                     FROM nodes ORDER BY name",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query([]).map_err(db_err)?;
            collect_rows(&mut rows, row_to_node)
        })
    }

    // ── Allocations ─────────────────────────────────────────────────

    fn put_allocation(&self, allocation: &Allocation) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO allocations (
                    id, node_id, ip, port, assigned_to, note
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT (id) DO UPDATE SET
                    node_id = excluded.node_id,
                    ip = excluded.ip,
                    port = excluded.port,
                    assigned_to = excluded.assigned_to,
                    note = excluded.note",
                params![
                    allocation.id.0 as i64,
                    uuid_to_text(allocation.node_id),
                    allocation.ip.to_string(),
                    allocation.port as i64,
                    allocation.assigned_to.map(uuid_to_text),
                    allocation.note,
                ],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn create_allocation(
        &self,
        node_id: &NodeId,
        ip: std::net::IpAddr,
        port: u16,
        note: Option<&str>,
    ) -> Result<zim_engine_domain::AllocationId, PersistenceError> {
        self.with_conn(|conn| {
            conn.query_row(
                "INSERT INTO allocations (id, node_id, ip, port, assigned_to, note)
                 VALUES (NULL, ?1, ?2, ?3, NULL, ?4)
                 RETURNING id",
                params![uuid_to_text(node_id), ip.to_string(), port as i64, note],
                |row| row.get::<_, i64>(0),
            )
            .map(|id| zim_engine_domain::AllocationId(id.max(0) as u64))
            .map_err(db_err)
        })
    }

    fn get_allocation(&self, id: &AllocationId) -> Result<Option<Allocation>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, node_id, ip, port, assigned_to, note
                     FROM allocations WHERE id = ?1",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![id.0 as i64]).map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Ok(Some(row_to_allocation(row)?)),
                None => Ok(None),
            }
        })
    }

    fn delete_allocation(&self, id: &AllocationId) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM allocations WHERE id = ?1",
                params![id.0 as i64],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn list_allocations_by_node(&self, node: &NodeId) -> Result<Vec<Allocation>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, node_id, ip, port, assigned_to, note
                     FROM allocations WHERE node_id = ?1 ORDER BY ip, port",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![uuid_to_text(node)]).map_err(db_err)?;
            collect_rows(&mut rows, row_to_allocation)
        })
    }

    fn find_allocation_by_instance(
        &self,
        instance: &InstanceId,
    ) -> Result<Option<Allocation>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, node_id, ip, port, assigned_to, note
                     FROM allocations WHERE assigned_to = ?1 ORDER BY id LIMIT 1",
                )
                .map_err(db_err)?;
            let mut rows = stmt
                .query(params![uuid_to_text(instance)])
                .map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Ok(Some(row_to_allocation(row)?)),
                None => Ok(None),
            }
        })
    }

    // ── Users ───────────────────────────────────────────────────────

    fn put_user(&self, user: &User) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO users (
                    id, email, username, external_id, password_hash, oauth_subject,
                    is_admin, root_admin
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT (id) DO UPDATE SET
                    email = excluded.email,
                    username = excluded.username,
                    external_id = excluded.external_id,
                    password_hash = excluded.password_hash,
                    oauth_subject = excluded.oauth_subject,
                    is_admin = excluded.is_admin,
                    root_admin = excluded.root_admin",
                params![
                    uuid_to_text(user.id),
                    user.email,
                    user.username,
                    user.external_id,
                    user.password_hash,
                    user.oauth_subject,
                    user.is_admin,
                    user.root_admin,
                ],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn get_user(&self, id: &UserId) -> Result<Option<User>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, email, username, external_id, password_hash, oauth_subject,
                            is_admin, root_admin
                     FROM users WHERE id = ?1",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![uuid_to_text(id)]).map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Ok(Some(row_to_user(row)?)),
                None => Ok(None),
            }
        })
    }

    fn get_user_by_email(&self, email: &str) -> Result<Option<User>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, email, username, external_id, password_hash, oauth_subject,
                            is_admin, root_admin
                     FROM users WHERE email = ?1 COLLATE NOCASE",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![email]).map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Ok(Some(row_to_user(row)?)),
                None => Ok(None),
            }
        })
    }

    fn delete_user(&self, id: &UserId) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM users WHERE id = ?1", params![uuid_to_text(id)])
                .map(|_| ())
                .map_err(db_err)
        })
    }

    fn list_users(&self) -> Result<Vec<User>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, email, username, external_id, password_hash, oauth_subject,
                            is_admin, root_admin
                     FROM users ORDER BY email",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query([]).map_err(db_err)?;
            collect_rows(&mut rows, row_to_user)
        })
    }

    // ── Permissions ─────────────────────────────────────────────────

    fn put_permission(&self, permission: &Permission) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO permissions (user_id, instance_id, grants)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT (user_id, instance_id) DO UPDATE SET
                    grants = excluded.grants",
                params![
                    uuid_to_text(permission.user_id),
                    uuid_to_text(permission.instance_id),
                    ser(&permission.grants)?,
                ],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn get_permission(
        &self,
        user: &UserId,
        instance: &InstanceId,
    ) -> Result<Option<Permission>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT user_id, instance_id, grants FROM permissions
                          WHERE user_id = ?1 AND instance_id = ?2",
                )
                .map_err(db_err)?;
            let mut rows = stmt
                .query(params![uuid_to_text(user), uuid_to_text(instance)])
                .map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Ok(Some(Permission {
                    user_id: uuid_from_text(
                        row.get::<_, String>(0).map_err(db_err)?.as_str(),
                        "user",
                    )?
                    .into(),
                    instance_id: uuid_from_text(
                        row.get::<_, String>(1).map_err(db_err)?.as_str(),
                        "instance",
                    )?
                    .into(),
                    grants: deser(&row.get::<_, String>(2).map_err(db_err)?)?,
                })),
                None => Ok(None),
            }
        })
    }

    fn delete_permission(
        &self,
        user: &UserId,
        instance: &InstanceId,
    ) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM permissions WHERE user_id = ?1 AND instance_id = ?2",
                params![uuid_to_text(user), uuid_to_text(instance)],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn list_permissions_by_instance(
        &self,
        instance: &InstanceId,
    ) -> Result<Vec<Permission>, PersistenceError> {
        self.list_permissions_where("WHERE instance_id = ?1", params![uuid_to_text(instance)])
    }

    fn list_permissions_by_user(&self, user: &UserId) -> Result<Vec<Permission>, PersistenceError> {
        self.list_permissions_where("WHERE user_id = ?1", params![uuid_to_text(user)])
    }

    // ── Jobs ────────────────────────────────────────────────────────

    fn put_job(&self, job: &Job) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO jobs (
                    id, kind, instance_id, state, created_at, updated_at, started_at,
                    completed_at, last_error, cleanup_pending, generation
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT (id) DO UPDATE SET
                    kind = excluded.kind,
                    instance_id = excluded.instance_id,
                    state = excluded.state,
                    created_at = excluded.created_at,
                    updated_at = excluded.updated_at,
                    started_at = excluded.started_at,
                    completed_at = excluded.completed_at,
                    last_error = excluded.last_error,
                    cleanup_pending = excluded.cleanup_pending,
                    generation = excluded.generation",
                params![
                    uuid_to_text(job.id),
                    enum_str(&job.kind)?,
                    uuid_to_text(job.instance_id),
                    enum_str(&job.state)?,
                    ts(&job.created_at),
                    ts(&job.updated_at),
                    ts_opt(&job.started_at),
                    ts_opt(&job.completed_at),
                    job.last_error,
                    job.cleanup_pending,
                    job.generation as i64,
                ],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn get_job(&self, id: &JobId) -> Result<Option<Job>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, kind, instance_id, state, created_at, updated_at, started_at,
                            completed_at, last_error, cleanup_pending, generation
                     FROM jobs WHERE id = ?1",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![uuid_to_text(id)]).map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Ok(Some(row_to_job(row)?)),
                None => Ok(None),
            }
        })
    }

    fn delete_job(&self, id: &JobId) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM jobs WHERE id = ?1", params![uuid_to_text(id)])
                .map(|_| ())
                .map_err(db_err)
        })
    }

    fn list_jobs_by_instance(&self, instance: &InstanceId) -> Result<Vec<Job>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, kind, instance_id, state, created_at, updated_at, started_at,
                            completed_at, last_error, cleanup_pending, generation
                     FROM jobs WHERE instance_id = ?1 ORDER BY created_at",
                )
                .map_err(db_err)?;
            let mut rows = stmt
                .query(params![uuid_to_text(instance)])
                .map_err(db_err)?;
            collect_rows(&mut rows, row_to_job)
        })
    }

    fn list_jobs_by_state(&self, state: JobState) -> Result<Vec<Job>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, kind, instance_id, state, created_at, updated_at, started_at,
                            completed_at, last_error, cleanup_pending, generation
                     FROM jobs WHERE state = ?1 ORDER BY created_at",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![enum_str(&state)?]).map_err(db_err)?;
            collect_rows(&mut rows, row_to_job)
        })
    }

    fn update_job_state(&self, id: &JobId, state: JobState) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE jobs SET state = ?2, updated_at = ?3 WHERE id = ?1",
                params![uuid_to_text(id), enum_str(&state)?, ts(&chrono::Utc::now())],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    // ── Backups ─────────────────────────────────────────────────────

    fn put_backup(&self, backup: &Backup) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO backups (
                    id, instance_id, job_id, uuid, file_name, bytes, sha256_hash, locked,
                    completed_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT (id) DO UPDATE SET
                    instance_id = excluded.instance_id,
                    job_id = excluded.job_id,
                    uuid = excluded.uuid,
                    file_name = excluded.file_name,
                    bytes = excluded.bytes,
                    sha256_hash = excluded.sha256_hash,
                    locked = excluded.locked,
                    completed_at = excluded.completed_at",
                params![
                    uuid_to_text(backup.id),
                    uuid_to_text(backup.instance_id),
                    uuid_to_text(backup.job_id),
                    backup.uuid,
                    backup.file_name,
                    backup.bytes as i64,
                    backup.sha256_hash,
                    backup.locked,
                    ts_opt(&backup.completed_at),
                ],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn get_backup(&self, id: &BackupId) -> Result<Option<Backup>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, instance_id, job_id, uuid, file_name, bytes, sha256_hash, locked,
                            completed_at
                     FROM backups WHERE id = ?1",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![uuid_to_text(id)]).map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Ok(Some(row_to_backup(row)?)),
                None => Ok(None),
            }
        })
    }

    fn delete_backup(&self, id: &BackupId) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM backups WHERE id = ?1",
                params![uuid_to_text(id)],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn list_backups_by_instance(
        &self,
        instance: &InstanceId,
    ) -> Result<Vec<Backup>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, instance_id, job_id, uuid, file_name, bytes, sha256_hash, locked,
                            completed_at
                     FROM backups WHERE instance_id = ?1 ORDER BY completed_at DESC",
                )
                .map_err(db_err)?;
            let mut rows = stmt
                .query(params![uuid_to_text(instance)])
                .map_err(db_err)?;
            collect_rows(&mut rows, row_to_backup)
        })
    }

    // ── Transfers ───────────────────────────────────────────────────

    fn put_transfer(&self, transfer: &Transfer) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO transfers (
                    id, instance_id, job_id, from_node_id, to_node_id, state, archive_path
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT (id) DO UPDATE SET
                    instance_id = excluded.instance_id,
                    job_id = excluded.job_id,
                    from_node_id = excluded.from_node_id,
                    to_node_id = excluded.to_node_id,
                    state = excluded.state,
                    archive_path = excluded.archive_path",
                params![
                    uuid_to_text(transfer.id),
                    uuid_to_text(transfer.instance_id),
                    uuid_to_text(transfer.job_id),
                    uuid_to_text(transfer.from_node_id),
                    uuid_to_text(transfer.to_node_id),
                    enum_str(&transfer.state)?,
                    transfer.archive_path,
                ],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn get_transfer(&self, id: &TransferId) -> Result<Option<Transfer>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, instance_id, job_id, from_node_id, to_node_id, state, archive_path
                     FROM transfers WHERE id = ?1",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![uuid_to_text(id)]).map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Ok(Some(row_to_transfer(row)?)),
                None => Ok(None),
            }
        })
    }

    fn list_transfers_by_instance(
        &self,
        instance: &InstanceId,
    ) -> Result<Vec<Transfer>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, instance_id, job_id, from_node_id, to_node_id, state, archive_path
                     FROM transfers WHERE instance_id = ?1 ORDER BY id",
                )
                .map_err(db_err)?;
            let mut rows = stmt
                .query(params![uuid_to_text(instance)])
                .map_err(db_err)?;
            collect_rows(&mut rows, row_to_transfer)
        })
    }

    // ── Schedules ───────────────────────────────────────────────────

    fn put_schedule(&self, schedule: &Schedule) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO schedules (
                    id, instance_id, name, cron, next_run_at, last_run_at, is_active,
                    only_when_online, actions
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT (id) DO UPDATE SET
                    instance_id = excluded.instance_id,
                    name = excluded.name,
                    cron = excluded.cron,
                    next_run_at = excluded.next_run_at,
                    last_run_at = excluded.last_run_at,
                    is_active = excluded.is_active,
                    only_when_online = excluded.only_when_online,
                    actions = excluded.actions",
                params![
                    uuid_to_text(schedule.id),
                    uuid_to_text(schedule.instance_id),
                    schedule.name,
                    schedule.cron,
                    ts_opt(&schedule.next_run_at),
                    ts_opt(&schedule.last_run_at),
                    schedule.is_active,
                    schedule.only_when_online,
                    ser(&schedule.actions)?,
                ],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn get_schedule(&self, id: &ScheduleId) -> Result<Option<Schedule>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, instance_id, name, cron, next_run_at, last_run_at, is_active,
                            only_when_online, actions
                     FROM schedules WHERE id = ?1",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![uuid_to_text(id)]).map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Ok(Some(row_to_schedule(row)?)),
                None => Ok(None),
            }
        })
    }

    fn delete_schedule(&self, id: &ScheduleId) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM schedules WHERE id = ?1",
                params![uuid_to_text(id)],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn list_schedules_by_instance(
        &self,
        instance: &InstanceId,
    ) -> Result<Vec<Schedule>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, instance_id, name, cron, next_run_at, last_run_at, is_active,
                            only_when_online, actions
                     FROM schedules WHERE instance_id = ?1 ORDER BY name",
                )
                .map_err(db_err)?;
            let mut rows = stmt
                .query(params![uuid_to_text(instance)])
                .map_err(db_err)?;
            collect_rows(&mut rows, row_to_schedule)
        })
    }

    // ── Audit (append-only) ─────────────────────────────────────────

    fn append_audit(&self, record: &AuditRecord) -> Result<u64, PersistenceError> {
        self.with_conn(|conn| {
            conn.query_row(
                "INSERT INTO audit (
                    timestamp, actor, actor_id, action, instance_id, metadata, ip_address,
                    user_agent
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                RETURNING id",
                params![
                    ts(&record.timestamp),
                    enum_str(&record.actor)?,
                    record.actor_id,
                    record.action,
                    record.instance_id.map(uuid_to_text),
                    record.metadata.to_string(),
                    record.ip_address.map(|ip| ip.to_string()),
                    record.user_agent,
                ],
                |row| row.get::<_, i64>(0),
            )
            .map(|id| id.max(0) as u64)
            .map_err(db_err)
        })
    }

    fn list_audit(
        &self,
        cursor: Option<u64>,
        limit: usize,
    ) -> Result<Vec<AuditRecord>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, timestamp, actor, actor_id, action, instance_id, metadata,
                            ip_address, user_agent
                     FROM audit WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
                )
                .map_err(db_err)?;
            let mut rows = stmt
                .query(params![cursor.unwrap_or(0) as i64, limit as i64])
                .map_err(db_err)?;
            collect_rows(&mut rows, row_to_audit)
        })
    }

    // ── Extensions ──────────────────────────────────────────────────

    fn put_extension(&self, extension: &ExtensionRegistration) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO extension_registrations (
                    id, manifest, version, installed_at, enabled, granted_permissions, signature
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT (id) DO UPDATE SET
                    manifest = excluded.manifest,
                    version = excluded.version,
                    installed_at = excluded.installed_at,
                    enabled = excluded.enabled,
                    granted_permissions = excluded.granted_permissions,
                    signature = excluded.signature",
                params![
                    uuid_to_text(extension.id),
                    ser(&extension.manifest)?,
                    extension.version,
                    ts(&extension.installed_at),
                    extension.enabled,
                    ser(&extension.granted_permissions)?,
                    extension.signature.as_ref().map(ser).transpose()?,
                ],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn get_extension(
        &self,
        id: &ExtensionId,
    ) -> Result<Option<ExtensionRegistration>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, manifest, version, installed_at, enabled, granted_permissions,
                            signature
                     FROM extension_registrations WHERE id = ?1",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![uuid_to_text(id)]).map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Ok(Some(row_to_extension(row)?)),
                None => Ok(None),
            }
        })
    }

    fn delete_extension(&self, id: &ExtensionId) -> Result<(), PersistenceError> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM extension_registrations WHERE id = ?1",
                params![uuid_to_text(id)],
            )
            .map(|_| ())
            .map_err(db_err)
        })
    }

    fn list_extensions(&self) -> Result<Vec<ExtensionRegistration>, PersistenceError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, manifest, version, installed_at, enabled, granted_permissions,
                            signature
                     FROM extension_registrations ORDER BY installed_at",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query([]).map_err(db_err)?;
            collect_rows(&mut rows, row_to_extension)
        })
    }
}

// ─── Private list helpers ─────────────────────────────────────────────────

impl crate::SqlitePersistence {
    fn list_instances_where(
        &self,
        predicate: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<Instance>, PersistenceError> {
        self.with_conn(|conn| {
            let sql = format!(
                "SELECT id, external_id, uuid, uuid_short, node_id, name, description,
                        status, skip_egg_scripts, owner_id, limits, allocation_id, egg_id,
                        startup, image, allocation_limit, database_limit, backup_limit,
                        crash_detection_enabled, installed_at, created_at, updated_at
                 FROM instances {predicate} ORDER BY name"
            );
            let mut stmt = conn.prepare(&sql).map_err(db_err)?;
            let mut rows = stmt.query(params).map_err(db_err)?;
            collect_rows(&mut rows, row_to_instance)
        })
    }

    fn list_permissions_where(
        &self,
        predicate: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<Permission>, PersistenceError> {
        self.with_conn(|conn| {
            let sql = format!("SELECT user_id, instance_id, grants FROM permissions {predicate}");
            let mut stmt = conn.prepare(&sql).map_err(db_err)?;
            let rows = stmt
                .query_map(params, |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(db_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_err)?;
            rows.into_iter()
                .map(|(user_id, instance_id, grants)| {
                    Ok(Permission {
                        user_id: uuid_from_text(&user_id, "user")?.into(),
                        instance_id: uuid_from_text(&instance_id, "instance")?.into(),
                        grants: deser(&grants)?,
                    })
                })
                .collect()
        })
    }
}

// ─── Row mappers ──────────────────────────────────────────────────────────

fn row_to_instance(row: &rusqlite::Row<'_>) -> Result<Instance, PersistenceError> {
    let installed_at: Option<String> = row.get(19).map_err(db_err)?;
    let created_at: String = row.get(20).map_err(db_err)?;
    let updated_at: String = row.get(21).map_err(db_err)?;
    let status: String = row.get(7).map_err(db_err)?;
    let image: String = row.get(14).map_err(db_err)?;
    let startup: String = row.get(13).map_err(db_err)?;
    let limits: String = row.get(10).map_err(db_err)?;
    Ok(Instance {
        id: uuid_from_text(&row.get::<_, String>(0).map_err(db_err)?, "instance")?.into(),
        external_id: row.get(1).map_err(db_err)?,
        uuid: row.get(2).map_err(db_err)?,
        uuid_short: row.get(3).map_err(db_err)?,
        node_id: uuid_from_text(&row.get::<_, String>(4).map_err(db_err)?, "node")?.into(),
        name: row.get(5).map_err(db_err)?,
        description: row.get(6).map_err(db_err)?,
        status: enum_from(&status)?,
        skip_egg_scripts: row.get(8).map_err(db_err)?,
        owner_id: uuid_from_text(&row.get::<_, String>(9).map_err(db_err)?, "user")?.into(),
        limits: deser(&limits)?,
        allocation_id: AllocationId::new(row.get::<_, i64>(11).map_err(db_err)?.max(0) as u64),
        egg_id: uuid_from_text(&row.get::<_, String>(12).map_err(db_err)?, "egg")?.into(),
        startup: CommandTemplate::new(startup),
        image: image.parse().map_err(|e| {
            PersistenceError::serialization(format!("bad docker image {image:?}: {e}"))
        })?,
        allocation_limit: row
            .get::<_, Option<i64>>(15)
            .map_err(db_err)?
            .map(|v| v.max(0) as u32),
        database_limit: row
            .get::<_, Option<i64>>(16)
            .map_err(db_err)?
            .map(|v| v.max(0) as u32),
        backup_limit: row.get::<_, i64>(17).map_err(db_err)?.max(0) as u32,
        crash_detection_enabled: row.get(18).map_err(db_err)?,
        installed_at: parse_ts_opt(installed_at.as_deref())?,
        created_at: parse_ts(&created_at)?,
        updated_at: parse_ts(&updated_at)?,
    })
}

fn row_to_node(row: &rusqlite::Row<'_>) -> Result<Node, PersistenceError> {
    let capabilities: String = row.get(7).map_err(db_err)?;
    let last_heartbeat: Option<String> = row.get(9).map_err(db_err)?;
    Ok(Node {
        id: uuid_from_text(&row.get::<_, String>(0).map_err(db_err)?, "node")?.into(),
        name: row.get(1).map_err(db_err)?,
        location_id: uuid_from_text(&row.get::<_, String>(2).map_err(db_err)?, "location")?.into(),
        fqdn: row.get(3).map_err(db_err)?,
        scheme: enum_from(&row.get::<_, String>(4).map_err(db_err)?)?,
        public_port: row.get::<_, i64>(5).map_err(db_err)?.max(0) as u16,
        listen_port: row.get::<_, i64>(6).map_err(db_err)?.max(0) as u16,
        capabilities: deser(&capabilities)?,
        deployment: enum_from(&row.get::<_, String>(8).map_err(db_err)?)?,
        last_heartbeat: parse_ts_opt(last_heartbeat.as_deref())?,
        last_seen_state: enum_from(&row.get::<_, String>(10).map_err(db_err)?)?,
    })
}

fn row_to_allocation(row: &rusqlite::Row<'_>) -> Result<Allocation, PersistenceError> {
    let assigned_to: Option<String> = row.get(4).map_err(db_err)?;
    Ok(Allocation {
        id: AllocationId::new(row.get::<_, i64>(0).map_err(db_err)?.max(0) as u64),
        node_id: uuid_from_text(&row.get::<_, String>(1).map_err(db_err)?, "node")?.into(),
        ip: row
            .get::<_, String>(2)
            .map_err(db_err)?
            .parse()
            .map_err(|e| PersistenceError::serialization(format!("bad ip: {e}")))?,
        port: row.get::<_, i64>(3).map_err(db_err)?.max(0) as u16,
        assigned_to: assigned_to
            .as_deref()
            .map(|text| uuid_from_text(text, "instance"))
            .transpose()?
            .map(UuidInto::into_id),
        note: row.get(5).map_err(db_err)?,
    })
}

/// Small shim so `.map(...)` chains read clearly when converting a
/// parsed `Uuid` back into a typed id.
struct UuidInto;

impl UuidInto {
    fn into_id(u: uuid::Uuid) -> InstanceId {
        InstanceId::from_uuid(u)
    }
}

fn row_to_user(row: &rusqlite::Row<'_>) -> Result<User, PersistenceError> {
    Ok(User {
        id: uuid_from_text(&row.get::<_, String>(0).map_err(db_err)?, "user")?.into(),
        email: row.get(1).map_err(db_err)?,
        username: row.get(2).map_err(db_err)?,
        external_id: row.get(3).map_err(db_err)?,
        password_hash: row.get(4).map_err(db_err)?,
        oauth_subject: row.get(5).map_err(db_err)?,
        is_admin: row.get(6).map_err(db_err)?,
        root_admin: row.get(7).map_err(db_err)?,
    })
}

fn row_to_job(row: &rusqlite::Row<'_>) -> Result<Job, PersistenceError> {
    let created_at: String = row.get(4).map_err(db_err)?;
    let updated_at: String = row.get(5).map_err(db_err)?;
    let started_at: Option<String> = row.get(6).map_err(db_err)?;
    let completed_at: Option<String> = row.get(7).map_err(db_err)?;
    Ok(Job {
        id: uuid_from_text(&row.get::<_, String>(0).map_err(db_err)?, "job")?.into(),
        kind: enum_from(&row.get::<_, String>(1).map_err(db_err)?)?,
        instance_id: uuid_from_text(&row.get::<_, String>(2).map_err(db_err)?, "instance")?.into(),
        state: enum_from(&row.get::<_, String>(3).map_err(db_err)?)?,
        created_at: parse_ts(&created_at)?,
        updated_at: parse_ts(&updated_at)?,
        started_at: parse_ts_opt(started_at.as_deref())?,
        completed_at: parse_ts_opt(completed_at.as_deref())?,
        last_error: row.get(8).map_err(db_err)?,
        cleanup_pending: row.get(9).map_err(db_err)?,
        generation: row.get::<_, i64>(10).map_err(db_err)?.max(0) as u64,
    })
}

fn row_to_backup(row: &rusqlite::Row<'_>) -> Result<Backup, PersistenceError> {
    let completed_at: Option<String> = row.get(8).map_err(db_err)?;
    Ok(Backup {
        id: uuid_from_text(&row.get::<_, String>(0).map_err(db_err)?, "backup")?.into(),
        instance_id: uuid_from_text(&row.get::<_, String>(1).map_err(db_err)?, "instance")?.into(),
        job_id: uuid_from_text(&row.get::<_, String>(2).map_err(db_err)?, "job")?.into(),
        uuid: row.get(3).map_err(db_err)?,
        file_name: row.get(4).map_err(db_err)?,
        bytes: row.get::<_, i64>(5).map_err(db_err)?.max(0) as u64,
        sha256_hash: row.get(6).map_err(db_err)?,
        locked: row.get(7).map_err(db_err)?,
        completed_at: parse_ts_opt(completed_at.as_deref())?,
    })
}

fn row_to_transfer(row: &rusqlite::Row<'_>) -> Result<Transfer, PersistenceError> {
    Ok(Transfer {
        id: uuid_from_text(&row.get::<_, String>(0).map_err(db_err)?, "transfer")?.into(),
        instance_id: uuid_from_text(&row.get::<_, String>(1).map_err(db_err)?, "instance")?.into(),
        job_id: uuid_from_text(&row.get::<_, String>(2).map_err(db_err)?, "job")?.into(),
        from_node_id: uuid_from_text(&row.get::<_, String>(3).map_err(db_err)?, "node")?.into(),
        to_node_id: uuid_from_text(&row.get::<_, String>(4).map_err(db_err)?, "node")?.into(),
        state: enum_from(&row.get::<_, String>(5).map_err(db_err)?)?,
        archive_path: row.get(6).map_err(db_err)?,
    })
}

fn row_to_schedule(row: &rusqlite::Row<'_>) -> Result<Schedule, PersistenceError> {
    let next_run_at: Option<String> = row.get(4).map_err(db_err)?;
    let last_run_at: Option<String> = row.get(5).map_err(db_err)?;
    let actions: String = row.get(8).map_err(db_err)?;
    Ok(Schedule {
        id: uuid_from_text(&row.get::<_, String>(0).map_err(db_err)?, "schedule")?.into(),
        instance_id: uuid_from_text(&row.get::<_, String>(1).map_err(db_err)?, "instance")?.into(),
        name: row.get(2).map_err(db_err)?,
        cron: row.get(3).map_err(db_err)?,
        next_run_at: parse_ts_opt(next_run_at.as_deref())?,
        last_run_at: parse_ts_opt(last_run_at.as_deref())?,
        is_active: row.get(6).map_err(db_err)?,
        only_when_online: row.get(7).map_err(db_err)?,
        actions: deser(&actions)?,
    })
}

fn row_to_audit(row: &rusqlite::Row<'_>) -> Result<AuditRecord, PersistenceError> {
    let timestamp: String = row.get(1).map_err(db_err)?;
    let instance_id: Option<String> = row.get(5).map_err(db_err)?;
    let metadata: String = row.get(6).map_err(db_err)?;
    let ip_address: Option<String> = row.get(7).map_err(db_err)?;
    Ok(AuditRecord {
        id: row.get::<_, i64>(0).map_err(db_err)?.max(0) as u64,
        timestamp: parse_ts(&timestamp)?,
        actor: enum_from(&row.get::<_, String>(2).map_err(db_err)?)?,
        actor_id: row.get(3).map_err(db_err)?,
        action: row.get(4).map_err(db_err)?,
        instance_id: instance_id
            .as_deref()
            .map(|text| uuid_from_text(text, "instance"))
            .transpose()?
            .map(UuidInto::into_id),
        metadata: deser(&metadata)?,
        ip_address: ip_address
            .as_deref()
            .map(|text| {
                text.parse().map_err(|e| {
                    PersistenceError::serialization(format!("bad audit ip {text:?}: {e}"))
                })
            })
            .transpose()?,
        user_agent: row.get(8).map_err(db_err)?,
    })
}

fn row_to_extension(row: &rusqlite::Row<'_>) -> Result<ExtensionRegistration, PersistenceError> {
    let manifest: String = row.get(1).map_err(db_err)?;
    let installed_at: String = row.get(3).map_err(db_err)?;
    let granted: String = row.get(5).map_err(db_err)?;
    let signature: Option<String> = row.get(6).map_err(db_err)?;
    Ok(ExtensionRegistration {
        id: uuid_from_text(&row.get::<_, String>(0).map_err(db_err)?, "extension")?.into(),
        manifest: deser(&manifest)?,
        version: row.get(2).map_err(db_err)?,
        installed_at: parse_ts(&installed_at)?,
        enabled: row.get(4).map_err(db_err)?,
        granted_permissions: deser::<PermissionSet>(&granted)?,
        signature: signature
            .as_deref()
            .map(deser::<ExtensionSignature>)
            .transpose()?,
    })
}
