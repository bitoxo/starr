#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicU32, Ordering};

use starr_core::jobs::RunState;
use starr_db::runs::{Run, RunFilter};

use super::*;
use crate::services_tests::TempDir;

/// Test handler: `system.noop` behaves as its payload says, everything else
/// runs the production handler. No schedules unless asked.
#[derive(Debug, Clone)]
struct Fake {
    timeout: Duration,
    max_attempts: u32,
    scheduled: bool,
    calls: Arc<AtomicU32>,
    system: Arc<SystemJobs>,
}

fn system_jobs() -> SystemJobs {
    SystemJobs::new(secrets::CredentialKey::ephemeral())
}

impl Default for Fake {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_attempts: 2,
            scheduled: false,
            calls: Arc::default(),
            system: Arc::new(system_jobs()),
        }
    }
}

impl JobHandler for Fake {
    fn spec(&self, kind: JobKind) -> JobSpec {
        JobSpec {
            timeout: self.timeout,
            max_attempts: self.max_attempts,
            backoff: Duration::ZERO,
            schedule: if self.scheduled {
                kind.spec().schedule
            } else {
                None
            },
        }
    }

    async fn run(&self, kind: JobKind, ctx: JobContext) -> Result<Report, JobError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if kind != JobKind::Noop {
            return self.system.run(kind, ctx).await;
        }
        if let Some(msg) = ctx.payload["fail"].as_str() {
            return Err(JobError::Failed(msg.to_owned()));
        }
        if let Some(ms) = ctx.payload["sleepMs"].as_u64() {
            let sleep = tokio::time::sleep(Duration::from_millis(ms));
            if ctx.payload["cooperative"].as_bool() == Some(true) {
                tokio::select! {
                    () = sleep => {}
                    () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
                }
            } else {
                sleep.await;
            }
        }
        Ok(Report::new(
            1,
            1,
            0,
            0,
            starr_core::jobs::Expectation::NotApplicable,
        )?)
    }
}

fn fast() -> WorkerConfig {
    WorkerConfig {
        poll_interval: Duration::from_millis(20),
        due_check: None,
        lease: Duration::from_secs(10),
        heartbeat: Duration::from_secs(3),
        shutdown_grace: Duration::from_secs(5),
    }
}

fn worker<H: JobHandler>(db: &Database, handler: H, config: WorkerConfig) -> Worker<H> {
    Worker::new(db.clone(), handler, Arc::new(Notify::new()), config)
}

async fn enqueue_noop(db: &Database, payload: &str) -> i64 {
    enqueue_noop_with(db, payload, 2).await
}

async fn enqueue_noop_with(db: &Database, payload: &str, max_attempts: u32) -> i64 {
    let now = Utc::now();
    db.enqueue_job(
        &NewJob {
            kind: JobKind::Noop.name(),
            payload,
            unique_key: None,
            trigger: Trigger::Manual,
            run_after: now,
            max_attempts,
        },
        now,
    )
    .await
    .unwrap()
}

async fn runs(db: &Database) -> Vec<Run> {
    let mut runs = db
        .list_runs(RunFilter {
            limit: 100,
            ..Default::default()
        })
        .await
        .unwrap();
    runs.reverse();
    runs
}

/// Poll until `pred` holds for the run list (max 5 s).
async fn wait_for_runs(db: &Database, pred: impl Fn(&[Run]) -> bool) -> Vec<Run> {
    for _ in 0..500 {
        let runs = runs(db).await;
        if pred(&runs) {
            return runs;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached; runs: {:?}", runs(db).await);
}

/// Claim whatever is due, to inspect the queue.
async fn claim_any(db: &Database) -> Option<ClaimedJob> {
    db.claim_job(
        "inspector",
        &JobKind::ALL.map(JobKind::name),
        Utc::now(),
        Duration::from_secs(10),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn noop_job_runs_and_records_a_report() {
    let db = Database::open_in_memory().await.unwrap();
    let w = worker(&db, system_jobs(), fast());
    let shutdown = CancellationToken::new();
    enqueue_noop(&db, "{}").await;

    assert!(w.tick(&shutdown).await.unwrap());

    let runs = runs(&db).await;
    let noop = runs.iter().find(|r| r.kind == "system.noop").unwrap();
    assert_eq!(noop.state, RunState::Succeeded);
    assert_eq!(noop.trigger, Trigger::Manual);
    assert_eq!(noop.expectation.as_deref(), Some("met"));
    assert!(noop.report.is_some());
}

#[tokio::test]
async fn housekeeping_is_scheduled_once_per_interval_across_workers() {
    let db = Database::open_in_memory().await.unwrap();
    let shutdown = CancellationToken::new();
    let a = worker(&db, system_jobs(), fast());
    let b = worker(&db, system_jobs(), fast());

    for _ in 0..3 {
        a.tick(&shutdown).await.unwrap();
        b.tick(&shutdown).await.unwrap();
    }

    // Inventory sync is scheduled too, but without consume instances it is
    // not even enqueued.
    let runs = runs(&db).await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].kind, "system.housekeeping");
    assert_eq!(runs[0].trigger, Trigger::Schedule);
    assert_eq!(runs[0].state, RunState::Succeeded);
    assert_eq!(
        runs[0].report.as_ref().unwrap()["counters"]["runs_deleted"],
        0
    );
    // The next execution is queued one interval later, not now.
    assert!(claim_any(&db).await.is_none());
}

#[tokio::test]
async fn housekeeping_tombstones_members_whose_grace_period_ended() {
    use starr_db::members::{MemberFilter, NewMember};

    let db = Database::open_in_memory().await.unwrap();
    let now = Utc::now();
    let mut ids = Vec::new();
    for name in ["mia", "noah"] {
        let email = format!("{name}@example.org");
        let id = db
            .create_member(
                &NewMember {
                    username: name,
                    email: &email,
                    display_name: name,
                    password_hash: None,
                },
                &[],
                now,
            )
            .await
            .unwrap();
        ids.push(id);
    }
    let past = now - chrono::TimeDelta::days(1);
    db.schedule_member_deletion(ids[0], "root", past, past)
        .await
        .unwrap();
    // Noah's grace period is still running.
    db.schedule_member_deletion(ids[1], "root", now + chrono::TimeDelta::days(1), now)
        .await
        .unwrap();

    let shutdown = CancellationToken::new();
    let w = worker(&db, system_jobs(), fast());
    w.tick(&shutdown).await.unwrap();

    let runs = runs(&db).await;
    assert_eq!(runs[0].state, RunState::Succeeded, "{runs:?}");
    assert_eq!(
        runs[0].report.as_ref().unwrap()["counters"]["members_tombstoned"],
        1
    );
    let listed = db.list_members(MemberFilter::default()).await.unwrap();
    assert_eq!(
        listed.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![ids[1]]
    );
    assert!(
        db.username_exists("mia").await.unwrap(),
        "name stays reserved"
    );
    let audit = db.recent_audit(5).await.unwrap();
    assert!(
        audit
            .iter()
            .any(|e| e.action == "member.tombstoned" && e.target.as_deref() == Some("mia"))
    );
}

#[tokio::test]
async fn failing_job_is_retried_then_given_up_and_keeps_its_runs() {
    let db = Database::open_in_memory().await.unwrap();
    let shutdown = CancellationToken::new();
    let w = worker(&db, Fake::default(), fast());
    enqueue_noop(&db, r#"{"fail": "boom"}"#).await;

    assert!(w.tick(&shutdown).await.unwrap());
    assert!(w.tick(&shutdown).await.unwrap());
    assert!(!w.tick(&shutdown).await.unwrap());

    let runs = runs(&db).await;
    assert_eq!(runs.len(), 2, "{runs:?}");
    for (i, run) in runs.iter().enumerate() {
        assert_eq!(run.state, RunState::Failed);
        assert_eq!(run.attempt, u32::try_from(i + 1).unwrap());
        assert_eq!(run.error.as_deref(), Some("boom"));
        assert_eq!(run.report, None);
    }
}

#[tokio::test]
async fn timeout_fails_the_run_and_stops_the_handler() {
    let db = Database::open_in_memory().await.unwrap();
    let shutdown = CancellationToken::new();
    let handler = Fake {
        timeout: Duration::from_millis(100),
        max_attempts: 1,
        ..Fake::default()
    };
    let w = worker(&db, handler, fast());
    enqueue_noop_with(&db, r#"{"sleepMs": 60000}"#, 1).await;

    let started = std::time::Instant::now();
    assert!(w.tick(&shutdown).await.unwrap());
    assert!(started.elapsed() < Duration::from_secs(5));

    let runs = runs(&db).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, RunState::Failed);
    assert_eq!(runs[0].error.as_deref(), Some("timed out after 0.1s"));
    // No attempts left: the job is gone.
    assert!(claim_any(&db).await.is_none());
}

async fn shutdown_while_running(payload: &str, config: WorkerConfig) {
    let db = Database::open_in_memory().await.unwrap();
    let shutdown = CancellationToken::new();
    let w = Arc::new(worker(&db, Fake::default(), config));
    let job_id = enqueue_noop(&db, payload).await;

    let task = tokio::spawn({
        let w = Arc::clone(&w);
        let shutdown = shutdown.clone();
        async move { w.run(shutdown).await }
    });
    wait_for_runs(&db, |runs| {
        runs.iter().any(|r| r.state == RunState::Running)
    })
    .await;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();

    let runs = runs(&db).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, RunState::Aborted);
    assert_eq!(runs[0].error.as_deref(), Some("cancelled by shutdown"));
    // Released without using up the attempt.
    let job = claim_any(&db).await.unwrap();
    assert_eq!(job.id, job_id);
    assert_eq!(job.attempt, 1);
}

#[tokio::test]
async fn shutdown_cancels_a_running_job_and_releases_it() {
    shutdown_while_running(r#"{"sleepMs": 60000, "cooperative": true}"#, fast()).await;
}

#[tokio::test]
async fn shutdown_drops_an_uncooperative_job_after_the_grace_period() {
    let config = WorkerConfig {
        shutdown_grace: Duration::from_millis(100),
        ..fast()
    };
    shutdown_while_running(r#"{"sleepMs": 60000}"#, config).await;
}

#[tokio::test]
async fn wake_starts_a_job_without_waiting_for_the_poll() {
    let db = Database::open_in_memory().await.unwrap();
    let shutdown = CancellationToken::new();
    let wake = Arc::new(Notify::new());
    let config = WorkerConfig {
        poll_interval: Duration::from_secs(3600),
        ..fast()
    };
    let w = Worker::new(db.clone(), Fake::default(), Arc::clone(&wake), config);
    let task = tokio::spawn({
        let shutdown = shutdown.clone();
        async move { w.run(shutdown).await }
    });
    // Let the worker find the empty queue and go to sleep.
    tokio::time::sleep(Duration::from_millis(50)).await;

    enqueue_noop(&db, "{}").await;
    wake.notify_one();
    wait_for_runs(&db, |runs| {
        runs.iter().any(|r| r.state == RunState::Succeeded)
    })
    .await;

    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn heartbeat_keeps_a_long_job_from_being_claimed_twice() {
    let db = Database::open_in_memory().await.unwrap();
    let shutdown = CancellationToken::new();
    let config = WorkerConfig {
        lease: Duration::from_millis(200),
        heartbeat: Duration::from_millis(50),
        ..fast()
    };
    let handler = Fake::default();
    let calls = Arc::clone(&handler.calls);
    let a = Arc::new(worker(&db, handler.clone(), config));
    let b = worker(&db, handler, config);
    enqueue_noop(&db, r#"{"sleepMs": 600}"#).await;

    let running = tokio::spawn({
        let a = Arc::clone(&a);
        let shutdown = shutdown.clone();
        async move { a.tick(&shutdown).await.unwrap() }
    });
    wait_for_runs(&db, |runs| !runs.is_empty()).await;
    // Meanwhile another worker keeps trying to claim the job.
    let until = std::time::Instant::now() + Duration::from_millis(700);
    while std::time::Instant::now() < until {
        b.tick(&shutdown).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(running.await.unwrap());

    let runs = runs(&db).await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].state, RunState::Succeeded);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expired_lease_of_a_dead_worker_is_reclaimed_and_its_run_aborted() {
    let db = Database::open_in_memory().await.unwrap();
    let shutdown = CancellationToken::new();
    // A worker claimed the job, started a run and died; its lease expired.
    enqueue_noop(&db, "{}").await;
    let now = Utc::now();
    let dead = db
        .claim_job(
            "dead",
            &[JobKind::Noop.name()],
            now,
            Duration::from_millis(1),
        )
        .await
        .unwrap()
        .unwrap();
    db.start_run(&dead, now).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    let w = worker(&db, Fake::default(), fast());
    assert!(w.tick(&shutdown).await.unwrap());

    let runs = runs(&db).await;
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0].state, RunState::Aborted);
    assert_eq!(runs[1].state, RunState::Succeeded);
    assert_eq!(runs[1].attempt, 2);
}

// --- worker lease: exactly one active worker (split mode) ---

/// Short leases so takeover happens within a test.
fn leased() -> WorkerConfig {
    WorkerConfig {
        lease: Duration::from_millis(600),
        heartbeat: Duration::from_millis(100),
        ..fast()
    }
}

/// A file database opened twice: two processes as far as SQLite is concerned.
async fn two_processes(dir: &TempDir) -> (Database, Database) {
    let path = dir.0.join("starr.db");
    let backups = dir.0.join("backups");
    let a = Database::open(&path, &backups).await.unwrap();
    let b = Database::open(&path, &backups).await.unwrap();
    (a, b)
}

fn spawn_run<H: JobHandler + 'static>(
    w: Worker<H>,
    shutdown: &CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let shutdown = shutdown.clone();
    tokio::spawn(async move { w.run(shutdown).await })
}

async fn wait_for_lease_holder(db: &Database) -> String {
    for _ in 0..500 {
        if let Some(holder) = db.worker_lease_holder().await.unwrap() {
            return holder.owner;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("nobody took the worker lease");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_worker_waits_and_takes_over_when_the_first_stops() {
    let dir = TempDir::new("lease-handover");
    let (db_a, db_b) = two_processes(&dir).await;
    let (a, b) = (Fake::default(), Fake::default());
    let (calls_a, calls_b) = (Arc::clone(&a.calls), Arc::clone(&b.calls));
    let (stop_a, stop_b) = (CancellationToken::new(), CancellationToken::new());

    let task_a = spawn_run(worker(&db_a, a, leased()), &stop_a);
    wait_for_lease_holder(&db_a).await;
    let task_b = spawn_run(worker(&db_b, b, leased()), &stop_b);
    for _ in 0..5 {
        enqueue_noop(&db_b, "{}").await;
    }
    wait_for_runs(&db_a, |runs| {
        runs.len() == 5 && runs.iter().all(|r| r.state == RunState::Succeeded)
    })
    .await;
    // Longer than one lease: b stays passive while a renews its lease.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(calls_a.load(Ordering::SeqCst), 5);
    assert_eq!(
        calls_b.load(Ordering::SeqCst),
        0,
        "the waiting worker ran jobs"
    );

    // A clean stop releases the lease; b takes over.
    stop_a.cancel();
    task_a.await.unwrap();
    enqueue_noop(&db_a, "{}").await;
    wait_for_runs(&db_b, |runs| {
        runs.len() == 6 && runs[5].state == RunState::Succeeded
    })
    .await;
    assert_eq!(calls_b.load(Ordering::SeqCst), 1);

    stop_b.cancel();
    task_b.await.unwrap();
    assert_eq!(db_b.worker_lease_holder().await.unwrap(), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lease_of_a_dead_worker_is_taken_over_after_it_expires() {
    let dir = TempDir::new("lease-expiry");
    let (db_dead, db) = two_processes(&dir).await;
    // A worker took the lease and died without releasing it.
    assert!(
        db_dead
            .acquire_worker_lease("dead", Utc::now(), Duration::from_millis(500))
            .await
            .unwrap()
    );
    let handler = Fake::default();
    let calls = Arc::clone(&handler.calls);
    let stop = CancellationToken::new();
    let task = spawn_run(worker(&db, handler, leased()), &stop);
    enqueue_noop(&db, "{}").await;

    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "ran while the lease was valid"
    );
    wait_for_runs(&db, |runs| {
        runs.len() == 1 && runs[0].state == RunState::Succeeded
    })
    .await;
    assert!(
        !db_dead
            .acquire_worker_lease("dead", Utc::now(), Duration::from_millis(500))
            .await
            .unwrap(),
        "the dead worker cannot come back while the new one is active"
    );

    stop.cancel();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_whose_lease_is_taken_over_stops_executing() {
    let dir = TempDir::new("lease-lost");
    let (db, db_other) = two_processes(&dir).await;
    let handler = Fake::default();
    let calls = Arc::clone(&handler.calls);
    let stop = CancellationToken::new();
    let task = spawn_run(worker(&db, handler, leased()), &stop);
    let holder = wait_for_lease_holder(&db).await;

    // As if the worker had stalled past its lease and another took over.
    assert!(db_other.release_worker_lease(&holder).await.unwrap());
    assert!(
        db_other
            .acquire_worker_lease("usurper", Utc::now(), Duration::from_secs(60))
            .await
            .unwrap()
    );
    // One heartbeat later the old worker has noticed.
    tokio::time::sleep(Duration::from_millis(300)).await;
    enqueue_noop(&db_other, "{}").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    stop.cancel();
    task.await.unwrap();
    // Its shutdown leaves the new holder's lease alone.
    assert_eq!(
        db_other.worker_lease_holder().await.unwrap().unwrap().owner,
        "usurper"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_active_worker_publishes_its_credential_recipient() {
    let dir = TempDir::new("lease-recipient");
    let (db_a, db_b) = two_processes(&dir).await;
    let (a, b) = (system_jobs(), system_jobs());
    let recipient_a = a.credentials.recipient().to_owned();
    let recipient_b = b.credentials.recipient().to_owned();
    let (stop_a, stop_b) = (CancellationToken::new(), CancellationToken::new());
    let task_a = spawn_run(worker(&db_a, a, leased()), &stop_a);
    wait_for_lease_holder(&db_a).await;
    let task_b = spawn_run(worker(&db_b, b, leased()), &stop_b);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let published = async || {
        db_b.meta_get(secrets::RECIPIENT_META_KEY)
            .await
            .unwrap()
            .unwrap()
    };
    assert_eq!(published().await, recipient_a);

    stop_a.cancel();
    task_a.await.unwrap();
    for _ in 0..100 {
        if published().await == recipient_b {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        published().await,
        recipient_b,
        "the new active worker publishes"
    );

    stop_b.cancel();
    task_b.await.unwrap();
}

#[tokio::test]
async fn due_check_finds_a_job_enqueued_by_another_process() {
    let db = Database::open_in_memory().await.unwrap();
    let shutdown = CancellationToken::new();
    let config = WorkerConfig {
        poll_interval: Duration::from_secs(3600),
        due_check: Some(Duration::from_millis(20)),
        ..fast()
    };
    // Nobody shares this worker's wake signal, as in split mode.
    let task = spawn_run(worker(&db, Fake::default(), config), &shutdown);
    tokio::time::sleep(Duration::from_millis(50)).await;

    enqueue_noop(&db, "{}").await;
    wait_for_runs(&db, |runs| {
        runs.iter().any(|r| r.state == RunState::Succeeded)
    })
    .await;

    shutdown.cancel();
    task.await.unwrap();
}
