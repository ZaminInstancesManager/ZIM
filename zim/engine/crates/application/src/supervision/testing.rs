//! Test doubles for the supervision module (feature `test-util`).
//!
//! [`MockRuntime`] is a scriptable in-memory `Runtime`: starts
//! consume a per-start behavior, stop/kill have configurable
//! latency, and every call is recorded so tests can assert
//! serialization (invariant 9) and race-freedom (invariant 7)
//! directly on the call log. [`MockRuntimeFactory`] binds the mock
//! into a [`Supervisor`](super::Supervisor).
//!
//! Off by default — production composition roots never enable it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::runtime::{Runtime, RuntimeFactory};
use super::state::{
    ConsoleAttachment, ExecCommand, ExecOutput, InstanceConfig, ObservedState, ResourceUsage,
    RuntimeError, RuntimeKind,
};

/// What one `start()` call does.
#[derive(Debug, Clone)]
pub enum MockStart {
    /// Start succeeds; the instance runs for `run_for` and then
    /// exits with `(exit_code, oom_killed)`.
    Run {
        /// How long the mock stays running.
        run_for: Duration,
        /// The exit code it exits with.
        exit_code: Option<i32>,
        /// Whether the exit is an OOM kill.
        oom_killed: bool,
    },
    /// Start fails with this error.
    Fail(RuntimeError),
}

/// The recorded call log (assertion surface).
#[derive(Debug, Default, Clone)]
pub struct MockCalls {
    /// How many `start()` calls completed.
    pub started: usize,
    /// How many `stop()` calls completed.
    pub stopped: usize,
    /// How many `kill()` calls completed.
    pub killed: usize,
    /// The sequence of operation *entries* (for ordering asserts).
    pub entries: Vec<&'static str>,
}

/// A scriptable in-memory runtime.
#[derive(Clone)]
pub struct MockRuntime {
    core: Arc<Mutex<MockCore>>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    calls: Arc<Mutex<MockCalls>>,
    stop_latency: Duration,
    /// Whether `start()` opens an in-memory console attachment whose
    /// `send_input` echoes the line back (the live console path for
    /// tests without a Docker daemon).
    echo_console: bool,
}

struct MockCore {
    state: ObservedState,
    exit: (Option<i32>, bool),
    script: VecDeque<MockStart>,
    /// The live console attachment's sending half (echo mode);
    /// `None` while the mock is not running.
    console_tx: Option<mpsc::Sender<String>>,
    /// The pending receiving half of the current run's attachment,
    /// taken by `attach_console` (one consumer per run).
    console_rx: Option<mpsc::Receiver<String>>,
}

impl MockRuntime {
    /// A mock whose next `script.len()` starts behave as scripted
    /// (subsequent starts run forever).
    pub fn scripted(script: Vec<MockStart>) -> Self {
        Self::with_stop_latency(script, Duration::ZERO)
    }

    /// A scripted mock with the given `stop()` latency (for
    /// serialization tests: the worker must still apply actions one
    /// at a time).
    pub fn with_stop_latency(script: Vec<MockStart>, stop_latency: Duration) -> Self {
        Self {
            core: Arc::new(Mutex::new(MockCore {
                state: ObservedState::Offline {
                    exit_code: None,
                    oom_killed: false,
                },
                exit: (None, false),
                script: script.into(),
                console_tx: None,
                console_rx: None,
            })),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            calls: Arc::new(Mutex::new(MockCalls::default())),
            stop_latency,
            echo_console: false,
        }
    }

    /// Enable the in-memory echo console (M7.1): every `start()`
    /// opens a console attachment, `attach_console` hands out its
    /// receiving half, `send_input` echoes the line onto it, and
    /// stop/kill close it (the consumer's stream ends after the
    /// buffered lines drain — the flush semantics of a real
    /// container exit). This is the `cat`-style echo container the
    /// API and supervision tests use to cover the live console path
    /// without a Docker daemon.
    pub fn with_echo_console(mut self) -> Self {
        self.echo_console = true;
        self
    }

    /// A mock that starts successfully and runs forever.
    pub fn always_running() -> Self {
        Self::scripted(vec![MockStart::Run {
            run_for: Duration::from_secs(3600),
            exit_code: Some(0),
            oom_killed: false,
        }])
    }

    /// Force the observed state from the outside (simulating a
    /// container that died — or a backend that went away).
    pub fn force_state(&self, state: ObservedState) {
        let mut core = self.lock_core();
        if let ObservedState::Offline {
            exit_code,
            oom_killed,
        } = &state
        {
            core.exit = (*exit_code, *oom_killed);
        }
        core.state = state;
    }

    /// Force the runtime into the unreachable form.
    pub fn force_unavailable(&self) {
        let mut core = self.lock_core();
        core.state = ObservedState::Unknown;
    }

    /// The recorded calls.
    pub fn calls(&self) -> MockCalls {
        self.calls.lock().expect("mock poisoned").clone()
    }

    /// The maximum number of concurrently-active runtime operations
    /// observed (1 = properly serialized).
    pub fn max_concurrent(&self) -> usize {
        self.max_active.load(Ordering::Acquire)
    }

    fn lock_core(&self) -> std::sync::MutexGuard<'_, MockCore> {
        self.core.lock().expect("mock poisoned")
    }

    fn enter(&self, what: &'static str) {
        self.calls.lock().expect("mock poisoned").entries.push(what);
        let now = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.max_active.fetch_max(now, Ordering::AcqRel);
    }

    fn leave(&self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }

    fn complete(&self, what: &'static str) {
        let mut calls = self.calls.lock().expect("mock poisoned");
        match what {
            "start" => calls.started += 1,
            "stop" => calls.stopped += 1,
            "kill" => calls.killed += 1,
            _ => {}
        }
    }
}

#[async_trait]
impl Runtime for MockRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Container
    }

    async fn state(&self) -> Result<ObservedState, RuntimeError> {
        let core = self.lock_core();
        match &core.state {
            ObservedState::Unknown => Err(RuntimeError::unavailable("mock runtime is unreachable")),
            other => Ok(other.clone()),
        }
    }

    async fn start(&self, _cfg: &InstanceConfig) -> Result<(), RuntimeError> {
        self.enter("start");
        let behavior = self.lock_core().script.pop_front();
        let outcome = match behavior {
            Some(MockStart::Fail(e)) => Err(e),
            Some(MockStart::Run {
                run_for,
                exit_code,
                oom_killed,
            }) => {
                {
                    let mut core = self.lock_core();
                    core.state = ObservedState::Running {
                        started_at: chrono::Utc::now(),
                        pid: Some(4242),
                    };
                    core.exit = (exit_code, oom_killed);
                    if self.echo_console {
                        let (tx, rx) = mpsc::channel(64);
                        core.console_tx = Some(tx);
                        core.console_rx = Some(rx);
                    }
                }
                // The mock's "process": flips state to Offline after
                // run_for (guarded by still-running so a later
                // force_state can pre-empt it). Fire-and-forget by
                // design: the mock's lifetime outlives any single call.
                let core = Arc::clone(&self.core);
                tokio::spawn(async move {
                    tokio::time::sleep(run_for).await;
                    let mut core = core.lock().expect("mock poisoned");
                    if core.state.is_running() {
                        core.state = ObservedState::Offline {
                            exit_code: core.exit.0,
                            oom_killed: core.exit.1,
                        };
                    }
                });
                Ok(())
            }
            None => {
                // Past the script: run forever.
                let mut core = self.lock_core();
                core.state = ObservedState::Running {
                    started_at: chrono::Utc::now(),
                    pid: Some(4242),
                };
                core.exit = (None, false);
                if self.echo_console {
                    let (tx, rx) = mpsc::channel(64);
                    core.console_tx = Some(tx);
                    core.console_rx = Some(rx);
                }
                Ok(())
            }
        };
        self.leave();
        if outcome.is_ok() {
            self.complete("start");
        }
        outcome
    }

    async fn stop(&self, _deadline: Duration) -> Result<(), RuntimeError> {
        self.enter("stop");
        tokio::time::sleep(self.stop_latency).await;
        {
            let mut core = self.lock_core();
            if matches!(core.state, ObservedState::Unknown) {
                self.leave();
                return Err(RuntimeError::unavailable("mock runtime is unreachable"));
            }
            core.state = ObservedState::Offline {
                exit_code: core.exit.0,
                oom_killed: core.exit.1,
            };
            // The run's console attachment closes: dropping the
            // sender ends the consumer's stream once the buffered
            // lines drain (a real container exit does the same).
            core.console_tx = None;
            core.console_rx = None;
        }
        self.leave();
        self.complete("stop");
        Ok(())
    }

    async fn kill(&self) -> Result<(), RuntimeError> {
        self.enter("kill");
        {
            let mut core = self.lock_core();
            if matches!(core.state, ObservedState::Unknown) {
                self.leave();
                return Err(RuntimeError::unavailable("mock runtime is unreachable"));
            }
            core.state = ObservedState::Offline {
                exit_code: core.exit.0.or(Some(137)),
                oom_killed: core.exit.1,
            };
            core.exit.0 = core.exit.0.or(Some(137));
            core.console_tx = None;
            core.console_rx = None;
        }
        self.leave();
        self.complete("kill");
        Ok(())
    }

    async fn exec(&self, _cmd: &ExecCommand) -> Result<ExecOutput, RuntimeError> {
        Ok(ExecOutput {
            exit_code: Some(0),
            stdout: b"mock\n".to_vec(),
            stderr: Vec::new(),
        })
    }

    async fn attach_console(&self) -> Result<ConsoleAttachment, RuntimeError> {
        let mut core = self.lock_core();
        if !core.state.is_running() {
            return Err(RuntimeError::NotRunning(
                "mock runtime: console attach requires the running state".to_owned(),
            ));
        }
        match core.console_rx.take() {
            Some(lines) => Ok(ConsoleAttachment { lines }),
            None => Err(RuntimeError::Backend(
                "mock runtime: this run's console attachment is already consumed".to_owned(),
            )),
        }
    }

    async fn send_input(&self, input: &[u8]) -> Result<(), RuntimeError> {
        if !self.echo_console {
            return Err(RuntimeError::Unsupported {
                feature: "console input".to_owned(),
                milestone: "echo console not enabled on this mock".to_owned(),
            });
        }
        // Clone the sender out of the lock: the bounded send below
        // must never hold the core mutex across an await. A stop that
        // lands in between simply ends the channel after this line
        // drains — the flush semantics of a real container exit.
        let sender = {
            let core = self.lock_core();
            if !core.state.is_running() {
                return Err(RuntimeError::NotRunning(
                    "mock runtime: console input requires the running state".to_owned(),
                ));
            }
            core.console_tx.clone()
        };
        let Some(tx) = sender else {
            return Err(RuntimeError::NotRunning(
                "mock runtime: no live console attachment".to_owned(),
            ));
        };
        // Line-oriented echo, mirroring the runtime contract: the
        // line keeps a trailing newline (added when the caller's
        // bytes do not end with one), decoded lossily.
        let mut line = String::from_utf8_lossy(input).into_owned();
        if !line.ends_with('\n') {
            line.push('\n');
        }
        tx.send(line)
            .await
            .map_err(|_| RuntimeError::Backend("mock console closed".to_owned()))
    }

    async fn exit_state(&self) -> Result<(Option<i32>, bool), RuntimeError> {
        let core = self.lock_core();
        if matches!(core.state, ObservedState::Unknown) {
            return Err(RuntimeError::unavailable("mock runtime is unreachable"));
        }
        Ok(core.exit)
    }

    async fn stats(&self) -> Result<ResourceUsage, RuntimeError> {
        Ok(ResourceUsage {
            memory_usage_bytes: Some(1024),
            memory_limit_bytes: Some(2048),
            pids_current: Some(1),
            ..Default::default()
        })
    }
}

/// A factory returning clones of one mock.
#[derive(Clone)]
pub struct MockRuntimeFactory {
    mock: MockRuntime,
    unavailable: Arc<std::sync::atomic::AtomicBool>,
}

impl MockRuntimeFactory {
    /// Bind a mock.
    pub fn new(mock: MockRuntime) -> Self {
        Self {
            mock,
            unavailable: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Make `check()` (and only `check()`) fail, simulating an
    /// unreachable backend at enqueue time.
    pub fn force_check_failure(&self) {
        self.unavailable.store(true, Ordering::Release);
    }
}

#[async_trait]
impl RuntimeFactory for MockRuntimeFactory {
    async fn create(&self, _cfg: &InstanceConfig) -> Result<Arc<dyn Runtime>, RuntimeError> {
        Ok(Arc::new(self.mock.clone()))
    }

    async fn check(&self) -> Result<(), RuntimeError> {
        if self.unavailable.load(Ordering::Acquire) {
            Err(RuntimeError::unavailable("mock factory check failed"))
        } else {
            Ok(())
        }
    }
}
