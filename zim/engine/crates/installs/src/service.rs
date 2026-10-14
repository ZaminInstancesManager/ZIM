//! The install service: validation, job creation, and the
//! install-in-flight guard for the §10.3 install API surface.
//!
//! [`InstallService`] is the API-facing half (the executable half
//! is [`crate::InstallJobHandler`], registered by
//! [`InstallService::register`]). There is deliberately *no*
//! companion entity row (unlike backups and transfers): an install
//! has no identity beyond its durable job — the linkage the
//! handler needs (skip flag, script family) rides the job's JSON
//! `payload`, and progress/status reads go through the job
//! endpoints and `job.progress` events like every other family.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;

use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::{InstallRunner, Supervisor};
use zim_engine_domain::job::{Job, JobKind};
use zim_engine_domain::status::JobState;
use zim_engine_domain::{EggId, InstanceId, JobId};

use crate::handler::InstallJobHandler;

/// Service-level failures (mapped by the API layer onto §6.1 error
/// codes).
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// The store failed.
    #[error("persistence: {0}")]
    Persistence(#[from] zim_engine_application::ports::PersistenceError),
    /// No instance with that id.
    #[error("instance {0} not found")]
    InstanceNotFound(InstanceId),
    /// The instance's egg no longer exists (deleted out from under
    /// it — the persistence delete guard should prevent this).
    #[error("egg {0} (referenced by instance {1}) not found")]
    EggNotFound(EggId, InstanceId),
    /// Another install (or update) job is already in flight for the
    /// instance.
    #[error("an install or update job is already in flight for instance {0}")]
    InstallInFlight(InstanceId),
    /// No install in flight (cancel path).
    #[error("no install in flight for instance {0}")]
    NoInstallInFlight(InstanceId),
}

impl From<zim_engine_jobs::JobsError> for InstallError {
    fn from(e: zim_engine_jobs::JobsError) -> Self {
        Self::Persistence(zim_engine_application::ports::PersistenceError::backend(
            format!("job system: {e}"),
        ))
    }
}

/// Request options for one install (§10.3's optional body).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InstallOptions {
    /// Skip the egg's install/update script entirely and just mark
    /// the instance installed (the "pre-installed data" hatch;
    /// defaults to the instance row's `skip_egg_scripts` flag).
    pub skip_egg_scripts: Option<bool>,
}

/// The install API's service object (see the module docs).
pub struct InstallService {
    persistence: Arc<dyn Persistence>,
    jobs: Arc<zim_engine_jobs::JobSystem>,
    supervisor: Arc<Supervisor>,
    runner: Arc<dyn InstallRunner>,
    data_root: PathBuf,
}

impl InstallService {
    /// Build the service; call [`InstallService::register`] before
    /// jobs start flowing.
    pub fn new(
        persistence: Arc<dyn Persistence>,
        jobs: Arc<zim_engine_jobs::JobSystem>,
        supervisor: Arc<Supervisor>,
        runner: Arc<dyn InstallRunner>,
        data_root: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            persistence,
            jobs,
            supervisor,
            runner,
            data_root,
        })
    }

    /// Register the install + update job handlers on the executor
    /// (one handler discriminates on the job's kind).
    pub async fn register(&self) {
        let handler: Arc<dyn zim_engine_jobs::JobHandler> = Arc::new(InstallJobHandler::new(
            Arc::clone(&self.persistence),
            Arc::clone(&self.supervisor),
            Arc::clone(&self.runner),
            self.data_root.clone(),
        ));
        self.jobs
            .register_handler(JobKind::Install, Arc::clone(&handler))
            .await;
        self.jobs.register_handler(JobKind::Update, handler).await;
    }

    /// Initiate an install (or reinstall) of `instance_id`: the
    /// instance and its egg must exist, no other install/update job
    /// may be in flight, and the durable `Pending` job is created
    /// and enqueued. Progress is observable through the job
    /// endpoints and `job.progress` events; the handler's stages
    /// are `stopping`, `pulling_image`, and
    /// `running_install_script`.
    pub async fn initiate(
        &self,
        instance_id: InstanceId,
        opts: InstallOptions,
    ) -> Result<Job, InstallError> {
        let instance = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            let id = instance_id;
            move || persistence.get_instance(&id)
        })
        .await
        .expect("install instance read task panicked")?
        .ok_or(InstallError::InstanceNotFound(instance_id))?;

        // The egg must exist for the handler to resolve a script
        // from (even a skip-scripts install reads the row for the
        // audit trail's sake — and a dangling egg_id is a broken
        // store, not a skip).
        let egg_id = instance.egg_id;
        let egg_exists = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            move || persistence.get_egg(&egg_id)
        })
        .await
        .expect("install egg read task panicked")?
        .is_some();
        if !egg_exists {
            return Err(InstallError::EggNotFound(egg_id, instance_id));
        }

        // One install at a time per instance (a second concurrent
        // script would race the same data directory).
        if self.in_flight(instance_id).await?.is_some() {
            return Err(InstallError::InstallInFlight(instance_id));
        }

        // The skip hatch: the request body overrides the row's
        // flag (an operator's one-off), the row is the default.
        let skip = opts.skip_egg_scripts.unwrap_or(instance.skip_egg_scripts);

        let now = Utc::now();
        let job = Job {
            id: JobId::new(),
            kind: JobKind::Install,
            instance_id,
            state: JobState::Pending,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            last_error: None,
            cleanup_pending: false,
            generation: 0,
            // The install job's linkage: no companion entity, so
            // the payload carries the request's knobs.
            payload: Some(
                serde_json::json!({
                    "skip_egg_scripts": skip,
                })
                .to_string(),
            ),
        };
        let job = self.jobs.enqueue(job).await?;
        Ok(job)
    }

    /// The instance's non-terminal install/update job, when one
    /// exists *and is still live* (the executor can end a job
    /// without its handler ever running — a `Pending` job
    /// cancelled through the jobs API — so the job's state is the
    /// arbiter, exactly like the transfer service's in-flight
    /// check).
    pub async fn in_flight(&self, instance_id: InstanceId) -> Result<Option<Job>, InstallError> {
        let jobs = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            let id = instance_id;
            move || persistence.list_jobs_by_instance(&id)
        })
        .await
        .expect("install in-flight read task panicked")?;
        Ok(jobs
            .into_iter()
            .filter(|job| {
                matches!(job.kind, JobKind::Install | JobKind::Update)
                    && !job.state.is_terminal()
                    && job.state != JobState::CleanupPending
            })
            .max_by_key(|job| job.created_at))
    }
}
