//! In-process integration suite for the SFTP transport: a real
//! russh client (real SSH handshake, real host-key exchange, real
//! password auth) driving a real SFTP session against the real
//! listener over a real (in-memory) database — no mocks anywhere in
//! the stack below the SSH wire itself.
//!
//! The suite mirrors the HTTP family's own socket-level tests: the
//! synthetic root, the full file round-trip, the §2.6 grammar, the
//! grant table, and the audit trail.

use std::sync::Arc;

use russh::client::Handler as ClientHandler;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use zim_engine_application::ports::Persistence;
use zim_engine_domain::{
    Instance, InstanceId, InstanceStatus, Node, NodeCapabilities, NodeDeployment, NodeId,
    NodeState, Permission, PermissionGrant, PermissionSet, Scheme, User, UserId,
};
use zim_engine_sftp::{start, SftpOptions};

/// A test world: database + data dir + running listener.
struct World {
    persistence: Arc<dyn Persistence>,
    data_dir: std::path::PathBuf,
    server: zim_engine_sftp::RunningSftp,
}

impl World {
    /// Boot everything on an ephemeral port with a fresh temp data
    /// dir and an in-memory database seeded with the local node.
    async fn start() -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();

        let node = Node {
            id: NodeId::new(),
            name: "local".to_owned(),
            location_id: zim_engine_domain::LocationId::new(),
            fqdn: "localhost".to_owned(),
            scheme: Scheme::Https,
            public_port: 8443,
            listen_port: 8444,
            capabilities: NodeCapabilities::default(),
            deployment: NodeDeployment::LocalLinux,
            last_heartbeat: None,
            last_seen_state: NodeState::Unknown,
        };
        persistence.put_node(&node).unwrap();

        let data_dir = std::env::temp_dir().join(format!("zim-sftp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).unwrap();

        let options = SftpOptions {
            enabled: true,
            bind: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: 0,
        };
        let server = start(Arc::clone(&persistence), data_dir.clone(), &options)
            .await
            .expect("the SFTP listener starts");
        Self {
            persistence,
            data_dir,
            server,
        }
    }

    /// Seed one user (Argon2id-hashed).
    fn seed_user(&self, username: &str, password: &str, admin: bool) -> User {
        let policy = zim_engine_auth::PasswordPolicy::default();
        let hash = zim_engine_auth::password::hash_password(&policy, password).unwrap();
        let user = User {
            id: UserId::new(),
            email: format!("{username}@zim.local"),
            username: username.to_owned(),
            external_id: None,
            password_hash: Some(hash),
            oauth_subject: None,
            is_admin: admin,
            root_admin: admin,
        };
        self.persistence.put_user(&user).unwrap();
        user
    }

    /// The next seeded allocation id (u64-keyed rows).
    fn next_allocation(&self) -> zim_engine_domain::Allocation {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(9000);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let allocation = zim_engine_domain::Allocation {
            id: zim_engine_domain::AllocationId::new(id),
            node_id: self.persistence.list_nodes().unwrap()[0].id,
            ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: (id % 60_000) as u16 + 1024,
            assigned_to: None,
            note: None,
        };
        self.persistence.put_allocation(&allocation).unwrap();
        allocation
    }

    /// Seed one instance on the local node, owned by `owner`.
    fn seed_instance(&self, name: &str, owner: &User) -> Instance {
        let (uuid, uuid_short) = Instance::legacy_uuid_pair();
        let allocation = self.next_allocation();
        let instance = Instance {
            id: InstanceId::new(),
            external_id: None,
            uuid,
            uuid_short,
            node_id: self.persistence.list_nodes().unwrap()[0].id,
            name: name.to_owned(),
            description: String::new(),
            status: InstanceStatus::Offline,
            skip_egg_scripts: false,
            owner_id: owner.id,
            limits: Default::default(),
            allocation_id: allocation.id,
            egg_id: zim_engine_domain::EggId::new(),
            startup: zim_engine_domain::CommandTemplate::new("echo hi"),
            image: "ghcr.io/parkervcp/yolks:alpine"
                .parse()
                .expect("valid image"),
            allocation_limit: None,
            database_limit: None,
            backup_limit: 5,
            crash_detection_enabled: true,
            installed_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        instance.validate().expect("valid instance");
        let mut claimed = allocation.clone();
        claimed.assigned_to = Some(instance.id);
        self.persistence.put_instance(&instance).unwrap();
        self.persistence.put_allocation(&claimed).unwrap();
        instance
    }

    /// Grant a member exactly one permission row on one instance.
    fn seed_grant(&self, user: &User, instance: &Instance, grants: &[PermissionGrant]) {
        let mut set = PermissionSet::new();
        for grant in grants {
            set.grant(*grant);
        }
        self.persistence
            .put_permission(&Permission {
                user_id: user.id,
                instance_id: instance.id,
                grants: set,
            })
            .unwrap();
    }

    fn port(&self) -> u16 {
        self.server.local_addr.port()
    }

    async fn shutdown(self) {
        self.server.shutdown().await;
        let _ = std::fs::remove_dir_all(&self.data_dir);
    }
}

/// The SFTP client's connection handler (trusts the generated host
/// key — the test proves the protocol, not the TOFU ceremony).
struct TrustingClient;

impl ClientHandler for TrustingClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Connect + authenticate + open the SFTP subsystem. Fails fast
/// (bounded waits) instead of hanging on a wedged handshake.
async fn sftp_client(world: &World, login: &str, password: &str) -> SftpSession {
    let config = Arc::new(russh::client::Config::default());
    let mut session = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        russh::client::connect(config, ("127.0.0.1", world.port()), TrustingClient),
    )
    .await
    .expect("the SSH connect resolves")
    .expect("the SSH handshake succeeds");

    let authed = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        session.authenticate_password(login, password),
    )
    .await
    .expect("the password auth resolves")
    .expect("the password auth round-trips");
    assert!(authed.success(), "password auth must succeed");

    let channel = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        session.channel_open_session(),
    )
    .await
    .expect("the channel open resolves")
    .expect("the channel opens");
    channel
        .request_subsystem(true, "sftp")
        .await
        .expect("the sftp subsystem is requested");
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        SftpSession::new(channel.into_stream()),
    )
    .await
    .expect("the SFTP init resolves")
    .expect("the SFTP session initializes")
}

/// Whether one SFTP operation failed with a specific status code.
async fn fails_with<T>(
    attempt: impl std::future::Future<Output = Result<T, russh_sftp::client::error::Error>>,
    code: russh_sftp::protocol::StatusCode,
) -> bool {
    match attempt.await {
        Err(russh_sftp::client::error::Error::Status(status)) => status.status_code == code,
        Err(other) => {
            eprintln!("expected status {code:?}, got a different error: {other:?}");
            false
        }
        // A success where the status refusal was expected.
        Ok(_) => {
            eprintln!("expected status {code:?}, got success");
            false
        }
    }
}

// ── The full round-trip ──────────────────────────────────────────

#[tokio::test]
async fn the_full_sftp_round_trip_over_the_real_protocol() {
    let world = World::start().await;
    let admin = world.seed_user("admin", "correct horse battery staple", true);
    let instance = world.seed_instance("My Server", &admin);
    world.seed_instance("Other Server", &admin);

    let sftp = sftp_client(&world, "admin", "correct horse battery staple").await;

    // The synthetic root lists every visible instance by its
    // sanitized display name.
    let root = tokio::time::timeout(std::time::Duration::from_secs(10), sftp.read_dir("."))
        .await
        .expect("the root listing resolves")
        .expect("the root listing succeeds");
    let names: Vec<String> = root.into_iter().map(|entry| entry.file_name()).collect();
    assert!(names.contains(&"My Server".to_owned()), "{names:?}");
    assert!(names.contains(&"Other Server".to_owned()), "{names:?}");

    // canonicalize(".") is the root, as every client expects.
    let home = sftp.canonicalize(".").await.expect("realpath resolves");
    assert_eq!(home, "/");

    // A fresh instance lists an empty directory (the engine created
    // its root at VFS build).
    let server_dir = "/My Server";
    let entries = sftp.read_dir(server_dir).await.expect("fresh listing");
    assert_eq!(entries.into_iter().count(), 0);

    // Create a directory, then a file with real content.
    sftp.create_dir(&format!("{server_dir}/plugins"))
        .await
        .expect("mkdir");
    let mut file = sftp
        .open_with_flags(
            &format!("{server_dir}/eula.txt"),
            OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::READ,
        )
        .await
        .expect("the file opens");
    use tokio::io::AsyncWriteExt;
    file.write_all(b"eula=true\n")
        .await
        .expect("the write lands");
    file.close().await.expect("the file closes");

    // The bytes read back unchanged (offset-driven reads).
    let content = sftp
        .read(&format!("{server_dir}/eula.txt"))
        .await
        .expect("read");
    assert_eq!(content, b"eula=true\n");

    // The listing sees the file with honest metadata.
    let entries = sftp.read_dir(server_dir).await.expect("listing");
    let names: Vec<String> = entries.into_iter().map(|e| e.file_name()).collect();
    assert!(names.contains(&"eula.txt".to_owned()), "{names:?}");
    assert!(names.contains(&"plugins".to_owned()), "{names:?}");

    // stat speaks the protocol's attribute shape.
    let metadata = sftp
        .metadata(&format!("{server_dir}/eula.txt"))
        .await
        .expect("stat works");
    assert_eq!(metadata.len(), 10);
    assert!(metadata.is_regular());

    // Rename, then the old name is gone.
    sftp.rename(
        &format!("{server_dir}/eula.txt"),
        &format!("{server_dir}/eula.txt.bak"),
    )
    .await
    .expect("rename works");
    let exists = sftp
        .try_exists(&format!("{server_dir}/eula.txt"))
        .await
        .expect("exists check works");
    assert!(!exists);

    // remove_file, then remove_dir on the now-empty directory.
    sftp.remove_file(&format!("{server_dir}/eula.txt.bak"))
        .await
        .expect("remove_file works");
    sftp.remove_dir(&format!("{server_dir}/plugins"))
        .await
        .expect("remove_dir on an empty directory works");

    // The audit trail: the login, and the write at close.
    let audit = world.persistence.list_audit(None, 500).unwrap();
    let actions: Vec<&str> = audit.iter().map(|r| r.action.as_str()).collect();
    assert!(actions.contains(&"auth.login"), "{actions:?}");
    let login = audit.iter().find(|r| r.action == "auth.login").unwrap();
    assert_eq!(login.metadata["transport"], "sftp");
    assert_eq!(login.metadata["success"], true);
    let write = audit
        .iter()
        .find(|r| r.action == "instance.file.write")
        .expect("the sftp write audited");
    assert_eq!(write.metadata["transport"], "sftp");
    assert_eq!(write.metadata["path"], "eula.txt");
    assert_eq!(write.metadata["bytes"], 10);
    assert_eq!(write.instance_id, Some(instance.id));
    let _ = world.data_dir; // kept for the shutdown cleanup
    world.shutdown().await;
}

// ── The grammar ──────────────────────────────────────────────────

#[tokio::test]
async fn the_grammar_and_the_chroot_hold() {
    let world = World::start().await;
    let admin = world.seed_user("admin", "correct horse battery staple", true);
    world.seed_instance("Target", &admin);

    let sftp = sftp_client(&world, "admin", "correct horse battery staple").await;
    let target = "/Target";

    let mut file = sftp
        .open_with_flags(
            &format!("{target}/stay.txt"),
            OpenFlags::CREATE | OpenFlags::WRITE,
        )
        .await
        .expect("the file opens");
    use tokio::io::AsyncWriteExt;
    file.write_all(b"staying").await.expect("write");
    file.close().await.expect("close");

    // Traversal through the synthetic layer is the files crate's
    // §2.6 refusal: ".." past the instance root collapses to the
    // instance root (chroot semantics), never the host filesystem.
    let listing = sftp
        .read_dir(&format!("{target}/.."))
        .await
        .expect("chroot ..");
    let names: Vec<String> = listing.into_iter().map(|e| e.file_name()).collect();
    // The instance root's own listing — "stay.txt" is there, and
    // nothing from the host.
    assert!(names.contains(&"stay.txt".to_owned()), "{names:?}");
    assert_eq!(names.len(), 1);

    // A write landing outside any instance is refused at the
    // synthetic layer (no such entry).
    assert!(
        fails_with(
            sftp.open_with_flags(
                "/NoSuchServer/escape.txt",
                OpenFlags::CREATE | OpenFlags::WRITE,
            ),
            russh_sftp::protocol::StatusCode::NoSuchFile
        )
        .await,
        "an unknown instance directory must not be writable"
    );

    // Renaming across instances is refused (a move is one operation
    // inside one chroot).
    assert!(
        fails_with(
            sftp.rename(&format!("{target}/stay.txt"), "/Elsewhere/stay.txt"),
            russh_sftp::protocol::StatusCode::NoSuchFile
        )
        .await,
        "a cross-instance rename must be refused"
    );

    world.shutdown().await;
}

// ── The grant table ──────────────────────────────────────────────

#[tokio::test]
async fn the_grant_table_governs_the_protocol() {
    let world = World::start().await;
    let admin = world.seed_user("admin", "correct horse battery staple", true);
    let member = world.seed_user("member", "watchword-protocol", false);
    world.seed_user("stranger", "watchword-protocol", false);
    let instance = world.seed_instance("Grants Target", &admin);

    // Read-only member: list + read pass; writes and mkdir refuse.
    world.seed_grant(
        &member,
        &instance,
        &[PermissionGrant::FileRead, PermissionGrant::FileReadContent],
    );
    // The admin seeds one file directly into the instance tree (the
    // member is read-only by design and must not be able to seed).
    let instance_root = world.data_dir.join("instances").join(&instance.uuid_short);
    std::fs::create_dir_all(&instance_root).unwrap();
    std::fs::write(instance_root.join("seed.txt"), b"seed").unwrap();

    let sftp = sftp_client(&world, "member", "watchword-protocol").await;
    let target = "/Grants Target";

    // The member sees exactly one directory (the instance), and can
    // list + read inside it.
    let root = sftp.read_dir(".").await.expect("the member's root");
    assert_eq!(root.into_iter().count(), 1);

    let content = sftp
        .read(&format!("{target}/seed.txt"))
        .await
        .expect("read");
    assert_eq!(content, b"seed");

    assert!(
        fails_with(
            sftp.open_with_flags(
                &format!("{target}/denied.txt"),
                OpenFlags::CREATE | OpenFlags::WRITE,
            ),
            russh_sftp::protocol::StatusCode::PermissionDenied
        )
        .await,
        "a read-only member must not write"
    );
    assert!(
        fails_with(
            sftp.create_dir(&format!("{target}/denied-dir")),
            russh_sftp::protocol::StatusCode::PermissionDenied
        )
        .await,
        "a read-only member must not mkdir"
    );
    assert!(
        fails_with(
            sftp.remove_file(&format!("{target}/seed.txt")),
            russh_sftp::protocol::StatusCode::PermissionDenied
        )
        .await,
        "a read-only member must not delete"
    );

    // A stranger's root is empty — the instance is uniformly
    // invisible, not merely unwritable.
    let stranger_sftp = sftp_client(&world, "stranger", "watchword-protocol").await;
    let root = stranger_sftp
        .read_dir(".")
        .await
        .expect("the stranger's root");
    assert_eq!(root.into_iter().count(), 0);
    assert!(
        fails_with(
            stranger_sftp.read_dir(target),
            russh_sftp::protocol::StatusCode::NoSuchFile
        )
        .await,
        "the stranger must not even see the instance directory"
    );

    world.shutdown().await;
}

// ── Authentication ──────────────────────────────────────────────

#[tokio::test]
async fn wrong_passwords_fail_and_lock_out() {
    let world = World::start().await;
    world.seed_user("admin", "correct horse battery staple", true);

    // One wrong password: a real handshake, a real rejection.
    let config = Arc::new(russh::client::Config::default());
    let mut session = russh::client::connect(config, ("127.0.0.1", world.port()), TrustingClient)
        .await
        .expect("the handshake itself succeeds");
    let authed = session
        .authenticate_password("admin", "wrong password")
        .await
        .expect("the rejection round-trips");
    assert!(!authed.success(), "the wrong password must fail");

    // Five more wrong attempts lock the username out; the seventh
    // (correct!) password is then refused too — the throttle is the
    // brute-force brake, not the password check.
    for _ in 0..5 {
        let config = Arc::new(russh::client::Config::default());
        let mut session =
            russh::client::connect(config, ("127.0.0.1", world.port()), TrustingClient)
                .await
                .expect("the handshake succeeds");
        let authed = session
            .authenticate_password("admin", "still wrong")
            .await
            .expect("the rejection round-trips");
        assert!(!authed.success());
    }
    let config = Arc::new(russh::client::Config::default());
    let mut session = russh::client::connect(config, ("127.0.0.1", world.port()), TrustingClient)
        .await
        .expect("the handshake succeeds");
    let authed = session
        .authenticate_password("admin", "correct horse battery staple")
        .await
        .expect("the locked-out answer round-trips");
    assert!(
        !authed.success(),
        "a locked-out username refuses even the correct password"
    );

    // The audit trail carries the failed logins with the presented
    // name and no actor (no account enumeration in the trail): the
    // six wrong attempts PLUS the locked-out refusal of the correct
    // password — the attempt happened, so the trail records it.
    let audit = world.persistence.list_audit(None, 500).unwrap();
    let failures: Vec<_> = audit
        .iter()
        .filter(|r| r.action == "auth.login" && r.metadata["success"] == false)
        .collect();
    assert_eq!(failures.len(), 7, "every failed attempt audited");
    assert!(failures.iter().all(|r| r.actor_id.is_none()));

    world.shutdown().await;
}
