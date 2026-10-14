//! The API error vocabulary and its JSON wire format.
//!
//! Every error response is a single structured JSON object per
//! `docs/api/ZIM_API_SPEC.md` §6:
//!
//! ```json
//! { "error": { "code": "...", "message": "...", "request_id": "..." } }
//! ```
//!
//! Operation-specific fields (`missing_permissions`,
//! `supported_versions`, ...) ride along under the same `error`
//! object. The `request_id` field is injected by the request-id
//! middleware (see `middleware.rs`); `IntoResponse` emits it as
//! `null` when the error escapes before a request id was generated.
//!
//! Status/code pairs follow the §6.1 vocabulary table. Two mappings
//! are documented deviations (additive, non-breaking per spec §2):
//!
//! - `bad_request` is also used for rejected `Host` headers (the
//!   DNS-rebinding mitigation rejects before routing).
//! - `bad_request` + HTTP 406 is used for unsupported `Accept`
//!   versions, with `supported_versions` listing what the engine
//!   speaks.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use zim_engine_domain::{PermissionGrant, UserId};

/// Every failure mode the M5 API surface can produce.
///
/// Variants map 1:1 onto the §6.1 error-code vocabulary; the mapping
/// is exercised by the `wire_format_matches_spec_table` test.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)] // variant docs live in the constructor docs below
pub enum ApiError {
    /// 400 `bad_request`.
    #[error("{0}")]
    BadRequest(String),
    /// 400 `validation_failed` — well-formed JSON, failed schema checks.
    #[error("{message}")]
    Validation {
        /// Human-readable description of the violated constraint.
        message: String,
        /// Offending request field, when identifiable.
        field: Option<String>,
    },
    /// 401 `unauthorized`.
    #[error("{0}")]
    Unauthorized(String),
    /// 403 `permission_denied` with the missing grants listed.
    #[error("{message}")]
    PermissionDenied {
        /// Human-readable message.
        message: String,
        /// Required-but-missing grants (dotted names in the body).
        missing: Vec<PermissionGrant>,
        /// The denied user (for the audit log; not serialized).
        user_id: UserId,
    },
    /// 403 `permission_denied` without a grant list (admin gates).
    #[error("{0}")]
    AdminRequired(UserId),
    /// 404 `not_found`.
    #[error("{resource} {id} not found")]
    NotFound {
        /// Resource kind, e.g. `"instance"`.
        resource: &'static str,
        /// The identifier that missed.
        id: String,
    },
    /// 409 `conflict`.
    #[error("{0}")]
    Conflict(String),
    /// 409 `instance_state_conflict`.
    #[error("{0}")]
    StateConflict(String),
    /// 412 `precondition_failed` (`If-Match` mismatch).
    #[error("If-Match {provided:?} does not match the current ETag {expected:?}")]
    PreconditionFailed {
        /// The ETag the client sent.
        provided: String,
        /// The resource's current ETag.
        expected: String,
    },
    /// 413 `payload_too_large`.
    #[error("request body exceeds the {limit}-byte limit")]
    PayloadTooLarge {
        /// The enforced limit in bytes.
        limit: usize,
    },
    /// 415 `unsupported_media_type`.
    #[error("{0}")]
    UnsupportedMediaType(String),
    /// 429 `rate_limited`.
    #[error("rate limit exceeded; retry after {retry_after_seconds}s")]
    RateLimited {
        /// Seconds until the window resets.
        retry_after_seconds: u64,
        /// The window's request budget.
        limit: u64,
    },
    /// 422 `frontend_smoke_test_failed` — a frontend package failed
    /// the engine's activation gate (§6.1 table; the report rides
    /// along so the admin sees exactly which check failed).
    #[error("the frontend package failed its activation smoke test")]
    FrontendSmokeTestFailed {
        /// The gate's full report (check-by-check).
        report: zim_engine_frontend_loader::SmokeReport,
    },
    /// 500 `internal_error`.
    #[error("{0}")]
    Internal(String),
    /// 503 `runtime_unavailable` — the workload runtime backend is
    /// unreachable, so lifecycle operations cannot be queued.
    ///
    /// Additive M6 mapping (documented per the M5 delivery-note
    /// convention): the supervision layer distinguishes "backend
    /// down" (this, retryable) from "instance stopped" — observed
    /// state stays `unknown`, never `offline`, while this error is
    /// in effect.
    #[error("runtime unavailable: {message}")]
    RuntimeUnavailable {
        /// The underlying failure (connection refused, socket missing).
        message: String,
    },
    /// 501 `not_implemented` (declared endpoints awaiting their milestone).
    #[error("{feature} is not implemented yet; scheduled for {milestone}")]
    NotImplemented {
        /// What is missing.
        feature: &'static str,
        /// The roadmap milestone that delivers it.
        milestone: &'static str,
    },
}

impl ApiError {
    /// A `bad_request` with a owned message.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    /// A `validation_failed` for a named field.
    pub fn validation(message: impl Into<String>, field: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
            field: Some(field.into()),
        }
    }

    /// A `not_found` for a typed resource and id.
    pub fn not_found(resource: &'static str, id: impl std::fmt::Display) -> Self {
        Self::NotFound {
            resource,
            id: id.to_string(),
        }
    }

    /// A `403 permission_denied` with a custom message and no grant
    /// list (policy gates like the frontend approval requirement).
    pub fn permission_denied_message(message: impl Into<String>) -> Self {
        Self::PermissionDenied {
            message: message.into(),
            missing: Vec::new(),
            user_id: UserId::new(),
        }
    }

    /// An `unauthorized`.
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::Unauthorized(message.into())
    }

    /// An `internal_error` wrapping a persistence failure.
    ///
    /// Callers that expect FK-style failures map them to `Conflict`
    /// before reaching here (see `handlers::conflict_on_backend`).
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    /// The HTTP status this error maps to (§6.1 table).
    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) | Self::Validation { .. } => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::PermissionDenied { .. } | Self::AdminRequired(_) => StatusCode::FORBIDDEN,
            Self::NotFound { .. } => StatusCode::NOT_FOUND,
            Self::Conflict(_) | Self::StateConflict(_) => StatusCode::CONFLICT,
            Self::PreconditionFailed { .. } => StatusCode::PRECONDITION_FAILED,
            Self::PayloadTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedMediaType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::FrontendSmokeTestFailed { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::RuntimeUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::NotImplemented { .. } => StatusCode::NOT_IMPLEMENTED,
        }
    }

    /// The stable `code` string (§6.1 vocabulary).
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Validation { .. } => "validation_failed",
            Self::Unauthorized(_) => "unauthorized",
            Self::PermissionDenied { .. } | Self::AdminRequired(_) => "permission_denied",
            Self::NotFound { .. } => "not_found",
            Self::Conflict(_) => "conflict",
            Self::StateConflict(_) => "instance_state_conflict",
            Self::PreconditionFailed { .. } => "precondition_failed",
            Self::PayloadTooLarge { .. } => "payload_too_large",
            Self::UnsupportedMediaType(_) => "unsupported_media_type",
            Self::RateLimited { .. } => "rate_limited",
            Self::FrontendSmokeTestFailed { .. } => "frontend_smoke_test_failed",
            Self::Internal(_) => "internal_error",
            Self::RuntimeUnavailable { .. } => "runtime_unavailable",
            Self::NotImplemented { .. } => "not_implemented",
        }
    }

    /// The structured body's operation-specific extras, if any.
    fn extras(&self) -> Vec<(&'static str, Value)> {
        match self {
            Self::Validation {
                field: Some(field), ..
            } => vec![("field", json!(field))],
            Self::Validation { field: None, .. } => Vec::new(),
            Self::PermissionDenied { missing, .. } => {
                vec![("missing_permissions", json!(missing_grants(missing)))]
            }
            Self::NotFound { resource, id } => {
                vec![("resource", json!(resource)), ("id", json!(id))]
            }
            Self::PreconditionFailed { expected, .. } => {
                vec![("current_etag", json!(expected))]
            }
            Self::PayloadTooLarge { limit } => vec![("limit_bytes", json!(limit))],
            Self::RateLimited { limit, .. } => vec![("limit", json!(limit))],
            Self::FrontendSmokeTestFailed { report } => {
                vec![("smoke", smoke_report_json(report))]
            }
            Self::NotImplemented { milestone, .. } => {
                vec![("milestone", json!(milestone))]
            }
            _ => Vec::new(),
        }
    }

    /// Serialize the `error` object (sans `request_id`, which the
    /// request-id middleware injects on the wire).
    pub fn error_object(&self) -> Value {
        let mut obj = json!({
            "code": self.code(),
            "message": self.to_string(),
        });
        if let Some(map) = obj.as_object_mut() {
            for (k, v) in self.extras() {
                map.insert(k.to_owned(), v);
            }
            // Filled in by the middleware on the wire; present here so
            // the shape is visible to unit tests.
            map.insert("request_id".to_owned(), Value::Null);
        }
        obj
    }

    /// The full response body (`{"error": {...}}`).
    pub fn body(&self) -> Value {
        json!({ "error": self.error_object() })
    }
}

/// Dotted canonical names for the `missing_permissions` array.
fn missing_grants(missing: &[PermissionGrant]) -> Vec<&'static str> {
    missing.iter().map(|g| g.as_str()).collect()
}

/// Serialize a smoke gate report for an error body's `smoke` extra.
fn smoke_report_json(report: &zim_engine_frontend_loader::SmokeReport) -> Value {
    json!({
        "passed": report.passed(),
        "checks": report.checks.iter().map(|check| json!({
            "name": check.name,
            "passed": check.passed,
            "detail": check.detail,
        })).collect::<Vec<_>>(),
    })
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.status(), Json(self.body())).into_response();
        // 429 responses carry the plain-text Retry-After hint as well
        // (§9); the numeric form also lives in the error body.
        if let Self::RateLimited {
            retry_after_seconds,
            ..
        } = &self
        {
            if let Ok(v) = axum::http::HeaderValue::from_str(&retry_after_seconds.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        response
    }
}

impl From<zim_engine_application::PersistenceError> for ApiError {
    /// Persistence failures surface as `internal_error`; delete paths
    /// that expect FK rejections remap to `conflict` at the call site
    /// (`handlers::conflict_on_backend`).
    fn from(e: zim_engine_application::PersistenceError) -> Self {
        Self::Internal(format!("persistence failure: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_matches_spec_table() {
        // §6.1: code → HTTP status. One representative per code.
        let cases: Vec<(ApiError, StatusCode, &'static str)> = vec![
            (
                ApiError::bad_request("x"),
                StatusCode::BAD_REQUEST,
                "bad_request",
            ),
            (
                ApiError::validation("x", "field"),
                StatusCode::BAD_REQUEST,
                "validation_failed",
            ),
            (
                ApiError::unauthorized("x"),
                StatusCode::UNAUTHORIZED,
                "unauthorized",
            ),
            (
                ApiError::AdminRequired(UserId::new()),
                StatusCode::FORBIDDEN,
                "permission_denied",
            ),
            (
                ApiError::not_found("instance", "abc"),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                ApiError::Conflict("x".into()),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::StateConflict("x".into()),
                StatusCode::CONFLICT,
                "instance_state_conflict",
            ),
            (
                ApiError::PreconditionFailed {
                    provided: "\"a\"".into(),
                    expected: "\"b\"".into(),
                },
                StatusCode::PRECONDITION_FAILED,
                "precondition_failed",
            ),
            (
                ApiError::PayloadTooLarge { limit: 10 },
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
            ),
            (
                ApiError::UnsupportedMediaType("x".into()),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
            ),
            (
                ApiError::RateLimited {
                    retry_after_seconds: 7,
                    limit: 600,
                },
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
            ),
            (
                ApiError::FrontendSmokeTestFailed {
                    report: zim_engine_frontend_loader::SmokeReport {
                        checks: vec![zim_engine_frontend_loader::CheckResult {
                            name: "html_structure",
                            passed: false,
                            detail: "inline script".to_owned(),
                        }],
                    },
                },
                StatusCode::UNPROCESSABLE_ENTITY,
                "frontend_smoke_test_failed",
            ),
            (
                ApiError::internal("x"),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            ),
            (
                ApiError::NotImplemented {
                    feature: "power",
                    milestone: "M6",
                },
                StatusCode::NOT_IMPLEMENTED,
                "not_implemented",
            ),
        ];
        for (error, status, code) in cases {
            assert_eq!(error.status(), status, "{code}");
            assert_eq!(error.code(), code);
            let body = error.body();
            assert_eq!(body["error"]["code"], code, "{code}");
            assert!(body["error"]["message"].is_string());
            assert!(body["error"]["request_id"].is_null());
        }
    }

    #[test]
    fn permission_denied_lists_missing_grants() {
        let missing = vec![PermissionGrant::ControlStart, PermissionGrant::FileRead];
        let error = ApiError::PermissionDenied {
            message: "no".into(),
            missing,
            user_id: UserId::new(),
        };
        let body = error.body();
        assert_eq!(
            body["error"]["missing_permissions"],
            json!(["control.start", "file.read"])
        );
    }

    #[test]
    fn rate_limited_carries_retry_after_header() {
        use axum::response::IntoResponse;
        let error = ApiError::RateLimited {
            retry_after_seconds: 30,
            limit: 600,
        };
        let response = error.into_response();
        assert_eq!(response.headers()[header::RETRY_AFTER], "30");
    }
}
