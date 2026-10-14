//! Executor state-machine tests over an in-memory SQLite store:
//! claim/run/terminal transitions, failure + cleaner semantics,
//! cancellation (both states), crash reconciliation, per-instance
//! serialization, the concurrency cap, and hub event publication.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;

use zim_engine_application::ports::Persistence;
use zim_engine_domain::job::{Job, JobKind};
use zim_engine_domain::status::JobState;
use zim_engine_domain::{
    Allocation, AllocationId, Bytes, CommandTemplate, EggId, Instance, InstanceId, InstanceStatus,
    JobId, LocationId, Node, NodeCapabilities, NodeDeployment, NodeState, ResourceLimits, Scheme,
    User, UserId,
};
use zim_engine_jobs::{CancelStatus, JobContext, JobHandler, JobOutcome, JobPolicy, JobSystem};
use zim_engine_streaming::{EventData, StreamHub};

/// Read one job row through the blocking bridge (test sugar).
async fn get_job(persistence: Arc<dyn Persistence>, id: JobId) -> Job {
    tokio::task::spawn_blocking(move || persistence.get_job(&id))
        .await
        .expect("join")
        .expect("persistence")
        .expect("job row exists")
}

/// Write one job row through the blocking bridge.
async fn put_job(persistence: Arc<dyn Persistence>, job: Job) {
    tokio::task::spawn_blocking(move || persistence.put_job(&job))
        .await
        .expect("join")
        .expect("persistence write")
}

/// Seed a real instance row (the jobs table's FK demands one) and
/// return its id.
async fn seed_instance(persistence: Arc<dyn Persistence>) -> InstanceId {
    let node = Node {
        id: zim_engine_domain::NodeId::new(),
        name: "local-test".to_owned(),
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
        id: AllocationId::new(40000),
        node_id: node.id,
        ip: "127.0.0.1".parse().unwrap(),
        port: 40000,
        assigned_to: None,
        note: None,
    };
    let (uuid, uuid_short) = Instance::legacy_uuid_pair();
    let user = User {
        id: user.id,
        email: format!("owner-{uuid_short}@zim.local"),
        username: format!("owner-{uuid_short}"),
        ..user
    };
    let instance = Instance {
        id: InstanceId::new(),
        external_id: None,
        uuid,
        uuid_short,
        node_id: node.id,
        name: "Jobs Test".to_owned(),
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
    let id = instance.id;
    tokio::task::spawn_blocking(move || {
        persistence.put_node(&node).unwrap();
        persistence.put_user(&user).unwrap();
        persistence.put_allocation(&allocation).unwrap();
        persistence.put_instance(&instance).unwrap();
    })
    .await
    .unwrap();
    id
}

/// A scripted handler: what its `run` does, and what its `cleanup`
/// has been asked to remove.
struct ScriptedHandler {
    /// What `run` answers after (possibly) waiting.
    behavior: Behavior,
    /// Blocks the handler's run until this gate opens (None: no
    /// gate).
    gate: Mutex<()>,
    /// Times `cleanup` was called.
    cleanups: AtomicUsize,
    /// Whether `cleanup` should fail the first N calls (retry
    /// semantics).
    cleanup_failures_first: AtomicUsize,
}

enum Behavior {
    Succeed,
    Fail {
        message: &'static str,
        cleanup_pending: bool,
    },
    /// Wait for cancellation, then answer `Cancelled`.
    AwaitCancel,
    /// Panic inside the handler.
    Panic,
}

impl ScriptedHandler {
    fn succeed() -> Arc<Self> {
        Arc::new(Self {
            behavior: Behavior::Succeed,
            gate: Mutex::new(()),
            cleanups: AtomicUsize::new(0),
            cleanup_failures_first: AtomicUsize::new(0),
        })
    }

    fn fail(message: &'static str, cleanup_pending: bool) -> Arc<Self> {
        Arc::new(Self {
            behavior: Behavior::Fail {
                message,
                cleanup_pending,
            },
            gate: Mutex::new(()),
            cleanups: AtomicUsize::new(0),
            cleanup_failures_first: AtomicUsize::new(0),
        })
    }

    fn await_cancel() -> Arc<Self> {
        Arc::new(Self {
            behavior: Behavior::AwaitCancel,
            gate: Mutex::new(()),
            cleanups: AtomicUsize::new(0),
            cleanup_failures_first: AtomicUsize::new(0),
        })
    }

    fn panic() -> Arc<Self> {
        Arc::new(Self {
            behavior: Behavior::Panic,
            gate: Mutex::new(()),
            cleanups: AtomicUsize::new(0),
            cleanup_failures_first: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl JobHandler for ScriptedHandler {
    async fn run(&self, ctx: JobContext) -> JobOutcome {
        // Hold the gate so tests can act while the job "runs".
        let _held = self.gate.lock().await;
        match &self.behavior {
            Behavior::Succeed => JobOutcome::Succeeded,
            Behavior::Fail {
                message,
                cleanup_pending,
            } => JobOutcome::Failed {
                message: (*message).to_owned(),
                cleanup_pending: *cleanup_pending,
            },
            Behavior::AwaitCancel => {
                ctx.cancelled().await;
                JobOutcome::Cancelled
            }
            Behavior::Panic => panic!("scripted handler panic"),
        }
    }

    async fn cleanup(&self, _job: &Job) -> Result<(), String> {
        let n = self.cleanups.fetch_add(1, Ordering::SeqCst) + 1;
        let first_failures = self.cleanup_failures_first.load(Ordering::SeqCst);
        if n <= first_failures {
            Err("scripted cleanup failure".to_owned())
        } else {
            Ok(())
        }
    }
}

/// The test harness: an in-memory store + a fast-policy executor
/// over a hub.
struct Testbed {
    persistence: Arc<dyn Persistence>,
    system: Arc<JobSystem>,
    hub: StreamHub,
}

impl Testbed {
    async fn new() -> Self {
        Self::build(true).await
    }

    /// A testbed whose executor loops have not been started (for
    /// tests that need rows to sit in a state without the queue
    /// claiming them).
    async fn new_stopped() -> Self {
        Self::build(false).await
    }

    async fn build(start: bool) -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();
        let hub = StreamHub::new(Default::default());
        let system = JobSystem::new(
            Arc::clone(&persistence),
            Some(hub.clone()),
            JobPolicy {
                poll_interval: Duration::from_millis(10),
                cleaner_interval: Duration::from_millis(25),
                max_concurrent: 2,
                shutdown_grace: Duration::from_secs(5),
            },
        );
        if start {
            system.start();
        }
        Self {
            persistence,
            system,
            hub,
        }
    }

    async fn register(&self, handler: Arc<dyn JobHandler>) {
        self.system.register_handler(JobKind::Backup, handler).await;
    }

    /// A fresh Pending job row (not yet enqueued).
    fn job(instance: InstanceId) -> Job {
        let now = chrono::Utc::now();
        Job {
            id: JobId::new(),
            kind: JobKind::Backup,
            instance_id: instance,
            state: JobState::Pending,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            last_error: None,
            cleanup_pending: false,
            generation: 0,
            payload: None,
        }
    }

    /// Poll the store until the job reaches `want` (or the deadline
    /// passes, failing the test with the last observed state).
    async fn wait_for_state(&self, id: JobId, want: JobState) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let job = get_job(Arc::clone(&self.persistence), id).await;
            if job.state == want {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "job {id} never reached {want:?}; last seen {:?}",
                job.state
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test]
async fn pending_job_is_claimed_and_succeeds() {
    let bed = Testbed::new().await;
    let handler = ScriptedHandler::succeed();
    bed.register(handler).await;

    let instance = seed_instance(Arc::clone(&bed.persistence)).await;
    let job = bed.system.enqueue(Testbed::job(instance)).await.unwrap();
    bed.wait_for_state(job.id, JobState::Succeeded).await;

    // The row carries the execution window.
    let row = get_job(Arc::clone(&bed.persistence), job.id).await;
    assert!(row.started_at.is_some());
    assert!(row.completed_at.is_some());
    assert_eq!(row.last_error, None);
    assert!(!row.cleanup_pending);
}

#[tokio::test]
async fn failure_without_partial_state_lands_in_failed() {
    let bed = Testbed::new().await;
    bed.register(ScriptedHandler::fail("nope", false)).await;

    let job = bed
        .system
        .enqueue(Testbed::job(
            seed_instance(Arc::clone(&bed.persistence)).await,
        ))
        .await
        .unwrap();
    bed.wait_for_state(job.id, JobState::Failed).await;

    let row = get_job(Arc::clone(&bed.persistence), job.id).await;
    assert_eq!(row.last_error.as_deref(), Some("nope"));
    assert!(!row.cleanup_pending);
}

#[tokio::test]
async fn failure_with_partial_state_is_cleaned_then_failed() {
    let bed = Testbed::new().await;
    let handler = ScriptedHandler::fail("interrupted", true);
    let dyn_handler: Arc<dyn JobHandler> = handler.clone();
    bed.register(dyn_handler).await;

    let job = bed
        .system
        .enqueue(Testbed::job(
            seed_instance(Arc::clone(&bed.persistence)).await,
        ))
        .await
        .unwrap();

    // Pending → InProgress → CleanupPending (non-terminal) → the
    // cleaner's pass → terminal Failed with the flag cleared. The
    // cleaner wakes immediately, so the intermediate state is not
    // reliably observable; assert the terminal outcome and that the
    // kind-specific cleanup actually ran.
    bed.wait_for_state(job.id, JobState::Failed).await;
    assert_eq!(handler.cleanups.load(Ordering::SeqCst), 1);
    let row = get_job(Arc::clone(&bed.persistence), job.id).await;
    assert!(!row.cleanup_pending);
    assert!(row.last_error.as_deref().unwrap().contains("interrupted"));
}

#[tokio::test]
async fn cleaner_retries_a_failing_cleanup() {
    let bed = Testbed::new().await;
    let handler = ScriptedHandler::fail("boom", true);
    // The first two cleanup attempts fail; the third succeeds.
    handler.cleanup_failures_first.store(2, Ordering::SeqCst);
    let dyn_handler: Arc<dyn JobHandler> = handler.clone();
    bed.register(dyn_handler).await;

    let job = bed
        .system
        .enqueue(Testbed::job(
            seed_instance(Arc::clone(&bed.persistence)).await,
        ))
        .await
        .unwrap();
    bed.wait_for_state(job.id, JobState::Failed).await;
    // Two failures + the successful third pass.
    assert!(handler.cleanups.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn pending_job_cancels_immediately() {
    // Stopped executor: the enqueued row stays Pending long enough
    // to take the direct cancellation path deterministically.
    let bed = Testbed::new_stopped().await;
    // A handler that would run forever if claimed.
    bed.register(ScriptedHandler::await_cancel()).await;

    // Enqueue without starting the executor: the row sits Pending.
    let job = bed
        .system
        .enqueue(Testbed::job(
            seed_instance(Arc::clone(&bed.persistence)).await,
        ))
        .await
        .unwrap();

    let status = bed.system.cancel(job.id).await.unwrap();
    assert_eq!(status, CancelStatus::Cancelled);

    let row = get_job(Arc::clone(&bed.persistence), job.id).await;
    assert_eq!(row.state, JobState::Cancelled);
    assert!(row.completed_at.is_some());

    // A second cancel attempt is a conflict.
    assert!(bed.system.cancel(job.id).await.is_err());
}

#[tokio::test]
async fn inprogress_job_cancels_cooperatively() {
    let bed = Testbed::new().await;
    bed.register(ScriptedHandler::await_cancel()).await;

    let job = bed
        .system
        .enqueue(Testbed::job(
            seed_instance(Arc::clone(&bed.persistence)).await,
        ))
        .await
        .unwrap();
    bed.wait_for_state(job.id, JobState::InProgress).await;

    let status = bed.system.cancel(job.id).await.unwrap();
    assert_eq!(status, CancelStatus::Cancelling);
    bed.wait_for_state(job.id, JobState::Cancelled).await;

    let row = get_job(Arc::clone(&bed.persistence), job.id).await;
    assert_eq!(row.last_error, None);
}

#[tokio::test]
async fn panicking_handler_becomes_cleanup_pending_failure() {
    let bed = Testbed::new().await;
    bed.register(ScriptedHandler::panic()).await;

    let job = bed
        .system
        .enqueue(Testbed::job(
            seed_instance(Arc::clone(&bed.persistence)).await,
        ))
        .await
        .unwrap();
    // The panic is contained: the job still lands (via the cleaner)
    // in Failed, with the panic recorded. (The panic hook prints the
    // scripted panic to stderr; that is the containment, not a
    // failure.)
    bed.wait_for_state(job.id, JobState::Failed).await;
    let row = get_job(Arc::clone(&bed.persistence), job.id).await;
    assert!(row.last_error.as_deref().unwrap().contains("panic"));
}

#[tokio::test]
async fn orphaned_inprogress_jobs_are_reconciled_at_boot() {
    let bed = Testbed::new().await;
    // A previous process died mid-run: seed the row directly, the
    // way a crash would have left it.
    let mut orphan = Testbed::job(seed_instance(Arc::clone(&bed.persistence)).await);
    orphan.state = JobState::InProgress;
    orphan.started_at = Some(chrono::Utc::now());
    let orphan_id = orphan.id;
    put_job(Arc::clone(&bed.persistence), orphan).await;

    // The handler must NOT re-run the orphan (a fresh run of a
    // half-finished job would be wrong); only its cleanup runs.
    bed.register(ScriptedHandler::succeed()).await;

    let count = bed.system.reconcile_orphans().await.unwrap();
    assert_eq!(count, 1);
    bed.wait_for_state(orphan_id, JobState::CleanupPending)
        .await;
    bed.wait_for_state(orphan_id, JobState::Failed).await;

    let row = get_job(Arc::clone(&bed.persistence), orphan_id).await;
    assert!(row
        .last_error
        .as_deref()
        .unwrap()
        .contains("engine restarted"));
}

#[tokio::test]
async fn per_instance_serialization_holds_the_second_job() {
    let bed = Testbed::new().await;
    bed.register(ScriptedHandler::await_cancel()).await;

    let instance = seed_instance(Arc::clone(&bed.persistence)).await;
    let first = bed.system.enqueue(Testbed::job(instance)).await.unwrap();
    bed.wait_for_state(first.id, JobState::InProgress).await;

    let second = bed.system.enqueue(Testbed::job(instance)).await.unwrap();
    // Give the queue a few poll cycles: the second job must stay
    // Pending while the first runs.
    tokio::time::sleep(Duration::from_millis(120)).await;
    let row = get_job(Arc::clone(&bed.persistence), second.id).await;
    assert_eq!(row.state, JobState::Pending);

    // Different instances run in parallel (up to the cap of 2).
    let other = bed
        .system
        .enqueue(Testbed::job(
            seed_instance(Arc::clone(&bed.persistence)).await,
        ))
        .await
        .unwrap();
    bed.wait_for_state(other.id, JobState::InProgress).await;
    assert_eq!(bed.system.running_count().await, 2);
}

#[tokio::test]
async fn job_events_reach_the_hub() {
    let bed = Testbed::new().await;
    // A handler that reports progress then succeeds.
    struct Reporting;
    #[async_trait]
    impl JobHandler for Reporting {
        async fn run(&self, ctx: JobContext) -> JobOutcome {
            ctx.progress("archiving", 0.25);
            ctx.progress("archiving", 0.75);
            JobOutcome::Succeeded
        }
        async fn cleanup(&self, _job: &Job) -> Result<(), String> {
            Ok(())
        }
    }
    bed.register(Arc::new(Reporting)).await;

    let job = bed
        .system
        .enqueue(Testbed::job(
            seed_instance(Arc::clone(&bed.persistence)).await,
        ))
        .await
        .unwrap();

    // Subscribe before the terminal event fires.
    let handle = bed
        .hub
        .subscribe(&zim_engine_streaming::StreamFilter::instance(
            job.instance_id,
        ))
        .unwrap();
    bed.wait_for_state(job.id, JobState::Succeeded).await;

    let mut saw_progress = false;
    let mut saw_completed = false;
    // Drain with a deadline: the terminal event is the last one the
    // executor publishes for this job.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let next = tokio::time::timeout_at(deadline, handle.recv()).await;
        let event = match next {
            Ok(Some(event)) => event,
            Ok(None) | Err(_) => break,
        };
        match &event.data {
            EventData::JobProgress { job_id, .. } if *job_id == job.id => saw_progress = true,
            EventData::JobCompleted { job_id, success } if *job_id == job.id => {
                saw_completed = true;
                assert!(*success);
            }
            _ => {}
        }
        if saw_progress && saw_completed {
            break;
        }
    }
    handle.close();
    assert!(saw_progress, "no job.progress event observed");
    assert!(saw_completed, "no job.completed event observed");
}
