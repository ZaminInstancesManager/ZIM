//! ETag validators and `If-Match` enforcement (§8 concurrency
//! control).
//!
//! Validators are strong ETags derived from the row's identity and
//! `updated_at`: any successful mutation bumps `updated_at`, which
//! changes the ETag, which makes stale writers fail closed with
//! `412 precondition_failed`.
//!
//! M5 scope: instance `PATCH`/`DELETE` require `If-Match` (the
//! spec's "mutating operations on a single instance"). Sub-resources
//! (schedules, subusers) ride the instance's validator in later
//! milestones; here they are documented as unconstrained.

use axum::http::{header::IF_MATCH, HeaderMap};

use crate::error::ApiError;
use zim_engine_domain::Instance;

/// Compute the strong ETag for an instance row.
pub fn instance_etag(instance: &Instance) -> String {
    // Nanosecond precision when available; seconds as fallback. The
    // exact scheme is private to the engine — clients treat the
    // value as opaque.
    let stamp = instance
        .updated_at
        .timestamp_nanos_opt()
        .unwrap_or_else(|| instance.updated_at.timestamp() * 1_000_000);
    format!("\"inst-{}-{stamp}\"", instance.id)
}

/// Enforce `If-Match` for a mutating instance operation.
///
/// - Header absent → `400 bad_request` (the spec requires clients to
///   send it; failing closed prevents lost-update races).
/// - `*` → matches any current state.
/// - Exact current ETag → allowed.
/// - Anything else → `412 precondition_failed` carrying the current
///   ETag so the client can re-read and retry.
pub fn require_if_match(headers: &HeaderMap, current_etag: &str) -> Result<(), ApiError> {
    let raw = headers
        .get(IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ApiError::bad_request(
                "mutating an instance requires an If-Match header carrying the last-read ETag \
                 (send If-Match: \"*\" to force)",
            )
        })?;
    // A list of ETags is allowed by RFC 9110; we accept either the
    // wildcard or an exact single match.
    if raw.trim() == "*"
        || raw
            .split(',')
            .any(|candidate| candidate.trim() == current_etag)
    {
        Ok(())
    } else {
        Err(ApiError::PreconditionFailed {
            provided: raw.trim().to_owned(),
            expected: current_etag.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use chrono::Utc;

    fn headers_with(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(IF_MATCH, HeaderValue::from_str(value).unwrap());
        h
    }

    fn sample_instance() -> Instance {
        let (uuid, uuid_short) = Instance::legacy_uuid_pair();

        Instance {
            id: zim_engine_domain::InstanceId::new(),
            external_id: None,
            uuid,
            uuid_short,
            node_id: zim_engine_domain::NodeId::new(),
            name: "test".to_owned(),
            description: String::new(),
            status: Default::default(),
            skip_egg_scripts: false,
            owner_id: zim_engine_domain::UserId::new(),
            limits: Default::default(),
            allocation_id: zim_engine_domain::AllocationId(1),
            egg_id: zim_engine_domain::EggId::new(),
            startup: zim_engine_domain::CommandTemplate("java -jar server.jar".to_owned()),
            image: "ghcr.io/parkervcp/yolks:java_21".parse().unwrap(),
            allocation_limit: None,
            database_limit: None,
            backup_limit: 0,
            crash_detection_enabled: true,
            installed_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn etag_changes_when_updated_at_changes() {
        let mut instance = sample_instance();
        let before = instance_etag(&instance);
        instance.updated_at += chrono::Duration::milliseconds(1);
        let after = instance_etag(&instance);
        assert_ne!(before, after);
    }

    #[test]
    fn if_match_rules() {
        let instance = sample_instance();
        let etag = instance_etag(&instance);

        // Absent header: rejected (failing closed).
        assert!(require_if_match(&HeaderMap::new(), &etag).is_err());

        // Wildcard.
        assert!(require_if_match(&headers_with("*"), &etag).is_ok());

        // Exact match.
        assert!(require_if_match(&headers_with(&etag), &etag).is_ok());

        // Match within a list.
        assert!(require_if_match(&headers_with(&format!("\"x\", {etag}")), &etag).is_ok());

        // Stale etag: 412 with the current one attached.
        let err = require_if_match(&headers_with("\"inst-stale\""), &etag).unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::PRECONDITION_FAILED);
        assert_eq!(err.code(), "precondition_failed");
        assert_eq!(err.body()["error"]["current_etag"], serde_json::json!(etag));
    }
}
