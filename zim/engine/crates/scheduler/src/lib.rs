//! Per-instance cron scheduler (roadmap M9).
//!
//! The persisted `Schedule` rows (M5's §10.6 surface) become
//! executable here: a driver loop ticks, every active schedule's
//! `next_run_at` is compared against the clock, due schedules
//! dispatch their actions through the supervision module's power
//! queue and console input, and the row's `next_run_at` /
//! `last_run_at` walk forward. Schedules survive engine restart
//! (the rows are persisted); on restart the loop re-initializes
//! any `next_run_at` that is `None` *without* firing it — a
//! freshly-seen schedule never fires for a past instant.
//!
//! # Semantics
//!
//! - **Expressions** are the domain's standard 5-field cron
//!   (minute hour day-of-month month day-of-week). The `cron`
//!   crate's 6-field dialect is fed a prepended zero-seconds field,
//!   so schedules fire at the top of their minute.
//! - **Timezone**: evaluation is in UTC (the `Schedule` row carries
//!   no timezone field; the API surface says so).
//! - **Missed instants coalesce**: if the engine was down across a
//!   scheduled instant, the schedule fires at most once on the next
//!   tick and then jumps to the next future instant (no catch-up
//!   storm — invariantly the Pterodactyl behavior operators expect).
//! - **`only_when_online`**: the gate consults the supervisor's
//!   observed state; an offline instance skips the fire (the slot
//!   is consumed, `last_run_at` stays untouched).
//! - **Run-now** (`POST /instances/{id}/schedules/{sid}/run`):
//!   dispatches the actions immediately, respecting the
//!   `only_when_online` gate, and stamps `last_run_at` — the cron
//!   schedule itself is not disturbed.
//!
//! # Determinism
//!
//! [`Scheduler::tick`] takes the `now` explicitly: the driver loop
//! feeds it the wall clock; the tests feed it synthetic instants
//! and assert the boundary behavior exactly.
//!
//! # Architecture boundary
//!
//! Depends on the application layer (persistence port +
//! supervision) and the domain layer (Schedule rows), per
//! `docs/architecture/RUST_ENGINE.md` §2.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;

use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::{ObservedState, PowerAction, Supervisor};
use zim_engine_domain::schedule::{PowerAction as DomainPowerAction, Schedule, ScheduleAction};
use zim_engine_domain::{InstanceId, ScheduleId};

/// The canonical crate name.
pub const CRATE_NAME: &str = "zim_engine_scheduler";

/// Tick-loop timing knobs.
#[derive(Debug, Clone)]
pub struct SchedulerPolicy {
    /// The driver loop's cadence (the comparison granularity; cron
    /// itself is minute-grained).
    pub tick_interval: Duration,
    /// The graceful-stop deadline handed to scheduled stop/restart
    /// actions.
    pub stop_deadline: Duration,
}

impl Default for SchedulerPolicy {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(1),
            stop_deadline: Duration::from_secs(30),
        }
    }
}

/// The per-instance cron scheduler (see the module docs).
pub struct Scheduler {
    persistence: Arc<dyn Persistence>,
    supervisor: Arc<Supervisor>,
    policy: SchedulerPolicy,
}

/// Scheduler failures.
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    /// The store failed.
    #[error("persistence: {0}")]
    Persistence(#[from] zim_engine_application::ports::PersistenceError),
}

/// Run-now failures.
#[derive(Debug, thiserror::Error)]
pub enum RunNowError {
    /// No schedule with that id.
    #[error("schedule {0} not found")]
    NotFound(ScheduleId),
    /// The store failed.
    #[error("persistence: {0}")]
    Persistence(#[from] zim_engine_application::ports::PersistenceError),
    /// The schedule is gated on the instance being online and it is
    /// not.
    #[error("the schedule only fires while the instance is running, and it is not")]
    InstanceOffline,
    /// The instance is not manageable by this engine.
    #[error("the instance is not manageable by this engine: {0}")]
    NotManageable(String),
    /// An action could not be dispatched.
    #[error("cannot dispatch the schedule's actions: {0}")]
    Dispatch(String),
}

impl Scheduler {
    /// Build the scheduler with the default policy.
    pub fn new(persistence: Arc<dyn Persistence>, supervisor: Arc<Supervisor>) -> Arc<Self> {
        Self::with_policy(persistence, supervisor, SchedulerPolicy::default())
    }

    /// Build the scheduler with explicit knobs (tests tighten the
    /// deadline; production leaves the default).
    pub fn with_policy(
        persistence: Arc<dyn Persistence>,
        supervisor: Arc<Supervisor>,
        policy: SchedulerPolicy,
    ) -> Arc<Self> {
        Arc::new(Self {
            persistence,
            supervisor,
            policy,
        })
    }

    /// Spawn the driver loop (call once per process, after the
    /// supervisor is up). The loop runs for the process lifetime,
    /// like the durable-job executor's own loops.
    pub fn start(self: &Arc<Self>) {
        let scheduler = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(scheduler.policy.tick_interval).await;
                let now = Utc::now();
                if let Err(e) = scheduler.tick(now).await {
                    eprintln!("zim_engine_scheduler: schedule scan failed: {e}");
                }
            }
        });
    }

    /// One scheduler step at `now`: initialize freshly-seen
    /// schedules, fire the due ones (through the
    /// [`crate::Scheduler::dispatch`] path), and walk every touched
    /// row's `next_run_at` / `last_run_at` forward. Returns how
    /// many schedules fired.
    pub async fn tick(&self, now: DateTime<Utc>) -> Result<usize, SchedulerError> {
        let schedules = {
            let persistence = Arc::clone(&self.persistence);
            tokio::task::spawn_blocking(move || persistence.list_all_schedules())
                .await
                .expect("schedule scan task panicked")?
        };
        let mut fired = 0usize;
        for mut schedule in schedules {
            if !schedule.is_active {
                continue;
            }
            // Parse (the domain checked the 5-field structure at the
            // API boundary; the semantic parse is here). A schedule
            // that fails it is parked with `next_run_at = None`.
            let cron = parse_cron(&schedule.cron);
            if cron.is_none() && schedule.next_run_at.is_some() {
                schedule.next_run_at = None;
                self.put(&schedule).await?;
                continue;
            }
            let Some(cron) = cron else {
                continue;
            };
            // The next fire strictly after `now`.
            let next = cron.after(&now).next();
            match schedule.next_run_at {
                // First sighting: initialize without firing (a new
                // or freshly-restarted schedule never fires for a
                // past instant).
                None => {
                    schedule.next_run_at = next;
                    self.put(&schedule).await?;
                }
                // Due — or overdue (missed instants coalesce into a
                // single fire).
                Some(due) if due <= now => {
                    let online = if schedule.only_when_online {
                        self.is_running(schedule.instance_id).await
                    } else {
                        true
                    };
                    if online {
                        if let Err(e) = self.dispatch(&schedule).await {
                            eprintln!(
                                "zim_engine_scheduler: schedule {:?} ({}) could not dispatch: {e}",
                                schedule.id, schedule.name
                            );
                        } else {
                            schedule.last_run_at = Some(now);
                            fired += 1;
                        }
                    }
                    schedule.next_run_at = next;
                    self.put(&schedule).await?;
                }
                // A future instant: already persisted, nothing to do.
                Some(_) => {}
            }
        }
        Ok(fired)
    }

    /// Run a schedule now (`POST /instances/{id}/schedules/{sid}/run`):
    /// dispatch the actions immediately, respect the
    /// `only_when_online` gate, stamp `last_run_at`, leave the cron
    /// schedule itself alone.
    pub async fn run_now(&self, schedule_id: ScheduleId) -> Result<(), RunNowError> {
        let mut schedule = {
            let persistence = Arc::clone(&self.persistence);
            tokio::task::spawn_blocking(move || persistence.get_schedule(&schedule_id))
                .await
                .expect("run-now schedule read task panicked")?
        }
        .ok_or(RunNowError::NotFound(schedule_id))?;
        if schedule.only_when_online && !self.is_running(schedule.instance_id).await {
            return Err(RunNowError::InstanceOffline);
        }
        self.dispatch(&schedule)
            .await
            .map_err(RunNowError::Dispatch)?;
        schedule.last_run_at = Some(Utc::now());
        let persistence = Arc::clone(&self.persistence);
        let row = schedule;
        tokio::task::spawn_blocking(move || persistence.put_schedule(&row))
            .await
            .expect("run-now schedule write task panicked")?;
        Ok(())
    }

    /// Dispatch a schedule's actions in order.
    async fn dispatch(&self, schedule: &Schedule) -> Result<(), String> {
        for action in &schedule.actions {
            match action {
                ScheduleAction::Power { action } => {
                    let power = self.power_action(*action);
                    self.supervisor
                        .request_power(&schedule.instance_id, power)
                        .await
                        .map_err(|e| format!("power action rejected: {e}"))?;
                }
                ScheduleAction::SendCommand { command } => {
                    self.supervisor
                        .send_console_input(&schedule.instance_id, command.as_bytes())
                        .await
                        .map_err(|e| format!("console input rejected: {e}"))?;
                }
            }
        }
        Ok(())
    }

    /// Whether the supervisor currently observes the instance
    /// `Running` (best-effort: an unobservable instance counts as
    /// offline for gating purposes).
    async fn is_running(&self, instance: InstanceId) -> bool {
        self.supervisor
            .observed(&instance)
            .await
            .map(|snapshot| matches!(snapshot.observed, ObservedState::Running { .. }))
            .unwrap_or(false)
    }

    /// Map the domain's power vocabulary onto the supervision
    /// module's (the same mapping the `/power` endpoints use).
    fn power_action(&self, action: DomainPowerAction) -> PowerAction {
        match action {
            DomainPowerAction::Start => PowerAction::Start,
            DomainPowerAction::Stop => PowerAction::Stop {
                deadline: self.policy.stop_deadline,
            },
            DomainPowerAction::Restart => PowerAction::Restart {
                deadline: self.policy.stop_deadline,
            },
            DomainPowerAction::Kill => PowerAction::Kill,
        }
    }

    /// Persist a schedule row (the blocking bridge).
    async fn put(&self, schedule: &Schedule) -> Result<(), SchedulerError> {
        let persistence = Arc::clone(&self.persistence);
        let row = schedule.clone();
        tokio::task::spawn_blocking(move || persistence.put_schedule(&row))
            .await
            .expect("schedule write task panicked")?;
        Ok(())
    }
}

/// Parse the domain's 5-field expression into the `cron` crate's
/// 6-field dialect (a prepended zero-seconds field: schedules fire
/// at the top of their minute).
fn parse_cron(expression: &str) -> Option<CronSchedule> {
    format!("0 {expression}").parse::<CronSchedule>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_field_expressions_parse_with_a_zero_seconds_field() {
        let schedule = parse_cron("*/5 * * * *").expect("5-field expressions parse");
        let start = Utc::now();
        let next = schedule.after(&start).next().expect("there is a next fire");
        // The next fire is at most 5 minutes out and lands on a
        // zero-second boundary.
        assert!(next > start);
        assert!(next - start <= chrono::Duration::minutes(5));
        assert_eq!(next.timestamp_subsec_millis(), 0);
    }

    #[test]
    fn malformed_expressions_do_not_parse() {
        assert!(parse_cron("not a cron").is_none());
        assert!(parse_cron("* * * *").is_none());
        assert!(parse_cron("61 * * * *").is_none());
    }

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_scheduler");
    }
}
