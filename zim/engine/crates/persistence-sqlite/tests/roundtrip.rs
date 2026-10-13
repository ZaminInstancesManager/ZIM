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
    assert_eq!(port.schema_version().unwrap(), 1);
}
