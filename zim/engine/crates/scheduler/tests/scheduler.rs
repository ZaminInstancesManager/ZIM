//! Scheduler integration tests: the three roadmap-M9 acceptance
//! criteria through the real supervisor (the scripted mock
//! runtime) and the in-memory store, driven by synthetic `now`
//! instants through [`Scheduler::tick`]:
//!
//! 1. A schedule fires at the configured cron time and dispatches
//!    the configured action.
//! 2. An `only_when_online` schedule is skipped while the instance
//!    is not running (and fires once it is).
//! 3. `run_now` dispatches immediately, respects the gate, and
//!    `missed instants coalesce into one fire`.

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};

use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::testing::{MockRuntime, MockRuntimeFactory};
use zim_engine_application::supervision::{
    ObservedState, PowerAction, SupervisionPolicy, Supervisor,
};
use zim_engine_domain::schedule::{PowerAction as DomainPowerAction, ScheduleAction};
use zim_engine_domain::{
    Allocation, AllocationId, Bytes, CommandTemplate, EggId, Instance, InstanceId, InstanceStatus,
    JobId, LocationId, Node, NodeCapabilities, NodeDeployment, NodeState, ResourceLimits, Schedule,
    ScheduleId, Scheme, User, UserId,
};
use zim_engine_scheduler::{RunNowError, Scheduler};

/// A fixed minute boundary (deterministic times throughout).
fn at(hour: u32, minute: u32, second: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, hour, minute, second)
        .unwrap()
}

/// The test harness: in-memory store + supervisor over the shared
/// mock runtime + the real scheduler (its driver loop is never
/// started — the tests drive [`Scheduler::tick`] directly).
struct Testbed {
    persistence: Arc<dyn Persistence>,
    supervisor: Arc<Supervisor>,
    scheduler: Arc<Scheduler>,
}

impl Testbed {
    async fn new() -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();
        let supervisor = Supervisor::new(
            Arc::clone(&persistence),
            Arc::new(MockRuntimeFactory::new(MockRuntime::always_running())),
            SupervisionPolicy {
                monitor_poll_interval: Duration::from_millis(50),
                ..Default::default()
            },
            std::env::temp_dir().join("zim-scheduler-test"),
        );
        let scheduler = Scheduler::new(Arc::clone(&persistence), Arc::clone(&supervisor));
        Self {
            persistence,
            supervisor,
            scheduler,
        }
    }

    /// Seed a local node, an owner, an allocation, and an offline
    /// instance; supervise it.
    async fn seed_instance(&self) -> Instance {
        let node = Node {
            id: zim_engine_domain::NodeId::new(),
            name: "local".to_owned(),
            location_id: LocationId::new(),
            fqdn: "localhost".to_owned(),
            scheme: Scheme::Https,
            public_port: 8443,
            listen_port: 8444,
            capabilities: NodeCapabilities::default(),
            deployment: NodeDeployment::LocalLinux,
            last_heartbeat: None,
            last_seen_state: NodeState::Unknown,
        };
        let user = User {
            id: UserId::new(),
            email: "owner@zim.local".to_owned(),
            username: "owner".to_owned(),
            external_id: None,
            password_hash: None,
            oauth_subject: None,
            is_admin: false,
            root_admin: false,
        };
        let allocation = Allocation {
            id: AllocationId::new(41000),
            node_id: node.id,
            ip: "127.0.0.1".parse().unwrap(),
            port: 41000,
            assigned_to: None,
            note: None,
        };
        let (uuid, uuid_short) = Instance::legacy_uuid_pair();
        let instance = Instance {
            id: InstanceId::new(),
            external_id: None,
            uuid,
            uuid_short,
            node_id: node.id,
            name: "Scheduler Test".to_owned(),
            description: String::new(),
            status: InstanceStatus::Offline,
            skip_egg_scripts: false,
            owner_id: user.id,
            limits: ResourceLimits {
                memory: Bytes::from_mib(512),
                swap: Bytes::from_bytes(0),
                disk: Bytes::from_gib(1),
                io_weight: 500,
                cpu_millicores: 500,
                threads: None,
                oom_disabled: false,
            },
            allocation_id: allocation.id,
            egg_id: EggId::new(),
            startup: CommandTemplate::new("sleep 1"),
            image: "alpine:latest".parse().unwrap(),
            allocation_limit: None,
            database_limit: None,
            backup_limit: 5,
            crash_detection_enabled: true,
            installed_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let persistence = Arc::clone(&self.persistence);
        let rows = (node, user, allocation, instance.clone());
        tokio::task::spawn_blocking(move || {
            let (node, user, allocation, instance) = rows;
            persistence.put_node(&node).unwrap();
            persistence.put_user(&user).unwrap();
            persistence.put_allocation(&allocation).unwrap();
            persistence.put_instance(&instance).unwrap();
        })
        .await
        .unwrap();
        self.supervisor.load_local().await.unwrap();
        instance
    }

    /// Persist a schedule for `instance` with the given cron and
    /// actions.
    async fn put_schedule(
        &self,
        instance: InstanceId,
        cron: &str,
        gated: bool,
        actions: Vec<ScheduleAction>,
    ) -> ScheduleId {
        let schedule = Schedule {
            id: ScheduleId::new(),
            instance_id: instance,
            name: "test schedule".to_owned(),
            cron: cron.to_owned(),
            next_run_at: None,
            last_run_at: None,
            is_active: true,
            only_when_online: gated,
            actions,
        };
        let id = schedule.id;
        let persistence = Arc::clone(&self.persistence);
        tokio::task::spawn_blocking(move || persistence.put_schedule(&schedule).unwrap())
            .await
            .unwrap();
        id
    }

    /// Read a schedule row.
    async fn schedule_row(&self, id: ScheduleId) -> Schedule {
        let persistence = Arc::clone(&self.persistence);
        tokio::task::spawn_blocking(move || persistence.get_schedule(&id))
            .await
            .unwrap()
            .unwrap()
            .expect("schedule row must exist")
    }
}

/// Poll the supervisor until the instance is observed `Running`.
async fn wait_for_running(supervisor: &Supervisor, instance: InstanceId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = supervisor.observed(&instance).await.expect("observed");
        if matches!(snapshot.observed, ObservedState::Running { .. }) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "instance never reached Running"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ── Acceptance: fires at the cron time, dispatches the action ─────

#[tokio::test]
async fn a_schedule_fires_at_the_configured_cron_time() {
    let bed = Testbed::new().await;
    let instance = bed.seed_instance().await;
    let schedule_id = bed
        .put_schedule(
            instance.id,
            "* * * * *",
            false,
            vec![ScheduleAction::Power {
                action: DomainPowerAction::Start,
            }],
        )
        .await;

    // First sighting at 12:00:30 — initialize, never fire for a
    // past instant.
    assert_eq!(bed.scheduler.tick(at(12, 0, 30)).await.unwrap(), 0);
    let row = bed.schedule_row(schedule_id).await;
    assert_eq!(row.next_run_at, Some(at(12, 1, 0)));
    assert_eq!(row.last_run_at, None);

    // Before the boundary: no fire.
    assert_eq!(bed.scheduler.tick(at(12, 0, 55)).await.unwrap(), 0);
    let row = bed.schedule_row(schedule_id).await;
    assert_eq!(row.last_run_at, None);

    // At the boundary (12:01:05 is past 12:01:00): fire — the
    // Start action dispatched through the supervisor.
    assert_eq!(bed.scheduler.tick(at(12, 1, 5)).await.unwrap(), 1);
    wait_for_running(&bed.supervisor, instance.id).await;

    // The row walked forward: fired at this tick, next due at
    // 12:02:00.
    let row = bed.schedule_row(schedule_id).await;
    assert_eq!(row.last_run_at, Some(at(12, 1, 5)));
    assert_eq!(row.next_run_at, Some(at(12, 2, 0)));

    // No double fire within the same minute.
    assert_eq!(bed.scheduler.tick(at(12, 1, 6)).await.unwrap(), 0);
}

#[tokio::test]
async fn a_send_command_schedule_dispatches_console_input() {
    // The console-input dispatch arm (the echo-console mock
    // accepts input while running).
    let persistence: Arc<dyn Persistence> = {
        let p: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        p.migrate().unwrap();
        p
    };
    let supervisor = Supervisor::new(
        Arc::clone(&persistence),
        Arc::new(MockRuntimeFactory::new(
            MockRuntime::always_running().with_echo_console(),
        )),
        SupervisionPolicy {
            monitor_poll_interval: Duration::from_millis(50),
            ..Default::default()
        },
        std::env::temp_dir().join("zim-scheduler-test-cmd"),
    );
    let scheduler = Scheduler::new(Arc::clone(&persistence), Arc::clone(&supervisor));
    let bed = Testbed {
        persistence,
        supervisor,
        scheduler,
    };
    let instance = bed.seed_instance().await;

    // The workload must be running for console input.
    bed.supervisor
        .request_power(&instance.id, PowerAction::Start)
        .await
        .unwrap();
    wait_for_running(&bed.supervisor, instance.id).await;

    let schedule_id = bed
        .put_schedule(
            instance.id,
            "* * * * *",
            false,
            vec![ScheduleAction::SendCommand {
                command: "say scheduled hello\n".to_owned(),
            }],
        )
        .await;

    assert_eq!(bed.scheduler.tick(at(12, 0, 30)).await.unwrap(), 0);
    // Due at 12:01: the input dispatches (any rejection would
    // surface as a zero count with the row's last_run_at unset).
    assert_eq!(bed.scheduler.tick(at(12, 1, 5)).await.unwrap(), 1);
    let row = bed.schedule_row(schedule_id).await;
    assert!(row.last_run_at.is_some());
}

// ── Acceptance: the only_when_online gate ─────────────────────────

#[tokio::test]
async fn an_online_gated_schedule_skips_while_the_instance_is_offline() {
    let bed = Testbed::new().await;
    let instance = bed.seed_instance().await;
    let schedule_id = bed
        .put_schedule(
            instance.id,
            "* * * * *",
            true,
            vec![ScheduleAction::Power {
                action: DomainPowerAction::Kill,
            }],
        )
        .await;

    // Initialize at 12:00:30.
    assert_eq!(bed.scheduler.tick(at(12, 0, 30)).await.unwrap(), 0);

    // Due at 12:01, but the instance is offline and the schedule is
    // gated: skipped (the Kill action must NOT dispatch — the
    // instance stays offline and the row shows no run).
    assert_eq!(bed.scheduler.tick(at(12, 1, 5)).await.unwrap(), 0);
    let row = bed.schedule_row(schedule_id).await;
    assert_eq!(row.last_run_at, None);
    assert_eq!(row.next_run_at, Some(at(12, 2, 0)));
    let snapshot = bed.supervisor.observed(&instance.id).await.unwrap();
    assert!(!matches!(snapshot.observed, ObservedState::Running { .. }));

    // Bring the workload up; the next slot fires.
    bed.supervisor
        .request_power(&instance.id, PowerAction::Start)
        .await
        .unwrap();
    wait_for_running(&bed.supervisor, instance.id).await;
    assert_eq!(bed.scheduler.tick(at(12, 2, 5)).await.unwrap(), 1);
    let row = bed.schedule_row(schedule_id).await;
    assert_eq!(row.last_run_at, Some(at(12, 2, 5)));
}

// ── Run-now ───────────────────────────────────────────────────────

#[tokio::test]
async fn run_now_dispatches_immediately_and_respects_the_gate() {
    let bed = Testbed::new().await;
    let instance = bed.seed_instance().await;

    // An ungated Start schedule: run-now brings the instance up.
    let start_id = bed
        .put_schedule(
            instance.id,
            "0 4 * * *",
            false,
            vec![ScheduleAction::Power {
                action: DomainPowerAction::Start,
            }],
        )
        .await;
    bed.scheduler.run_now(start_id).await.unwrap();
    wait_for_running(&bed.supervisor, instance.id).await;
    let row = bed.schedule_row(start_id).await;
    assert!(row.last_run_at.is_some());

    // A gated schedule while the instance now runs: accepted (the
    // gate consults the observed state, not the row).
    let gated_id = bed
        .put_schedule(
            instance.id,
            "0 5 * * *",
            true,
            vec![ScheduleAction::SendCommand {
                command: "ping\n".to_owned(),
            }],
        )
        .await;
    // The echo console is not enabled on this mock — the dispatch
    // surfaces the honest rejection.
    let err = bed.scheduler.run_now(gated_id).await.unwrap_err();
    assert!(matches!(err, RunNowError::Dispatch(_)));
    // But an offline gated schedule is refused before dispatch:
    // stop the workload and try a third, gated schedule.
    bed.supervisor
        .request_power(
            &instance.id,
            PowerAction::Stop {
                deadline: Duration::from_secs(5),
            },
        )
        .await
        .unwrap();
    let gated_id = bed
        .put_schedule(
            instance.id,
            "0 6 * * *",
            true,
            vec![ScheduleAction::Power {
                action: DomainPowerAction::Kill,
            }],
        )
        .await;
    let err = bed.scheduler.run_now(gated_id).await.unwrap_err();
    assert!(matches!(err, RunNowError::InstanceOffline));

    // Unknown schedule: NotFound.
    let err = bed.scheduler.run_now(ScheduleId::new()).await.unwrap_err();
    assert!(matches!(err, RunNowError::NotFound(_)));
}

// ── Missed instants coalesce ──────────────────────────────────────

#[tokio::test]
async fn missed_instants_coalesce_into_one_fire() {
    let bed = Testbed::new().await;
    let instance = bed.seed_instance().await;
    let schedule_id = bed
        .put_schedule(
            instance.id,
            "* * * * *",
            false,
            vec![ScheduleAction::Power {
                action: DomainPowerAction::Start,
            }],
        )
        .await;

    // Seed the row as if the engine had been down for over an hour:
    // the next_run_at is long past.
    let mut stale = bed.schedule_row(schedule_id).await;
    stale.next_run_at = Some(at(11, 0, 0));
    let persistence = Arc::clone(&bed.persistence);
    tokio::task::spawn_blocking(move || persistence.put_schedule(&stale).unwrap())
        .await
        .unwrap();

    // One tick at 12:05: fires exactly once and jumps to the next
    // future instant (no catch-up storm).
    assert_eq!(bed.scheduler.tick(at(12, 5, 0)).await.unwrap(), 1);
    let row = bed.schedule_row(schedule_id).await;
    assert_eq!(row.last_run_at, Some(at(12, 5, 0)));
    assert_eq!(row.next_run_at, Some(at(12, 6, 0)));
    wait_for_running(&bed.supervisor, instance.id).await;

    // A quiet job id space is untouched (the scheduler never
    // creates jobs).
    let jobs = {
        let persistence = Arc::clone(&bed.persistence);
        tokio::task::spawn_blocking(move || {
            persistence.list_jobs_by_state(zim_engine_domain::JobState::Pending)
        })
        .await
        .unwrap()
        .unwrap()
    };
    assert!(jobs.is_empty());
    let _ = JobId::new();
}

// ── Malformed cron parking ────────────────────────────────────────

#[tokio::test]
async fn a_semantically_invalid_cron_is_parked_not_fired() {
    // The domain layer checks only the 5-field structure; a
    // semantically invalid expression ("61" minutes) reaches the
    // scheduler, which parks it with next_run_at = None instead of
    // firing or crashing.
    let bed = Testbed::new().await;
    let instance = bed.seed_instance().await;
    let schedule_id = bed
        .put_schedule(
            instance.id,
            "61 * * * *",
            false,
            vec![ScheduleAction::Power {
                action: DomainPowerAction::Start,
            }],
        )
        .await;

    assert_eq!(bed.scheduler.tick(at(12, 0, 30)).await.unwrap(), 0);
    let row = bed.schedule_row(schedule_id).await;
    assert_eq!(row.next_run_at, None);
}
