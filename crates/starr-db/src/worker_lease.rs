//! The worker lease (ADR 0003, split mode): exactly one worker process
//! executes jobs. Whoever holds the single `worker_lease` row is the active
//! worker; it renews the lease by heartbeat and deletes it on a clean
//! shutdown. Every other worker waits and takes over once the lease is
//! released or expired.
//!
//! Job leases already keep two workers from running the same job; this lease
//! also keeps worker-wide side effects (publishing the credential recipient,
//! marking credentials unreadable, scheduling) to one process, so two workers
//! with different `secret.key` files cannot undo each other's work.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::jobs::delta;
use crate::{Database, DbError, parse_ts, ts};

/// The worker currently holding the lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLeaseHolder {
    pub owner: String,
    pub expires_at: DateTime<Utc>,
}

impl Database {
    /// Take the lease for `owner` if nobody holds it or its holder's lease
    /// expired, or renew it if `owner` already holds it. Either way it is
    /// valid until `now + ttl`. `false` if another worker holds a valid lease.
    pub async fn acquire_worker_lease(
        &self,
        owner: &str,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<bool, DbError> {
        let now_ts = ts(now);
        let row: Option<(String,)> = sqlx::query_as(
            "INSERT INTO worker_lease (id, owner, acquired_at, expires_at) VALUES (1, ?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET
                 acquired_at = iif(owner = excluded.owner, acquired_at, excluded.acquired_at),
                 owner = excluded.owner,
                 expires_at = excluded.expires_at
             WHERE owner = excluded.owner OR expires_at <= excluded.acquired_at
             RETURNING owner",
        )
        .bind(owner)
        .bind(&now_ts)
        .bind(ts(now + delta(ttl)))
        .fetch_optional(self.pool())
        .await?;
        Ok(row.is_some())
    }

    /// Extend `owner`'s lease to `now + ttl`. Unlike
    /// [`Database::acquire_worker_lease`] this never takes a free lease:
    /// `false` if `owner` no longer holds it (taken over, or taken over and
    /// released meanwhile), so the caller must stop and acquire afresh.
    pub async fn renew_worker_lease(
        &self,
        owner: &str,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<bool, DbError> {
        let res = sqlx::query("UPDATE worker_lease SET expires_at = ? WHERE id = 1 AND owner = ?")
            .bind(ts(now + delta(ttl)))
            .bind(owner)
            .execute(self.pool())
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Give the lease up (clean shutdown) so a waiting worker takes over at
    /// once. `false` if `owner` did not hold it.
    pub async fn release_worker_lease(&self, owner: &str) -> Result<bool, DbError> {
        let res = sqlx::query("DELETE FROM worker_lease WHERE owner = ?")
            .bind(owner)
            .execute(self.pool())
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// The current holder, if any (also when its lease has expired).
    pub async fn worker_lease_holder(&self) -> Result<Option<WorkerLeaseHolder>, DbError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT owner, expires_at FROM worker_lease WHERE id = 1")
                .fetch_optional(self.pool())
                .await?;
        row.map(|(owner, expires_at)| {
            Ok(WorkerLeaseHolder {
                owner,
                expires_at: parse_ts(&expires_at)?,
            })
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;
    use crate::test_support::{TempDir, t0};

    const TTL: Duration = Duration::from_secs(30);

    /// Two handles on one file: two processes as far as SQLite is concerned.
    async fn two_processes(dir: &TempDir) -> (Database, Database) {
        let a = Database::open(&dir.db(), &dir.backups()).await.unwrap();
        let b = Database::open(&dir.db(), &dir.backups()).await.unwrap();
        (a, b)
    }

    #[tokio::test]
    async fn only_one_worker_holds_the_lease() {
        let dir = TempDir::new();
        let (a, b) = two_processes(&dir).await;
        let now = t0();

        assert!(a.acquire_worker_lease("a", now, TTL).await.unwrap());
        assert!(!b.acquire_worker_lease("b", now, TTL).await.unwrap());
        // Renewing extends the holder's lease.
        let later = now + TimeDelta::seconds(20);
        assert!(a.acquire_worker_lease("a", later, TTL).await.unwrap());
        assert!(
            !b.acquire_worker_lease("b", now + TimeDelta::seconds(31), TTL)
                .await
                .unwrap(),
            "the renewed lease is still valid"
        );
        assert_eq!(
            b.worker_lease_holder().await.unwrap(),
            Some(WorkerLeaseHolder {
                owner: "a".to_owned(),
                expires_at: later + TimeDelta::seconds(30),
            })
        );
    }

    #[tokio::test]
    async fn expired_lease_is_taken_over_and_the_old_holder_loses_it() {
        let dir = TempDir::new();
        let (a, b) = two_processes(&dir).await;
        let now = t0();
        assert!(a.acquire_worker_lease("a", now, TTL).await.unwrap());

        let expired = now + TimeDelta::seconds(30);
        assert!(b.acquire_worker_lease("b", expired, TTL).await.unwrap());
        assert!(
            !a.acquire_worker_lease("a", expired, TTL).await.unwrap(),
            "the old holder cannot renew a lease taken over"
        );
        assert!(!a.release_worker_lease("a").await.unwrap());
        assert_eq!(
            a.worker_lease_holder().await.unwrap().unwrap().owner,
            "b".to_owned()
        );
    }

    #[tokio::test]
    async fn renewal_never_takes_a_lease_lost_in_between() {
        let dir = TempDir::new();
        let (a, b) = two_processes(&dir).await;
        let now = t0();
        assert!(a.acquire_worker_lease("a", now, TTL).await.unwrap());
        assert!(a.renew_worker_lease("a", now, TTL).await.unwrap());

        // a stalls past its lease; b takes over and stops cleanly.
        let later = now + TimeDelta::seconds(31);
        assert!(b.acquire_worker_lease("b", later, TTL).await.unwrap());
        assert!(b.release_worker_lease("b").await.unwrap());

        assert!(!a.renew_worker_lease("a", later, TTL).await.unwrap());
        assert_eq!(a.worker_lease_holder().await.unwrap(), None);
    }

    #[tokio::test]
    async fn released_lease_is_free_at_once() {
        let dir = TempDir::new();
        let (a, b) = two_processes(&dir).await;
        let now = t0();
        assert!(a.acquire_worker_lease("a", now, TTL).await.unwrap());
        assert!(a.release_worker_lease("a").await.unwrap());
        assert_eq!(b.worker_lease_holder().await.unwrap(), None);
        assert!(b.acquire_worker_lease("b", now, TTL).await.unwrap());
    }

    #[tokio::test]
    async fn concurrent_acquirers_get_one_winner() {
        let dir = TempDir::new();
        let (a, b) = two_processes(&dir).await;
        let now = t0();
        let mut tasks = Vec::new();
        for (i, db) in [a, b].into_iter().cycle().take(8).enumerate() {
            tasks.push(tokio::spawn(async move {
                db.acquire_worker_lease(&format!("w{i}"), now, TTL)
                    .await
                    .unwrap()
            }));
        }
        let mut winners = 0;
        for task in tasks {
            winners += usize::from(task.await.unwrap());
        }
        assert_eq!(winners, 1);
    }
}
