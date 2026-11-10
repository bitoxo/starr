//! Service instances (spec 0002, ADR 0009). The credential is stored sealed;
//! this module only moves bytes and never sees plaintext. The listing type
//! [`ServiceInstance`] carries no credential at all; only
//! [`Database::service_connection`] (used by the worker) returns it.

use chrono::{DateTime, Utc};
use starr_core::adapters::SecretClass;

use crate::{Database, DbError, ts};

/// A service instance as shown to admins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInstance {
    pub id: i64,
    pub adapter_type: String,
    pub name: String,
    pub base_url: String,
    /// Address members reach the web client at, for links; `None`: `base_url`.
    pub public_url: Option<String>,
    pub mode: String,
    pub detected_version: Option<String>,
    pub capabilities: Vec<String>,
    pub last_check_at: Option<DateTime<Utc>>,
    /// Error taxonomy code of the last check; `None` if it succeeded or
    /// none ran yet.
    pub last_error: Option<String>,
    /// Sources: since when an admin paused writes to it (spec 0003,
    /// maintainer 2026-11-02); `None` while writes go out.
    pub writes_paused_at: Option<DateTime<Utc>>,
    /// Username of the admin who paused them.
    pub writes_paused_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A sealed credential as stored: ciphertext, the recipient it was sealed
/// for, and the class of key that recipient belongs to (ADR 0003
/// amendment 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCredential {
    pub ciphertext: Vec<u8>,
    pub recipient: String,
    pub class: SecretClass,
}

fn parse_class(raw: &str) -> Result<SecretClass, DbError> {
    SecretClass::parse(raw).ok_or_else(|| crate::unknown("credential class", raw))
}

/// What the worker needs to talk to an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConnection {
    pub id: i64,
    pub adapter_type: String,
    pub name: String,
    pub base_url: String,
    pub credential: StoredCredential,
    /// An admin paused writes to it: nothing may be added, monitored or
    /// searched there.
    pub writes_paused: bool,
}

#[derive(Debug)]
pub struct NewServiceInstance<'a> {
    pub adapter_type: &'a str,
    pub name: &'a str,
    pub base_url: &'a str,
    pub public_url: Option<&'a str>,
    pub credential: &'a StoredCredential,
}

/// Fields to change; `None` keeps the current value.
#[derive(Debug, Default)]
pub struct ServiceUpdate<'a> {
    pub name: Option<&'a str>,
    pub base_url: Option<&'a str>,
    /// `Some(None)` clears the public URL.
    #[allow(clippy::option_option)]
    pub public_url: Option<Option<&'a str>>,
    pub credential: Option<&'a StoredCredential>,
}

/// Outcome of one check, written by the worker.
#[derive(Debug)]
pub enum CheckOutcome<'a> {
    Ok {
        version: &'a str,
        capabilities: &'a [&'a str],
    },
    Failed {
        /// Error taxonomy code.
        code: &'a str,
        /// Version, if the service reported one (e.g. outside the tested range).
        version: Option<&'a str>,
    },
}

#[derive(sqlx::FromRow)]
struct ConnectionRow {
    id: i64,
    adapter_type: String,
    name: String,
    base_url: String,
    credential_sealed: Vec<u8>,
    credential_recipient: String,
    credential_class: String,
    writes_paused: bool,
}

macro_rules! connection_columns {
    () => {
        "id, adapter_type, name, base_url, credential_sealed, credential_recipient, credential_class, \
         writes_paused_at IS NOT NULL AS writes_paused"
    };
}

impl TryFrom<ConnectionRow> for ServiceConnection {
    type Error = DbError;

    fn try_from(row: ConnectionRow) -> Result<Self, DbError> {
        Ok(Self {
            id: row.id,
            adapter_type: row.adapter_type,
            name: row.name,
            base_url: row.base_url,
            credential: StoredCredential {
                ciphertext: row.credential_sealed,
                recipient: row.credential_recipient,
                class: parse_class(&row.credential_class)?,
            },
            writes_paused: row.writes_paused,
        })
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    adapter_type: String,
    name: String,
    base_url: String,
    public_url: Option<String>,
    mode: String,
    detected_version: Option<String>,
    capabilities: Option<String>,
    last_check_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    writes_paused_at: Option<DateTime<Utc>>,
    writes_paused_by: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// The columns read into [`Row`], as a literal for `concat!`.
macro_rules! columns {
    () => {
        "id, adapter_type, name, base_url, public_url, mode, detected_version, capabilities, \
         last_check_at, last_error, writes_paused_at, writes_paused_by, created_at, updated_at"
    };
}

impl TryFrom<Row> for ServiceInstance {
    type Error = DbError;

    fn try_from(row: Row) -> Result<Self, DbError> {
        let capabilities = row
            .capabilities
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            id: row.id,
            adapter_type: row.adapter_type,
            name: row.name,
            base_url: row.base_url,
            public_url: row.public_url,
            mode: row.mode,
            detected_version: row.detected_version,
            capabilities,
            last_check_at: row.last_check_at,
            last_error: row.last_error,
            writes_paused_at: row.writes_paused_at,
            writes_paused_by: row.writes_paused_by,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

fn name_conflict(err: sqlx::Error) -> DbError {
    if let sqlx::Error::Database(db) = &err
        && db.is_unique_violation()
    {
        return DbError::ServiceNameTaken;
    }
    DbError::Sqlx(err)
}

impl Database {
    pub async fn create_service(
        &self,
        new: &NewServiceInstance<'_>,
        now: DateTime<Utc>,
    ) -> Result<ServiceInstance, DbError> {
        let row: Row = sqlx::query_as(concat!("INSERT INTO service_instance
                 (adapter_type, name, base_url, public_url, credential_sealed, credential_recipient, credential_class, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING ", columns!(), ""))
        .bind(new.adapter_type)
        .bind(new.name)
        .bind(new.base_url)
        .bind(new.public_url)
        .bind(&new.credential.ciphertext)
        .bind(&new.credential.recipient)
        .bind(new.credential.class.as_str())
        .bind(ts(now))
        .bind(ts(now))
        .fetch_one(self.pool())
        .await
        .map_err(name_conflict)?;
        row.try_into()
    }

    /// All instances, by name.
    pub async fn list_services(&self) -> Result<Vec<ServiceInstance>, DbError> {
        let rows: Vec<Row> = sqlx::query_as(concat!(
            "SELECT ",
            columns!(),
            " FROM service_instance ORDER BY name COLLATE NOCASE, id"
        ))
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(TryInto::try_into))
    }

    pub async fn get_service(&self, id: i64) -> Result<Option<ServiceInstance>, DbError> {
        let row: Option<Row> = sqlx::query_as(concat!(
            "SELECT ",
            columns!(),
            " FROM service_instance WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    /// Apply `update`. A changed URL or credential invalidates the facts of
    /// the last check (they described another connection). A changed URL
    /// drops the source lists read from the old address; the settings picked
    /// from them stay with the instance (spec 0008 ADM-01) until the lists
    /// read from the new address show they are not offered there
    /// ([`Database::store_source_lists`]). `None` if the
    /// instance does not exist.
    pub async fn update_service(
        &self,
        id: i64,
        update: &ServiceUpdate<'_>,
        now: DateTime<Utc>,
    ) -> Result<Option<ServiceInstance>, DbError> {
        let connection_changed = update.base_url.is_some() || update.credential.is_some();
        let mut tx = self.begin_write().await?;
        let row: Option<Row> = sqlx::query_as(concat!(
            "UPDATE service_instance SET
                 name = coalesce(?1, name),
                 base_url = coalesce(?2, base_url),
                 credential_sealed = coalesce(?3, credential_sealed),
                 credential_recipient = coalesce(?4, credential_recipient),
                 credential_class = coalesce(?8, credential_class),
                 detected_version = iif(?5, NULL, detected_version),
                 capabilities = iif(?5, NULL, capabilities),
                 last_check_at = iif(?5, NULL, last_check_at),
                 last_error = iif(?5, NULL, last_error),
                 public_url = iif(?9, ?10, public_url),
                 updated_at = ?6
             WHERE id = ?7
             RETURNING ",
            columns!(),
            ""
        ))
        .bind(update.name)
        .bind(update.base_url)
        .bind(update.credential.map(|c| c.ciphertext.as_slice()))
        .bind(update.credential.map(|c| c.recipient.as_str()))
        .bind(connection_changed)
        .bind(ts(now))
        .bind(id)
        .bind(update.credential.map(|c| c.class.as_str()))
        .bind(update.public_url.is_some())
        .bind(update.public_url.flatten())
        .fetch_optional(&mut *tx)
        .await
        .map_err(name_conflict)?;
        if row.is_some() && update.base_url.is_some() {
            sqlx::query("DELETE FROM source_lists WHERE service_instance_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        row.map(TryInto::try_into).transpose()
    }

    /// Pause writes to the instance (`paused_by`: the admin's username) or
    /// resume them (`None`). Pausing a paused instance keeps who paused it
    /// and since when; a call that changes nothing leaves the row alone.
    /// Returns the instance and whether it changed (decided inside the
    /// write transaction, so concurrent admins see one change), or `None`
    /// if the instance does not exist.
    pub async fn set_writes_paused(
        &self,
        id: i64,
        paused_by: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Option<(ServiceInstance, bool)>, DbError> {
        let mut tx = self.begin_write().await?;
        let row: Option<(bool,)> = sqlx::query_as(
            "SELECT writes_paused_at IS NOT NULL FROM service_instance WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((was_paused,)) = row else {
            return Ok(None);
        };
        let changed = was_paused != paused_by.is_some();
        if changed {
            sqlx::query(
                "UPDATE service_instance SET writes_paused_at = iif(?1 IS NULL, NULL, ?2),
                     writes_paused_by = ?1, updated_at = ?2
                 WHERE id = ?3",
            )
            .bind(paused_by)
            .bind(ts(now))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        let row: Row = sqlx::query_as(concat!(
            "SELECT ",
            columns!(),
            " FROM service_instance WHERE id = ?"
        ))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some((row.try_into()?, changed)))
    }

    /// Remove the instance from Starr (the service itself is untouched).
    /// Its service roles go by cascade (the oldest remaining catalog becomes
    /// the default where needed). Entitlements scoped to it go in the same
    /// transaction:
    /// `entitlement.service_instance_id` has no foreign key (the table
    /// predates `service_instance`), so this is where the reference is kept
    /// consistent.
    pub async fn delete_service(&self, id: i64, now: DateTime<Utc>) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let name: Option<(String,)> =
            sqlx::query_as("SELECT name FROM service_instance WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((name,)) = &name {
            // Acquisitions stay (without instance); open ones fail first.
            crate::requests::fail_acquisitions_at(&mut tx, id, name, now).await?;
        }
        sqlx::query("DELETE FROM entitlement WHERE service_instance_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let res = sqlx::query("DELETE FROM service_instance WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        // Its roles went by cascade; a removed default catalog hands over.
        Self::repair_default_catalogs(&mut tx).await?;
        tx.commit().await?;
        Ok(res.rows_affected() == 1)
    }

    /// Whether a service instance with this id exists. Code that stores a
    /// non-null `entitlement.service_instance_id` must check this first (no
    /// foreign key enforces it).
    pub async fn service_exists(&self, id: i64) -> Result<bool, DbError> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM service_instance WHERE id = ?")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.is_some())
    }

    /// Connection data including the sealed credential. Opening it needs
    /// the key of its class: the worker's for service credentials, the web
    /// key for catalog tokens.
    pub async fn service_connection(&self, id: i64) -> Result<Option<ServiceConnection>, DbError> {
        let row: Option<ConnectionRow> = sqlx::query_as(concat!(
            "SELECT ",
            connection_columns!(),
            " FROM service_instance WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    /// Connections of every instance of `adapter_type` whose credential is
    /// of `class` (the worker re-seals TMDB instances still sealed with its
    /// own key, ADR 0003 amendment 1).
    pub async fn service_connections_of(
        &self,
        adapter_type: &str,
        class: SecretClass,
    ) -> Result<Vec<ServiceConnection>, DbError> {
        let rows: Vec<ConnectionRow> = sqlx::query_as(concat!(
            "SELECT ",
            connection_columns!(),
            " FROM service_instance WHERE adapter_type = ? AND credential_class = ? ORDER BY id"
        ))
        .bind(adapter_type)
        .bind(class.as_str())
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(TryInto::try_into))
    }

    /// Replace the credential of `conn` with `resealed` (same secret, other
    /// key or class), keeping the facts of the last check: the connection is
    /// the same. Only if the stored credential is still the one in `conn`;
    /// `false` if it changed meanwhile (the admin entered a new key).
    pub async fn reseal_credential(
        &self,
        conn: &ServiceConnection,
        resealed: &StoredCredential,
    ) -> Result<bool, DbError> {
        let res = sqlx::query(
            "UPDATE service_instance SET
                 credential_sealed = ?, credential_recipient = ?, credential_class = ?
             WHERE id = ? AND credential_sealed = ? AND credential_class = ?",
        )
        .bind(&resealed.ciphertext)
        .bind(&resealed.recipient)
        .bind(resealed.class.as_str())
        .bind(conn.id)
        .bind(&conn.credential.ciphertext)
        .bind(conn.credential.class.as_str())
        .execute(self.pool())
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Mark one instance's credential as unreadable with error `code` (it
    /// could not be opened, e.g. while re-sealing).
    pub async fn mark_credential_unreadable(
        &self,
        id: i64,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE service_instance SET last_error = ?, last_check_at = ?, capabilities = NULL
             WHERE id = ?",
        )
        .bind(code)
        .bind(ts(now))
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Ids of all instances.
    pub async fn service_ids(&self) -> Result<Vec<i64>, DbError> {
        let rows: Vec<(i64,)> = sqlx::query_as("SELECT id FROM service_instance ORDER BY id")
            .fetch_all(self.pool())
            .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// After the key of `class` was lost or replaced: mark every service
    /// whose credential of that class was sealed for another recipient than
    /// `current` with error `code`, so the UI asks for the API key again,
    /// and clear the mark from services `current` can open again. Returns
    /// how many are marked. Credentials of the other class are untouched:
    /// they belong to the other key.
    pub async fn mark_credentials_unreadable(
        &self,
        class: SecretClass,
        current: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<u64, DbError> {
        let mut tx = self.begin_write().await?;
        // The right key is back (e.g. restored from backup): the flag no
        // longer applies; show these as "not checked yet".
        sqlx::query(
            "UPDATE service_instance SET last_error = NULL, last_check_at = NULL
             WHERE credential_recipient = ? AND last_error = ? AND credential_class = ?",
        )
        .bind(current)
        .bind(code)
        .bind(class.as_str())
        .execute(&mut *tx)
        .await?;
        let res = sqlx::query(
            "UPDATE service_instance SET last_error = ?, last_check_at = ?, capabilities = NULL
             WHERE credential_recipient <> ? AND credential_class = ?",
        )
        .bind(code)
        .bind(ts(now))
        .bind(current)
        .bind(class.as_str())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(res.rows_affected())
    }

    /// Store the result of a check of `checked` (the connection the check
    /// used). Ignored if the URL or credential changed meanwhile: the result
    /// describes a connection that no longer exists. `true` if stored.
    pub async fn record_service_check(
        &self,
        checked: &ServiceConnection,
        outcome: &CheckOutcome<'_>,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let (version, capabilities, error) = match outcome {
            CheckOutcome::Ok {
                version,
                capabilities,
            } => (
                Some(*version),
                Some(serde_json::to_string(capabilities)?),
                None,
            ),
            CheckOutcome::Failed { code, version } => (*version, None, Some(*code)),
        };
        let res = sqlx::query(
            "UPDATE service_instance SET
                 detected_version = coalesce(?, detected_version),
                 capabilities = ?,
                 last_check_at = ?,
                 last_error = ?
             WHERE id = ? AND base_url = ? AND credential_sealed = ?",
        )
        .bind(version)
        .bind(capabilities)
        .bind(ts(now))
        .bind(error)
        .bind(checked.id)
        .bind(&checked.base_url)
        .bind(&checked.credential.ciphertext)
        .execute(self.pool())
        .await?;
        Ok(res.rows_affected() == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::t0;

    fn cred(bytes: &[u8]) -> StoredCredential {
        StoredCredential {
            ciphertext: bytes.to_vec(),
            recipient: "age1test".to_owned(),
            class: SecretClass::ServiceCredential,
        }
    }

    async fn radarr(db: &Database, name: &str) -> ServiceInstance {
        db.create_service(
            &NewServiceInstance {
                adapter_type: "radarr",
                name,
                base_url: "http://radarr:7878",
                public_url: None,
                credential: &cred(b"sealed"),
            },
            t0(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn create_list_get_delete() {
        let db = Database::open_in_memory().await.unwrap();
        let created = radarr(&db, "Movies").await;
        assert_eq!(created.mode, "probe");
        assert_eq!(created.detected_version, None);
        assert_eq!(created.capabilities, Vec::<String>::new());
        assert_eq!(db.list_services().await.unwrap(), vec![created.clone()]);
        assert_eq!(
            db.get_service(created.id).await.unwrap(),
            Some(created.clone())
        );
        let conn = db.service_connection(created.id).await.unwrap().unwrap();
        assert_eq!(conn.credential, cred(b"sealed"));
        assert!(db.delete_service(created.id, t0()).await.unwrap());
        assert!(!db.delete_service(created.id, t0()).await.unwrap());
        assert_eq!(db.get_service(created.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn names_are_unique_case_insensitively() {
        let db = Database::open_in_memory().await.unwrap();
        let first = radarr(&db, "Movies").await;
        let err = db
            .create_service(
                &NewServiceInstance {
                    adapter_type: "radarr",
                    name: "movies",
                    base_url: "http://other",
                    public_url: None,
                    credential: &cred(b"x"),
                },
                t0(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::ServiceNameTaken), "{err}");
        let other = radarr(&db, "4K").await;
        let err = db
            .update_service(
                other.id,
                &ServiceUpdate {
                    name: Some("MOVIES"),
                    ..ServiceUpdate::default()
                },
                t0(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::ServiceNameTaken), "{err}");
        assert_ne!(first.id, other.id);
    }

    #[tokio::test]
    async fn check_results_are_stored_and_connection_changes_reset_them() {
        let db = Database::open_in_memory().await.unwrap();
        let s = radarr(&db, "Movies").await;
        let conn = db.service_connection(s.id).await.unwrap().unwrap();
        let later = t0() + chrono::TimeDelta::minutes(1);
        let stored = db
            .record_service_check(
                &conn,
                &CheckOutcome::Ok {
                    version: "6.4.4.10685",
                    capabilities: &["requests"],
                },
                later,
            )
            .await
            .unwrap();
        assert!(stored);
        let s = db.get_service(s.id).await.unwrap().unwrap();
        assert_eq!(s.detected_version.as_deref(), Some("6.4.4.10685"));
        assert_eq!(s.capabilities, vec!["requests"]);
        assert_eq!(s.last_check_at, Some(later));
        assert_eq!(s.last_error, None);

        // A failure keeps the last known version unless a new one is reported.
        db.record_service_check(
            &conn,
            &CheckOutcome::Failed {
                code: "credentials_invalid",
                version: None,
            },
            later,
        )
        .await
        .unwrap();
        let s = db.get_service(s.id).await.unwrap().unwrap();
        assert_eq!(s.last_error.as_deref(), Some("credentials_invalid"));
        assert_eq!(s.detected_version.as_deref(), Some("6.4.4.10685"));
        assert_eq!(s.capabilities, Vec::<String>::new());

        // Rename keeps the facts; a new credential resets them.
        let renamed = db
            .update_service(
                s.id,
                &ServiceUpdate {
                    name: Some("Films"),
                    ..ServiceUpdate::default()
                },
                later,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(renamed.name, "Films");
        assert_eq!(renamed.last_error.as_deref(), Some("credentials_invalid"));
        let rekeyed = db
            .update_service(
                s.id,
                &ServiceUpdate {
                    credential: Some(&cred(b"new")),
                    ..ServiceUpdate::default()
                },
                later,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rekeyed.last_error, None);
        assert_eq!(rekeyed.detected_version, None);
        assert_eq!(rekeyed.last_check_at, None);

        // A check that ran against the old credential is discarded.
        let stale = db
            .record_service_check(
                &conn,
                &CheckOutcome::Ok {
                    version: "6.4.4.10685",
                    capabilities: &[],
                },
                later,
            )
            .await
            .unwrap();
        assert!(!stale);
        assert_eq!(
            db.update_service(999, &ServiceUpdate::default(), later)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn deleting_an_instance_removes_entitlements_scoped_to_it() {
        let db = Database::open_in_memory().await.unwrap();
        let movies = radarr(&db, "Movies").await;
        let other = radarr(&db, "4K").await;
        assert!(db.service_exists(movies.id).await.unwrap());
        sqlx::query(
            "INSERT INTO member (username, display_name, created_at) VALUES ('mia', 'Mia', ?)",
        )
        .bind(ts(t0()))
        .execute(db.pool())
        .await
        .unwrap();
        for instance in [None, Some(movies.id), Some(other.id)] {
            sqlx::query(
                "INSERT INTO entitlement (member_id, category, service_instance_id, enabled, updated_at)
                 VALUES (1, 'movies', ?, 1, ?)",
            )
            .bind(instance)
            .bind(ts(t0()))
            .execute(db.pool())
            .await
            .unwrap();
        }

        assert!(db.delete_service(movies.id, t0()).await.unwrap());

        let left: Vec<(Option<i64>,)> = sqlx::query_as(
            "SELECT service_instance_id FROM entitlement ORDER BY service_instance_id",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        assert_eq!(left, vec![(None,), (Some(other.id),)]);
        assert!(!db.service_exists(movies.id).await.unwrap());
    }

    #[tokio::test]
    async fn unreadable_marks_and_reseals_stay_within_their_class() {
        let db = Database::open_in_memory().await.unwrap();
        let radarr = radarr(&db, "Movies").await;
        let tmdb = db
            .create_service(
                &NewServiceInstance {
                    adapter_type: "tmdb",
                    name: "TMDB",
                    base_url: "https://api.themoviedb.org",
                    public_url: None,
                    credential: &cred(b"tmdb-sealed"),
                },
                t0(),
            )
            .await
            .unwrap();
        // Re-seal TMDB to the web key: class and recipient change together.
        let conn = db.service_connection(tmdb.id).await.unwrap().unwrap();
        let web = StoredCredential {
            ciphertext: b"tmdb-web".to_vec(),
            recipient: "age1web".to_owned(),
            class: SecretClass::CatalogReadToken,
        };
        assert!(db.reseal_credential(&conn, &web).await.unwrap());
        // The stale connection no longer matches: nothing is overwritten.
        assert!(!db.reseal_credential(&conn, &web).await.unwrap());
        assert_eq!(
            db.service_connection(tmdb.id)
                .await
                .unwrap()
                .unwrap()
                .credential,
            web
        );
        assert_eq!(
            db.service_connections_of("tmdb", SecretClass::ServiceCredential)
                .await
                .unwrap(),
            vec![]
        );

        // A new worker key marks service credentials only, a new web key
        // catalog tokens only.
        let marked = db
            .mark_credentials_unreadable(SecretClass::ServiceCredential, "age1new", "x", t0())
            .await
            .unwrap();
        assert_eq!(marked, 1);
        let get = |id| {
            let db = db.clone();
            async move { db.get_service(id).await.unwrap().unwrap().last_error }
        };
        assert_eq!(get(radarr.id).await.as_deref(), Some("x"));
        assert_eq!(get(tmdb.id).await, None);
        let marked = db
            .mark_credentials_unreadable(SecretClass::CatalogReadToken, "age1web", "x", t0())
            .await
            .unwrap();
        assert_eq!(marked, 0);
        db.mark_credentials_unreadable(SecretClass::CatalogReadToken, "age1other", "x", t0())
            .await
            .unwrap();
        assert_eq!(get(tmdb.id).await.as_deref(), Some("x"));
    }

    #[tokio::test]
    async fn public_urls_are_set_kept_and_cleared_without_resetting_checks() {
        let db = Database::open_in_memory().await.unwrap();
        let s = radarr(&db, "Watch").await;
        assert_eq!(s.public_url, None);
        let conn = db.service_connection(s.id).await.unwrap().unwrap();
        db.record_service_check(
            &conn,
            &CheckOutcome::Ok {
                version: "12.3.0",
                capabilities: &["libraries"],
            },
            t0(),
        )
        .await
        .unwrap();
        let set = |url| ServiceUpdate {
            public_url: Some(url),
            ..ServiceUpdate::default()
        };
        let s = db
            .update_service(s.id, &set(Some("https://watch.example.com")), t0())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.public_url.as_deref(), Some("https://watch.example.com"));
        assert_eq!(s.detected_version.as_deref(), Some("12.3.0"), "check kept");
        let s = db
            .update_service(
                s.id,
                &ServiceUpdate {
                    name: Some("Jellyfin"),
                    ..ServiceUpdate::default()
                },
                t0(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.public_url.as_deref(), Some("https://watch.example.com"));
        let s = db
            .update_service(s.id, &set(None), t0())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.public_url, None);
    }

    #[tokio::test]
    async fn writes_are_paused_by_an_admin_and_resumed() {
        let db = Database::open_in_memory().await.unwrap();
        let s = radarr(&db, "Movies").await;
        assert_eq!((s.writes_paused_at, s.writes_paused_by), (None, None));
        let conn = db.service_connection(s.id).await.unwrap().unwrap();
        assert!(!conn.writes_paused);

        let (paused, changed) = db
            .set_writes_paused(s.id, Some("root"), t0())
            .await
            .unwrap()
            .unwrap();
        assert!(changed);
        assert_eq!(paused.writes_paused_at, Some(t0()));
        assert_eq!(paused.writes_paused_by.as_deref(), Some("root"));
        let conn = db.service_connection(s.id).await.unwrap().unwrap();
        assert!(conn.writes_paused);

        // Pausing again keeps who paused it and since when.
        let later = t0() + chrono::TimeDelta::minutes(5);
        let (again, changed) = db
            .set_writes_paused(s.id, Some("other"), later)
            .await
            .unwrap()
            .unwrap();
        assert!(!changed);
        assert_eq!(
            again.updated_at, paused.updated_at,
            "a no-op leaves the row"
        );
        assert_eq!(again.writes_paused_at, Some(t0()));
        assert_eq!(again.writes_paused_by.as_deref(), Some("root"));

        let (resumed, changed) = db
            .set_writes_paused(s.id, None, later)
            .await
            .unwrap()
            .unwrap();
        assert!(changed);
        assert_eq!(
            (resumed.writes_paused_at, resumed.writes_paused_by),
            (None, None)
        );
        let conn = db.service_connection(s.id).await.unwrap().unwrap();
        assert!(!conn.writes_paused);
        assert_eq!(db.set_writes_paused(999, None, later).await.unwrap(), None);
    }

    #[tokio::test]
    async fn service_ids_are_listed() {
        let db = Database::open_in_memory().await.unwrap();
        radarr(&db, "A").await;
        radarr(&db, "B").await;
        assert_eq!(db.service_ids().await.unwrap().len(), 2);
    }
}
