//! Job queue (ADR 0004): enqueue, atomic claim with a lease, heartbeat,
//! complete / fail with exponential backoff, release on shutdown.
//!
//! A `job` row exists only while the job is pending or running. "Running"
//! means "holds a valid lease"; an expired lease is claimable again, so a
//! crashed worker never leaves a stuck job behind (lesson 14).
//!
//! All functions take `now` explicitly so the queue is deterministic in tests.

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use starr_core::jobs::Trigger;

use crate::runs::abort_running_runs_in;
use crate::{Database, DbError, ts};

/// Longest delay between two attempts, however many attempts failed.
const MAX_BACKOFF: Duration = Duration::from_hours(24);

/// A job to put on the queue.
#[derive(Debug, Clone)]
pub struct NewJob<'a> {
    pub kind: &'a str,
    /// JSON object handed to the handler.
    pub payload: &'a str,
    /// Dedup key: while a pending or running job holds this key, enqueueing
    /// again returns that job (and pulls its `run_after` forward if earlier).
    pub unique_key: Option<&'a str>,
    pub trigger: Trigger,
    pub run_after: DateTime<Utc>,
    pub max_attempts: u32,
}

/// A job claimed by a worker, holding a lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedJob {
    pub id: i64,
    pub kind: String,
    pub payload: String,
    pub trigger: Trigger,
    /// 1 for the first execution.
    pub attempt: u32,
    pub max_attempts: u32,
}

/// What happened to a job whose execution failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOutcome {
    /// Attempts left: the job runs again at `at`.
    Retry { at: DateTime<Utc> },
    /// No attempts left: the job was removed from the queue.
    Exhausted,
    /// The caller no longer holds the lease; nothing was changed.
    LeaseLost,
}

pub(crate) fn delta(d: Duration) -> TimeDelta {
    TimeDelta::from_std(d).unwrap_or(TimeDelta::MAX)
}

/// Delay before attempt `attempt + 1`: `base * 2^(attempt - 1)`, capped.
fn backoff(base: Duration, attempt: u32) -> Duration {
    let factor = 2u32
        .checked_pow(attempt.saturating_sub(1))
        .unwrap_or(u32::MAX);
    base.checked_mul(factor)
        .unwrap_or(MAX_BACKOFF)
        .min(MAX_BACKOFF)
}

type ClaimRow = (i64, String, String, String, i64, i64);

impl Database {
    /// Put a job on the queue and return its id. With a `unique_key` already
    /// held by a pending or running job, that job's id is returned instead and
    /// its `run_after` (and `trigger`, if that moves it) is moved to the
    /// earlier of both times.
    pub async fn enqueue_job(&self, job: &NewJob<'_>, now: DateTime<Utc>) -> Result<i64, DbError> {
        // A leased (running) job is left alone; its id is returned as is.
        // The conflicting row can vanish between the two statements when the
        // job finishes, hence the retry.
        for _ in 0..3 {
            let inserted: Option<(i64,)> = sqlx::query_as(
                "INSERT INTO job (kind, payload, unique_key, trigger, run_after, max_attempts, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (unique_key) WHERE unique_key IS NOT NULL
                 DO UPDATE SET
                     trigger = iif(excluded.run_after < run_after, excluded.trigger, trigger),
                     run_after = min(run_after, excluded.run_after)
                 WHERE lease_owner IS NULL
                 RETURNING id",
            )
            .bind(job.kind)
            .bind(job.payload)
            .bind(job.unique_key)
            .bind(job.trigger.as_str())
            .bind(ts(job.run_after))
            .bind(i64::from(job.max_attempts.max(1)))
            .bind(ts(now))
            .fetch_optional(self.pool())
            .await?;
            if let Some((id,)) = inserted {
                return Ok(id);
            }
            let running: Option<(i64,)> = sqlx::query_as("SELECT id FROM job WHERE unique_key = ?")
                .bind(job.unique_key)
                .fetch_optional(self.pool())
                .await?;
            if let Some((id,)) = running {
                return Ok(id);
            }
        }
        Err(DbError::InvalidData(format!(
            "could not enqueue {} with unique key {:?}",
            job.kind, job.unique_key
        )))
    }

    /// Atomically claim the oldest due job of one of `kinds` that is not
    /// leased (or whose lease expired) and has attempts left. A single
    /// `UPDATE … RETURNING` statement, so two workers never get the same job.
    pub async fn claim_job(
        &self,
        owner: &str,
        kinds: &[&str],
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<Option<ClaimedJob>, DbError> {
        let kinds = serde_json::to_string(kinds)?;
        let now_ts = ts(now);
        let row: Option<ClaimRow> = sqlx::query_as(
            "UPDATE job
             SET lease_owner = ?1, lease_until = ?2, attempts = attempts + 1
             WHERE id = (
                 SELECT id FROM job
                 WHERE run_after <= ?3
                   AND (lease_until IS NULL OR lease_until <= ?3)
                   AND attempts < max_attempts
                   AND kind IN (SELECT value FROM json_each(?4))
                 ORDER BY run_after, id
                 LIMIT 1
             )
             RETURNING id, kind, payload, trigger, attempts, max_attempts",
        )
        .bind(owner)
        .bind(ts(now + delta(lease)))
        .bind(&now_ts)
        .bind(kinds)
        .fetch_optional(self.pool())
        .await?;
        row.map(claimed_from_row).transpose()
    }

    /// Whether [`Database::claim_job`] would find a job right now. A cheap
    /// read for a worker in another process than the enqueuer, which cannot
    /// be woken in memory (split mode).
    pub async fn has_claimable_job(
        &self,
        kinds: &[&str],
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let kinds = serde_json::to_string(kinds)?;
        let (found,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (
                 SELECT 1 FROM job
                 WHERE run_after <= ?1
                   AND (lease_until IS NULL OR lease_until <= ?1)
                   AND attempts < max_attempts
                   AND kind IN (SELECT value FROM json_each(?2))
             )",
        )
        .bind(ts(now))
        .bind(kinds)
        .fetch_one(self.pool())
        .await?;
        Ok(found)
    }

    /// Extend the lease to `until`. `false` if the caller lost the lease.
    pub async fn heartbeat_job(
        &self,
        id: i64,
        owner: &str,
        until: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let res = sqlx::query("UPDATE job SET lease_until = ? WHERE id = ? AND lease_owner = ?")
            .bind(ts(until))
            .bind(id)
            .bind(owner)
            .execute(self.pool())
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// The job finished for good: remove it from the queue. `false` if the
    /// caller lost the lease.
    pub async fn complete_job(&self, id: i64, owner: &str) -> Result<bool, DbError> {
        let res = sqlx::query("DELETE FROM job WHERE id = ? AND lease_owner = ?")
            .bind(id)
            .bind(owner)
            .execute(self.pool())
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// The execution failed. With attempts left the lease is dropped and the
    /// job is due again after an exponential backoff starting at `base`;
    /// otherwise it is removed.
    pub async fn fail_job(
        &self,
        id: i64,
        owner: &str,
        now: DateTime<Utc>,
        base: Duration,
    ) -> Result<FailOutcome, DbError> {
        let row: Option<(i64, i64)> = sqlx::query_as(
            "SELECT attempts, max_attempts FROM job WHERE id = ? AND lease_owner = ?",
        )
        .bind(id)
        .bind(owner)
        .fetch_optional(self.pool())
        .await?;
        let Some((attempts, max_attempts)) = row else {
            return Ok(FailOutcome::LeaseLost);
        };
        if attempts >= max_attempts {
            return Ok(if self.complete_job(id, owner).await? {
                FailOutcome::Exhausted
            } else {
                FailOutcome::LeaseLost
            });
        }
        let attempt = u32::try_from(attempts).unwrap_or(u32::MAX);
        let at = now + delta(backoff(base, attempt));
        let res = sqlx::query(
            "UPDATE job SET lease_owner = NULL, lease_until = NULL, run_after = ?
             WHERE id = ? AND lease_owner = ?",
        )
        .bind(ts(at))
        .bind(id)
        .bind(owner)
        .execute(self.pool())
        .await?;
        Ok(if res.rows_affected() == 1 {
            FailOutcome::Retry { at }
        } else {
            FailOutcome::LeaseLost
        })
    }

    /// Give a claimed job back without using up an attempt (graceful
    /// shutdown). `false` if the caller lost the lease.
    pub async fn release_job(
        &self,
        id: i64,
        owner: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let res = sqlx::query(
            "UPDATE job
             SET lease_owner = NULL, lease_until = NULL, run_after = ?,
                 attempts = max(attempts - 1, 0)
             WHERE id = ? AND lease_owner = ?",
        )
        .bind(ts(now))
        .bind(id)
        .bind(owner)
        .execute(self.pool())
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Remove jobs whose lease expired after their last attempt (the worker
    /// died during it). Their runs still marked running are recorded as
    /// aborted. Returns the number of removed jobs.
    pub async fn reap_exhausted_jobs(&self, now: DateTime<Utc>) -> Result<u64, DbError> {
        // One transaction, starting with the write: a job is never removed
        // while its run stays "running" forever.
        let mut tx = self.begin_write().await?;
        let ids: Vec<(i64,)> = sqlx::query_as(
            "DELETE FROM job WHERE lease_until <= ? AND attempts >= max_attempts RETURNING id",
        )
        .bind(ts(now))
        .fetch_all(&mut *tx)
        .await?;
        for (id,) in &ids {
            abort_running_runs_in(&mut tx, *id, "lease expired after the last attempt", now)
                .await?;
        }
        tx.commit().await?;
        Ok(ids.len() as u64)
    }
}

fn claimed_from_row(
    (id, kind, payload, trigger, attempts, max_attempts): ClaimRow,
) -> Result<ClaimedJob, DbError> {
    let trigger = Trigger::parse(&trigger).ok_or_else(|| crate::unknown("trigger", &trigger))?;
    Ok(ClaimedJob {
        id,
        kind,
        payload,
        trigger,
        attempt: u32::try_from(attempts).unwrap_or(u32::MAX),
        max_attempts: u32::try_from(max_attempts).unwrap_or(u32::MAX),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::test_support::{TempDir, t0};

    const LEASE: Duration = Duration::from_secs(30);
    const KINDS: &[&str] = &["test.a", "test.b"];

    fn job(kind: &str, run_after: DateTime<Utc>) -> NewJob<'_> {
        NewJob {
            kind,
            payload: "{}",
            unique_key: None,
            trigger: Trigger::Manual,
            run_after,
            max_attempts: 3,
        }
    }

    async fn job_count(db: &Database) -> i64 {
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM job")
            .fetch_one(db.pool())
            .await
            .unwrap();
        n
    }

    #[tokio::test]
    async fn claims_due_jobs_oldest_first_with_lease() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        let late = db.enqueue_job(&job("test.a", now), now).await.unwrap();
        let early = db
            .enqueue_job(&job("test.a", now - TimeDelta::seconds(5)), now)
            .await
            .unwrap();
        db.enqueue_job(&job("test.a", now + TimeDelta::seconds(5)), now)
            .await
            .unwrap();

        let first = db
            .claim_job("w1", KINDS, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.id, early);
        assert_eq!(first.attempt, 1);
        assert_eq!(first.trigger, Trigger::Manual);
        let second = db
            .claim_job("w1", KINDS, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.id, late);
        // The third is not due yet, the others are leased.
        assert_eq!(db.claim_job("w1", KINDS, now, LEASE).await.unwrap(), None);
    }

    #[tokio::test]
    async fn only_known_kinds_are_claimed() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        db.enqueue_job(&job("from.newer.release", now), now)
            .await
            .unwrap();
        assert_eq!(db.claim_job("w1", KINDS, now, LEASE).await.unwrap(), None);
    }

    #[tokio::test]
    async fn has_claimable_job_matches_what_claim_would_find() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        assert!(!db.has_claimable_job(KINDS, now).await.unwrap());
        db.enqueue_job(&job("from.newer.release", now), now)
            .await
            .unwrap();
        db.enqueue_job(&job("test.a", now + TimeDelta::seconds(5)), now)
            .await
            .unwrap();
        assert!(!db.has_claimable_job(KINDS, now).await.unwrap());

        db.enqueue_job(&job("test.b", now), now).await.unwrap();
        assert!(db.has_claimable_job(KINDS, now).await.unwrap());
        db.claim_job("w1", KINDS, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        // Leased now; the other one is not due yet.
        assert!(!db.has_claimable_job(KINDS, now).await.unwrap());
        assert!(
            db.has_claimable_job(KINDS, now + TimeDelta::seconds(5))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn concurrent_claimers_never_get_the_same_job() {
        let dir = TempDir::new();
        let db = Database::open(&dir.db(), &dir.backups()).await.unwrap();
        let now = t0();
        for _ in 0..40 {
            db.enqueue_job(&job("test.a", now), now).await.unwrap();
        }

        let mut tasks = Vec::new();
        for worker in 0..4 {
            let db = db.clone();
            tasks.push(tokio::spawn(async move {
                let owner = format!("w{worker}");
                let mut got = Vec::new();
                while let Some(job) = db.claim_job(&owner, KINDS, now, LEASE).await.unwrap() {
                    got.push(job.id);
                }
                got
            }));
        }
        let mut all = Vec::new();
        for task in tasks {
            all.extend(task.await.unwrap());
        }
        let unique: HashSet<i64> = all.iter().copied().collect();
        assert_eq!(all.len(), 40, "every job claimed exactly once: {all:?}");
        assert_eq!(unique.len(), 40);
    }

    #[tokio::test]
    async fn expired_lease_is_reclaimed_and_old_owner_loses_it() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        let id = db.enqueue_job(&job("test.a", now), now).await.unwrap();
        db.claim_job("dead", KINDS, now, LEASE)
            .await
            .unwrap()
            .unwrap();

        // Still leased just before expiry.
        let before = now + delta(LEASE) - TimeDelta::milliseconds(1);
        assert_eq!(
            db.claim_job("w2", KINDS, before, LEASE).await.unwrap(),
            None
        );

        let after = now + delta(LEASE);
        let job = db
            .claim_job("w2", KINDS, after, LEASE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.id, id);
        assert_eq!(job.attempt, 2);

        assert!(
            !db.heartbeat_job(id, "dead", after + delta(LEASE))
                .await
                .unwrap()
        );
        assert!(!db.complete_job(id, "dead").await.unwrap());
        assert!(
            db.heartbeat_job(id, "w2", after + delta(LEASE))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn heartbeat_keeps_the_lease() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        let id = db.enqueue_job(&job("test.a", now), now).await.unwrap();
        db.claim_job("w1", KINDS, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        let later = now + TimeDelta::seconds(25);
        assert!(
            db.heartbeat_job(id, "w1", later + delta(LEASE))
                .await
                .unwrap()
        );
        // Past the original lease, but the heartbeat extended it.
        let past_original = now + TimeDelta::seconds(40);
        assert_eq!(
            db.claim_job("w2", KINDS, past_original, LEASE)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn failure_retries_with_exponential_backoff_until_exhausted() {
        let db = Database::open_in_memory().await.unwrap();
        let base = Duration::from_secs(10);
        let mut now = t0();
        let id = db.enqueue_job(&job("test.a", now), now).await.unwrap();

        // Attempt 1 fails: retry after 10 s.
        db.claim_job("w1", KINDS, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        let outcome = db.fail_job(id, "w1", now, base).await.unwrap();
        assert_eq!(
            outcome,
            FailOutcome::Retry {
                at: now + TimeDelta::seconds(10)
            }
        );
        assert_eq!(
            db.claim_job("w1", KINDS, now + TimeDelta::seconds(9), LEASE)
                .await
                .unwrap(),
            None
        );

        // Attempt 2 fails: retry after 20 s.
        now += TimeDelta::seconds(10);
        let job2 = db
            .claim_job("w1", KINDS, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job2.attempt, 2);
        let outcome = db.fail_job(id, "w1", now, base).await.unwrap();
        assert_eq!(
            outcome,
            FailOutcome::Retry {
                at: now + TimeDelta::seconds(20)
            }
        );

        // Attempt 3 (the last) fails: removed.
        now += TimeDelta::seconds(20);
        let job3 = db
            .claim_job("w1", KINDS, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job3.attempt, 3);
        assert_eq!(
            db.fail_job(id, "w1", now, base).await.unwrap(),
            FailOutcome::Exhausted
        );
        assert_eq!(job_count(&db).await, 0);

        assert_eq!(
            db.fail_job(id, "w1", now, base).await.unwrap(),
            FailOutcome::LeaseLost
        );
    }

    #[test]
    fn backoff_doubles_and_is_capped() {
        let base = Duration::from_secs(60);
        assert_eq!(backoff(base, 1), Duration::from_secs(60));
        assert_eq!(backoff(base, 2), Duration::from_secs(120));
        assert_eq!(backoff(base, 3), Duration::from_secs(240));
        assert_eq!(backoff(base, 40), MAX_BACKOFF);
    }

    #[tokio::test]
    async fn unique_key_dedups_pending_and_running_jobs() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        let keyed = |run_after| NewJob {
            unique_key: Some("kind:test.a"),
            ..job("test.a", run_after)
        };
        let later = now + TimeDelta::seconds(60);
        let scheduled = NewJob {
            trigger: Trigger::Schedule,
            ..keyed(later)
        };
        let first = db.enqueue_job(&scheduled, now).await.unwrap();
        let second = db.enqueue_job(&scheduled, now).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(job_count(&db).await, 1);

        // An earlier request pulls the pending job forward instead of adding one.
        assert_eq!(db.enqueue_job(&keyed(now), now).await.unwrap(), first);
        let claimed = db
            .claim_job("w1", KINDS, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, first);
        // The earlier (manual) request decided when it ran, so it owns the trigger.
        assert_eq!(claimed.trigger, Trigger::Manual);

        // While running, the key still dedups and the running job is left
        // untouched (trigger and run_after stay as claimed).
        let manual_later = NewJob {
            trigger: Trigger::Schedule,
            ..keyed(now - TimeDelta::seconds(30))
        };
        assert_eq!(db.enqueue_job(&manual_later, now).await.unwrap(), first);
        let (trigger, run_after): (String, String) =
            sqlx::query_as("SELECT trigger, run_after FROM job WHERE id = ?")
                .bind(first)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(trigger, "manual");
        assert_eq!(run_after, ts(now));
        assert_eq!(job_count(&db).await, 1);

        // Once finished, the key is free again.
        assert!(db.complete_job(first, "w1").await.unwrap());
        let next = db.enqueue_job(&keyed(now), now).await.unwrap();
        assert_ne!(next, first);

        // Jobs without a key never dedup.
        db.enqueue_job(&job("test.a", now), now).await.unwrap();
        db.enqueue_job(&job("test.a", now), now).await.unwrap();
        assert_eq!(job_count(&db).await, 3);
    }

    #[tokio::test]
    async fn release_returns_the_job_without_using_an_attempt() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        let id = db.enqueue_job(&job("test.a", now), now).await.unwrap();
        db.claim_job("w1", KINDS, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        assert!(db.release_job(id, "w1", now).await.unwrap());
        let again = db
            .claim_job("w2", KINDS, now, LEASE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.id, id);
        assert_eq!(again.attempt, 1);
    }

    #[tokio::test]
    async fn exhausted_jobs_with_expired_lease_are_reaped() {
        let db = Database::open_in_memory().await.unwrap();
        let now = t0();
        let single = NewJob {
            max_attempts: 1,
            ..job("test.a", now)
        };
        db.enqueue_job(&single, now).await.unwrap();
        db.claim_job("dead", KINDS, now, LEASE)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(db.reap_exhausted_jobs(now).await.unwrap(), 0);
        let expired = now + delta(LEASE);
        // Not claimable: no attempts left.
        assert_eq!(
            db.claim_job("w2", KINDS, expired, LEASE).await.unwrap(),
            None
        );
        assert_eq!(db.reap_exhausted_jobs(expired).await.unwrap(), 1);
        assert_eq!(job_count(&db).await, 0);
    }
}
