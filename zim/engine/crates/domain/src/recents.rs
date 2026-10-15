//! Recent-server rows: engine-backed quick-access state (M23).
//!
//! The frontend's recent/frequent tiles (`zim://newtab`, §9 of the
//! integration architecture) rank servers by what the user actually
//! opened — per BROWSER_INTEGRATION.md §11 this is *engine-backed
//! app state*, not a browser-history database. One row per (user,
//! instance): when it was last opened and how often. The store caps
//! the rows per user (quick access, not history); rows for deleted
//! instances are harmless orphans the read path filters.

use chrono::{DateTime, Utc};

use crate::id::{InstanceId, UserId};

/// One recent-servers row: a user's opening of one server view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentServer {
    /// The user whose quick-access state this is.
    pub user_id: UserId,
    /// The server that was opened.
    pub instance_id: InstanceId,
    /// When it was last opened (RFC 3339, UTC, at rest).
    pub last_opened_at: DateTime<Utc>,
    /// How many opens were recorded (from 1).
    pub open_count: u64,
}
