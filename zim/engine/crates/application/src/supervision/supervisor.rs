//! The supervisor: the registry of supervised instances, the API's
//! power-operation front door, and the engine-restart reconciler.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::ports::Persistence;
use crate::supervision::instance::{
    build_instance_config, ObservedSnapshot, PowerAction, PowerError, SupervisedInstance,
    SupervisionPolicy,
};
use crate::supervision::runtime::RuntimeFactory;
use crate::supervision::state::{DesiredState, InstanceConfig, ObservedState, RuntimeError};
use zim_engine_domain::{InstanceStatus, NodeDeployment, NodeId};

/// The outcome of an engine-restart reconciliation pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Instances now supervised.
    pub supervised: usize,
    /// Instances observed running at reconcile time (adopted).
    pub observed_running: usize,
    /// Restarts queued for was-desired-started instances found
    /// offline (subject to crash-loop protection).
    pub queued_restarts: usize,
    /// Instances that entered (or stayed in) crash-loop.
    pub crash_loops: usize,
    /// Instances whose runtime could not be reached.
    pub unknown: usize,
    /// Instances on non-local nodes (not supervised by this engine).
    pub skipped_remote: usize,
}

/// The supervision registry.
///
/// Owns one [`SupervisedInstance`] per instance on this engine's
/// local nodes; fronts power requests (the API layer calls
/// [`Self::request_power`]); and reconciles persisted vs. observed
/// state on boot (invariants 1 and 10).
pub struct Supervisor {
    inner: Arc<SupervisorInner>,
}

struct SupervisorInner {
    instances: RwLock<HashMap<zim_engine_domain::InstanceId, SupervisedInstance>>,
    factory: Arc<dyn RuntimeFactory>,
    persistence: Arc<dyn Persistence>,
    policy: SupervisionPolicy,
    data_root: PathBuf,
}

impl Supervisor {
    /// Assemble the supervisor over its ports.
    pub fn new(
        persistence: Arc<dyn Persistence>,
        factory: Arc<dyn RuntimeFactory>,
        policy: SupervisionPolicy,
        data_root: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(SupervisorInner {
                instances: RwLock::new(HashMap::new()),
                factory,
                persistence,
                policy,
                data_root,
            }),
        })
    }

    /// The supervision policy (for tests and composition roots that
    /// need the effective tunables).
    pub fn policy(&self) -> &SupervisionPolicy {
        &self.inner.policy
    }

    /// Boot: load every local-node instance, then reconcile.
    ///
    /// Composition roots call this once at engine startup; it is the
    /// invariant-1/10 gate — after it returns, every supervised
    /// instance's observed state comes from the runtime, never from
    /// the persisted row.
    pub async fn boot(&self) -> Result<ReconcileReport, RuntimeError> {
        self.load_local().await?;
        self.reconcile().await
    }

    /// Load (supervise) every instance on local-deployment nodes.
    ///
    /// Loading does not start anything and does not trust persisted
    /// status: instances load in the offline seed; [`Self::reconcile`]
    /// decides what is actually running.
    pub async fn load_local(&self) -> Result<usize, RuntimeError> {
        let nodes = blocking_read(&self.inner.persistence, |p| p.list_nodes()).await?;

        let mut loaded = 0usize;
        for node in nodes {
            if node.deployment != NodeDeployment::LocalLinux {
                continue;
            }
            let instances = blocking_read(&self.inner.persistence, {
                let id = node.id;
                move |p| p.list_instances_by_node(&id)
            })
            .await?;
            for instance in instances {
                if self.ensure_supervised(&instance).await.is_ok() {
                    loaded += 1;
                }
            }
        }
        Ok(loaded)
    }

    /// Create (or refresh) the supervised instance for a persisted
    /// row. Returns the handle.
    async fn ensure_supervised(
        &self,
        instance: &zim_engine_domain::Instance,
    ) -> Result<SupervisedInstance, RuntimeError> {
        // Fast path: already supervised — refresh the config from the
        // row so a patched image/limit/startup applies on next start.
        if let Some(existing) = self.inner.instances.read().await.get(&instance.id) {
            let cfg = self.build_config(instance).await?;
            existing.refresh_config(cfg);
            return Ok(existing.clone());
        }

        let cfg = self.build_config(instance).await?;
        let runtime = self.inner.factory.create(&cfg).await?;
        let supervised = SupervisedInstance::spawn(
            cfg,
            runtime,
            Arc::clone(&self.inner.persistence),
            self.inner.policy.clone(),
        );
        self.inner
            .instances
            .write()
            .await
            .insert(instance.id, supervised.clone());
        Ok(supervised)
    }

    /// Build the runtime-facing config for a persisted row (reads the
    /// row's allocations; a missing primary allocation degrades to an
    /// unpublished-ports container with a logged warning).
    async fn build_config(
        &self,
        instance: &zim_engine_domain::Instance,
    ) -> Result<InstanceConfig, RuntimeError> {
        let id = instance.id;
        let primary = blocking_read(&self.inner.persistence, {
            move |p| p.find_allocation_by_instance(&id)
        })
        .await?;

        let default_allocation = match primary {
            Some(allocation) => allocation,
            None => {
                eprintln!(
                    "zim_engine: supervision: instance {} has no primary allocation; \
                     container will start without published ports",
                    instance.id
                );
                zim_engine_domain::Allocation {
                    id: instance.allocation_id,
                    node_id: instance.node_id,
                    ip: "127.0.0.1".parse().expect("loopback parses"),
                    port: 0,
                    assigned_to: None,
                    note: None,
                }
            }
        };

        build_instance_config(instance, &default_allocation, &[], &self.inner.data_root)
    }

    /// Engine-restart reconciliation (invariants 1 and 10).
    ///
    /// For every supervised instance: observe the runtime; adopt a
    /// running target; for a was-desired-started row found offline,
    /// record the crash and queue a restart unless the crash-loop
    /// protection engages.
    pub async fn reconcile(&self) -> Result<ReconcileReport, RuntimeError> {
        let mut report = ReconcileReport::default();
        let nodes = blocking_read(&self.inner.persistence, |p| p.list_nodes()).await?;

        let mut rows = Vec::new();
        for node in nodes {
            let instances = blocking_read(&self.inner.persistence, {
                let id: NodeId = node.id;
                move |p| p.list_instances_by_node(&id)
            })
            .await?;
            if node.deployment == NodeDeployment::LocalLinux {
                rows.extend(instances);
            } else {
                report.skipped_remote += instances.len();
            }
        }
        report.supervised = rows.len();

        for row in rows {
            let supervised = self.ensure_supervised(&row).await?;

            // Invariant 1: the persisted row's status seeded nothing;
            // what follows is purely the runtime's observation.
            let observed = match supervised.runtime_state().await {
                Ok(state) => state,
                Err(e) if e.is_unavailable() => {
                    supervised.set_observed_direct(ObservedState::Unknown);
                    supervised.persist_status_now().await;
                    report.unknown += 1;
                    continue;
                }
                Err(e) => {
                    // Transient backend errors also surface as Unknown
                    // for this pass (never Offline); the next
                    // transition or poll re-observes.
                    eprintln!(
                        "zim_engine: supervision: instance {} reconcile observation failed: {e}",
                        row.id
                    );
                    supervised.set_observed_direct(ObservedState::Unknown);
                    supervised.persist_status_now().await;
                    report.unknown += 1;
                    continue;
                }
            };

            if observed.is_running() {
                // Adopt: it is alive, so desired reflects running.
                supervised.set_observed_direct(observed);
                supervised.set_desired_direct(DesiredState::Started);
                supervised.persist_status_now().await;
                report.observed_running += 1;
                supervised.arm_monitor_now();
                continue;
            }

            // Offline (or created-but-never-run) target.
            let was_desired_started = matches!(
                row.status,
                InstanceStatus::Running | InstanceStatus::Starting
            );
            if was_desired_started {
                // Invariant 10: queue a restart, subject to crash-loop
                // protection. The crash counts: an engine restart
                // while the instance silently died is a crash event
                // for boot-loop protection purposes.
                let tripped = supervised.record_crash_now();
                supervised.set_observed_direct(observed);
                if tripped {
                    supervised.persist_status_now().await;
                    report.crash_loops += 1;
                    continue;
                }
                supervised.set_desired_direct(DesiredState::Started);
                supervised.persist_status_now().await;
                let _ = supervised.power_tx().send(PowerAction::Start).await;
                report.queued_restarts += 1;
            } else {
                // Was-desired-stopped (or suspended): stay stopped.
                supervised.set_observed_direct(observed);
                if row.status == InstanceStatus::Suspended {
                    supervised.set_desired_direct(DesiredState::Suspended);
                }
                supervised.persist_status_now().await;
            }
        }

        Ok(report)
    }

    /// Look up (or lazily supervise) the instance for `id`.
    ///
    /// Instances created after engine boot enter supervision here:
    /// the row is read, its node checked (`LocalLinux` only —
    /// remote-node instances are not this engine's to run), and the
    /// runtime handle created on demand. `Ok(None)` = not
    /// supervisable from here (missing row, or a remote node's
    /// instance).
    pub async fn ensure_supervised_id(
        &self,
        instance_id: &zim_engine_domain::InstanceId,
    ) -> Result<Option<SupervisedInstance>, RuntimeError> {
        if let Some(existing) = self.inner.instances.read().await.get(instance_id) {
            return Ok(Some(existing.clone()));
        }
        let id = *instance_id;
        let row = match blocking_read(&self.inner.persistence, move |p| p.get_instance(&id)).await {
            Ok(Some(row)) => row,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e),
        };
        // Gate on the node's deployment shape.
        let node_id = row.node_id;
        let node = blocking_read(&self.inner.persistence, move |p| {
            p.list_nodes()
                .map(|nodes| nodes.into_iter().find(|n| n.id == node_id))
        })
        .await?;
        match node {
            Some(node) if node.deployment == NodeDeployment::LocalLinux => {
                Ok(Some(self.ensure_supervised(&row).await?))
            }
            _ => Ok(None),
        }
    }

    /// Request a power operation (the API layer's front door).
    ///
    /// Acceptance is asynchronous (invariant 2): the returned
    /// generation identifies the pre-action state; the action's
    /// outcome is observable through [`Self::observed`].
    pub async fn request_power(
        &self,
        instance_id: &zim_engine_domain::InstanceId,
        action: PowerAction,
    ) -> Result<u64, PowerError> {
        let supervised = self
            .ensure_supervised_id(instance_id)
            .await
            .map_err(|e| PowerError::Backend(e.to_string()))?
            .ok_or(PowerError::UnknownInstance(*instance_id))?;

        // Fail fast when the backend is unreachable (503 at the API;
        // never accept an action the queue cannot act on).
        self.inner
            .factory
            .check()
            .await
            .map_err(|e| PowerError::RuntimeUnavailable(e.to_string()))?;

        let starts = matches!(action, PowerAction::Start | PowerAction::Restart { .. });
        if starts {
            if supervised.is_suspended() {
                return Err(PowerError::Suspended(*instance_id));
            }
            // A user-issued start clears the crash loop (invariant
            // 14's "user intervention").
            supervised.clear_crash_state();
            // Refresh the configuration from the persisted row so a
            // patched image/limit/startup applies to this start.
            if let Ok(row) = blocking_read(&self.inner.persistence, {
                let id = *instance_id;
                move |p| p.get_instance(&id)
            })
            .await
            {
                // Nested rather than a let-chain (the workspace pins
                // edition 2021, where let-chains are unavailable).
                #[allow(clippy::collapsible_if, clippy::collapsible_match)]
                if let Some(row) = row {
                    #[allow(clippy::collapsible_if)]
                    if let Ok(cfg) = self.build_config(&row).await {
                        supervised.refresh_config(cfg);
                    }
                }
            }
        }

        let generation = supervised.generation();
        supervised
            .power_tx()
            .send(action)
            .await
            .map_err(|e| PowerError::Backend(e.to_string()))?;
        Ok(generation)
    }

    /// The observed-state snapshot for an instance (the API's
    /// `GET /instances/{id}/state` source; falls back to the caller's
    /// persistence read when the instance is not supervised here).
    pub async fn observed(
        &self,
        instance_id: &zim_engine_domain::InstanceId,
    ) -> Result<ObservedSnapshot, PowerError> {
        if let Some(supervised) = self.inner.instances.read().await.get(instance_id) {
            return Ok(supervised.snapshot());
        }
        // Lazily supervise (post-boot creations), then re-read.
        self.ensure_supervised_id(instance_id)
            .await
            .map_err(|e| PowerError::Backend(e.to_string()))?
            .map(|supervised| supervised.snapshot())
            .ok_or(PowerError::UnknownInstance(*instance_id))
    }

    /// Whether an instance is currently supervised.
    pub async fn is_supervised(&self, instance_id: &zim_engine_domain::InstanceId) -> bool {
        self.inner.instances.read().await.contains_key(instance_id)
    }

    /// Stop every supervised instance's worker and monitor
    /// (engine shutdown; containers are left running by design).
    pub async fn shutdown(&self) {
        let instances: Vec<SupervisedInstance> = self
            .inner
            .instances
            .write()
            .await
            .drain()
            .map(|(_, v)| v)
            .collect();
        for supervised in instances {
            supervised.shutdown().await;
        }
    }
}

/// Bridge a synchronous persistence read onto the async runtime (the
/// M3 `spawn_blocking` decision, applied at the supervision layer).
///
/// Both failure layers (the join itself and the persistence call)
/// map into the runtime error vocabulary.
async fn blocking_read<T, F>(persistence: &Arc<dyn Persistence>, f: F) -> Result<T, RuntimeError>
where
    T: Send + 'static,
    F: FnOnce(&dyn Persistence) -> Result<T, crate::ports::PersistenceError> + Send + 'static,
{
    let persistence = Arc::clone(persistence);
    tokio::task::spawn_blocking(move || f(persistence.as_ref()))
        .await
        .map_err(|e| RuntimeError::Backend(format!("persistence task join error: {e}")))?
        .map_err(runtime_backend)
}

/// Map persistence failures into the runtime error vocabulary.
fn runtime_backend(e: crate::ports::PersistenceError) -> RuntimeError {
    RuntimeError::Backend(format!("persistence read failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_report_is_default_constructible() {
        let report = ReconcileReport::default();
        assert_eq!(report.supervised, 0);
        assert_eq!(report.queued_restarts, 0);
    }
}
