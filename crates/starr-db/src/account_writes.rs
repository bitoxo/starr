//! Account writes per consume instance (spec 0005 slice 4, M8, D15):
//! whether an admin switched them live, paused them, the last complete live
//! run (one-hour rule), an admin's approval of one plan, and what the last
//! write per member reported. The decision whether a run executes is the
//! pure `starr_core::provisioning::gate`; this module stores its inputs.

use chrono::{DateTime, Utc};
use starr_core::provisioning::{FRESH_FOR, WritesMode};

use crate::remote_accounts::ObservedAccount;
use crate::{Database, DbError, ts};

/// An admin's approval of one plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredApproval {
    pub fingerprint: String,
    pub at: DateTime<Utc>,
    pub by: Option<String>,
}

/// The account-write state of one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountWrites {
    pub live_since: Option<DateTime<Utc>>,
    pub live_by: Option<String>,
    pub paused_at: Option<DateTime<Utc>>,
    pub paused_by: Option<String>,
    pub last_live_run_at: Option<DateTime<Utc>>,
    pub approval: Option<StoredApproval>,
}

impl AccountWrites {
    #[must_use]
    pub const fn mode(&self) -> WritesMode {
        match (self.live_since.is_some(), self.paused_at.is_some()) {
            (false, _) => WritesMode::PlanOnly,
            (true, true) => WritesMode::Paused,
            (true, false) => WritesMode::Live,
        }
    }
}

/// Why a confirmation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmRefusal {
    /// No stored plan, or another one than the admin saw.
    PlanChanged,
    /// The plan rests on an incomplete listing.
    PlanIncomplete,
    /// The plan is older than an hour: scan again first.
    PlanStale,
}

/// What a confirmation changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmOutcome {
    Confirmed {
        writes: AccountWrites,
        /// The instance was plan only and is live now.
        went_live: bool,
        /// The writes were paused and go out again.
        resumed: bool,
    },
    Refused(ConfirmRefusal),
}

/// What pausing changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PauseOutcome {
    Paused {
        writes: AccountWrites,
        changed: bool,
    },
    /// Never switched live: nothing to pause.
    NotLive,
}

/// The result of one write to an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteResult<'a> {
    Ok,
    Failed { code: &'a str, detail: &'a str },
}

/// A failed write waiting for an admin ("needs you").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFailure {
    pub service_id: i64,
    pub service_name: String,
    pub member_id: i64,
    pub username: String,
    pub code: String,
    pub detail: Option<String>,
    pub at: DateTime<Utc>,
}

/// Service id and name, member id and username, code, detail, at.
type FailureRow = (
    i64,
    String,
    i64,
    String,
    String,
    Option<String>,
    DateTime<Utc>,
);

type WritesRow = (
    Option<DateTime<Utc>>,
    Option<String>,
    Option<DateTime<Utc>>,
    Option<String>,
    Option<DateTime<Utc>>,
    Option<String>,
    Option<DateTime<Utc>>,
    Option<String>,
);

const WRITES_COLUMNS: &str = "accounts_live_since, accounts_live_by, writes_paused_at,
     writes_paused_by, accounts_last_live_run_at, accounts_approved_fingerprint,
     accounts_approved_at, accounts_approved_by";

fn writes_from(row: WritesRow) -> AccountWrites {
    let (live_since, live_by, paused_at, paused_by, last_live_run_at, fingerprint, at, by) = row;
    AccountWrites {
        live_since,
        live_by,
        paused_at,
        paused_by,
        last_live_run_at,
        approval: match (fingerprint, at) {
            (Some(fingerprint), Some(at)) => Some(StoredApproval {
                fingerprint,
                at,
                by,
            }),
            _ => None,
        },
    }
}

async fn writes_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: i64,
) -> Result<Option<AccountWrites>, DbError> {
    let sql = format!("SELECT {WRITES_COLUMNS} FROM service_instance WHERE id = ?");
    let row: Option<WritesRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row.map(writes_from))
}

impl Database {
    /// The account-write state of an instance; `None` if it does not exist.
    pub async fn account_writes(&self, id: i64) -> Result<Option<AccountWrites>, DbError> {
        let sql = format!("SELECT {WRITES_COLUMNS} FROM service_instance WHERE id = ?");
        let row: Option<WritesRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.map(writes_from))
    }

    /// An admin confirms the instance's stored plan by its `fingerprint`:
    /// going live (plan only → live), resuming paused writes, or approving
    /// a held plan. The plan must be the stored one, complete and at most an
    /// hour old (checked in the same transaction). Stores the approval, so
    /// the next run executes exactly this plan. `None` if the instance does
    /// not exist.
    pub async fn confirm_account_plan(
        &self,
        id: i64,
        fingerprint: &str,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<ConfirmOutcome>, DbError> {
        let mut tx = self.begin_write().await?;
        let Some(before) = writes_in(&mut tx, id).await? else {
            return Ok(None);
        };
        let plan: Option<(String, bool, DateTime<Utc>)> = sqlx::query_as(
            "SELECT fingerprint, complete, computed_at FROM account_plan
             WHERE service_instance_id = ?",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let refusal = match plan {
            Some((stored, _, _)) if stored != fingerprint => Some(ConfirmRefusal::PlanChanged),
            None => Some(ConfirmRefusal::PlanChanged),
            Some((_, false, _)) => Some(ConfirmRefusal::PlanIncomplete),
            Some((_, true, computed_at)) if now - computed_at > FRESH_FOR => {
                Some(ConfirmRefusal::PlanStale)
            }
            Some(_) => None,
        };
        if let Some(refusal) = refusal {
            tx.rollback().await?;
            return Ok(Some(ConfirmOutcome::Refused(refusal)));
        }
        sqlx::query(
            "UPDATE service_instance SET
                 accounts_live_since = coalesce(accounts_live_since, ?1),
                 accounts_live_by = iif(accounts_live_since IS NULL, ?2, accounts_live_by),
                 writes_paused_at = NULL, writes_paused_by = NULL,
                 accounts_approved_fingerprint = ?3, accounts_approved_at = ?1,
                 accounts_approved_by = ?2, updated_at = ?1
             WHERE id = ?4",
        )
        .bind(ts(now))
        .bind(by)
        .bind(fingerprint)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let writes = writes_in(&mut tx, id)
            .await?
            .ok_or_else(|| DbError::InvalidData("service instance vanished".to_owned()))?;
        tx.commit().await?;
        Ok(Some(ConfirmOutcome::Confirmed {
            went_live: before.live_since.is_none(),
            resumed: before.mode() == WritesMode::Paused,
            writes,
        }))
    }

    /// Pause the account writes of a live instance (plans keep being
    /// computed). A pending approval is dropped. `None` if the instance does
    /// not exist.
    pub async fn pause_account_writes(
        &self,
        id: i64,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<PauseOutcome>, DbError> {
        let mut tx = self.begin_write().await?;
        let Some(before) = writes_in(&mut tx, id).await? else {
            return Ok(None);
        };
        match before.mode() {
            WritesMode::PlanOnly => {
                tx.rollback().await?;
                return Ok(Some(PauseOutcome::NotLive));
            }
            WritesMode::Paused => {
                tx.rollback().await?;
                return Ok(Some(PauseOutcome::Paused {
                    writes: before,
                    changed: false,
                }));
            }
            WritesMode::Live => {}
        }
        sqlx::query(
            "UPDATE service_instance SET writes_paused_at = ?1, writes_paused_by = ?2,
                 accounts_approved_fingerprint = NULL, accounts_approved_at = NULL,
                 accounts_approved_by = NULL, updated_at = ?1
             WHERE id = ?3",
        )
        .bind(ts(now))
        .bind(by)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let writes = writes_in(&mut tx, id)
            .await?
            .ok_or_else(|| DbError::InvalidData("service instance vanished".to_owned()))?;
        tx.commit().await?;
        Ok(Some(PauseOutcome::Paused {
            writes,
            changed: true,
        }))
    }

    /// A complete live run finished at `now`. An approval of the plan it
    /// executed (`executed`) is used up.
    pub async fn record_live_run(
        &self,
        id: i64,
        executed: &str,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE service_instance SET accounts_last_live_run_at = ?1,
                 accounts_approved_fingerprint = iif(accounts_approved_fingerprint = ?2, NULL, accounts_approved_fingerprint),
                 accounts_approved_by = iif(accounts_approved_fingerprint = ?2, NULL, accounts_approved_by),
                 accounts_approved_at = iif(accounts_approved_fingerprint = ?2, NULL, accounts_approved_at)
             WHERE id = ?3",
        )
        .bind(ts(now))
        .bind(executed)
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Store what a write read back into the instance's inventory, so the
    /// next plan starts from it (the next scan confirms it). An account
    /// Starr just created is added (seen now).
    pub async fn store_read_back(
        &self,
        id: i64,
        account: &ObservedAccount,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        let libraries = account
            .libraries
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        sqlx::query(
            "INSERT INTO remote_account (service_instance_id, remote_id, username, enabled,
                 privileged, locked_by_service, libraries, first_seen, last_seen)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8
             WHERE EXISTS (SELECT 1 FROM service_instance WHERE id = ?1)
             ON CONFLICT (service_instance_id, remote_id) DO UPDATE SET
                 username = excluded.username, enabled = excluded.enabled,
                 privileged = excluded.privileged,
                 locked_by_service = excluded.locked_by_service,
                 libraries = excluded.libraries, last_seen = excluded.last_seen",
        )
        .bind(id)
        .bind(&account.remote_id)
        .bind(&account.username)
        .bind(account.enabled)
        .bind(account.privileged)
        .bind(account.locked_by_service)
        .bind(libraries)
        .bind(ts(now))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Record the result of a write for a member's account: a failure is
    /// kept for "needs you", a success clears it.
    pub async fn record_account_write(
        &self,
        id: i64,
        member_id: i64,
        result: WriteResult<'_>,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        let (code, detail, at) = match result {
            WriteResult::Ok => (None, None, None),
            WriteResult::Failed { code, detail } => (Some(code), Some(detail), Some(ts(now))),
        };
        sqlx::query(
            "UPDATE service_account SET last_error = ?, last_error_detail = ?, last_error_at = ?
             WHERE service_instance_id = ? AND member_id = ?",
        )
        .bind(code)
        .bind(detail)
        .bind(at)
        .bind(id)
        .bind(member_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Drop the failures of members on an instance who have no step left
    /// to retry (`pending`: the members whose plan entry still has an
    /// executable step), e.g. the account got locked, ignored or decided
    /// otherwise since.
    pub async fn clear_settled_write_failures(
        &self,
        id: i64,
        pending: &[i64],
    ) -> Result<(), DbError> {
        let pending = serde_json::to_string(pending)?;
        sqlx::query(
            "UPDATE service_account SET last_error = NULL, last_error_detail = NULL,
                 last_error_at = NULL
             WHERE service_instance_id = ? AND last_error IS NOT NULL
               AND member_id NOT IN (SELECT value FROM json_each(?))",
        )
        .bind(id)
        .bind(pending)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Every failed account write still unresolved, newest first.
    pub async fn account_write_failures(&self) -> Result<Vec<WriteFailure>, DbError> {
        let rows: Vec<FailureRow> = sqlx::query_as(
            "SELECT s.id, s.name, m.id, m.username, a.last_error, a.last_error_detail,
                        a.last_error_at
                 FROM service_account a
                 JOIN service_instance s ON s.id = a.service_instance_id
                 JOIN member m ON m.id = a.member_id
                 WHERE a.last_error IS NOT NULL
                 ORDER BY a.last_error_at DESC, s.id, m.id",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(service_id, service_name, member_id, username, code, detail, at)| WriteFailure {
                    service_id,
                    service_name,
                    member_id,
                    username,
                    code,
                    detail,
                    at,
                },
            )
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;
    use crate::members::NewMember;
    use crate::remote_accounts::AccountScan;
    use crate::service_accounts::{DecisionChange, StoredPlan};
    use crate::services::{NewServiceInstance, StoredCredential};
    use crate::test_support::t0;

    async fn instance(db: &Database) -> i64 {
        db.create_service(
            &NewServiceInstance {
                adapter_type: "jellyfin",
                name: "Jellyfin",
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

    async fn plan(db: &Database, id: i64, fingerprint: &str, complete: bool, at: DateTime<Utc>) {
        db.store_account_plan(&StoredPlan {
            service_instance_id: id,
            computed_at: at,
            complete,
            fingerprint: fingerprint.to_owned(),
            plan: r#"{"complete":true,"entries":[]}"#.to_owned(),
            counts: "{}".to_owned(),
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn every_instance_starts_plan_only() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        let writes = db.account_writes(id).await.unwrap().unwrap();
        assert_eq!(writes.mode(), WritesMode::PlanOnly);
        assert_eq!(writes.approval, None);
        assert_eq!(db.account_writes(id + 1).await.unwrap(), None);
    }

    #[tokio::test]
    async fn going_live_needs_the_current_complete_fresh_plan() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        let now = t0();
        let refused = |r: Option<ConfirmOutcome>| match r {
            Some(ConfirmOutcome::Refused(why)) => why,
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert_eq!(
            refused(
                db.confirm_account_plan(id, "fp", "root", now)
                    .await
                    .unwrap()
            ),
            ConfirmRefusal::PlanChanged
        );
        plan(&db, id, "fp", false, now).await;
        assert_eq!(
            refused(
                db.confirm_account_plan(id, "fp", "root", now)
                    .await
                    .unwrap()
            ),
            ConfirmRefusal::PlanIncomplete
        );
        plan(&db, id, "fp", true, now).await;
        assert_eq!(
            refused(
                db.confirm_account_plan(id, "other", "root", now)
                    .await
                    .unwrap()
            ),
            ConfirmRefusal::PlanChanged
        );
        let later = now + TimeDelta::minutes(61);
        assert_eq!(
            refused(
                db.confirm_account_plan(id, "fp", "root", later)
                    .await
                    .unwrap()
            ),
            ConfirmRefusal::PlanStale
        );
        assert_eq!(
            db.account_writes(id).await.unwrap().unwrap().mode(),
            WritesMode::PlanOnly,
            "refusals change nothing"
        );
        let Some(ConfirmOutcome::Confirmed {
            writes,
            went_live,
            resumed,
        }) = db
            .confirm_account_plan(id, "fp", "root", now)
            .await
            .unwrap()
        else {
            panic!("not confirmed");
        };
        assert!(went_live && !resumed);
        assert_eq!(writes.mode(), WritesMode::Live);
        assert_eq!(writes.live_by.as_deref(), Some("root"));
        assert_eq!(
            writes.approval,
            Some(StoredApproval {
                fingerprint: "fp".into(),
                at: now,
                by: Some("root".into())
            })
        );
    }

    #[tokio::test]
    async fn pause_and_resume_keep_who_went_live() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        let now = t0();
        assert_eq!(
            db.pause_account_writes(id, "root", now).await.unwrap(),
            Some(PauseOutcome::NotLive)
        );
        plan(&db, id, "fp", true, now).await;
        db.confirm_account_plan(id, "fp", "root", now)
            .await
            .unwrap();
        let Some(PauseOutcome::Paused { writes, changed }) =
            db.pause_account_writes(id, "ops", now).await.unwrap()
        else {
            panic!("not paused");
        };
        assert!(changed);
        assert_eq!(writes.mode(), WritesMode::Paused);
        assert_eq!(writes.paused_by.as_deref(), Some("ops"));
        assert_eq!(writes.approval, None, "a pause drops the pending approval");
        let Some(PauseOutcome::Paused { changed, .. }) =
            db.pause_account_writes(id, "ops", now).await.unwrap()
        else {
            panic!("not paused");
        };
        assert!(!changed);
        let Some(ConfirmOutcome::Confirmed {
            writes,
            went_live,
            resumed,
        }) = db.confirm_account_plan(id, "fp", "ops", now).await.unwrap()
        else {
            panic!("not resumed");
        };
        assert!(resumed && !went_live);
        assert_eq!(writes.mode(), WritesMode::Live);
        assert_eq!(writes.live_by.as_deref(), Some("root"));
    }

    #[tokio::test]
    async fn a_live_run_uses_up_only_the_approval_it_executed() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        let now = t0();
        plan(&db, id, "fp", true, now).await;
        db.confirm_account_plan(id, "fp", "root", now)
            .await
            .unwrap();
        db.record_live_run(id, "other", now).await.unwrap();
        let writes = db.account_writes(id).await.unwrap().unwrap();
        assert_eq!(writes.last_live_run_at, Some(now));
        assert!(writes.approval.is_some());
        db.record_live_run(id, "fp", now).await.unwrap();
        assert_eq!(db.account_writes(id).await.unwrap().unwrap().approval, None);
    }

    #[tokio::test]
    async fn write_failures_are_kept_until_a_write_succeeds() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        let member = db
            .create_member(
                &NewMember {
                    username: "alex",
                    email: "alex@example.org",
                    display_name: "Alex",
                    password_hash: None,
                },
                &[],
                t0(),
            )
            .await
            .unwrap();
        db.store_account_scan(
            id,
            &AccountScan {
                complete: true,
                observed: vec![ObservedAccount {
                    remote_id: "r1".into(),
                    username: "Alex".into(),
                    enabled: true,
                    privileged: false,
                    locked_by_service: false,
                    libraries: None,
                }],
                missing: vec![],
            },
            t0(),
        )
        .await
        .unwrap();
        db.decide_service_account(
            member,
            id,
            DecisionChange::Link { remote_id: "r1" },
            "root",
            t0(),
        )
        .await
        .unwrap();
        db.record_account_write(
            id,
            member,
            WriteResult::Failed {
                code: "transient",
                detail: "POST Users/r1/Policy: HTTP 503",
            },
            t0(),
        )
        .await
        .unwrap();
        let failures = db.account_write_failures().await.unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].username, "alex");
        assert_eq!(failures[0].code, "transient");
        db.record_account_write(id, member, WriteResult::Ok, t0())
            .await
            .unwrap();
        assert_eq!(db.account_write_failures().await.unwrap(), vec![]);

        // A read-back lands in the inventory.
        db.store_read_back(
            id,
            &ObservedAccount {
                remote_id: "r1".into(),
                username: "Alex".into(),
                enabled: false,
                privileged: false,
                locked_by_service: false,
                libraries: Some(vec!["m".into()]),
            },
            t0(),
        )
        .await
        .unwrap();
        let rows = db.remote_accounts(id).await.unwrap();
        assert!(!rows[0].enabled);
        assert_eq!(rows[0].libraries, Some(vec!["m".to_owned()]));
    }
}
