//! Account lifecycle, drift and hardening (spec 0005 slice 6): due
//! deletion orders (D16), accounts deleted in or gone from the service,
//! the drift baseline (D13), the rolling destructive cap (D15), the
//! password attempt marker (retry safety, ADR 0010) and pausing every live
//! instance when an active admin is without TOTP (M10 1).

use chrono::{DateTime, TimeDelta, Utc};
use starr_core::provisioning::{CAP_WINDOW, Origin};

use crate::{Database, DbError, ts};

/// A member whose deletion order is due (grace period over), tombstoned or
/// not yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueDeletion {
    pub member_id: i64,
    pub username: String,
    /// Already a tombstone (only listed while it still owns accounts).
    pub tombstoned: bool,
}

/// An account Starr stopped owning when the member's deletion order was
/// due: gone from the service, or one Starr never deletes (administrator,
/// ignored), so a tombstone does not own it forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasedAccount {
    pub service_instance_id: i64,
    pub service_name: String,
    pub member_id: i64,
    pub username: String,
    pub remote_id: String,
    pub reason: ReleaseReason,
}

/// Why Starr stopped owning an account of a due deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseReason {
    /// Gone from the service already.
    Gone,
    /// An administrator or ignored account: never deleted (D16).
    HandsOff,
    /// Adopted (linked) and disabled now: only accounts Starr created are
    /// deleted (D16, maintainer 2026-11-03).
    Adopted,
}

impl ReleaseReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gone => "goneBeforeDeletion",
            Self::HandsOff => "handsOffAtDeletion",
            Self::Adopted => "adoptedAtDeletion",
        }
    }
}

/// How long destructive write records are kept (far beyond the window).
const DESTRUCTIVE_RETENTION: TimeDelta = TimeDelta::days(1);

impl Database {
    /// Members whose deletion order is due at `now`: every one not yet
    /// tombstoned, and tombstones that still own an account somewhere.
    pub async fn due_deletions(&self, now: DateTime<Utc>) -> Result<Vec<DueDeletion>, DbError> {
        let rows: Vec<(i64, String, bool)> = sqlx::query_as(
            "SELECT m.id, m.username, d.tombstoned_at IS NOT NULL
             FROM member_deletion d JOIN member m ON m.id = d.member_id
             WHERE d.grace_until <= ?
               AND (d.tombstoned_at IS NULL OR EXISTS (
                   SELECT 1 FROM service_account a
                   WHERE a.member_id = m.id AND a.remote_id IS NOT NULL))
             ORDER BY m.id",
        )
        .bind(ts(now))
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(member_id, username, tombstoned)| DueDeletion {
                member_id,
                username,
                tombstoned,
            })
            .collect())
    }

    /// The account `remote_id` of `member_id` was deleted in the service
    /// (D16): Starr's record and the inventory row go, in one transaction.
    /// `false` if Starr did not own it for that member (any more).
    pub async fn forget_deleted_account(
        &self,
        instance_id: i64,
        member_id: i64,
        remote_id: &str,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let owned = sqlx::query(
            "DELETE FROM service_account
             WHERE service_instance_id = ? AND member_id = ? AND remote_id = ?",
        )
        .bind(instance_id)
        .bind(member_id)
        .bind(remote_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !owned {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query("DELETE FROM remote_account WHERE service_instance_id = ? AND remote_id = ?")
            .bind(instance_id)
            .bind(remote_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Release accounts of members whose deletion order is due that are
    /// gone from the service already (two complete listings without them,
    /// D7, or purged from the inventory since), that Starr never deletes
    /// (administrator or ignored, D16), or that were adopted and are
    /// disabled now (only accounts Starr created are deleted, maintainer
    /// 2026-11-03): nothing is left for Starr to do, the account stays in
    /// the service as it is.
    /// Returns what was released, for the audit.
    pub async fn release_gone_accounts_of_due_deletions(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ReleasedAccount>, DbError> {
        let mut tx = self.begin_write().await?;
        let rows: Vec<(i64, String, i64, String, String, String)> = sqlx::query_as(
            "SELECT a.service_instance_id, i.name, a.member_id, m.username, a.remote_id,
                    CASE WHEN r.remote_id IS NULL OR r.gone_since IS NOT NULL THEN 'gone'
                         WHEN r.privileged = 1 OR r.ignored_at IS NOT NULL THEN 'handsOff'
                         ELSE 'adopted' END
             FROM service_account a
             JOIN member_deletion d ON d.member_id = a.member_id
             JOIN member m ON m.id = a.member_id
             JOIN service_instance i ON i.id = a.service_instance_id
             LEFT JOIN remote_account r
                 ON r.service_instance_id = a.service_instance_id AND r.remote_id = a.remote_id
             WHERE a.remote_id IS NOT NULL AND d.grace_until <= ?
               AND (r.remote_id IS NULL OR r.gone_since IS NOT NULL
                    OR r.privileged = 1 OR r.ignored_at IS NOT NULL
                    OR (a.origin = 'adopted' AND (r.enabled = 0 OR r.locked_by_starr = 1)))
             ORDER BY a.service_instance_id, a.member_id",
        )
        .bind(ts(now))
        .fetch_all(&mut *tx)
        .await?;
        for (instance, _, member, _, remote, _) in &rows {
            sqlx::query(
                "DELETE FROM service_account
                 WHERE service_instance_id = ? AND member_id = ? AND remote_id = ?",
            )
            .bind(instance)
            .bind(member)
            .bind(remote)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .map(
                |(service_instance_id, service_name, member_id, username, remote_id, why)| {
                    ReleasedAccount {
                        service_instance_id,
                        service_name,
                        member_id,
                        username,
                        remote_id,
                        reason: match why.as_str() {
                            "gone" => ReleaseReason::Gone,
                            "handsOff" => ReleaseReason::HandsOff,
                            _ => ReleaseReason::Adopted,
                        },
                    }
                },
            )
            .collect())
    }

    /// Whether the member owns any account in a service (a deletion order
    /// would disable and later delete them).
    pub async fn member_owns_accounts(&self, member_id: i64) -> Result<bool, DbError> {
        let (owns,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM service_account
                            WHERE member_id = ? AND remote_id IS NOT NULL)",
        )
        .bind(member_id)
        .fetch_one(self.pool())
        .await?;
        Ok(owns)
    }

    /// Record what Starr left the member's account as after its own write
    /// (drift baseline, D13): `libraries` `None` = every library.
    pub async fn record_expected(
        &self,
        instance_id: i64,
        member_id: i64,
        remote_id: &str,
        enabled: bool,
        libraries: Option<&[String]>,
    ) -> Result<(), DbError> {
        let libraries = libraries.map(serde_json::to_string).transpose()?;
        sqlx::query(
            "UPDATE service_account SET expected_enabled = ?, expected_libraries = ?
             WHERE service_instance_id = ? AND member_id = ? AND remote_id = ?",
        )
        .bind(enabled)
        .bind(libraries)
        .bind(instance_id)
        .bind(member_id)
        .bind(remote_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// A complete live run found these members' accounts in sync: their
    /// observed state becomes the drift baseline.
    pub async fn record_in_sync(&self, instance_id: i64, members: &[i64]) -> Result<(), DbError> {
        if members.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "UPDATE service_account SET
                 expected_enabled = (SELECT r.enabled FROM remote_account r
                     WHERE r.service_instance_id = service_account.service_instance_id
                       AND r.remote_id = service_account.remote_id),
                 expected_libraries = (SELECT r.libraries FROM remote_account r
                     WHERE r.service_instance_id = service_account.service_instance_id
                       AND r.remote_id = service_account.remote_id)
             WHERE service_instance_id = ? AND remote_id IS NOT NULL
               AND member_id IN (SELECT value FROM json_each(?))
               AND EXISTS (SELECT 1 FROM remote_account r
                   WHERE r.service_instance_id = service_account.service_instance_id
                     AND r.remote_id = service_account.remote_id AND r.gone_since IS NULL)",
        )
        .bind(instance_id)
        .bind(serde_json::to_string(members)?)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Record `count` destructive writes executed on an instance at `now`
    /// (rolling cap); records older than a day are pruned.
    pub async fn record_destructive_writes(
        &self,
        instance_id: i64,
        count: u32,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        let mut tx = self.begin_write().await?;
        for _ in 0..count {
            sqlx::query(
                "INSERT INTO account_destructive_write (service_instance_id, at) VALUES (?, ?)",
            )
            .bind(instance_id)
            .bind(ts(now))
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM account_destructive_write WHERE at < ?")
            .bind(ts(now - DESTRUCTIVE_RETENTION))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Destructive writes executed on an instance within the cap window
    /// before `now`.
    pub async fn recent_destructive_writes(
        &self,
        instance_id: i64,
        now: DateTime<Utc>,
    ) -> Result<u32, DbError> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM account_destructive_write
             WHERE service_instance_id = ? AND at > ?",
        )
        .bind(instance_id)
        .bind(ts(now - CAP_WINDOW))
        .fetch_one(self.pool())
        .await?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Mark that a password write of `generation` into the member's
    /// account is about to start (retry safety). `false` if the account is
    /// not the member's any more.
    pub async fn begin_password_attempt(
        &self,
        instance_id: i64,
        member_id: i64,
        remote_id: &str,
        generation: i64,
    ) -> Result<bool, DbError> {
        Ok(sqlx::query(
            "UPDATE service_account SET password_attempt_generation = ?
             WHERE service_instance_id = ? AND member_id = ? AND remote_id = ?",
        )
        .bind(generation)
        .bind(instance_id)
        .bind(member_id)
        .bind(remote_id)
        .execute(self.pool())
        .await?
        .rows_affected()
            == 1)
    }

    /// The password write's result is known (it failed): the marker goes.
    pub async fn clear_password_attempt(
        &self,
        instance_id: i64,
        member_id: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE service_account SET password_attempt_generation = NULL
             WHERE service_instance_id = ? AND member_id = ?",
        )
        .bind(instance_id)
        .bind(member_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Pause the account writes of every live instance at once (M10 1: an
    /// active admin has no TOTP, maintainer decision 2026-11-03), by
    /// `by`. Pending approvals are dropped. Returns the instances paused
    /// now (id, name); already paused ones stay as they are.
    pub async fn pause_all_live_account_writes(
        &self,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<(i64, String)>, DbError> {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "UPDATE service_instance SET writes_paused_at = ?1, writes_paused_by = ?2,
                 accounts_approved_fingerprint = NULL, accounts_approved_at = NULL,
                 accounts_approved_by = NULL, updated_at = ?1
             WHERE accounts_live_since IS NOT NULL AND writes_paused_at IS NULL
             RETURNING id, name",
        )
        .bind(ts(now))
        .bind(by)
        .fetch_all(self.pool())
        .await?;
        let mut rows = rows;
        rows.sort_by_key(|(id, _)| *id);
        Ok(rows)
    }
}

/// Whether the member owns an account Starr created (a fast-path delete
/// must not leave it behind unmanaged in the service).
pub(crate) async fn owns_provisioned_in(
    conn: &mut sqlx::SqliteConnection,
    member_id: i64,
) -> Result<bool, DbError> {
    let (owns,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM service_account
                        WHERE member_id = ? AND origin = ? AND remote_id IS NOT NULL)",
    )
    .bind(member_id)
    .bind(Origin::Provisioned.as_str())
    .fetch_one(conn)
    .await?;
    Ok(owns)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::members::NewMember;
    use crate::remote_accounts::{AccountScan, MissingAccount, ObservedAccount};
    use crate::service_accounts::DecisionChange;
    use crate::services::{NewServiceInstance, StoredCredential};
    use crate::test_support::t0;

    async fn instance(db: &Database, name: &str) -> i64 {
        db.create_service(
            &NewServiceInstance {
                adapter_type: "jellyfin",
                name,
                base_url: "http://jellyfin:8096",
                public_url: None,
                credential: &StoredCredential {
                    ciphertext: b"sealed".to_vec(),
                    recipient: "age1test".to_owned(),
                    class: starr_core::adapters::SecretClass::ServiceCredential,
                },
            },
            t0(),
        )
        .await
        .unwrap()
        .id
    }

    async fn member(db: &Database, name: &str) -> i64 {
        let email = format!("{name}@example.org");
        db.create_member(
            &NewMember {
                username: name,
                email: &email,
                display_name: name,
                password_hash: None,
            },
            &[],
            t0(),
        )
        .await
        .unwrap()
    }

    fn observed(id: &str, name: &str) -> ObservedAccount {
        ObservedAccount {
            remote_id: id.to_owned(),
            username: name.to_owned(),
            enabled: true,
            privileged: false,
            locked_by_service: false,
            libraries: None,
        }
    }

    async fn scan(db: &Database, id: i64, observed: Vec<ObservedAccount>, missing: &[&str]) {
        db.store_account_scan(
            id,
            &AccountScan {
                complete: true,
                observed,
                missing: missing
                    .iter()
                    .map(|m| MissingAccount {
                        remote_id: (*m).to_owned(),
                        missing_runs: 2,
                        gone: true,
                    })
                    .collect(),
            },
            t0(),
        )
        .await
        .unwrap();
    }

    async fn link(db: &Database, member: i64, instance: i64, remote: &str) {
        db.decide_service_account(
            member,
            instance,
            DecisionChange::Link { remote_id: remote },
            "root",
            t0(),
        )
        .await
        .unwrap();
    }

    /// Lesson 4: two members, only the one whose grace period is over is
    /// due; a tombstone stays due while it owns an account.
    #[tokio::test]
    async fn deletions_are_due_after_the_grace_period_also_as_tombstones() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let alex = member(&db, "alex").await;
        let sam = member(&db, "sam").await;
        scan(
            &db,
            jf,
            vec![observed("r-alex", "alex"), observed("r-sam", "sam")],
            &[],
        )
        .await;
        link(&db, alex, jf, "r-alex").await;
        link(&db, sam, jf, "r-sam").await;
        let day = TimeDelta::days(1);
        db.schedule_member_deletion(alex, "root", t0() + day, t0())
            .await
            .unwrap();
        db.schedule_member_deletion(sam, "root", t0() + day * 30, t0())
            .await
            .unwrap();
        assert_eq!(db.due_deletions(t0()).await.unwrap(), vec![]);
        let later = t0() + day * 2;
        let due = db.due_deletions(later).await.unwrap();
        assert_eq!(
            due,
            vec![DueDeletion {
                member_id: alex,
                username: "alex".into(),
                tombstoned: false,
            }]
        );
        db.tombstone_due_members(later).await.unwrap();
        assert!(db.due_deletions(later).await.unwrap()[0].tombstoned);
        // Deleted in the service: Starr forgets it, the tombstone is done.
        assert!(db.forget_deleted_account(jf, alex, "r-alex").await.unwrap());
        assert!(!db.forget_deleted_account(jf, alex, "r-alex").await.unwrap());
        assert!(!db.forget_deleted_account(jf, sam, "r-alex").await.unwrap());
        assert_eq!(db.due_deletions(later).await.unwrap(), vec![]);
        let inventory = db.remote_accounts(jf).await.unwrap();
        assert!(inventory.iter().all(|a| a.remote_id != "r-alex"));
        assert!(db.member_owns_accounts(sam).await.unwrap());
        assert!(!db.member_owns_accounts(alex).await.unwrap());
    }

    #[tokio::test]
    async fn accounts_gone_from_the_service_are_released_only_for_due_deletions() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let alex = member(&db, "alex").await;
        let sam = member(&db, "sam").await;
        scan(
            &db,
            jf,
            vec![observed("r-alex", "alex"), observed("r-sam", "sam")],
            &[],
        )
        .await;
        link(&db, alex, jf, "r-alex").await;
        link(&db, sam, jf, "r-sam").await;
        db.schedule_member_deletion(alex, "root", t0(), t0())
            .await
            .unwrap();
        // Both gone from the service; only alex's deletion is due.
        scan(&db, jf, vec![], &["r-alex", "r-sam"]).await;
        let released = db
            .release_gone_accounts_of_due_deletions(t0())
            .await
            .unwrap();
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].remote_id, "r-alex");
        assert_eq!(released[0].username, "alex");
        assert_eq!(released[0].reason, ReleaseReason::Gone);
        assert!(db.member_owns_accounts(sam).await.unwrap());
    }

    /// Adopted accounts of a due deletion are released once disabled (only
    /// accounts Starr created are deleted, maintainer 2026-11-03); while
    /// still enabled they stay owned so the planner disables them first.
    #[tokio::test]
    async fn adopted_accounts_are_released_once_disabled_when_the_deletion_is_due() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let alex = member(&db, "alex").await;
        let sam = member(&db, "sam").await;
        let mut disabled = observed("r-sam", "sam");
        disabled.enabled = false;
        scan(&db, jf, vec![observed("r-alex", "alex"), disabled], &[]).await;
        link(&db, alex, jf, "r-alex").await;
        link(&db, sam, jf, "r-sam").await;
        for m in [alex, sam] {
            db.schedule_member_deletion(m, "root", t0(), t0())
                .await
                .unwrap();
        }
        let released = db
            .release_gone_accounts_of_due_deletions(t0())
            .await
            .unwrap();
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].remote_id, "r-sam");
        assert_eq!(released[0].reason, ReleaseReason::Adopted);
        assert!(db.member_owns_accounts(alex).await.unwrap());
    }

    /// An account Starr never deletes (admin rights gained, or ignored) is
    /// released when the deletion is due, never deleted; a member in the
    /// grace period keeps theirs.
    #[tokio::test]
    async fn hands_off_accounts_are_released_not_deleted_when_the_deletion_is_due() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let alex = member(&db, "alex").await;
        let sam = member(&db, "sam").await;
        scan(
            &db,
            jf,
            vec![observed("r-alex", "alex"), observed("r-sam", "sam")],
            &[],
        )
        .await;
        link(&db, alex, jf, "r-alex").await;
        link(&db, sam, jf, "r-sam").await;
        db.schedule_member_deletion(alex, "root", t0(), t0())
            .await
            .unwrap();
        db.schedule_member_deletion(sam, "root", t0() + TimeDelta::days(1), t0())
            .await
            .unwrap();
        let mut admin = observed("r-alex", "alex");
        admin.privileged = true;
        let mut sam_admin = observed("r-sam", "sam");
        sam_admin.privileged = true;
        scan(&db, jf, vec![admin, sam_admin], &[]).await;
        let released = db
            .release_gone_accounts_of_due_deletions(t0())
            .await
            .unwrap();
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].remote_id, "r-alex");
        assert_eq!(released[0].reason, ReleaseReason::HandsOff);
        assert!(!db.member_owns_accounts(alex).await.unwrap());
        assert!(db.member_owns_accounts(sam).await.unwrap());
        // The account itself stays in the inventory (and the service).
        let inventory = db.remote_accounts(jf).await.unwrap();
        assert!(inventory.iter().any(|a| a.remote_id == "r-alex"));
    }

    #[tokio::test]
    async fn the_destructive_window_counts_one_instance_for_an_hour() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let emby = instance(&db, "Emby").await;
        db.record_destructive_writes(jf, 3, t0()).await.unwrap();
        db.record_destructive_writes(emby, 1, t0()).await.unwrap();
        assert_eq!(db.recent_destructive_writes(jf, t0()).await.unwrap(), 3);
        let later = t0() + TimeDelta::minutes(59);
        assert_eq!(db.recent_destructive_writes(jf, later).await.unwrap(), 3);
        let after = t0() + TimeDelta::minutes(61);
        assert_eq!(db.recent_destructive_writes(jf, after).await.unwrap(), 0);
        assert_eq!(db.recent_destructive_writes(emby, t0()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn every_live_instance_is_paused_at_once() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let emby = instance(&db, "Emby").await;
        let _plan_only = instance(&db, "Other").await;
        for id in [jf, emby] {
            sqlx::query("UPDATE service_instance SET accounts_live_since = ? WHERE id = ?")
                .bind(ts(t0()))
                .bind(id)
                .execute(db.pool())
                .await
                .unwrap();
        }
        db.pause_account_writes(emby, "root", t0()).await.unwrap();
        let paused = db
            .pause_all_live_account_writes("starr", t0())
            .await
            .unwrap();
        assert_eq!(paused, vec![(jf, "Jellyfin".to_owned())]);
        let writes = db.account_writes(jf).await.unwrap().unwrap();
        assert_eq!(writes.paused_by.as_deref(), Some("starr"));
        let again = db
            .pause_all_live_account_writes("starr", t0())
            .await
            .unwrap();
        assert_eq!(again, vec![]);
    }

    #[tokio::test]
    async fn a_password_attempt_is_marked_until_its_result_is_recorded() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let alex = member(&db, "alex").await;
        scan(&db, jf, vec![observed("r-alex", "alex")], &[]).await;
        link(&db, alex, jf, "r-alex").await;
        db.request_password_switch(alex, jf).await.unwrap();
        assert!(
            db.begin_password_attempt(jf, alex, "r-alex", 1)
                .await
                .unwrap()
        );
        assert!(
            !db.begin_password_attempt(jf, alex, "r-other", 1)
                .await
                .unwrap()
        );
        let row = db.member_service_accounts(alex).await.unwrap()[0].clone();
        assert_eq!(row.password_attempt, Some(1));
        db.record_password_written(jf, alex, "r-alex", 1)
            .await
            .unwrap();
        let row = db.member_service_accounts(alex).await.unwrap()[0].clone();
        assert_eq!(
            (row.password_attempt, row.password_written),
            (None, Some(1))
        );
        db.begin_password_attempt(jf, alex, "r-alex", 2)
            .await
            .unwrap();
        db.clear_password_attempt(jf, alex).await.unwrap();
        let row = db.member_service_accounts(alex).await.unwrap()[0].clone();
        assert_eq!(row.password_attempt, None);
    }

    #[tokio::test]
    async fn the_drift_baseline_follows_writes_and_in_sync_runs() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let alex = member(&db, "alex").await;
        let sam = member(&db, "sam").await;
        let mut limited = observed("r-sam", "sam");
        limited.libraries = Some(vec!["movies".into()]);
        scan(&db, jf, vec![observed("r-alex", "alex"), limited], &[]).await;
        link(&db, alex, jf, "r-alex").await;
        link(&db, sam, jf, "r-sam").await;
        let movies = ["movies".to_owned()];
        db.record_expected(jf, alex, "r-alex", false, Some(&movies))
            .await
            .unwrap();
        db.record_in_sync(jf, &[sam]).await.unwrap();
        let rows = db.service_accounts(jf).await.unwrap();
        let expected: Vec<_> = rows.iter().map(|r| r.expected.clone()).collect();
        assert_eq!(
            expected,
            vec![
                Some(starr_core::provisioning::Expected {
                    enabled: false,
                    libraries: Some(movies.to_vec()),
                }),
                Some(starr_core::provisioning::Expected {
                    enabled: true,
                    libraries: Some(movies.to_vec()),
                }),
            ]
        );
    }

    #[tokio::test]
    async fn a_member_owning_a_created_account_is_never_deleted_outright() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let alex = member(&db, "alex").await;
        let sam = member(&db, "sam").await;
        db.decide_service_account(alex, jf, DecisionChange::Create, "root", t0())
            .await
            .unwrap();
        let owned = db
            .record_created_account(jf, alex, "r-new", "alex", Some(0))
            .await
            .unwrap();
        assert!(owned);
        assert!(!db.delete_unused_member(alex).await.unwrap());
        assert!(db.delete_unused_member(sam).await.unwrap());
    }
}
