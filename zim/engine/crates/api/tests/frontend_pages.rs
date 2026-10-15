//! M11 integration tests: the §10.14 frontend-package endpoints and
//! the `/zim/` page surface, over a real HTTPS engine.
//!
//! Covers the acceptance surface of the milestone:
//!
//! - The engine serves the default frontend's pages at `/zim/`
//!   (the HTTP form of the `zim://` origin) with the §3.8 CSP.
//! - A custom package that fails its smoke gate is rejected on
//!   activation (422 `frontend_smoke_test_failed`); the default is
//!   kept.
//! - The admin gates, the upload→activate→reset lifecycle, the
//!   approval requirement for unsigned packages, and ETag
//!   revalidation.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use serde_json::json;
use zim_engine_api::tls::{generate_loopback_certificate, serve_https, TlsMaterial};
use zim_engine_api::{ApiConfig, AppState};
use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::Supervisor;
use zim_engine_auth::{ApiKeyManager, SessionManager};
use zim_engine_domain::User;
use zim_engine_frontend_loader::manifest::{FrontendManifest, SignatureInfo, MANIFEST_FILE};
use zim_engine_frontend_loader::signature::{dev_sign, public_key_of, DEV_SIGNING_KEY};
use zim_engine_frontend_loader::FrontendService;

struct TestEngine {
    base: String,
    pages_base: String,
    client: reqwest::Client,
    persistence: Arc<dyn Persistence>,
    _frontend_dir: std::path::PathBuf,
}

impl TestEngine {
    async fn start(frontend_dir: std::path::PathBuf) -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();

        // A supervisor over the always-running mock runtime: the
        // console/list endpoints need supervised state.
        let supervisor = {
            use zim_engine_application::supervision::testing::{MockRuntime, MockRuntimeFactory};
            use zim_engine_application::supervision::SupervisionPolicy;
            let node = zim_engine_domain::Node {
                id: zim_engine_domain::NodeId::new(),
                name: "local-test".to_owned(),
                location_id: zim_engine_domain::LocationId::new(),
                fqdn: "localhost".to_owned(),
                scheme: zim_engine_domain::Scheme::Https,
                public_port: 8443,
                listen_port: 8444,
                capabilities: Default::default(),
                deployment: zim_engine_domain::NodeDeployment::LocalLinux,
                last_heartbeat: None,
                last_seen_state: Default::default(),
            };
            persistence.put_node(&node).unwrap();
            let supervisor = Supervisor::new(
                Arc::clone(&persistence),
                Arc::new(MockRuntimeFactory::new(MockRuntime::always_running())),
                SupervisionPolicy {
                    monitor_poll_interval: std::time::Duration::from_millis(50),
                    ..Default::default()
                },
                std::env::temp_dir().join("zim-frontend-api-test"),
            );
            supervisor.load_local().await.unwrap();
            supervisor
        };

        let jobs = zim_engine_jobs::JobSystem::new(
            Arc::clone(&persistence),
            None,
            zim_engine_jobs::JobPolicy {
                poll_interval: std::time::Duration::from_millis(20),
                cleaner_interval: std::time::Duration::from_millis(50),
                max_concurrent: 2,
                shutdown_grace: std::time::Duration::from_secs(5),
            },
        );
        let data_root = std::env::temp_dir().join("zim-frontend-api-test");
        let backups = zim_engine_backups::BackupService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            data_root.clone(),
        );
        backups.register().await;
        let transfers = zim_engine_transfers::TransferService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            Arc::clone(&supervisor),
            data_root.clone(),
        );
        transfers.register().await;
        let installs = zim_engine_installs::InstallService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            Arc::clone(&supervisor),
            Arc::new(
                zim_engine_application::supervision::testing::MockInstallRunner::always_succeeds(),
            ),
            data_root.clone(),
        );
        installs.register().await;
        jobs.start();
        let scheduler =
            zim_engine_scheduler::Scheduler::new(Arc::clone(&persistence), Arc::clone(&supervisor));

        let frontend = Arc::new(FrontendService::new(
            frontend_dir.clone(),
            semver::Version::new(0, 0, 1),
            "v1.0.0",
        ));
        // M24: the addon system + theme registry over the same data
        // root (fresh temp dir = the bundled defaults seeded).
        let extensions = Arc::new(zim_engine_extensions::ExtensionsService::new(
            Arc::clone(&persistence),
            Arc::clone(&frontend),
            &frontend_dir,
            semver::Version::new(0, 0, 1),
            "v1.0.0",
        ));
        let themes = Arc::new(zim_engine_extensions::ThemeService::new(&frontend_dir));
        let state = Arc::new(AppState::new(
            Arc::clone(&persistence),
            supervisor,
            Arc::new(SessionManager::new(Default::default())),
            Arc::new(ApiKeyManager::new()),
            Arc::new(zim_engine_auth::TabTokenManager::new(Default::default())),
            Arc::clone(&jobs),
            Arc::clone(&backups),
            Arc::clone(&transfers),
            Arc::clone(&installs),
            Arc::clone(&scheduler),
            Arc::clone(&frontend),
            Arc::clone(&extensions),
            Arc::clone(&themes),
            ApiConfig::default(),
        ));
        let router = zim_engine_api::router(Arc::clone(&state));
        let tls: TlsMaterial = generate_loopback_certificate().unwrap();
        let server = serve_https(router, IpAddr::V4(Ipv4Addr::LOCALHOST), 0, &tls)
            .await
            .unwrap();
        let port = server.local_addr.port();
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();
        TestEngine {
            base: format!("https://127.0.0.1:{port}/api/v1"),
            pages_base: format!("https://127.0.0.1:{port}/zim"),
            client,
            persistence,
            _frontend_dir: frontend_dir,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn page_url(&self, path: &str) -> String {
        format!("{}{}", self.pages_base, path)
    }

    /// Seed a user directly in persistence (Argon2id-hashed).
    fn seed_user(&self, email: &str, password: &str, admin: bool) {
        let policy = zim_engine_auth::PasswordPolicy::default();
        let hash = zim_engine_auth::password::hash_password(&policy, password).unwrap();
        let user = User {
            id: zim_engine_domain::UserId::new(),
            email: email.to_owned(),
            username: email.split('@').next().unwrap().to_owned(),
            external_id: None,
            password_hash: Some(hash),
            oauth_subject: None,
            is_admin: admin,
            root_admin: admin,
        };
        self.persistence.put_user(&user).unwrap();
    }

    /// Login helper returning the session cookie.
    async fn login(&self, email: &str, password: &str) -> String {
        let response = self
            .client
            .post(self.url("/auth/login"))
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await
            .unwrap();
        response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .find_map(|v| {
                let raw = v.to_str().ok()?;
                let token = raw.strip_prefix("zim_session=")?.split(';').next()?;
                Some(token.to_owned())
            })
            .expect("login sets the session cookie")
    }
}

fn temp_frontend_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("zim-frontend-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build a manifest for a minimal custom package.
fn custom_manifest(id: &str, inline_script: bool, signed: bool) -> FrontendManifest {
    let mut manifest = FrontendManifest {
        id: id.to_owned(),
        manifest_version: 1,
        version: "0.1.0".to_owned(),
        name: format!("Custom {id}"),
        description: "an integration-test package".to_owned(),
        author: "Example".to_owned(),
        license: "MIT".to_owned(),
        api_version: "v1".to_owned(),
        engines: std::collections::BTreeMap::from([("zim".to_owned(), ">=0.0.1".to_owned())]),
        full_replacement: false,
        smoke_test: Some("pages/smoke.html".to_owned()),
        page_surfaces: vec!["zim://newtab".to_owned()],
        entrypoints: std::collections::BTreeMap::from([(
            "newtab".to_owned(),
            "pages/newtab.html".to_owned(),
        )]),
        assets: vec!["assets/api.js".to_owned()],
        permissions: vec![],
        accessibility: None,
        signature: None,
    };
    if signed {
        manifest.signature = Some(SignatureInfo {
            algorithm: "ed25519".to_owned(),
            public_key: public_key_of(DEV_SIGNING_KEY),
            value: dev_sign(&manifest),
        });
    }
    let _ = inline_script; // Selected by the page body below.
    manifest
}

/// Zip helper: (path, bytes) pairs → archive bytes.
fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buffer = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
        let options = zip::write::SimpleFileOptions::default();
        for (path, bytes) in files {
            writer.start_file(*path, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }
    buffer
}

/// A complete, gate-passing custom package archive.
fn good_custom_zip(id: &str, signed: bool) -> Vec<u8> {
    let manifest = custom_manifest(id, false, signed);
    make_zip(&[
        (MANIFEST_FILE, &serde_json::to_vec(&manifest).unwrap()),
        (
            "pages/newtab.html",
            br#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>custom</title>
<script type="module" src="/zim/assets/api.js"></script>
</head><body>custom-frontend-marker</body></html>"#,
        ),
        (
            "pages/smoke.html",
            br#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>smoke</title>
<script type="module" src="/zim/assets/api.js"></script>
</head><body></body></html>"#,
        ),
        (
            "assets/api.js",
            b"export const whoami = () => fetch('/api/v1/users/me');",
        ),
    ])
}

/// A package whose entrypoint carries an inline script (§3.8
/// violation): stored on upload, refused on activation.
fn broken_custom_zip(id: &str) -> Vec<u8> {
    let manifest = custom_manifest(id, true, true);
    make_zip(&[
        (MANIFEST_FILE, &serde_json::to_vec(&manifest).unwrap()),
        (
            "pages/newtab.html",
            br#"<html><head></head><body><script>inline()</script></body></html>"#,
        ),
        (
            "pages/smoke.html",
            br#"<html><head></head><body></body></html>"#,
        ),
        (
            "assets/api.js",
            b"export const whoami = () => fetch('/api/v1/users/me');",
        ),
    ])
}

// ── Page serving ──────────────────────────────────────────────────

#[tokio::test]
async fn pages_serve_the_default_frontend_with_csp() {
    let engine = TestEngine::start(temp_frontend_dir()).await;

    // The newtab surface.
    let response = engine
        .client
        .get(engine.page_url("/newtab"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    let csp = response.headers()["content-security-policy"]
        .to_str()
        .unwrap();
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("script-src 'self'"));
    // M22 (AD-24): the app chrome's same-origin iframe panes.
    assert!(csp.contains("frame-src 'self'"));
    assert!(csp.contains("frame-ancestors 'self'"));
    // M23 (AD-26): the chrome's shell-command channel (Tauri IPC).
    assert!(csp.contains("ipc:"));
    assert!(csp.contains("http://ipc.localhost"));
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["x-frame-options"], "SAMEORIGIN");
    let body = response.text().await.unwrap();
    assert!(body.contains("ZIM"));
    // M23: the completed NTP grammar — centered search, tile grid,
    // actions, customization entry point (§9/§12).
    assert!(body.contains("id=\"ntp-search\""));
    assert!(body.contains("id=\"recent-tiles\""));
    assert!(body.contains("id=\"all-tiles\""));
    assert!(body.contains("id=\"theme-presets\""));
    assert!(body.contains("id=\"create-dialog\""));

    // The M22 application chrome surface.
    let response = engine
        .client
        .get(engine.page_url("/app"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("tabstrip"));

    // A declared per-instance stub surface (the §3 inventory rows
    // M22 brings into the package; one entrypoint, six surfaces).
    let response = engine
        .client
        .get(engine.page_url("/servers/0a1b2c3d-0000-0000-0000-000000000000/files"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("surface"));

    // The console surface, parameterized.
    let response = engine
        .client
        .get(engine.page_url("/servers/0a1b2c3d-0000-0000-0000-000000000000/console"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("console"));

    // An asset, with cache headers + ETag.
    let response = engine
        .client
        .get(engine.page_url("/assets/main.css"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "text/css; charset=utf-8"
    );
    assert_eq!(response.headers()["cache-control"], "public, max-age=3600");
    let etag = response.headers()["etag"].to_str().unwrap().to_owned();
    assert!(etag.starts_with('"'));
    // Revalidation: If-None-Match → 304.
    let response = engine
        .client
        .get(engine.page_url("/assets/main.css"))
        .header(reqwest::header::IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 304);

    // The package root → newtab (NTP replacement rule).
    let response = engine.client.get(engine.page_url("")).send().await.unwrap();
    assert_eq!(response.status(), 200);

    // An undeclared surface → the engine's 404 page.
    let response = engine
        .client
        .get(engine.page_url("/settings"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    assert!(response.headers().contains_key("content-security-policy"));
    assert!(response.text().await.unwrap().contains("not declared"));

    // A traversal attempt never reads the filesystem.
    let response = engine
        .client
        .get(engine.page_url("/../engine.db"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn pages_serve_without_credentials() {
    // Page shells are static: no session required (data flows through
    // the credential-checked API).
    let engine = TestEngine::start(temp_frontend_dir()).await;
    let response = engine
        .client
        .get(engine.page_url("/newtab"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

// ── The §10.14 endpoints ──────────────────────────────────────────

#[tokio::test]
async fn frontend_active_requires_a_session_and_reports_the_default() {
    let engine = TestEngine::start(temp_frontend_dir()).await;
    engine.seed_user("admin@zim.local", "password123", true);

    // 401 without credentials.
    let response = engine
        .client
        .get(engine.url("/frontend/active"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    let cookie = engine.login("admin@zim.local", "password123").await;
    let response = engine
        .client
        .get(engine.url("/frontend/active"))
        .header(reqwest::header::COOKIE, format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["object"], "frontend_package");
    assert_eq!(body["id"], "org.zaminmc.frontend.default");
    assert_eq!(body["is_default"], true);
    assert_eq!(body["origin"], "bundled");
    assert!(body["recovery_notice"].is_null());
    assert!(body["page_surfaces"].as_array().unwrap().len() >= 3);
}

#[tokio::test]
async fn frontend_mutations_are_admin_gated() {
    let engine = TestEngine::start(temp_frontend_dir()).await;
    engine.seed_user("member@zim.local", "password123", false);
    let cookie = engine.login("member@zim.local", "password123").await;
    let auth = format!("zim_session={cookie}");

    let response = engine
        .client
        .post(engine.url("/frontend/active"))
        .header(reqwest::header::COOKIE, &auth)
        .json(&json!({ "id": "default" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"]["code"],
        "permission_denied"
    );

    let response = engine
        .client
        .post(engine.url("/frontend/upload"))
        .header(reqwest::header::COOKIE, &auth)
        .header(reqwest::header::CONTENT_TYPE, "application/zip")
        .body(good_custom_zip("org.example.frontend.gated", true))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);

    let response = engine
        .client
        .post(engine.url("/frontend/reset"))
        .header(reqwest::header::COOKIE, &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn upload_activate_and_reset_lifecycle() {
    let engine = TestEngine::start(temp_frontend_dir()).await;
    engine.seed_user("admin@zim.local", "password123", true);
    let cookie = engine.login("admin@zim.local", "password123").await;
    let auth = format!("zim_session={cookie}");

    // Upload (trusted signature) → 201 + passing report.
    let response = engine
        .client
        .post(engine.url("/frontend/upload"))
        .header(reqwest::header::COOKIE, &auth)
        .header(reqwest::header::CONTENT_TYPE, "application/zip")
        .body(good_custom_zip("org.example.frontend.custom", true))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["package"]["id"], "org.example.frontend.custom");
    assert_eq!(body["package"]["signature"], "trusted");
    assert_eq!(body["smoke"]["passed"], true);

    // The page surface still serves the default until activation.
    let response = engine
        .client
        .get(engine.page_url("/newtab"))
        .send()
        .await
        .unwrap();
    assert!(!response
        .text()
        .await
        .unwrap()
        .contains("custom-frontend-marker"));

    // Activate → the custom package serves.
    let response = engine
        .client
        .post(engine.url("/frontend/active"))
        .header(reqwest::header::COOKIE, &auth)
        .json(&json!({ "id": "org.example.frontend.custom", "version": "0.1.0" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["is_default"], false);
    assert_eq!(body["origin"], "data-dir");

    let response = engine
        .client
        .get(engine.page_url("/newtab"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("custom-frontend-marker"));

    // Undeclared default surfaces now 404 (the custom package only
    // declares newtab).
    let response = engine
        .client
        .get(engine.page_url("/servers"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);

    // Reset → the default is back.
    let response = engine
        .client
        .post(engine.url("/frontend/reset"))
        .header(reqwest::header::COOKIE, &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["is_default"], true);
    let response = engine
        .client
        .get(engine.page_url("/servers"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    // Re-upload of the same id+version conflicts.
    let response = engine
        .client
        .post(engine.url("/frontend/upload"))
        .header(reqwest::header::COOKIE, &auth)
        .header(reqwest::header::CONTENT_TYPE, "application/zip")
        .body(good_custom_zip("org.example.frontend.custom", true))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
}

#[tokio::test]
async fn a_failing_package_is_rejected_and_the_default_is_kept() {
    let engine = TestEngine::start(temp_frontend_dir()).await;
    engine.seed_user("admin@zim.local", "password123", true);
    let cookie = engine.login("admin@zim.local", "password123").await;
    let auth = format!("zim_session={cookie}");

    // Upload the inline-script package: stored, but its report fails.
    let response = engine
        .client
        .post(engine.url("/frontend/upload"))
        .header(reqwest::header::COOKIE, &auth)
        .header(reqwest::header::CONTENT_TYPE, "application/zip")
        .body(broken_custom_zip("org.example.frontend.broken"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["smoke"]["passed"], false);

    // Activation → 422 frontend_smoke_test_failed, with the report.
    let response = engine
        .client
        .post(engine.url("/frontend/active"))
        .header(reqwest::header::COOKIE, &auth)
        .json(&json!({ "id": "org.example.frontend.broken", "approve_untrusted": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 422);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "frontend_smoke_test_failed");
    let checks = body["error"]["smoke"]["checks"].as_array().unwrap();
    assert!(checks
        .iter()
        .any(|c| c["name"] == "html_structure" && c["passed"] == false));

    // The default is kept.
    let response = engine
        .client
        .get(engine.page_url("/newtab"))
        .send()
        .await
        .unwrap();
    assert!(!response.text().await.unwrap().contains("inline()"));

    // The standalone smoke-test endpoint reports the same failure.
    let response = engine
        .client
        .post(engine.url("/frontend/smoke-test"))
        .header(reqwest::header::COOKIE, &auth)
        .json(&json!({ "id": "org.example.frontend.broken" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["smoke"]["passed"], false);
}

#[tokio::test]
async fn unsigned_packages_need_explicit_approval() {
    let engine = TestEngine::start(temp_frontend_dir()).await;
    engine.seed_user("admin@zim.local", "password123", true);
    let cookie = engine.login("admin@zim.local", "password123").await;
    let auth = format!("zim_session={cookie}");

    let response = engine
        .client
        .post(engine.url("/frontend/upload"))
        .header(reqwest::header::COOKIE, &auth)
        .header(reqwest::header::CONTENT_TYPE, "application/zip")
        .body(good_custom_zip("org.example.frontend.unsigned", false))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["package"]["signature"],
        "unsigned"
    );

    // Activation without approval → 403 with the reason.
    let response = engine
        .client
        .post(engine.url("/frontend/active"))
        .header(reqwest::header::COOKIE, &auth)
        .json(&json!({ "id": "org.example.frontend.unsigned" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("admin approval"));

    // With explicit approval → activates.
    let response = engine
        .client
        .post(engine.url("/frontend/active"))
        .header(reqwest::header::COOKIE, &auth)
        .json(&json!({ "id": "org.example.frontend.unsigned", "approve_untrusted": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn activating_an_unknown_package_is_404_and_upload_validates_the_archive() {
    let engine = TestEngine::start(temp_frontend_dir()).await;
    engine.seed_user("admin@zim.local", "password123", true);
    let cookie = engine.login("admin@zim.local", "password123").await;
    let auth = format!("zim_session={cookie}");

    let response = engine
        .client
        .post(engine.url("/frontend/active"))
        .header(reqwest::header::COOKIE, &auth)
        .json(&json!({ "id": "org.example.frontend.nope" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);

    // A non-ZIP body is a 415.
    let response = engine
        .client
        .post(engine.url("/frontend/upload"))
        .header(reqwest::header::COOKIE, &auth)
        .header(reqwest::header::CONTENT_TYPE, "application/zip")
        .body("this is not a zip")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);

    // The wrong content type is rejected before the body is read.
    let response = engine
        .client
        .post(engine.url("/frontend/upload"))
        .header(reqwest::header::COOKIE, &auth)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 415);
}

#[tokio::test]
async fn activating_the_bundled_default_by_name_resets() {
    let engine = TestEngine::start(temp_frontend_dir()).await;
    engine.seed_user("admin@zim.local", "password123", true);
    let cookie = engine.login("admin@zim.local", "password123").await;
    let auth = format!("zim_session={cookie}");

    // Activate the custom package first.
    engine
        .client
        .post(engine.url("/frontend/upload"))
        .header(reqwest::header::COOKIE, &auth)
        .header(reqwest::header::CONTENT_TYPE, "application/zip")
        .body(good_custom_zip("org.example.frontend.two", true))
        .send()
        .await
        .unwrap();
    engine
        .client
        .post(engine.url("/frontend/active"))
        .header(reqwest::header::COOKIE, &auth)
        .json(&json!({ "id": "org.example.frontend.two" }))
        .send()
        .await
        .unwrap();

    // Then "default" resets.
    let response = engine
        .client
        .post(engine.url("/frontend/active"))
        .header(reqwest::header::COOKIE, &auth)
        .json(&json!({ "id": "default" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["is_default"], true);
}
