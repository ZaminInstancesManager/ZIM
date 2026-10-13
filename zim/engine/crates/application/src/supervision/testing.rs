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

use super::runtime::{Runtime, RuntimeFactory};
use super::state::{
    ExecCommand, ExecOutput, InstanceConfig, ObservedState, ResourceUsage, RuntimeError,
    RuntimeKind,
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
}

struct MockCore {
    state: ObservedState,
    exit: (Option<i32>, bool),
    script: VecDeque<MockStart>,
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
            })),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            calls: Arc::new(Mutex::new(MockCalls::default())),
            stop_latency,
        }
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

    async fn send_input(&self, _input: &[u8]) -> Result<(), RuntimeError> {
        Err(RuntimeError::Unsupported {
            feature: "console input".to_owned(),
            milestone: "M7 (streaming)".to_owned(),
        })
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
