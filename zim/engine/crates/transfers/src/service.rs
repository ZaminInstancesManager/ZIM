//! The transfer service: validation, row creation, and job dispatch
//! for the transfer API surface.
//!
//! [`TransferService`] is the API-facing half: it validates the
//! move (destination node exists and differs, a destination
//! allocation is provided and free, no other transfer is in
//! flight), creates the linked `Job` + `Transfer` rows (the
//! companion-entity pattern — the `Transfer` row's `job_id` points
//! at the durable job, and the job's JSON payload names the
//! transfer back), enqueues the job on the [`JobSystem`], and
//! implements cancel and status reads. The executable half — the
//! two-phase-commit state machine that moves bytes — is
//! [`crate::handler::TransferJobHandler`], registered by
//! [`TransferService::register`].

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;

use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::Supervisor;
use zim_engine_domain::job::{Job, JobKind};
use zim_engine_domain::status::{JobState, TransferState};
use zim_engine_domain::{AllocationId, InstanceId, JobId, NodeId, Transfer, TransferId};

use crate::handler::TransferJobHandler;
use crate::store::TransferStore;
use zim_engine_jobs::{CancelStatus, JobSystem};

/// Service-level failures (mapped by the API layer onto §6.1 error
/// codes).
#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    /// The store failed.
    #[error("persistence: {0}")]
    Persistence(#[from] zim_engine_application::ports::PersistenceError),
    /// No instance with that id.
    #[error("instance {0} not found")]
    InstanceNotFound(InstanceId),
    /// No node with that id.
    #[error("node {0} not found")]
    NodeNotFound(NodeId),
    /// The destination is the instance's current node.
    #[error("instance {0} is already on node {1}")]
    SameNode(InstanceId, NodeId),
    /// Another transfer is already in flight for the instance.
    #[error("a transfer is already in flight for instance {0}")]
    TransferInFlight(InstanceId),
    /// The requested allocation does not exist (or belongs to
    /// another node, or is already assigned).
    #[error("allocation {0} is not a free allocation on the destination node")]
    AllocationNotAvailable(AllocationId),
    /// No transfer is in flight for the instance.
    #[error("no transfer in flight for instance {0}")]
    NoTransferInFlight(InstanceId),
    /// The transfer (or its job) can no longer be cancelled.
    #[error("transfer for instance {0} cannot be cancelled in its current state")]
    NotCancellable(InstanceId),
}

impl From<zim_engine_jobs::JobsError> for TransferError {
    fn from(e: zim_engine_jobs::JobsError) -> Self {
        Self::Persistence(zim_engine_application::ports::PersistenceError::backend(
            format!("job system: {e}"),
        ))
    }
}

/// The transfer API's service object (see the module docs).
pub struct TransferService {
    persistence: Arc<dyn Persistence>,
    jobs: Arc<JobSystem>,
    supervisor: Arc<Supervisor>,
    store: TransferStore,
    data_root: PathBuf,
}

impl TransferService {
    /// Build the service; call [`TransferService::register`] before
    /// jobs start flowing.
    pub fn new(
        persistence: Arc<dyn Persistence>,
        jobs: Arc<JobSystem>,
        supervisor: Arc<Supervisor>,
        data_root: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            store: TransferStore::new(&data_root),
            persistence,
            jobs,
            supervisor,
            data_root,
        })
    }

    /// Register the transfer job handler on the executor.
    pub async fn register(&self) {
        self.jobs
            .register_handler(
                JobKind::Transfer,
                Arc::new(TransferJobHandler::new(
                    Arc::clone(&self.persistence),
                    Arc::clone(&self.supervisor),
                    self.store.clone(),
                    self.data_root.clone(),
                )),
            )
            .await;
    }

    /// Initiate a transfer of `instance_id` to `to_node_id`, landing
    /// on `allocation_id` (which must be a free allocation on the
    /// destination node). Creates the `Pending` `Transfer` row and
    /// its durable job, enqueues both, and returns them; the state
    /// machine's progress is observable through
    /// `GET /instances/{id}/transfer` and the job endpoints.
    pub async fn initiate(
        &self,
        instance_id: InstanceId,
        to_node_id: NodeId,
        allocation_id: AllocationId,
    ) -> Result<(Job, Transfer), TransferError> {
        let persistence = Arc::clone(&self.persistence);
        let instance = tokio::task::spawn_blocking({
            let id = instance_id;
            move || persistence.get_instance(&id)
        })
        .await
        .expect("transfer instance read task panicked")?
        .ok_or(TransferError::InstanceNotFound(instance_id))?;

        // The destination must be a real, different node.
        if to_node_id == instance.node_id {
            return Err(TransferError::SameNode(instance_id, to_node_id));
        }
        let persistence = Arc::clone(&self.persistence);
        let node = tokio::task::spawn_blocking({
            let id = to_node_id;
            move || persistence.get_node(&id)
        })
        .await
        .expect("transfer node read task panicked")?
        .ok_or(TransferError::NodeNotFound(to_node_id))?;

        // One transfer at a time per instance (the state machine
        // serializes against itself; a second concurrent move would
        // race the live directory).
        if let Some(existing) = self.in_flight(instance_id).await? {
            return Err(TransferError::TransferInFlight(existing.instance_id));
        }

        // The landing allocation: on the destination node, and free.
        let persistence = Arc::clone(&self.persistence);
        let allocation = tokio::task::spawn_blocking({
            let id = allocation_id;
            move || persistence.get_allocation(&id)
        })
        .await
        .expect("transfer allocation read task panicked")?
        .ok_or(TransferError::AllocationNotAvailable(allocation_id))?;
        if allocation.node_id != node.id || allocation.assigned_to.is_some() {
            return Err(TransferError::AllocationNotAvailable(allocation_id));
        }

        let now = Utc::now();
        let transfer_id = TransferId::new();
        let job = Job {
            id: JobId::new(),
            kind: JobKind::Transfer,
            instance_id,
            state: JobState::Pending,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            last_error: None,
            cleanup_pending: false,
            generation: 0,
            // The transfer job's linkage: the payload names the
            // transfer row (the Transfer row's job_id names the job
            // back — the companion-entity pattern).
            payload: Some(
                serde_json::json!({
                    "transfer_id": transfer_id.to_string(),
                    "allocation_id": allocation_id.to_string(),
                })
                .to_string(),
            ),
        };
        let transfer = Transfer {
            id: transfer_id,
            instance_id,
            job_id: job.id,
            from_node_id: instance.node_id,
            to_node_id: node.id,
            state: TransferState::Pending,
            archive_path: None,
        };

        // Job row first (the transfer row's FK points at it), then
        // the transfer row; the enqueue's put_job is an idempotent
        // upsert that also wakes the queue.
        let persistence = Arc::clone(&self.persistence);
        let job_row = job.clone();
        tokio::task::spawn_blocking(move || persistence.put_job(&job_row))
            .await
            .expect("job row task panicked")?;
        let persistence = Arc::clone(&self.persistence);
        let transfer_row = transfer.clone();
        tokio::task::spawn_blocking(move || persistence.put_transfer(&transfer_row))
            .await
            .expect("transfer row task panicked")?;

        let job = self.jobs.enqueue(job).await?;
        Ok((job, transfer))
    }

    /// Cancel the instance's in-flight transfer: requests
    /// cancellation of the driving job (the handler observes the
    /// token between phases and unwinds at the next safe point —
    /// after the commit point the move is already content-complete,
    /// so cancellation there only stops the waiting).
    ///
    /// When the job cancels straight out of `Pending` (or out of
    /// the claimed-but-never-started crash window) no handler ever
    /// runs, so nothing would walk the `Transfer` row to a terminal
    /// state — this method does it itself, mirroring the job's
    /// terminal state onto the row so later status reads (and the
    /// in-flight check) stay honest.
    pub async fn cancel(&self, instance_id: InstanceId) -> Result<Job, TransferError> {
        let transfer = self
            .in_flight(instance_id)
            .await?
            .ok_or(TransferError::NoTransferInFlight(instance_id))?;
        let status = self.jobs.cancel(transfer.job_id).await?;
        let persistence = Arc::clone(&self.persistence);
        let job = tokio::task::spawn_blocking({
            let id = transfer.job_id;
            move || persistence.get_job(&id)
        })
        .await
        .expect("cancel job read task panicked")?
        .ok_or(TransferError::NoTransferInFlight(instance_id))?;
        if status == CancelStatus::Cancelled {
            // The job is terminal and no handler will touch the row
            // again — mirror the job's outcome onto it.
            let terminal = match job.state {
                JobState::Cancelled => TransferState::Cancelled,
                _ => TransferState::Failed,
            };
            let mut row = transfer.clone();
            row.state = terminal;
            let persistence = Arc::clone(&self.persistence);
            let updated = row.clone();
            tokio::task::spawn_blocking(move || persistence.put_transfer(&updated))
                .await
                .expect("cancel transfer row write task panicked")?;
        }
        Ok(job)
    }

    /// The instance's current status row: the in-flight transfer
    /// when one exists, else the most recent terminal one (the
    /// API's `GET /instances/{id}/transfer` source). A non-terminal
    /// row whose driving job already ended (a `Pending` job
    /// cancelled through the jobs API never runs its handler) is
    /// reported with the job's terminal state — the job is the
    /// truth, the row is the view.
    pub async fn status(&self, instance_id: InstanceId) -> Result<Option<Transfer>, TransferError> {
        if let Some(live) = self.in_flight(instance_id).await? {
            return Ok(Some(live));
        }
        let persistence = Arc::clone(&self.persistence);
        let transfers = tokio::task::spawn_blocking({
            let id = instance_id;
            move || persistence.list_transfers_by_instance(&id)
        })
        .await
        .expect("transfer status read task panicked")?;
        let Some(mut row) = transfers.into_iter().next() else {
            return Ok(None);
        };
        if !row.state.is_terminal() {
            // Stale view: the row's job ended without the handler
            // walking the row. Report what the job says.
            let persistence = Arc::clone(&self.persistence);
            let job_id = row.job_id;
            let job = tokio::task::spawn_blocking(move || persistence.get_job(&job_id))
                .await
                .expect("transfer status job read task panicked")?;
            if let Some(job) = job {
                row.state = match job.state {
                    JobState::Succeeded => TransferState::Succeeded,
                    JobState::Cancelled => TransferState::Cancelled,
                    _ if job.state.is_terminal() || job.state == JobState::CleanupPending => {
                        TransferState::Failed
                    }
                    // The job is still live after all (a row
                    // pre-created before its job): keep the row's
                    // own state.
                    _ => row.state,
                };
            }
        }
        Ok(Some(row))
    }

    /// The instance's non-terminal transfer row, when one exists
    /// *and its driving job is still live*. The executor can end a
    /// job without its handler ever running (a `Pending` job
    /// cancelled through the jobs API moves straight to
    /// `Cancelled`), which would otherwise leave the row `Pending`
    /// forever — treating that orphan as in flight would block the
    /// instance's next transfer permanently, so the job's state is
    /// the arbiter.
    pub async fn in_flight(
        &self,
        instance_id: InstanceId,
    ) -> Result<Option<Transfer>, TransferError> {
        let persistence = Arc::clone(&self.persistence);
        let transfers = tokio::task::spawn_blocking({
            let id = instance_id;
            move || persistence.list_transfers_by_instance(&id)
        })
        .await
        .expect("in-flight transfer read task panicked")?;
        for transfer in transfers.into_iter() {
            if transfer.state.is_terminal() {
                continue;
            }
            let persistence = Arc::clone(&self.persistence);
            let job_id = transfer.job_id;
            let job = tokio::task::spawn_blocking(move || persistence.get_job(&job_id))
                .await
                .expect("in-flight transfer job read task panicked")?;
            let job_live = job
                .map(|job| !job.state.is_terminal() && job.state != JobState::CleanupPending)
                .unwrap_or(false);
            if job_live {
                return Ok(Some(transfer));
            }
        }
        Ok(None)
    }
}
