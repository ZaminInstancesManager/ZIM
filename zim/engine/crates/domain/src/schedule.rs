//! Schedule (power/command cron) entities.
//!
//! # Source of truth
//!
//! `Schedule` and `ScheduleAction` mirror
//! `docs/architecture/RUST_ENGINE.md` §3. The `cron` field carries a
//! standard 5-field (minute hour day-of-month month day-of-week)
//! expression; *semantic* cron evaluation (timezone, next-run
//! computation) lives in the `scheduler` crate (M-later) — the
//! domain layer performs only the structural 5-field check so that
//! malformed expressions are rejected at the API boundary.

use crate::id::{InstanceId, ScheduleId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A scheduled set of actions for an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Strongly-typed schedule identifier.
    pub id: ScheduleId,
    /// The instance this schedule belongs to.
    pub instance_id: InstanceId,
    /// Display name (non-empty).
    pub name: String,
    /// 5-field cron expression (structural validation only at the
    /// domain layer; see module docs).
    pub cron: String,
    /// Next fire time (None while paused or being recomputed).
    pub next_run_at: Option<DateTime<Utc>>,
    /// Last fire time (None if never fired).
    pub last_run_at: Option<DateTime<Utc>>,
    /// Whether the schedule is active.
    pub is_active: bool,
    /// Only fire when the instance is observed `Running`.
    pub only_when_online: bool,
    /// The actions to run when the schedule fires.
    pub actions: Vec<ScheduleAction>,
}

impl Schedule {
    /// Validate the invariants the constructor cannot enforce.
    pub fn validate(&self) -> Result<(), ScheduleError> {
        if self.name.trim().is_empty() {
            return Err(ScheduleError::EmptyName);
        }
        if !is_structurally_valid_cron(&self.cron) {
            return Err(ScheduleError::MalformedCron(self.cron.clone()));
        }
        if self.actions.is_empty() {
            return Err(ScheduleError::NoActions);
        }
        Ok(())
    }
}

/// Domain-level validation errors for Schedule.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScheduleError {
    /// The schedule name is empty or whitespace-only.
    #[error("schedule name must not be empty")]
    EmptyName,
    /// The cron expression is not 5 whitespace-separated non-empty
    /// fields.
    #[error("malformed cron expression {0:?}; expected 5 fields: minute hour day-of-month month day-of-week")]
    MalformedCron(String),
    /// The schedule has no actions.
    #[error("schedule must have at least one action")]
    NoActions,
}

/// Structural cron check: exactly 5 whitespace-separated fields,
/// each non-empty. (Field-level range validation is the scheduler
/// crate's job; the domain check catches the common
/// copy-paste-truncated expressions at the API boundary.)
fn is_structurally_valid_cron(expr: &str) -> bool {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    fields.len() == 5 && fields.iter().all(|f| !f.is_empty())
}

/// A single action a schedule performs when it fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleAction {
    /// Issue a power command to the instance.
    Power {
        /// Which power action to issue.
        action: PowerAction,
    },
    /// Send a line of console input to the instance.
    SendCommand {
        /// The console input to send.
        command: String,
    },
}

/// A power command (matches the API's power surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    /// Start the instance.
    Start,
    /// Gracefully stop the instance.
    Stop,
    /// Stop then start.
    Restart,
    /// Immediately kill (SIGKILL) the instance's process.
    Kill,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_schedule() -> Schedule {
        Schedule {
            id: ScheduleId::new(),
            instance_id: InstanceId::new(),
            name: "Nightly restart".to_owned(),
            cron: "0 4 * * *".to_owned(),
            next_run_at: Some(Utc::now()),
            last_run_at: None,
            is_active: true,
            only_when_online: true,
            actions: vec![
                ScheduleAction::Power {
                    action: PowerAction::Restart,
                },
                ScheduleAction::SendCommand {
                    command: "say Restarting in 5 minutes".to_owned(),
                },
            ],
        }
    }

    #[test]
    fn schedule_round_trips_through_serde() {
        let schedule = sample_schedule();
        let json = serde_json::to_string(&schedule).unwrap();
        let back: Schedule = serde_json::from_str(&json).unwrap();
        assert_eq!(schedule, back);
    }

    #[test]
    fn schedule_validate_accepts_sample() {
        assert!(sample_schedule().validate().is_ok());
    }

    #[test]
    fn malformed_cron_is_rejected() {
        let mut schedule = sample_schedule();
        for bad in ["", "* * * *", "* * * * * *", "0 4"] {
            schedule.cron = bad.to_owned();
            assert!(
                matches!(schedule.validate(), Err(ScheduleError::MalformedCron(_))),
                "cron {bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn cron_with_surrounding_whitespace_is_accepted() {
        // `split_whitespace` ignores leading/trailing whitespace —
        // a padded but well-formed expression is still 5 fields.
        let mut schedule = sample_schedule();
        schedule.cron = "  0 4 * * *  ".to_owned();
        assert!(schedule.validate().is_ok());
    }

    #[test]
    fn empty_name_and_no_actions_are_rejected() {
        let mut schedule = sample_schedule();
        schedule.name = "  ".to_owned();
        assert!(matches!(schedule.validate(), Err(ScheduleError::EmptyName)));

        let mut schedule = sample_schedule();
        schedule.actions = Vec::new();
        assert!(matches!(schedule.validate(), Err(ScheduleError::NoActions)));
    }

    #[test]
    fn schedule_action_tagged_serialization() {
        let json = serde_json::to_string(&ScheduleAction::Power {
            action: PowerAction::Kill,
        })
        .unwrap();
        assert_eq!(json, r#"{"type":"power","action":"kill"}"#);

        let json = serde_json::to_string(&ScheduleAction::SendCommand {
            command: "stop".to_owned(),
        })
        .unwrap();
        assert_eq!(json, r#"{"type":"send_command","command":"stop"}"#);

        let back: ScheduleAction = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back,
            ScheduleAction::SendCommand {
                command: "stop".to_owned()
            }
        );
    }

    #[test]
    fn power_action_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&PowerAction::Start).unwrap(),
            "\"start\""
        );
        assert_eq!(
            serde_json::to_string(&PowerAction::Restart).unwrap(),
            "\"restart\""
        );
    }
}
