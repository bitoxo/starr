//! Foreign account inventory (spec 0005 D7, slice 1): every account a
//! consume service lists, per instance. Presence decisions come from the
//! pure `starr_core::accounts::presence`; this module stores and loads rows.

use chrono::{DateTime, TimeDelta, Utc};

use crate::{Database, DbError, ts};

/// Gone accounts are purged this long after they were declared gone (D7).
pub const GONE_RETENTION: TimeDelta = TimeDelta::days(30);

/// One stored account (account × service instance).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // the inventory row's flags
pub struct RemoteAccountRow {
    pub remote_id: String,
    pub username: String,
    pub enabled: bool,
    pub privileged: bool,
    pub locked_by_service: bool,
    /// `None`: every library; otherwise the service's grant IDs.
    pub libraries: Option<Vec<String>>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub missing_runs: u32,
    pub gone_since: Option<DateTime<Utc>>,
    pub ignored_at: Option<DateTime<Utc>>,
    /// Username of the admin who marked it ignored.
    pub ignored_by: Option<String>,
    /// Starr locked the account with a random password (a service without
    /// a disable flag, D9); cleared when a member's password is written.
    pub locked_by_starr: bool,
}

/// An account as listed by this scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedAccount {
    pub remote_id: String,
    pub username: String,
    pub enabled: bool,
    pub privileged: bool,
    pub locked_by_service: bool,
    pub libraries: Option<Vec<String>>,
}

/// A known account this scan did not list (complete listings only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingAccount {
    pub remote_id: String,
    pub missing_runs: u32,
    /// Declared gone by this scan.
    pub gone: bool,
}

/// What one successful scan of an instance stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountScan {
    pub complete: bool,
    pub observed: Vec<ObservedAccount>,
    pub missing: Vec<MissingAccount>,
}

/// The last scan of an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanState {
    pub scanned_at: Option<DateTime<Utc>>,
    pub complete: Option<bool>,
    /// Error code of the last scan; `None` when it succeeded.
    pub error: Option<String>,
}

/// Scanned at, complete, error.
type ScanStateRow = (Option<DateTime<Utc>>, Option<bool>, Option<String>);

#[derive(sqlx::FromRow)]
#[allow(clippy::struct_excessive_bools)] // the inventory row's flags
struct Row {
    remote_id: String,
    username: String,
    enabled: bool,
    privileged: bool,
    locked_by_service: bool,
    libraries: Option<String>,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    missing_runs: i64,
    gone_since: Option<DateTime<Utc>>,
    ignored_at: Option<DateTime<Utc>>,
    ignored_by: Option<String>,
    locked_by_starr: bool,
}

impl TryFrom<Row> for RemoteAccountRow {
    type Error = DbError;

    fn try_from(row: Row) -> Result<Self, DbError> {
        Ok(Self {
            libraries: row
                .libraries
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            missing_runs: u32::try_from(row.missing_runs).map_err(|_| {
                DbError::InvalidData(format!(
                    "remote_account.missing_runs = {}",
                    row.missing_runs
                ))
            })?,
            remote_id: row.remote_id,
            username: row.username,
            enabled: row.enabled,
            privileged: row.privileged,
            locked_by_service: row.locked_by_service,
            first_seen: row.first_seen,
            last_seen: row.last_seen,
            gone_since: row.gone_since,
            ignored_at: row.ignored_at,
            ignored_by: row.ignored_by,
            locked_by_starr: row.locked_by_starr,
        })
    }
}

const COLUMNS: &str = "remote_id, username, enabled, privileged, locked_by_service, libraries,
     first_seen, last_seen, missing_runs, gone_since, ignored_at, ignored_by, locked_by_starr";

impl Database {
    /// Every stored account of one instance, by name (case-insensitive).
    pub async fn remote_accounts(
        &self,
        instance_id: i64,
    ) -> Result<Vec<RemoteAccountRow>, DbError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM remote_account WHERE service_instance_id = ?
             ORDER BY username COLLATE NOCASE, remote_id"
        );
        let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(instance_id)
            .fetch_all(self.pool())
            .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Store one successful scan in one transaction: listed accounts are
    /// inserted or refreshed (present again, missing counter reset), missing
    /// ones get their counter and `gone_since`, gone rows older than
    /// [`GONE_RETENTION`] are purged and the scan state is recorded.
    /// `false`, and nothing stored, if the instance no longer exists.
    pub async fn store_account_scan(
        &self,
        instance_id: i64,
        scan: &AccountScan,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let updated = sqlx::query(
            "UPDATE service_instance SET accounts_scanned_at = ?, accounts_scan_complete = ?,
                 accounts_scan_error = NULL
             WHERE id = ?",
        )
        .bind(ts(now))
        .bind(scan.complete)
        .bind(instance_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Ok(false);
        }
        for a in &scan.observed {
            let libraries = a
                .libraries
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            sqlx::query(
                "INSERT INTO remote_account (service_instance_id, remote_id, username, enabled,
                     privileged, locked_by_service, libraries, first_seen, last_seen)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (service_instance_id, remote_id) DO UPDATE SET
                     username = excluded.username,
                     enabled = excluded.enabled,
                     privileged = excluded.privileged,
                     locked_by_service = excluded.locked_by_service,
                     libraries = excluded.libraries,
                     last_seen = excluded.last_seen,
                     missing_runs = 0,
                     gone_since = NULL",
            )
            .bind(instance_id)
            .bind(&a.remote_id)
            .bind(&a.username)
            .bind(a.enabled)
            .bind(a.privileged)
            .bind(a.locked_by_service)
            .bind(libraries)
            .bind(ts(now))
            .bind(ts(now))
            .execute(&mut *tx)
            .await?;
        }
        for m in &scan.missing {
            sqlx::query(
                "UPDATE remote_account SET missing_runs = ?,
                     gone_since = CASE WHEN ? THEN coalesce(gone_since, ?) ELSE gone_since END
                 WHERE service_instance_id = ? AND remote_id = ?",
            )
            .bind(m.missing_runs)
            .bind(m.gone)
            .bind(ts(now))
            .bind(instance_id)
            .bind(&m.remote_id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM remote_account WHERE service_instance_id = ? AND gone_since < ?")
            .bind(instance_id)
            .bind(ts(now - GONE_RETENTION))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Record a failed scan (the error code); stored accounts stay as they
    /// are.
    pub async fn record_account_scan_failure(
        &self,
        instance_id: i64,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE service_instance SET accounts_scanned_at = ?, accounts_scan_complete = NULL,
                 accounts_scan_error = ?
             WHERE id = ?",
        )
        .bind(ts(now))
        .bind(code)
        .bind(instance_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The last scan of an instance; `None` if the instance is unknown.
    pub async fn account_scan_state(&self, instance_id: i64) -> Result<Option<ScanState>, DbError> {
        let row: Option<ScanStateRow> = sqlx::query_as(
            "SELECT accounts_scanned_at, accounts_scan_complete, accounts_scan_error
             FROM service_instance WHERE id = ?",
        )
        .bind(instance_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|(scanned_at, complete, error)| ScanState {
            scanned_at,
            complete,
            error,
        }))
    }

    /// Mark an account ignored by `admin` (`Some`) or not ignored (`None`).
    /// Returns the row as it is now and whether this call changed it;
    /// `None` if there is no such account.
    pub async fn set_remote_account_ignored(
        &self,
        instance_id: i64,
        remote_id: &str,
        admin: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Option<(RemoteAccountRow, bool)>, DbError> {
        let sql = format!(
            "UPDATE remote_account SET
                 ignored_at = CASE WHEN ?1 IS NULL THEN NULL ELSE coalesce(ignored_at, ?2) END,
                 ignored_by = CASE WHEN ?1 IS NULL THEN NULL ELSE coalesce(ignored_by, ?1) END
             WHERE service_instance_id = ?3 AND remote_id = ?4
               AND (ignored_at IS NULL) = (?1 IS NOT NULL)
             RETURNING {COLUMNS}"
        );
        let changed: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(admin)
            .bind(ts(now))
            .bind(instance_id)
            .bind(remote_id)
            .fetch_optional(self.pool())
            .await?;
        if let Some(row) = changed {
            return Ok(Some((row.try_into()?, true)));
        }
        let sql = format!(
            "SELECT {COLUMNS} FROM remote_account WHERE service_instance_id = ? AND remote_id = ?"
        );
        let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(instance_id)
            .bind(remote_id)
            .fetch_optional(self.pool())
            .await?;
        row.map(|r| r.try_into().map(|row| (row, false)))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn scan(observed: Vec<ObservedAccount>, missing: Vec<MissingAccount>) -> AccountScan {
        AccountScan {
            complete: true,
            observed,
            missing,
        }
    }

    #[tokio::test]
    async fn scans_insert_refresh_and_mark_missing_accounts() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        let restricted = ObservedAccount {
            libraries: Some(vec!["m".to_owned()]),
            privileged: true,
            ..observed("b", "Bo")
        };
        assert!(
            db.store_account_scan(
                id,
                &scan(vec![observed("a", "alex"), restricted], vec![]),
                t0()
            )
            .await
            .unwrap()
        );
        let rows = db.remote_accounts(id).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].username, "alex");
        assert_eq!(rows[1].libraries, Some(vec!["m".to_owned()]));
        assert!(rows[1].privileged);
        assert_eq!(rows[0].first_seen, t0());

        let later = t0() + TimeDelta::hours(1);
        let renamed = observed("a", "Alexander");
        let missing = MissingAccount {
            remote_id: "b".to_owned(),
            missing_runs: 2,
            gone: true,
        };
        db.store_account_scan(id, &scan(vec![renamed], vec![missing]), later)
            .await
            .unwrap();
        let rows = db.remote_accounts(id).await.unwrap();
        assert_eq!(rows[0].username, "Alexander");
        assert_eq!(rows[0].first_seen, t0(), "first seen kept");
        assert_eq!(rows[0].last_seen, later);
        assert_eq!(rows[1].gone_since, Some(later));
        assert_eq!(rows[1].missing_runs, 2);
        let state = db.account_scan_state(id).await.unwrap().unwrap();
        assert_eq!(state.scanned_at, Some(later));
        assert_eq!(state.complete, Some(true));

        // Listed again: present, counter reset.
        db.store_account_scan(id, &scan(vec![observed("b", "Bo")], vec![]), later)
            .await
            .unwrap();
        let bo = &db.remote_accounts(id).await.unwrap()[1];
        assert_eq!((bo.gone_since, bo.missing_runs), (None, 0));
    }

    #[tokio::test]
    async fn gone_accounts_are_purged_after_thirty_days() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        db.store_account_scan(id, &scan(vec![observed("a", "a")], vec![]), t0())
            .await
            .unwrap();
        let gone = MissingAccount {
            remote_id: "a".to_owned(),
            missing_runs: 2,
            gone: true,
        };
        db.store_account_scan(id, &scan(vec![], vec![gone]), t0())
            .await
            .unwrap();
        db.store_account_scan(id, &scan(vec![], vec![]), t0() + TimeDelta::days(29))
            .await
            .unwrap();
        assert_eq!(db.remote_accounts(id).await.unwrap().len(), 1);
        db.store_account_scan(id, &scan(vec![], vec![]), t0() + TimeDelta::days(31))
            .await
            .unwrap();
        assert_eq!(db.remote_accounts(id).await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn ignoring_is_a_starr_side_flag_kept_across_scans() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        db.store_account_scan(id, &scan(vec![observed("a", "a")], vec![]), t0())
            .await
            .unwrap();
        let (row, changed) = db
            .set_remote_account_ignored(id, "a", Some("root"), t0())
            .await
            .unwrap()
            .unwrap();
        assert!(changed);
        assert_eq!(row.ignored_by.as_deref(), Some("root"));
        assert_eq!(row.ignored_at, Some(t0()));
        // Again, by someone else: nothing changes.
        let later = t0() + TimeDelta::hours(1);
        let (row, changed) = db
            .set_remote_account_ignored(id, "a", Some("other"), later)
            .await
            .unwrap()
            .unwrap();
        assert!(!changed);
        assert_eq!(
            (row.ignored_by.as_deref(), row.ignored_at),
            (Some("root"), Some(t0()))
        );
        db.store_account_scan(id, &scan(vec![observed("a", "a")], vec![]), t0())
            .await
            .unwrap();
        assert_eq!(
            db.remote_accounts(id).await.unwrap()[0]
                .ignored_by
                .as_deref(),
            Some("root")
        );
        let (row, changed) = db
            .set_remote_account_ignored(id, "a", None, t0())
            .await
            .unwrap()
            .unwrap();
        assert!(changed);
        assert_eq!((row.ignored_at, row.ignored_by), (None, None));
        assert_eq!(
            db.set_remote_account_ignored(id, "nope", Some("root"), t0())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn failures_and_removed_instances() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        db.record_account_scan_failure(id, "transient", t0())
            .await
            .unwrap();
        let state = db.account_scan_state(id).await.unwrap().unwrap();
        assert_eq!(state.error.as_deref(), Some("transient"));
        assert_eq!(state.complete, None);
        assert!(db.delete_service(id, t0()).await.unwrap());
        assert!(
            !db.store_account_scan(id, &scan(vec![observed("a", "a")], vec![]), t0())
                .await
                .unwrap()
        );
        assert_eq!(db.account_scan_state(id).await.unwrap(), None);
    }
}
