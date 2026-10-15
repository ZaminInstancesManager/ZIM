//! The no-backend runtime factory: what the engine composes when no
//! container runtime is reachable on the host.
//!
//! ZIM boots and serves its API even without Docker (the management
//! surface — users, instances, allocations — stays usable). Power
//! operations fail fast with a structured `runtime_unavailable`, and
//! observed state reports `Unknown` rather than `Offline` per the
//! failure matrix in `docs/architecture/ZIM_ARCHITECTURE.md` §5
//! ("runtime unreachable" is not "process stopped").

use std::sync::Arc;

use async_trait::async_trait;

use super::runtime::{Runtime, RuntimeFactory};
use super::state::{
    ConsoleAttachment, ExecCommand, ExecOutput, InstanceConfig, ObservedState, ResourceUsage,
    RuntimeError, RuntimeKind,
};

/// The unreachable-backend [`RuntimeFactory`].
#[derive(Debug, Clone, Default)]
pub struct NullRuntimeFactory;

/// The unreachable-backend runtime handle.
#[derive(Debug, Clone, Default)]
struct NullRuntime;

const NO_RUNTIME: &str = "no container runtime is available on this host \
     (Docker not detected); instance lifecycle operations are unavailable";

#[async_trait]
impl RuntimeFactory for NullRuntimeFactory {
    async fn create(&self, _cfg: &InstanceConfig) -> Result<Arc<dyn Runtime>, RuntimeError> {
        Ok(Arc::new(NullRuntime))
    }

    async fn check(&self) -> Result<(), RuntimeError> {
        Err(RuntimeError::unavailable(NO_RUNTIME))
    }
}

#[async_trait]
impl Runtime for NullRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Container
    }

    async fn state(&self) -> Result<ObservedState, RuntimeError> {
        Err(RuntimeError::unavailable(NO_RUNTIME))
    }

    async fn start(&self, _cfg: &InstanceConfig) -> Result<(), RuntimeError> {
        Err(RuntimeError::unavailable(NO_RUNTIME))
    }

    async fn stop(&self, _deadline: std::time::Duration) -> Result<(), RuntimeError> {
        Err(RuntimeError::unavailable(NO_RUNTIME))
    }

    async fn kill(&self) -> Result<(), RuntimeError> {
        Err(RuntimeError::unavailable(NO_RUNTIME))
    }

    async fn exec(&self, _cmd: &ExecCommand) -> Result<ExecOutput, RuntimeError> {
        Err(RuntimeError::unavailable(NO_RUNTIME))
    }

    async fn attach_console(&self) -> Result<ConsoleAttachment, RuntimeError> {
        Err(RuntimeError::unavailable(NO_RUNTIME))
    }

    async fn send_input(&self, _input: &[u8]) -> Result<(), RuntimeError> {
        Err(RuntimeError::unavailable(NO_RUNTIME))
    }

    async fn exit_state(&self) -> Result<(Option<i32>, bool), RuntimeError> {
        Err(RuntimeError::unavailable(NO_RUNTIME))
    }

    async fn stats(&self) -> Result<ResourceUsage, RuntimeError> {
        Err(RuntimeError::unavailable(NO_RUNTIME))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn null_factory_reports_unavailable() {
        let factory = NullRuntimeFactory;
        assert!(factory.check().await.is_err());

        let runtime = factory
            .create(&InstanceConfig {
                instance_id: zim_engine_domain::InstanceId::new(),
                container_name: "u".to_owned(),
                hostname: "u".to_owned(),
                image: "img".to_owned(),
                entrypoint: None,
                command: vec!["true".to_owned()],
                env: Vec::new(),
                limits: Default::default(),
                port_bindings: Vec::new(),
                data_dir: std::path::PathBuf::from("/tmp/none"),
                container_data_dir: std::path::PathBuf::from("/home/container"),
                labels: Vec::new(),
                crash_detection_enabled: true,
                user: None,
                readonly_rootfs: None,
                cap_drop: None,
            })
            .await
            .unwrap();
        assert_eq!(runtime.kind(), RuntimeKind::Container);
        // Every operation fails as Unavailable — never as a silent
        // success that would fake an Offline observation.
        assert!(runtime.state().await.unwrap_err().is_unavailable());
        assert!(runtime.kill().await.unwrap_err().is_unavailable());
        assert!(runtime.stats().await.unwrap_err().is_unavailable());
        assert!(runtime.attach_console().await.unwrap_err().is_unavailable());
    }
}
