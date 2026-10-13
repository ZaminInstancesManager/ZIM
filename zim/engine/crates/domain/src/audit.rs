//! Audit record entities.
//!
//! # Source of truth
//!
//! `AuditRecord` and `AuditActor` mirror
//! `docs/architecture/RUST_ENGINE.md` §3. Audit records are
//! append-only (no updates, no deletes) — the audit API surface is
//! cursor-paginated over `timestamp` per
//! `docs/api/ZIM_API_SPEC.md` §10.

use crate::id::InstanceId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::net::IpAddr;

/// Who performed an audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditActor {
    /// A logged-in user (see `actor_id`).
    User,
    /// An installed extension acting under its granted permissions.
    Extension,
    /// The engine itself (schedules, reconciliation, cleaners).
    System,
}

/// An append-only audit log entry.
///
/// The `id` is a monotonically increasing rowid (u64) — SQLite's
/// INTEGER PRIMARY KEY gives this for free; the API layer exposes it
/// as the pagination cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Monotonic row identifier (cursor).
    pub id: u64,
    /// When the action happened.
    pub timestamp: DateTime<Utc>,
    /// What kind of actor performed the action.
    pub actor: AuditActor,
    /// Actor identity (user UUID / extension id), if applicable.
    pub actor_id: Option<String>,
    /// Dotted action name (e.g. `instance.power.start`).
    pub action: String,
    /// The instance the action targeted, if any.
    pub instance_id: Option<InstanceId>,
    /// Arbitrary structured metadata about the action.
    pub metadata: JsonValue,
    /// Client IP address, when the action came over the API.
    pub ip_address: Option<IpAddr>,
    /// Client user agent, when the action came over the API.
    pub user_agent: Option<String>,
}

impl AuditRecord {
    /// Validate the invariants the constructor cannot enforce.
    ///
    /// Only structural checks: a non-empty dotted action name and a
    /// non-null metadata object.
    pub fn validate(&self) -> Result<(), AuditError> {
        if self.action.is_empty() || !self.action.contains('.') {
            return Err(AuditError::MalformedAction(self.action.clone()));
        }
        if !self.metadata.is_object() {
            return Err(AuditError::MetadataNotObject);
        }
        Ok(())
    }
}

/// Domain-level validation errors for AuditRecord.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuditError {
    /// The action name is empty or not dotted (e.g. `instance.start`).
    #[error("malformed audit action {0:?}; expected a dotted name like \"instance.power.start\"")]
    MalformedAction(String),
    /// The metadata value is not a JSON object.
    #[error("audit metadata must be a JSON object")]
    MetadataNotObject,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> AuditRecord {
        AuditRecord {
            id: 421,
            timestamp: Utc::now(),
            actor: AuditActor::User,
            actor_id: Some("550e8400-e29b-41d4-a716-446655440000".to_owned()),
            action: "instance.power.start".to_owned(),
            instance_id: Some(InstanceId::new()),
            metadata: serde_json::json!({
                "desired_state": "running",
                "generation": 12,
            }),
            ip_address: Some("203.0.113.99".parse().unwrap()),
            user_agent: Some("zim-frontend/0.1".to_owned()),
        }
    }

    #[test]
    fn audit_record_round_trips_through_serde() {
        let record = sample_record();
        let json = serde_json::to_string(&record).unwrap();
        let back: AuditRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, back);
    }

    #[test]
    fn audit_validate_accepts_sample() {
        assert!(sample_record().validate().is_ok());
    }

    #[test]
    fn malformed_actions_are_rejected() {
        let mut record = sample_record();
        record.action = "start".to_owned();
        assert!(matches!(
            record.validate(),
            Err(AuditError::MalformedAction(_))
        ));
        record.action = String::new();
        assert!(matches!(
            record.validate(),
            Err(AuditError::MalformedAction(_))
        ));
    }

    #[test]
    fn non_object_metadata_is_rejected() {
        let mut record = sample_record();
        record.metadata = serde_json::json!([1, 2, 3]);
        assert!(matches!(
            record.validate(),
            Err(AuditError::MetadataNotObject)
        ));
    }

    #[test]
    fn all_actor_kinds_round_trip() {
        for actor in [AuditActor::User, AuditActor::Extension, AuditActor::System] {
            let mut record = sample_record();
            record.actor = actor;
            let json = serde_json::to_string(&record).unwrap();
            let back: AuditRecord = serde_json::from_str(&json).unwrap();
            assert_eq!(back.actor, actor);
        }
        assert_eq!(
            serde_json::to_string(&AuditActor::System).unwrap(),
            "\"system\""
        );
    }

    #[test]
    fn ipv6_address_round_trips() {
        let mut record = sample_record();
        record.ip_address = Some("2001:db8::42".parse().unwrap());
        let json = serde_json::to_string(&record).unwrap();
        let back: AuditRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.ip_address,
            Some("2001:db8::42".parse::<IpAddr>().unwrap())
        );
    }
}
