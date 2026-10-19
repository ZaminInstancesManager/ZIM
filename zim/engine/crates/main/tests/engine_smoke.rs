//! End-to-end smoke test for the composed engine binary library.
//!
//! Boots the real `run()` composition (SQLite file, migrations,
//! bootstrap admin, persisted loopback certificate, HTTPS listener),
//! logs in as the bootstrap admin, and shuts down cleanly.

use std::time::Duration;

use serde_json::json;

#[tokio::test]
async fn engine_boots_bootstraps_and_serves_https() {
    // A private data dir per run (tempfile would be nicer; std keeps
    // the dev-dep set minimal for this crate).
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let data_dir =
        std::env::temp_dir().join(format!("zim_engine_smoke_{}_{}", std::process::id(), stamp));
    let data_dir = data_dir.to_str().unwrap().to_owned();

    let options = zim_engine_main::EngineOptions {
        data_dir: data_dir.clone().into(),
        // Ephemeral port: run() returns the bound address.
        port: 0,
        bootstrap_admin_email: "smoke@zim.local".to_owned(),
        bootstrap_admin_password: Some("smoke test password".to_owned()),
        // M28: the SFTP transport rides along (ephemeral port too).
        sftp: zim_engine_sftp::SftpOptions {
            enabled: true,
            bind: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: 0,
        },
        metrics: Default::default(),
    };

    let engine = tokio::time::timeout(Duration::from_secs(20), zim_engine_main::startup(options))
        .await
        .expect("engine boots within 20s")
        .expect("engine starts serving");
    let base = format!("https://127.0.0.1:{}/api/v1", engine.local_addr.port());
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();

    // Health.
    let response = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(response.status(), 200);

    // The bootstrap admin can log in.
    let response = client
        .post(format!("{base}/auth/login"))
        .json(&json!({ "email": "smoke@zim.local", "password": "smoke test password" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let cookie = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .find_map(|v| {
            v.to_str()
                .ok()?
                .strip_prefix("zim_session=")?
                .split(';')
                .next()
                .map(str::to_owned)
        })
        .expect("session cookie set");
    let response = client
        .get(format!("{base}/users/me"))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["root_admin"], true);

    // The frontend loader serves the bundled default package's
    // surfaces (M11): the newtab page under the §3.8 CSP.
    let pages_base = format!("https://127.0.0.1:{}/zim", engine.local_addr.port());
    let response = client
        .get(format!("{pages_base}/newtab"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    assert!(response.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("default-src 'none'"));
    assert!(response.text().await.unwrap().contains("ZIM"));

    // The active-frontend info reports the bundled default.
    let response = client
        .get(format!("{base}/frontend/active"))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["id"], "org.zaminmc.frontend.default");
    assert_eq!(body["is_default"], true);

    // The data dir holds the database and the TLS PEM files.
    assert!(std::path::Path::new(&data_dir).join("engine.db").exists());
    assert!(std::path::Path::new(&data_dir)
        .join("loopback-cert.pem")
        .exists());
    assert!(std::path::Path::new(&data_dir)
        .join("loopback-key.pem")
        .exists());

    // M28: the SFTP listener answers with the SSH identification
    // banner on its own port (the full protocol round-trip lives in
    // the sftp crate's own suite; this is the composition proof).
    let sftp_port = discovery_placeholder::sftp_port_of(&engine, &data_dir);
    let banner = ssh_banner(sftp_port).await;
    assert!(banner.starts_with("SSH-2.0-"), "the banner was {banner:?}");
    assert!(banner.contains("ZaminZIM"));

    // M21: the shell-discovery file records the bound port, PID,
    // and version while the engine runs.
    let discovery = zim_engine_main::discovery::read(std::path::Path::new(&data_dir))
        .expect("discovery file exists while the engine runs");
    assert_eq!(discovery.schema, 1);
    assert_eq!(discovery.port, engine.local_addr.port());
    assert!(discovery.pid != 0);
    assert_eq!(discovery.engine_version, env!("CARGO_PKG_VERSION"));
    assert!(discovery.started_at_unix > 0);
    assert_eq!(discovery.sftp_port, Some(sftp_port));
    // The frontend state (the active.json record) lives under
    // the loader's data subdir.
    assert!(std::path::Path::new(&data_dir)
        .join("frontend")
        .join("active.json")
        .exists());

    // Clean shutdown removes the discovery file (M21), then remove
    // the scratch dir (best-effort).
    engine.shutdown().await;
    assert!(
        zim_engine_main::discovery::read(std::path::Path::new(&data_dir)).is_none(),
        "discovery file removed on graceful shutdown"
    );
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// Read the discovery file's SFTP port while the engine runs (the
/// RunningEngine does not expose the SFTP listener's address — the
/// discovery file is its public carrier).
mod discovery_placeholder {
    use std::path::Path;

    pub fn sftp_port_of(_engine: &zim_engine_main::RunningEngine, data_dir: &str) -> u16 {
        zim_engine_main::discovery::read(Path::new(data_dir))
            .and_then(|d| d.sftp_port)
            .expect("the discovery file carries the sftp port")
    }
}

/// Read the SSH identification banner one TCP connection receives.
async fn ssh_banner(port: u16) -> String {
    use tokio::io::AsyncReadExt;
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .expect("the sftp connect resolves")
    .expect("the sftp listener accepts");
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        let n = tokio::time::timeout(std::time::Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .expect("the banner read resolves")
            .expect("the banner read succeeds");
        buffer.extend_from_slice(&chunk[..n]);
        if buffer.contains(&b'\n') || n == 0 {
            break;
        }
    }
    String::from_utf8_lossy(&buffer)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

/// M28's degrade discipline: an SFTP port already owned by another
/// process (another engine's transport, an operator's listener) must
/// never brick the boot — the engine comes up healthy, serves HTTPS,
/// and the discovery file simply carries no `sftp_port`.
#[tokio::test]
async fn a_busy_sftp_port_degrades_instead_of_failing_the_boot() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let data_dir = std::env::temp_dir().join(format!(
        "zim_engine_smoke_sftpbusy_{}_{}",
        std::process::id(),
        stamp
    ));
    let data_dir = data_dir.to_str().unwrap().to_owned();

    // Occupy an SFTP port with an unrelated listener first.
    let squatter = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let taken_port = squatter.local_addr().unwrap().port();

    let options = zim_engine_main::EngineOptions {
        data_dir: data_dir.clone().into(),
        port: 0,
        bootstrap_admin_email: "smoke@zim.local".to_owned(),
        bootstrap_admin_password: Some("smoke test password".to_owned()),
        sftp: zim_engine_sftp::SftpOptions {
            enabled: true,
            bind: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: taken_port,
        },
        metrics: Default::default(),
    };
    let engine = tokio::time::timeout(Duration::from_secs(30), zim_engine_main::startup(options))
        .await
        .expect("the engine boots despite the busy SFTP port")
        .expect("the engine starts serving");

    // The HTTPS surface is fully alive.
    let base = format!("https://127.0.0.1:{}/api/v1", engine.local_addr.port());
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let response = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(response.status(), 200);

    // The discovery file records the HTTPS port but NOT an SFTP one.
    let discovery = zim_engine_main::discovery::read(std::path::Path::new(&data_dir))
        .expect("discovery exists");
    assert_eq!(discovery.port, engine.local_addr.port());
    assert_eq!(discovery.sftp_port, None, "no sftp_port when degraded");

    // The squatter still owns the port (the engine never took it).
    let still_owner = squatter.local_addr().unwrap().port();
    assert_eq!(still_owner, taken_port);
    drop(squatter);

    engine.shutdown().await;
    let _ = std::fs::remove_dir_all(&data_dir);
}
