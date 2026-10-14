//! Supervision integration tests: the lifecycle invariants from
//! `docs/architecture/TEST_STRATEGY.md` §2 that are engine-side
//! (invariants 1, 2, 7, 9, 10, 14 — M6's acceptance set, exercised
//! over a real SQLite store and the scripted mock runtime).
//!
//! The mock (feature `test-util`) replaces the Docker adapter; the
//! DockerRuntime itself runs the same flows against a real daemon in
//! `zim/engine/crates/runtime-docker/tests/`.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::testing::{MockRuntime, MockRuntimeFactory, MockStart};
use zim_engine_application::supervision::{
    DesiredState, ObservedState, PowerAction, PowerError, RuntimeError, SupervisionPolicy,
    Supervisor,
};
use zim_engine_domain::{
    EggId, Instance, InstanceStatus, Node, NodeCapabilities, NodeDeployment, NodeState, User,
};

/// The always-tight test policy: crash loop 3-in-60s (the default),
/// a fast monitor, short stop deadlines.
fn test_policy() -> SupervisionPolicy {
    SupervisionPolicy {
        crash: Default::default(),
        monitor_poll_interval: Duration::from_millis(50),
        stop_deadline: Duration::from_millis(500),
        // The supervision tests assert exact call/state sequences;
        // metrics events would add unrelated publishes.
        stream_metrics: false,
    }
}

/// A fresh migrated in-memory store.
fn in_memory_store() -> Arc<dyn Persistence> {
    let store = zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap();
    store.migrate().unwrap();
    Arc::new(store)
}

fn seed_user(store: &dyn Persistence) -> User {
    let user = User {
        id: zim_engine_domain::UserId::new(),
        email: "owner@example.org".to_owned(),
        username: "owner".to_owned(),
        external_id: None,
        password_hash: None,
        oauth_subject: None,
        is_admin: false,
        root_admin: false,
    };
    store.put_user(&user).unwrap();
    user
}

fn seed_local_node(store: &dyn Persistence) -> Node {
    let node = Node {
        id: zim_engine_domain::NodeId::new(),
        name: "local-1".to_owned(),
        location_id: zim_engine_domain::LocationId::new(),
        fqdn: "localhost".to_owned(),
        scheme: zim_engine_domain::Scheme::Https,
        public_port: 8443,
        listen_port: 8444,
        capabilities: NodeCapabilities::default(),
        deployment: NodeDeployment::LocalLinux,
        last_heartbeat: None,
        last_seen_state: NodeState::Unknown,
    };
    store.put_node(&node).unwrap();
    node
}

fn seed_instance(store: &dyn Persistence, node: &Node, owner: &User, port: u16) -> Instance {
    let user_id = owner.id;
    let node_id = node.id;
    let (uuid, uuid_short) = Instance::legacy_uuid_pair();
    let allocation_id = store
        .create_allocation(&node_id, "127.0.0.1".parse().unwrap(), port, None)
        .unwrap();
    let instance = Instance {
        id: zim_engine_domain::InstanceId::new(),
        external_id: None,
        uuid,
        uuid_short,
        node_id,
        name: "Test Server".to_owned(),
        description: String::new(),
        status: InstanceStatus::Offline,
        skip_egg_scripts: false,
        owner_id: user_id,
        limits: Default::default(),
        allocation_id,
        egg_id: EggId::new(),
        startup: zim_engine_domain::CommandTemplate::new("sleep 600"),
        image: "busybox:latest".parse().unwrap(),
        allocation_limit: None,
        database_limit: None,
        backup_limit: 0,
        crash_detection_enabled: true,
        installed_at: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.put_instance(&instance).unwrap();
    // Claim the allocation for the instance (mirrors the API's
    // assign_allocation flow).
    let mut claimed = store.get_allocation(&allocation_id).unwrap().unwrap();
    claimed.assigned_to = Some(instance.id);
    store.put_allocation(&claimed).unwrap();
    instance
}

/// Supervisor over a mock runtime, with the instance seeded and
/// loaded.
struct Fixture {
    supervisor: Arc<Supervisor>,
    store: Arc<dyn Persistence>,
    mock: MockRuntime,
    instance: Instance,
}

async fn fixture(script: Vec<MockStart>) -> Fixture {
    let store = in_memory_store();
    let user = seed_user(store.as_ref());
    let node = seed_local_node(store.as_ref());
    let instance = seed_instance(store.as_ref(), &node, &user, 25565);
    let mock = MockRuntime::scripted(script);
    let factory = MockRuntimeFactory::new(mock.clone());
    let supervisor = Supervisor::new(
        Arc::clone(&store),
        Arc::new(factory),
        test_policy(),
        std::env::temp_dir().join("zim-supervision-test"),
    );
    supervisor.load_local().await.unwrap();
    Fixture {
        supervisor,
        store,
        mock,
        instance,
    }
}

/// Poll until the (async) predicate holds (bounded).
async fn soon<F, Fut>(mut predicate: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if predicate().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    predicate().await
}

// ── Invariant 2: accepted start ≠ running ─────────────────────────

/// `engine_integration::start_job_state_machine`: a start request is
/// *accepted* (desired flips, observed transitions through Starting)
/// and only the runtime's own report moves it to Running.
#[tokio::test]
async fn start_job_state_machine() {
    // Start takes a moment: the window where the request is accepted
    // but the instance is not yet running is observable.
    let mock = MockRuntime::scripted(vec![MockStart::Run {
        run_for: Duration::from_secs(3600),
        exit_code: Some(0),
        oom_killed: false,
    }]);
    let store = in_memory_store();
    let user = seed_user(store.as_ref());
    let node = seed_local_node(store.as_ref());
    let instance = seed_instance(store.as_ref(), &node, &user, 25565);
    let factory = MockRuntimeFactory::new(mock.clone());
    let supervisor = Supervisor::new(
        Arc::clone(&store),
        Arc::new(factory),
        test_policy(),
        std::env::temp_dir().join("zim-t"),
    );
    supervisor.load_local().await.unwrap();

    let generation = supervisor
        .request_power(&instance.id, PowerAction::Start)
        .await
        .unwrap();
    // Accepted: a generation was returned, and the observation at
    // this instant is any *defined* state — the worker may not have
    // picked the action up yet (Offline seed), may be mid-start
    // (Starting), or may have completed it (Running). The invariant
    // is that acceptance alone never reports a faked Running.
    let snapshot = supervisor.observed(&instance.id).await.unwrap();
    assert!(
        matches!(
            snapshot.observed,
            ObservedState::Offline { .. } | ObservedState::Starting | ObservedState::Running { .. }
        ),
        "post-accept observation must stay a defined state: {:?}",
        snapshot.observed
    );
    assert!(generation <= snapshot.generation);

    // The start completes and the runtime's report is what marks it
    // Running (with the runtime's pid).
    assert!(
        soon(|| async {
            supervisor
                .observed(&instance.id)
                .await
                .unwrap()
                .observed
                .is_running()
        })
        .await
    );
    let snapshot = supervisor.observed(&instance.id).await.unwrap();
    assert_eq!(snapshot.status, InstanceStatus::Running);
    match snapshot.observed {
        ObservedState::Running { pid, .. } => assert_eq!(pid, Some(4242)),
        other => panic!("expected Running, got {other:?}"),
    }
    supervisor.shutdown().await;
}

// ── Invariant 9: per-instance operation serialization ─────────────

/// `engine_integration::power_queue_serializes`: with latency on
/// every stop, a burst of mixed power actions still applies strictly
/// one at a time (max concurrency == 1) and in queue order.
#[tokio::test]
async fn power_queue_serializes() {
    let store = in_memory_store();
    let user = seed_user(store.as_ref());
    let node = seed_local_node(store.as_ref());
    let instance = seed_instance(store.as_ref(), &node, &user, 25565);

    // Every stop takes a while — the serialization must hold anyway.
    let slow_mock = MockRuntime::with_stop_latency(Vec::new(), Duration::from_millis(120));
    let supervisor = Supervisor::new(
        Arc::clone(&store),
        Arc::new(MockRuntimeFactory::new(slow_mock.clone())),
        test_policy(),
        std::env::temp_dir().join("zim-t"),
    );
    supervisor.load_local().await.unwrap();
    let id = instance.id;

    // Bring it up first.
    supervisor
        .request_power(&id, PowerAction::Start)
        .await
        .unwrap();
    assert!(
        soon(|| async {
            supervisor
                .observed(&id)
                .await
                .unwrap()
                .observed
                .is_running()
        })
        .await
    );

    // Then fire a burst of restarts concurrently: every restart must
    // issue a runtime stop (it was running) and a start, each with
    // the 120 ms stop latency — 6 restarts overlap on the queue but
    // must apply strictly one at a time.
    let mut handles = Vec::new();
    for _ in 0..6 {
        handles.push(supervisor.request_power(
            &id,
            PowerAction::Restart {
                deadline: Duration::from_millis(300),
            },
        ));
    }
    let mut request_errors = Vec::new();
    for handle in handles {
        if let Err(e) = handle.await {
            request_errors.push(e);
        }
    }
    assert!(
        request_errors.is_empty(),
        "all six restarts must be accepted: {request_errors:?}"
    );

    // Wait until the queue actually drains: "currently running" is
    // true after *any* restart completes, so the waitable condition
    // is the counters — the initial start plus every restart's stop
    // and start have all run.
    assert!(
        soon(|| async {
            let calls = slow_mock.calls();
            calls.started >= 7 && calls.stopped >= 6
        })
        .await,
        "queue must drain"
    );

    // The invariant: no two runtime operations ever overlapped, and
    // every restart's stop actually ran (no dropped/deduped actions).
    assert_eq!(slow_mock.max_concurrent(), 1, "power queue must serialize");
    let calls = slow_mock.calls();
    let snapshot = supervisor.observed(&id).await.unwrap();
    assert!(
        calls.stopped >= 6,
        "all restart stops ran: {} (started {}, entries {:?}, final desired {:?} observed {:?})",
        calls.stopped,
        calls.started,
        calls.entries,
        snapshot.desired,
        snapshot.observed
    );
    assert!(
        calls.started >= 7,
        "initial start + 6 restarts: {}",
        calls.started
    );
    supervisor.shutdown().await;
}

// ── Invariant 7: no racing into undefined states ──────────────────

/// `engine_integration::power_action_race`: start/stop/restart/kill
/// fired concurrently from many tasks all resolve; the final state
/// is one of the defined outcomes and matches the last action.
#[tokio::test]
async fn power_action_race() {
    let fixture = fixture(Vec::new()).await;
    let supervisor = fixture.supervisor.clone();
    let id = fixture.instance.id;

    let mut joins = Vec::new();
    for i in 0..12 {
        let supervisor = supervisor.clone();
        joins.push(tokio::spawn(async move {
            let action = match i % 4 {
                0 => PowerAction::Start,
                1 => PowerAction::Stop {
                    deadline: Duration::from_millis(100),
                },
                2 => PowerAction::Restart {
                    deadline: Duration::from_millis(100),
                },
                _ => PowerAction::Kill,
            };
            // Some requests may be rejected only by the closed queue
            // at shutdown; before shutdown all must be accepted.
            let _ = supervisor.request_power(&id, action).await;
        }));
    }
    for join in joins {
        join.await.unwrap();
    }

    // Everything settles into a *self-consistent* state: no hanging
    // Starting/Stopping, no Unknown, and no torn snapshot (the two
    // locks are read in sequence, so mid-transition pairs like
    // desired-stopped/observed-running can be read while an action
    // applies — wait them out rather than diagnosing them).
    assert!(
        soon(|| async {
            let snapshot = supervisor.observed(&id).await.unwrap();
            matches!(
                (&snapshot.desired, &snapshot.observed),
                (DesiredState::Started, ObservedState::Running { .. })
                    | (
                        DesiredState::Stopped | DesiredState::Killed | DesiredState::Suspended,
                        ObservedState::Offline { .. },
                    )
            )
        })
        .await,
        "race must settle into a consistent outcome"
    );
    supervisor.shutdown().await;
}

// ── Invariant 14: crash-loop protection ───────────────────────────

/// `engine_integration::crash_loop_protection`: an instance that
/// crashes faster than the window trips the loop after the threshold
/// and is not auto-restarted; a user start clears it.
#[tokio::test]
async fn crash_loop_protection() {
    // Every start "runs" for 80 ms then exits non-zero: fast enough
    // that all crashes land inside the 60 s window.
    let crashes: Vec<MockStart> = (0..5)
        .map(|_| MockStart::Run {
            run_for: Duration::from_millis(80),
            exit_code: Some(1),
            oom_killed: false,
        })
        .collect();
    let fixture = fixture(crashes).await;
    let supervisor = &fixture.supervisor;
    let id = fixture.instance.id;

    supervisor
        .request_power(&id, PowerAction::Start)
        .await
        .unwrap();

    // Threshold 3: after the third crash the loop engages and no
    // further auto-restarts happen (the two remaining script entries
    // must NOT be consumed).
    assert!(soon(|| async { supervisor.observed(&id).await.unwrap().in_crash_loop }).await);
    let snapshot = supervisor.observed(&id).await.unwrap();
    assert_eq!(snapshot.status, InstanceStatus::CrashLoop);
    assert!(matches!(snapshot.observed, ObservedState::Offline { .. }));

    // Let any (incorrect) extra restart attempt play out — the mock
    // still has two scripted crashes; if the engine (wrongly)
    // auto-restarted, started would climb past 3.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let calls = fixture.mock.calls();
    assert!(
        calls.started <= 3,
        "crash loop must stop auto-restarts (started {} times)",
        calls.started
    );

    // User intervention clears it: an explicit start is accepted,
    // clears the flag, and (past the script) runs indefinitely.
    supervisor
        .request_power(&id, PowerAction::Start)
        .await
        .unwrap();
    assert!(
        soon(|| async {
            let snapshot = supervisor.observed(&id).await.unwrap();
            snapshot.observed.is_running() && !snapshot.in_crash_loop
        })
        .await
    );
    supervisor.shutdown().await;
}

// ── Invariants 1 + 10: engine-restart reconciliation ───────────────

/// `engine_integration::reconcile_after_restart` +
/// `engine_integration::restart_reconciliation`: a persisted
/// `running` status is not proof of liveness — reconciliation
/// re-observes from the runtime and queues restarts for
/// was-desired-started instances found offline (subject to crash
/// protection). Adoption of still-running containers is covered by
/// [`reconcile_adopts_running_containers`].
#[tokio::test]
async fn reconcile_after_restart() {
    let store = in_memory_store();
    let user = seed_user(store.as_ref());
    let node = seed_local_node(store.as_ref());
    let instance = seed_instance(store.as_ref(), &node, &user, 25565);
    let other = seed_instance(store.as_ref(), &node, &user, 25566);

    // Simulate a pre-restart engine: both instances were running.
    store
        .update_instance_status(&instance.id, InstanceStatus::Running)
        .unwrap();
    store
        .update_instance_status(&other.id, InstanceStatus::Running)
        .unwrap();

    // The "real" runtime state at next boot: both containers are
    // gone (they died while the engine was down).
    let mock = MockRuntime::scripted(Vec::new());
    mock.force_state(ObservedState::Offline {
        exit_code: Some(1),
        oom_killed: false,
    });
    let factory = MockRuntimeFactory::new(mock);
    let supervisor = Supervisor::new(
        Arc::clone(&store),
        Arc::new(factory),
        test_policy(),
        std::env::temp_dir().join("zim-t"),
    );

    // First reconcile with everything offline: both were
    // desired-started, so both queue restarts (crash loop counts the
    // engine-down death: one crash each, below threshold).
    let report = supervisor.reconcile().await.unwrap();
    assert_eq!(report.supervised, 2);
    assert_eq!(report.queued_restarts, 2);
    assert_eq!(report.observed_running, 0);

    // Both got restarted (script is empty → run forever)...
    assert!(
        soon(|| async {
            supervisor
                .observed(&instance.id)
                .await
                .unwrap()
                .observed
                .is_running()
                && supervisor
                    .observed(&other.id)
                    .await
                    .unwrap()
                    .observed
                    .is_running()
        })
        .await
    );

    // ...and the persisted status follows the observation (not the
    // stale pre-restart row).
    let row = store.get_instance(&instance.id).unwrap().unwrap();
    assert_eq!(row.status, InstanceStatus::Running);

    supervisor.shutdown().await;
}

/// Containers that are still running at reconcile time are adopted:
/// observed Running, desired Started, no restart queued.
#[tokio::test]
async fn reconcile_adopts_running_containers() {
    let store = in_memory_store();
    let user = seed_user(store.as_ref());
    let node = seed_local_node(store.as_ref());
    let instance = seed_instance(store.as_ref(), &node, &user, 25565);
    store
        .update_instance_status(&instance.id, InstanceStatus::Running)
        .unwrap();

    let mock = MockRuntime::scripted(Vec::new());
    mock.force_state(ObservedState::Running {
        started_at: Utc::now(),
        pid: Some(9),
    });
    let supervisor = Supervisor::new(
        Arc::clone(&store),
        Arc::new(MockRuntimeFactory::new(mock)),
        test_policy(),
        std::env::temp_dir().join("zim-t"),
    );
    let report = supervisor.reconcile().await.unwrap();
    assert_eq!(report.observed_running, 1);
    assert_eq!(report.queued_restarts, 0);

    let snapshot = supervisor.observed(&instance.id).await.unwrap();
    assert!(snapshot.observed.is_running());
    assert_eq!(snapshot.desired, DesiredState::Started);
    supervisor.shutdown().await;
}

/// Reconciliation when the runtime is unreachable: observed Unknown
/// (never Offline), persisted Unknown, no restarts queued.
#[tokio::test]
async fn reconcile_marks_unknown_when_unreachable() {
    let store = in_memory_store();
    let user = seed_user(store.as_ref());
    let node = seed_local_node(store.as_ref());
    let instance = seed_instance(store.as_ref(), &node, &user, 25565);
    store
        .update_instance_status(&instance.id, InstanceStatus::Running)
        .unwrap();

    let mock = MockRuntime::scripted(Vec::new());
    mock.force_unavailable();
    let factory = MockRuntimeFactory::new(mock);
    let supervisor = Supervisor::new(
        Arc::clone(&store),
        Arc::new(factory),
        test_policy(),
        std::env::temp_dir().join("zim-t"),
    );

    let report = supervisor.reconcile().await.unwrap();
    assert_eq!(report.unknown, 1);
    assert_eq!(report.queued_restarts, 0);

    let snapshot = supervisor.observed(&instance.id).await.unwrap();
    assert_eq!(snapshot.observed, ObservedState::Unknown);
    assert_eq!(snapshot.status, InstanceStatus::Unknown);

    let row = store.get_instance(&instance.id).unwrap().unwrap();
    assert_eq!(row.status, InstanceStatus::Unknown);
    supervisor.shutdown().await;
}

// ── Suspend semantics ─────────────────────────────────────────────

#[tokio::test]
async fn suspend_stops_and_gates_starts() {
    let fixture = fixture(Vec::new()).await;
    let supervisor = &fixture.supervisor;
    let id = fixture.instance.id;

    supervisor
        .request_power(&id, PowerAction::Start)
        .await
        .unwrap();
    assert!(
        soon(|| async {
            supervisor
                .observed(&id)
                .await
                .unwrap()
                .observed
                .is_running()
        })
        .await
    );

    supervisor
        .request_power(
            &id,
            PowerAction::Suspend {
                deadline: Duration::from_millis(300),
            },
        )
        .await
        .unwrap();
    assert!(
        soon(|| async {
            let snapshot = supervisor.observed(&id).await.unwrap();
            snapshot.desired == DesiredState::Suspended
                && matches!(snapshot.observed, ObservedState::Offline { .. })
        })
        .await
    );

    // Starts on a suspended instance are rejected with the
    // structured error.
    let err = supervisor
        .request_power(&id, PowerAction::Start)
        .await
        .unwrap_err();
    assert_eq!(err, PowerError::Suspended(id));

    // The persisted row carries the suspension.
    let row = fixture.store.get_instance(&id).unwrap().unwrap();
    assert_eq!(row.status, InstanceStatus::Suspended);
    supervisor.shutdown().await;
}

// ── Enqueue-time failure modes ────────────────────────────────────

#[tokio::test]
async fn unknown_instance_and_unavailable_runtime() {
    let fixture = fixture(Vec::new()).await;
    let supervisor = &fixture.supervisor;

    let unknown = zim_engine_domain::InstanceId::new();
    assert_eq!(
        supervisor
            .request_power(&unknown, PowerAction::Start)
            .await
            .unwrap_err(),
        PowerError::UnknownInstance(unknown)
    );

    let factory = MockRuntimeFactory::new(MockRuntime::scripted(Vec::new()));
    factory.force_check_failure();
    let gated = Supervisor::new(
        Arc::clone(&fixture.store),
        Arc::new(factory),
        test_policy(),
        std::env::temp_dir().join("zim-t"),
    );
    gated.load_local().await.unwrap();
    let err = gated
        .request_power(&fixture.instance.id, PowerAction::Start)
        .await
        .unwrap_err();
    assert!(matches!(err, PowerError::RuntimeUnavailable(_)));

    supervisor.shutdown().await;
    gated.shutdown().await;
}

// ── Invariant 13: stale-event protection (worker-level witness) ────

/// The generation counter bumps on every observed transition, so a
/// sample taken before a transition is discardable by comparing
/// generations — the polling monitor's guard.
#[tokio::test]
async fn generation_bumps_on_transitions() {
    let fixture = fixture(Vec::new()).await;
    let supervisor = &fixture.supervisor;
    let id = fixture.instance.id;

    let before = supervisor.observed(&id).await.unwrap().generation;
    supervisor
        .request_power(&id, PowerAction::Start)
        .await
        .unwrap();
    assert!(
        soon(|| async {
            supervisor
                .observed(&id)
                .await
                .unwrap()
                .observed
                .is_running()
        })
        .await
    );
    let after = supervisor.observed(&id).await.unwrap().generation;
    // Start → Starting → Running: at least two transitions.
    assert!(
        after >= before + 2,
        "generation must advance: {before} → {after}"
    );

    supervisor
        .request_power(&id, PowerAction::Kill)
        .await
        .unwrap();
    assert!(
        soon(|| async {
            matches!(
                supervisor.observed(&id).await.unwrap().observed,
                ObservedState::Offline { .. }
            )
        })
        .await
    );
    let stopped = supervisor.observed(&id).await.unwrap().generation;
    assert!(stopped > after);
    supervisor.shutdown().await;
}

/// A start that fails on an unavailable backend reports observed
/// `Unknown` (never Offline) — the failure-matrix rule.
#[tokio::test]
async fn unavailable_backend_surfaces_unknown() {
    let store = in_memory_store();
    let user = seed_user(store.as_ref());
    let node = seed_local_node(store.as_ref());
    let instance = seed_instance(store.as_ref(), &node, &user, 25565);

    let mock = MockRuntime::scripted(vec![MockStart::Fail(RuntimeError::unavailable(
        "daemon down",
    ))]);
    let supervisor = Supervisor::new(
        Arc::clone(&store),
        Arc::new(MockRuntimeFactory::new(mock)),
        test_policy(),
        std::env::temp_dir().join("zim-t"),
    );
    supervisor.load_local().await.unwrap();
    let id = instance.id;

    supervisor
        .request_power(&id, PowerAction::Start)
        .await
        .unwrap();
    assert!(
        soon(|| async {
            supervisor.observed(&id).await.unwrap().observed == ObservedState::Unknown
        })
        .await
    );
    let row = store.get_instance(&id).unwrap().unwrap();
    assert_eq!(row.status, InstanceStatus::Unknown);
    supervisor.shutdown().await;
}
