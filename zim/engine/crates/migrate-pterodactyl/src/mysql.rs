//! The live MySQL panel source (feature `mysql`).
//!
//! Connects read-only, loads every §2.3 table in one snapshot
//! (ordered by id for deterministic mapping), and converts JSON
//! and DATETIME columns into the DTO shapes the fixture source
//! produces, so the mapping layer cannot tell the two sources
//! apart.
//!
//! The connection is opened with a plain DSN
//! (`mysql://user:pass@host:3306/panel`); the importer never
//! writes (it only `SELECT`s), and the Panel's data is read once
//! into memory per the [`crate::source`] module's snapshot
//! rationale.

use mysql::prelude::Queryable;
use mysql::{Conn, Opts, Row};
use serde_json::Value;

use crate::dto::*;
use crate::source::{PanelSource, PanelSourceError, PanelTables};

/// A live panel database reached over MySQL.
pub struct MysqlPanelSource {
    opts: Opts,
}

impl MysqlPanelSource {
    /// Wrap a MySQL DSN (`mysql://user:pass@host:port/database`).
    pub fn from_dsn(dsn: &str) -> Result<Self, PanelSourceError> {
        let opts = Opts::from_url(dsn).map_err(|e| {
            PanelSourceError::Read(format!("the DSN {dsn:?} is not a valid MySQL URL: {e}"))
        })?;
        Ok(Self { opts })
    }
}

impl PanelSource for MysqlPanelSource {
    fn load(&self) -> Result<PanelTables, PanelSourceError> {
        let mut conn = Conn::new(self.opts.clone())
            .map_err(|e| PanelSourceError::Read(format!("cannot connect: {e}")))?;
        let mut tables = PanelTables {
            users: load_users(&mut conn)?,
            servers: load_servers(&mut conn)?,
            allocations: load_allocations(&mut conn)?,
            nodes: load_nodes(&mut conn)?,
            locations: load_locations(&mut conn)?,
            nests: load_nests(&mut conn)?,
            eggs: load_eggs(&mut conn)?,
            egg_variables: load_egg_variables(&mut conn)?,
            server_variables: load_server_variables(&mut conn)?,
            subusers: load_subusers(&mut conn)?,
            schedules: load_schedules(&mut conn)?,
            schedule_tasks: load_schedule_tasks(&mut conn)?,
            backups: load_backups(&mut conn)?,
            activity_logs: load_activity_logs(&mut conn)?,
            activity_log_subjects: load_activity_subjects(&mut conn)?,
            server_transfers: load_server_transfers(&mut conn)?,
            ..Default::default()
        };
        for table in DEFERRED_TABLES {
            if let Ok(rows) = count(&mut conn, table) {
                if rows > 0 {
                    tables
                        .deferred
                        .insert((*table).to_owned(), Value::from(rows));
                }
            }
        }
        Ok(tables)
    }
}

/// The deferred tables the report counts (§2.3's unmapped set).
const DEFERRED_TABLES: &[&str] = &[
    "databases",
    "database_hosts",
    "mounts",
    "mount_node",
    "mount_server",
    "api_keys",
    "settings",
];

// ─── Row extraction helpers ──────────────────────────────────────────
//
// Every helper is lenient by design: a NULL or a value with an
// unexpected wire shape degrades to the DTO's default rather than
// panicking (the report narrates nothing here — the mapping layer's
// validation is the row's real gate).

fn text(row: &Row, column: &str) -> String {
    row.get_opt::<String, _>(column)
        .and_then(|result| result.ok())
        .unwrap_or_default()
}

fn opt_text(row: &Row, column: &str) -> Option<String> {
    row.get_opt::<String, _>(column)
        .and_then(|result| result.ok())
}

fn int(row: &Row, column: &str) -> i64 {
    row.get_opt::<i64, _>(column)
        .and_then(|result| result.ok())
        .unwrap_or_default()
}

fn opt_int(row: &Row, column: &str) -> Option<i64> {
    row.get_opt::<i64, _>(column).and_then(|result| result.ok())
}

fn flag(row: &Row, column: &str) -> bool {
    int(row, column) != 0
}

fn opt_flag(row: &Row, column: &str) -> Option<bool> {
    opt_int(row, column).map(|value| value != 0)
}

/// A JSON column: the driver hands back the JSON text; embedded
/// and malformed values both degrade into `Value::String` (the
/// mapping layer's `normalize_json` handles both).
fn json_column(row: &Row, column: &str) -> Option<Value> {
    opt_text(row, column)
        .map(|raw| serde_json::from_str::<Value>(&raw).unwrap_or(Value::String(raw)))
}

/// A DATETIME column rendered in the Panel's text form (the same
/// shape the fixture source carries).
fn datetime(row: &Row, column: &str) -> Option<String> {
    row.get_opt::<chrono::NaiveDateTime, _>(column)
        .and_then(|result| result.ok())
        .map(|moment| moment.format("%Y-%m-%d %H:%M:%S").to_string())
}

/// Runs a whole-table SELECT and maps every row.
fn load<T>(conn: &mut Conn, sql: &str, map: fn(&Row) -> T) -> Result<Vec<T>, PanelSourceError> {
    let rows: Vec<Row> = conn
        .exec(sql, ())
        .map_err(|e| PanelSourceError::Read(format!("{e}")))?;
    Ok(rows.iter().map(map).collect())
}

fn count(conn: &mut Conn, table: &str) -> Result<u64, PanelSourceError> {
    conn.exec_first::<u64, _, _>(format!("SELECT COUNT(*) FROM `{table}`"), ())
        .map_err(|e| PanelSourceError::Read(format!("{e}")))
        .map(Option::unwrap_or_default)
}

// ─── Per-table loaders ───────────────────────────────────────────────

fn load_users(conn: &mut Conn) -> Result<Vec<PanelUser>, PanelSourceError> {
    load(
        conn,
        "SELECT id, external_id, uuid, username, email, password, root_admin \
         FROM users ORDER BY id",
        |row| PanelUser {
            id: int(row, "id"),
            external_id: opt_text(row, "external_id"),
            uuid: text(row, "uuid"),
            username: text(row, "username"),
            email: text(row, "email"),
            password: opt_text(row, "password"),
            root_admin: flag(row, "root_admin"),
        },
    )
}

fn load_servers(conn: &mut Conn) -> Result<Vec<PanelServer>, PanelSourceError> {
    load(
        conn,
        "SELECT id, external_id, uuid, uuidShort, node_id, name, description, status, \
         skip_scripts, owner_id, memory, swap, disk, io, cpu, threads, oom_disabled, \
         allocation_id, egg_id, startup, image, allocation_limit, database_limit, \
         backup_limit, installed_at, created_at, updated_at \
         FROM servers ORDER BY id",
        |row| PanelServer {
            id: int(row, "id"),
            external_id: opt_text(row, "external_id"),
            uuid: text(row, "uuid"),
            uuid_short: text(row, "uuidShort"),
            node_id: int(row, "node_id"),
            name: text(row, "name"),
            description: opt_text(row, "description"),
            status: opt_text(row, "status"),
            skip_scripts: flag(row, "skip_scripts"),
            owner_id: int(row, "owner_id"),
            memory: int(row, "memory"),
            swap: int(row, "swap"),
            disk: int(row, "disk"),
            io: int(row, "io"),
            cpu: int(row, "cpu"),
            threads: opt_text(row, "threads"),
            oom_disabled: flag(row, "oom_disabled"),
            allocation_id: int(row, "allocation_id"),
            egg_id: opt_int(row, "egg_id"),
            startup: opt_text(row, "startup"),
            image: opt_text(row, "image"),
            allocation_limit: opt_int(row, "allocation_limit"),
            database_limit: opt_int(row, "database_limit"),
            backup_limit: opt_int(row, "backup_limit"),
            installed_at: datetime(row, "installed_at"),
            created_at: datetime(row, "created_at"),
            updated_at: datetime(row, "updated_at"),
        },
    )
}

fn load_allocations(conn: &mut Conn) -> Result<Vec<PanelAllocation>, PanelSourceError> {
    load(
        conn,
        "SELECT id, node_id, ip, port, server_id, notes FROM allocations ORDER BY id",
        |row| PanelAllocation {
            id: int(row, "id"),
            node_id: int(row, "node_id"),
            ip: text(row, "ip"),
            port: int(row, "port"),
            server_id: opt_int(row, "server_id"),
            notes: opt_text(row, "notes"),
        },
    )
}

fn load_nodes(conn: &mut Conn) -> Result<Vec<PanelNode>, PanelSourceError> {
    load(
        conn,
        "SELECT id, uuid, name, location_id, fqdn, scheme, daemonListen FROM nodes ORDER BY id",
        |row| PanelNode {
            id: int(row, "id"),
            uuid: opt_text(row, "uuid"),
            name: text(row, "name"),
            location_id: int(row, "location_id"),
            fqdn: text(row, "fqdn"),
            scheme: opt_text(row, "scheme"),
            daemon_listen: opt_int(row, "daemonListen"),
        },
    )
}

fn load_locations(conn: &mut Conn) -> Result<Vec<PanelLocation>, PanelSourceError> {
    load(
        conn,
        "SELECT id, short, long FROM locations ORDER BY id",
        |row| PanelLocation {
            id: int(row, "id"),
            short: text(row, "short"),
            long: text(row, "long"),
        },
    )
}

fn load_nests(conn: &mut Conn) -> Result<Vec<PanelNest>, PanelSourceError> {
    load(conn, "SELECT name FROM nests ORDER BY id", |row| {
        PanelNest {
            name: text(row, "name"),
        }
    })
}

fn load_eggs(conn: &mut Conn) -> Result<Vec<PanelEgg>, PanelSourceError> {
    load(
        conn,
        "SELECT id, uuid, name, author, description, features, docker_image, \
         docker_images, config_files, config_startup, config_stop, startup, \
         script_is_privileged, script_install, script_entry, script_container, created_at \
         FROM eggs ORDER BY id",
        |row| PanelEgg {
            id: int(row, "id"),
            uuid: text(row, "uuid"),
            name: text(row, "name"),
            author: opt_text(row, "author"),
            description: opt_text(row, "description"),
            features: json_column(row, "features"),
            docker_image: opt_text(row, "docker_image"),
            docker_images: json_column(row, "docker_images"),
            config_files: opt_text(row, "config_files"),
            config_startup: opt_text(row, "config_startup"),
            config_stop: opt_text(row, "config_stop"),
            startup: opt_text(row, "startup"),
            script_is_privileged: flag(row, "script_is_privileged"),
            script_install: opt_text(row, "script_install"),
            script_entry: opt_text(row, "script_entry"),
            script_container: opt_text(row, "script_container"),
            created_at: datetime(row, "created_at"),
        },
    )
}

fn load_egg_variables(conn: &mut Conn) -> Result<Vec<PanelEggVariable>, PanelSourceError> {
    load(
        conn,
        "SELECT id, egg_id, name, description, env_variable, default_value, user_viewable, \
         user_editable, rules FROM egg_variables ORDER BY id",
        |row| PanelEggVariable {
            id: int(row, "id"),
            egg_id: int(row, "egg_id"),
            name: text(row, "name"),
            description: opt_text(row, "description"),
            env_variable: text(row, "env_variable"),
            default_value: opt_text(row, "default_value"),
            user_viewable: flag(row, "user_viewable"),
            user_editable: flag(row, "user_editable"),
            rules: opt_text(row, "rules"),
        },
    )
}

fn load_server_variables(conn: &mut Conn) -> Result<Vec<PanelServerVariable>, PanelSourceError> {
    load(
        conn,
        "SELECT id, server_id, variable_id, variable_value FROM server_variables ORDER BY id",
        |row| PanelServerVariable {
            id: int(row, "id"),
            server_id: int(row, "server_id"),
            variable_id: int(row, "variable_id"),
            variable_value: text(row, "variable_value"),
        },
    )
}

fn load_subusers(conn: &mut Conn) -> Result<Vec<PanelSubuser>, PanelSourceError> {
    load(
        conn,
        "SELECT id, user_id, server_id, permissions FROM subusers ORDER BY id",
        |row| PanelSubuser {
            id: int(row, "id"),
            user_id: int(row, "user_id"),
            server_id: int(row, "server_id"),
            permissions: json_column(row, "permissions"),
        },
    )
}

fn load_schedules(conn: &mut Conn) -> Result<Vec<PanelSchedule>, PanelSourceError> {
    load(
        conn,
        "SELECT id, server_id, name, cron_minute, cron_hour, cron_day_of_month, cron_month, \
         cron_day_of_week, is_active, only_when_online, last_run_at, next_run_at \
         FROM schedules ORDER BY id",
        |row| PanelSchedule {
            id: int(row, "id"),
            server_id: int(row, "server_id"),
            name: text(row, "name"),
            cron_minute: text(row, "cron_minute"),
            cron_hour: text(row, "cron_hour"),
            cron_day_of_month: text(row, "cron_day_of_month"),
            cron_month: text(row, "cron_month"),
            cron_day_of_week: text(row, "cron_day_of_week"),
            is_active: flag(row, "is_active"),
            only_when_online: flag(row, "only_when_online"),
            last_run_at: datetime(row, "last_run_at"),
            next_run_at: datetime(row, "next_run_at"),
        },
    )
}

fn load_schedule_tasks(conn: &mut Conn) -> Result<Vec<PanelScheduleTask>, PanelSourceError> {
    load(
        conn,
        "SELECT id, schedule_id, sequence_id, action, payload, time_offset, \
         continue_on_failure FROM schedule_tasks ORDER BY id",
        |row| PanelScheduleTask {
            id: int(row, "id"),
            schedule_id: int(row, "schedule_id"),
            sequence_id: opt_int(row, "sequence_id"),
            action: text(row, "action"),
            payload: text(row, "payload"),
            time_offset: opt_int(row, "time_offset"),
            continue_on_failure: flag(row, "continue_on_failure"),
        },
    )
}

fn load_backups(conn: &mut Conn) -> Result<Vec<PanelBackup>, PanelSourceError> {
    load(
        conn,
        "SELECT id, server_id, uuid, is_successful, is_locked, name, checksum, bytes, \
         completed_at, deleted_at, created_at FROM backups ORDER BY id",
        |row| PanelBackup {
            id: int(row, "id"),
            server_id: int(row, "server_id"),
            uuid: text(row, "uuid"),
            is_successful: flag(row, "is_successful"),
            is_locked: flag(row, "is_locked"),
            name: opt_text(row, "name"),
            checksum: opt_text(row, "checksum"),
            bytes: opt_int(row, "bytes"),
            completed_at: datetime(row, "completed_at"),
            deleted_at: datetime(row, "deleted_at"),
            created_at: datetime(row, "created_at"),
        },
    )
}

fn load_activity_logs(conn: &mut Conn) -> Result<Vec<PanelActivityLog>, PanelSourceError> {
    load(
        conn,
        "SELECT id, event, ip, description, actor_type, actor_id, properties, timestamp \
         FROM activity_logs ORDER BY id",
        |row| PanelActivityLog {
            id: int(row, "id"),
            event: text(row, "event"),
            ip: opt_text(row, "ip"),
            description: opt_text(row, "description"),
            actor_type: opt_text(row, "actor_type"),
            actor_id: opt_int(row, "actor_id"),
            properties: json_column(row, "properties"),
            timestamp: datetime(row, "timestamp"),
        },
    )
}

fn load_activity_subjects(conn: &mut Conn) -> Result<Vec<PanelActivitySubject>, PanelSourceError> {
    load(
        conn,
        "SELECT activity_log_id, subject_type, subject_id FROM activity_log_subjects \
         ORDER BY activity_log_id",
        |row| PanelActivitySubject {
            activity_log_id: int(row, "activity_log_id"),
            subject_type: text(row, "subject_type"),
            subject_id: opt_int(row, "subject_id"),
        },
    )
}

fn load_server_transfers(conn: &mut Conn) -> Result<Vec<PanelServerTransfer>, PanelSourceError> {
    load(
        conn,
        "SELECT id, server_id, old_node, new_node, successful, created_at \
         FROM server_transfers ORDER BY id",
        |row| PanelServerTransfer {
            id: int(row, "id"),
            server_id: int(row, "server_id"),
            old_node: int(row, "old_node"),
            new_node: int(row, "new_node"),
            successful: opt_flag(row, "successful"),
            created_at: datetime(row, "created_at"),
        },
    )
}
