//! Starr background worker (ADR 0004): executes jobs from the queue in
//! `starr-db`, records every execution as a run, and enqueues scheduled jobs.
//!
//! The worker owns execution; the web process only enqueues and may wake the
//! worker through the shared [`Notify`] (single-process mode, ADR 0003). In
//! split mode the worker cannot be woken in memory; it checks the queue
//! cheaply every [`WorkerConfig::due_check`] instead.
//!
//! Exactly one worker is active: a worker executes jobs only while it holds
//! the database-backed worker lease (`starr_db::worker_lease`). Any further
//! worker process waits, and takes over when the lease is released or
//! expires.
//!
//! One job runs at a time. A job holds a lease that a heartbeat renews while
//! its handler runs; the handler gets a [`CancellationToken`] that fires on
//! timeout, lost lease or shutdown.

mod account_writes;
mod accounts;
pub mod adapters;
pub mod catalog;
mod follows;
mod handlers;
pub mod image_cache;
mod inventory;
pub mod notifications;
mod passwords;
pub mod provisioning;
mod requests;
pub mod secrets;
mod services;
mod sources;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use starr_core::jobs::{JobKind, JobSpec, Report, ReportError, Trigger};
use starr_db::jobs::{ClaimedJob, FailOutcome, NewJob};
use starr_db::runs::RunEnd;
use starr_db::{Database, DbError};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub use handlers::{DEFAULT_IMAGE_CACHE_BYTES, SystemJobs};

/// Everything a handler gets for one execution.
#[derive(Debug, Clone)]
pub struct JobContext {
    pub db: Database,
    /// The job's JSON payload.
    pub payload: serde_json::Value,
    /// Fires on timeout, lost lease or shutdown. Long handlers must check it
    /// and return [`JobError::Cancelled`].
    pub cancel: CancellationToken,
}

/// Why a handler did not produce a report.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Report(#[from] ReportError),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
}

/// Executes job kinds. Production uses [`SystemJobs`]; tests substitute
/// handlers with controlled behaviour and short timeouts.
pub trait JobHandler: Send + Sync {
    /// Timeout, retries and schedule for `kind`.
    fn spec(&self, kind: JobKind) -> JobSpec {
        kind.spec()
    }

    /// Run one execution. Returning a report is the only way to succeed: a
    /// run without a report counts as failed (lesson 28).
    fn run(
        &self,
        kind: JobKind,
        ctx: JobContext,
    ) -> impl Future<Output = Result<Report, JobError>> + Send;

    /// Whether scheduled kind `kind` has anything to do now; `false` skips
    /// enqueueing it for this tick. A job already queued still runs.
    fn has_work(
        &self,
        kind: JobKind,
        db: &Database,
    ) -> impl Future<Output = Result<bool, DbError>> + Send {
        let _ = (kind, db);
        async { Ok(true) }
    }

    /// Called each time this worker becomes the active worker, before it
    /// executes jobs: the place for worker-wide side effects that only the
    /// active worker may have. An error gives the lease back.
    fn on_lease_acquired(&self, db: &Database) -> impl Future<Output = Result<(), DbError>> + Send {
        let _ = db;
        async { Ok(()) }
    }
}

/// Worker loop errors (database trouble); the loop logs them and continues.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Timing of the worker loop.
#[derive(Debug, Clone, Copy)]
pub struct WorkerConfig {
    /// How often the queue is checked when nobody wakes the worker.
    pub poll_interval: Duration,
    /// Split mode: how often to look for a claimable job with one cheap
    /// read in between full polls, since jobs enqueued by the web process
    /// cannot wake this one. `None` in single-process mode.
    pub due_check: Option<Duration>,
    /// Lease length, for job leases and the worker lease alike: a job, or
    /// the worker role, whose holder stops heartbeating is free after this.
    pub lease: Duration,
    /// How often leases are renewed (well below `lease`); also how often a
    /// waiting worker tries to take the worker lease.
    pub heartbeat: Duration,
    /// How long a running handler gets to react to shutdown before it is
    /// dropped.
    pub shutdown_grace: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            due_check: None,
            lease: Duration::from_secs(60),
            heartbeat: Duration::from_secs(15),
            shutdown_grace: Duration::from_secs(10),
        }
    }
}

fn delta(d: Duration) -> chrono::TimeDelta {
    chrono::TimeDelta::from_std(d).unwrap_or(chrono::TimeDelta::MAX)
}

/// How one execution ended, before it is recorded.
#[derive(Debug)]
enum Outcome {
    Finished(Result<Report, JobError>),
    TimedOut(Duration),
    LeaseLost,
    /// Shutdown and the handler did not stop within the grace period.
    ShutdownGraceExceeded,
}

/// The worker loop. Run it with [`Worker::run`].
#[derive(Debug)]
pub struct Worker<H> {
    db: Database,
    handler: H,
    wake: Arc<Notify>,
    config: WorkerConfig,
    owner: String,
}

impl<H: JobHandler> Worker<H> {
    /// `wake` is shared with whoever enqueues jobs in this process; they call
    /// `notify_one()` after enqueueing so the job starts without waiting for
    /// the next poll.
    pub fn new(db: Database, handler: H, wake: Arc<Notify>, config: WorkerConfig) -> Self {
        Self {
            db,
            handler,
            wake,
            config,
            owner: format!("worker-{}", uuid::Uuid::new_v4()),
        }
    }

    /// Process jobs until `shutdown` fires, while holding the worker lease.
    /// A job running at that moment gets its cancellation token fired and
    /// [`WorkerConfig::shutdown_grace`] to finish; unfinished jobs are
    /// released back to the queue, and so is the worker lease.
    pub async fn run(&self, shutdown: CancellationToken) {
        tracing::info!(owner = %self.owner, "worker started");
        while self.acquire_lease(&shutdown).await {
            // Fires on shutdown or when the worker lease is lost.
            let active = shutdown.child_token();
            tokio::join!(self.keep_lease(&active), self.process(&active));
        }
        match self.db.release_worker_lease(&self.owner).await {
            Ok(true) => tracing::info!(owner = %self.owner, "worker lease released"),
            Ok(false) => {}
            Err(err) => tracing::warn!(
                error = %err,
                "releasing the worker lease failed; another worker takes over once it expires"
            ),
        }
        tracing::info!(owner = %self.owner, "worker stopped");
    }

    /// Wait until this worker holds the worker lease. `false` on shutdown.
    async fn acquire_lease(&self, shutdown: &CancellationToken) -> bool {
        let mut reported_holder: Option<String> = None;
        while !shutdown.is_cancelled() {
            match self
                .db
                .acquire_worker_lease(&self.owner, Utc::now(), self.config.lease)
                .await
            {
                Ok(true) => match self.handler.on_lease_acquired(&self.db).await {
                    Ok(()) => {
                        tracing::info!(owner = %self.owner, "worker lease acquired; this worker executes jobs");
                        return true;
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "preparing the active worker failed; retrying");
                        let _ = self.db.release_worker_lease(&self.owner).await;
                    }
                },
                Ok(false) => match self.db.worker_lease_holder().await {
                    Ok(Some(holder)) if reported_holder.as_ref() != Some(&holder.owner) => {
                        tracing::warn!(
                            holder = %holder.owner,
                            expires_at = %holder.expires_at,
                            "another worker is active; this worker executes no jobs and takes over when that worker stops or its lease expires"
                        );
                        reported_holder = Some(holder.owner);
                    }
                    Ok(_) => {}
                    Err(err) => tracing::error!(error = %err, "reading the worker lease failed"),
                },
                Err(err) => tracing::error!(error = %err, "acquiring the worker lease failed"),
            }
            tokio::select! {
                () = shutdown.cancelled() => {}
                () = tokio::time::sleep(self.config.heartbeat) => {}
            }
        }
        false
    }

    /// Renew the worker lease until `active` fires. Cancels `active` when the
    /// lease is lost: taken over by another worker, or not renewable before
    /// it expired.
    async fn keep_lease(&self, active: &CancellationToken) {
        // When the last successful renewal was sent (its expiry counts from then).
        let mut renewed = tokio::time::Instant::now();
        loop {
            tokio::select! {
                () = active.cancelled() => return,
                () = tokio::time::sleep(self.config.heartbeat) => {}
            }
            let attempt = tokio::time::Instant::now();
            // Renew only: a lease lost meanwhile is never silently re-taken,
            // so `on_lease_acquired` runs on every acquisition.
            match self
                .db
                .renew_worker_lease(&self.owner, Utc::now(), self.config.lease)
                .await
            {
                Ok(true) => renewed = attempt,
                Ok(false) => {
                    tracing::error!(owner = %self.owner, "this worker lost the worker lease; it stops executing jobs");
                    active.cancel();
                    return;
                }
                // Stop before the lease can lapse, not after: the next
                // attempt would come one heartbeat too late.
                Err(err) if renewed.elapsed() + self.config.heartbeat >= self.config.lease => {
                    tracing::error!(error = %err, "the worker lease expired before it could be renewed; this worker stops executing jobs");
                    active.cancel();
                    return;
                }
                // The lease is still valid for a while; try again.
                Err(err) => tracing::warn!(error = %err, "renewing the worker lease failed"),
            }
        }
    }

    /// The job loop of the active worker, until `active` fires.
    async fn process(&self, active: &CancellationToken) {
        while !active.is_cancelled() {
            match self.tick(active).await {
                // Look for the next job right away.
                Ok(true) => continue,
                Ok(false) => {}
                Err(err) => tracing::error!(error = %err, "worker iteration failed"),
            }
            tokio::select! {
                biased;
                () = active.cancelled() => break,
                () = self.wake.notified() => {}
                () = self.claimable_job() => {}
                () = tokio::time::sleep(self.config.poll_interval) => {}
            }
        }
    }

    /// Resolves once a cheap check finds a claimable job (split mode); never
    /// without [`WorkerConfig::due_check`].
    async fn claimable_job(&self) {
        let Some(every) = self.config.due_check else {
            return std::future::pending().await;
        };
        let kinds = JobKind::ALL.map(JobKind::name);
        loop {
            tokio::time::sleep(every).await;
            match self.db.has_claimable_job(&kinds, Utc::now()).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(err) => tracing::warn!(error = %err, "checking the queue failed"),
            }
        }
    }

    /// One iteration: clean up dead jobs, enqueue due scheduled jobs, then
    /// claim and execute at most one job. `true` if a job was executed.
    pub async fn tick(&self, shutdown: &CancellationToken) -> Result<bool, WorkerError> {
        let now = Utc::now();
        let reaped = self.db.reap_exhausted_jobs(now).await?;
        if reaped > 0 {
            tracing::warn!(
                count = reaped,
                "removed jobs whose last attempt was abandoned"
            );
        }
        self.schedule_due().await?;

        let kinds = JobKind::ALL.map(JobKind::name);
        let Some(job) = self
            .db
            .claim_job(&self.owner, &kinds, Utc::now(), self.config.lease)
            .await?
        else {
            return Ok(false);
        };
        self.execute(job, shutdown).await?;
        Ok(true)
    }

    /// Enqueue every scheduled kind for one interval after its last run (or
    /// now, if it never ran). The kind's unique key makes this idempotent
    /// across ticks, restarts and concurrent workers.
    async fn schedule_due(&self) -> Result<(), WorkerError> {
        let now = Utc::now();
        for kind in JobKind::ALL {
            let spec = self.handler.spec(kind);
            let Some(interval) = spec.schedule else {
                continue;
            };
            if !self.handler.has_work(kind, &self.db).await? {
                continue;
            }
            let run_after = match self.db.last_run_started(kind.name()).await? {
                Some(last) => last + delta(interval),
                None => now,
            };
            let key = kind.unique_key();
            self.db
                .enqueue_job(
                    &NewJob {
                        kind: kind.name(),
                        payload: "{}",
                        unique_key: Some(&key),
                        trigger: Trigger::Schedule,
                        run_after,
                        max_attempts: spec.max_attempts,
                    },
                    now,
                )
                .await?;
        }
        Ok(())
    }

    async fn execute(
        &self,
        job: ClaimedJob,
        shutdown: &CancellationToken,
    ) -> Result<(), WorkerError> {
        let run_id = self.db.start_run(&job, Utc::now()).await?;

        // `claim_job` only returns kinds this binary knows.
        let Some(kind) = JobKind::from_name(&job.kind) else {
            return self
                .record_failure(&job, run_id, Duration::ZERO, "unknown job kind")
                .await;
        };
        let spec = self.handler.spec(kind);
        let payload = match serde_json::from_str(&job.payload) {
            Ok(payload) => payload,
            Err(err) => {
                let msg = format!("invalid payload: {err}");
                return self.record_failure(&job, run_id, spec.backoff, &msg).await;
            }
        };
        let cancel = shutdown.child_token();
        let ctx = JobContext {
            db: self.db.clone(),
            payload,
            cancel: cancel.clone(),
        };
        tracing::info!(kind = %job.kind, job_id = job.id, run_id, attempt = job.attempt, "job started");
        let outcome = self.drive(kind, spec, &job, ctx, shutdown).await;
        cancel.cancel();
        self.record(&job, run_id, spec, outcome, shutdown).await
    }

    /// Run the handler while renewing the lease, enforcing the timeout and the
    /// shutdown grace period.
    async fn drive(
        &self,
        kind: JobKind,
        spec: JobSpec,
        job: &ClaimedJob,
        ctx: JobContext,
        shutdown: &CancellationToken,
    ) -> Outcome {
        let cancel = ctx.cancel.clone();
        let handler = self.handler.run(kind, ctx);
        tokio::pin!(handler);
        let deadline = tokio::time::sleep(spec.timeout);
        tokio::pin!(deadline);
        let grace = async {
            shutdown.cancelled().await;
            tokio::time::sleep(self.config.shutdown_grace).await;
        };
        tokio::pin!(grace);
        let start = tokio::time::Instant::now() + self.config.heartbeat;
        let mut heartbeat = tokio::time::interval_at(start, self.config.heartbeat);

        loop {
            tokio::select! {
                result = &mut handler => return Outcome::Finished(result),
                () = &mut deadline => {
                    cancel.cancel();
                    return Outcome::TimedOut(spec.timeout);
                }
                () = &mut grace => return Outcome::ShutdownGraceExceeded,
                _ = heartbeat.tick() => {
                    let until = Utc::now() + delta(self.config.lease);
                    match self.db.heartbeat_job(job.id, &self.owner, until).await {
                        Ok(true) => {}
                        Ok(false) => {
                            cancel.cancel();
                            return Outcome::LeaseLost;
                        }
                        // Keep going: the lease is still valid for a while and
                        // the next heartbeat may succeed.
                        Err(err) => tracing::warn!(error = %err, job_id = job.id, "heartbeat failed"),
                    }
                }
            }
        }
    }

    /// Record how the execution ended: the run first (its own transaction,
    /// so the record survives whatever happens to the job), then the job.
    /// The job step runs even if recording the run failed, so a finished
    /// handler is never executed again just because of a busy database.
    async fn record(
        &self,
        job: &ClaimedJob,
        run_id: i64,
        spec: JobSpec,
        outcome: Outcome,
        shutdown: &CancellationToken,
    ) -> Result<(), WorkerError> {
        let failure = match &outcome {
            Outcome::Finished(Ok(_)) | Outcome::LeaseLost => None,
            Outcome::Finished(Err(JobError::Cancelled)) | Outcome::ShutdownGraceExceeded
                if shutdown.is_cancelled() =>
            {
                None
            }
            Outcome::TimedOut(timeout) => {
                Some(format!("timed out after {}s", timeout.as_secs_f64()))
            }
            Outcome::Finished(Err(err)) => Some(err.to_string()),
            Outcome::ShutdownGraceExceeded => Some("shutdown grace exceeded".to_owned()),
        };
        let end = match (&outcome, &failure) {
            (_, Some(error)) => RunEnd::Failed(error),
            (Outcome::Finished(Ok(report)), None) => RunEnd::Succeeded(report),
            (Outcome::LeaseLost, None) => RunEnd::Aborted("lease lost"),
            (_, None) => RunEnd::Aborted("cancelled by shutdown"),
        };

        let now = Utc::now();
        let run_recorded = self.db.finish_run(run_id, end, now).await;
        if let Ok(false) = run_recorded {
            // Whoever holds the lease now has already aborted this run.
            tracing::warn!(kind = %job.kind, run_id, "run was no longer running when it ended");
        }

        let job_result =
            match (&outcome, &failure) {
                (_, Some(error)) => self
                    .db
                    .fail_job(job.id, &self.owner, now, spec.backoff)
                    .await
                    .map(|fail| {
                        tracing::warn!(kind = %job.kind, run_id, error, ?fail, "job failed");
                        fail != FailOutcome::LeaseLost
                    }),
                (Outcome::Finished(Ok(_)), None) => self
                    .db
                    .complete_job(job.id, &self.owner)
                    .await
                    .inspect(|_| {
                        tracing::info!(kind = %job.kind, run_id, "job succeeded");
                    }),
                (Outcome::LeaseLost, None) => Ok(true),
                (_, None) => self.db.release_job(job.id, &self.owner, now).await.inspect(
                    |_| tracing::info!(kind = %job.kind, run_id, "job released on shutdown"),
                ),
            };
        if let Ok(false) = job_result {
            tracing::warn!(kind = %job.kind, run_id, "job lease was lost before the outcome was recorded");
        }
        if matches!(outcome, Outcome::LeaseLost) {
            tracing::warn!(kind = %job.kind, run_id, "job lost its lease");
        }
        run_recorded?;
        job_result?;
        Ok(())
    }

    /// Fail a run that never reached its handler (unknown kind, bad payload).
    async fn record_failure(
        &self,
        job: &ClaimedJob,
        run_id: i64,
        backoff: Duration,
        error: &str,
    ) -> Result<(), WorkerError> {
        let spec = JobSpec {
            backoff,
            ..JobKind::Noop.spec()
        };
        let outcome = Outcome::Finished(Err(JobError::Failed(error.to_owned())));
        self.record(job, run_id, spec, outcome, &CancellationToken::new())
            .await
    }
}

#[cfg(test)]
mod account_writes_tests;
#[cfg(test)]
mod accounts_tests;
#[cfg(test)]
mod catalog_tests;
#[cfg(test)]
mod inventory_tests;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod navidrome_tests;
#[cfg(test)]
mod passwords_tests;
#[cfg(test)]
mod requests_tests;
#[cfg(test)]
mod services_tests;
#[cfg(test)]
mod tests;
