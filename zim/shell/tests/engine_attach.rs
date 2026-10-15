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
use std::time::{Duration, Instant};

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
    let _ = child.kill();
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
    // the engine's page root → the newtab entrypoint.
    let origin = format!("https://127.0.0.1:{port}");
    let url = zim_url::resolve("zim://newtab", &origin).unwrap();
    let (status, content_type, body) = engine::https_get(port, "/zim/").unwrap();
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
fn attach_spawns_when_nothing_answers() {
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
