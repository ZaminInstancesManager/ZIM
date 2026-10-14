//! Install-system tests: the service's validation and in-flight
//! guard, and the roadmap-M8.3 install acceptance criteria
//! end-to-end through the durable-job executor over the mock
//! install runner:
//!
//! 1. An install job runs the egg's script in the constrained
//!    container (the spec the mock received: image, env, data-dir
//!    bind) and stamps `installed_at` on success — with a running
//!    workload stopped first (through the power queue) and *not*
//!    restarted afterwards (Pterodactyl reinstall semantics).
//! 2. A failed script fails the job with the exit code and output
//!    tail in `last_error`, `cleanup_pending = false` (partial
//!    data is retry state), and no `installed_at` stamp.
//! 3. Cancellation (a stalled script) kills the run at the
//!    cooperative hook and answers `Cancelled`.
//! 4. `skip_egg_scripts` (and an egg with no script) succeeds
//!    without ever invoking the runner.
//! 5. The in-flight guard rejects a second install while one runs.
//! 6. Update jobs select the egg's update script, falling back to
//!    the install script (egg-change reinstall parity).
//! 7. The null runner (degraded, API-only engine) fails fast with
//!    a structured message.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::testing::{
    MockInstallRun, MockInstallRunner, MockRuntime, MockRuntimeFactory,
};
use zim_engine_application::supervision::{
    InstallRunner, NullInstallRunner, ObservedState, PowerAction, SupervisionPolicy, Supervisor,
};
use zim_engine_domain::job::{Job, JobKind};
use zim_engine_domain::status::JobState;
use zim_engine_domain::{
    Allocation, AllocationId, Bytes, CommandTemplate, Egg, EggId, EggImage, EggScript, EggScripts,
    EggVariable, Instance, InstanceId, InstanceStatus, JobId, LocationId, Node, NodeCapabilities,
    NodeDeployment, NodeId, NodeState, ResourceLimits, Scheme, User, UserId,
};
use zim_engine_installs::{InstallError, InstallOptions, InstallService};
use zim_engine_jobs::{JobPolicy, JobSystem};

/// A unique scratch directory per call (the transfers tests' shape).
fn scratch(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "zim-installs-test-{}-{n}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The test harness: in-memory store, a supervisor over the shared
/// mock runtime, a fast executor, and the real install service over
/// a scriptable mock install runner.
struct Testbed {
    persistence: Arc<dyn Persistence>,
    supervisor: Arc<Supervisor>,
    jobs: Arc<JobSystem>,
    installs: Arc<InstallService>,
    runner: MockInstallRunner,
    data_root: PathBuf,
}

impl Testbed {
    async fn new(script: Vec<MockInstallRun>) -> Self {
        let runner = MockInstallRunner::scripted(script);
        Self::assemble(Arc::new(runner.clone()), runner).await
    }

    /// [`Testbed::new`] with an explicit runner (the null-runner
    /// test binds the degraded engine's adapter; its `runner`
    /// recording surface is unused there).
    async fn new_with_runner(runner: Arc<dyn InstallRunner>) -> Self {
        Self::assemble(runner, MockInstallRunner::always_succeeds()).await
    }

    async fn assemble(runner: Arc<dyn InstallRunner>, mock: MockInstallRunner) -> Self {
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
        let installs = InstallService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            Arc::clone(&supervisor),
            runner,
            data_root.clone(),
        );
        installs.register().await;
        jobs.start();
        Self {
            persistence,
            supervisor,
            jobs,
            installs,
            runner: mock,
            data_root,
        }
    }

    /// Seed the topology: a local node, a claimed allocation, an
    /// owner, an egg with both install + update scripts and two
    /// variables, and an instance on that egg. Returns the seeded
    /// rows.
    async fn seed(&self) -> Seeded {
        self.seed_with_egg(build_egg(true, true)).await
    }

    /// [`Testbed::seed`] with a custom egg.
    async fn seed_with_egg(&self, egg: Egg) -> Seeded {
        let node = Node {
            id: NodeId::new(),
            name: "local-node".to_owned(),
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
            name: "Installs Test".to_owned(),
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
            egg_id: egg.id,
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
        let rows = (node, user, allocation, instance.clone(), egg.clone());
        tokio::task::spawn_blocking(move || {
            let (node, user, allocation, instance, egg) = rows;
            persistence.put_node(&node).unwrap();
            persistence.put_user(&user).unwrap();
            persistence.put_allocation(&allocation).unwrap();
            persistence.put_egg(&egg).unwrap();
            persistence.put_instance(&instance).unwrap();
            let mut claimed = allocation;
            claimed.assigned_to = Some(instance.id);
            persistence.put_allocation(&claimed).unwrap();
        })
        .await
        .unwrap();

        self.supervisor.load_local().await.unwrap();

        Seeded { instance }
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

    /// Read the instance row (panics when missing).
    async fn instance_row(&self, id: InstanceId) -> Instance {
        let p = Arc::clone(&self.persistence);
        tokio::task::spawn_blocking(move || p.get_instance(&id))
            .await
            .unwrap()
            .unwrap()
            .expect("instance row must exist")
    }
}

/// What [`Testbed::seed`] seeded.
struct Seeded {
    instance: Instance,
}

/// An egg with optional install/update scripts and two variables
/// (the env-passing assertion surface). The legacy UUID is unique
/// per call (two eggs in one store must not collide on the import
/// dedup key).
fn build_egg(with_install: bool, with_update: bool) -> Egg {
    let now = chrono::Utc::now();
    let (legacy_uuid, _) = Instance::legacy_uuid_pair();
    Egg {
        id: EggId::new(),
        legacy_uuid: Some(legacy_uuid),
        name: "Paper".to_owned(),
        description: "PaperMC".to_owned(),
        author: "test".to_owned(),
        images: vec![EggImage {
            label: "Java 21".to_owned(),
            image: "ghcr.io/parkervcp/yolks:java_21".parse().unwrap(),
        }],
        startup: CommandTemplate::new("java -jar {{SERVER_JAR}}"),
        features: Default::default(),
        scripts: EggScripts {
            install: with_install.then(|| EggScript {
                container: "ghcr.io/parkervcp/installers:alpine".parse().unwrap(),
                entrypoint: "bash".to_owned(),
                script: "cd /home/container && echo installing".to_owned(),
            }),
            update: with_update.then(|| EggScript {
                container: "ghcr.io/parkervcp/installers:debian".parse().unwrap(),
                entrypoint: "bash".to_owned(),
                script: "cd /home/container && echo updating".to_owned(),
            }),
        },
        config: Default::default(),
        variables: vec![
            EggVariable {
                id: zim_engine_domain::EggVariableId::new(),
                env_name: "SERVER_JAR".to_owned(),
                name: "Server jar".to_owned(),
                description: String::new(),
                default_value: "server.jar".to_owned(),
                user_viewable: true,
                user_editable: true,
                rules: vec!["required".to_owned()],
            },
            EggVariable {
                id: zim_engine_domain::EggVariableId::new(),
                env_name: "MINECRAFT_VERSION".to_owned(),
                name: "Version".to_owned(),
                description: String::new(),
                default_value: "1.20.4".to_owned(),
                user_viewable: true,
                user_editable: true,
                rules: vec![],
            },
        ],
        created_at: now,
        updated_at: now,
    }
}

// ── Acceptance: the install pipeline ──────────────────────────────

#[tokio::test]
async fn install_runs_the_script_and_stamps_installed_at() {
    // Roadmap M8.3 acceptance: the install job pulls the egg's
    // installer image, runs the script in the constrained container
    // bound to the instance's data directory, and marks
    // installed_at on success. The workload was running: it was
    // stopped through the power queue first — and stays stopped
    // (Pterodactyl reinstall semantics).
    let bed = Testbed::new(vec![MockInstallRun::Run {
        run_for: Duration::ZERO,
        exit_code: 0,
        output: "installing\n".to_owned(),
    }])
    .await;
    let seeded = bed.seed().await;

    // Bring the workload up: the install must stop it.
    bed.supervisor
        .request_power(&seeded.instance.id, PowerAction::Start)
        .await
        .expect("start must be accepted");
    bed.wait_for_observed_running(seeded.instance.id).await;

    let job = bed
        .installs
        .initiate(seeded.instance.id, InstallOptions::default())
        .await
        .expect("initiation must be accepted");
    assert_eq!(job.kind, JobKind::Install);

    let job = bed.wait_for_job(job.id, JobState::Succeeded).await;
    assert!(job.last_error.is_none());
    assert!(!job.cleanup_pending);

    // The script ran exactly once, in the egg's installer image,
    // with the instance env + the egg's variables, bound to the
    // instance's data directory.
    assert_eq!(bed.runner.ran(), 1);
    let specs = bed.runner.specs();
    assert_eq!(specs.len(), 1);
    let spec = &specs[0];
    assert_eq!(spec.image, "ghcr.io/parkervcp/installers:alpine");
    assert_eq!(spec.entrypoint, "bash");
    assert_eq!(spec.script, "cd /home/container && echo installing");
    assert_eq!(
        spec.data_dir,
        bed.data_root
            .join("instances")
            .join(&seeded.instance.uuid_short)
    );
    assert_eq!(spec.container_data_dir, PathBuf::from("/home/container"));
    assert!(spec.container_name.contains("_installer_"));
    assert!(spec
        .env
        .iter()
        .any(|(k, v)| k == "SERVER_MEMORY" && v == "512"));
    assert!(spec
        .env
        .iter()
        .any(|(k, v)| k == "SERVER_PORT" && v == "41000"));
    assert!(spec
        .env
        .iter()
        .any(|(k, v)| k == "SERVER_IP" && v == "127.0.0.1"));
    assert!(spec
        .env
        .iter()
        .any(|(k, v)| k == "SERVER_JAR" && v == "server.jar"));
    assert!(spec
        .env
        .iter()
        .any(|(k, v)| k == "MINECRAFT_VERSION" && v == "1.20.4"));

    // The stamp.
    let instance = bed.instance_row(seeded.instance.id).await;
    assert!(instance.installed_at.is_some());

    // Pterodactyl reinstall semantics: the workload stays stopped.
    let snapshot = bed
        .supervisor
        .observed(&seeded.instance.id)
        .await
        .expect("supervisor must answer");
    assert!(matches!(snapshot.observed, ObservedState::Offline { .. }));
}

#[tokio::test]
async fn failed_script_fails_the_job_with_the_tail_and_no_stamp() {
    // Roadmap M8.3 acceptance: a failing script fails the job with
    // the exit code + output tail; partial data is retry state
    // (cleanup_pending = false); installed_at is not stamped.
    let bed = Testbed::new(vec![MockInstallRun::Run {
        run_for: Duration::ZERO,
        exit_code: 1,
        output: "curl: (6) could not resolve host\n".to_owned(),
    }])
    .await;
    let seeded = bed.seed().await;

    let job = bed
        .installs
        .initiate(seeded.instance.id, InstallOptions::default())
        .await
        .expect("initiation must be accepted");
    let job = bed.wait_for_job(job.id, JobState::Failed).await;

    let message = job.last_error.expect("failure must carry a message");
    assert!(
        message.contains("exited with status 1"),
        "message: {message}"
    );
    assert!(
        message.contains("could not resolve host"),
        "message: {message}"
    );
    assert!(!job.cleanup_pending);

    // No stamp; the script still ran (over retryable partial data).
    assert_eq!(bed.runner.ran(), 1);
    let instance = bed.instance_row(seeded.instance.id).await;
    assert!(instance.installed_at.is_none());
}

#[tokio::test]
async fn cancelled_install_answers_cancelled() {
    // Roadmap M8.3 acceptance: cancellation kills the run at the
    // cooperative hook (the stalled script) and answers Cancelled.
    let bed = Testbed::new(vec![MockInstallRun::Stall]).await;
    let seeded = bed.seed().await;

    let job = bed
        .installs
        .initiate(seeded.instance.id, InstallOptions::default())
        .await
        .expect("initiation must be accepted");

    // Wait until the script is provably running (the spec was
    // recorded), then cancel through the executor.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while bed.runner.specs().is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "script never started"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    bed.jobs
        .cancel(job.id)
        .await
        .expect("cancel must be accepted");
    let job = bed.wait_for_job(job.id, JobState::Cancelled).await;
    assert!(job.last_error.is_none());

    // The run never completed (only the hook ended it).
    assert_eq!(bed.runner.ran(), 0);
    // No stamp.
    let instance = bed.instance_row(seeded.instance.id).await;
    assert!(instance.installed_at.is_none());
}

#[tokio::test]
async fn skip_egg_scripts_succeeds_without_running_the_script() {
    // The "pre-installed data" hatch: the row's flag (or the
    // request's override) skips the script entirely; the job is the
    // stamp.
    let bed = Testbed::new(vec![]).await;
    let seeded = bed.seed().await;

    let job = bed
        .installs
        .initiate(
            seeded.instance.id,
            InstallOptions {
                skip_egg_scripts: Some(true),
            },
        )
        .await
        .expect("initiation must be accepted");
    let job = bed.wait_for_job(job.id, JobState::Succeeded).await;
    assert!(job.last_error.is_none());

    assert!(bed.runner.specs().is_empty(), "no script may run");
    let instance = bed.instance_row(seeded.instance.id).await;
    assert!(instance.installed_at.is_some());
}

#[tokio::test]
async fn egg_without_a_script_stamps_only() {
    // An egg with no install script: the whole job is the stamp
    // (nothing to run, nothing to fail).
    let bed = Testbed::new(vec![]).await;
    let seeded = bed.seed_with_egg(build_egg(false, false)).await;

    let job = bed
        .installs
        .initiate(seeded.instance.id, InstallOptions::default())
        .await
        .expect("initiation must be accepted");
    bed.wait_for_job(job.id, JobState::Succeeded).await;

    assert!(bed.runner.specs().is_empty());
    let instance = bed.instance_row(seeded.instance.id).await;
    assert!(instance.installed_at.is_some());
}

#[tokio::test]
async fn in_flight_guard_rejects_a_second_install() {
    // One install at a time per instance: a second initiation
    // while the first is stalled answers InstallInFlight.
    let bed = Testbed::new(vec![MockInstallRun::Stall]).await;
    let seeded = bed.seed().await;

    let first = bed
        .installs
        .initiate(seeded.instance.id, InstallOptions::default())
        .await
        .expect("the first install must be accepted");
    let second = bed
        .installs
        .initiate(seeded.instance.id, InstallOptions::default())
        .await;
    assert!(matches!(second, Err(InstallError::InstallInFlight(_))));

    // End the stall so the executor can drain at test end.
    bed.jobs
        .cancel(first.id)
        .await
        .expect("cancel must be accepted");
    bed.wait_for_job(first.id, JobState::Cancelled).await;
}

#[tokio::test]
async fn update_jobs_run_the_update_script_with_install_fallback() {
    // JobKind::Update: the egg's update script when present; a
    // later job on an update-less egg falls back to the install
    // script (egg-change reinstall parity).
    let bed = Testbed::new(vec![
        MockInstallRun::Run {
            run_for: Duration::ZERO,
            exit_code: 0,
            output: String::new(),
        },
        MockInstallRun::Run {
            run_for: Duration::ZERO,
            exit_code: 0,
            output: String::new(),
        },
    ])
    .await;
    let seeded = bed.seed().await;

    // An update job, created directly (the egg-change flow that
    // will enqueue these arrives with the frontend surface).
    let job = Job {
        id: JobId::new(),
        kind: JobKind::Update,
        instance_id: seeded.instance.id,
        state: zim_engine_domain::status::JobState::Pending,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        last_error: None,
        cleanup_pending: false,
        generation: 0,
        payload: Some(r#"{"skip_egg_scripts": false}"#.to_owned()),
    };
    let p = Arc::clone(&bed.persistence);
    let row = job.clone();
    tokio::task::spawn_blocking(move || p.put_job(&row))
        .await
        .unwrap()
        .unwrap();
    let job = bed.jobs.enqueue(job).await.expect("enqueue must accept");
    bed.wait_for_job(job.id, JobState::Succeeded).await;

    // The update script ran (the debian installer image).
    let specs = bed.runner.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].image, "ghcr.io/parkervcp/installers:debian");

    // The fallback: an egg with only an install script.
    let egg = build_egg(true, false);
    let mut instance = bed.instance_row(seeded.instance.id).await;
    instance.egg_id = egg.id;
    let p = Arc::clone(&bed.persistence);
    let row = instance.clone();
    tokio::task::spawn_blocking(move || p.put_egg(&egg).unwrap())
        .await
        .unwrap();
    let p = Arc::clone(&bed.persistence);
    tokio::task::spawn_blocking(move || p.put_instance(&row))
        .await
        .unwrap()
        .unwrap();

    let job = Job {
        id: JobId::new(),
        kind: JobKind::Update,
        instance_id: instance.id,
        state: zim_engine_domain::status::JobState::Pending,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        last_error: None,
        cleanup_pending: false,
        generation: 0,
        payload: Some(r#"{"skip_egg_scripts": false}"#.to_owned()),
    };
    let job = bed.jobs.enqueue(job).await.expect("enqueue must accept");
    bed.wait_for_job(job.id, JobState::Succeeded).await;

    let specs = bed.runner.specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[1].image, "ghcr.io/parkervcp/installers:alpine");
}

#[tokio::test]
async fn null_runner_fails_fast_with_a_structured_message() {
    // The degraded, API-only engine: install jobs fail fast with
    // the structured unavailability instead of hanging.
    let bed = Testbed::new_with_runner(Arc::new(NullInstallRunner)).await;
    let seeded = bed.seed().await;

    let job = bed
        .installs
        .initiate(seeded.instance.id, InstallOptions::default())
        .await
        .expect("initiation must be accepted");

    let job = bed.wait_for_job(job.id, JobState::Failed).await;
    let message = job.last_error.expect("failure must carry a message");
    assert!(
        message.contains("could not run") || message.contains("unavailable"),
        "message: {message}"
    );

    // No stamp.
    let instance = bed.instance_row(seeded.instance.id).await;
    assert!(instance.installed_at.is_none());
}

// ── Service validation ────────────────────────────────────────────

#[tokio::test]
async fn initiate_rejects_unknown_instances_and_dangling_eggs() {
    let bed = Testbed::new(vec![]).await;

    let missing = bed
        .installs
        .initiate(InstanceId::new(), InstallOptions::default())
        .await;
    assert!(matches!(missing, Err(InstallError::InstanceNotFound(_))));

    // An instance whose egg row is gone (the delete guard's
    // escape): EggNotFound, not a panic.
    let seeded = bed.seed_with_egg(build_egg(false, false)).await;
    let mut instance = seeded.instance.clone();
    instance.egg_id = EggId::new();
    let p = Arc::clone(&bed.persistence);
    let row = instance.clone();
    tokio::task::spawn_blocking(move || p.put_instance(&row))
        .await
        .unwrap()
        .unwrap();
    let dangling = bed
        .installs
        .initiate(instance.id, InstallOptions::default())
        .await;
    assert!(matches!(dangling, Err(InstallError::EggNotFound(..))));
}

#[tokio::test]
async fn instances_without_an_allocation_install_with_placeholder_env() {
    // An allocation-less instance (created without one) still
    // installs: the env keeps the legacy placeholders.
    let bed = Testbed::new(vec![MockInstallRun::Run {
        run_for: Duration::ZERO,
        exit_code: 0,
        output: String::new(),
    }])
    .await;
    let seeded = bed.seed().await;

    // Detach the allocation.
    let p = Arc::clone(&bed.persistence);
    tokio::task::spawn_blocking(move || {
        let mut alloc = p
            .get_allocation(&AllocationId::new(41000))
            .unwrap()
            .unwrap();
        alloc.assigned_to = None;
        p.put_allocation(&alloc).unwrap();
    })
    .await
    .unwrap();

    let job = bed
        .installs
        .initiate(seeded.instance.id, InstallOptions::default())
        .await
        .expect("initiation must be accepted");
    bed.wait_for_job(job.id, JobState::Succeeded).await;

    let specs = bed.runner.specs();
    assert_eq!(specs.len(), 1);
    assert!(specs[0]
        .env
        .iter()
        .any(|(k, v)| k == "SERVER_IP" && v == "127.0.0.1"));
    assert!(specs[0]
        .env
        .iter()
        .any(|(k, v)| k == "SERVER_PORT" && v == "0"));
}

#[tokio::test]
async fn instance_variable_overrides_win_over_the_egg_defaults() {
    // M10: the per-instance variable overrides (the migration
    // importer's server_variables) layer over the egg's defaults in
    // the install env — the override's value wins, and an orphaned
    // override (a name the egg no longer declares) must not leak
    // into the environment.
    let bed = Testbed::new(vec![MockInstallRun::Run {
        run_for: Duration::ZERO,
        exit_code: 0,
        output: "installing\n".to_owned(),
    }])
    .await;
    let seeded = bed.seed().await;

    // The panel-import shape: SERVER_JAR overridden, plus one
    // orphaned name the egg does not declare.
    bed.persistence
        .put_instance_variables(
            &seeded.instance.id,
            &[
                zim_engine_domain::InstanceVariable {
                    env_name: "SERVER_JAR".to_owned(),
                    value: "paper.jar".to_owned(),
                },
                zim_engine_domain::InstanceVariable {
                    env_name: "PANEL_ORPHAN".to_owned(),
                    value: "must-not-leak".to_owned(),
                },
            ],
        )
        .unwrap();

    let job = bed
        .installs
        .initiate(seeded.instance.id, InstallOptions::default())
        .await
        .expect("initiation must be accepted");
    bed.wait_for_job(job.id, JobState::Succeeded).await;

    let specs = bed.runner.specs();
    assert_eq!(specs.len(), 1);
    let env = &specs[0].env;
    // The override replaced the egg's default.
    assert!(env
        .iter()
        .any(|(k, v)| k == "SERVER_JAR" && v == "paper.jar"));
    assert!(!env
        .iter()
        .any(|(k, v)| k == "SERVER_JAR" && v == "server.jar"));
    // The untouched default survives.
    assert!(env
        .iter()
        .any(|(k, v)| k == "MINECRAFT_VERSION" && v == "1.20.4"));
    // The orphaned override did not leak.
    assert!(!env.iter().any(|(k, _)| k == "PANEL_ORPHAN"));
}
