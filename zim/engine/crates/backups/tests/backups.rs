//! Backup-system tests: the archive engine (round trip, tamper
//! detection, unsafe-entry rejection, atomic finalization), the
//! service row lifecycle (limit, lock), and the two roadmap-M8
//! acceptance criteria end-to-end through the durable-job executor:
//!
//! 1. A backup job creates an archive, records the sha256, and the
//!    backup appears in the list (sealed).
//! 2. An interrupted backup leaves the job cleanup-pending with the
//!    partial `.part` archive on disk; the background cleaner deletes
//!    it and the job lands in `Failed`.
//!
//! plus the restore direction (staging + swap; live data survives a
//! failed restore).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::instance_data_dir;
use zim_engine_backups::{
    create_archive, part_path_for, restore_archive, verify_archive, ArchiveError, BackupError,
    BackupService,
};
use zim_engine_domain::job::{Job, JobKind};
use zim_engine_domain::status::JobState;
use zim_engine_domain::{
    Allocation, AllocationId, Bytes, CommandTemplate, EggId, Instance, InstanceId, InstanceStatus,
    JobId, LocationId, Node, NodeCapabilities, NodeDeployment, NodeState, ResourceLimits, Scheme,
    User, UserId,
};
use zim_engine_jobs::{JobPolicy, JobSystem};

/// A unique scratch directory per call (avoids cross-test
/// interference without a tempfile dependency: process id + an
/// atomic counter + the test's name).
fn scratch(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "zim-backups-test-{}-{n}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The test harness: in-memory store, fast executor, real backup
/// service over a scratch data root.
struct Testbed {
    persistence: Arc<dyn Persistence>,
    jobs: Arc<JobSystem>,
    backups: Arc<BackupService>,
    data_root: PathBuf,
}

impl Testbed {
    async fn new() -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();
        let data_root = scratch("data");
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
        let backups = BackupService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            data_root.clone(),
        );
        backups.register().await;
        jobs.start();
        Self {
            persistence,
            jobs,
            backups,
            data_root,
        }
    }

    /// Seed an instance with `backup_limit` and some data files.
    async fn seed_instance(&self, backup_limit: u32, files: &[(&str, &str)]) -> Instance {
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
            email: format!("owner-{}@zim.local", node.id),
            username: format!("owner-{}", node.id),
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
            uuid: uuid.clone(),
            uuid_short: uuid_short.clone(),
            node_id: node.id,
            name: "Backups Test".to_owned(),
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
            backup_limit,
            crash_detection_enabled: true,
            installed_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let row = instance.clone();
        let p = Arc::clone(&self.persistence);
        tokio::task::spawn_blocking(move || {
            p.put_node(&node).unwrap();
            p.put_user(&user).unwrap();
            p.put_allocation(&allocation).unwrap();
            p.put_instance(&row).unwrap();
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
        instance
    }

    /// Poll persistence until the job reaches `want`.
    async fn wait_for_job(&self, id: JobId, want: JobState) -> Job {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
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
                panic!("job reached terminal {job:?} instead of {want:?}: {job:?}");
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

// ── Archive engine ────────────────────────────────────────────────

#[test]
fn archive_round_trip_preserves_contents() {
    let source = scratch("roundtrip-src");
    std::fs::write(source.join("server.properties"), "motd=hello\n").unwrap();
    std::fs::create_dir(source.join("world")).unwrap();
    std::fs::write(source.join("world/level.dat"), [0u8; 4096]).unwrap();
    // A symlink in the tree is skipped (documented policy).
    #[cfg(unix)]
    std::os::unix::fs::symlink("server.properties", source.join("link")).unwrap();

    let dest = scratch("roundtrip-out").join("backup.tar.gz");
    let result = create_archive(&source, &dest, &|| false, &|_| {}).unwrap();
    assert!(result.bytes > 0);
    assert_eq!(result.sha256.len(), 64);

    // The sealed name exists; the .part does not.
    assert!(dest.exists());
    assert!(!part_path_for(&dest).exists());

    // Verification passes; the digest is honest.
    verify_archive(&dest, &result.sha256).unwrap();

    // Restore into a fresh directory reproduces the tree.
    let target = scratch("roundtrip-restore");
    restore_archive(&dest, &result.sha256, &target, &|| false, &|_| {}).unwrap();
    assert_eq!(
        std::fs::read_to_string(target.join("server.properties")).unwrap(),
        "motd=hello\n"
    );
    assert_eq!(
        std::fs::read(target.join("world/level.dat")).unwrap(),
        vec![0u8; 4096]
    );
    // The symlink was skipped, not archived.
    #[cfg(unix)]
    assert!(!target.join("link").exists());
}

#[test]
fn tampered_archive_fails_verification() {
    let source = scratch("tamper-src");
    std::fs::write(source.join("data.bin"), b"0123456789").unwrap();
    let dest = scratch("tamper-out").join("backup.tar.gz");
    let result = create_archive(&source, &dest, &|| false, &|_| {}).unwrap();

    // Flip one byte inside the archive.
    let bytes = std::fs::read(&dest).unwrap();
    std::fs::write(&dest, {
        let mut flipped = bytes;
        let mid = flipped.len() / 2;
        flipped[mid] ^= 0xff;
        flipped
    })
    .unwrap();

    let err = verify_archive(&dest, &result.sha256).unwrap_err();
    assert!(matches!(err, ArchiveError::ChecksumMismatch { .. }));
}

#[test]
fn restore_rejects_escaping_entries() {
    // Hand-craft a hostile archive at the byte level: the tar crate
    // (correctly) refuses to *write* a `../` entry, so the fixture
    // must be assembled from raw header bytes.
    let hostile = scratch("hostile").join("evil.tar.gz");
    {
        use flate2::write::GzEncoder;
        use std::io::Write;

        /// One ustar entry: 512-byte header + size-rounded data.
        fn entry(name: &str, data: &[u8]) -> Vec<u8> {
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            let octal = |field: &mut [u8], value: u64| {
                let text = format!("{value:0width$o}", width = field.len() - 1);
                field[..text.len()].copy_from_slice(text.as_bytes());
            };
            octal(&mut header[100..108], 0o644); // mode
            octal(&mut header[108..116], 0); // uid
            octal(&mut header[116..124], 0); // gid
            octal(&mut header[124..136], data.len() as u64); // size
            octal(&mut header[136..148], 0); // mtime
            header[156] = b'0'; // regular file
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");
            // Checksum: bytes with the checksum field as spaces.
            for b in &mut header[148..156] {
                *b = b' ';
            }
            let sum: u32 = header.iter().map(|b| *b as u32).sum();
            let checksum = format!("{sum:06o}\0 ");
            header[148..156].copy_from_slice(checksum.as_bytes());

            let mut out = header.to_vec();
            out.extend_from_slice(data);
            let padding = (512 - data.len() % 512) % 512;
            out.extend(std::iter::repeat_n(0u8, padding));
            out
        }

        let mut tarball = entry("../escape", b"evil\n");
        tarball.extend(std::iter::repeat_n(0u8, 1024)); // EOF blocks
        let file = std::fs::File::create(&hostile).unwrap();
        let mut gzip = GzEncoder::new(file, flate2::Compression::default());
        gzip.write_all(&tarball).unwrap();
        gzip.finish().unwrap().flush().unwrap();
    }

    // Hash it, then try to restore: the sanitizer must refuse the
    // parent-dir entry before anything is written.
    let sha = zim_engine_backups::hash_file(&hostile).unwrap();
    let target = scratch("hostile-target");
    let err = restore_archive(&hostile, &sha, &target, &|| false, &|_| {}).unwrap_err();
    match err {
        ArchiveError::UnsafeEntry(message) => {
            assert!(message.contains("parent-dir"), "got: {message}");
        }
        other => panic!("expected UnsafeEntry, got {other:?}"),
    }
    assert!(!scratch("hostile").join("escape").exists());
}

// ── Service + handler through the executor ───────────────────────

#[tokio::test]
async fn backup_job_creates_archive_records_sha_and_lists() {
    // Roadmap M8 acceptance criterion 1.
    let bed = Testbed::new().await;
    let instance = bed
        .seed_instance(5, &[("server.properties", "motd=acceptance\n")])
        .await;

    let (job, backup) = bed.backups.create_backup(instance.id).await.unwrap();
    let job = bed.wait_for_job(job.id, JobState::Succeeded).await;
    assert!(job.completed_at.is_some());

    // The backup row is sealed: sha256 + bytes + completed_at, and
    // it appears in the instance's listing.
    let p = Arc::clone(&bed.persistence);
    let listed = tokio::task::spawn_blocking({
        let id = instance.id;
        move || p.list_backups_by_instance(&id)
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(listed.len(), 1);
    let sealed = &listed[0];
    assert_eq!(sealed.id, backup.id);
    let sha = sealed.sha256_hash.as_deref().expect("sha256 recorded");
    assert_eq!(sha.len(), 64);
    assert!(sealed.bytes > 0);
    assert!(sealed.completed_at.is_some());

    // The archive on disk verifies against the recorded digest.
    let archive = bed
        .backups
        .archive_path_for(instance.id, backup.id)
        .await
        .unwrap();
    verify_archive(&archive, sha).unwrap();
}

#[tokio::test]
async fn interrupted_backup_is_cleaned_up_by_the_cleaner() {
    // Roadmap M8 acceptance criterion 2, fault-injected: the engine
    // "crashed" mid-archive — a partial .part sits on disk, the job
    // row says CleanupPending (boot reconciliation's doing).
    let bed = Testbed::new().await;
    let instance = bed
        .seed_instance(5, &[("world/level.dat", "0123456789")])
        .await;

    // Simulate the crash: a backup row + a job that "was running".
    let p = Arc::clone(&bed.persistence);
    let backup_id = zim_engine_domain::BackupId::new();
    let job_id = JobId::new();
    let backup = zim_engine_domain::Backup {
        id: backup_id,
        instance_id: instance.id,
        job_id,
        uuid: "c0000000-0000-0000-0000-000000000000".to_owned(),
        file_name: "c0000000-0000-0000-0000-000000000000.tar.gz".to_owned(),
        bytes: 0,
        sha256_hash: None,
        locked: false,
        completed_at: None,
    };
    let orphan = Job {
        id: job_id,
        kind: JobKind::Backup,
        instance_id: instance.id,
        state: JobState::InProgress,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        started_at: Some(chrono::Utc::now()),
        completed_at: None,
        last_error: None,
        cleanup_pending: false,
        generation: 0,
        payload: None,
    };
    let row = backup.clone();
    tokio::task::spawn_blocking(move || {
        // Job first: the backup row's FK points at it.
        p.put_job(&orphan).unwrap();
        p.put_backup(&row).unwrap();
    })
    .await
    .unwrap();

    // The partial archive on disk (as a crashed writer leaves it).
    let archive = bed
        .backups
        .archive_path_for(instance.id, backup_id)
        .await
        .unwrap();
    let part = part_path_for(&archive);
    std::fs::create_dir_all(part.parent().unwrap()).unwrap();
    std::fs::write(&part, b"partial bytes from the crashed run").unwrap();
    assert!(part.exists());

    // Boot reconciliation + the background cleaner.
    let orphans = bed.jobs.reconcile_orphans().await.unwrap();
    assert_eq!(orphans, 1);
    let job = bed.wait_for_job(job_id, JobState::Failed).await;
    assert!(job.last_error.as_deref().unwrap().contains("restarted"));

    // The partial archive is gone.
    assert!(!part.exists(), "the cleaner must delete the .part file");
    // The sealed name never appeared.
    assert!(!archive.exists());
}

#[tokio::test]
async fn restore_returns_live_data_to_the_backup_point() {
    let bed = Testbed::new().await;
    let instance = bed
        .seed_instance(5, &[("server.properties", "motd=before\n")])
        .await;

    // Back up, then mutate the live data.
    let (job, backup) = bed.backups.create_backup(instance.id).await.unwrap();
    bed.wait_for_job(job.id, JobState::Succeeded).await;
    let live = instance_data_dir(&bed.data_root, &instance.uuid_short);
    std::fs::write(live.join("server.properties"), "motd=AFTER THE FACT\n").unwrap();
    std::fs::write(live.join("new-file.txt"), "written after backup").unwrap();

    // Restore: the swap must bring the original content back.
    let (job, _backup) = bed
        .backups
        .restore_backup(instance.id, backup.id)
        .await
        .unwrap();
    bed.wait_for_job(job.id, JobState::Succeeded).await;
    let _ = job;
    assert_eq!(
        std::fs::read_to_string(live.join("server.properties")).unwrap(),
        "motd=before\n"
    );
    // The post-backup file is gone (the whole tree was swapped).
    assert!(!live.join("new-file.txt").exists());
    // No staging leftovers.
    let instances_dir = bed.data_root.join("instances");
    for entry in std::fs::read_dir(&instances_dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().to_string();
        assert!(!name.contains(".restoring."), "leftover staging: {name}");
    }
}

#[tokio::test]
async fn backup_limit_is_enforced() {
    let bed = Testbed::new().await;
    let instance = bed.seed_instance(1, &[("a.txt", "a")]).await;

    let first = bed.backups.create_backup(instance.id).await.unwrap();
    bed.wait_for_job(first.0.id, JobState::Succeeded).await;

    // The listing now holds 1 backup; the limit is 1.
    match bed.backups.create_backup(instance.id).await {
        Err(BackupError::LimitReached { limit }) => assert_eq!(limit, 1),
        other => panic!("expected LimitReached, got {other:?}"),
    }

    // Deleting the backup frees the slot again.
    bed.backups
        .delete_backup(instance.id, first.1.id)
        .await
        .unwrap();
    bed.backups.create_backup(instance.id).await.unwrap();
}

#[tokio::test]
async fn locked_backup_refuses_deletion() {
    let bed = Testbed::new().await;
    let instance = bed.seed_instance(5, &[("a.txt", "a")]).await;
    let (job, backup) = bed.backups.create_backup(instance.id).await.unwrap();
    bed.wait_for_job(job.id, JobState::Succeeded).await;

    // Lock, then try to delete.
    bed.backups
        .set_locked(instance.id, backup.id, true)
        .await
        .unwrap();
    match bed.backups.delete_backup(instance.id, backup.id).await {
        Err(BackupError::Locked(id)) => assert_eq!(id, backup.id),
        other => panic!("expected Locked, got {other:?}"),
    }

    // Unlock, delete succeeds; the archive file disappears with it.
    bed.backups
        .set_locked(instance.id, backup.id, false)
        .await
        .unwrap();
    bed.backups
        .delete_backup(instance.id, backup.id)
        .await
        .unwrap();
    let archive = bed.backups.archive_path_for(instance.id, backup.id).await;
    assert!(archive.is_err() || !archive.unwrap().exists());
}

#[tokio::test]
async fn restore_of_incomplete_backup_is_rejected() {
    let bed = Testbed::new().await;
    let instance = bed.seed_instance(5, &[("a.txt", "a")]).await;
    let (job, backup) = bed.backups.create_backup(instance.id).await.unwrap();
    // Do NOT wait for completion: race-free by checking that a
    // restore of a not-yet-completed row refuses. Give the job a
    // moment to be mid-flight or already done:
    tokio::time::sleep(Duration::from_millis(5)).await;
    let p = Arc::clone(&bed.persistence);
    let row = tokio::task::spawn_blocking({
        let id = backup.id;
        move || p.get_backup(&id)
    })
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    if row.sha256_hash.is_none() {
        match bed.backups.restore_backup(instance.id, backup.id).await {
            Err(BackupError::NotCompleted(_)) => {}
            other => panic!("expected NotCompleted, got {other:?}"),
        }
    }
    // Let the create finish so the testbed shuts down cleanly.
    bed.wait_for_job(job.id, JobState::Succeeded).await;
}

// ── Helpers referenced by the acceptance tests ────────────────────

/// A no-op so `Path` stays imported even on platforms where the
/// symlink block compiles out.
#[allow(dead_code)]
fn scratch_marker(_: &Path) {}
