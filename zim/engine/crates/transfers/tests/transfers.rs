//! Transfer-system tests: the service's validation and row
//! lifecycle, and the three roadmap-M8 transfer acceptance
//! criteria end-to-end through the durable-job executor:
//!
//! 1. A transfer goes through the two-phase commit: the instance
//!    lands on the destination node, rebound onto its allocation,
//!    with the data moved and the transfer tree cleaned.
//! 2. A partial failure leaves the job `Failed` *without affecting
//!    the source instance's running state* — fault-injected by a
//!    read-only transfer-tree parent (the archive step fails after
//!    the stop was confirmed), asserting the power-restore path
//!    brings the workload back.
//! 3. A crashed transfer (interrupted mid-archive) is reconciled at
//!    boot: the job lands `Failed` and the cleaner removes the
//!    partial tree, without blocking the instance's next transfer.
//!
//! plus the companion-row lifecycle (a transfer cancelled straight
//! out of `Pending` — no handler ever runs — must not leave the
//! instance permanently untransferable).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::instance_data_dir;
use zim_engine_application::supervision::testing::{MockRuntime, MockRuntimeFactory};
use zim_engine_application::supervision::{
    ObservedState, PowerAction, SupervisionPolicy, Supervisor,
};
use zim_engine_domain::job::{Job, JobKind};
use zim_engine_domain::status::{JobState, TransferState};
use zim_engine_domain::{
    Allocation, AllocationId, Bytes, CommandTemplate, EggId, Instance, InstanceId, InstanceStatus,
    JobId, LocationId, Node, NodeCapabilities, NodeDeployment, NodeId, NodeState, ResourceLimits,
    Scheme, Transfer, TransferId, User, UserId,
};
use zim_engine_jobs::{JobContext, JobHandler, JobOutcome, JobPolicy, JobSystem};
use zim_engine_transfers::{TransferError, TransferService};

/// A unique scratch directory per call (same shape as the backups
/// tests: process id + an atomic counter + the test's name).
fn scratch(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "zim-transfers-test-{}-{n}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The test harness: in-memory store, a supervisor over the shared
/// mock runtime, a fast executor, and the real transfer service over
/// a scratch data root.
struct Testbed {
    persistence: Arc<dyn Persistence>,
    supervisor: Arc<Supervisor>,
    jobs: Arc<JobSystem>,
    transfers: Arc<TransferService>,
    data_root: PathBuf,
}

impl Testbed {
    async fn new() -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();
        let data_root = scratch("data");
        let supervisor = Supervisor::new(
            Arc::clone(&persistence),
            Arc::new(MockRuntimeFactory::new(MockRuntime::always_running())),
            SupervisionPolicy {
                monitor_poll_interval: Duration::from_millis(50),
                ..Default::default()
            },
            data_root.clone(),
        );
        let jobs = JobSystem::new(
            Arc::clone(&persistence),
            None,
            JobPolicy {
                poll_interval: Duration::from_millis(10),
                cleaner_interval: Duration::from_millis(20),
                max_concurrent: 2,
                shutdown_grace: Duration::from_secs(5),
            },
        );
        let transfers = TransferService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            Arc::clone(&supervisor),
            data_root.clone(),
        );
        transfers.register().await;
        jobs.start();
        Self {
            persistence,
            supervisor,
            jobs,
            transfers,
            data_root,
        }
    }

    /// Seed the move's topology: a source node and a destination
    /// node (both engine-local — the milestone's execution model),
    /// a free allocation on each, an owner, and an offline instance
    /// on the source node with `files` in its data directory.
    async fn seed_topology(&self, files: &[(&str, &str)]) -> Seeded {
        let node_a = Node {
            id: NodeId::new(),
            name: "source-node".to_owned(),
            location_id: LocationId::new(),
            fqdn: "source.zim.local".to_owned(),
            scheme: Scheme::Https,
            public_port: 8443,
            listen_port: 8444,
            capabilities: NodeCapabilities::default(),
            deployment: NodeDeployment::LocalLinux,
            last_heartbeat: None,
            last_seen_state: NodeState::Unknown,
        };
        let node_b = Node {
            id: NodeId::new(),
            name: "destination-node".to_owned(),
            location_id: LocationId::new(),
            fqdn: "destination.zim.local".to_owned(),
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
        let alloc_a = Allocation {
            id: AllocationId::new(41000),
            node_id: node_a.id,
            ip: "127.0.0.1".parse().unwrap(),
            port: 41000,
            assigned_to: None,
            note: None,
        };
        let alloc_b = Allocation {
            id: AllocationId::new(42000),
            node_id: node_b.id,
            ip: "127.0.0.1".parse().unwrap(),
            port: 42000,
            assigned_to: None,
            note: None,
        };
        let (uuid, uuid_short) = Instance::legacy_uuid_pair();
        let instance = Instance {
            id: InstanceId::new(),
            external_id: None,
            uuid: uuid.clone(),
            uuid_short: uuid_short.clone(),
            node_id: node_a.id,
            name: "Transfers Test".to_owned(),
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
            allocation_id: alloc_a.id,
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
        let rows = (
            node_a.clone(),
            node_b.clone(),
            user.clone(),
            alloc_a.clone(),
            alloc_b.clone(),
            instance.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let (node_a, node_b, user, alloc_a, alloc_b, instance) = rows;
            persistence.put_node(&node_a).unwrap();
            persistence.put_node(&node_b).unwrap();
            persistence.put_user(&user).unwrap();
            persistence.put_allocation(&alloc_a).unwrap();
            persistence.put_allocation(&alloc_b).unwrap();
            persistence.put_instance(&instance).unwrap();
            // Claim the source allocation for the instance (the
            // production create flow's second half): the transfer's
            // rebind will release it.
            let mut claimed = alloc_a;
            claimed.assigned_to = Some(instance.id);
            persistence.put_allocation(&claimed).unwrap();
        })
        .await
        .unwrap();

        // The instance's data directory + files.
        let data_dir = instance_data_dir(&self.data_root, &uuid_short);
        for (path, contents) in files {
            let file = data_dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, contents).unwrap();
        }

        // Supervise the seeded instance explicitly (mirrors the
        // production boot's load_local).
        self.supervisor.load_local().await.unwrap();

        Seeded {
            node_a,
            node_b,
            alloc_a,
            alloc_b,
            instance,
        }
    }

    /// Poll persistence until the job reaches `want`.
    async fn wait_for_job(&self, id: JobId, want: JobState) -> Job {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let p = Arc::clone(&self.persistence);
            let job = tokio::task::spawn_blocking(move || p.get_job(&id))
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if job.state == want {
                return job;
            }
            if job.state.is_terminal() {
                panic!("job reached terminal {job:?} instead of {want:?}");
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "job {id} never reached {want:?}; last seen {:?}",
                job.state
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Poll the supervisor until the instance is observed `Running`.
    async fn wait_for_observed_running(&self, id: InstanceId) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = self
                .supervisor
                .observed(&id)
                .await
                .expect("supervisor must answer");
            if matches!(snapshot.observed, ObservedState::Running { .. }) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "instance {id} never reached Running; last observed {:?}",
                snapshot.observed
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Read the transfer row (panics when missing).
    async fn transfer_row(&self, id: TransferId) -> Transfer {
        let p = Arc::clone(&self.persistence);
        tokio::task::spawn_blocking(move || p.get_transfer(&id))
            .await
            .unwrap()
            .unwrap()
            .expect("transfer row must exist")
    }

    /// Read the instance row (panics when missing).
    async fn instance_row(&self, id: InstanceId) -> Instance {
        let p = Arc::clone(&self.persistence);
        tokio::task::spawn_blocking(move || p.get_instance(&id))
            .await
            .unwrap()
            .unwrap()
            .expect("instance row must exist")
    }

    /// Read an allocation row (panics when missing).
    async fn allocation_row(&self, id: AllocationId) -> Allocation {
        let p = Arc::clone(&self.persistence);
        tokio::task::spawn_blocking(move || p.get_allocation(&id))
            .await
            .unwrap()
            .unwrap()
            .expect("allocation row must exist")
    }
}

/// What [`Testbed::seed_topology`] seeded.
struct Seeded {
    node_a: Node,
    node_b: Node,
    alloc_a: Allocation,
    alloc_b: Allocation,
    instance: Instance,
}

// ── Acceptance: the two-phase commit ──────────────────────────────

#[tokio::test]
async fn transfer_moves_instance_to_the_destination_node() {
    // Roadmap M8 acceptance criterion: a transfer goes through the
    // two-phase commit — stop, archive, ship+verify, extract,
    // commit, rebind, (no start: the workload was offline), clean.
    let bed = Testbed::new().await;
    let seeded = bed
        .seed_topology(&[
            ("server.properties", "motd=transferred\n"),
            ("world/level.dat", "0123456789"),
        ])
        .await;

    let (job, transfer) = bed
        .transfers
        .initiate(seeded.instance.id, seeded.node_b.id, seeded.alloc_b.id)
        .await
        .expect("initiation must be accepted");
    assert_eq!(transfer.state, TransferState::Pending);
    assert_eq!(transfer.from_node_id, seeded.node_a.id);
    assert_eq!(transfer.to_node_id, seeded.node_b.id);

    let job = bed.wait_for_job(job.id, JobState::Succeeded).await;
    assert!(job.last_error.is_none());
    assert!(!job.cleanup_pending);

    // The transfer row completed.
    let row = bed.transfer_row(transfer.id).await;
    assert_eq!(row.state, TransferState::Succeeded);

    // The instance is rebound: destination node + landing
    // allocation.
    let instance = bed.instance_row(seeded.instance.id).await;
    assert_eq!(instance.node_id, seeded.node_b.id);
    assert_eq!(instance.allocation_id, seeded.alloc_b.id);

    // The source allocation was released; the landing one claimed.
    let freed = bed.allocation_row(seeded.alloc_a.id).await;
    assert_eq!(freed.assigned_to, None);
    let claimed = bed.allocation_row(seeded.alloc_b.id).await;
    assert_eq!(claimed.assigned_to, Some(seeded.instance.id));

    // The data moved with it (the live directory is keyed by the
    // instance's uuid_short, not its node).
    let live = instance_data_dir(&bed.data_root, &instance.uuid_short);
    assert_eq!(
        std::fs::read_to_string(live.join("server.properties")).unwrap(),
        "motd=transferred\n"
    );
    assert_eq!(
        std::fs::read_to_string(live.join("world/level.dat")).unwrap(),
        "0123456789"
    );

    // The transfer tree is gone (SourceCleanup + prune).
    assert!(!bed
        .data_root
        .join("transfers")
        .join(&instance.uuid_short)
        .exists());

    // Status reads the terminal row.
    let status = bed.transfers.status(seeded.instance.id).await.unwrap();
    assert_eq!(status.map(|row| row.state), Some(TransferState::Succeeded));
}

#[cfg(unix)]
#[tokio::test]
async fn failed_transfer_restores_the_source_running_state() {
    // Roadmap M8 acceptance criterion: a partial failure leaves the
    // job Failed *without affecting the source instance's running
    // state*. Fault injection: the per-instance transfers parent is
    // read-only, so the archive step fails with EACCES *after* the
    // graceful stop was confirmed — the failure path must restore
    // the pre-transfer power state.
    let bed = Testbed::new().await;
    let seeded = bed
        .seed_topology(&[("server.properties", "motd=keep\n")])
        .await;

    // Bring the workload up.
    bed.supervisor
        .request_power(&seeded.instance.id, PowerAction::Start)
        .await
        .expect("start must be accepted");
    bed.wait_for_observed_running(seeded.instance.id).await;

    // The fault: nothing can be created under the instance's
    // transfers directory.
    let transfers_parent = bed
        .data_root
        .join("transfers")
        .join(&seeded.instance.uuid_short);
    std::fs::create_dir_all(&transfers_parent).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&transfers_parent).unwrap().permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&transfers_parent, perms).unwrap();

    let (job, transfer) = bed
        .transfers
        .initiate(seeded.instance.id, seeded.node_b.id, seeded.alloc_b.id)
        .await
        .expect("initiation must be accepted");
    let job = bed.wait_for_job(job.id, JobState::Failed).await;
    assert!(
        job.last_error.as_deref().unwrap().contains("archiving"),
        "the failure must point at the archive step: {:?}",
        job.last_error
    );

    // The transfer row is Failed.
    let row = bed.transfer_row(transfer.id).await;
    assert_eq!(row.state, TransferState::Failed);

    // The source instance is untouched: same node, same allocation,
    // both still assigned to it.
    let instance = bed.instance_row(seeded.instance.id).await;
    assert_eq!(instance.node_id, seeded.node_a.id);
    assert_eq!(instance.allocation_id, seeded.alloc_a.id);
    let still_assigned = bed.allocation_row(seeded.alloc_a.id).await;
    assert_eq!(still_assigned.assigned_to, Some(seeded.instance.id));
    let untouched = bed.allocation_row(seeded.alloc_b.id).await;
    assert_eq!(untouched.assigned_to, None);

    // The data survived byte-for-byte.
    let live = instance_data_dir(&bed.data_root, &instance.uuid_short);
    assert_eq!(
        std::fs::read_to_string(live.join("server.properties")).unwrap(),
        "motd=keep\n"
    );

    // The source workload is running again (the restore path).
    bed.wait_for_observed_running(seeded.instance.id).await;

    // Clear the fault deterministically before the retry: removing
    // the read-only parent needs write on `transfers/` (owned by
    // this test, 0o755), and a missing parent (the cleaner may have
    // pruned it) is already clear.
    let _ = std::fs::remove_dir_all(&transfers_parent);

    // A later transfer is not blocked by the failed one — and it
    // completes.
    let (retry, _row) = bed
        .transfers
        .initiate(seeded.instance.id, seeded.node_b.id, seeded.alloc_b.id)
        .await
        .expect("the failed transfer must not block the next one");
    let _ = bed.wait_for_job(retry.id, JobState::Succeeded).await;
    let instance = bed.instance_row(seeded.instance.id).await;
    assert_eq!(instance.node_id, seeded.node_b.id);
}

// ── Validation ────────────────────────────────────────────────────

#[tokio::test]
async fn initiation_validates_the_move() {
    let bed = Testbed::new().await;
    let seeded = bed.seed_topology(&[]).await;

    // The destination must differ from the current node.
    let err = bed
        .transfers
        .initiate(seeded.instance.id, seeded.node_a.id, seeded.alloc_a.id)
        .await
        .unwrap_err();
    assert!(matches!(err, TransferError::SameNode(_, _)));

    // The destination node must exist.
    let err = bed
        .transfers
        .initiate(seeded.instance.id, NodeId::new(), seeded.alloc_b.id)
        .await
        .unwrap_err();
    assert!(matches!(err, TransferError::NodeNotFound(_)));

    // The landing allocation must be on the destination node.
    let err = bed
        .transfers
        .initiate(seeded.instance.id, seeded.node_b.id, seeded.alloc_a.id)
        .await
        .unwrap_err();
    assert!(matches!(err, TransferError::AllocationNotAvailable(_)));

    // The landing allocation must be free: claim it for a real
    // rival instance (the FK on `assigned_to` demands one).
    let (rival_uuid, rival_short) = Instance::legacy_uuid_pair();
    let rival = Instance {
        id: InstanceId::new(),
        external_id: None,
        uuid: rival_uuid,
        uuid_short: rival_short,
        node_id: seeded.node_b.id,
        name: "Rival".to_owned(),
        description: String::new(),
        status: InstanceStatus::Offline,
        skip_egg_scripts: false,
        owner_id: seeded.instance.owner_id,
        limits: ResourceLimits {
            memory: Bytes::from_mib(256),
            swap: Bytes::from_bytes(0),
            disk: Bytes::from_gib(1),
            io_weight: 500,
            cpu_millicores: 500,
            threads: None,
            oom_disabled: false,
        },
        allocation_id: seeded.alloc_b.id,
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
    let mut claimed_b = seeded.alloc_b.clone();
    claimed_b.assigned_to = Some(rival.id);
    let p = Arc::clone(&bed.persistence);
    let rival_row = rival.clone();
    tokio::task::spawn_blocking(move || {
        p.put_instance(&rival_row).unwrap();
        p.put_allocation(&claimed_b).unwrap();
    })
    .await
    .unwrap();
    let err = bed
        .transfers
        .initiate(seeded.instance.id, seeded.node_b.id, seeded.alloc_b.id)
        .await
        .unwrap_err();
    assert!(matches!(err, TransferError::AllocationNotAvailable(_)));

    // Free it again for the rest of the test.
    let mut freed_b = seeded.alloc_b.clone();
    freed_b.assigned_to = None;
    let p = Arc::clone(&bed.persistence);
    tokio::task::spawn_blocking(move || p.put_allocation(&freed_b).unwrap())
        .await
        .unwrap();

    // One transfer at a time: a live transfer row blocks initiation.
    // The seeded job stays InProgress (never claimable — the queue
    // scans Pending only), so the row is genuinely in flight and no
    // handler interferes with the test.
    let now = chrono::Utc::now();
    let blocking_job = Job {
        id: JobId::new(),
        kind: JobKind::Transfer,
        instance_id: seeded.instance.id,
        state: JobState::InProgress,
        created_at: now,
        updated_at: now,
        started_at: Some(now),
        completed_at: None,
        last_error: None,
        cleanup_pending: false,
        generation: 0,
        payload: None,
    };
    let blocking_transfer = Transfer {
        id: TransferId::new(),
        instance_id: seeded.instance.id,
        job_id: blocking_job.id,
        from_node_id: seeded.node_a.id,
        to_node_id: seeded.node_b.id,
        state: TransferState::ArchiveTransferring,
        archive_path: None,
    };
    let p = Arc::clone(&bed.persistence);
    let (job_row, transfer_row) = (blocking_job.clone(), blocking_transfer.clone());
    tokio::task::spawn_blocking(move || {
        p.put_job(&job_row).unwrap();
        p.put_transfer(&transfer_row).unwrap();
    })
    .await
    .unwrap();
    let err = bed
        .transfers
        .initiate(seeded.instance.id, seeded.node_b.id, seeded.alloc_b.id)
        .await
        .unwrap_err();
    assert!(matches!(err, TransferError::TransferInFlight(_)));

    // An orphaned row (its job already ended without the handler
    // walking it) must NOT block: the job is the arbiter.
    let mut ended = blocking_job.clone();
    ended.state = JobState::Cancelled;
    ended.completed_at = Some(chrono::Utc::now());
    let p = Arc::clone(&bed.persistence);
    tokio::task::spawn_blocking(move || p.put_job(&ended).unwrap())
        .await
        .unwrap();
    assert!(bed
        .transfers
        .in_flight(seeded.instance.id)
        .await
        .unwrap()
        .is_none());

    // The orphaned row is reported with the job's terminal state.
    let status = bed.transfers.status(seeded.instance.id).await.unwrap();
    assert_eq!(status.map(|row| row.state), Some(TransferState::Cancelled));

    // And the next initiation goes through — the full recovery.
    let (job, _transfer) = bed
        .transfers
        .initiate(seeded.instance.id, seeded.node_b.id, seeded.alloc_b.id)
        .await
        .expect("the orphaned row must not block the next transfer");
    bed.wait_for_job(job.id, JobState::Succeeded).await;
    let instance = bed.instance_row(seeded.instance.id).await;
    assert_eq!(instance.node_id, seeded.node_b.id);
}

// ── Cancellation lifecycle ────────────────────────────────────────

/// A job handler that waits for cancellation (occupies the
/// executor's per-instance serialization slot for its instance).
struct Stalled;

#[async_trait]
impl JobHandler for Stalled {
    async fn run(&self, ctx: JobContext) -> JobOutcome {
        while !ctx.is_cancelled() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        JobOutcome::Cancelled
    }

    async fn cleanup(&self, _job: &Job) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn cancelling_a_pending_transfer_releases_the_instance() {
    // A transfer cancelled straight out of `Pending` never runs its
    // handler, so the service must walk the row itself — and the
    // instance must remain transferable afterwards.
    let bed = Testbed::new().await;
    // The blocker: a stalled Backup job for the same instance keeps
    // the transfer job Pending (per-instance serialization).
    bed.jobs
        .register_handler(JobKind::Backup, Arc::new(Stalled))
        .await;
    let seeded = bed.seed_topology(&[("f.txt", "x")]).await;

    let now = chrono::Utc::now();
    let blocker = Job {
        id: JobId::new(),
        kind: JobKind::Backup,
        instance_id: seeded.instance.id,
        state: JobState::Pending,
        created_at: now,
        updated_at: now,
        started_at: None,
        completed_at: None,
        last_error: None,
        cleanup_pending: false,
        generation: 0,
        payload: None,
    };
    let blocker = bed.jobs.enqueue(blocker).await.unwrap();
    bed.wait_for_job(blocker.id, JobState::InProgress).await;

    // The transfer waits behind the blocker.
    let (job, transfer) = bed
        .transfers
        .initiate(seeded.instance.id, seeded.node_b.id, seeded.alloc_b.id)
        .await
        .expect("initiation must be accepted");
    let row = bed.transfer_row(transfer.id).await;
    assert_eq!(row.state, TransferState::Pending);

    // Cancel through the service.
    let cancelled_job = bed
        .transfers
        .cancel(seeded.instance.id)
        .await
        .expect("cancellation must be accepted");
    assert_eq!(cancelled_job.state, JobState::Cancelled);
    assert_eq!(cancelled_job.id, job.id);

    // The row was walked to Cancelled (no handler ran).
    let row = bed.transfer_row(transfer.id).await;
    assert_eq!(row.state, TransferState::Cancelled);

    // No transfer is in flight anymore.
    assert!(bed
        .transfers
        .in_flight(seeded.instance.id)
        .await
        .unwrap()
        .is_none());

    // Cancelling again: nothing in flight.
    let err = bed.transfers.cancel(seeded.instance.id).await.unwrap_err();
    assert!(matches!(err, TransferError::NoTransferInFlight(_)));

    // The instance is transferable again, and the retry completes.
    let (retry, _row) = bed
        .transfers
        .initiate(seeded.instance.id, seeded.node_b.id, seeded.alloc_b.id)
        .await
        .expect("the cancelled transfer must not block the next one");

    // Release the blocker and let the retry run to completion.
    bed.jobs.cancel(blocker.id).await.unwrap();
    bed.wait_for_job(blocker.id, JobState::Cancelled).await;
    bed.wait_for_job(retry.id, JobState::Succeeded).await;

    let instance = bed.instance_row(seeded.instance.id).await;
    assert_eq!(instance.node_id, seeded.node_b.id);
    assert_eq!(instance.allocation_id, seeded.alloc_b.id);
}

// ── Crash reconciliation ──────────────────────────────────────────

#[tokio::test]
async fn crashed_transfer_tree_is_reconciled_and_cleaned() {
    // Roadmap M8 fault-injection: the engine died mid-archive. The
    // job row says InProgress, the transfer row says
    // SourceArchiving, and a partial archive sits in the transfer
    // tree. Boot-time reconciliation must move the job to
    // CleanupPending; the cleaner must remove the tree; the
    // instance must remain transferable.
    let bed = Testbed::new().await;
    let seeded = bed
        .seed_topology(&[("server.properties", "motd=crash\n")])
        .await;

    // The interrupted transfer's rows.
    let transfer_id = TransferId::new();
    let now = chrono::Utc::now();
    let job = Job {
        id: JobId::new(),
        kind: JobKind::Transfer,
        instance_id: seeded.instance.id,
        state: JobState::InProgress,
        created_at: now,
        updated_at: now,
        started_at: Some(now),
        completed_at: None,
        last_error: None,
        cleanup_pending: false,
        generation: 0,
        payload: Some(
            serde_json::json!({
                "transfer_id": transfer_id.to_string(),
                "allocation_id": seeded.alloc_b.id.to_string(),
            })
            .to_string(),
        ),
    };
    let transfer = Transfer {
        id: transfer_id,
        instance_id: seeded.instance.id,
        job_id: job.id,
        from_node_id: seeded.node_a.id,
        to_node_id: seeded.node_b.id,
        state: TransferState::SourceArchiving,
        archive_path: None,
    };
    let p = Arc::clone(&bed.persistence);
    let (job_row, transfer_row) = (job.clone(), transfer.clone());
    tokio::task::spawn_blocking(move || {
        p.put_job(&job_row).unwrap();
        p.put_transfer(&transfer_row).unwrap();
    })
    .await
    .unwrap();

    // The partial on-disk state: a half-written archive in the
    // transfer tree's source half.
    let tree = bed
        .data_root
        .join("transfers")
        .join(&seeded.instance.uuid_short)
        .join(transfer_id.to_string());
    std::fs::create_dir_all(tree.join("source")).unwrap();
    std::fs::write(tree.join("source/archive.tar.gz.part"), b"partial bytes").unwrap();

    // Boot-time reconciliation.
    let orphaned = bed.jobs.reconcile_orphans().await.unwrap();
    assert_eq!(orphaned, 1);

    // The cleaner finishes the story: job Failed, tree gone.
    let job = bed.wait_for_job(job.id, JobState::Failed).await;
    assert!(
        job.last_error.as_deref().unwrap().contains("orphaned"),
        "the failure must say the job was orphaned: {:?}",
        job.last_error
    );
    assert!(!tree.exists());

    // The instance is untouched by the crashed move.
    let instance = bed.instance_row(seeded.instance.id).await;
    assert_eq!(instance.node_id, seeded.node_a.id);
    assert_eq!(instance.allocation_id, seeded.alloc_a.id);

    // The stale row does not block the next transfer, and status
    // reports the job's verdict.
    assert!(bed
        .transfers
        .in_flight(seeded.instance.id)
        .await
        .unwrap()
        .is_none());
    let status = bed.transfers.status(seeded.instance.id).await.unwrap();
    assert_eq!(status.map(|row| row.state), Some(TransferState::Failed));

    // The live data survived.
    let live = instance_data_dir(&bed.data_root, &instance.uuid_short);
    assert_eq!(
        std::fs::read_to_string(live.join("server.properties")).unwrap(),
        "motd=crash\n"
    );
}
