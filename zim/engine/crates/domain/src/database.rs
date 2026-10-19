//! Managed databases and database hosts (M31).
//!
//! The panel-side half of the managed-database feature: the ROWS a
//! panel keeps about the databases it provisions for a server —
//! which host they live on, the generated credentials, the allowed
//! connection source — plus the hosts themselves. This mirrors
//! Pterodactyl's `databases` and `database_hosts` tables verbatim
//! (verified against
//! `upstream/pterodactyl-panel/database/migrations/
//! 2016_02_07_172148_add_databases_tables.php`,
//! `2016_02_07_181319_add_database_servers_table.php`, the
//! `2017_03_16_181109` rename to `database_hosts`, and the
//! `2020_04_22` `max_connections` column), because those rows are
//! exactly what a migrating operator brings with them.
//!
//! # The honest boundary (recorded, not hidden)
//!
//! The Panel provisions databases by connecting to the host with
//! its own credentials and issuing `CREATE DATABASE` / `CREATE
//! USER` / `GRANT` (the `DynamicDatabaseConnection` +
//! `DatabaseRepository` pair). The single-node desktop engine does
//! not carry a MySQL client, and M31 does not add one: the engine
//! stores the rows, generates the credentials, enforces the
//! limits, and shows the operator (or the workload's config
//! template) everything needed to connect. Provisioning against a
//! live host is the `workloads/database` integration's milestone
//! (WORKLOAD_ABSTRACTION.md §5) — when it ships, these rows become
//! its input instead of its output, and the surface does not
//! change shape.
//!
//! # Naming conventions (parity, adapted to UUID row ids)
//!
//! The Panel namespaces per-server databases on a SHARED host by
//! prefixing: `s{server_id}_{name}` for the database (48-char
//! budget) and `u{server_id}_{random}` for the user. ZIM's row ids
//! are UUIDs, so the integer convention adapts: the instance's
//! legacy 8-hex `uuid_short` is the namespace — `s{short}_{name}`
//! and `u{short}_{rand}` — the same collision-resisting shape, the
//! same budget math, and a form a Pterodactyl operator already
//! recognizes. Imported rows keep their panel-era names verbatim
//! (the source database's uniqueness carries over), and uniqueness
//! of both `name` and `username` remains GLOBAL, exactly as the
//! Panel enforces it.
//!
//! # Credential generation
//!
//! The Panel's password is 24 characters from the alphanumeric
//! base with two to six substitutions from `! @ = . + ^` — chosen
//! to satisfy MySQL password policies without characters that
//! break generated config files (`Utilities::
//! randomStringWithSpecialCharacters`). ZIM mirrors the shape and
//! the alphabet. The entropy source is `uuid`'s v4 generator — the
//! domain crate's only randomness dependency, already on the
//! permitted list — so no new crate crosses the domain boundary.

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::id::{DatabaseHostId, InstanceId, ManagedDatabaseId, NodeId};
use crate::instance::Instance;

/// The maximum length of a database name (the Panel's 48-char
/// budget, including the `s{short}_` namespace prefix ZIM adds on
/// create; imported rows may use the full budget as-is).
pub const DATABASE_NAME_MAX: usize = 48;

/// The maximum length of a database username (the Panel's 100-char
/// budget; ZIM's generated form is 16).
pub const DATABASE_USERNAME_MAX: usize = 100;

/// The maximum length of a host display name (the Panel's 191).
pub const HOST_NAME_MAX: usize = 191;

/// The maximum length of a host connection username (the Panel's
/// 32-char budget).
pub const HOST_USERNAME_MAX: usize = 32;

/// The password length ZIM generates (the Panel's 24).
pub const PASSWORD_LEN: usize = 24;

/// The substitution alphabet for generated passwords (the Panel's
/// exact set — MySQL-policy friendly, config-file safe).
const PASSWORD_SPECIALS: [char; 6] = ['!', '@', '=', '.', '+', '^'];

/// The random suffix length in a generated username.
const USERNAME_RAND_LEN: usize = 6;

/// The `remote` (allowed connection source) grammar: the Panel's
/// permissive form — hostnames, IPs, `%` wildcards, CIDR-ish
/// slashes, dots, colons — bounded in length.
const REMOTE_MAX: usize = 191;

/// Validation failures for a database host row.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DatabaseHostError {
    /// The display name is empty or whitespace-only.
    #[error("database host name must not be empty")]
    EmptyName,
    /// The display name exceeds the 191-char budget.
    #[error("database host name exceeds {0} characters")]
    NameTooLong(usize),
    /// The host address is empty.
    #[error("database host address must not be empty")]
    EmptyHost,
    /// The host address grammar is invalid (hostname/IP-ish tokens).
    #[error("database host address {0:?} is malformed; expected a hostname or IP")]
    MalformedHost(String),
    /// The port is outside 1-65535.
    #[error("database host port {0} out of range; expected 1-65535")]
    PortOutOfRange(u16),
    /// The connection username is empty or too long.
    #[error("database host username must be 1-32 characters")]
    BadUsername,
}

/// Validation failures for a managed-database row.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ManagedDatabaseError {
    /// The database name is not 3-48 `alpha_dash` characters.
    #[error(
        "database name {0:?} is malformed; expected 3-48 alphanumeric/dash/underscore characters"
    )]
    MalformedName(String),
    /// The username is not 3-100 `alpha_dash` characters.
    #[error("database username {0:?} is malformed; expected 3-100 alphanumeric/dash/underscore characters")]
    MalformedUsername(String),
    /// The `remote` field is empty or over-long.
    #[error("database remote {0:?} is malformed; expected 1-191 connection-source characters")]
    MalformedRemote(String),
    /// The generated password came out with the wrong length.
    #[error("database password must be exactly {0} characters")]
    MalformedPassword(usize),
}

/// One external database server the engine knows how to reach —
/// the Panel's `database_hosts` row: WHERE databases live and the
/// administrative credential the provisioning side would use.
///
/// `node_id` is optional (the Panel's nullable `linked_node`): a
/// host tied to a node is preferred when provisioning for a server
/// on that node; a host with no node is usable by any server (the
/// `allow_random` fallback in the Panel's
/// `DeployServerDatabaseService`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseHost {
    /// The row's identifier.
    pub id: DatabaseHostId,
    /// Display name (non-empty, ≤191).
    pub name: String,
    /// The host's address (hostname or IP; no scheme, no port).
    pub host: String,
    /// The host's DB server port (1-65535; e.g. 3306/5432).
    pub port: u16,
    /// The administrative connection username (≤32).
    pub username: String,
    /// The administrative connection password (never serialized to
    /// non-admin responses; never logged).
    pub password: String,
    /// How many databases this host may carry (`None` = no cap).
    pub max_databases: Option<u32>,
    /// The node this host serves, when tied to one.
    pub node_id: Option<NodeId>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Row last-update time.
    pub updated_at: DateTime<Utc>,
}

impl DatabaseHost {
    /// Validate the invariants the constructor cannot enforce.
    pub fn validate(&self) -> Result<(), DatabaseHostError> {
        if self.name.trim().is_empty() {
            return Err(DatabaseHostError::EmptyName);
        }
        if self.name.chars().count() > HOST_NAME_MAX {
            return Err(DatabaseHostError::NameTooLong(self.name.chars().count()));
        }
        if self.host.trim().is_empty() {
            return Err(DatabaseHostError::EmptyHost);
        }
        if !is_valid_host_address(&self.host) {
            return Err(DatabaseHostError::MalformedHost(self.host.clone()));
        }
        if self.port == 0 {
            return Err(DatabaseHostError::PortOutOfRange(self.port));
        }
        let username_len = self.username.chars().count();
        if username_len == 0 || username_len > HOST_USERNAME_MAX {
            return Err(DatabaseHostError::BadUsername);
        }
        Ok(())
    }
}

/// One database provisioned for one instance — the Panel's
/// `databases` row: the name ON THE HOST, the generated user
/// credential, the allowed connection source, and the host it
/// lives on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedDatabase {
    /// The row's identifier.
    pub id: ManagedDatabaseId,
    /// The instance this database belongs to (cascade-delete with
    /// the instance).
    pub instance_id: InstanceId,
    /// The host the database lives on.
    pub host_id: DatabaseHostId,
    /// The database's name on the host — namespaced
    /// (`s{short}_{name}`) for ZIM-created rows, verbatim for
    /// imported ones; globally unique.
    pub name: String,
    /// The database's connection username — generated
    /// (`u{short}_{rand}`), globally unique.
    pub username: String,
    /// The generated password (never serialized to non-admin
    /// responses; never logged).
    pub password: String,
    /// The allowed connection source (the Panel's `remote`;
    /// `%` = any).
    pub remote: String,
    /// The per-user connection cap (`0` = unlimited — the Panel's
    /// default).
    pub max_connections: u32,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Row last-update time.
    pub updated_at: DateTime<Utc>,
}

impl ManagedDatabase {
    /// Validate the invariants the constructor cannot enforce.
    pub fn validate(&self) -> Result<(), ManagedDatabaseError> {
        if !is_alpha_dash(&self.name, 3, DATABASE_NAME_MAX) {
            return Err(ManagedDatabaseError::MalformedName(self.name.clone()));
        }
        if !is_alpha_dash(&self.username, 3, DATABASE_USERNAME_MAX) {
            return Err(ManagedDatabaseError::MalformedUsername(
                self.username.clone(),
            ));
        }
        let remote_len = self.remote.chars().count();
        if remote_len == 0 || remote_len > REMOTE_MAX {
            return Err(ManagedDatabaseError::MalformedRemote(self.remote.clone()));
        }
        if self.password.chars().count() != PASSWORD_LEN {
            return Err(ManagedDatabaseError::MalformedPassword(
                self.password.chars().count(),
            ));
        }
        Ok(())
    }
}

// ── Derivation (the create path's server-side name generation) ───

/// Derive the namespaced database name for a new row: the Panel's
/// `s{server_id}_{name}` convention with the instance's 8-hex
/// `uuid_short` as the namespace, truncated to the 48-char budget
/// (the user-supplied part loses the tail, exactly as the Panel's
/// `generateUniqueDatabaseName` truncates).
pub fn derive_database_name(instance: &Instance, user_name: &str) -> String {
    let prefix = format!("s{}_", instance.uuid_short);
    let budget = DATABASE_NAME_MAX.saturating_sub(prefix.chars().count());
    let tail: String = user_name.chars().take(budget).collect();
    format!("{prefix}{tail}")
}

/// Derive the generated username for a new row: the Panel's
/// `u{server_id}_{random}` convention with the instance's short
/// uuid and 6 random alphanumeric characters.
pub fn derive_username(instance: &Instance) -> String {
    format!(
        "u{}_{}",
        instance.uuid_short,
        random_alphanumeric(USERNAME_RAND_LEN)
    )
}

/// Generate a database password: 24 characters — an alphanumeric
/// base with two to six substitutions from the Panel's special
/// alphabet (MySQL-policy friendly, config-file safe).
///
/// The entropy source is `uuid`'s v4 generator — the domain
/// crate's only randomness dependency (the permitted-list
/// discipline in `lib.rs`); two UUIDs' worth of bits drive the base
/// and the substitution positions.
pub fn generate_password() -> String {
    let mut chars: Vec<char> = random_alphanumeric(PASSWORD_LEN).chars().collect();
    // How many substitutions (2-6) and where: the second UUID's
    // bytes decide, one byte per draw, so the shape matches the
    // Panel's `random_int(2, 6)` loop.
    let entropy = uuid::Uuid::new_v4();
    let bytes = entropy.as_bytes();
    let count = 2 + (bytes[0] % 5) as usize; // 2..=6
    for slot in 0..count {
        let position = bytes[1 + slot] as usize % PASSWORD_LEN;
        let special = PASSWORD_SPECIALS[bytes[7 + slot] as usize % PASSWORD_SPECIALS.len()];
        chars[position] = special;
    }
    chars.into_iter().collect()
}

/// Random alphanumeric characters from the `uuid` v4 generator
/// (hex-encoded — a 16-symbol alphabet; 24 hex chars carry 96 bits,
/// the same order of entropy as the Panel's alphanumeric base).
fn random_alphanumeric(len: usize) -> String {
    let mut out = String::with_capacity(len);
    while out.len() < len {
        // One simple-format UUID yields 32 hex chars.
        out.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    out.truncate(len);
    out
}

// ── Grammars (the Panel's validation rules, restated) ─────────────

/// `alpha_dash`: non-empty, ASCII alphanumeric / `-` / `_`, within
/// the length bounds (the Panel's `alpha_dash` + `between` rules).
fn is_alpha_dash(value: &str, min: usize, max: usize) -> bool {
    let len = value.chars().count();
    if len < min || len > max {
        return false;
    }
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A host address: hostname or IP — tokens of
/// alphanumeric/hyphen/dot (the Panel's `^[\w\-\.]+$` grammar,
/// which also admits underscores and bare IPv6 colons; ZIM keeps
/// the Panel's permissive set rather than a stricter hostname RFC,
/// because the field's job is operator-entered addresses, not URL
/// parsing).
fn is_valid_host_address(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' || c == ':')
        && value.len() <= 253
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_instance() -> Instance {
        let (uuid, uuid_short) = Instance::legacy_uuid_pair();
        Instance {
            id: InstanceId::new(),
            external_id: None,
            uuid,
            uuid_short,
            node_id: NodeId::new(),
            name: "db host instance".into(),
            description: String::new(),
            status: crate::status::InstanceStatus::Offline,
            skip_egg_scripts: false,
            owner_id: crate::id::UserId::new(),
            limits: crate::instance::ResourceLimits::default(),
            allocation_id: crate::id::AllocationId::new(1),
            egg_id: crate::id::EggId::new(),
            startup: crate::value::CommandTemplate::new("echo hi"),
            image: "busybox:latest".parse().expect("image parses"),
            allocation_limit: None,
            database_limit: Some(5),
            backup_limit: 3,
            crash_detection_enabled: true,
            installed_at: None,
            created_at: Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap(),
        }
    }

    fn sample_host() -> DatabaseHost {
        DatabaseHost {
            id: DatabaseHostId::new(),
            name: "main mysql".into(),
            host: "db.internal".into(),
            port: 3306,
            username: "zim_admin".into(),
            password: "secret".into(),
            max_databases: Some(100),
            node_id: None,
            created_at: Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap(),
        }
    }

    fn sample_database() -> ManagedDatabase {
        let instance = sample_instance();
        ManagedDatabase {
            id: ManagedDatabaseId::new(),
            instance_id: instance.id,
            host_id: DatabaseHostId::new(),
            name: derive_database_name(&instance, "world"),
            username: derive_username(&instance),
            password: generate_password(),
            remote: "%".into(),
            max_connections: 0,
            created_at: Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn host_validation_accepts_the_panel_shapes() {
        assert!(sample_host().validate().is_ok());
        let ipv6 = DatabaseHost {
            host: "::1".into(),
            ..sample_host()
        };
        assert!(ipv6.validate().is_ok());
    }

    #[test]
    fn host_validation_rejects_the_bad_shapes() {
        let mut host = sample_host();
        host.name = "   ".into();
        assert_eq!(host.validate(), Err(DatabaseHostError::EmptyName));

        host = sample_host();
        host.host = "mysql://bad".into();
        assert!(matches!(
            host.validate(),
            Err(DatabaseHostError::MalformedHost(_))
        ));

        host = sample_host();
        host.port = 0;
        assert_eq!(host.validate(), Err(DatabaseHostError::PortOutOfRange(0)));

        host = sample_host();
        host.username = String::new();
        assert_eq!(host.validate(), Err(DatabaseHostError::BadUsername));
    }

    #[test]
    fn database_validation_accepts_the_derived_shapes() {
        let db = sample_database();
        assert!(db.validate().is_ok());
    }

    #[test]
    fn database_validation_rejects_the_bad_shapes() {
        let mut db = sample_database();
        db.name = "s".into();
        assert!(matches!(
            db.validate(),
            Err(ManagedDatabaseError::MalformedName(_))
        ));

        db = sample_database();
        db.remote = String::new();
        assert!(matches!(
            db.validate(),
            Err(ManagedDatabaseError::MalformedRemote(_))
        ));

        db = sample_database();
        db.password = "short".into();
        assert!(matches!(
            db.validate(),
            Err(ManagedDatabaseError::MalformedPassword(_))
        ));
    }

    #[test]
    fn derived_names_carry_the_panel_convention() {
        let instance = sample_instance();
        let name = derive_database_name(&instance, "world");
        assert!(name.starts_with(&format!("s{}_", instance.uuid_short)));
        assert!(name.ends_with("world"));
        assert!(name.chars().count() <= DATABASE_NAME_MAX);

        let username = derive_username(&instance);
        assert!(username.starts_with(&format!("u{}_", instance.uuid_short)));
        assert_eq!(
            username.chars().count(),
            instance.uuid_short.len() + 2 + USERNAME_RAND_LEN
        );
    }

    #[test]
    fn derived_names_truncate_to_the_budget() {
        let instance = sample_instance();
        let long_name = "x".repeat(200);
        let name = derive_database_name(&instance, &long_name);
        assert_eq!(name.chars().count(), DATABASE_NAME_MAX);
        assert!(name.starts_with(&format!("s{}_", instance.uuid_short)));
    }

    #[test]
    fn generated_passwords_match_the_panel_shape() {
        for _ in 0..50 {
            let password = generate_password();
            assert_eq!(password.chars().count(), PASSWORD_LEN);
            let specials = password
                .chars()
                .filter(|c| PASSWORD_SPECIALS.contains(c))
                .count();
            assert!(
                (2..=6).contains(&specials),
                "substitution count {specials} outside 2-6: {password}"
            );
            // Every character is either alphanumeric or one of the
            // Panel's specials — nothing config-hostile.
            assert!(password
                .chars()
                .all(|c| { c.is_ascii_alphanumeric() || PASSWORD_SPECIALS.contains(&c) }));
        }
    }

    #[test]
    fn generated_passwords_are_not_degenerate() {
        // Fifty draws: at least 45 distinct values (a broken entropy
        // source would repeat).
        let mut seen = std::collections::HashSet::new();
        for _ in 0..50 {
            seen.insert(generate_password());
        }
        assert!(
            seen.len() >= 45,
            "only {} distinct passwords in 50 draws",
            seen.len()
        );
    }
}
