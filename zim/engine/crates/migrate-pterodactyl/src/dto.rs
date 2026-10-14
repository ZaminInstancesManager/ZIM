//! Panel row DTOs: the §2.3 source tables as they exist in the
//! Pterodactyl Panel's MySQL schema (verified at the pinned Panel
//! revision v1.15.1 against
//! `upstream/pterodactyl-panel/database/migrations/` and the models'
//! `@property` docblocks per
//! `docs/audits/PTERODACTYL_ARCHITECTURE.md` §7.1.3).
//!
//! The DTOs are the *only* place the Panel's column vocabulary
//! exists in the engine; [`crate::mapping`] translates them into
//! the ZIM domain immediately after load, so no Panel shape leaks
//! past the importer.
//!
//! Leniency contract (mirroring the egg importer's): a row that is
//! structurally present maps unless it violates a domain invariant
//! or a referenced row is missing — everything uncertain becomes a
//! skip-with-reason or a warning in the import report, never a
//! silent behavior change and never a wholesale import failure.
//!
//! Two column-shape quirks are absorbed here:
//!
//! - *Booleans*: MySQL `TINYINT(0/1)` fixtures may carry `0`/`1`,
//!   `true`/`false`, or `"0"`/`"1"` — [`de_bool`] and
//!   [`de_opt_bool`] accept all three.
//! - *JSON columns*: the Panel stores `features`, `docker_images`,
//!   `permissions`, and `properties` as JSON; a dump may carry them
//!   embedded (a real JSON value) or as the raw JSON *text* —
//!   [`crate::mapping::normalize_json`] accepts both.

use serde::Deserialize;
use serde_json::Value;

// ─── Flexible primitives ──────────────────────────────────────────────

/// Deserializes a Panel boolean that may arrive as `true`, `0`, `1`,
/// `"0"`, or `"1"` (the MySQL `TINYINT` convention vs. a JSON dump).
pub(crate) fn de_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(bool_of(&Value::deserialize(deserializer)?))
}

/// [`de_bool`] for nullable columns (`bool|null` in the Panel).
pub(crate) fn de_opt_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(bool_of(&value)))
}

/// The shared boolean coercion (see [`de_bool`]).
fn bool_of(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_i64().unwrap_or(0) != 0,
        Value::String(text) => {
            let text = text.trim();
            text == "1" || text.eq_ignore_ascii_case("true")
        }
        _ => false,
    }
}

// ─── Users ────────────────────────────────────────────────────────────

/// One `users` row.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelUser {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// External identifier (billing integration), nullable.
    pub(crate) external_id: Option<String>,
    /// Panel user UUID (36-char).
    pub(crate) uuid: String,
    /// Login username.
    pub(crate) username: String,
    /// Login email.
    pub(crate) email: String,
    /// bcrypt password hash (`$2y$...`).
    pub(crate) password: Option<String>,
    /// Root administrator flag.
    #[serde(default, deserialize_with = "de_bool")]
    pub(crate) root_admin: bool,
}

// ─── Locations & nodes ────────────────────────────────────────────────

/// One `locations` row (folded — see the mapping docs).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelLocation {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// Short code (e.g. `eu-central`).
    pub(crate) short: String,
    /// Long label (e.g. `Frankfurt, Germany`).
    pub(crate) long: String,
}

/// One `nodes` row.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelNode {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// Node UUID (Panel 1.11+; NULL on very old panels).
    pub(crate) uuid: Option<String>,
    /// Display name.
    pub(crate) name: String,
    /// Location grouping id.
    pub(crate) location_id: i64,
    /// Fully-qualified domain name.
    pub(crate) fqdn: String,
    /// `http` or `https`.
    pub(crate) scheme: Option<String>,
    /// The Wings daemon's API port (the Panel column is camelCase
    /// `daemonListen`).
    #[serde(rename = "daemonListen", alias = "daemon_listen")]
    pub(crate) daemon_listen: Option<i64>,
}

// ─── Nests & eggs ─────────────────────────────────────────────────────

/// One `nests` row (counted + warned — ZIM's egg store has no
/// nests; only the display names surface, in the report's note).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelNest {
    /// Display name.
    pub(crate) name: String,
}

/// One `eggs` row.
///
/// The Panel splits what a legacy egg export carries as one
/// `config` object across the `config_files` / `config_startup` /
/// `config_stop` string columns (each holding JSON text), keeps
/// `docker_images` as a JSON object, and retains the deprecated
/// single-string `docker_image`. [`crate::mapping`] reassembles the
/// legacy shape and feeds it through the M9 importer.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelEgg {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// Egg UUID (the legacy-egg identity, used for dedup).
    pub(crate) uuid: String,
    /// Display name.
    pub(crate) name: String,
    /// Author attribution.
    pub(crate) author: Option<String>,
    /// Free-form description.
    pub(crate) description: Option<String>,
    /// Feature flags (JSON array or object, possibly raw text).
    pub(crate) features: Option<Value>,
    /// Deprecated single image reference.
    pub(crate) docker_image: Option<String>,
    /// Labeled images (JSON object `label → reference`).
    pub(crate) docker_images: Option<Value>,
    /// Per-file parser rules (JSON text).
    pub(crate) config_files: Option<String>,
    /// Startup detection (JSON text: `{"done": ...}`).
    pub(crate) config_startup: Option<String>,
    /// Stop behavior (JSON text: `{"type", "send"}`).
    pub(crate) config_stop: Option<String>,
    /// Startup command template.
    pub(crate) startup: Option<String>,
    /// The install script needs privileged execution (the legacy
    /// `priviledged_install_scripts` feature's storage form).
    #[serde(default, deserialize_with = "de_bool")]
    pub(crate) script_is_privileged: bool,
    /// Install script body.
    pub(crate) script_install: Option<String>,
    /// Install script entrypoint (`bash`).
    pub(crate) script_entry: Option<String>,
    /// Install script container image.
    pub(crate) script_container: Option<String>,
    /// Row creation time.
    pub(crate) created_at: Option<String>,
}

/// One `egg_variables` row.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelEggVariable {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// Owning egg id.
    pub(crate) egg_id: i64,
    /// Display name.
    pub(crate) name: String,
    /// Free-form description.
    pub(crate) description: Option<String>,
    /// Environment variable name.
    pub(crate) env_variable: String,
    /// Default value.
    pub(crate) default_value: Option<String>,
    /// Whether users see the variable.
    #[serde(default = "default_true", deserialize_with = "de_bool")]
    pub(crate) user_viewable: bool,
    /// Whether users may override it per server.
    #[serde(default, deserialize_with = "de_bool")]
    pub(crate) user_editable: bool,
    /// Validation rules — the Panel's pipe-separated string
    /// (`"required|string|max:20"`) or a JSON array.
    pub(crate) rules: Option<String>,
}

/// serde's `Default::default` cannot be used for `true` in attribute
/// position; this is the explicit form.
const fn default_true() -> bool {
    true
}

/// One `server_variables` row (a per-instance override).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelServerVariable {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// Owning server id.
    pub(crate) server_id: i64,
    /// The egg variable being overridden.
    pub(crate) variable_id: i64,
    /// The override value.
    #[serde(default)]
    pub(crate) variable_value: String,
}

// ─── Servers & allocations ────────────────────────────────────────────

/// One `servers` row (the `@property` field set from
/// `app/Models/Server.php`, verified per the audit's §7.1.3).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelServer {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// External identifier (billing integration), nullable.
    pub(crate) external_id: Option<String>,
    /// Long UUID — becomes the ZIM `instance_id` (§4.3).
    pub(crate) uuid: String,
    /// Short UUID (8 hex chars; the Panel column is camelCase
    /// `uuidShort`).
    #[serde(rename = "uuidShort", alias = "uuid_short")]
    pub(crate) uuid_short: String,
    /// Placement node.
    pub(crate) node_id: i64,
    /// Display name.
    pub(crate) name: String,
    /// Free-form description.
    pub(crate) description: Option<String>,
    /// Lifecycle status string (nullable in the Panel).
    pub(crate) status: Option<String>,
    /// Skip install/update scripts.
    #[serde(default, deserialize_with = "de_bool")]
    pub(crate) skip_scripts: bool,
    /// Owning user id.
    pub(crate) owner_id: i64,
    /// Memory limit (MiB).
    pub(crate) memory: i64,
    /// Swap limit (MiB; -1 = unlimited in the Panel).
    pub(crate) swap: i64,
    /// Disk quota (MiB).
    pub(crate) disk: i64,
    /// BlkIO weight (the Panel's `io`, 0 = unset).
    pub(crate) io: i64,
    /// CPU limit (percent; 0 = unlimited, 100 = one core).
    pub(crate) cpu: i64,
    /// CPU pinning (cpuset string), nullable.
    pub(crate) threads: Option<String>,
    /// OOM killer disabled for this server's container.
    #[serde(default, deserialize_with = "de_bool")]
    pub(crate) oom_disabled: bool,
    /// Primary allocation id.
    pub(crate) allocation_id: i64,
    /// Egg id (nullable mid-install in the Panel).
    pub(crate) egg_id: Option<i64>,
    /// Startup command template (per-server override).
    pub(crate) startup: Option<String>,
    /// Docker image (per-server override).
    pub(crate) image: Option<String>,
    /// Max self-assigned allocations (nullable = admin unlimited).
    pub(crate) allocation_limit: Option<i64>,
    /// Max databases (nullable = feature off).
    pub(crate) database_limit: Option<i64>,
    /// Max backups.
    pub(crate) backup_limit: Option<i64>,
    /// When the install last completed.
    pub(crate) installed_at: Option<String>,
    /// Row creation time.
    pub(crate) created_at: Option<String>,
    /// Row last-update time.
    pub(crate) updated_at: Option<String>,
}

/// One `allocations` row.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelAllocation {
    /// Panel auto-increment id (preserved as ZIM's `AllocationId`).
    pub(crate) id: i64,
    /// Owning node id.
    pub(crate) node_id: i64,
    /// IP address.
    pub(crate) ip: String,
    /// Port.
    pub(crate) port: i64,
    /// Assigned server id (nullable = free allocation).
    pub(crate) server_id: Option<i64>,
    /// Operator note.
    pub(crate) notes: Option<String>,
}

// ─── Subusers ─────────────────────────────────────────────────────────

/// One `subusers` row (→ a ZIM permissions row).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelSubuser {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// The granted user.
    pub(crate) user_id: i64,
    /// The granted server.
    pub(crate) server_id: i64,
    /// Granted permission names (JSON array; `*` widens to all).
    pub(crate) permissions: Option<Value>,
}

// ─── Schedules ────────────────────────────────────────────────────────

/// One `schedules` row (five separate cron part columns in the
/// Panel; joined into ZIM's single 5-field expression).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelSchedule {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// Owning server id.
    pub(crate) server_id: i64,
    /// Display name.
    pub(crate) name: String,
    /// Minute field.
    #[serde(default)]
    pub(crate) cron_minute: String,
    /// Hour field.
    #[serde(default)]
    pub(crate) cron_hour: String,
    /// Day-of-month field.
    #[serde(default)]
    pub(crate) cron_day_of_month: String,
    /// Month field.
    #[serde(default)]
    pub(crate) cron_month: String,
    /// Day-of-week field.
    #[serde(default)]
    pub(crate) cron_day_of_week: String,
    /// Whether the schedule is armed.
    #[serde(default, deserialize_with = "de_bool")]
    pub(crate) is_active: bool,
    /// Fire only while the workload is online.
    #[serde(default, deserialize_with = "de_bool")]
    pub(crate) only_when_online: bool,
    /// Last fire time.
    pub(crate) last_run_at: Option<String>,
    /// Next armed fire time.
    pub(crate) next_run_at: Option<String>,
}

/// One `schedule_tasks` row.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelScheduleTask {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// Owning schedule id.
    pub(crate) schedule_id: i64,
    /// Execution order within the schedule.
    pub(crate) sequence_id: Option<i64>,
    /// `power` or `send_command`.
    pub(crate) action: String,
    /// The power action name or the console line.
    pub(crate) payload: String,
    /// Seconds to wait before this task (dropped — warned).
    pub(crate) time_offset: Option<i64>,
    /// Continue on failure (not modeled — warned).
    #[serde(default, deserialize_with = "de_bool")]
    pub(crate) continue_on_failure: bool,
}

// ─── Backups & transfers ──────────────────────────────────────────────

/// One `backups` row.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelBackup {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// Owning server id.
    pub(crate) server_id: i64,
    /// Backup UUID (the archive's stem).
    pub(crate) uuid: String,
    /// Whether the archive was sealed successfully.
    #[serde(default, deserialize_with = "de_bool")]
    pub(crate) is_successful: bool,
    /// Locked against deletion.
    #[serde(default, deserialize_with = "de_bool")]
    pub(crate) is_locked: bool,
    /// Display name (not modeled in ZIM — folded, warned).
    pub(crate) name: Option<String>,
    /// sha256 hex digest.
    pub(crate) checksum: Option<String>,
    /// Archive size in bytes.
    pub(crate) bytes: Option<i64>,
    /// When the archive was sealed.
    pub(crate) completed_at: Option<String>,
    /// Soft-delete marker (skipped when set).
    pub(crate) deleted_at: Option<String>,
    /// Row creation time.
    pub(crate) created_at: Option<String>,
}

/// One `server_transfers` row.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelServerTransfer {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// Owning server id.
    pub(crate) server_id: i64,
    /// Source node id.
    pub(crate) old_node: i64,
    /// Destination node id.
    pub(crate) new_node: i64,
    /// `true`/`false` when the transfer reached an outcome; `null`
    /// while in flight (imported as failed — an in-flight Panel
    /// transfer cannot be resumed by ZIM's two-phase commit).
    #[serde(default, deserialize_with = "de_opt_bool")]
    pub(crate) successful: Option<bool>,
    /// Row creation time.
    pub(crate) created_at: Option<String>,
}

// ─── Activity logs ────────────────────────────────────────────────────

/// One `activity_logs` row (→ a ZIM audit row).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelActivityLog {
    /// Panel auto-increment id.
    pub(crate) id: i64,
    /// Event name (the Panel's colon-dotted vocabulary, e.g.
    /// `server:backup.start`).
    pub(crate) event: String,
    /// Client IP (nullable).
    pub(crate) ip: Option<String>,
    /// Human description (folded into the audit metadata).
    pub(crate) description: Option<String>,
    /// Actor kind (`user`, `api_key`, ...; nullable).
    pub(crate) actor_type: Option<String>,
    /// Actor user id (nullable).
    pub(crate) actor_id: Option<i64>,
    /// Structured properties (JSON; possibly non-JSON text in old
    /// rows — §2.6's string fallback).
    pub(crate) properties: Option<Value>,
    /// When the event happened (the Panel's `timestamp` column).
    pub(crate) timestamp: Option<String>,
}

/// One `activity_log_subjects` row (the polymorphic link that tells
/// an activity log which server — if any — it targeted).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PanelActivitySubject {
    /// Owning activity log id.
    pub(crate) activity_log_id: i64,
    /// Laravel morph type (`server`, `user`, ...).
    pub(crate) subject_type: String,
    /// Morph key (the server id for `server` subjects).
    pub(crate) subject_id: Option<i64>,
}

/// Whether a subject row points at a server (the Panel stores the
/// Laravel morph alias; both `server` and the fully-qualified
/// `App\Models\Server` form appear in the wild).
pub(crate) fn subject_is_server(subject_type: &str) -> bool {
    let lowered = subject_type.to_ascii_lowercase();
    lowered == "server" || lowered.ends_with("\\server") || lowered.ends_with("::server")
}
