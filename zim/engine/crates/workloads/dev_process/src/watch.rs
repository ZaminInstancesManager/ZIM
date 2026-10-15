//! The file-watch restart supervisor (M19).
//!
//! The §M19 DevProcess scope: "file-watch restart via `notify`
//! crate". The watcher here owns the *detection* half — watching
//! the workload's data directory (the bind mount the process
//! reads its sources through) for matching changes — and
//! reports restart requests; the *restart* half (stop, start,
//! the console generation's reset) belongs to
//! [`DevProcessWorkload`][crate::DevProcessWorkload], which
//! consumes the requests.
//!
//! # The watcher's shape
//!
//! [`FileWatcher::start`] spawns one OS-thread-backed watcher
//! (the `notify` crate's recommended backend — inotify on
//! Linux, FSEvents on macOS, ReadDirectoryChangesW on Windows)
//! over the data directory, recursive, and one consumer thread
//! that classifies events:
//!
//! - **Matching:** the event's path, relativized against the
//!   data root, must match one of the spec's watch patterns
//!   ([`glob_matches`]: `**` crosses directories, `*` and `?`
//!   stay inside one segment).
//! - **Debouncing:** a burst of changes coalesces into one
//!   restart — the consumer fires at most one request per
//!   debounce window (an editor's save-all writes several files
//!   in one burst; the dev loop restarts once).
//! - **Reporting:** each request lands in a channel the
//!   supervisor polls; the [`WatchState`] (the §6 `Watching`
//!   extended state) is queryable at any time.
//!
//! Non-matching events (the build artifacts a pattern excluded,
//! the `.git` directory's churn) update nothing but the state's
//! last-event stamp and the diagnostics counters.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use notify::{Event, EventKind, RecursiveMode, Watcher as _};

use crate::spec::DevProcessSpec;

/// The §6 `Watching` extended state, surfaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchState {
    /// The watcher is active (the §6 green/Running family with
    /// the "watching for file changes" tooltip).
    Active {
        /// When the last matching event arrived (`None` until
        /// the first one).
        last_event: Option<SystemTime>,
    },
    /// The watcher is not running (the policy does not watch, or
    /// the supervisor was suspended).
    Suspended,
}

/// One restart request the watcher raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartRequest {
    /// The matching paths that triggered the request (the
    /// burst's members, deduplicated).
    pub trigger_paths: Vec<String>,
}

/// The watcher's shared state (the consumer thread writes; the
/// handle reads).
struct WatchShared {
    last_event: Mutex<Option<SystemTime>>,
    events_seen: AtomicU64,
    requests_fired: AtomicU64,
}

/// A running file watcher.
///
/// Dropping the [`FileWatcher`] stops it: the boxed notify
/// watcher closes the event channel's producer side, the
/// consumer's blocking receive ends, and the thread exits. The
/// consumer thread is detached by design (its lifetime is the
/// event channel's, not any handle's).
pub struct FileWatcher {
    shared: Arc<WatchShared>,
    requests: Mutex<std::sync::mpsc::Receiver<RestartRequest>>,
    /// The notify watcher itself — held (never read) so its
    /// lifetime bounds the event channel's producer side: when
    /// this handle drops, the watcher drops, the producer closes,
    /// and the consumer thread exits.
    _watcher: notify::RecommendedWatcher,
}

impl FileWatcher {
    /// Start watching `data_dir` for the spec's patterns.
    ///
    /// The watcher is recursive over the whole data root (the
    /// patterns decide what matches, not the watch set — a
    /// `.git` directory's churn costs one classification, not
    /// one restart).
    pub fn start(data_dir: PathBuf, spec: &DevProcessSpec) -> notify::Result<Self> {
        let (request_tx, request_rx) = std::sync::mpsc::sync_channel::<RestartRequest>(64);
        let shared = Arc::new(WatchShared {
            last_event: Mutex::new(None),
            events_seen: AtomicU64::new(0),
            requests_fired: AtomicU64::new(0),
        });

        let patterns = spec.effective_watch_patterns();
        let debounce = spec.debounce();

        // The notify watcher delivers on its own callback thread;
        // the consumer does the classification and debouncing.
        let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<Event>(256);
        let mut watcher =
            notify::recommended_watcher(move |result: Result<Event, notify::Error>| {
                if let Ok(event) = result {
                    // The send fails only when the consumer is
                    // gone — which means the watcher is being
                    // dropped anyway.
                    let _ = event_tx.send(event);
                }
            })?;
        watcher.watch(&data_dir, RecursiveMode::Recursive)?;

        let consumer_shared = Arc::clone(&shared);
        let consumer_root = data_dir.clone();
        let _detached = std::thread::Builder::new()
            .name("zim-dev-watch".to_owned())
            .spawn(move || {
                let mut pending: Vec<String> = Vec::new();
                let mut last_event: Option<SystemTime> = None;
                loop {
                    // The trailing debounce: consume every event
                    // as it arrives (updating the burst's last-event
                    // stamp), and fire only after the burst has
                    // settled — `debounce` of quiet since its last
                    // event. One save (which the filesystem reports
                    // as a create-modify pair) is one restart, not
                    // one per raw event.
                    match event_rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(event) => {
                            consumer_shared.events_seen.fetch_add(1, Ordering::Relaxed);
                            if let Some(relative) =
                                matching_paths(&event, &consumer_root, &patterns)
                            {
                                let now = SystemTime::now();
                                if let Ok(mut last) = consumer_shared.last_event.lock() {
                                    *last = Some(now);
                                }
                                last_event = Some(now);
                                for path in relative {
                                    if !pending.contains(&path) {
                                        pending.push(path);
                                    }
                                }
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    // Fire when the burst has settled: something
                    // is pending and the quiet window since its
                    // last matching event has passed.
                    if !pending.is_empty() {
                        let settled = last_event
                            .map(|last| {
                                SystemTime::now()
                                    .duration_since(last)
                                    .unwrap_or(Duration::ZERO)
                                    >= debounce
                            })
                            .unwrap_or(false);
                        if settled {
                            let request = RestartRequest {
                                trigger_paths: std::mem::take(&mut pending),
                            };
                            consumer_shared
                                .requests_fired
                                .fetch_add(1, Ordering::Relaxed);
                            if request_tx.send(request).is_err() {
                                break; // The supervisor side is gone.
                            }
                        }
                    }
                }
            })?;

        Ok(Self {
            shared,
            requests: Mutex::new(request_rx),
            _watcher: watcher,
        })
    }

    /// The watcher's current state (the §6 `Watching` surface).
    pub fn state(&self) -> WatchState {
        let last = *self
            .shared
            .last_event
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        WatchState::Active { last_event: last }
    }

    /// How many raw filesystem events the watcher has seen
    /// (matching or not — the diagnostics counter).
    pub fn events_seen(&self) -> u64 {
        self.shared.events_seen.load(Ordering::Relaxed)
    }

    /// How many restart requests the watcher has fired.
    pub fn requests_fired(&self) -> u64 {
        self.shared.requests_fired.load(Ordering::Relaxed)
    }

    /// Drain one pending restart request (the supervisor task's
    /// poll).
    pub fn take_request(&self) -> Option<RestartRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .try_recv()
            .ok()
    }
}

/// Whether one event is a content-relevant change (a create,
/// modify, or remove — not a metadata touch or an access).
fn is_relevant(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// The event's paths that match the patterns, relativized
/// against the watched root.
///
/// `None` when the event carries no matching path. A path
/// outside the root (a rename's moved-away source, say) does
/// not match — the relativization fails, not the glob.
fn matching_paths(event: &Event, root: &Path, patterns: &[String]) -> Option<Vec<String>> {
    if !is_relevant(&event.kind) {
        return None;
    }
    let matched: Vec<String> = event
        .paths
        .iter()
        .filter_map(|path| path.strip_prefix(root).ok())
        .map(|relative| {
            let text = relative.to_string_lossy();
            text.trim_start_matches('/').to_owned()
        })
        .filter(|relative| {
            !relative.is_empty()
                && patterns
                    .iter()
                    .any(|pattern| glob_matches(pattern, relative))
        })
        .collect();
    if matched.is_empty() {
        None
    } else {
        Some(matched)
    }
}

/// Whether one relative path matches one glob pattern.
///
/// The supported vocabulary (the dev-loop's own):
///
/// - `**` matches across directory separators (`src/**/*.rs`
///   matches `src/a/b/main.rs`);
/// - `*` matches within one segment (`*.rs` matches `main.rs`,
///   not `a/main.rs`);
/// - `?` matches one character within one segment;
/// - everything else matches literally (including dots).
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let path: Vec<char> = path.chars().collect();
    glob_match_inner(&pattern, &path)
}

/// The recursive matcher: `pattern` against `path`, both as char
/// slices.
fn glob_match_inner(pattern: &[char], path: &[char]) -> bool {
    // A trailing `/**` matches the directory itself as well as
    // everything under it (`src/**` covers `src` and
    // `src/a/b.rs`) — the watch vocabulary's own convention.
    if pattern.len() == 3 && pattern[0] == '/' && pattern[1] == '*' && pattern[2] == '*' {
        return path.is_empty() || path[0] == '/';
    }
    // `**` in the pattern: either it consumes nothing, or it
    // consumes at least one segment (the cross-separator form).
    if pattern.len() >= 2 && pattern[0] == '*' && pattern[1] == '*' {
        // `**/` — the segment-spanning form: consume the slash
        // too, then try every suffix of the path.
        if pattern.len() > 2 && pattern[2] == '/' {
            let rest = &pattern[3..];
            for split in 0..=path.len() {
                if glob_match_inner(rest, &path[split..]) {
                    return true;
                }
            }
            return false;
        }
        // A bare `**` at the end of the pattern (or mid-pattern
        // without its slash, which the vocabulary does not
        // generate): match any suffix.
        let rest = &pattern[2..];
        for split in 0..=path.len() {
            if glob_match_inner(rest, &path[split..]) {
                return true;
            }
        }
        return false;
    }
    if pattern.is_empty() {
        return path.is_empty();
    }
    if path.is_empty() {
        return false;
    }
    match pattern[0] {
        '*' => {
            // `*` stays inside one segment.
            for skip in 0..=path.len() {
                if path.get(skip) == Some(&'/') {
                    break;
                }
                if glob_match_inner(&pattern[1..], &path[skip..]) {
                    return true;
                }
            }
            false
        }
        '?' => path[0] != '/' && glob_match_inner(&pattern[1..], &path[1..]),
        literal => path[0] == literal && glob_match_inner(&pattern[1..], &path[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_star_stays_inside_one_segment() {
        assert!(glob_matches("*.rs", "main.rs"));
        assert!(!glob_matches("*.rs", "src/main.rs"));
        assert!(glob_matches("src/*.rs", "src/main.rs"));
        assert!(!glob_matches("src/*.rs", "src/a/main.rs"));
    }

    #[test]
    fn the_double_star_crosses_segments() {
        assert!(glob_matches("**/*.rs", "main.rs"));
        assert!(glob_matches("**/*.rs", "src/main.rs"));
        assert!(glob_matches("**/*.rs", "src/a/b/main.rs"));
        assert!(!glob_matches("**/*.rs", "src/main.ts"));
        assert!(glob_matches("src/**", "src/a/b/c.txt"));
        assert!(glob_matches("src/**", "src"));
    }

    #[test]
    fn the_question_mark_matches_one_character() {
        assert!(glob_matches("mai?.rs", "main.rs"));
        assert!(!glob_matches("mai?.rs", "mai.rs"));
        assert!(!glob_matches("???.rs", "main.rs"));
    }

    #[test]
    fn literals_match_literally() {
        assert!(glob_matches("Cargo.toml", "Cargo.toml"));
        assert!(!glob_matches("Cargo.toml", "Cargo.lock"));
        assert!(glob_matches("a/b.txt", "a/b.txt"));
        assert!(!glob_matches("a/b.txt", "a/b/b.txt"));
    }

    #[test]
    fn the_everything_pattern_matches_everything() {
        for path in ["makefile", "src/a.rs", "deep/dir/x.js"] {
            assert!(glob_matches("**/*", path), "{path}");
        }
    }

    #[test]
    fn a_watcher_over_a_live_directory_fires_for_matching_changes() {
        let dir =
            std::env::temp_dir().join(format!("zim-watch-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("src")).expect("create the watch root");
        let spec = crate::spec::DevProcessSpec {
            command: vec!["true".to_owned()],
            working_directory: None,
            env: Vec::new(),
            watch_patterns: vec!["**/*.rs".to_owned()],
            restart_policy: crate::spec::DevRestartPolicy::OnFileChange,
            debounce_ms: Some(50),
        };
        let watcher = FileWatcher::start(dir.clone(), &spec).expect("the watcher starts");
        assert_eq!(watcher.state(), WatchState::Active { last_event: None });

        // A matching write fires a request carrying the
        // root-relative path.
        std::fs::write(dir.join("src/main.rs"), b"fn main() {}").expect("write");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let request = loop {
            if let Some(request) = watcher.take_request() {
                break request;
            }
            if std::time::Instant::now() > deadline {
                panic!("the matching write never fired a restart request");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(
            request
                .trigger_paths
                .iter()
                .any(|path| path == "src/main.rs"),
            "{:?}",
            request.trigger_paths
        );
        assert_eq!(watcher.requests_fired(), 1);

        // A non-matching write does not fire.
        std::fs::write(dir.join("notes.txt"), b"not rust").expect("write");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(watcher.requests_fired(), 1);

        // The state carries the last event's stamp.
        assert!(matches!(
            watcher.state(),
            WatchState::Active {
                last_event: Some(_)
            }
        ));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_burst_of_changes_coalesces_into_one_request() {
        let dir =
            std::env::temp_dir().join(format!("zim-watch-burst-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("src")).expect("create the watch root");
        let spec = crate::spec::DevProcessSpec {
            command: vec!["true".to_owned()],
            working_directory: None,
            env: Vec::new(),
            watch_patterns: vec!["**/*.rs".to_owned()],
            restart_policy: crate::spec::DevRestartPolicy::OnFileChange,
            debounce_ms: Some(400),
        };
        let watcher = FileWatcher::start(dir.clone(), &spec).expect("the watcher starts");

        // A burst: five writes inside one debounce window.
        for index in 0..5 {
            std::fs::write(dir.join(format!("src/mod{index}.rs")), b"// x").expect("write");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Let the window settle.
        std::thread::sleep(Duration::from_millis(700));

        let requests: Vec<RestartRequest> = std::iter::from_fn(|| watcher.take_request()).collect();
        assert_eq!(requests.len(), 1, "the burst coalesces: {requests:?}");
        assert!(requests[0].trigger_paths.len() >= 2, "{requests:?}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
