//! Migration importer end-to-end tests (roadmap §M10): the
//! fixture panel imports into an in-memory SQLite store with every
//! §2.3 mapping verified — entities, vocabulary folds, synthetic
//! job companions, the FK graph — plus the dry-run rehearsal and
//! re-import idempotency the acceptance criteria name.

use std::path::PathBuf;
use std::sync::Arc;

use zim_engine_application::ports::Persistence;
use zim_engine_domain::{
    InstanceId, InstanceStatus, JobKind, JobState, NodeDeployment, PermissionGrant, ScheduleAction,
    TransferState,
};
use zim_engine_migrate_pterodactyl::{import_panel, ImportPolicy, ImportReport, JsonPanelSource};

/// The fixture's path.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/panel_fixture.json")
}

/// A migrated in-memory sink.
fn sink() -> Arc<zim_engine_persistence_sqlite::SqlitePersistence> {
    let store = zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap();
    store.migrate().unwrap();
    Arc::new(store)
}

/// One fixture import into a fresh sink.
fn import() -> (
    Arc<zim_engine_persistence_sqlite::SqlitePersistence>,
    ImportReport,
) {
    let source = JsonPanelSource::from_path(fixture_path());
    let store = sink();
    let report = import_panel(&source, &*store, &ImportPolicy::default()).unwrap();
    (store, report)
}

/// The two imported instance ids (uuids from the fixture).
fn survival() -> InstanceId {
    "2ab5c1d0-0000-4000-8000-000000010101".parse().unwrap()
}

fn sky_factory() -> InstanceId {
    "9f8e7d6c-0000-4000-8000-000000010102".parse().unwrap()
}

fn outcome<'a>(
    report: &'a ImportReport,
    table: &str,
) -> &'a zim_engine_migrate_pterodactyl::TableOutcome {
    report
        .outcome(table)
        .unwrap_or_else(|| panic!("the report must carry an outcome for {table:?}"))
}

#[test]
fn fixture_imports_every_mapped_table() {
    let (_, report) = import();
    assert_eq!(outcome(&report, "users").imported, 2);
    assert_eq!(outcome(&report, "nodes").imported, 2);
    assert_eq!(outcome(&report, "eggs").imported, 1);
    assert_eq!(outcome(&report, "eggs").skipped.len(), 1);
    assert_eq!(outcome(&report, "instances").imported, 2);
    assert_eq!(outcome(&report, "instances").skipped.len(), 1);
    assert_eq!(outcome(&report, "allocations").imported, 4);
    assert_eq!(outcome(&report, "permissions").imported, 2);
    assert_eq!(outcome(&report, "permissions").skipped.len(), 1);
    assert_eq!(outcome(&report, "instance_variables").imported, 2);
    assert_eq!(outcome(&report, "instance_variables").skipped.len(), 1);
    assert_eq!(outcome(&report, "schedules").imported, 1);
    assert_eq!(outcome(&report, "schedules").skipped.len(), 1);
    assert_eq!(outcome(&report, "backups").imported, 2);
    assert_eq!(outcome(&report, "backups").skipped.len(), 1);
    assert_eq!(outcome(&report, "transfers").imported, 2);
    assert_eq!(outcome(&report, "audit").imported, 4);
    // The FK companions: two backup jobs + two transfer jobs.
    assert_eq!(outcome(&report, "jobs").imported, 4);
    // The rejected egg's skip reason names the security gate
    // (Pterodactyl's own spelling of the flag).
    let reason = &outcome(&report, "eggs").skipped[0].reason;
    assert!(
        reason.contains("priviledged_install_scripts"),
        "the skip reason should name the security gate: {reason}"
    );
}

#[test]
fn users_land_with_roles_and_external_ids() {
    let (store, _) = import();
    let admin = store
        .get_user_by_email("admin@panel.example")
        .unwrap()
        .unwrap();
    assert!(admin.root_admin && admin.is_admin);
    assert_eq!(admin.username, "zamin");
    // The Panel UUID becomes the ZIM user id (round-trip).
    assert_eq!(admin.id.to_string(), "0c47e1f6-8b1a-4a2f-9e7d-6f3d21a3e211");
    // Mixed-case email lowercased; external id preserved.
    let nova = store
        .get_user_by_email("nova@example.com")
        .unwrap()
        .unwrap();
    assert!(!nova.root_admin);
    assert_eq!(nova.external_id.as_deref(), Some("billing-77"));
    assert_eq!(nova.id.to_string(), "3b9f2c8d-7e6a-4d5b-8c1a-9d0e1f2a3b22");
}

#[test]
fn nodes_import_as_remote_linux_with_unknown_state() {
    let (store, _) = import();
    let nodes = store.list_nodes().unwrap();
    assert_eq!(nodes.len(), 2);
    assert!(nodes.iter().all(|node| {
        node.deployment == NodeDeployment::RemoteLinux
            && node.last_heartbeat.is_none()
            && node.last_seen_state == zim_engine_domain::NodeState::Unknown
    }));
    // The panel's daemonListen serves both port surfaces.
    let fra_1 = nodes
        .iter()
        .find(|node| node.name == "wings-fra-1")
        .expect("node 1 imported");
    assert_eq!(fra_1.public_port, 8443);
    assert_eq!(fra_1.listen_port, 8443);
    assert_eq!(fra_1.fqdn, "fra-1.panel.example");
}

#[test]
fn eggs_reconstruct_through_the_legacy_importer() {
    let (store, _) = import();
    let egg = store
        .get_egg_by_legacy_uuid("d6b5f3a2-7e8f-4c9d-a0b1-c2d3e4f5a605")
        .unwrap()
        .expect("the paper egg imported");
    assert_eq!(egg.name, "Paper");
    assert_eq!(egg.author, "parkervcp");
    assert_eq!(egg.variables.len(), 3);
    let jar = egg
        .variables
        .iter()
        .find(|v| v.env_name == "SERVER_JARFILE")
        .expect("the jar variable");
    assert_eq!(jar.default_value, "server.jar");
    assert_eq!(
        jar.rules,
        vec![
            "required".to_owned(),
            "string".to_owned(),
            "max:20".to_owned()
        ]
    );
    assert!(egg.startup.as_str().contains("{{SERVER_JARFILE}}"));
    // The install script runs in its constrained container.
    let install = egg.scripts.install.as_ref().expect("an install script");
    assert_eq!(install.container.to_string(), "debian:12-slim");
    assert_eq!(install.entrypoint, "bash");
    assert!(install.script.contains("paper.jar"));
    // The labeled images survive as the labeled map.
    assert_eq!(egg.images.len(), 1);
    assert_eq!(egg.images[0].label, "Java 21");
    // The Panel's creation time carries forward.
    assert_eq!(egg.created_at.to_rfc3339(), "2024-01-05T09:00:00+00:00");
}

#[test]
fn instances_map_status_limits_and_fallbacks() {
    let (store, _) = import();
    let egg = store
        .get_egg_by_legacy_uuid("d6b5f3a2-7e8f-4c9d-a0b1-c2d3e4f5a605")
        .unwrap()
        .unwrap();

    // ── The healthy server ──
    let survival = store.get_instance(&survival()).unwrap().unwrap();
    assert_eq!(survival.status, InstanceStatus::Running);
    assert_eq!(survival.name, "Survival Prime");
    assert_eq!(survival.uuid_short, "2ab5c1d0");
    assert_eq!(survival.external_id.as_deref(), Some("ext-101"));
    assert_eq!(survival.egg_id, egg.id);
    assert_eq!(survival.allocation_id.0, 1001);
    assert_eq!(survival.backup_limit, 5);
    assert_eq!(survival.allocation_limit, Some(3));
    assert_eq!(survival.limits.memory.as_mib(), 4096);
    assert_eq!(survival.limits.io_weight, 500);
    assert_eq!(survival.limits.cpu_millicores, 2000);
    assert_eq!(survival.limits.threads.as_deref(), Some("0-3"));
    assert!(!survival.skip_egg_scripts);
    assert_eq!(
        survival.installed_at.map(|ts| ts.to_rfc3339()),
        Some("2024-03-02T08:15:00+00:00".to_owned())
    );

    // ── The crashed server with every documented fold ──
    let sky = store.get_instance(&sky_factory()).unwrap().unwrap();
    assert_eq!(sky.status, InstanceStatus::CrashLoop);
    // Unlimited swap folded to disabled; io clamped; pin dropped.
    assert_eq!(sky.limits.swap.as_bytes(), 0);
    assert_eq!(sky.limits.io_weight, 1000);
    assert_eq!(sky.limits.threads, None);
    assert_eq!(sky.limits.cpu_millicores, 0);
    assert!(sky.skip_egg_scripts);
    assert_eq!(sky.backup_limit, 0);
    // The malformed uuidShort was derived from the canonical uuid.
    assert_eq!(sky.uuid_short, "9f8e7d6c");
    // Absent startup/image fell back to the egg's.
    assert!(sky.startup.as_str().contains("{{SERVER_JARFILE}}"));
    assert_eq!(sky.image.to_string(), "ghcr.io/parkervcp/yolks:java_21");
}

#[test]
fn allocations_preserve_ids_and_follow_instances() {
    let (store, _) = import();
    let primary = store
        .find_allocation_by_instance(&survival())
        .unwrap()
        .unwrap();
    assert_eq!(primary.id.0, 1001);
    assert_eq!(primary.port, 25565);
    assert_eq!(primary.ip.to_string(), "192.0.2.10");
    assert_eq!(primary.note.as_deref(), Some("primary"));
    // The allocation bound to the skipped server imported as free.
    let fra_1 = store
        .list_nodes()
        .unwrap()
        .into_iter()
        .find(|node| node.name == "wings-fra-1")
        .unwrap();
    let allocations = store.list_allocations_by_node(&fra_1.id).unwrap();
    let freed = allocations
        .iter()
        .find(|a| a.id.0 == 1003)
        .expect("allocation 1003 imported");
    assert!(freed.assigned_to.is_none());
    assert_eq!(allocations.len(), 3);
}

#[test]
fn subuser_grants_map_with_wildcards_and_unknown_names() {
    let (store, _) = import();
    let nova: zim_engine_domain::UserId = "3b9f2c8d-7e6a-4d5b-8c1a-9d0e1f2a3b22".parse().unwrap();
    let scoped = store
        .get_permission(&nova, &survival())
        .unwrap()
        .expect("the subuser row");
    assert!(scoped.grants.contains(PermissionGrant::ControlStart));
    assert!(scoped.grants.contains(PermissionGrant::ControlStop));
    assert!(scoped.grants.contains(PermissionGrant::WebsocketConnect));
    assert!(scoped.grants.contains(PermissionGrant::ScheduleRead));
    assert!(!scoped.grants.contains(PermissionGrant::BackupCreate));
    // The wildcard widened the second subuser to the full set.
    let widened = store
        .get_permission(&nova, &sky_factory())
        .unwrap()
        .expect("the wildcard subuser row");
    assert!(widened.grants.superset_of(&scoped.grants));
    assert!(widened.grants.contains(PermissionGrant::WorkloadDeploy));
}

#[test]
fn instance_variables_carry_the_overrides() {
    let (store, _) = import();
    let variables = store.list_instance_variables(&survival()).unwrap();
    assert_eq!(
        variables,
        vec![
            zim_engine_domain::InstanceVariable {
                env_name: "MINECRAFT_VERSION".to_owned(),
                value: "1.20.1".to_owned(),
            },
            zim_engine_domain::InstanceVariable {
                env_name: "SERVER_JARFILE".to_owned(),
                value: "paper.jar".to_owned(),
            },
        ]
    );
    assert!(store
        .list_instance_variables(&sky_factory())
        .unwrap()
        .is_empty());
}

#[test]
fn synthetic_jobs_satisfy_the_fk_contract() {
    let (store, _) = import();
    let jobs = store.list_jobs_by_instance(&survival()).unwrap();
    assert_eq!(jobs.len(), 4);
    let backup_jobs: Vec<_> = jobs
        .iter()
        .filter(|job| job.kind == JobKind::Backup)
        .collect();
    assert_eq!(backup_jobs.len(), 2);
    assert!(backup_jobs
        .iter()
        .any(|job| job.state == JobState::Succeeded && job.completed_at.is_some()));
    let failed = backup_jobs
        .iter()
        .find(|job| job.state == JobState::Failed)
        .expect("the failed backup's job");
    assert!(failed.last_error.is_some());
    // The transfer jobs mirror the transfer outcomes.
    let transfer_jobs: Vec<_> = jobs
        .iter()
        .filter(|job| job.kind == JobKind::Transfer)
        .collect();
    assert_eq!(transfer_jobs.len(), 2);
    assert!(transfer_jobs.iter().all(|job| job.state.is_terminal()));

    // Backups link to their jobs and carry the canonical names.
    let backups = store.list_backups_by_instance(&survival()).unwrap();
    assert_eq!(backups.len(), 2);
    for backup in &backups {
        let linked = store.get_job(&backup.job_id).unwrap();
        assert!(linked.is_some(), "the backup's job exists");
        assert_eq!(backup.file_name, format!("{}.tar.gz", backup.uuid));
    }
    let locked = backups
        .iter()
        .find(|b| b.uuid.ends_with("0501"))
        .expect("the nightly backup");
    assert!(locked.locked);
    assert_eq!(locked.bytes, 1048576);
    assert_eq!(
        locked.sha256_hash.as_deref(),
        Some("1111111111111111111111111111111111111111111111111111111111111111")
    );

    // Transfers carry the node pair and terminal states.
    let transfers = store.list_transfers_by_instance(&survival()).unwrap();
    assert_eq!(transfers.len(), 2);
    let node_1 = nodes_id_by_name(&store, "wings-fra-1");
    let node_2 = nodes_id_by_name(&store, "wings-fra-2");
    let succeeded = transfers
        .iter()
        .find(|t| t.state == TransferState::Succeeded)
        .expect("the completed transfer");
    assert_eq!(succeeded.from_node_id, node_1);
    assert_eq!(succeeded.to_node_id, node_2);
    let in_flight = transfers
        .iter()
        .find(|t| t.state == TransferState::Failed)
        .expect("the in-flight transfer imported as failed");
    assert_eq!(in_flight.from_node_id, node_2);
    assert_eq!(in_flight.to_node_id, node_1);
}

#[test]
fn schedules_join_cron_parts_and_order_actions() {
    let (store, _) = import();
    let schedules = store.list_schedules_by_instance(&survival()).unwrap();
    assert_eq!(schedules.len(), 1);
    let schedule = &schedules[0];
    assert_eq!(schedule.name, "Nightly restart");
    assert_eq!(schedule.cron, "0 4 * * *");
    assert!(schedule.is_active);
    assert!(schedule.only_when_online);
    assert!(schedule.next_run_at.is_some());
    assert_eq!(
        schedule.actions,
        vec![
            ScheduleAction::Power {
                action: zim_engine_domain::PowerAction::Restart
            },
            ScheduleAction::SendCommand {
                command: "say Restarting in 5 minutes".to_owned()
            },
        ]
    );
}

#[test]
fn audit_records_normalize_events_actors_and_metadata() {
    let (store, _) = import();
    let records = store.list_audit(None, 100).unwrap();
    assert_eq!(records.len(), 4);
    // Timestamp order (not panel id order): 602 < 600 < 601 < 603.
    let actions: Vec<&str> = records.iter().map(|r| r.action.as_str()).collect();
    assert_eq!(
        actions,
        vec![
            "user.login",
            "server.power.start",
            "server.file.download",
            "auth.ip-blocked"
        ]
    );
    // The user actor resolves through the user map.
    let login = &records[0];
    assert_eq!(login.actor, zim_engine_domain::AuditActor::System);
    assert!(login.instance_id.is_none());
    let power = &records[1];
    assert_eq!(power.actor, zim_engine_domain::AuditActor::User);
    assert_eq!(
        power.actor_id.as_deref(),
        Some("0c47e1f6-8b1a-4a2f-9e7d-6f3d21a3e211")
    );
    assert_eq!(power.instance_id, Some(survival()));
    assert_eq!(
        power.ip_address.map(|ip| ip.to_string()).as_deref(),
        Some("203.0.113.50")
    );
    // The string fallback for non-object properties (§2.6).
    let download = &records[2];
    assert_eq!(download.instance_id, Some(survival()));
    assert_eq!(
        download
            .metadata
            .get("legacy_value")
            .and_then(|v| v.as_str()),
        Some("legacy junk that is not json")
    );
    // The human description folded into the metadata.
    assert_eq!(
        download
            .metadata
            .get("panel_description")
            .and_then(|v| v.as_str()),
        Some("world_backup.zip")
    );
}

/// A node's id by name (the transfer-pair assertions).
fn nodes_id_by_name(
    store: &zim_engine_persistence_sqlite::SqlitePersistence,
    name: &str,
) -> zim_engine_domain::NodeId {
    store
        .list_nodes()
        .unwrap()
        .into_iter()
        .find(|node| node.name == name)
        .unwrap()
        .id
}

#[test]
fn the_fk_graph_cascades_like_the_engine_expects() {
    let (store, _) = import();
    // Unassign the allocation first (the engine's delete flow
    // does the same — allocations RESTRICT instance deletes).
    let mut primary = store
        .get_allocation(&zim_engine_domain::AllocationId::new(1001))
        .unwrap()
        .unwrap();
    primary.assigned_to = None;
    store.put_allocation(&primary).unwrap();
    // Deleting an instance clears its companions — the imported
    // rows participate in the engine's FK contract.
    store.delete_instance(&survival()).unwrap();
    assert!(store.list_jobs_by_instance(&survival()).unwrap().is_empty());
    assert!(store
        .list_backups_by_instance(&survival())
        .unwrap()
        .is_empty());
    assert!(store
        .list_schedules_by_instance(&survival())
        .unwrap()
        .is_empty());
    assert!(store
        .list_instance_variables(&survival())
        .unwrap()
        .is_empty());
    // The allocation survives, unassigned (ON DELETE SET NULL).
    let freed = store
        .get_allocation(&zim_engine_domain::AllocationId::new(1001))
        .unwrap()
        .unwrap();
    assert!(freed.assigned_to.is_none());
}

#[test]
fn reimporting_converges_on_entity_rows() {
    let source = JsonPanelSource::from_path(fixture_path());
    let store = sink();
    import_panel(&source, &*store, &ImportPolicy::default()).unwrap();
    import_panel(&source, &*store, &ImportPolicy::default()).unwrap();
    // Entity rows converge: the store's upserts plus the egg's
    // legacy-UUID and the backup's UUID deduplication.
    assert_eq!(store.list_eggs().unwrap().len(), 1);
    assert_eq!(store.list_users().unwrap().len(), 2);
    let instances = store
        .list_instances_by_owner(&"0c47e1f6-8b1a-4a2f-9e7d-6f3d21a3e211".parse().unwrap())
        .unwrap();
    assert_eq!(instances.len(), 1);
    assert_eq!(
        store.list_backups_by_instance(&survival()).unwrap().len(),
        2
    );
    // History rows append: audit (and transfer/job rows) record
    // each pass (the documented re-import semantics).
    let audit = store.list_audit(None, 1000).unwrap();
    assert_eq!(audit.len(), 8, "audit appends on both passes");
    let transfers = store.list_transfers_by_instance(&survival()).unwrap();
    assert_eq!(transfers.len(), 4, "transfer history duplicates per pass");
}

#[test]
fn dry_run_rehearsal_writes_nothing() {
    // A unique scratch path; the rehearsal must not create it.
    let target = std::env::temp_dir().join(format!(
        "zim-migrate-dryrun-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let args = vec![
        "pterodactyl".to_owned(),
        format!("--panel-fixture={}", fixture_path().display()),
        format!("--zim-db={}", target.display()),
        "--dry-run".to_owned(),
    ];
    let code = zim_engine_migrate_pterodactyl::cli::run_cli(&args);
    assert_eq!(code, 0, "the dry run exits successfully");
    assert!(
        !target.exists(),
        "the dry run must not create the target database"
    );
}

#[test]
fn a_real_import_through_the_cli_writes_the_database() {
    let target = std::env::temp_dir().join(format!(
        "zim-migrate-real-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let args = vec![
        "pterodactyl".to_owned(),
        format!("--panel-fixture={}", fixture_path().display()),
        format!("--zim-db={}", target.display()),
    ];
    let code = zim_engine_migrate_pterodactyl::cli::run_cli(&args);
    assert_eq!(code, 0);
    assert!(target.exists(), "the real import creates the database");
    // The written store is a valid engine database.
    let store = zim_engine_persistence_sqlite::SqlitePersistence::open(&target).unwrap();
    assert_eq!(store.schema_version().unwrap(), 4);
    assert_eq!(store.list_users().unwrap().len(), 2);
    let _ = std::fs::remove_file(&target);
}

#[test]
fn usage_errors_exit_with_code_two() {
    let code = zim_engine_migrate_pterodactyl::cli::run_cli(&[]);
    assert_eq!(code, 2);
    let code = zim_engine_migrate_pterodactyl::cli::run_cli(&["wings".to_owned()]);
    assert_eq!(code, 2);
    let code = zim_engine_migrate_pterodactyl::cli::run_cli(&[
        "pterodactyl".to_owned(),
        "--panel-fixture=/nonexistent.json".to_owned(),
    ]);
    assert_eq!(code, 2, "a fixture without --zim-db is a usage error");
}
