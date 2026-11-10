//! Run history (ADR 0004, lessons 19 and 28): one `run` per execution with
//! state and report, plus append-only `run_transition`s. Each write happens in
//! its own transaction, independent of the job queue, so a failing job always
//! leaves its run record behind.

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;
use starr_core::jobs::{Report, RunState, Trigger};

use crate::jobs::ClaimedJob;
use crate::{Database, DbError, ts};

/// One execution of a job.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub id: i64,
    pub job_id: i64,
    pub kind: String,
    pub trigger: Trigger,
    pub attempt: u32,
    pub state: RunState,
    pub progress: Option<f64>,
    pub report: Option<serde_json::Value>,
    /// `met`, `unmet` or `not_applicable`; set together with the report.
    pub expectation: Option<String>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// One state change of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTransition {
    pub from: Option<RunState>,
    pub to: RunState,
    pub note: Option<String>,
    pub at: DateTime<Utc>,
}

/// How a run ended.
#[derive(Debug, Clone, Copy)]
pub enum RunEnd<'a> {
    Succeeded(&'a Report),
    Failed(&'a str),
    Aborted(&'a str),
}

/// How a run turned out, as admins filter the run history: a succeeded run
/// whose expectation was not met "needs attention"; aborted runs count as
/// failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Running,
    Succeeded,
    Attention,
    Failed,
}

impl RunOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Attention => "attention",
            Self::Failed => "failed",
        }
    }
}

/// Listing filter; results are newest first.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunFilter<'a> {
    pub kind: Option<&'a str>,
    pub state: Option<RunState>,
    /// Only runs of this job (all attempts).
    pub job_id: Option<i64>,
    /// Only runs that ended this way (spec 0008 ADM-29).
    pub outcome: Option<RunOutcome>,
    /// Only runs with an id below this (cursor for the next page).
    pub before: Option<i64>,
    pub limit: u32,
}

type TransitionRow = (Option<String>, String, Option<String>, DateTime<Utc>);

#[derive(sqlx::FromRow)]
struct RunRow {
    id: i64,
    job_id: i64,
    kind: String,
    trigger: String,
    attempt: i64,
    state: String,
    progress: Option<f64>,
    report: Option<String>,
    expectation: Option<String>,
    error: Option<String>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

/// `SELECT <all run columns> FROM run <tail>` as a static string.
macro_rules! select_runs {
    ($tail:literal) => {
        concat!(
            "SELECT id, job_id, kind, trigger, attempt, state, progress, report, ",
            "expectation, error, started_at, finished_at FROM run ",
            $tail
        )
    };
}

fn parse_state(run_id: i64, s: &str) -> Result<RunState, DbError> {
    RunState::parse(s).ok_or_else(|| {
        tracing::debug!(run_id, "run with an unknown state");
        crate::unknown("run state", s)
    })
}

impl TryFrom<RunRow> for Run {
    type Error = DbError;

    fn try_from(row: RunRow) -> Result<Self, DbError> {
        let trigger =
            Trigger::parse(&row.trigger).ok_or_else(|| crate::unknown("trigger", &row.trigger))?;
        let report = row
            .report
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        Ok(Self {
            id: row.id,
            job_id: row.job_id,
            state: parse_state(row.id, &row.state)?,
            kind: row.kind,
            trigger,
            attempt: u32::try_from(row.attempt).unwrap_or(u32::MAX),
            progress: row.progress,
            report,
            expectation: row.expectation,
            error: row.error,
            started_at: row.started_at,
            finished_at: row.finished_at,
        })
    }
}

/// Mark every running run of `job_id` as aborted, with a transition.
pub(crate) async fn abort_running_runs_in(
    conn: &mut SqliteConnection,
    job_id: i64,
    note: &str,
    now: DateTime<Utc>,
) -> Result<u64, DbError> {
    // Write first: a transaction that starts with a write takes the write lock
    // up front and never hits a stale read snapshot.
    sqlx::query(
        "INSERT INTO run_transition (run_id, from_state, to_state, note, at)
         SELECT id, 'running', 'aborted', ?, ? FROM run WHERE job_id = ? AND state = 'running'",
    )
    .bind(note)
    .bind(ts(now))
    .bind(job_id)
    .execute(&mut *conn)
    .await?;
    let res = sqlx::query(
        "UPDATE run SET state = 'aborted', error = ?, finished_at = ?
         WHERE job_id = ? AND state = 'running'",
    )
    .bind(note)
    .bind(ts(now))
    .bind(job_id)
    .execute(&mut *conn)
    .await?;
    Ok(res.rows_affected())
}

impl Database {
    /// Record the start of an execution of a claimed job. Runs of the same job
    /// still marked running belong to a worker whose lease expired; they are
    /// recorded as aborted first (lesson 14).
    pub async fn start_run(&self, job: &ClaimedJob, now: DateTime<Utc>) -> Result<i64, DbError> {
        let mut tx = self.begin_write().await?;
        abort_running_runs_in(&mut tx, job.id, "lease expired; job was claimed again", now).await?;
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO run (job_id, kind, trigger, attempt, state, started_at)
             VALUES (?, ?, ?, ?, 'running', ?)
             RETURNING id",
        )
        .bind(job.id)
        .bind(&job.kind)
        .bind(job.trigger.as_str())
        .bind(i64::from(job.attempt))
        .bind(ts(now))
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO run_transition (run_id, from_state, to_state, at)
             VALUES (?, NULL, 'running', ?)",
        )
        .bind(id)
        .bind(ts(now))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Record the end of a run. `false` if it was no longer running (for
    /// example already aborted because its lease expired).
    pub async fn finish_run(
        &self,
        run_id: i64,
        end: RunEnd<'_>,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let (state, report, expectation, error, note) = match end {
            RunEnd::Succeeded(report) => (
                RunState::Succeeded,
                Some(serde_json::to_string(report)?),
                Some(report.expectation().status()),
                None,
                None,
            ),
            RunEnd::Failed(error) => (RunState::Failed, None, None, Some(error), Some(error)),
            RunEnd::Aborted(reason) => (RunState::Aborted, None, None, Some(reason), Some(reason)),
        };
        let mut tx = self.begin_write().await?;
        let res = sqlx::query(
            "UPDATE run SET state = ?, report = ?, expectation = ?, error = ?, finished_at = ?
             WHERE id = ? AND state = 'running'",
        )
        .bind(state.as_str())
        .bind(report)
        .bind(expectation)
        .bind(error)
        .bind(ts(now))
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO run_transition (run_id, from_state, to_state, note, at)
             VALUES (?, 'running', ?, ?, ?)",
        )
        .bind(run_id)
        .bind(state.as_str())
        .bind(note)
        .bind(ts(now))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Mark the running runs of `job_id` as aborted. Returns how many.
    pub async fn abort_running_runs(
        &self,
        job_id: i64,
        note: &str,
        now: DateTime<Utc>,
    ) -> Result<u64, DbError> {
        let mut tx = self.begin_write().await?;
        let n = abort_running_runs_in(&mut tx, job_id, note, now).await?;
        tx.commit().await?;
        Ok(n)
    }

    /// Runs matching `filter`, newest first.
    pub async fn list_runs(&self, filter: RunFilter<'_>) -> Result<Vec<Run>, DbError> {
        let rows: Vec<RunRow> = sqlx::query_as(select_runs!(
            "WHERE (?1 IS NULL OR kind = ?1)
               AND (?2 IS NULL OR state = ?2)
               AND (?3 IS NULL OR id < ?3)
               AND (?5 IS NULL OR job_id = ?5)
               AND (?6 IS NULL
                    OR (?6 = 'running' AND state = 'running')
                    OR (?6 = 'succeeded' AND state = 'succeeded'
                        AND ifnull(expectation, '') <> 'unmet')
                    OR (?6 = 'attention' AND state = 'succeeded' AND expectation = 'unmet')
                    OR (?6 = 'failed' AND state IN ('failed', 'aborted')))
             ORDER BY id DESC
             LIMIT ?4"
        ))
        .bind(filter.kind)
        .bind(filter.state.map(RunState::as_str))
        .bind(filter.before)
        .bind(i64::from(filter.limit))
        .bind(filter.job_id)
        .bind(filter.outcome.map(RunOutcome::as_str))
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(Run::try_from))
    }

    /// One run with its transitions in order, or `None`.
    pub async fn get_run(&self, id: i64) -> Result<Option<(Run, Vec<RunTransition>)>, DbError> {
        let row: Option<RunRow> = sqlx::query_as(select_runs!("WHERE id = ?"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let run = Run::try_from(row)?;
        let rows: Vec<TransitionRow> = sqlx::query_as(
            "SELECT from_state, to_state, note, at FROM run_transition WHERE run_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(self.pool())
        .await?;
        let transitions = crate::readable(rows.into_iter().map(|(from, to, note, at)| {
            Ok(RunTransition {
                from: from.as_deref().map(|s| parse_state(id, s)).transpose()?,
                to: parse_state(id, &to)?,
                note,
                at,
            })
        }))?;
        Ok(Some((run, transitions)))
    }

    /// Start time of the newest run of `kind`, if any.
    pub async fn last_run_started(&self, kind: &str) -> Result<Option<DateTime<Utc>>, DbError> {
        let row: Option<(DateTime<Utc>,)> =
            sqlx::query_as("SELECT started_at FROM run WHERE kind = ? ORDER BY id DESC LIMIT 1")
                .bind(kind)
                .fetch_optional(self.pool())
                .await?;
        Ok(row.map(|(at,)| at))
    }

    /// Delete finished runs (and their transitions) that ended before `cutoff`.
    pub async fn delete_runs_finished_before(&self, cutoff: DateTime<Utc>) -> Result<u64, DbError> {
        let res = sqlx::query("DELETE FROM run WHERE finished_at < ?")
            .bind(ts(cutoff))
            .execute(self.pool())
            .await?;
        Ok(res.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use starr_core::jobs::Expectation;

    use super::*;
    use crate::jobs::NewJob;
    use crate::test_support::t0;

    async fn claimed(db: &Database, kind: &str, now: DateTime<Utc>) -> ClaimedJob {
        db.enqueue_job(
            &NewJob {
                kind,
                payload: "{}",
                unique_key: None,
                trigger: Trigger::Schedule,
                run_after: now,
                max_attempts: 2,
            },
            now,
        )
        .await
        .unwrap();
        db.claim_job("w1", &[kind], now, std::time::Duration::from_secs(30))
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn successful_run_has_report_and_transitions() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        let job = claimed(&db, "test.a", now).await;
        let run_id = db.start_run(&job, now).await.unwrap();

        let report = Report::new(2, 2, 0, 0, Expectation::Met("ok".into()))
            .unwrap()
            .with_counter("things", 2);
        let end = now + TimeDelta::seconds(1);
        assert!(
            db.finish_run(run_id, RunEnd::Succeeded(&report), end)
                .await
                .unwrap()
        );
        // Finishing twice is refused.
        assert!(
            !db.finish_run(run_id, RunEnd::Failed("x"), end)
                .await
                .unwrap()
        );

        let (run, transitions) = db.get_run(run_id).await.unwrap().unwrap();
        assert_eq!(run.state, RunState::Succeeded);
        assert_eq!(run.kind, "test.a");
        assert_eq!(run.trigger, Trigger::Schedule);
        assert_eq!(run.attempt, 1);
        assert_eq!(run.started_at, now);
        assert_eq!(run.finished_at, Some(end));
        assert_eq!(run.expectation.as_deref(), Some("met"));
        assert_eq!(run.report.unwrap()["counters"]["things"], 2);
        assert_eq!(
            transitions
                .iter()
                .map(|t| (t.from, t.to))
                .collect::<Vec<_>>(),
            vec![
                (None, RunState::Running),
                (Some(RunState::Running), RunState::Succeeded)
            ]
        );
    }

    #[tokio::test]
    async fn run_record_survives_job_failure() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        let job = claimed(&db, "test.a", now).await;
        let run_id = db.start_run(&job, now).await.unwrap();
        db.finish_run(run_id, RunEnd::Failed("boom"), now)
            .await
            .unwrap();
        db.fail_job(job.id, "w1", now, std::time::Duration::from_secs(1))
            .await
            .unwrap();

        let (run, transitions) = db.get_run(run_id).await.unwrap().unwrap();
        assert_eq!(run.state, RunState::Failed);
        assert_eq!(run.error.as_deref(), Some("boom"));
        assert_eq!(run.report, None);
        assert_eq!(transitions.last().unwrap().note.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn reclaiming_an_expired_lease_aborts_the_stale_run() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        let job = claimed(&db, "test.a", now).await;
        let stale = db.start_run(&job, now).await.unwrap();

        let later = now + TimeDelta::seconds(31);
        let again = db
            .claim_job("w2", &["test.a"], later, std::time::Duration::from_secs(30))
            .await
            .unwrap()
            .unwrap();
        let fresh = db.start_run(&again, later).await.unwrap();

        let (old, transitions) = db.get_run(stale).await.unwrap().unwrap();
        assert_eq!(old.state, RunState::Aborted);
        assert_eq!(transitions.last().unwrap().to, RunState::Aborted);
        let (new, _) = db.get_run(fresh).await.unwrap().unwrap();
        assert_eq!(new.state, RunState::Running);
        assert_eq!(new.attempt, 2);
        // The stale worker can no longer overwrite its aborted run.
        assert!(
            !db.finish_run(stale, RunEnd::Failed("late"), later)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn list_filters_by_outcome() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        let mut ids = Vec::new();
        for _ in 0..5 {
            let job = claimed(&db, "test.a", now).await;
            ids.push(db.start_run(&job, now).await.unwrap());
        }
        let met = Report::new(1, 1, 0, 0, Expectation::Met("ok".into())).unwrap();
        let unmet = Report::new(1, 0, 1, 0, Expectation::Unmet("no".into())).unwrap();
        db.finish_run(ids[0], RunEnd::Succeeded(&met), now)
            .await
            .unwrap();
        db.finish_run(ids[1], RunEnd::Succeeded(&unmet), now)
            .await
            .unwrap();
        db.finish_run(ids[2], RunEnd::Failed("x"), now)
            .await
            .unwrap();
        db.finish_run(ids[3], RunEnd::Aborted("lease"), now)
            .await
            .unwrap();
        let with = |outcome| {
            let db = &db;
            async move {
                db.list_runs(RunFilter {
                    outcome: Some(outcome),
                    limit: 10,
                    ..Default::default()
                })
                .await
                .unwrap()
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>()
            }
        };
        assert_eq!(with(RunOutcome::Succeeded).await, vec![ids[0]]);
        assert_eq!(with(RunOutcome::Attention).await, vec![ids[1]]);
        assert_eq!(with(RunOutcome::Failed).await, vec![ids[3], ids[2]]);
        assert_eq!(with(RunOutcome::Running).await, vec![ids[4]]);
    }

    #[tokio::test]
    async fn list_filters_and_pages_newest_first() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        let mut ids = Vec::new();
        for kind in ["test.a", "test.b", "test.a"] {
            let job = claimed(&db, kind, now).await;
            ids.push(db.start_run(&job, now).await.unwrap());
        }
        db.finish_run(ids[0], RunEnd::Failed("x"), now)
            .await
            .unwrap();

        let all = db
            .list_runs(RunFilter {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            all.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ids[2], ids[1], ids[0]]
        );

        let page = db
            .list_runs(RunFilter {
                limit: 2,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.len(), 2);
        let next = db
            .list_runs(RunFilter {
                limit: 2,
                before: Some(page[1].id),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(next.iter().map(|r| r.id).collect::<Vec<_>>(), vec![ids[0]]);

        let a = db
            .list_runs(RunFilter {
                kind: Some("test.a"),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(a.len(), 2);
        let failed = db
            .list_runs(RunFilter {
                state: Some(RunState::Failed),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            failed.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ids[0]]
        );
        let (second, _) = db.get_run(ids[1]).await.unwrap().unwrap();
        let by_job = db
            .list_runs(RunFilter {
                job_id: Some(second.job_id),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            by_job.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ids[1]]
        );
        assert_eq!(db.last_run_started("test.b").await.unwrap(), Some(now));
        assert_eq!(db.last_run_started("test.c").await.unwrap(), None);
    }

    #[tokio::test]
    async fn old_finished_runs_are_deleted_with_transitions() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        let job = claimed(&db, "test.a", now).await;
        let old = db.start_run(&job, now).await.unwrap();
        db.finish_run(old, RunEnd::Failed("x"), now).await.unwrap();
        let job = claimed(&db, "test.b", now).await;
        let running = db.start_run(&job, now).await.unwrap();

        let deleted = db
            .delete_runs_finished_before(now + TimeDelta::days(31))
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        assert!(db.get_run(old).await.unwrap().is_none());
        assert!(db.get_run(running).await.unwrap().is_some());
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM run_transition WHERE run_id = ?")
            .bind(old)
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(n, 0);
    }
}
