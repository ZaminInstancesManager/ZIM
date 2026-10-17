//! Engine-attach integration test (M21).
//!
//! Runs the shell's REAL attach logic against a REAL engine binary
//! — no GUI, no webview: the pieces under test are discovery-file
//! reading, the loopback TLS health probe, the crash/respawn
//! semantics, and the `zim://` mapping composed end to end.
//!
//! Gated on `ZIM_SHELL_IT_ENGINE` (path to a built `zim_engine`
//! binary — CI sets it; without it the test skips so `cargo test`
//! stays runnable on webview-less dev machines):
//!
//! ```text
//! ZIM_SHELL_IT_ENGINE=../engine/target/debug/zim_engine \
//!   cargo test --test engine_attach
//! ```

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Serializes the tests whose `attach()` spawns a DEFAULT-port
/// engine: attach()'s step 2 answers "already running" to ANY
/// engine on the default port, so two parallel spawn-tests would
/// steal each other's outcomes (the fresh-boot regression test
/// learned this the hard way in CI — its attach answered to the
/// sibling test's engine and never spawned at all).
fn spawn_serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

use zim_shell::engine::{self, AttachOutcome};
use zim_shell::zim_url;

/// Where CI points the engine binary (and the dev override).
fn engine_bin() -> Option<PathBuf> {
    std::env::var("ZIM_SHELL_IT_ENGINE")
        .ok()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

/// A unique scratch data dir (std-only, like the engine's own
/// smoke test).
fn scratch_data_dir(label: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "zim_shell_it_{label}_{}_{}",
        std::process::id(),
        stamp
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Start an engine (attached to our test process so a panic cleans
/// it up) with an ephemeral port, and wait until it is healthy.
fn start_engine(bin: &Path, data_dir: &Path) -> Child {
    let mut child = Command::new(bin)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--port")
        .arg("0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn zim_engine");

    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(info) = engine::discover(data_dir) {
            if engine::probe(info.port).is_ok() {
                return child;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    // Reap the child on the failure path too — an unreaped kill
    // leaves a zombie (clippy::zombie_processes).
    let _ = child.kill();
    let _ = child.wait();
    panic!("engine did not become healthy within 30s");
}

/// Kill hard (crash semantics: the discovery file stays stale).
fn crash(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn attach_finds_a_running_engine_and_maps_newtab() {
    let Some(bin) = engine_bin() else {
        eprintln!("skipping: ZIM_SHELL_IT_ENGINE is not set to an engine binary");
        return;
    };
    let data_dir = scratch_data_dir("attach");

    // An engine is already running → attach() must find it via the
    // discovery file WITHOUT spawning anything.
    let mut child = start_engine(&bin, &data_dir);
    let outcome = engine::attach(&data_dir, Path::new("definitely-not-a-binary"));
    let port = match &outcome {
        AttachOutcome::Attached { port, version, .. } => {
            assert!(version.is_some(), "discovery carries the version");
            *port
        }
        other => panic!("expected Attached, got {other:?}"),
    };
    assert_ne!(port, 0, "the ephemeral port was recorded");

    // The composed URL the shell would navigate to: zim://newtab →
    // the engine's page root → the newtab entrypoint. The fetch
    // uses the MAPPED URL's own path, so the mapping itself is
    // under test, not a restated constant.
    let origin = format!("https://127.0.0.1:{port}");
    let url = zim_url::resolve("zim://newtab", &origin).unwrap();
    assert!(
        url.starts_with(&origin),
        "the mapped URL stays on the engine origin: {url}"
    );
    let path = &url[origin.len()..];
    let (status, content_type, body) = engine::https_get(port, path).unwrap();
    assert_eq!(status, 200);
    assert!(content_type.starts_with("text/html"));
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("ZIM"),
        "the newtab page identifies as ZIM (body starts: {:?})",
        &body[..body.len().min(120)]
    );

    // A deeper surface from the §3 inventory serves too.
    let (status, _, _) = engine::https_get(port, "/zim/servers").unwrap();
    assert_eq!(status, 200);

    // The M22 application chrome surface serves as well (the tab
    // strip + omnibox host; its panes are the pages above).
    let (status, content_type, body) = engine::https_get(port, "/zim/app").unwrap();
    assert_eq!(status, 200);
    assert!(content_type.starts_with("text/html"));
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("tabstrip"),
        "the app chrome identifies its tab strip (body starts: {:?})",
        &body[..body.len().min(120)]
    );

    // Crash semantics: kill -9 leaves a stale discovery file, which
    // must read back but answer no probe.
    crash(&mut child);
    let stale = engine::discover(&data_dir).expect("stale discovery still parses");
    assert_eq!(stale.port, port);
    assert!(
        engine::probe(port).is_err(),
        "a killed engine answers no health probe"
    );
    // And a respawn on the same data dir recovers (SQLite lock gone
    // with the dead process).
    let mut child = start_engine(&bin, &data_dir);
    let outcome = engine::attach(&data_dir, Path::new("definitely-not-a-binary"));
    assert!(
        matches!(outcome, AttachOutcome::Attached { .. }),
        "respawned engine is attachable again"
    );

    // Graceful shutdown (SIGTERM where available) removes the
    // discovery file.
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        let ok = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                if engine::discover(&data_dir).is_none() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            assert!(
                engine::discover(&data_dir).is_none(),
                "graceful shutdown removed the discovery file"
            );
            // Reap the SIGTERM'd engine (a reaped exit is also the
            // proof it actually stopped).
            let _ = child.wait();
        } else {
            eprintln!("note: could not SIGTERM the engine; skipping the removal assert");
            crash(&mut child);
        }
    }
    #[cfg(not(unix))]
    crash(&mut child);

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn attach_spawns_into_a_dir_that_does_not_exist_yet() {
    let _serial = spawn_serial();
    // The first-boot contract (M27's regression): a fresh install's
    // engine data dir does not exist — `~/.local/share/<app>/engine`
    // is first needed by the SHELL, which opens engine.log inside it
    // before the engine ever runs. The packaged-app smoke found the
    // alpha's install bytes dead-on-arrival here: spawn_detached
    // failed with ENOENT and nothing but down.html ever said so.
    let Some(bin) = engine_bin() else {
        eprintln!("skipping: ZIM_SHELL_IT_ENGINE is not set to an engine binary");
        return;
    };
    let parent = scratch_data_dir("freshboot");
    let data_dir = parent.join("engine"); // deliberately NOT created

    let outcome = engine::attach(&data_dir, &bin);
    let port = match &outcome {
        AttachOutcome::Spawned { port, .. } => *port,
        other => panic!("expected Spawned on a virgin data dir, got {other:?}"),
    };
    assert!(engine::probe(port).is_ok());
    assert!(data_dir.join("engine.log").is_file());
    assert!(data_dir.join("engine.json").is_file());

    #[cfg(unix)]
    {
        let pid = engine::discover(&data_dir).map(|d| d.pid);
        if let Some(pid) = pid {
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                if engine::discover(&data_dir).is_none() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }

    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn attach_spawns_when_nothing_answers() {
    let _serial = spawn_serial();
    let Some(bin) = engine_bin() else {
        eprintln!("skipping: ZIM_SHELL_IT_ENGINE is not set to an engine binary");
        return;
    };
    let data_dir = scratch_data_dir("spawn");

    // Nothing is running on this fresh data dir → attach() spawns
    // the engine (detached, via engine::spawn_detached) and waits.
    let outcome = engine::attach(&data_dir, &bin);
    let port = match &outcome {
        AttachOutcome::Spawned { port, .. } => *port,
        other => panic!("expected Spawned on a fresh data dir, got {other:?}"),
    };

    // The engine the shell spawned is real and healthy…
    assert!(engine::probe(port).is_ok());
    // …its log was captured into the data dir…
    assert!(data_dir.join("engine.log").is_file());
    // …and it is DETACHED: killing this test process's children
    // semantics aside, we terminate it by SIGTERM via its recorded
    // pid to leave a clean slate.
    #[cfg(unix)]
    {
        let pid = engine::discover(&data_dir).map(|d| d.pid);
        if let Some(pid) = pid {
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                if engine::discover(&data_dir).is_none() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }

    let _ = std::fs::remove_dir_all(&data_dir);
}
