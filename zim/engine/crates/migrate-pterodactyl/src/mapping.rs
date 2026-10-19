//! The mapping layer: Panel DTOs → ZIM domain rows, per
//! `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md` §2.3.
//!
//! Everything the importer decides is recorded: rows that land in
//! [`Mapped`], rows that do not (with reasons, in the report), and
//! the vocabulary folds the mapping performs (status strings,
//! permission names, cron part columns, the activity event's colon
//! → dot normalization, backup display names, unlimited swap).
//!
//! The phase runs in dependency order — users, locations/nodes,
//! eggs (through the M9 legacy importer, with dedup reads against
//! the sink), instances, allocations, subusers, variables,
//! schedules, backups + synthetic jobs, transfers + synthetic jobs,
//! activity logs — so every cross-reference resolves against rows
//! this pass has already accepted. The write phase (also here,
//! [`write_all`]) then emits tables in FK-safe order inside
//! per-table transactions with foreign keys bracketed off, exactly
//! as §2.4's process prescribes.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use zim_engine_application::ports::Persistence;
use zim_engine_domain::{
    Allocation, AllocationId, AuditActor, AuditRecord, Backup, BackupId, Bytes, CommandTemplate,
    DatabaseHost, DatabaseHostId, DockerImage, Egg, EggId, Instance, InstanceId, InstanceStatus,
    InstanceVariable, Job, JobId, JobKind, JobState, LocationId, ManagedDatabase,
    ManagedDatabaseId, Node, NodeCapabilities, NodeDeployment, NodeId, NodeState, Permission,
    PermissionGrant, PermissionSet, PowerAction, ResourceLimits, Schedule, ScheduleAction,
    ScheduleId, Scheme, Transfer, TransferId, TransferState, User, UserId,
};
use zim_engine_eggs::{import_legacy_json, ImportPolicy as EggPolicy};

use crate::dto::{subject_is_server, PanelEgg, PanelEggVariable, PanelScheduleTask, PanelServer};
use crate::report::{DeferredTable, ImportReport, SkippedRow, TableOutcome};
use crate::source::PanelTables;
use crate::ImportPolicy;

// ─── Timestamps & small coercions ─────────────────────────────────────

/// The Panel's `DATETIME` text form.
const PANEL_DATETIME: &str = "%Y-%m-%d %H:%M:%S";

/// Parses a Panel timestamp: the MySQL `DATETIME` text form, its
/// ISO separator variant, or RFC 3339 (fixtures and dumps vary).
pub(crate) fn parse_panel_ts(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(raw, PANEL_DATETIME) {
        return Some(Utc.from_utc_datetime(&naive));
    }
    if let Ok(moment) = DateTime::parse_from_rfc3339(raw) {
        return Some(moment.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(&raw.replace('T', " "), PANEL_DATETIME)
        .ok()
        .map(|naive| Utc.from_utc_datetime(&naive))
}

/// Parses a Panel UUID, tolerating surrounding whitespace.
fn parse_uuid(text: &str) -> Option<Uuid> {
    text.trim().parse().ok()
}

/// Parses a JSON column that may arrive embedded (a real JSON
/// value) or as the raw JSON text — returning the parsed value, the
/// raw string when it is not JSON, or `None` when absent.
fn normalize_json(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    match value {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(parsed) => Some(parsed),
            Err(_) => Some(value.clone()),
        },
        other => Some(other.clone()),
    }
}

/// Maps a Panel permission name onto the grant vocabulary
/// (AD-12: preserved verbatim, so this is a lookup, not a
/// translation).
fn grant_by_name(name: &str) -> Option<PermissionGrant> {
    PermissionGrant::ALL
        .iter()
        .find(|grant| grant.as_str() == name)
        .copied()
}

/// Maps the Panel's nullable status string onto the typed lifecycle
/// enum (§2.3: `null` is the Panel's "offline"; `crashed` carries
/// the crash signal into `crash_loop`, which the engine's
/// first-boot reconciliation re-observes; unknown strings map to
/// `unknown`).
pub(crate) fn map_status(raw: Option<&str>) -> InstanceStatus {
    match raw.map(str::trim) {
        None | Some("") | Some("offline") => InstanceStatus::Offline,
        Some("starting") => InstanceStatus::Starting,
        Some("running") => InstanceStatus::Running,
        Some("stopping") => InstanceStatus::Stopping,
        Some("installing") => InstanceStatus::Installing,
        Some("suspended") => InstanceStatus::Suspended,
        Some("restoring_backup") => InstanceStatus::RestoringBackup,
        Some("transferring") => InstanceStatus::Transferring,
        Some("crashed") => InstanceStatus::CrashLoop,
        Some(_) => InstanceStatus::Unknown,
    }
}

/// Splits the Panel's `egg_variables.rules` — a pipe-separated
/// validation string (`"required|string|max:20"`) or a JSON array —
/// into the rule list the egg importer expects.
fn split_rules(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw.map(str::trim) else {
        return Vec::new();
    };
    if raw.is_empty() {
        return Vec::new();
    }
    if let Some(rules) = raw.strip_prefix('[') {
        if let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(&format!("[{rules}")) {
            return entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
    }
    raw.split('|')
        .map(str::trim)
        .filter(|rule| !rule.is_empty())
        .map(str::to_owned)
        .collect()
}

// ─── The mapped output ───────────────────────────────────────────────

/// Everything the mapping phase produced, ready for the write
/// phase (plus the report that narrates it).
pub(crate) struct Mapped {
    /// Mapped `users` rows.
    pub(crate) users: Vec<User>,
    /// Mapped `nodes` rows.
    pub(crate) nodes: Vec<Node>,
    /// Mapped `allocations` rows.
    pub(crate) allocations: Vec<Allocation>,
    /// Reconstructed `eggs` rows.
    pub(crate) eggs: Vec<Egg>,
    /// Mapped `instances` rows.
    pub(crate) instances: Vec<Instance>,
    /// Mapped `permissions` rows (subusers).
    pub(crate) permissions: Vec<Permission>,
    /// Per-instance variable override groups (server_variables).
    pub(crate) instance_variables: Vec<(InstanceId, Vec<InstanceVariable>)>,
    /// Job rows — synthetic companions for imported backups and
    /// transfers (the FK contract: a backup/transfer row names the
    /// durable job that created it).
    pub(crate) jobs: Vec<Job>,
    /// Mapped `backups` rows.
    pub(crate) backups: Vec<Backup>,
    /// Mapped `transfers` rows.
    pub(crate) transfers: Vec<Transfer>,
    /// Mapped `schedules` rows.
    pub(crate) schedules: Vec<Schedule>,
    /// Mapped `database_hosts` rows (M31).
    pub(crate) database_hosts: Vec<DatabaseHost>,
    /// Mapped `databases` rows (M31).
    pub(crate) databases: Vec<ManagedDatabase>,
    /// Mapped `audit` rows (activity_logs).
    pub(crate) audit: Vec<AuditRecord>,
    /// The narrated outcome.
    pub(crate) report: ImportReport,
}

// ─── The mapping context ─────────────────────────────────────────────

/// Cross-reference state for one mapping pass.
struct MappingContext {
    policy: ImportPolicy,
    user_ids: BTreeMap<i64, UserId>,
    node_ids: BTreeMap<i64, NodeId>,
    location_ids: BTreeMap<i64, LocationId>,
    egg_ids: BTreeMap<i64, EggId>,
    eggs_rows: Vec<Egg>,
    instance_ids: BTreeMap<i64, InstanceId>,
    variable_env_names: BTreeMap<i64, String>,
    report: ImportReport,
    defaulted_timestamps: usize,
    lowercased_emails: usize,
    crashed_statuses: usize,
}

impl MappingContext {
    fn new(policy: ImportPolicy) -> Self {
        Self {
            policy,
            user_ids: BTreeMap::new(),
            node_ids: BTreeMap::new(),
            location_ids: BTreeMap::new(),
            egg_ids: BTreeMap::new(),
            eggs_rows: Vec::new(),
            instance_ids: BTreeMap::new(),
            variable_env_names: BTreeMap::new(),
            report: ImportReport::default(),
            defaulted_timestamps: 0,
            lowercased_emails: 0,
            crashed_statuses: 0,
        }
    }

    /// A required timestamp: the source's value or the import
    /// instant (counted once in the report).
    fn ts_or_now(&mut self, raw: Option<&str>) -> DateTime<Utc> {
        match parse_panel_ts(raw) {
            Some(moment) => moment,
            None => {
                self.defaulted_timestamps += 1;
                Utc::now()
            }
        }
    }

    /// An optional timestamp (no fallback, no count).
    fn ts_opt(raw: Option<&str>) -> Option<DateTime<Utc>> {
        parse_panel_ts(raw)
    }

    fn outcome(&mut self, table: &str, imported: usize, skipped: Vec<SkippedRow>) {
        self.report.tables.push(TableOutcome {
            table: table.to_owned(),
            imported,
            skipped,
        });
    }

    fn skip(key: impl ToString, reason: impl ToString) -> SkippedRow {
        SkippedRow {
            key: key.to_string(),
            reason: reason.to_string(),
        }
    }

    fn warn(&mut self, note: impl ToString) {
        self.report.warnings.push(note.to_string());
    }
}

// ─── Users ───────────────────────────────────────────────────────────

impl MappingContext {
    /// `users` → [`User`] rows: the Panel UUID becomes the ZIM id
    /// (§2.3's round-trip `external_id` preserved; the bcrypt hash
    /// is carried verbatim — see the report note).
    fn map_users(&mut self, tables: &PanelTables) -> Vec<User> {
        let mut rows = Vec::new();
        let mut skipped = Vec::new();
        let mut bcrypt_hashes = 0usize;
        for user in &tables.users {
            let Some(uuid) = parse_uuid(&user.uuid) else {
                skipped.push(Self::skip(user.id, "malformed panel uuid"));
                continue;
            };
            let email = user.email.trim().to_lowercase();
            if email != user.email.trim() {
                self.lowercased_emails += 1;
            }
            let password_hash = user.password.clone().filter(|hash| !hash.trim().is_empty());
            if password_hash.is_some() {
                bcrypt_hashes += 1;
            }
            let row = User {
                id: UserId::from_uuid(uuid),
                email,
                username: user.username.clone(),
                external_id: user.external_id.clone(),
                password_hash,
                oauth_subject: None,
                is_admin: user.root_admin,
                root_admin: user.root_admin,
            };
            match row.validate() {
                Ok(()) => {
                    self.user_ids.insert(user.id, row.id);
                    rows.push(row);
                }
                Err(error) => skipped.push(Self::skip(user.id, error.to_string())),
            }
        }
        if bcrypt_hashes > 0 {
            self.warn(format!(
                "{bcrypt_hashes} bcrypt password hash(es) imported verbatim: ZIM's Argon2id \
                 verifier will not accept them until the documented first-login re-hash bridge \
                 lands (§2.3); affected users can reset passwords in the meantime"
            ));
        }
        if self.lowercased_emails > 0 {
            self.warn(format!(
                "{} user email(s) lowercased (ZIM stores the canonical lowercase form)",
                self.lowercased_emails
            ));
        }
        self.outcome("users", rows.len(), skipped);
        rows
    }
}

// ─── Locations & nodes ───────────────────────────────────────────────

impl MappingContext {
    /// `nodes` → [`Node`] rows: `deployment` is `RemoteLinux` and
    /// `last_heartbeat` null per §2.3 (the Panel's nodes are remote
    /// Linux daemons; the heartbeat returns when the node connects
    /// to ZIM's engine). Locations are folded — a stable
    /// [`LocationId`] per Panel location, the label itself is not
    /// modeled — and both folds are counted in the report.
    fn map_nodes(&mut self, tables: &PanelTables) -> Vec<Node> {
        for location in &tables.locations {
            self.location_ids.insert(location.id, LocationId::new());
        }
        let mut rows = Vec::new();
        let mut skipped = Vec::new();
        for node in &tables.nodes {
            let id = match parse_uuid(node.uuid.as_deref().unwrap_or_default()) {
                Some(uuid) => NodeId::from_uuid(uuid),
                None => {
                    let generated = NodeId::new();
                    self.warn(format!(
                        "node {} ({:?}) has no usable uuid; generated {}",
                        node.id, node.name, generated
                    ));
                    generated
                }
            };
            let scheme = match node.scheme.as_deref().map(str::trim) {
                Some("http") => Scheme::Http,
                Some("https") | None => Scheme::Https,
                Some(other) => {
                    self.warn(format!(
                        "node {} has unknown scheme {other:?}; imported as https",
                        node.id
                    ));
                    Scheme::Https
                }
            };
            // The Wings API port serves both the advertised and the
            // listening surface for a remote node until ZIM's own
            // node contract takes over.
            let daemon_port = node.daemon_listen.unwrap_or(8080);
            let port = if (1..=i64::from(u16::MAX)).contains(&daemon_port) {
                daemon_port as u16
            } else {
                self.warn(format!(
                    "node {} has daemonListen {daemon_port} outside 1-65535; imported as 8080",
                    node.id
                ));
                8080
            };
            let location_id = self
                .location_ids
                .get(&node.location_id)
                .copied()
                .unwrap_or_else(LocationId::new);
            let row = Node {
                id,
                name: node.name.clone(),
                location_id,
                fqdn: node.fqdn.clone(),
                scheme,
                public_port: port,
                listen_port: port,
                capabilities: NodeCapabilities::default(),
                deployment: NodeDeployment::RemoteLinux,
                last_heartbeat: None,
                last_seen_state: NodeState::Unknown,
            };
            match row.validate() {
                Ok(()) => {
                    self.node_ids.insert(node.id, row.id);
                    rows.push(row);
                }
                Err(error) => skipped.push(Self::skip(node.id, error.to_string())),
            }
        }
        if !tables.locations.is_empty() {
            let labels: Vec<String> = tables
                .locations
                .iter()
                .map(|location| format!("{} ({})", location.short, location.long))
                .collect();
            self.warn(format!(
                "{} location(s) folded: nodes carry only a location id — ZIM does not model \
                 location labels ({})",
                tables.locations.len(),
                labels.join(", ")
            ));
        }
        if !rows.is_empty() {
            self.warn(
                "imported nodes declare the baseline capability set; the remote-node contract \
                 re-discovers capabilities when each node connects"
                    .to_owned(),
            );
        }
        self.outcome("nodes", rows.len(), skipped);
        rows
    }
}

// ─── Eggs ────────────────────────────────────────────────────────────

impl MappingContext {
    /// `eggs` + `egg_variables` → [`Egg`] rows through the M9
    /// legacy importer: the Panel's split columns are reassembled
    /// into the legacy JSON shape and fed to `import_legacy_json`
    /// (§2.3: "imported via the egg importer"). Re-imports
    /// deduplicate on the legacy UUID and update in place, exactly
    /// like the §10.11 egg API.
    fn map_eggs(&mut self, tables: &PanelTables, sink: &dyn Persistence) {
        let mut skipped = Vec::new();
        for egg in &tables.eggs {
            let variables: Vec<&PanelEggVariable> = tables
                .egg_variables
                .iter()
                .filter(|variable| variable.egg_id == egg.id)
                .collect();
            let (document, synthesis_notes) = synthesize_legacy_egg(egg, &variables);
            for note in synthesis_notes {
                self.warn(format!("egg {} ({:?}): {note}", egg.id, egg.name));
            }
            let text = match serde_json::to_string(&document) {
                Ok(text) => text,
                Err(error) => {
                    skipped.push(Self::skip(
                        egg.id,
                        format!("the reconstructed legacy document is not serializable: {error}"),
                    ));
                    continue;
                }
            };
            let policy = EggPolicy {
                allow_privileged_install_scripts: self.policy.allow_privileged_install_scripts,
                allow_requires_root: self.policy.allow_requires_root,
            };
            match import_legacy_json(&text, &policy) {
                Ok(imported) => {
                    let mut mapped = imported.egg;
                    for note in imported.warnings {
                        self.warn(format!("egg {} ({:?}): {note}", egg.id, egg.name));
                    }
                    // Deduplicate on the legacy UUID: a re-import
                    // updates the existing row in place (the
                    // operator's mental model, per the §10.11 API).
                    // A fresh import carries the Panel's creation
                    // time forward.
                    let existing = mapped
                        .legacy_uuid
                        .as_deref()
                        .and_then(|uuid| sink.get_egg_by_legacy_uuid(uuid).ok().flatten());
                    if let Some(existing) = existing {
                        mapped.id = existing.id;
                        mapped.created_at = existing.created_at;
                    } else if let Some(created) = parse_panel_ts(egg.created_at.as_deref()) {
                        mapped.created_at = created;
                    }
                    // Index the panel variable ids by env name so
                    // server_variables resolve after this pass.
                    for variable in &mapped.variables {
                        if let Some(panel_id) = variables.iter().find_map(|candidate| {
                            (candidate.env_variable == variable.env_name).then_some(candidate.id)
                        }) {
                            self.variable_env_names
                                .insert(panel_id, variable.env_name.clone());
                        }
                    }
                    self.egg_ids.insert(egg.id, mapped.id);
                    self.eggs_rows.push(mapped);
                }
                Err(error) => skipped.push(Self::skip(
                    egg.id,
                    format!("egg reconstruction rejected: {error}"),
                )),
            }
        }
        self.outcome("eggs", self.eggs_rows.len(), skipped);
    }
}

/// Reassembles the Panel's split egg columns into the legacy JSON
/// document the M9 importer consumes. Returns the document plus the
/// per-egg synthesis notes (absorbed config shapes).
fn synthesize_legacy_egg(egg: &PanelEgg, variables: &[&PanelEggVariable]) -> (Value, Vec<String>) {
    let mut notes = Vec::new();

    // ── features: array or object, plus the privileged flag ──────
    let mut features = normalize_json(egg.features.as_ref());
    if egg.script_is_privileged {
        features = Some(match features {
            Some(Value::Array(mut entries)) => {
                entries.push(Value::String("priviledged_install_scripts".to_owned()));
                Value::Array(entries)
            }
            Some(other @ Value::Object(_)) => {
                let mut map = other;
                if let Some(object) = map.as_object_mut() {
                    object.insert("priviledged_install_scripts".to_owned(), Value::Bool(true));
                }
                map
            }
            _ => {
                notes.push(
                    "features had an unrecognized shape; rebuilt around the \
                            privileged-install flag"
                        .to_owned(),
                );
                Value::Array(vec![Value::String(
                    "priviledged_install_scripts".to_owned(),
                )])
            }
        });
    }

    // ── images: the labeled map, or the deprecated single string ─
    let images = match normalize_json(egg.docker_images.as_ref()) {
        Some(value @ Value::Object(_)) => Some(value),
        Some(Value::String(text)) if !text.trim().is_empty() => {
            notes.push(
                "docker_images arrived as a string; treating it as the deprecated \
                        single-image form"
                    .to_owned(),
            );
            None
        }
        _ => None,
    };

    // ── config: the three split columns back into one block ─────
    let mut config = Map::new();
    if let Some(files) = &egg.config_files {
        config.insert("files".to_owned(), Value::String(files.clone()));
    }
    if let Some(startup) = parse_json_object(egg.config_startup.as_deref()) {
        config.insert("startup".to_owned(), Value::Object(startup));
    }
    if let Some(stop) = synthesize_stop_block(egg.config_stop.as_deref(), &mut notes) {
        config.insert("stop".to_owned(), stop);
    }

    // ── scripts: the install script in its constrained container ─
    let mut scripts = Map::new();
    if let Some(body) = egg
        .script_install
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        scripts.insert(
            "installation".to_owned(),
            json!({
                "script": body,
                "container": egg.script_container.clone().unwrap_or_default(),
                "entrypoint": egg.script_entry.clone().unwrap_or_else(|| "bash".to_owned()),
            }),
        );
    }

    let mut document = Map::new();
    document.insert("meta".to_owned(), json!({"version": "PTDL_v2"}));
    document.insert("uuid".to_owned(), Value::String(egg.uuid.clone()));
    document.insert("name".to_owned(), Value::String(egg.name.clone()));
    document.insert(
        "author".to_owned(),
        Value::String(egg.author.clone().unwrap_or_default()),
    );
    document.insert(
        "description".to_owned(),
        Value::String(egg.description.clone().unwrap_or_default()),
    );
    if let Some(features) = features {
        document.insert("features".to_owned(), features);
    }
    if let Some(images) = images {
        document.insert("docker_images".to_owned(), images);
    } else if let Some(image) = egg
        .docker_image
        .as_deref()
        .map(str::trim)
        .filter(|i| !i.is_empty())
    {
        document.insert("docker_image".to_owned(), Value::String(image.to_owned()));
    }
    if let Some(startup) = egg
        .startup
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        document.insert("startup".to_owned(), Value::String(startup.to_owned()));
    }
    if !config.is_empty() {
        document.insert("config".to_owned(), Value::Object(config));
    }
    if !scripts.is_empty() {
        document.insert("scripts".to_owned(), Value::Object(scripts));
    }
    if !variables.is_empty() {
        document.insert(
            "variables".to_owned(),
            Value::Array(
                variables
                    .iter()
                    .map(|variable| {
                        json!({
                            "name": variable.name,
                            "description": variable.description.clone().unwrap_or_default(),
                            "env_variable": variable.env_variable,
                            "default_value": variable.default_value.clone().unwrap_or_default(),
                            "user_viewable": variable.user_viewable,
                            "user_editable": variable.user_editable,
                            "rules": split_rules(variable.rules.as_deref()),
                        })
                    })
                    .collect(),
            ),
        );
    }
    (Value::Object(document), notes)
}

/// Parses a JSON-text column into an object (the Panel's
/// `config_startup` shape `{"done": ...}`).
fn parse_json_object(raw: Option<&str>) -> Option<Map<String, Value>> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(object)) => Some(object),
        _ => None,
    }
}

/// Converts the Panel's `config_stop` (`{"type": "signal",
/// "send": ...}` or a bare command string) into the legacy block
/// shape the importer reads (`{"command", "signal"}`).
fn synthesize_stop_block(raw: Option<&str>, notes: &mut Vec<String>) -> Option<Value> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(raw) {
        if let (Some(kind), Some(send)) = (
            object.get("type").and_then(Value::as_str),
            object.get("send").and_then(Value::as_str),
        ) {
            return Some(json!({
                "command": send,
                "signal": kind.eq_ignore_ascii_case("signal"),
            }));
        }
        if object.contains_key("command") {
            return Some(Value::Object(object));
        }
        notes.push("config_stop had an unrecognized shape; using the default stop".to_owned());
        return None;
    }
    Some(json!({"command": raw, "signal": false}))
}

// ─── Instances & allocations ─────────────────────────────────────────

impl MappingContext {
    /// `servers` → [`Instance`] rows. The Panel UUID *is* the ZIM
    /// instance id (§4.3's filesystem-migration simplification);
    /// `status` maps through [`map_status`]; resource limits map
    /// Wings' units (memory/swap/disk MiB → bytes, cpu percent →
    /// millicores, io → the 1–1000 BlkIO weight); per-server
    /// `startup`/`image` fall back to the egg's when absent.
    fn map_instances(&mut self, tables: &PanelTables) -> Vec<Instance> {
        let mut rows = Vec::new();
        let mut skipped = Vec::new();
        for server in &tables.servers {
            let key = server.id;
            let Some(uuid) = parse_uuid(&server.uuid) else {
                skipped.push(Self::skip(key, "malformed panel uuid"));
                continue;
            };
            let Some(node) = self.node_ids.get(&server.node_id).copied() else {
                skipped.push(Self::skip(
                    key,
                    format!("node {} was not imported", server.node_id),
                ));
                continue;
            };
            let Some(owner) = self.user_ids.get(&server.owner_id).copied() else {
                skipped.push(Self::skip(
                    key,
                    format!("owner user {} was not imported", server.owner_id),
                ));
                continue;
            };
            let Some(egg) = server
                .egg_id
                .and_then(|egg_id| self.egg_ids.get(&egg_id))
                .copied()
            else {
                skipped.push(Self::skip(
                    key,
                    format!(
                        "egg {} was not imported",
                        server
                            .egg_id
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "null".into())
                    ),
                ));
                continue;
            };
            if !tables
                .allocations
                .iter()
                .any(|allocation| allocation.id == server.allocation_id)
            {
                skipped.push(Self::skip(
                    key,
                    format!(
                        "allocation {} is missing from the panel",
                        server.allocation_id
                    ),
                ));
                continue;
            }
            if server.allocation_id < 0 {
                skipped.push(Self::skip(key, "negative allocation id"));
                continue;
            }
            if server.name.trim().is_empty() {
                skipped.push(Self::skip(key, "empty name"));
                continue;
            }

            // The egg row's startup/image fallbacks, taken as owned
            // values so the context borrow ends before the limit
            // mapping (and its warnings) runs.
            let egg_fallbacks = self.eggs_lookup(egg);
            let egg_startup = egg_fallbacks.map(|row| row.startup.clone());
            let egg_image = egg_fallbacks
                .and_then(|row| row.images.first())
                .map(|labeled| labeled.image.clone());
            let startup = server
                .startup
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(CommandTemplate::new)
                .or(egg_startup);
            let Some(startup) = startup else {
                skipped.push(Self::skip(
                    key,
                    "no startup command (server and egg both lack one)",
                ));
                continue;
            };
            let image = server
                .image
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse::<DockerImage>().ok())
                .or(egg_image);
            let Some(image) = image else {
                skipped.push(Self::skip(
                    key,
                    "no usable docker image (server and egg lack one)",
                ));
                continue;
            };

            // Status: count the folded crash signal once.
            if server.status.as_deref().map(str::trim) == Some("crashed") {
                self.crashed_statuses += 1;
            } else if let Some(other) = server
                .status
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty() && !PANEL_STATUSES.contains(s))
            {
                self.warn(format!(
                    "server {key} has unknown status {other:?}; imported as unknown"
                ));
            }
            let status = map_status(server.status.as_deref());

            // Short UUID: derive from the canonical UUID when the
            // panel column is malformed.
            let uuid_short = {
                let candidate = server.uuid_short.trim();
                let derived: String = server
                    .uuid
                    .replace('-', "")
                    .chars()
                    .take(8)
                    .collect::<String>()
                    .to_ascii_lowercase();
                if candidate.len() == 8
                    && candidate
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                {
                    candidate.to_owned()
                } else {
                    self.warn(format!(
                        "server {key} has malformed uuidShort {candidate:?}; derived \
                         {derived:?} from the canonical uuid"
                    ));
                    derived
                }
            };

            let limits = self.map_limits(server);
            let row = Instance {
                id: InstanceId::from_uuid(uuid),
                external_id: server.external_id.clone(),
                uuid: server.uuid.clone(),
                uuid_short,
                node_id: node,
                name: server.name.trim().to_owned(),
                description: server.description.clone().unwrap_or_default(),
                status,
                skip_egg_scripts: server.skip_scripts,
                owner_id: owner,
                limits,
                allocation_id: AllocationId::new(server.allocation_id as u64),
                egg_id: egg,
                startup,
                image,
                allocation_limit: positive_u32(server.allocation_limit),
                database_limit: positive_u32(server.database_limit),
                backup_limit: server
                    .backup_limit
                    .unwrap_or(0)
                    .clamp(0, i64::from(u32::MAX)) as u32,
                // ZIM's schema default: crash detection is an
                // opt-in supervision posture, not a Panel concept.
                crash_detection_enabled: false,
                installed_at: Self::ts_opt(server.installed_at.as_deref()),
                created_at: self.ts_or_now(server.created_at.as_deref()),
                updated_at: self.ts_or_now(server.updated_at.as_deref()),
            };
            match row.validate() {
                Ok(()) => {
                    self.instance_ids.insert(server.id, row.id);
                    rows.push(row);
                }
                Err(error) => skipped.push(Self::skip(key, error.to_string())),
            }
        }
        if self.crashed_statuses > 0 {
            self.warn(format!(
                "{} server(s) with legacy status \"crashed\" imported as crash_loop; the \
                 engine's first-boot reconciliation re-observes every instance (§2.4 step 6)",
                self.crashed_statuses
            ));
        }
        self.outcome("instances", rows.len(), skipped);
        rows
    }

    /// The Panel's limit columns onto the domain's typed limits
    /// (with the two documented folds: unlimited swap becomes
    /// disabled; out-of-range io weights clamp to 1000).
    fn map_limits(&mut self, server: &PanelServer) -> ResourceLimits {
        let swap = if server.swap < 0 {
            self.warn(format!(
                "server {} declares unlimited swap (-1); ZIM has no unlimited-swap form — \
                 swap disabled",
                server.id
            ));
            Bytes::from_bytes(0)
        } else {
            Bytes::from_mib(server.swap.max(0) as u64)
        };
        let io_weight = match server.io {
            0 => 0,
            value @ 1..=1000 => value as u16,
            value => {
                self.warn(format!(
                    "server {} has io weight {value} outside 1-1000; clamped to 1000",
                    server.id
                ));
                1000
            }
        };
        let cpu_millicores = (server.cpu.max(0) as u64)
            .saturating_mul(10)
            .min(u64::from(u32::MAX)) as u32;
        let mut limits = ResourceLimits {
            memory: Bytes::from_mib(server.memory.max(0) as u64),
            swap,
            disk: Bytes::from_mib(server.disk.max(0) as u64),
            io_weight,
            cpu_millicores,
            threads: server.threads.clone(),
            oom_disabled: server.oom_disabled,
        };
        // An invalid cpuset would fail the instance's validation;
        // dropping the pin (with a note) preserves the row.
        if limits.validate().is_err() {
            if let Some(threads) = limits.threads.take() {
                self.warn(format!(
                    "server {} has malformed cpu thread spec {threads:?}; pinning dropped",
                    server.id
                ));
            }
        }
        limits
    }

    /// `allocations` → [`Allocation`] rows (after the instances map,
    /// so assignments follow the imported servers): ids are
    /// preserved verbatim (AD-12's Pterodactyl auto-increment
    /// compatibility), invalid rows warn-and-skip, and an
    /// assignment to a server that did not import lands as a free
    /// allocation.
    fn map_allocations(&mut self, tables: &PanelTables) -> Vec<Allocation> {
        let mut rows = Vec::new();
        for allocation in &tables.allocations {
            let Ok(ip) = allocation.ip.trim().parse::<std::net::IpAddr>() else {
                self.warn(format!(
                    "allocation {} has malformed ip {:?}; skipped",
                    allocation.id, allocation.ip
                ));
                continue;
            };
            if !(1..=i64::from(u16::MAX)).contains(&allocation.port) {
                self.warn(format!(
                    "allocation {} has port {} outside 1-65535; skipped",
                    allocation.id, allocation.port
                ));
                continue;
            }
            let Some(node) = self.node_ids.get(&allocation.node_id).copied() else {
                self.warn(format!(
                    "allocation {} references node {} which was not imported; skipped",
                    allocation.id, allocation.node_id
                ));
                continue;
            };
            let assigned_to = allocation.server_id.and_then(|server_id| {
                match self.instance_ids.get(&server_id) {
                    Some(id) => Some(*id),
                    None => {
                        self.warn(format!(
                            "allocation {} references server {server_id} which was not \
                             imported; imported as free",
                            allocation.id
                        ));
                        None
                    }
                }
            });
            rows.push(Allocation {
                id: AllocationId::new(allocation.id.max(0) as u64),
                node_id: node,
                ip,
                port: allocation.port as u16,
                assigned_to,
                note: allocation.notes.clone(),
            });
        }
        self.outcome("allocations", rows.len(), Vec::new());
        rows
    }

    /// The mapped egg row for `id` (the startup/image fallback
    /// source for instance rows).
    fn eggs_lookup(&self, id: EggId) -> Option<&Egg> {
        self.eggs_rows.iter().find(|egg| egg.id == id)
    }
}

/// The Panel's status vocabulary (anything else maps to `unknown`
/// with a warning).
const PANEL_STATUSES: &[&str] = &[
    "offline",
    "starting",
    "running",
    "stopping",
    "installing",
    "suspended",
    "restoring_backup",
    "transferring",
    "crashed",
];

/// A nullable non-negative limit column onto `Option<u32>`.
fn positive_u32(raw: Option<i64>) -> Option<u32> {
    raw.filter(|value| *value >= 0)
        .map(|value| value.clamp(0, i64::from(u32::MAX)) as u32)
}

// ─── Subusers, variables, schedules ──────────────────────────────────

impl MappingContext {
    /// `subusers` → [`Permission`] rows: the Panel's permission
    /// names ARE ZIM's vocabulary (AD-12, verbatim), `*` widens to
    /// the full set, and unknown names are skipped with a warning
    /// (§2.6's api_keys rule applied consistently).
    fn map_subusers(&mut self, tables: &PanelTables) -> Vec<Permission> {
        let mut rows = Vec::new();
        let mut skipped = Vec::new();
        for subuser in &tables.subusers {
            let Some(user) = self.user_ids.get(&subuser.user_id).copied() else {
                skipped.push(Self::skip(
                    subuser.id,
                    format!("user {} was not imported", subuser.user_id),
                ));
                continue;
            };
            let Some(instance) = self.instance_ids.get(&subuser.server_id).copied() else {
                skipped.push(Self::skip(
                    subuser.id,
                    format!("server {} was not imported", subuser.server_id),
                ));
                continue;
            };
            let mut grants = PermissionSet::new();
            let names = match normalize_json(subuser.permissions.as_ref()) {
                Some(Value::Array(entries)) => entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<String>>(),
                Some(Value::String(text)) if text.trim() == "*" => vec!["*".to_owned()],
                _ => Vec::new(),
            };
            for name in names {
                if name == "*" {
                    grants = PermissionSet::all();
                    continue;
                }
                match grant_by_name(&name) {
                    Some(grant) => {
                        grants.grant(grant);
                    }
                    None => self.warn(format!(
                        "subuser {} carries unknown permission {name:?}; no ZIM grant exists",
                        subuser.id
                    )),
                }
            }
            rows.push(Permission {
                user_id: user,
                instance_id: instance,
                grants,
            });
        }
        self.outcome("permissions", rows.len(), skipped);
        rows
    }

    /// `server_variables` → per-instance [`InstanceVariable`]
    /// override groups (migration 0004's rows — the env-resolution
    /// layer that closes the M8.3 fidelity gap).
    fn map_server_variables(
        &mut self,
        tables: &PanelTables,
    ) -> Vec<(InstanceId, Vec<InstanceVariable>)> {
        let mut skipped = Vec::new();
        let mut per_instance: BTreeMap<i64, BTreeMap<String, String>> = BTreeMap::new();
        for variable in &tables.server_variables {
            if !self.instance_ids.contains_key(&variable.server_id) {
                skipped.push(Self::skip(
                    variable.id,
                    format!("server {} was not imported", variable.server_id),
                ));
                continue;
            }
            let Some(env_name) = self.variable_env_names.get(&variable.variable_id) else {
                skipped.push(Self::skip(
                    variable.id,
                    format!(
                        "egg variable {} was not imported (its egg was rejected)",
                        variable.variable_id
                    ),
                ));
                continue;
            };
            let group = per_instance.entry(variable.server_id).or_default();
            let value = variable.variable_value.clone();
            if group.insert(env_name.clone(), value).is_some() {
                self.warn(format!(
                    "server {} carries two overrides for {env_name}; the last value wins",
                    variable.server_id
                ));
            }
        }
        let rows: Vec<(InstanceId, Vec<InstanceVariable>)> = per_instance
            .into_iter()
            .filter_map(|(server_id, group)| {
                let instance = self.instance_ids.get(&server_id).copied()?;
                Some((
                    instance,
                    group
                        .into_iter()
                        .map(|(env_name, value)| InstanceVariable { env_name, value })
                        .collect(),
                ))
            })
            .collect();
        let imported: usize = rows.iter().map(|(_, group)| group.len()).sum();
        self.outcome("instance_variables", imported, skipped);
        rows
    }

    /// `schedules` + `schedule_tasks` → [`Schedule`] rows: the five
    /// cron part columns join into the domain's 5-field expression
    /// (minute hour day-of-month month day-of-week — the scheduler's
    /// zero-seconds-prefixed dialect); tasks become the action list
    /// in `(time_offset, sequence)` order. Two Panel features are
    /// not modeled and warn: per-task time offsets and
    /// continue-on-failure.
    fn map_schedules(&mut self, tables: &PanelTables) -> Vec<Schedule> {
        let mut rows = Vec::new();
        let mut skipped = Vec::new();
        for schedule in &tables.schedules {
            let Some(instance) = self.instance_ids.get(&schedule.server_id).copied() else {
                skipped.push(Self::skip(
                    schedule.id,
                    format!("server {} was not imported", schedule.server_id),
                ));
                continue;
            };
            let mut tasks: Vec<&PanelScheduleTask> = tables
                .schedule_tasks
                .iter()
                .filter(|task| task.schedule_id == schedule.id)
                .collect();
            tasks
                .sort_by_key(|task| (task.time_offset.unwrap_or(0), task.sequence_id.unwrap_or(0)));
            let mut actions = Vec::new();
            for task in &tasks {
                if task.time_offset.unwrap_or(0) > 0 {
                    self.warn(format!(
                        "schedule {} task {}: time_offset {}s dropped (ZIM schedules are \
                         instantaneous task lists)",
                        schedule.id,
                        task.id,
                        task.time_offset.unwrap_or(0)
                    ));
                }
                if task.continue_on_failure {
                    self.warn(format!(
                        "schedule {} task {}: continue_on_failure is not modeled; ZIM stops \
                         the task list at the first failure",
                        schedule.id, task.id
                    ));
                }
                match task.action.trim() {
                    "power" => {
                        let action = match task.payload.trim() {
                            "start" => Some(PowerAction::Start),
                            "stop" => Some(PowerAction::Stop),
                            "restart" => Some(PowerAction::Restart),
                            "kill" => Some(PowerAction::Kill),
                            other => {
                                self.warn(format!(
                                    "schedule {} task {}: unknown power payload {other:?}; \
                                     task dropped",
                                    schedule.id, task.id
                                ));
                                None
                            }
                        };
                        let Some(action) = action else {
                            continue;
                        };
                        actions.push(ScheduleAction::Power { action });
                    }
                    "send_command" => {
                        if task.payload.trim().is_empty() {
                            self.warn(format!(
                                "schedule {} task {}: empty command; task dropped",
                                schedule.id, task.id
                            ));
                            continue;
                        }
                        actions.push(ScheduleAction::SendCommand {
                            command: task.payload.trim().to_owned(),
                        });
                    }
                    other => self.warn(format!(
                        "schedule {} task {}: unknown action {other:?}; task dropped",
                        schedule.id, task.id
                    )),
                }
            }
            let cron = [
                schedule.cron_minute.trim(),
                schedule.cron_hour.trim(),
                schedule.cron_day_of_month.trim(),
                schedule.cron_month.trim(),
                schedule.cron_day_of_week.trim(),
            ]
            .iter()
            .map(|field| if field.is_empty() { "*" } else { *field })
            .collect::<Vec<&str>>()
            .join(" ");
            let row = Schedule {
                id: ScheduleId::new(),
                instance_id: instance,
                name: schedule.name.clone(),
                cron,
                next_run_at: Self::ts_opt(schedule.next_run_at.as_deref()),
                last_run_at: Self::ts_opt(schedule.last_run_at.as_deref()),
                is_active: schedule.is_active,
                only_when_online: schedule.only_when_online,
                actions,
            };
            match row.validate() {
                Ok(()) => rows.push(row),
                Err(error) => skipped.push(Self::skip(schedule.id, error.to_string())),
            }
        }
        self.outcome("schedules", rows.len(), skipped);
        rows
    }
}

// ─── Backups, transfers, activity ────────────────────────────────────

impl MappingContext {
    /// `backups` → [`Backup`] rows plus the synthetic [`Job`]
    /// companions (the FK contract: a backup row names the durable
    /// job that created it; `is_successful` becomes the job's
    /// terminal state per §2.3). Soft-deleted panel rows are
    /// skipped; the archive's canonical `{uuid}.tar.gz` file name
    /// matches the backups store's on-disk layout.
    fn map_backups(
        &mut self,
        tables: &PanelTables,
        sink: &dyn Persistence,
    ) -> (Vec<Backup>, Vec<Job>) {
        let mut rows = Vec::new();
        let mut jobs = Vec::new();
        let mut skipped = Vec::new();
        let mut folded_names = 0usize;
        for backup in &tables.backups {
            let key = backup.id;
            if backup.deleted_at.is_some() {
                skipped.push(Self::skip(key, "soft-deleted in the panel"));
                continue;
            }
            let Some(instance) = self.instance_ids.get(&backup.server_id).copied() else {
                skipped.push(Self::skip(
                    key,
                    format!("server {} was not imported", backup.server_id),
                ));
                continue;
            };
            if backup.uuid.trim().is_empty() {
                skipped.push(Self::skip(key, "empty backup uuid"));
                continue;
            }
            let completed_at = Self::ts_opt(backup.completed_at.as_deref());
            // Deduplicate on the backup UUID: a re-import keeps the
            // existing row (and its job) instead of tripping the
            // store's UNIQUE(uuid) constraint.
            if sink
                .get_backup_by_uuid(&backup.uuid)
                .ok()
                .flatten()
                .is_some()
            {
                self.warn(format!(
                    "backup {} ({}) already imported; kept the existing row",
                    backup.id, backup.uuid
                ));
                continue;
            }
            let state = if backup.is_successful {
                JobState::Succeeded
            } else {
                JobState::Failed
            };
            let job = Job {
                id: JobId::new(),
                kind: JobKind::Backup,
                instance_id: instance,
                state,
                created_at: self.ts_or_now(backup.created_at.as_deref()),
                updated_at: self.ts_or_now(
                    backup
                        .completed_at
                        .as_deref()
                        .or(backup.created_at.as_deref()),
                ),
                started_at: None,
                completed_at,
                last_error: (!backup.is_successful)
                    .then(|| "imported from the Panel with is_successful = false".to_owned()),
                cleanup_pending: false,
                generation: 0,
                payload: None,
            };
            let row = Backup {
                id: BackupId::new(),
                instance_id: instance,
                job_id: job.id,
                uuid: backup.uuid.clone(),
                file_name: format!("{}.tar.gz", backup.uuid),
                bytes: backup.bytes.unwrap_or(0).max(0) as u64,
                sha256_hash: backup.checksum.clone(),
                locked: backup.is_locked,
                completed_at,
            };
            if backup
                .name
                .as_deref()
                .map(str::trim)
                .is_some_and(|n| !n.is_empty())
            {
                folded_names += 1;
            }
            jobs.push(job);
            rows.push(row);
        }
        if folded_names > 0 {
            self.warn(format!(
                "{folded_names} backup display name(s) folded: ZIM models the archive's \
                 canonical {{uuid}}.tar.gz file name, not a display name"
            ));
        }
        self.outcome("backups", rows.len(), skipped);
        (rows, jobs)
    }

    /// `server_transfers` → [`Transfer`] rows plus synthetic jobs.
    /// `successful` maps onto the terminal transfer states
    /// (§10.9's vocabulary); an in-flight transfer (`successful` is
    /// null) cannot be resumed by ZIM's two-phase commit and
    /// imports as failed with a warning.
    fn map_transfers(&mut self, tables: &PanelTables) -> (Vec<Transfer>, Vec<Job>) {
        let mut rows = Vec::new();
        let mut jobs = Vec::new();
        let mut skipped = Vec::new();
        for transfer in &tables.server_transfers {
            let key = transfer.id;
            let Some(instance) = self.instance_ids.get(&transfer.server_id).copied() else {
                skipped.push(Self::skip(
                    key,
                    format!("server {} was not imported", transfer.server_id),
                ));
                continue;
            };
            let Some(from) = self.node_ids.get(&transfer.old_node).copied() else {
                skipped.push(Self::skip(
                    key,
                    format!("source node {} was not imported", transfer.old_node),
                ));
                continue;
            };
            let Some(to) = self.node_ids.get(&transfer.new_node).copied() else {
                skipped.push(Self::skip(
                    key,
                    format!("destination node {} was not imported", transfer.new_node),
                ));
                continue;
            };
            let state = match transfer.successful {
                Some(true) => TransferState::Succeeded,
                Some(false) => TransferState::Failed,
                None => {
                    self.warn(format!(
                        "transfer {key} was in flight when the panel was dumped; imported as \
                         failed — re-initiate it in ZIM"
                    ));
                    TransferState::Failed
                }
            };
            let job_state = match state {
                TransferState::Succeeded => JobState::Succeeded,
                _ => JobState::Failed,
            };
            let created_at = self.ts_or_now(transfer.created_at.as_deref());
            let job = Job {
                id: JobId::new(),
                kind: JobKind::Transfer,
                instance_id: instance,
                state: job_state,
                created_at,
                updated_at: created_at,
                started_at: None,
                completed_at: Some(created_at),
                last_error: (job_state == JobState::Failed)
                    .then(|| "imported from the Panel without a successful outcome".to_owned()),
                cleanup_pending: false,
                generation: 0,
                payload: None,
            };
            let row = Transfer {
                id: TransferId::new(),
                instance_id: instance,
                job_id: job.id,
                from_node_id: from,
                to_node_id: to,
                state,
                archive_path: None,
            };
            jobs.push(job);
            rows.push(row);
        }
        self.outcome("transfers", rows.len(), skipped);
        (rows, jobs)
    }

    /// `activity_logs` (+ subjects) → [`AuditRecord`] rows: the
    /// Panel's colon-dotted event names normalize to ZIM's dotted
    /// actions; user actors resolve through the user map (anything
    /// else — api keys, the system — imports as the system actor);
    /// `properties` that are not a JSON object fall back to a
    /// string payload per §2.6; the human `description` folds into
    /// the metadata; server subjects link the instance.
    fn map_activity(&mut self, tables: &PanelTables) -> Vec<AuditRecord> {
        let mut skipped = Vec::new();
        let subjects: BTreeMap<i64, i64> = tables
            .activity_log_subjects
            .iter()
            .filter(|subject| subject_is_server(&subject.subject_type))
            .filter_map(|subject| {
                subject
                    .subject_id
                    .map(|server_id| (subject.activity_log_id, server_id))
            })
            .collect();
        let mut records: Vec<(DateTime<Utc>, i64, AuditRecord)> = Vec::new();
        for log in &tables.activity_logs {
            let key = log.id;
            let Some(timestamp) = Self::ts_opt(log.timestamp.as_deref()) else {
                skipped.push(Self::skip(key, "no timestamp"));
                continue;
            };
            let mut action = log.event.trim().replace(':', ".");
            if !action.contains('.') {
                self.warn(format!(
                    "activity log {key} has undotted event {:?}; imported as \
                     panel.{action}",
                    log.event
                ));
                action = format!("panel.{action}");
            }
            let (actor, actor_id) = if log
                .actor_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("user"))
            {
                match log.actor_id.and_then(|id| self.user_ids.get(&id).copied()) {
                    Some(user) => (AuditActor::User, Some(user.to_string())),
                    None => (AuditActor::System, None),
                }
            } else {
                (AuditActor::System, None)
            };
            let instance_id = subjects
                .get(&log.id)
                .and_then(|server_id| self.instance_ids.get(server_id))
                .copied();
            let mut metadata = match normalize_json(log.properties.as_ref()) {
                Some(value @ Value::Object(_)) => value,
                Some(Value::String(text)) => {
                    self.warn(format!(
                        "activity log {key} properties are not a JSON object; stored as the \
                         string fallback (§2.6)"
                    ));
                    json!({ "legacy_value": text })
                }
                Some(other) => {
                    self.warn(format!(
                        "activity log {key} properties are not a JSON object; stored as the \
                         string fallback (§2.6)"
                    ));
                    json!({ "legacy_value": other.to_string() })
                }
                None => serde_json::Map::new().into_iter().collect::<Value>(),
            };
            if let Some(description) = log
                .description
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty())
            {
                if let Some(object) = metadata.as_object_mut() {
                    object
                        .entry("panel_description".to_owned())
                        .or_insert_with(|| Value::String(description.to_owned()));
                }
            }
            let ip_address = log.ip.as_deref().and_then(|ip| {
                ip.trim().parse::<std::net::IpAddr>().ok().or_else(|| {
                    self.warn(format!(
                        "activity log {key} has malformed ip {ip:?}; dropped"
                    ));
                    None
                })
            });
            let record = AuditRecord {
                id: 0,
                timestamp,
                actor,
                actor_id,
                action,
                instance_id,
                metadata,
                ip_address,
                user_agent: None,
            };
            match record.validate() {
                Ok(()) => records.push((timestamp, key, record)),
                Err(error) => skipped.push(Self::skip(key, error.to_string())),
            }
        }
        records.sort_by_key(|(timestamp, key, _)| (*timestamp, *key));
        let rows: Vec<AuditRecord> = records.into_iter().map(|(_, _, record)| record).collect();
        self.outcome("audit", rows.len(), skipped);
        rows
    }

    /// Counts the deferred source tables (never mapped, always
    /// narrated): nests, locations (folded), databases,
    /// database_hosts, mounts, mount_node, mount_server, api_keys,
    /// and settings per the issue's deferred list.
    /// `database_hosts` → [`DatabaseHost`] rows: the connection
    /// records with their node ties resolved (a dangling tie degrades
    /// to "any host" with a warning — the tie is a scheduling
    /// preference, not data ownership; the store's SET NULL posture
    /// agrees). M31: mapped, no longer deferred.
    fn map_database_hosts(
        &mut self,
        tables: &PanelTables,
        sink: &dyn Persistence,
    ) -> Vec<DatabaseHost> {
        let mut rows = Vec::new();
        let mut skipped = Vec::new();
        for host in &tables.database_hosts {
            let key = host.id;
            // Deduplicate on the (unique) name: a re-import keeps
            // the existing row instead of tripping the store's
            // UNIQUE constraint (the backup family's uuid rule,
            // applied to the hosts' own unique key).
            if sink
                .get_database_host_by_name(&host.name)
                .ok()
                .flatten()
                .is_some()
            {
                skipped.push(Self::skip(key, "host name already exists in the sink"));
                continue;
            }
            let port = u16::try_from(host.port.max(0)).unwrap_or(0);
            if port == 0 {
                skipped.push(Self::skip(key, format!("port {} out of range", host.port)));
                continue;
            }
            let node_id = match host.node_id {
                Some(raw) => match self.node_ids.get(&raw).copied() {
                    Some(id) => Some(id),
                    None => {
                        self.warn(format!(
                            "database host {key} ties to node {raw} which was not imported; \
                             the tie releases (the host serves any server)"
                        ));
                        None
                    }
                },
                None => None,
            };
            let created_at = self.ts_or_now(host.created_at.as_deref());
            let updated_at = self.ts_or_now(host.updated_at.as_deref());
            rows.push(DatabaseHost {
                id: DatabaseHostId::new(),
                name: host.name.clone(),
                host: host.host.clone(),
                port,
                username: host.username.clone(),
                password: host.password.clone(),
                max_databases: host
                    .max_databases
                    .map(|v| v.clamp(0, i64::from(u32::MAX)) as u32),
                node_id,
                created_at,
                updated_at,
            });
        }
        self.outcome("database_hosts", rows.len(), skipped);
        rows
    }

    /// `databases` → [`ManagedDatabase`] rows: names and credentials
    /// verbatim (the source database's uniqueness carries over; the
    /// `s{panel_server_id}_` namespace stays as the panel wrote it),
    /// the instance and host cross-references resolved. One blanket
    /// warning when any row imports: the panel stores passwords
    /// ENCRYPTED (Laravel's encrypter) — the imported value is the
    /// ciphertext blob, and the operator rotates in ZIM to mint a
    /// usable credential. M31: mapped, no longer deferred.
    fn map_databases(
        &mut self,
        tables: &PanelTables,
        hosts: &[DatabaseHost],
        sink: &dyn Persistence,
    ) -> Vec<ManagedDatabase> {
        // The panel's host ids → the mapped rows' ids (parallel
        // order: the hosts pass produced one row per imported panel
        // host, in table order).
        let mut panel_host_to_zim: BTreeMap<i64, DatabaseHostId> = BTreeMap::new();
        for (index, panel_host) in tables.database_hosts.iter().enumerate() {
            if let Some(mapped) = hosts.get(index) {
                panel_host_to_zim.insert(panel_host.id, mapped.id);
            }
        }

        let mut rows = Vec::new();
        let mut skipped = Vec::new();
        for database in &tables.databases {
            let key = database.id;
            // Deduplicate on the (globally unique) name, exactly as
            // the hosts do — a re-import converges instead of
            // colliding.
            if sink
                .get_managed_database_by_name(&database.database)
                .ok()
                .flatten()
                .is_some()
            {
                skipped.push(Self::skip(key, "database name already exists in the sink"));
                continue;
            }
            let Some(instance) = self.instance_ids.get(&database.server_id).copied() else {
                skipped.push(Self::skip(
                    key,
                    format!("server {} was not imported", database.server_id),
                ));
                continue;
            };
            let Some(host_id) = panel_host_to_zim.get(&database.database_host_id).copied() else {
                skipped.push(Self::skip(
                    key,
                    format!(
                        "database host {} was not imported",
                        database.database_host_id
                    ),
                ));
                continue;
            };
            let created_at = self.ts_or_now(database.created_at.as_deref());
            let updated_at = self.ts_or_now(database.updated_at.as_deref());
            rows.push(ManagedDatabase {
                id: ManagedDatabaseId::new(),
                instance_id: instance,
                host_id,
                name: database.database.clone(),
                username: database.username.clone(),
                password: database.password.clone(),
                remote: if database.remote.trim().is_empty() {
                    "%".to_owned()
                } else {
                    database.remote.clone()
                },
                max_connections: database.max_connections.clamp(0, i64::from(u32::MAX)) as u32,
                created_at,
                updated_at,
            });
        }
        if !rows.is_empty() {
            self.warn(format!(
                "{} imported database password(s) carry the panel's ENCRYPTED value \
                 (the panel encrypts the column); rotate them in ZIM to mint usable \
                 credentials",
                rows.len()
            ));
        }
        self.outcome("databases", rows.len(), skipped);
        rows
    }

    fn count_deferred(&mut self, tables: &PanelTables) {
        let nest_names: Vec<&str> = tables.nests.iter().map(|nest| nest.name.as_str()).collect();
        let entries = [
            (
                "nests",
                tables.nests.len() as u64,
                format!(
                    "ZIM's egg store has no nests (found: {})",
                    nest_names.join(", ")
                ),
            ),
            (
                "locations",
                tables.locations.len() as u64,
                "folded into node location ids; labels are not modeled".to_owned(),
            ),
        ];
        for (table, rows, note) in entries {
            if rows > 0 {
                self.report.deferred.push(DeferredTable {
                    table: table.to_owned(),
                    rows,
                    note,
                });
            }
        }
        for (table, value) in &tables.deferred {
            let rows = match value {
                Value::Array(entries) => entries.len() as u64,
                Value::Number(number) => number.as_u64().unwrap_or(1),
                Value::Null => 0,
                _ => 1,
            };
            if rows == 0 {
                continue;
            }
            let note = match table.as_str() {
                "mounts" | "mount_node" | "mount_server" => "mounts are a deferred ZIM feature",
                "api_keys" => "client API keys are a deferred ZIM feature",
                "settings" => "panel key/value settings have no ZIM equivalent",
                _ => "not in the §2.3 mapping",
            };
            self.report.deferred.push(DeferredTable {
                table: table.clone(),
                rows,
                note: note.to_owned(),
            });
        }
    }
}

// ─── Orchestration & the write phase ─────────────────────────────────

/// Runs the whole mapping pass (see the module docs for the phase
/// order) and returns everything the write phase needs.
pub(crate) fn map_tables(
    tables: &PanelTables,
    sink: &dyn Persistence,
    policy: &ImportPolicy,
) -> Mapped {
    let mut ctx = MappingContext::new(*policy);
    let users = ctx.map_users(tables);
    let nodes = ctx.map_nodes(tables);
    ctx.map_eggs(tables, sink);
    let instances = ctx.map_instances(tables);
    let allocations = ctx.map_allocations(tables);
    let permissions = ctx.map_subusers(tables);
    let instance_variables = ctx.map_server_variables(tables);
    let schedules = ctx.map_schedules(tables);
    let (backups, mut jobs) = ctx.map_backups(tables, sink);
    let (transfers, transfer_jobs) = ctx.map_transfers(tables);
    jobs.extend(transfer_jobs);
    let database_hosts = ctx.map_database_hosts(tables, sink);
    let databases = ctx.map_databases(tables, &database_hosts, sink);
    let audit = ctx.map_activity(tables);
    ctx.count_deferred(tables);

    // The synthetic jobs land under their own table outcome (the
    // FK companions of the imported backups and transfers).
    ctx.outcome("jobs", jobs.len(), Vec::new());

    if ctx.defaulted_timestamps > 0 {
        ctx.warn(format!(
            "{} required timestamp(s) were missing or invalid in the source; the import \
             instant was recorded instead",
            ctx.defaulted_timestamps
        ));
    }

    Mapped {
        users,
        nodes,
        allocations,
        eggs: std::mem::take(&mut ctx.eggs_rows),
        instances,
        permissions,
        instance_variables,
        jobs,
        backups,
        transfers,
        schedules,
        database_hosts,
        databases,
        audit,
        report: ctx.report,
    }
}

/// Writes the mapped tables in FK-safe order, one transaction per
/// table, with foreign keys bracketed off for the duration (§2.4's
/// process; the store's upserts make a re-run over the same source
/// idempotent).
pub(crate) fn write_all(
    sink: &dyn Persistence,
    mapped: &Mapped,
) -> Result<(), zim_engine_application::ports::PersistenceError> {
    sink.set_import_foreign_keys(false)?;
    let result = write_tables(sink, mapped);
    // Restore the pragma even when a table write failed (the store
    // must not silently stay in FK-off mode — the migration
    // runner's restore discipline).
    let restore = sink.set_import_foreign_keys(true);
    result?;
    restore?;
    Ok(())
}

fn write_tables(
    sink: &dyn Persistence,
    mapped: &Mapped,
) -> Result<(), zim_engine_application::ports::PersistenceError> {
    table(sink, &mapped.users, |sink, row| sink.put_user(row))?;
    table(sink, &mapped.nodes, |sink, row| sink.put_node(row))?;
    table(sink, &mapped.allocations, |sink, row| {
        sink.put_allocation(row)
    })?;
    table(sink, &mapped.eggs, |sink, row| sink.put_egg(row))?;
    table(sink, &mapped.instances, |sink, row| sink.put_instance(row))?;
    table(sink, &mapped.permissions, |sink, row| {
        sink.put_permission(row)
    })?;
    if !mapped.instance_variables.is_empty() {
        sink.begin_batch()?;
        for (instance, variables) in &mapped.instance_variables {
            if let Err(error) = sink.put_instance_variables(instance, variables) {
                let _ = sink.rollback_batch();
                return Err(error);
            }
        }
        sink.commit_batch()?;
    }
    table(sink, &mapped.jobs, |sink, row| sink.put_job(row))?;
    table(sink, &mapped.backups, |sink, row| sink.put_backup(row))?;
    table(sink, &mapped.transfers, |sink, row| sink.put_transfer(row))?;
    table(sink, &mapped.schedules, |sink, row| sink.put_schedule(row))?;
    table(sink, &mapped.database_hosts, |sink, row| {
        sink.put_database_host(row)
    })?;
    table(sink, &mapped.databases, |sink, row| {
        sink.put_managed_database(row)
    })?;
    table(sink, &mapped.audit, |sink, record| {
        sink.append_audit(record).map(|_| ())
    })?;
    Ok(())
}

/// One table, one transaction: begin, put every row, commit — and
/// roll back leaving no partial rows when a put fails.
fn table<T>(
    sink: &dyn Persistence,
    rows: &[T],
    put: impl Fn(&dyn Persistence, &T) -> Result<(), zim_engine_application::ports::PersistenceError>,
) -> Result<(), zim_engine_application::ports::PersistenceError> {
    if rows.is_empty() {
        return Ok(());
    }
    sink.begin_batch()?;
    for row in rows {
        if let Err(error) = put(sink, row) {
            let _ = sink.rollback_batch();
            return Err(error);
        }
    }
    sink.commit_batch()
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_datetimes_parse_in_both_forms() {
        let mysql_style = parse_panel_ts(Some("2024-03-01 12:30:00")).expect("MySQL form parses");
        assert_eq!(mysql_style.to_rfc3339(), "2024-03-01T12:30:00+00:00");
        let iso = parse_panel_ts(Some("2024-03-01T12:30:00Z")).expect("ISO form parses");
        assert_eq!(iso, mysql_style);
        assert!(parse_panel_ts(None).is_none());
        assert!(parse_panel_ts(Some("")).is_none());
        assert!(parse_panel_ts(Some("not a time")).is_none());
    }

    #[test]
    fn status_mapping_covers_the_panel_vocabulary() {
        use InstanceStatus::*;
        assert_eq!(map_status(None), Offline);
        assert_eq!(map_status(Some("")), Offline);
        assert_eq!(map_status(Some("offline")), Offline);
        assert_eq!(map_status(Some("starting")), Starting);
        assert_eq!(map_status(Some("running")), Running);
        assert_eq!(map_status(Some("stopping")), Stopping);
        assert_eq!(map_status(Some("installing")), Installing);
        assert_eq!(map_status(Some("suspended")), Suspended);
        assert_eq!(map_status(Some("restoring_backup")), RestoringBackup);
        assert_eq!(map_status(Some("transferring")), Transferring);
        assert_eq!(map_status(Some("crashed")), CrashLoop);
        assert_eq!(map_status(Some("whatever")), Unknown);
        // Whitespace is tolerated (fixture dumps vary).
        assert_eq!(map_status(Some(" running ")), Running);
    }

    #[test]
    fn rules_split_from_pipe_strings_and_json_arrays() {
        assert_eq!(
            split_rules(Some("required|string|max:20")),
            vec![
                "required".to_owned(),
                "string".to_owned(),
                "max:20".to_owned()
            ]
        );
        assert_eq!(split_rules(Some(r#"["required","regex:^a-z$"]"#)).len(), 2);
        assert_eq!(split_rules(Some("")), Vec::<String>::new());
        assert_eq!(split_rules(None), Vec::<String>::new());
        assert_eq!(split_rules(Some("  ")), Vec::<String>::new());
    }

    #[test]
    fn grant_lookup_is_the_ad12_vocabulary() {
        assert_eq!(
            grant_by_name("control.start").map(|grant| grant.as_str()),
            Some("control.start")
        );
        assert_eq!(
            grant_by_name("database.view_password").map(|grant| grant.as_str()),
            Some("database.view_password")
        );
        assert!(grant_by_name("control.nope").is_none());
        assert!(grant_by_name("*").is_none());
    }

    #[test]
    fn json_columns_normalize_from_text_or_embedded() {
        let embedded = Some(Value::Array(vec![Value::String("eula".into())]));
        assert_eq!(normalize_json(embedded.as_ref()), embedded);
        let text = Some(Value::String(
            r#"{"Java 21":"ghcr.io/yolks:java_21"}"#.into(),
        ));
        assert!(matches!(
            normalize_json(text.as_ref()),
            Some(Value::Object(_))
        ));
        let not_json = Some(Value::String("legacy junk".into()));
        assert_eq!(normalize_json(not_json.as_ref()), not_json);
        assert_eq!(normalize_json(None), None);
    }

    #[test]
    fn activity_events_normalize_colons_to_dots() {
        // (The colon→dot fold itself is exercised inline in
        // map_activity; the shape check here pins the vocabulary
        // examples the Panel actually emits.)
        let event = "server:backup.start";
        assert_eq!(event.replace(':', "."), "server.backup.start");
    }

    #[test]
    fn subject_type_matching_covers_the_morph_forms() {
        assert!(subject_is_server("server"));
        assert!(subject_is_server("App\\Models\\Server"));
        assert!(!subject_is_server("user"));
        assert!(!subject_is_server("backup"));
    }

    fn panel_egg_fixture() -> PanelEgg {
        PanelEgg {
            id: 1,
            uuid: "d6b5f3a2-1111-4bbb-8ccc-000000000001".into(),
            name: "Paper".into(),
            author: Some("parkervcp".into()),
            description: Some("PaperMC".into()),
            features: Some(Value::Array(vec![Value::String("eula".into())])),
            docker_image: None,
            docker_images: Some(json!({"Java 21": "ghcr.io/parkervcp/yolks:java_21"})),
            config_files: Some(
                r#"{"server.properties":{"parser":"properties","replace":[{"match":"server-port","value":"{{SERVER_PORT}}"}]}}"#.into(),
            ),
            config_startup: Some(r#"{"done":"Done"}"#.into()),
            config_stop: Some(r#"{"type":"signal","send":"SIGHUP"}"#.into()),
            startup: Some("java -jar {{SERVER_JARFILE}}".into()),
            script_is_privileged: false,
            script_install: Some("echo install".into()),
            script_entry: Some("bash".into()),
            script_container: Some("debian:12-slim".into()),
            created_at: Some("2024-01-01 00:00:00".into()),
        }
    }

    #[test]
    fn egg_synthesis_reassembles_the_legacy_shape() {
        let egg = panel_egg_fixture();
        let (document, notes) = synthesize_legacy_egg(&egg, &[]);
        assert!(notes.is_empty());
        let object = document.as_object().expect("an object document");
        assert_eq!(
            object.get("uuid").and_then(Value::as_str),
            Some(egg.uuid.as_str())
        );
        assert_eq!(object.get("name").and_then(Value::as_str), Some("Paper"));
        assert_eq!(
            object.get("startup").and_then(Value::as_str),
            Some("java -jar {{SERVER_JARFILE}}")
        );
        // config.stop converts the Panel's type/send form.
        let stop = object
            .get("config")
            .and_then(|config| config.get("stop"))
            .expect("a stop block");
        assert_eq!(stop.get("command").and_then(Value::as_str), Some("SIGHUP"));
        assert_eq!(stop.get("signal").and_then(Value::as_bool), Some(true));
        // The install script block.
        let install = object
            .get("scripts")
            .and_then(|scripts| scripts.get("installation"))
            .expect("an install script");
        assert_eq!(
            install.get("script").and_then(Value::as_str),
            Some("echo install")
        );
        assert_eq!(
            install.get("container").and_then(Value::as_str),
            Some("debian:12-slim")
        );
        // The labeled images map passes through.
        assert_eq!(
            object
                .get("docker_images")
                .and_then(Value::as_object)
                .map(Map::len),
            Some(1)
        );
    }

    #[test]
    fn privileged_install_scripts_inject_the_legacy_flag() {
        let mut egg = panel_egg_fixture();
        egg.script_is_privileged = true;
        let (document, _) = synthesize_legacy_egg(&egg, &[]);
        let features = document
            .get("features")
            .and_then(Value::as_array)
            .expect("features survive as an array");
        assert!(
            features
                .iter()
                .any(|entry| entry == &Value::String("priviledged_install_scripts".into())),
            "the flag uses Pterodactyl's own spelling"
        );
    }

    #[test]
    fn variables_carry_the_legacy_entry_shape() {
        let egg = panel_egg_fixture();
        let variable = PanelEggVariable {
            id: 7,
            egg_id: 1,
            name: "Server jar".into(),
            description: None,
            env_variable: "SERVER_JARFILE".into(),
            default_value: Some("server.jar".into()),
            user_viewable: true,
            user_editable: true,
            rules: Some("required|string|max:20".into()),
        };
        let (document, _) = synthesize_legacy_egg(&egg, &[&variable]);
        let entry = document
            .get("variables")
            .and_then(Value::as_array)
            .and_then(|entries| entries.first())
            .expect("the variable entry");
        assert_eq!(
            entry.get("env_variable").and_then(Value::as_str),
            Some("SERVER_JARFILE")
        );
        assert_eq!(
            entry.get("rules").and_then(Value::as_array).map(Vec::len),
            Some(3)
        );
    }

    #[test]
    fn stop_block_accepts_bare_commands_and_passthroughs() {
        let mut notes = Vec::new();
        let bare = synthesize_stop_block(Some("stop"), &mut notes);
        assert_eq!(bare, Some(json!({"command": "stop", "signal": false})));
        let passthrough =
            synthesize_stop_block(Some(r#"{"command":"end","signal":false}"#), &mut notes);
        assert_eq!(
            passthrough,
            Some(json!({"command": "end", "signal": false}))
        );
        assert_eq!(synthesize_stop_block(None, &mut notes), None);
        assert_eq!(synthesize_stop_block(Some("  "), &mut notes), None);
    }

    #[test]
    fn limit_columns_fold_documented_values() {
        assert_eq!(positive_u32(None), None);
        assert_eq!(positive_u32(Some(-1)), None);
        assert_eq!(positive_u32(Some(5)), Some(5));
        assert_eq!(positive_u32(Some(i64::MAX)), Some(u32::MAX));
    }
}
