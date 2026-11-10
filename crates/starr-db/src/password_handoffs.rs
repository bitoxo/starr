//! Password handoffs (spec 0005 M1 B, ADR 0010): a member's password sealed
//! by the web process to the worker's recipient (ADR 0003), so the worker
//! can write it into the member's service accounts. This module only moves
//! the sealed bytes; it never sees a plaintext. At most one per member (a
//! newer one replaces it); deleted when the worker is done, at the latest
//! when it expires (one hour).

use chrono::{DateTime, TimeDelta, Utc};

use crate::{Database, DbError, parse_ts, ts};

/// How long a handoff lives at most (ADR 0010).
pub const HANDOFF_LIFETIME: TimeDelta = TimeDelta::hours(1);

/// One sealed handoff as stored.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredHandoff {
    pub member_id: i64,
    /// The member's password generation the password belongs to.
    pub generation: i64,
    /// The age recipient it was sealed for.
    pub recipient: String,
    pub sealed: Vec<u8>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for StoredHandoff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredHandoff")
            .field("member_id", &self.member_id)
            .field("generation", &self.generation)
            .field("sealed", &format_args!("<{} bytes>", self.sealed.len()))
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

type Row = (i64, i64, String, Vec<u8>, String, String);

fn from_row(row: Row) -> Result<StoredHandoff, DbError> {
    let (member_id, generation, recipient, sealed, created_at, expires_at) = row;
    Ok(StoredHandoff {
        member_id,
        generation,
        recipient,
        sealed,
        created_at: parse_ts(&created_at)?,
        expires_at: parse_ts(&expires_at)?,
    })
}

impl Database {
    /// Store a handoff for `member_id` at password `generation`, replacing
    /// an older one, valid for [`HANDOFF_LIFETIME`]. Stored only while the
    /// member is active and still at that generation (a password changed
    /// meanwhile brings its own handoff). `false` if not stored.
    pub async fn store_password_handoff(
        &self,
        member_id: i64,
        generation: i64,
        recipient: &str,
        sealed: &[u8],
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let stored = sqlx::query(
            "INSERT INTO password_handoff
                 (member_id, generation, recipient, sealed, created_at, expires_at)
             SELECT id, ?, ?, ?, ?, ? FROM member
             WHERE id = ? AND status = 'active' AND password_generation = ?
             ON CONFLICT (member_id) DO UPDATE SET
                 generation = excluded.generation, recipient = excluded.recipient,
                 sealed = excluded.sealed, created_at = excluded.created_at,
                 expires_at = excluded.expires_at",
        )
        .bind(generation)
        .bind(recipient)
        .bind(sealed)
        .bind(ts(now))
        .bind(ts(now + HANDOFF_LIFETIME))
        .bind(member_id)
        .bind(generation)
        .execute(self.pool())
        .await?
        .rows_affected()
            == 1;
        Ok(stored)
    }

    /// Every handoff that has not expired at `now`, oldest first.
    pub async fn password_handoffs(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<StoredHandoff>, DbError> {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT member_id, generation, recipient, sealed, created_at, expires_at
             FROM password_handoff WHERE expires_at > ? ORDER BY created_at, member_id",
        )
        .bind(ts(now))
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(from_row).collect()
    }

    /// Whether the member has a handoff in flight at `now`.
    pub async fn has_password_handoff(
        &self,
        member_id: i64,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM password_handoff WHERE member_id = ? AND expires_at > ?)",
        )
        .bind(member_id)
        .bind(ts(now))
        .fetch_one(self.pool())
        .await?;
        Ok(exists)
    }

    /// The members with a handoff in flight at `now`.
    pub async fn members_with_password_handoff(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<i64>, DbError> {
        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT member_id FROM password_handoff WHERE expires_at > ? ORDER BY member_id",
        )
        .bind(ts(now))
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Delete the member's handoff of `generation` (a newer one that
    /// replaced it meanwhile stays).
    pub async fn delete_password_handoff(
        &self,
        member_id: i64,
        generation: i64,
    ) -> Result<(), DbError> {
        sqlx::query("DELETE FROM password_handoff WHERE member_id = ? AND generation = ?")
            .bind(member_id)
            .bind(generation)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Delete every handoff that expired at `now`; how many.
    pub async fn delete_expired_password_handoffs(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, DbError> {
        Ok(
            sqlx::query("DELETE FROM password_handoff WHERE expires_at <= ?")
                .bind(ts(now))
                .execute(self.pool())
                .await?
                .rows_affected(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::members::NewMember;
    use crate::test_support::t0;

    async fn member(db: &Database, name: &str) -> i64 {
        let email = format!("{name}@example.org");
        db.create_member(
            &NewMember {
                username: name,
                email: &email,
                display_name: name,
                password_hash: Some("$argon2id$dummy"),
            },
            &[],
            t0(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn one_handoff_per_member_bound_to_the_current_generation() {
        let db = Database::open_in_memory().await.unwrap();
        let mia = member(&db, "mia").await;
        let noah = member(&db, "noah").await;
        // A password set at creation is generation 0.
        assert_eq!(db.member_password_generation(mia).await.unwrap(), Some(0));
        assert!(
            !db.store_password_handoff(mia, 9, "age1x", b"old", t0())
                .await
                .unwrap()
        );
        assert!(
            db.store_password_handoff(mia, 0, "age1x", b"one", t0())
                .await
                .unwrap()
        );
        assert!(
            db.store_password_handoff(noah, 0, "age1x", b"noah", t0())
                .await
                .unwrap()
        );
        // A newer password replaces the handoff.
        let generation = db
            .change_member_password(mia, "h2", &[0; 32])
            .await
            .unwrap();
        assert_eq!(generation, 1);
        assert!(
            db.store_password_handoff(mia, 1, "age1x", b"two", t0())
                .await
                .unwrap()
        );
        let all = db.password_handoffs(t0()).await.unwrap();
        assert_eq!(all.len(), 2);
        let mias = all.iter().find(|h| h.member_id == mia).unwrap();
        assert_eq!((mias.generation, mias.sealed.as_slice()), (1, &b"two"[..]));
        assert_eq!(mias.expires_at, t0() + HANDOFF_LIFETIME);
        assert!(!format!("{mias:?}").contains("two"), "no bytes in Debug");
        // Deleting an older generation keeps the newer handoff.
        db.delete_password_handoff(mia, 0).await.unwrap();
        assert!(db.has_password_handoff(mia, t0()).await.unwrap());
        db.delete_password_handoff(mia, 1).await.unwrap();
        assert!(!db.has_password_handoff(mia, t0()).await.unwrap());
        assert_eq!(
            db.members_with_password_handoff(t0()).await.unwrap(),
            vec![noah]
        );
    }

    #[tokio::test]
    async fn handoffs_expire_after_an_hour_and_never_for_inactive_members() {
        let db = Database::open_in_memory().await.unwrap();
        let mia = member(&db, "mia").await;
        assert!(
            db.store_password_handoff(mia, 0, "age1x", b"one", t0())
                .await
                .unwrap()
        );
        let later = t0() + HANDOFF_LIFETIME;
        assert_eq!(db.password_handoffs(later).await.unwrap(), vec![]);
        assert!(!db.has_password_handoff(mia, later).await.unwrap());
        assert_eq!(db.delete_expired_password_handoffs(later).await.unwrap(), 1);
        db.suspend_member(mia).await.unwrap();
        assert!(
            !db.store_password_handoff(mia, 0, "age1x", b"one", t0())
                .await
                .unwrap()
        );
    }
}
