//! Query-string pagination: page-based for ordinary lists,
//! cursor-based for the audit log (per `docs/api/ZIM_API_SPEC.md` §7
//! and §15's preliminary decision — opaque base64 cursors).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::json;

use crate::error::ApiError;

/// Maximum `per_page` accepted on any page-based endpoint (§7).
pub const MAX_PER_PAGE: u32 = 200;

/// Default `per_page` (§7).
pub const DEFAULT_PER_PAGE: u32 = 50;

/// Parsed `page` / `per_page` query parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageParams {
    /// 1-indexed page number.
    pub page: u32,
    /// Items per page (1..=200).
    pub per_page: u32,
}

impl Default for PageParams {
    fn default() -> Self {
        Self {
            page: 1,
            per_page: DEFAULT_PER_PAGE,
        }
    }
}

impl PageParams {
    /// Build from typed query values (`None` = parameter absent;
    /// malformed values were already rejected by the query parser's
    /// typed deserialization).
    ///
    /// Out-of-range values are `400 validation_failed`, not silently
    /// coerced — a client asking for page 0 or per_page 100000 has a
    /// bug worth surfacing. `per_page` is capped at 200 per §7.
    pub fn from_query(page: Option<u32>, per_page: Option<u32>) -> Result<Self, ApiError> {
        let page = match page {
            None => 1,
            Some(raw) if raw >= 1 => raw,
            Some(_) => {
                return Err(ApiError::validation("page must be >= 1", "page"));
            }
        };
        let per_page = per_page.unwrap_or(DEFAULT_PER_PAGE).min(MAX_PER_PAGE);
        Ok(Self { page, per_page })
    }

    /// Slice bounds `[start, end)` over a collection of length
    /// `total`.
    pub fn bounds(&self, total: usize) -> (usize, usize) {
        let start = ((self.page as usize).saturating_sub(1)) * self.per_page as usize;
        let end = (start + self.per_page as usize).min(total);
        (start.min(total), end)
    }
}

/// The `pagination` envelope for page-based responses (§7).
pub fn page_envelope<T: serde::Serialize>(
    page: PageParams,
    total: usize,
    data: Vec<T>,
) -> serde_json::Value {
    let total_pages = if total == 0 {
        1
    } else {
        total.div_ceil(page.per_page as usize)
    };
    json!({
        "data": data,
        "pagination": {
            "page": page.page,
            "per_page": page.per_page,
            "total": total,
            "total_pages": total_pages,
        }
    })
}

/// Parsed cursor-pagination parameters for the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorParams {
    /// Resume *after* this audit row id (0 = from the beginning).
    pub after_id: u64,
    /// Items per page (1..=200; default 50).
    pub limit: u32,
}

impl Default for CursorParams {
    fn default() -> Self {
        Self {
            after_id: 0,
            limit: DEFAULT_PER_PAGE,
        }
    }
}

/// The cursor payload, JSON + base64url (opaque to clients per §15).
#[derive(serde::Serialize, serde::Deserialize)]
struct CursorPayload {
    /// Resume position (the last id returned).
    a: u64,
}

impl CursorParams {
    /// Encode a cursor pointing after `after_id`.
    pub fn encode_cursor(after_id: u64) -> String {
        let payload = CursorPayload { a: after_id };
        let raw = serde_json::to_vec(&payload).unwrap_or_default();
        URL_SAFE_NO_PAD.encode(raw)
    }

    /// Decode a client-supplied cursor into resume parameters.
    pub fn from_query(cursor: Option<String>, limit: Option<u32>) -> Result<Self, ApiError> {
        let after_id = match cursor {
            None => 0,
            Some(raw) => {
                let bytes = URL_SAFE_NO_PAD.decode(&raw).map_err(|_| {
                    ApiError::validation("cursor is not a valid opaque cursor", "cursor")
                })?;
                let payload: CursorPayload = serde_json::from_slice(&bytes).map_err(|_| {
                    ApiError::validation("cursor is not a valid opaque cursor", "cursor")
                })?;
                payload.a
            }
        };
        let limit = limit.unwrap_or(DEFAULT_PER_PAGE).min(MAX_PER_PAGE);
        Ok(Self { after_id, limit })
    }
}

/// The `pagination` envelope for cursor responses (§7): the position
/// this page ends at (as the opaque `cursor`), the next page's
/// opaque cursor, and `has_more`.
pub fn cursor_envelope<T: serde::Serialize>(
    params: CursorParams,
    page_end_id: u64,
    data: Vec<T>,
    has_more: bool,
) -> serde_json::Value {
    json!({
        "data": data,
        "pagination": {
            "cursor": CursorParams::encode_cursor(page_end_id),
            "next_cursor": if has_more {
                serde_json::json!(CursorParams::encode_cursor(page_end_id))
            } else {
                serde_json::Value::Null
            },
            "has_more": has_more,
            "after_id": params.after_id,
            "limit": params.limit,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_params_defaults_and_bounds() {
        let p = PageParams::from_query(None, None).unwrap();
        assert_eq!((p.page, p.per_page), (1, 50));
        assert_eq!(p.bounds(0), (0, 0));
        assert_eq!(p.bounds(10), (0, 10));

        let p = PageParams::from_query(Some(2), Some(3)).unwrap();
        assert_eq!(p.bounds(10), (3, 6));
        assert_eq!(p.bounds(4), (3, 4)); // short last page
        assert_eq!(p.bounds(2), (2, 2)); // past the end: empty
    }

    #[test]
    fn page_params_reject_zero_page() {
        assert!(PageParams::from_query(Some(0), None).is_err());
    }

    #[test]
    fn per_page_is_capped_at_200() {
        let p = PageParams::from_query(None, Some(100_000)).unwrap();
        assert_eq!(p.per_page, MAX_PER_PAGE);
    }

    #[test]
    fn cursors_round_trip_opaquely() {
        let token = CursorParams::encode_cursor(12345);
        assert!(!token.contains("12345")); // opaque to clients
        let p = CursorParams::from_query(Some(token), None).unwrap();
        assert_eq!(p.after_id, 12345);
        assert_eq!(p.limit, 50);
    }

    #[test]
    fn cursor_garbage_is_rejected() {
        assert!(CursorParams::from_query(Some("!!!".to_owned()), None).is_err());
        // Valid base64 wrapping invalid JSON.
        assert!(CursorParams::from_query(Some("aGVsbG8".to_owned()), None).is_err());
    }

    #[test]
    fn envelope_shapes_match_the_spec() {
        let env = page_envelope(
            PageParams {
                page: 2,
                per_page: 2,
            },
            5,
            vec!["a"],
        );
        assert_eq!(env["pagination"]["page"], 2);
        assert_eq!(env["pagination"]["total"], 5);
        assert_eq!(env["pagination"]["total_pages"], 3);
        assert!(env["data"].is_array());

        let env = cursor_envelope(
            CursorParams {
                after_id: 7,
                limit: 2,
            },
            9,
            vec!["x"],
            false,
        );
        assert_eq!(env["pagination"]["has_more"], false);
        assert!(env["pagination"]["next_cursor"].is_null());
        assert!(env["pagination"]["cursor"].is_string());
    }
}
