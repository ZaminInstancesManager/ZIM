//! Entity persistence round-trip tests — the M3 acceptance
//! criterion "SQLite persistence round-trip tests pass for every
//! entity" from `docs/architecture/IMPLEMENTATION_ROADMAP.md` §M3.
//!
//! Each test builds a fully-populated entity graph, writes it
//! through the [`Persistence`] port, reads it back, and asserts
//! field-for-field equality. The port is exercised as a trait object
//! where possible so the tests verify the contract, not the concrete
//! type.

use std::collections::BTreeMap;

use zim_engine_application::ports::Persistence;
use zim_engine_domain::{
    Allocation, AllocationId, AuditActor, AuditRecord, Backup, BackupId, Bytes, CommandTemplate,
    EggId, ExtensionId, ExtensionManifest, ExtensionRegistration, ExtensionSignature, Instance,
    InstanceId, InstanceScope, InstanceStatus, Job, JobId, JobKind, JobState, Node,
    NodeCapabilities, NodeDeployment, NodeId, NodeState, Permission, PermissionGrant,
    PermissionSet, ResourceLimits, Schedule, ScheduleAction, ScheduleId, Scheme, SignatureScheme,
    Transfer, TransferId, TransferState, User, UserId,
};
use zim_engine_persistence_sqlite::SqlitePersistence;

fn migrated_store() -> SqlitePersistence {
    let store = SqlitePersistence::open_in_memory().unwrap();
    store.migrate().unwrap();
    store
}

fn sample_node(name: &str) -> Node {
    Node {
        id: NodeId::new(),
        name: name.to_owned(),
        location_id: "6ba7b810-9dad-11d1-80b4-00c04fd430c8".parse().unwrap(),
        fqdn: format!("{name}.example.org"),
        scheme: Scheme::Https,
        public_port: 2022,
        listen_port: 8443,
        capabilities: NodeCapabilities {
            container_runtime: true,
            console: true,
            ready_probe: false,
            health_check: true,
            filesystem: true,
            network: false,
            snapshot: true,
        },
        deployment: NodeDeployment::LocalLinux,
        last_heartbeat: Some(chrono::Utc::now()),
        last_seen_state: NodeState::Online,
    }
}

fn sample_user(email: &str) -> User {
    User {
        id: UserId::new(),
        email: email.to_owned(),
        username: email.split('@').next().unwrap().to_owned(),
        external_id: None,
        password_hash: Some("$argon2id$v=19$m=65536,t=3,p=1$c29tZXNhbHQ".to_owned()),
        oauth_subject: None,
        is_admin: false,
        root_admin: false,
    }
}

fn sample_allocation(node: &Node, port: u16) -> Allocation {
    Allocation {
        id: AllocationId::new(port as u64),
        node_id: node.id,
        ip: "203.0.113.10".parse().unwrap(),
        port,
        assigned_to: None,
        note: Some("primary".to_owned()),
    }
}

fn sample_instance(node: &Node, owner: &User, allocation: &Allocation) -> Instance {
    let (uuid, uuid_short) = Instance::legacy_uuid_pair();
    Instance {
        id: InstanceId::new(),
        external_id: Some("ext-1".to_owned()),
        uuid,
        uuid_short,
        node_id: node.id,
        name: "Survival Server".to_owned(),
        description: "Friendly survival world".to_owned(),
        status: InstanceStatus::Offline,
        skip_egg_scripts: false,
        owner_id: owner.id,
        limits: ResourceLimits {
            memory: Bytes::from_mib(2048),
            swap: Bytes::from_bytes(0),
            disk: Bytes::from_gib(10),
            io_weight: 500,
            cpu_millicores: 1500,
            threads: Some("0-3".to_owned()),
            oom_disabled: false,
        },
        allocation_id: allocation.id,
        egg_id: EggId::new(),
        startup: CommandTemplate::new("java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JAR}}"),
        image: "ghcr.io/parkervcp/yolks:java_21".parse().unwrap(),
        allocation_limit: Some(3),
        database_limit: Some(2),
        backup_limit: 5,
        crash_detection_enabled: true,
        installed_at: Some(chrono::Utc::now()),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn sample_job(instance: &Instance) -> Job {
    Job {
        id: JobId::new(),
        kind: JobKind::Backup,
        instance_id: instance.id,
        state: JobState::InProgress,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        started_at: Some(chrono::Utc::now()),
        completed_at: None,
        last_error: None,
        cleanup_pending: false,
        generation: 7,
        payload: None,
    }
}

fn sample_backup(instance: &Instance, job: &Job) -> Backup {
    Backup {
        id: BackupId::new(),
        instance_id: instance.id,
        job_id: job.id,
        uuid: "6ba7b810-9dad-11d1-80b4-00c04fd430c8".to_owned(),
        file_name: "6ba7b810-9dad-11d1-80b4-00c04fd430c8.tar.gz".to_owned(),
        bytes: 104_857_600,
        sha256_hash: Some(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_owned(),
        ),
        locked: true,
        completed_at: Some(chrono::Utc::now()),
    }
}

fn sample_transfer(instance: &Instance, job: &Job, from: &Node, to: &Node) -> Transfer {
    Transfer {
        id: TransferId::new(),
        instance_id: instance.id,
        job_id: job.id,
        from_node_id: from.id,
        to_node_id: to.id,
        state: TransferState::ArchiveTransferring,
        archive_path: Some("/var/lib/zim/transfers/tmp-abc.tar.gz".to_owned()),
    }
}

fn sample_schedule(instance: &Instance) -> Schedule {
    Schedule {
        id: ScheduleId::new(),
        instance_id: instance.id,
        name: "Nightly restart".to_owned(),
        cron: "0 4 * * *".to_owned(),
        next_run_at: Some(chrono::Utc::now()),
        last_run_at: None,
        is_active: true,
        only_when_online: true,
        actions: vec![
            ScheduleAction::SendCommand {
                command: "say Restarting in 5 minutes".to_owned(),
            },
            ScheduleAction::Power {
                action: zim_engine_domain::PowerAction::Restart,
            },
        ],
    }
}

fn sample_audit(instance: &Instance) -> AuditRecord {
    AuditRecord {
        id: 0, // assigned by the store
        timestamp: chrono::Utc::now(),
        actor: AuditActor::User,
        actor_id: Some("550e8400-e29b-41d4-a716-446655440000".to_owned()),
        action: "instance.power.start".to_owned(),
        instance_id: Some(instance.id),
        metadata: serde_json::json!({
            "desired_state": "running",
            "generation": 12,
            "nested": { "ok": true },
        }),
        ip_address: Some("203.0.113.99".parse().unwrap()),
        user_agent: Some("zim-frontend/0.1".to_owned()),
    }
}

fn sample_extension() -> ExtensionRegistration {
    ExtensionRegistration {
        id: ExtensionId::new(),
        manifest: ExtensionManifest {
            manifest_version: 1,
            id: "org.example.zim-addon.database-manager".to_owned(),
            name: "Database Manager".to_owned(),
            description: "Adds a database management UI to ZIM.".to_owned(),
            author: "Example Author".to_owned(),
            license: "MIT".to_owned(),
            api_version: "v1".to_owned(),
            engines: ">=0.1.0".to_owned(),
            permissions: vec!["database.read".to_owned()],
            instance_scope: InstanceScope::Optional,
            extra: serde_json::json!({
                "page_surfaces": ["zim://servers/{id}/databases"],
                "event_subscriptions": ["instance.state_change"],
            }),
        },
        version: "0.1.0".to_owned(),
        installed_at: chrono::Utc::now(),
        enabled: true,
        granted_permissions: {
            let mut set = PermissionSet::new();
            set.grant(PermissionGrant::DatabaseRead)
                .grant(PermissionGrant::DatabaseDelete);
            set
        },
        signature: Some(ExtensionSignature {
            scheme: SignatureScheme::Ed25519,
            key_id: "SHA256:7xK3dEfQm2RzX5wY".to_owned(),
            signature: "base64encodedsignature==".to_owned(),
        }),
    }
}

/// The complete entity graph, written and read back field-for-field.
/// This is the headline M3 acceptance test: every entity round-trips.
#[test]
fn full_entity_graph_round_trips() {
    let store = migrated_store();

    let node_a = sample_node("node-a");
    let node_b = sample_node("node-b");
    let user = sample_user("ops@example.org");
    let allocation = sample_allocation(&node_a, 25565);
    let instance = sample_instance(&node_a, &user, &allocation);
    let job = sample_job(&instance);
    let backup = sample_backup(&instance, &job);
    let transfer = sample_transfer(&instance, &job, &node_a, &node_b);
    let schedule = sample_schedule(&instance);
    let extension = sample_extension();

    store.put_node(&node_a).unwrap();
    store.put_node(&node_b).unwrap();
    store.put_user(&user).unwrap();
    store.put_allocation(&allocation).unwrap();
    store.put_instance(&instance).unwrap();
    store
        .put_permission(&Permission {
            user_id: user.id,
            instance_id: instance.id,
            grants: {
                let mut set = PermissionSet::new();
                set.grant(PermissionGrant::ControlStart)
                    .grant(PermissionGrant::ControlStop)
                    .grant(PermissionGrant::FileRead);
                set
            },
        })
        .unwrap();
    store.put_job(&job).unwrap();
    store.put_backup(&backup).unwrap();
    store.put_transfer(&transfer).unwrap();
    store.put_schedule(&schedule).unwrap();
    store.put_extension(&extension).unwrap();
    let audit_id = store.append_audit(&sample_audit(&instance)).unwrap();

    // ── Read everything back ────────────────────────────────────────
    assert_eq!(store.get_node(&node_a.id).unwrap().unwrap(), node_a);
    assert_eq!(store.get_node(&node_b.id).unwrap().unwrap(), node_b);
    assert_eq!(store.get_user(&user.id).unwrap().unwrap(), user);
    assert_eq!(
        store.get_user_by_email("ops@example.org").unwrap().unwrap(),
        user
    );
    assert_eq!(
        store.get_allocation(&allocation.id).unwrap().unwrap(),
        allocation
    );
    assert_eq!(store.get_instance(&instance.id).unwrap().unwrap(), instance);
    assert_eq!(store.get_job(&job.id).unwrap().unwrap(), job);
    assert_eq!(store.get_backup(&backup.id).unwrap().unwrap(), backup);
    assert_eq!(store.get_transfer(&transfer.id).unwrap().unwrap(), transfer);
    assert_eq!(store.get_schedule(&schedule.id).unwrap().unwrap(), schedule);
    assert_eq!(
        store.get_extension(&extension.id).unwrap().unwrap(),
        extension
    );

    let permission = store
        .get_permission(&user.id, &instance.id)
        .unwrap()
        .unwrap();
    assert!(permission.grants.contains(PermissionGrant::ControlStart));
    assert!(permission.grants.contains(PermissionGrant::ControlStop));
    assert!(permission.grants.contains(PermissionGrant::FileRead));
    assert!(!permission.grants.contains(PermissionGrant::ControlConsole));

    let audit_page = store.list_audit(None, 10).unwrap();
    assert_eq!(audit_page.len(), 1);
    assert_eq!(audit_page[0].id, audit_id);
    assert_eq!(audit_page[0].action, "instance.power.start");
    assert_eq!(audit_page[0].actor, AuditActor::User);
    assert_eq!(audit_page[0].instance_id, Some(instance.id));
    assert_eq!(
        audit_page[0].metadata["desired_state"].as_str().unwrap(),
        "running"
    );
    assert_eq!(
        audit_page[0].ip_address,
        Some("203.0.113.99".parse().unwrap())
    );

    // ── List queries ────────────────────────────────────────────────
    assert_eq!(store.list_nodes().unwrap().len(), 2);
    assert_eq!(store.list_users().unwrap().len(), 1);
    assert_eq!(store.list_allocations_by_node(&node_a.id).unwrap().len(), 1);
    assert_eq!(
        store.list_instances_by_node(&node_a.id).unwrap(),
        vec![instance.clone()]
    );
    assert_eq!(
        store.list_instances_by_owner(&user.id).unwrap(),
        vec![instance.clone()]
    );
    assert_eq!(
        store.list_jobs_by_instance(&instance.id).unwrap(),
        vec![job.clone()]
    );
    assert_eq!(
        store
            .list_jobs_by_state(JobState::InProgress)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store.list_jobs_by_state(JobState::Pending).unwrap().len(),
        0
    );
    assert_eq!(
        store.list_backups_by_instance(&instance.id).unwrap(),
        vec![backup.clone()]
    );
    assert_eq!(
        store.list_transfers_by_instance(&instance.id).unwrap(),
        vec![transfer.clone()]
    );
    assert_eq!(
        store.list_schedules_by_instance(&instance.id).unwrap(),
        vec![schedule.clone()]
    );
    assert_eq!(
        store
            .list_permissions_by_instance(&instance.id)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.list_permissions_by_user(&user.id).unwrap().len(), 1);
    assert_eq!(store.list_extensions().unwrap(), vec![extension.clone()]);
}

#[test]
fn missing_entities_are_none_not_errors() {
    let store = migrated_store();
    assert!(store.get_instance(&InstanceId::new()).unwrap().is_none());
    assert!(store.get_node(&NodeId::new()).unwrap().is_none());
    assert!(store.get_user(&UserId::new()).unwrap().is_none());
    assert!(store.get_job(&JobId::new()).unwrap().is_none());
    assert!(store.get_backup(&BackupId::new()).unwrap().is_none());
    assert!(store.get_transfer(&TransferId::new()).unwrap().is_none());
    assert!(store.get_schedule(&ScheduleId::new()).unwrap().is_none());
    assert!(store.get_extension(&ExtensionId::new()).unwrap().is_none());
    assert!(store
        .get_allocation(&AllocationId::new(9999))
        .unwrap()
        .is_none());
    assert!(store
        .get_user_by_email("nobody@example.org")
        .unwrap()
        .is_none());
}

#[test]
fn put_is_an_upsert() {
    let store = migrated_store();
    let node = sample_node("node-x");
    store.put_node(&node).unwrap();

    // Second put with the same id replaces the row.
    let renamed = Node {
        name: "renamed".to_owned(),
        last_seen_state: NodeState::Offline,
        ..node.clone()
    };
    store.put_node(&renamed).unwrap();
    assert_eq!(store.get_node(&node.id).unwrap().unwrap(), renamed);
    assert_eq!(store.list_nodes().unwrap().len(), 1);
}

#[test]
fn instance_upsert_does_not_trip_foreign_keys() {
    // INSERT OR REPLACE semantics would DELETE + re-INSERT the
    // parent instance row and violate the jobs FK; the upsert must
    // keep the row stable (see the crate docs).
    let store = migrated_store();
    let node = sample_node("node-a");
    let user = sample_user("ops@example.org");
    let allocation = sample_allocation(&node, 25565);
    let instance = sample_instance(&node, &user, &allocation);
    let job = sample_job(&instance);

    store.put_node(&node).unwrap();
    store.put_user(&user).unwrap();
    store.put_allocation(&allocation).unwrap();
    store.put_instance(&instance).unwrap();
    store.put_job(&job).unwrap();

    // Re-put the instance (a parent of jobs) with a changed field.
    let touched = Instance {
        name: "Renamed Server".to_owned(),
        ..instance.clone()
    };
    store.put_instance(&touched).unwrap();

    assert_eq!(store.get_instance(&instance.id).unwrap().unwrap(), touched);
    assert_eq!(store.list_jobs_by_instance(&instance.id).unwrap().len(), 1);
}

#[test]
fn deletes_remove_rows_and_respect_foreign_keys() {
    let store = migrated_store();
    let node = sample_node("node-a");
    let user = sample_user("ops@example.org");
    let allocation = sample_allocation(&node, 25565);
    let instance = sample_instance(&node, &user, &allocation);
    let job = sample_job(&instance);

    store.put_node(&node).unwrap();
    store.put_user(&user).unwrap();
    store.put_allocation(&allocation).unwrap();
    store.put_instance(&instance).unwrap();
    store.put_job(&job).unwrap();

    // Node still carries instances: deleting it must fail.
    assert!(store.delete_node(&node.id).is_err());

    // User still owns instances: deleting it must fail.
    assert!(store.delete_user(&user.id).is_err());

    // Job delete is allowed (backups/transfers referencing it would
    // block it, but none do yet).
    store.delete_job(&job.id).unwrap();
    assert!(store.get_job(&job.id).unwrap().is_none());

    // Delete the instance: children (permissions) cascade per the
    // schema; audit rows survive with instance_id nulled.
    let audit_id = store.append_audit(&sample_audit(&instance)).unwrap();
    store
        .put_permission(&Permission {
            user_id: user.id,
            instance_id: instance.id,
            grants: PermissionSet::new(),
        })
        .unwrap();
    store.delete_instance(&instance.id).unwrap();
    assert!(store.get_instance(&instance.id).unwrap().is_none());
    assert!(store
        .get_permission(&user.id, &instance.id)
        .unwrap()
        .is_none());

    // The audit row survived, with the instance link severed.
    let audit = &store.list_audit(None, 10).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].id, audit_id);
    assert_eq!(audit[0].instance_id, None);

    // Now the node and user can go (once the allocation that
    // references the node is removed).
    store.delete_allocation(&allocation.id).unwrap();
    store.delete_node(&node.id).unwrap();
    store.delete_user(&user.id).unwrap();
    assert!(store.get_node(&node.id).unwrap().is_none());
    assert!(store.get_user(&user.id).unwrap().is_none());
}

#[test]
fn allocation_assignment_link_round_trips() {
    let store = migrated_store();
    let node = sample_node("node-a");
    let user = sample_user("ops@example.org");
    let allocation = sample_allocation(&node, 25565);
    let instance = sample_instance(&node, &user, &allocation);

    store.put_node(&node).unwrap();
    store.put_user(&user).unwrap();
    store.put_allocation(&allocation).unwrap();
    store.put_instance(&instance).unwrap();

    // Assign the allocation to the instance now that it exists.
    let assigned = Allocation {
        assigned_to: Some(instance.id),
        ..allocation.clone()
    };
    store.put_allocation(&assigned).unwrap();
    assert_eq!(
        store.get_allocation(&allocation.id).unwrap().unwrap(),
        assigned
    );
    assert_eq!(
        store
            .find_allocation_by_instance(&instance.id)
            .unwrap()
            .unwrap(),
        assigned
    );
}

#[test]
fn update_instance_status_touches_status_only() {
    let store = migrated_store();
    let node = sample_node("node-a");
    let user = sample_user("ops@example.org");
    let allocation = sample_allocation(&node, 25565);
    let instance = sample_instance(&node, &user, &allocation);

    store.put_node(&node).unwrap();
    store.put_user(&user).unwrap();
    store.put_allocation(&allocation).unwrap();
    store.put_instance(&instance).unwrap();

    store
        .update_instance_status(&instance.id, InstanceStatus::Running)
        .unwrap();
    let updated = store.get_instance(&instance.id).unwrap().unwrap();
    assert_eq!(updated.status, InstanceStatus::Running);
    // updated_at moved forward; created_at is untouched.
    assert!(updated.updated_at > instance.updated_at);
    assert_eq!(updated.created_at, instance.created_at);
    assert_eq!(updated.name, instance.name);
}

#[test]
fn update_job_state_advances_the_state_machine() {
    let store = migrated_store();
    let node = sample_node("node-a");
    let user = sample_user("ops@example.org");
    let allocation = sample_allocation(&node, 25565);
    let instance = sample_instance(&node, &user, &allocation);
    let job = sample_job(&instance);

    store.put_node(&node).unwrap();
    store.put_user(&user).unwrap();
    store.put_allocation(&allocation).unwrap();
    store.put_instance(&instance).unwrap();
    store.put_job(&job).unwrap();

    store
        .update_job_state(&job.id, JobState::CleanupPending)
        .unwrap();
    let updated = store.get_job(&job.id).unwrap().unwrap();
    assert_eq!(updated.state, JobState::CleanupPending);
    assert!(updated.updated_at > job.updated_at);
    assert_eq!(
        store
            .list_jobs_by_state(JobState::CleanupPending)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn audit_is_cursor_paginated_in_id_order() {
    let store = migrated_store();
    let node = sample_node("node-a");
    let user = sample_user("ops@example.org");
    let allocation = sample_allocation(&node, 25565);
    let instance = sample_instance(&node, &user, &allocation);
    store.put_node(&node).unwrap();
    store.put_user(&user).unwrap();
    store.put_allocation(&allocation).unwrap();
    store.put_instance(&instance).unwrap();

    for i in 0..10 {
        let record = AuditRecord {
            id: 0,
            timestamp: chrono::Utc::now(),
            actor: AuditActor::System,
            actor_id: None,
            action: format!("test.event.{i}"),
            instance_id: None,
            metadata: serde_json::json!({ "i": i }),
            ip_address: None,
            user_agent: None,
        };
        store.append_audit(&record).unwrap();
    }

    let page1 = store.list_audit(None, 4).unwrap();
    let page2 = store.list_audit(Some(page1.last().unwrap().id), 4).unwrap();
    let page3 = store.list_audit(Some(page2.last().unwrap().id), 4).unwrap();
    let page4 = store.list_audit(Some(page3.last().unwrap().id), 4).unwrap();

    let all: Vec<AuditRecord> = [page1, page2, page3, page4].concat();

    let ids: Vec<u64> = all.iter().map(|r| r.id).collect();
    assert_eq!(ids, (1..=10).collect::<Vec<u64>>());
    let actions: Vec<String> = all.iter().map(|r| r.action.clone()).collect();
    assert_eq!(
        actions,
        (0..10)
            .map(|i| format!("test.event.{i}"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn command_template_variables_survive_the_round_trip() {
    let store = migrated_store();
    let node = sample_node("node-a");
    let user = sample_user("ops@example.org");
    let allocation = sample_allocation(&node, 25565);
    let instance = sample_instance(&node, &user, &allocation);

    store.put_node(&node).unwrap();
    store.put_user(&user).unwrap();
    store.put_allocation(&allocation).unwrap();
    store.put_instance(&instance).unwrap();

    let back = store.get_instance(&instance.id).unwrap().unwrap();
    let mut vars = BTreeMap::new();
    vars.insert("SERVER_MEMORY".to_owned(), "2048".to_owned());
    vars.insert("SERVER_JAR".to_owned(), "server.jar".to_owned());
    assert_eq!(
        back.startup.render(&vars).unwrap(),
        "java -Xms128M -Xmx2048M -jar server.jar"
    );
    // The typed DockerImage survived too.
    assert_eq!(back.image.to_string(), "ghcr.io/parkervcp/yolks:java_21");
}

#[test]
fn file_backed_store_survives_reopen() {
    // "Migration runner applies migrations forward-only without
    // data loss" — the practical reading: data written before a
    // reopen (and before a no-op re-migrate) is still there.
    let dir = std::env::temp_dir();
    let path = dir.join(format!("zim-test-{}.sqlite", uuid::Uuid::new_v4()));

    let node_id = {
        let store = SqlitePersistence::open(&path).unwrap();
        store.migrate().unwrap();
        let node = sample_node("persistent");
        store.put_node(&node).unwrap();
        node.id
    };

    {
        let store = SqlitePersistence::open(&path).unwrap();
        // Re-migrating an already-migrated store is a no-op.
        store.migrate().unwrap();
        let node = store.get_node(&node_id).unwrap().unwrap();
        assert_eq!(node.name, "persistent");
    }

    let _ = std::fs::remove_file(&path);
    // WAL sidecar files too, if present.
    let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
}

#[test]
fn port_trait_object_dispatch_works() {
    // The engine will hold the adapter as `dyn Persistence`; verify
    // the port is object-safe and dispatchable.
    let store = migrated_store();
    let port: Box<dyn Persistence> = Box::new(store);
    let node = sample_node("via-trait");
    port.put_node(&node).unwrap();
    assert_eq!(port.get_node(&node.id).unwrap().unwrap(), node);
    // Migration 0006 (metrics_samples) is the current head.
    assert_eq!(port.schema_version().unwrap(), 6);
}

#[test]
fn create_allocation_assigns_fresh_autoincrement_ids() {
    // The M5 API's node-allocation endpoint creates rows through
    // `create_allocation` (NULL id + RETURNING) because
    // `put_allocation` requires an explicit id for upserts.
    let store = migrated_store();
    let node = sample_node("node-a");
    store.put_node(&node).unwrap();

    let first = store
        .create_allocation(
            &node.id,
            "127.0.0.1".parse().unwrap(),
            25565,
            Some("primary"),
        )
        .unwrap();
    let second = store
        .create_allocation(&node.id, "127.0.0.1".parse().unwrap(), 25566, None)
        .unwrap();
    assert_ne!(first.0, second.0, "fresh rows get distinct ids");

    let stored = store.get_allocation(&second).unwrap().unwrap();
    assert_eq!(stored.port, 25566);
    assert_eq!(stored.assigned_to, None);
    assert_eq!(stored.note, None);

    // The (node, ip, port) uniqueness is enforced by the store.
    let dup = store
        .create_allocation(&node.id, "127.0.0.1".parse().unwrap(), 25566, None)
        .unwrap_err();
    assert!(matches!(
        dup,
        zim_engine_application::ports::PersistenceError::Backend(_)
    ));
}

// ── Eggs (M9) ─────────────────────────────────────────────────────

#[test]
fn egg_round_trips_with_variables_and_satellite_payloads() {
    use zim_engine_domain::{
        Egg, EggConfig, EggFeatures, EggImage, EggScript, EggScripts, EggVariable, EggVariableId,
        FileParserRule, ParserKind, ReplaceRule, StopConfig,
    };

    let store = migrated_store();
    let now = chrono::Utc::now();
    let egg = Egg {
        id: EggId::new(),
        legacy_uuid: Some("6ba7b810-9dad-11d1-80b4-00c04fd430c9".to_owned()),
        name: "Roundtrip Egg".to_owned(),
        description: "Everything must survive the store.".to_owned(),
        author: "zim@local".to_owned(),
        images: vec![EggImage {
            label: "Java 21".to_owned(),
            image: "eclipse-temurin:21-jre".parse().unwrap(),
        }],
        startup: CommandTemplate::new("java -jar {{SERVER_JARFILE}}"),
        features: EggFeatures {
            eula: true,
            privileged_install_scripts: false,
            requires_root: false,
        },
        scripts: EggScripts {
            install: Some(EggScript {
                container: "debian:12-slim".parse().unwrap(),
                entrypoint: "bash".to_owned(),
                script: "#!/bin/bash\ntrue\n".to_owned(),
            }),
            update: None,
        },
        config: EggConfig {
            files: vec![FileParserRule {
                file: "server.properties".to_owned(),
                parser: ParserKind::Properties,
                replace: vec![ReplaceRule {
                    r#match: "server-port".to_owned(),
                    value: "{{SERVER_PORT}}".to_owned(),
                }],
            }],
            startup_done: Some("Done!".to_owned()),
            stop: Some(StopConfig {
                command: "stop".to_owned(),
                signal: false,
            }),
        },
        variables: vec![EggVariable {
            id: EggVariableId::new(),
            env_name: "SERVER_JARFILE".to_owned(),
            name: "Jar".to_owned(),
            description: "Which jar".to_owned(),
            default_value: "server.jar".to_owned(),
            user_viewable: true,
            user_editable: true,
            rules: vec!["required".to_owned()],
        }],
        created_at: now,
        updated_at: now,
    };
    egg.validate().unwrap();

    store.put_egg(&egg).unwrap();
    let read = store.get_egg(&egg.id).unwrap().expect("the egg row exists");
    assert_eq!(read, egg);

    // The legacy-UUID lookup agrees.
    let by_uuid = store
        .get_egg_by_legacy_uuid("6ba7b810-9dad-11d1-80b4-00c04fd430c9")
        .unwrap()
        .expect("the legacy-UUID lookup finds it");
    assert_eq!(by_uuid.id, egg.id);

    // The listing carries it (with variables attached).
    let listed = store.list_eggs().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].variables.len(), 1);

    // A re-put with one fewer variable replaces the projection
    // wholesale (no orphaned rows).
    let mut trimmed = egg.clone();
    trimmed.variables.clear();
    trimmed.name = "Trimmed".to_owned();
    store.put_egg(&trimmed).unwrap();
    let read = store.get_egg(&egg.id).unwrap().unwrap();
    assert_eq!(read.variables.len(), 0);
    assert_eq!(read.name, "Trimmed");

    // Deletion works while nothing references the egg.
    store.delete_egg(&egg.id).unwrap();
    assert!(store.get_egg(&egg.id).unwrap().is_none());
    assert!(store.list_eggs().unwrap().is_empty());

    // An instance referencing the egg blocks the delete (the FK is
    // the backstop).
    let node = sample_node("egg-fk");
    store.put_node(&node).unwrap();
    let user = sample_user("egg-owner@example.org");
    store.put_user(&user).unwrap();
    let allocation = sample_allocation(&node, 41000);
    store.put_allocation(&allocation).unwrap();
    let mut instance = sample_instance(&node, &user, &allocation);
    instance.egg_id = egg.id;
    store.put_instance(&instance).unwrap();
    store.put_egg(&egg).unwrap();
    let blockers = store.list_instances_by_egg(&egg.id).unwrap();
    assert_eq!(blockers.len(), 1);
    assert!(store.delete_egg(&egg.id).is_err());
}

#[test]
fn instance_variables_upsert_and_round_trip() {
    // M10: per-instance variable overrides — upserts by env name,
    // ordered reads, and the delete cascade rides the instance's
    // ON DELETE CASCADE (verified in the importer's E2E suite).
    let store = migrated_store();
    let node = sample_node("overrides");
    store.put_node(&node).unwrap();
    let user = sample_user("override-owner");
    store.put_user(&user).unwrap();
    let allocation = sample_allocation(&node, 24565);
    store.put_allocation(&allocation).unwrap();
    let instance = sample_instance(&node, &user, &allocation);
    store.put_instance(&instance).unwrap();

    // Empty writes are no-ops.
    store.put_instance_variables(&instance.id, &[]).unwrap();
    assert!(store
        .list_instance_variables(&instance.id)
        .unwrap()
        .is_empty());

    // Upsert per env name, in arbitrary order.
    store
        .put_instance_variables(
            &instance.id,
            &[
                zim_engine_domain::InstanceVariable {
                    env_name: "SERVER_JARFILE".to_owned(),
                    value: "paper.jar".to_owned(),
                },
                zim_engine_domain::InstanceVariable {
                    env_name: "MINECRAFT_VERSION".to_owned(),
                    value: "1.20.1".to_owned(),
                },
            ],
        )
        .unwrap();
    // The same name again: the value is replaced, not duplicated.
    store
        .put_instance_variables(
            &instance.id,
            &[zim_engine_domain::InstanceVariable {
                env_name: "SERVER_JARFILE".to_owned(),
                value: "paper-2.jar".to_owned(),
            }],
        )
        .unwrap();

    // Reads come back ordered by env name.
    assert_eq!(
        store.list_instance_variables(&instance.id).unwrap(),
        vec![
            zim_engine_domain::InstanceVariable {
                env_name: "MINECRAFT_VERSION".to_owned(),
                value: "1.20.1".to_owned(),
            },
            zim_engine_domain::InstanceVariable {
                env_name: "SERVER_JARFILE".to_owned(),
                value: "paper-2.jar".to_owned(),
            },
        ]
    );

    // Another instance's overrides are invisible.
    let other_allocation = sample_allocation(&node, 24566);
    store.put_allocation(&other_allocation).unwrap();
    let mut other = sample_instance(&node, &user, &other_allocation);
    other.external_id = None;
    store.put_instance(&other).unwrap();
    store
        .put_instance_variables(
            &other.id,
            &[zim_engine_domain::InstanceVariable {
                env_name: "SERVER_JARFILE".to_owned(),
                value: "other.jar".to_owned(),
            }],
        )
        .unwrap();
    let overrides = store.list_instance_variables(&instance.id).unwrap();
    assert!(overrides
        .iter()
        .all(|variable| variable.value != "other.jar"));
}

#[test]
fn recents_roundtrip_rank_counts_and_isolation() {
    // M23: the NTP quick-access state — one row per (user, instance),
    // ranked by recency, counted per open, isolated per user.
    let store = migrated_store();
    let node = sample_node("recents-node");
    store.put_node(&node).unwrap();
    let owner = sample_user("recents@example.org");
    store.put_user(&owner).unwrap();
    let allocation = sample_allocation(&node, 25565);
    store.put_allocation(&allocation).unwrap();
    let mut a = sample_instance(&node, &owner, &allocation);
    let mut b = sample_instance(&node, &owner, &allocation);
    // `external_id` is UNIQUE; the samples share the placeholder.
    a.external_id = Some("recents-a".to_owned());
    b.external_id = Some("recents-b".to_owned());
    store.put_instance(&a).unwrap();
    store.put_instance(&b).unwrap();

    // Open A twice, then B once: B ranks first (recency), A keeps
    // its count.
    let t0 = chrono::Utc::now();
    store.record_recent_server(&owner.id, &a.id, t0).unwrap();
    store
        .record_recent_server(&owner.id, &a.id, t0 + chrono::Duration::seconds(5))
        .unwrap();
    store
        .record_recent_server(&owner.id, &b.id, t0 + chrono::Duration::seconds(10))
        .unwrap();

    let rows = store.list_recent_servers(&owner.id, 8).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].instance_id, b.id);
    assert_eq!(rows[0].open_count, 1);
    assert_eq!(rows[1].instance_id, a.id);
    assert_eq!(rows[1].open_count, 2);
    assert_eq!(rows[1].user_id, owner.id);
    assert!(rows[0].last_opened_at >= rows[1].last_opened_at);

    // The limit is honored.
    assert_eq!(store.list_recent_servers(&owner.id, 1).unwrap().len(), 1);

    // Another user sees nothing (user-scoped state).
    let other = sample_user("other@example.org");
    store.put_user(&other).unwrap();
    assert!(store.list_recent_servers(&other.id, 8).unwrap().is_empty());
}

#[test]
fn recents_cap_keeps_the_newest_32() {
    // Quick access, not a history database (§11): the store trims
    // to the newest 32 per user on every record.
    let store = migrated_store();
    let node = sample_node("cap-node");
    store.put_node(&node).unwrap();
    let owner = sample_user("cap@example.org");
    store.put_user(&owner).unwrap();
    let allocation = sample_allocation(&node, 25566);
    store.put_allocation(&allocation).unwrap();
    let base = chrono::Utc::now();
    for i in 0..40 {
        let mut instance = sample_instance(&node, &owner, &allocation);
        instance.external_id = None; // UNIQUE; the placeholder would collide.
        store.put_instance(&instance).unwrap();
        store
            .record_recent_server(&owner.id, &instance.id, base + chrono::Duration::seconds(i))
            .unwrap();
    }
    let rows = store.list_recent_servers(&owner.id, 64).unwrap();
    assert_eq!(rows.len(), 32);
    // The newest opening survived the trim; the oldest did not.
    assert_eq!(rows[0].open_count, 1);
    assert!(rows[0].last_opened_at > rows[31].last_opened_at);
}

// ── Metrics samples (M30 telemetry ingestion) ─────────────────────────────

#[test]
fn metrics_samples_round_trip() {
    let store = migrated_store();
    let node = sample_node("metrics-node");
    let owner = sample_user("metrics@example.org");
    let allocation = sample_allocation(&node, 25570);
    let instance = sample_instance(&node, &owner, &allocation);
    store.put_node(&node).unwrap();
    store.put_user(&owner).unwrap();
    store.put_allocation(&allocation).unwrap();
    store.put_instance(&instance).unwrap();

    let base = chrono::Utc::now();
    let samples: Vec<zim_engine_domain::MetricsSample> = (0..5)
        .map(|index| zim_engine_domain::MetricsSample {
            instance_id: instance.id,
            ts: base + chrono::Duration::seconds(10 * index),
            cpu_usage_total_ns: Some(index as u64 * 100_000_000),
            system_cpu_total_ns: Some(index as u64 * 1_000_000_000),
            online_cpus: Some(4),
            memory_usage_bytes: Some(1024 * 1024 * (index as u64 + 1)),
            memory_limit_bytes: Some(1024 * 1024 * 8),
            pids_current: Some(index as u64 % 3 + 1),
            network_rx_bytes: Some(index as u64 * 2048),
            network_tx_bytes: Some(index as u64 * 512),
        })
        .collect();
    for sample in &samples {
        store.append_metrics_sample(sample).unwrap();
    }

    // The full range round-trips field-for-field, oldest first.
    let rows = store
        .list_metrics_history(
            &instance.id,
            base - chrono::Duration::seconds(60),
            base + chrono::Duration::seconds(120),
            100,
        )
        .unwrap();
    assert_eq!(rows, samples);

    // The window bounds are inclusive and the ordering is
    // ascending by timestamp.
    let middle = store
        .list_metrics_history(
            &instance.id,
            base + chrono::Duration::seconds(10),
            base + chrono::Duration::seconds(20),
            100,
        )
        .unwrap();
    assert_eq!(middle.len(), 2);
    assert_eq!(middle[0].ts, samples[1].ts);
    assert_eq!(middle[1].ts, samples[2].ts);

    // The row limit caps the fetch (a window wider than the cap
    // yields exactly the limit, not an error).
    let capped = store
        .list_metrics_history(
            &instance.id,
            base - chrono::Duration::seconds(60),
            base + chrono::Duration::seconds(120),
            3,
        )
        .unwrap();
    assert_eq!(capped.len(), 3);

    // A re-observed timestamp replaces, it does not duplicate.
    let mut revision = samples[2];
    revision.memory_usage_bytes = Some(999);
    store.append_metrics_sample(&revision).unwrap();
    let rows = store
        .list_metrics_history(
            &instance.id,
            base - chrono::Duration::seconds(60),
            base + chrono::Duration::seconds(120),
            100,
        )
        .unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[2].memory_usage_bytes, Some(999));

    // Retention prunes only the older side, across every instance.
    let removed = store
        .prune_metrics_before(base + chrono::Duration::seconds(15))
        .unwrap();
    assert_eq!(removed, 2);
    let rows = store
        .list_metrics_history(
            &instance.id,
            base - chrono::Duration::seconds(60),
            base + chrono::Duration::seconds(120),
            100,
        )
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows[0].ts >= base + chrono::Duration::seconds(20));
}

#[test]
fn deleting_an_instance_cascades_its_metrics_samples() {
    let store = migrated_store();
    let node = sample_node("cascade-node");
    let owner = sample_user("cascade@example.org");
    let allocation = sample_allocation(&node, 25571);
    let instance = sample_instance(&node, &owner, &allocation);
    store.put_node(&node).unwrap();
    store.put_user(&owner).unwrap();
    store.put_allocation(&allocation).unwrap();
    store.put_instance(&instance).unwrap();

    let base = chrono::Utc::now();
    for index in 0..3 {
        store
            .append_metrics_sample(&zim_engine_domain::MetricsSample {
                instance_id: instance.id,
                ts: base + chrono::Duration::seconds(index * 10),
                memory_usage_bytes: Some(1024),
                ..zim_engine_domain::MetricsSample::default()
            })
            .unwrap();
    }
    assert_eq!(
        store
            .list_metrics_history(
                &instance.id,
                base - chrono::Duration::seconds(60),
                base + chrono::Duration::seconds(60),
                100
            )
            .unwrap()
            .len(),
        3
    );

    // The same ON DELETE CASCADE posture every per-instance table
    // carries: the samples go with the instance.
    store.delete_instance(&instance.id).unwrap();
    assert_eq!(
        store
            .list_metrics_history(
                &instance.id,
                base - chrono::Duration::seconds(60),
                base + chrono::Duration::seconds(60),
                100
            )
            .unwrap()
            .len(),
        0
    );
}
