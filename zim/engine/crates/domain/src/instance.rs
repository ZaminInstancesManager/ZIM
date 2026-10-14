//! The Instance entity (Pterodactyl-compatible workload base).
//!
//! # Source of truth
//!
//! The `Instance` struct and `ResourceLimits` mirror
//! `docs/architecture/RUST_ENGINE.md` §3. The field set is
//! Pterodactyl-compatible (external_id / uuid / uuid_short legacy
//! fields preserved per AD-12 for migration compatibility), with the
//! `status` as a typed enum rather than a string.
//!
//! Expanded-scope note: per `docs/architecture/WORKLOAD_ABSTRACTION.md`
//! §3, an `Instance` is the Pterodactyl-compatible base row; a
//! separate `Workload` row (kind + spec) references it. The `Workload`
//! struct itself lands in milestone M16; `WorkloadKind` already exists
//! in the `workload` module.

use crate::id::{AllocationId, EggId, InstanceId, NodeId, UserId};
use crate::status::InstanceStatus;
use crate::value::{Bytes, CommandTemplate, DockerImage};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A managed workload instance (the Pterodactyl-compatible base).
///
/// The persisted form splits desired configuration (this struct) from
/// observed runtime state (the supervision module's concern): the
/// engine never trusts a persisted observed state as proof of
/// liveness (lifecycle invariant 1 in
/// `docs/architecture/ZIM_ARCHITECTURE.md` §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    /// Strongly-typed instance identifier.
    pub id: InstanceId,
    /// Legacy Pterodactyl external identifier (compat on migration).
    pub external_id: Option<String>,
    /// Legacy Pterodactyl long UUID (string form, compat).
    pub uuid: String,
    /// Legacy Pterodactyl short UUID (8 hex chars, compat).
    pub uuid_short: String,
    /// The node this instance is placed on.
    pub node_id: NodeId,
    /// Display name (non-empty).
    pub name: String,
    /// Free-form description (may be empty).
    pub description: String,
    /// Desired lifecycle state (typed enum, not a string).
    pub status: InstanceStatus,
    /// Skip egg install/update scripts on next install.
    pub skip_egg_scripts: bool,
    /// The owning user.
    pub owner_id: UserId,
    /// Resource limits (validated).
    pub limits: ResourceLimits,
    /// The primary allocation (IP + port assignment).
    pub allocation_id: AllocationId,
    /// The egg (server definition) this instance instantiates.
    pub egg_id: EggId,
    /// Startup command template with `{{VAR}}` placeholders.
    pub startup: CommandTemplate,
    /// The Docker image the instance's container runs.
    pub image: DockerImage,
    /// Maximum additional allocations the owner may self-assign
    /// (None = unlimited for admins, Some(0) = none).
    pub allocation_limit: Option<u32>,
    /// Maximum databases the owner may create (None = feature off).
    pub database_limit: Option<u32>,
    /// Maximum backups the owner may keep (0 = backups disabled).
    pub backup_limit: u32,
    /// Whether crash detection (and crash-loop protection) is on.
    pub crash_detection_enabled: bool,
    /// When the install last completed successfully.
    pub installed_at: Option<DateTime<Utc>>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Row last-update time.
    pub updated_at: DateTime<Utc>,
}

impl Instance {
    /// Validate the invariants that the constructor cannot enforce
    /// (name non-empty, uuid_short shape, legacy uuid shape).
    pub fn validate(&self) -> Result<(), InstanceError> {
        if self.name.trim().is_empty() {
            return Err(InstanceError::EmptyName);
        }
        if self.uuid.len() != 36 || self.uuid.chars().filter(|c| *c == '-').count() != 4 {
            return Err(InstanceError::MalformedLegacyUuid);
        }
        if self.uuid_short.len() != 8
            || !self
                .uuid_short
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(InstanceError::MalformedShortUuid);
        }
        if self.uuid.is_empty() || self.uuid_short.is_empty() {
            return Err(InstanceError::MalformedShortUuid);
        }
        self.limits.validate()?;
        Ok(())
    }

    /// Generate the legacy-compatible UUID pair for a new instance.
    ///
    /// Pterodactyl rows carry both a 36-char canonical UUID and an
    /// 8-char lowercase short form. ZIM keeps both for compat.
    pub fn legacy_uuid_pair() -> (String, String) {
        let long = uuid::Uuid::new_v4().to_string();
        let short = long.replace('-', "");
        let short = short[..8].to_owned();
        (long, short)
    }
}

/// One per-instance environment-variable override (M10).
///
/// The migration importer's `server_variables` target and the
/// runtime's env-resolution override layer: the value wins over the
/// egg variable's default when the instance's env is rendered (the
/// install runner's script env, and the workload env at startup).
///
/// Keyed by env *name* rather than egg-variable id: egg re-imports
/// replace the egg's variable rows wholesale (fresh ids per
/// `put_egg`), so an id-keyed override would be silently dropped on
/// every re-import; the env name is the stable vocabulary both the
/// egg declarations and the rendered environment share. Overrides
/// whose env name the egg no longer declares are ignored at render
/// time (an orphaned override must not leak into the environment).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstanceVariable {
    /// The environment variable name (the egg's declared
    /// `env_name`; uppercase by the egg's convention).
    pub env_name: String,
    /// The override value (empty is legitimate — an explicitly
    /// cleared variable).
    pub value: String,
}

impl InstanceVariable {
    /// Validate the structural invariant: a non-empty env name.
    pub fn validate(&self) -> Result<(), InstanceError> {
        if self.env_name.trim().is_empty() {
            return Err(InstanceError::EmptyEnvName);
        }
        Ok(())
    }
}

/// Resource limits for an instance.
///
/// Units follow Wings: memory/swap/disk in bytes (panel UIs use MiB),
/// `io_weight` 1–1000 (BlkIO weight), `cpu_millicores` where 1000 =
/// one full core (Wings divides by 1000 into CFS quota), and an
/// optional CPU-pinning thread spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Maximum memory (bytes).
    pub memory: Bytes,
    /// Maximum swap (bytes; 0 = disabled).
    pub swap: Bytes,
    /// Maximum disk quota (bytes).
    pub disk: Bytes,
    /// BlkIO weight 1–1000 (matches Wings).
    pub io_weight: u16,
    /// CPU limit in millicores (1000 = 1 core).
    pub cpu_millicores: u32,
    /// CPU pinning spec (cpuset string like `"0-3,8"`), if pinned.
    pub threads: Option<String>,
    /// Whether the OOM killer is disabled for this instance's
    /// container (per-instance override; Wings parity).
    pub oom_disabled: bool,
}

impl Default for ResourceLimits {
    /// Conservative zero-cost defaults: no limits (max values), no
    /// pinning, OOM killer enabled. Panels are expected to set real
    /// limits at creation time.
    fn default() -> Self {
        Self {
            memory: Bytes::from_bytes(0),
            swap: Bytes::from_bytes(0),
            disk: Bytes::from_bytes(0),
            io_weight: 0,
            cpu_millicores: 0,
            threads: None,
            oom_disabled: false,
        }
    }
}

impl ResourceLimits {
    /// Validate the limit ranges.
    ///
    /// - `io_weight` must be 0 (unset) or 1–1000.
    /// - `cpu_millicores` must be non-negative (u32 by construction).
    /// - `threads`, when present, must be a valid cpuset expression:
    ///   comma-separated ranges/indices of non-negative integers,
    ///   e.g. `"0"`, `"0-3"`, `"0-3,8,10-12"`.
    pub fn validate(&self) -> Result<(), InstanceError> {
        if self.io_weight != 0 && !(1..=1000).contains(&self.io_weight) {
            return Err(InstanceError::IoWeightOutOfRange(self.io_weight));
        }
        if let Some(threads) = &self.threads {
            if !is_valid_cpuset(threads) {
                return Err(InstanceError::MalformedThreadSpec(threads.clone()));
            }
        }
        Ok(())
    }
}

/// Domain-level validation errors for Instance / ResourceLimits.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InstanceError {
    /// The instance name is empty or whitespace-only.
    #[error("instance name must not be empty")]
    EmptyName,
    /// The legacy UUID string is not a canonical 36-char UUID.
    #[error("legacy uuid field is malformed")]
    MalformedLegacyUuid,
    /// The legacy short UUID is not 8 lowercase hex chars.
    #[error("legacy short uuid is malformed")]
    MalformedShortUuid,
    /// `io_weight` outside the 0 (unset) or 1–1000 range.
    #[error("io_weight {0} out of range; expected 0 (unset) or 1-1000")]
    IoWeightOutOfRange(u16),
    /// A per-instance variable override has an empty env name.
    #[error("instance variable env name must not be empty")]
    EmptyEnvName,
    /// The CPU pinning spec is not a valid cpuset expression.
    #[error("malformed cpu thread spec {0:?}; expected e.g. \"0-3,8\"")]
    MalformedThreadSpec(String),
}

/// Validates a cpuset expression: comma-separated list of single
/// indices or `start-end` ranges, no empty components.
fn is_valid_cpuset(spec: &str) -> bool {
    if spec.is_empty() {
        return false;
    }
    spec.split(',').all(|part| match part.split_once('-') {
        None => !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()),
        Some((start, end)) => {
            let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
            match (start.parse::<u32>(), end.parse::<u32>()) {
                (Ok(s), Ok(e)) if digits(start) && digits(end) => s <= e,
                _ => false,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{InstanceId, NodeId, UserId};
    use crate::value::CommandTemplate;

    /// Builds a fully-populated, valid Instance for round-trip tests.
    fn sample_instance() -> Instance {
        let (uuid, uuid_short) = Instance::legacy_uuid_pair();
        Instance {
            id: InstanceId::new(),
            external_id: Some("ext-123".to_owned()),
            uuid,
            uuid_short,
            node_id: NodeId::new(),
            name: "Survival Server".to_owned(),
            description: "Friendly survival world".to_owned(),
            status: InstanceStatus::Offline,
            skip_egg_scripts: false,
            owner_id: UserId::new(),
            limits: ResourceLimits {
                memory: Bytes::from_mib(2048),
                swap: Bytes::from_mib(0),
                disk: Bytes::from_gib(10),
                io_weight: 500,
                cpu_millicores: 1000,
                threads: Some("0-3".to_owned()),
                oom_disabled: false,
            },
            allocation_id: crate::id::AllocationId::new(1),
            egg_id: crate::id::EggId::new(),
            startup: CommandTemplate::new(
                "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JAR}}",
            ),
            image: "ghcr.io/parkervcp/yolks:java_21".parse().unwrap(),
            allocation_limit: Some(3),
            database_limit: Some(2),
            backup_limit: 5,
            crash_detection_enabled: true,
            installed_at: Some(Utc::now()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn instance_round_trips_through_serde() {
        let instance = sample_instance();
        let json = serde_json::to_string_pretty(&instance).unwrap();
        let back: Instance = serde_json::from_str(&json).unwrap();
        assert_eq!(instance, back);
    }

    #[test]
    fn instance_validate_accepts_sample() {
        assert!(sample_instance().validate().is_ok());
    }

    #[test]
    fn instance_validate_rejects_empty_name() {
        let mut instance = sample_instance();
        instance.name = "   ".to_owned();
        assert!(matches!(instance.validate(), Err(InstanceError::EmptyName)));
    }

    #[test]
    fn instance_validate_rejects_malformed_uuids() {
        let mut instance = sample_instance();
        instance.uuid = "not-a-uuid".to_owned();
        assert!(matches!(
            instance.validate(),
            Err(InstanceError::MalformedLegacyUuid)
        ));

        let mut instance = sample_instance();
        instance.uuid_short = "TOOLONG99".to_owned();
        assert!(matches!(
            instance.validate(),
            Err(InstanceError::MalformedShortUuid)
        ));
    }

    #[test]
    fn legacy_uuid_pair_shape() {
        let (long, short) = Instance::legacy_uuid_pair();
        assert_eq!(long.len(), 36);
        assert_eq!(long.chars().filter(|c| *c == '-').count(), 4);
        assert_eq!(short.len(), 8);
        assert!(short.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(long.replace('-', "").starts_with(&short));
    }

    #[test]
    fn instance_variable_round_trips_through_serde() {
        let variable = InstanceVariable {
            env_name: "SERVER_JARFILE".to_owned(),
            value: "server.jar".to_owned(),
        };
        let json = serde_json::to_string(&variable).unwrap();
        let back: InstanceVariable = serde_json::from_str(&json).unwrap();
        assert_eq!(variable, back);
    }

    #[test]
    fn instance_variable_rejects_empty_env_names() {
        let variable = InstanceVariable {
            env_name: "  ".to_owned(),
            value: String::new(),
        };
        assert!(matches!(
            variable.validate(),
            Err(InstanceError::EmptyEnvName)
        ));
    }

    #[test]
    fn resource_limits_round_trip() {
        let limits = sample_instance().limits;
        let json = serde_json::to_string(&limits).unwrap();
        let back: ResourceLimits = serde_json::from_str(&json).unwrap();
        assert_eq!(limits, back);
    }

    #[test]
    fn io_weight_bounds() {
        let mut limits = ResourceLimits::default();
        for valid in [0u16, 1, 500, 1000] {
            limits.io_weight = valid;
            assert!(
                limits.validate().is_ok(),
                "io_weight {valid} should be valid"
            );
        }
        for invalid in [1001u16, u16::MAX] {
            limits.io_weight = invalid;
            assert!(
                limits.validate().is_err(),
                "io_weight {invalid} should be invalid"
            );
        }
    }

    #[test]
    fn thread_spec_validation() {
        let mut limits = ResourceLimits::default();
        for valid in ["0", "0-3", "0-3,8", "10-12,0", "0,1,2"] {
            limits.threads = Some(valid.to_owned());
            assert!(
                limits.validate().is_ok(),
                "cpuset {valid:?} should be valid"
            );
        }
        for invalid in ["", ",0", "0,", "a-b", "0-3,", "0--3", "-"] {
            limits.threads = Some(invalid.to_owned());
            assert!(
                limits.validate().is_err(),
                "cpuset {invalid:?} should be invalid"
            );
        }
    }
}
