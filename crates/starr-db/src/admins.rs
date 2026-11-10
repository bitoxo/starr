//! Admin accounts, their second factor (TOTP secret, recovery codes, pending
//! logins) and the linked member (spec 0002 slice 6, domain model §1).
//!
//! Account rules: an admin never suspends or removes themselves, the actor
//! must be active, and so one active admin always remains. The statements
//! below check this in the same write as the change; the
//! `admin_account_keep_one_active` trigger enforces the last part for every
//! writer. Removal leaves a tombstone: the row (and username) stays.

use chrono::{DateTime, Utc};
use sqlx::{Sqlite, Transaction};
use starr_core::auth::AdminChangeError;
use starr_core::members::PasswordTokenPurpose;

use crate::auth::{NewAdmin, username_conflict};
use crate::members::TokenUse;
use crate::{Database, DbError, parse_ts, ts};

/// The member profile an admin is linked to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedMember {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    /// Only an active member can be switched into.
    pub active: bool,
}

/// An admin account as admins see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminRecord {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub suspended: bool,
    /// Tombstone: no login, username reserved.
    pub removed: bool,
    pub totp_enabled: bool,
    pub linked_member: Option<LinkedMember>,
    /// Username of the admin who created this one (`None` for the first).
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A status change of one admin by another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminStatusChange {
    Suspend,
    Reactivate,
    /// Tombstone: sessions, second factor and member link are dropped.
    Remove,
}

/// Outcome of [`Database::change_admin_status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminChange {
    Done,
    /// Unknown or removed target.
    NotFound,
    /// The target is not in a state the change applies to (e.g. reactivating
    /// an active admin).
    InvalidState,
    Refused(AdminChangeError),
}

/// Sealed TOTP secrets of an admin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TotpSecrets {
    /// Confirmed secret: TOTP is on.
    pub enabled: Option<Vec<u8>>,
    /// Enrollment waiting for its first code.
    pub pending: Option<Vec<u8>>,
}

/// A pending login taken out for its second step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingLogin {
    pub admin_id: i64,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Request of `starr admin recover`.
#[derive(Debug)]
pub struct RecoverAdmin<'a> {
    pub username: &'a str,
    /// Turn TOTP off and drop the recovery codes. Also replaces the password
    /// by `unusable_password_hash` and ends every session, so only the new
    /// link restores access (the old password alone must not suffice).
    pub reset_totp: bool,
    /// Hash of a random, discarded password (used with `reset_totp`).
    pub unusable_password_hash: &'a str,
    /// Create the admin if no account has this username: display name and
    /// an unusable password hash until the set-password link is used.
    pub create: Option<NewAdmin<'a>>,
    /// Hash of the new set-password token and its expiry.
    pub token_hash: &'a [u8],
    pub token_expires_at: DateTime<Utc>,
}

/// Outcome of [`Database::recover_admin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Reactivated (and TOTP reset if asked); a set-password token exists.
    Recovered { admin_id: i64 },
    /// Created; a set-password token exists.
    Created { admin_id: i64 },
    /// No admin with this username (and no `create`).
    NotFound,
    /// The username belongs to a removed admin and stays reserved.
    Tombstone,
}

type AdminRow = (
    i64,
    String,
    String,
    bool,
    bool,
    bool,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<bool>,
    Option<String>,
    String,
);

/// Confirmed and pending sealed TOTP secret.
type SealedPair = (Option<Vec<u8>>, Option<Vec<u8>>);

const ADMIN_SELECT: &str = "
    SELECT a.id, a.username, a.display_name, a.suspended_at IS NOT NULL,
           a.removed_at IS NOT NULL, a.totp_secret_sealed IS NOT NULL,
           m.id, m.username, m.display_name, m.status = 'active',
           c.username, a.created_at
    FROM admin_account a
    LEFT JOIN member m ON m.id = a.linked_member_id
    LEFT JOIN admin_account c ON c.id = a.created_by";

fn admin_record(row: AdminRow) -> Result<AdminRecord, DbError> {
    let (
        id,
        username,
        display_name,
        suspended,
        removed,
        totp,
        m_id,
        m_name,
        m_display,
        m_active,
        by,
        at,
    ) = row;
    let linked_member = match (m_id, m_name, m_display) {
        (Some(id), Some(username), Some(display_name)) => Some(LinkedMember {
            id,
            username,
            display_name,
            active: m_active.unwrap_or(false),
        }),
        _ => None,
    };
    Ok(AdminRecord {
        id,
        username,
        display_name,
        suspended,
        removed,
        totp_enabled: totp,
        linked_member,
        created_by: by,
        created_at: parse_ts(&at)?,
    })
}

/// Map the keep-one-active trigger to its rule.
fn last_active_guard(err: sqlx::Error) -> DbError {
    if let sqlx::Error::Database(db) = &err
        && db.message().contains("last_active_admin")
    {
        return DbError::LastActiveAdmin;
    }
    DbError::Sqlx(err)
}

/// End everything an admin's login left behind: own sessions, member-view
/// sessions, pending logins and password links.
async fn end_admin_access(tx: &mut Transaction<'_, Sqlite>, admin_id: i64) -> Result<(), DbError> {
    sqlx::query(
        "DELETE FROM session
         WHERE (subject_kind = 'admin' AND subject_id = ?) OR origin_admin_id = ?",
    )
    .bind(admin_id)
    .bind(admin_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM pending_login WHERE admin_id = ?")
        .bind(admin_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM password_token WHERE subject_kind = 'admin' AND subject_id = ?")
        .bind(admin_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn replace_recovery_codes(
    tx: &mut Transaction<'_, Sqlite>,
    admin_id: i64,
    hashes: &[[u8; 32]],
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    sqlx::query("DELETE FROM admin_recovery_code WHERE admin_id = ?")
        .bind(admin_id)
        .execute(&mut **tx)
        .await?;
    for hash in hashes {
        sqlx::query(
            "INSERT INTO admin_recovery_code (admin_id, code_hash, created_at) VALUES (?, ?, ?)",
        )
        .bind(admin_id)
        .bind(&hash[..])
        .bind(ts(now))
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn clear_totp_in(tx: &mut Transaction<'_, Sqlite>, admin_id: i64) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE admin_account
         SET totp_secret_sealed = NULL, totp_pending_sealed = NULL, totp_last_step = NULL
         WHERE id = ?",
    )
    .bind(admin_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM admin_recovery_code WHERE admin_id = ?")
        .bind(admin_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM pending_login WHERE admin_id = ?")
        .bind(admin_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Replace the admin's unused password links by one new token.
async fn insert_admin_password_token(
    tx: &mut Transaction<'_, Sqlite>,
    admin_id: i64,
    token_hash: &[u8],
    purpose: PasswordTokenPurpose,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    sqlx::query(
        "DELETE FROM password_token
         WHERE subject_kind = 'admin' AND subject_id = ? AND used_at IS NULL",
    )
    .bind(admin_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO password_token
            (token_hash, subject_kind, subject_id, purpose, created_at, expires_at)
         VALUES (?, 'admin', ?, ?, ?, ?)",
    )
    .bind(token_hash)
    .bind(admin_id)
    .bind(purpose.as_str())
    .bind(ts(now))
    .bind(ts(expires_at))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// SQL condition: the actor (parameter `?3`) is an active admin.
const ACTOR_ACTIVE: &str = "EXISTS (SELECT 1 FROM admin_account
    WHERE id = ?3 AND suspended_at IS NULL AND removed_at IS NULL)";

impl Database {
    /// Admin accounts without tombstones, oldest first.
    pub async fn list_admins(&self) -> Result<Vec<AdminRecord>, DbError> {
        let sql = format!("{ADMIN_SELECT} WHERE a.removed_at IS NULL ORDER BY a.id");
        let rows: Vec<AdminRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(admin_record).collect()
    }

    /// One admin account, tombstones included.
    pub async fn get_admin(&self, id: i64) -> Result<Option<AdminRecord>, DbError> {
        let row: Option<AdminRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{ADMIN_SELECT} WHERE a.id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(admin_record).transpose()
    }

    /// Create an admin account (created by `created_by`) with a set-password
    /// token, all or nothing. `admin.password_hash` must be unusable (the
    /// hash of a random, discarded password) until the link is used.
    pub async fn create_admin(
        &self,
        admin: &NewAdmin<'_>,
        created_by: i64,
        token_hash: &[u8],
        token_expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<i64, DbError> {
        let mut tx = self.begin_write().await?;
        let id = sqlx::query(
            "INSERT INTO admin_account (username, display_name, password_hash, created_by, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(admin.username)
        .bind(admin.display_name)
        .bind(admin.password_hash)
        .bind(created_by)
        .bind(ts(now))
        .execute(&mut *tx)
        .await
        .map_err(username_conflict)?
        .last_insert_rowid();
        insert_admin_password_token(
            &mut tx,
            id,
            token_hash,
            PasswordTokenPurpose::Set,
            token_expires_at,
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Suspend, reactivate or remove `target` on behalf of `actor`. The
    /// rules (not yourself, active actor, one active admin remains) are
    /// checked in the same statement as the change, so concurrent changes
    /// cannot together break them.
    pub async fn change_admin_status(
        &self,
        actor: i64,
        target: i64,
        change: AdminStatusChange,
        now: DateTime<Utc>,
    ) -> Result<AdminChange, DbError> {
        if actor == target && change != AdminStatusChange::Reactivate {
            return Ok(AdminChange::Refused(AdminChangeError::SelfTarget));
        }
        // ?1 = now, ?2 = target, ?3 = actor.
        let sql = match change {
            AdminStatusChange::Suspend => format!(
                "UPDATE admin_account SET suspended_at = ?1
                 WHERE id = ?2 AND id <> ?3 AND suspended_at IS NULL AND removed_at IS NULL
                   AND {ACTOR_ACTIVE}"
            ),
            AdminStatusChange::Reactivate => format!(
                "UPDATE admin_account SET suspended_at = NULL
                 WHERE id = ?2 AND id <> ?3 AND suspended_at IS NOT NULL AND removed_at IS NULL
                   AND {ACTOR_ACTIVE}"
            ),
            AdminStatusChange::Remove => format!(
                "UPDATE admin_account
                 SET removed_at = ?1, linked_member_id = NULL, totp_secret_sealed = NULL,
                     totp_pending_sealed = NULL, totp_last_step = NULL
                 WHERE id = ?2 AND id <> ?3 AND removed_at IS NULL AND {ACTOR_ACTIVE}"
            ),
        };
        let mut tx = self.begin_write().await?;
        let changed = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(ts(now))
            .bind(target)
            .bind(actor)
            .execute(&mut *tx)
            .await
            .map_err(last_active_guard);
        let changed = match changed {
            Ok(result) => result.rows_affected() == 1,
            Err(DbError::LastActiveAdmin) => {
                tx.rollback().await?;
                return Ok(AdminChange::Refused(AdminChangeError::LastActiveAdmin));
            }
            Err(err) => return Err(err),
        };
        if !changed {
            tx.rollback().await?;
            return self.why_unchanged(actor, target).await;
        }
        if change != AdminStatusChange::Reactivate {
            end_admin_access(&mut tx, target).await?;
        }
        if change == AdminStatusChange::Remove {
            sqlx::query("DELETE FROM admin_recovery_code WHERE admin_id = ?")
                .bind(target)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(AdminChange::Done)
    }

    /// Explain a status change that matched no row.
    async fn why_unchanged(&self, actor: i64, target: i64) -> Result<AdminChange, DbError> {
        let actor_active = self
            .get_admin(actor)
            .await?
            .is_some_and(|a| !a.suspended && !a.removed);
        if !actor_active {
            return Ok(AdminChange::Refused(AdminChangeError::ActorInactive));
        }
        Ok(match self.get_admin(target).await? {
            None => AdminChange::NotFound,
            Some(a) if a.removed => AdminChange::NotFound,
            Some(_) => AdminChange::InvalidState,
        })
    }

    /// Password hash of an active admin.
    pub async fn admin_password_hash(&self, id: i64) -> Result<Option<String>, DbError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT password_hash FROM admin_account
             WHERE id = ? AND suspended_at IS NULL AND removed_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(hash,)| hash))
    }

    /// Store a new password for the admin: ends every other session of the
    /// admin (the one with token hash `keep` stays) and voids unused
    /// password links, all in one transaction.
    pub async fn change_admin_password(
        &self,
        id: i64,
        password_hash: &str,
        keep: &[u8],
    ) -> Result<(), DbError> {
        let mut tx = self.begin_write().await?;
        sqlx::query("UPDATE admin_account SET password_hash = ? WHERE id = ?")
            .bind(password_hash)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        // Own sessions and member-view sessions started from them, like every
        // other place that ends an admin's access.
        sqlx::query(
            "DELETE FROM session
             WHERE ((subject_kind = 'admin' AND subject_id = ?) OR origin_admin_id = ?)
               AND token_hash <> ?",
        )
        .bind(id)
        .bind(id)
        .bind(keep)
        .execute(&mut *tx)
        .await?;
        // A login begun with the old password must not finish.
        sqlx::query("DELETE FROM pending_login WHERE admin_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "DELETE FROM password_token
             WHERE subject_kind = 'admin' AND subject_id = ? AND used_at IS NULL",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Link `member_id` to the admin (or unlink with `None`). A member is
    /// linked to at most one admin.
    pub async fn set_linked_member(
        &self,
        admin_id: i64,
        member_id: Option<i64>,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE admin_account SET linked_member_id = ? WHERE id = ?")
            .bind(member_id)
            .bind(admin_id)
            .execute(&self.pool)
            .await
            .map_err(|err| match &err {
                sqlx::Error::Database(db) if db.is_unique_violation() => {
                    DbError::MemberAlreadyLinked
                }
                _ => DbError::Sqlx(err),
            })?;
        Ok(())
    }

    // ------------------------------------------------------------ TOTP

    /// The admin's sealed TOTP secrets.
    pub async fn totp_secrets(&self, admin_id: i64) -> Result<TotpSecrets, DbError> {
        let row: Option<SealedPair> = sqlx::query_as(
            "SELECT totp_secret_sealed, totp_pending_sealed FROM admin_account WHERE id = ?",
        )
        .bind(admin_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(
            row.map_or_else(TotpSecrets::default, |(enabled, pending)| TotpSecrets {
                enabled,
                pending,
            }),
        )
    }

    /// Whether any admin has a sealed TOTP secret (confirmed or pending).
    pub async fn any_totp_secret(&self) -> Result<bool, DbError> {
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM admin_account
                WHERE totp_secret_sealed IS NOT NULL OR totp_pending_sealed IS NOT NULL)",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    /// Start (or restart) TOTP enrollment. False if TOTP is already on.
    pub async fn set_totp_pending(&self, admin_id: i64, sealed: &[u8]) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE admin_account SET totp_pending_sealed = ?
             WHERE id = ? AND totp_secret_sealed IS NULL AND removed_at IS NULL",
        )
        .bind(sealed)
        .bind(admin_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Turn TOTP on with the pending secret that was verified (`pending`
    /// must still be the stored one), remember the step of the confirming
    /// code and store the recovery code hashes. False if the enrollment
    /// changed meanwhile or TOTP is already on.
    pub async fn enable_totp(
        &self,
        admin_id: i64,
        pending: &[u8],
        step: i64,
        recovery_hashes: &[[u8; 32]],
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let enabled = sqlx::query(
            "UPDATE admin_account
             SET totp_secret_sealed = totp_pending_sealed, totp_pending_sealed = NULL,
                 totp_last_step = ?
             WHERE id = ? AND totp_pending_sealed = ? AND totp_secret_sealed IS NULL",
        )
        .bind(step)
        .bind(admin_id)
        .bind(pending)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !enabled {
            tx.rollback().await?;
            return Ok(false);
        }
        replace_recovery_codes(&mut tx, admin_id, recovery_hashes, now).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Turn TOTP off: secret, enrollment, recovery codes and pending logins go.
    pub async fn disable_totp(&self, admin_id: i64) -> Result<(), DbError> {
        let mut tx = self.begin_write().await?;
        clear_totp_in(&mut tx, admin_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Accept a TOTP code of time step `step` once: false if this step (or a
    /// later one) was already used, i.e. the code is a replay.
    pub async fn accept_totp_step(&self, admin_id: i64, step: i64) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE admin_account SET totp_last_step = ?
             WHERE id = ? AND totp_secret_sealed IS NOT NULL
               AND (totp_last_step IS NULL OR totp_last_step < ?)",
        )
        .bind(step)
        .bind(admin_id)
        .bind(step)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Use a recovery code (by hash). False if unknown or already used.
    pub async fn use_recovery_code(
        &self,
        admin_id: i64,
        code_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE admin_recovery_code SET used_at = ?
             WHERE admin_id = ? AND code_hash = ? AND used_at IS NULL",
        )
        .bind(ts(now))
        .bind(admin_id)
        .bind(code_hash)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Unused recovery codes of the admin.
    pub async fn recovery_codes_remaining(&self, admin_id: i64) -> Result<i64, DbError> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM admin_recovery_code WHERE admin_id = ? AND used_at IS NULL",
        )
        .bind(admin_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    // ------------------------------------------------------------ pending logins

    /// Store a pending login (password step passed, TOTP step open).
    pub async fn create_pending_login(
        &self,
        token_hash: &[u8],
        admin_id: i64,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO pending_login (token_hash, admin_id, created_at, expires_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(token_hash)
        .bind(admin_id)
        .bind(ts(now))
        .bind(ts(expires_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Claim a live pending login for its second step: removed at once, so
    /// a second request with the same token (double submit, replay) finds
    /// nothing and no factor is checked or used up for it. Only pending
    /// logins of active admins with TOTP on are claimed. Hand it back with
    /// [`Database::release_pending_login`] when the code was wrong.
    pub async fn claim_pending_login(
        &self,
        token_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<PendingLogin>, DbError> {
        let row: Option<(i64, String, String)> = sqlx::query_as(
            "DELETE FROM pending_login
             WHERE token_hash = ? AND expires_at > ?
               AND admin_id IN (SELECT id FROM admin_account
                   WHERE suspended_at IS NULL AND removed_at IS NULL
                     AND totp_secret_sealed IS NOT NULL)
             RETURNING admin_id, created_at, expires_at",
        )
        .bind(token_hash)
        .bind(ts(now))
        .fetch_optional(&self.pool)
        .await?;
        row.map(|(admin_id, created, expires)| {
            Ok(PendingLogin {
                admin_id,
                created_at: parse_ts(&created)?,
                expires_at: parse_ts(&expires)?,
            })
        })
        .transpose()
    }

    /// Put a claimed pending login back (wrong code: the admin may retry
    /// until it expires; attempts stay throttled).
    pub async fn release_pending_login(
        &self,
        token_hash: &[u8],
        pending: &PendingLogin,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT OR IGNORE INTO pending_login (token_hash, admin_id, created_at, expires_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(token_hash)
        .bind(pending.admin_id)
        .bind(ts(pending.created_at))
        .bind(ts(pending.expires_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// End every session of the admin (own and member view) except `keep`.
    pub async fn delete_other_admin_sessions(
        &self,
        admin_id: i64,
        keep: &[u8],
    ) -> Result<u64, DbError> {
        let result = sqlx::query(
            "DELETE FROM session
             WHERE ((subject_kind = 'admin' AND subject_id = ?) OR origin_admin_id = ?)
               AND token_hash <> ?",
        )
        .bind(admin_id)
        .bind(admin_id)
        .bind(keep)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Replace all recovery codes of an admin with TOTP on. False if TOTP
    /// is off.
    pub async fn regenerate_recovery_codes(
        &self,
        admin_id: i64,
        hashes: &[[u8; 32]],
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        // Write first: takes the write lock before the TOTP check.
        sqlx::query("DELETE FROM admin_recovery_code WHERE admin_id = ?")
            .bind(admin_id)
            .execute(&mut *tx)
            .await?;
        let (enabled,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM admin_account
                WHERE id = ? AND totp_secret_sealed IS NOT NULL)",
        )
        .bind(admin_id)
        .fetch_one(&mut *tx)
        .await?;
        if !enabled {
            tx.rollback().await?;
            return Ok(false);
        }
        replace_recovery_codes(&mut tx, admin_id, hashes, now).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Drop expired pending logins.
    pub async fn delete_expired_pending_logins(&self, now: DateTime<Utc>) -> Result<u64, DbError> {
        let result = sqlx::query("DELETE FROM pending_login WHERE expires_at <= ?")
            .bind(ts(now))
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    // ------------------------------------------------------------ password links

    /// Use an admin's password token: mark it used, store the password and
    /// end the admin's sessions, all or nothing. Rejected when the token is
    /// used or expired at `now` or the admin is not active.
    pub async fn use_admin_password_token(
        &self,
        token_id: i64,
        admin_id: i64,
        password_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<TokenUse, DbError> {
        let mut tx = self.begin_write().await?;
        let claimed = sqlx::query(
            "UPDATE password_token SET used_at = ?
             WHERE id = ? AND subject_kind = 'admin' AND subject_id = ?
               AND used_at IS NULL AND expires_at > ?",
        )
        .bind(ts(now))
        .bind(token_id)
        .bind(admin_id)
        .bind(ts(now))
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        let stored = claimed
            && sqlx::query(
                "UPDATE admin_account SET password_hash = ?
                 WHERE id = ? AND suspended_at IS NULL AND removed_at IS NULL",
            )
            .bind(password_hash)
            .bind(admin_id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
                == 1;
        if !stored {
            tx.rollback().await?;
            return Ok(TokenUse::Rejected);
        }
        sqlx::query(
            "DELETE FROM session
             WHERE (subject_kind = 'admin' AND subject_id = ?) OR origin_admin_id = ?",
        )
        .bind(admin_id)
        .bind(admin_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM pending_login WHERE admin_id = ?")
            .bind(admin_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(TokenUse::Done)
    }

    // ------------------------------------------------------------ recovery

    /// Operator recovery (`starr admin recover`): reactivate the admin,
    /// optionally reset TOTP, and store a new set-password token, all or
    /// nothing. A tombstone is never revived; `create` makes a new admin
    /// only when no account has the username.
    pub async fn recover_admin(
        &self,
        request: &RecoverAdmin<'_>,
        now: DateTime<Utc>,
    ) -> Result<Recovery, DbError> {
        let mut tx = self.begin_write().await?;
        // Write first so the lookups below run under the write lock.
        let reactivated: Option<(i64,)> = sqlx::query_as(
            "UPDATE admin_account SET suspended_at = NULL
             WHERE username = ? AND removed_at IS NULL RETURNING id",
        )
        .bind(request.username)
        .fetch_optional(&mut *tx)
        .await?;
        let outcome = if let Some((admin_id,)) = reactivated {
            if request.reset_totp {
                clear_totp_in(&mut tx, admin_id).await?;
                sqlx::query("UPDATE admin_account SET password_hash = ? WHERE id = ?")
                    .bind(request.unusable_password_hash)
                    .bind(admin_id)
                    .execute(&mut *tx)
                    .await?;
                end_admin_access(&mut tx, admin_id).await?;
            }
            Recovery::Recovered { admin_id }
        } else {
            let (tombstone,): (bool,) =
                sqlx::query_as("SELECT EXISTS (SELECT 1 FROM admin_account WHERE username = ?)")
                    .bind(request.username)
                    .fetch_one(&mut *tx)
                    .await?;
            match &request.create {
                _ if tombstone => Recovery::Tombstone,
                None => Recovery::NotFound,
                Some(admin) => {
                    let admin_id = sqlx::query(
                        "INSERT INTO admin_account (username, display_name, password_hash, created_at)
                         VALUES (?, ?, ?, ?)",
                    )
                    .bind(admin.username)
                    .bind(admin.display_name)
                    .bind(admin.password_hash)
                    .bind(ts(now))
                    .execute(&mut *tx)
                    .await
                    .map_err(username_conflict)?
                    .last_insert_rowid();
                    Recovery::Created { admin_id }
                }
            }
        };
        let (admin_id, purpose) = match outcome {
            Recovery::Recovered { admin_id } => (admin_id, PasswordTokenPurpose::Reset),
            Recovery::Created { admin_id } => (admin_id, PasswordTokenPurpose::Set),
            Recovery::NotFound | Recovery::Tombstone => {
                tx.rollback().await?;
                return Ok(outcome);
            }
        };
        insert_admin_password_token(
            &mut tx,
            admin_id,
            request.token_hash,
            purpose,
            request.token_expires_at,
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;
    use crate::test_support::t0;

    const HASH: &str = "$argon2id$dummy";

    fn admin(username: &str) -> NewAdmin<'_> {
        NewAdmin {
            username,
            display_name: username,
            password_hash: HASH,
        }
    }

    async fn two_admins(db: &Database) -> (i64, i64) {
        let root = db
            .create_first_admin(&admin("root"), t0())
            .await
            .unwrap()
            .unwrap();
        let ada = db
            .create_admin(
                &admin("ada"),
                root,
                &[1; 32],
                t0() + TimeDelta::days(7),
                t0(),
            )
            .await
            .unwrap();
        (root, ada)
    }

    async fn active_admins(db: &Database) -> i64 {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM admin_account WHERE suspended_at IS NULL AND removed_at IS NULL",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        n
    }

    #[tokio::test]
    async fn admins_are_listed_without_tombstones() {
        let db = Database::open_in_memory().await.unwrap();
        let (root, ada) = two_admins(&db).await;
        let list = db.list_admins().await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].created_by.as_deref(), Some("root"));
        assert_eq!(
            db.change_admin_status(root, ada, AdminStatusChange::Remove, t0())
                .await
                .unwrap(),
            AdminChange::Done
        );
        assert_eq!(db.list_admins().await.unwrap().len(), 1);
        assert!(db.get_admin(ada).await.unwrap().unwrap().removed);
    }

    #[tokio::test]
    async fn nobody_suspends_or_removes_themselves() {
        let db = Database::open_in_memory().await.unwrap();
        let (root, _) = two_admins(&db).await;
        for change in [AdminStatusChange::Suspend, AdminStatusChange::Remove] {
            assert_eq!(
                db.change_admin_status(root, root, change, t0())
                    .await
                    .unwrap(),
                AdminChange::Refused(AdminChangeError::SelfTarget)
            );
        }
    }

    #[tokio::test]
    async fn a_suspended_actor_cannot_change_anyone() {
        let db = Database::open_in_memory().await.unwrap();
        let (root, ada) = two_admins(&db).await;
        assert_eq!(
            db.change_admin_status(root, ada, AdminStatusChange::Suspend, t0())
                .await
                .unwrap(),
            AdminChange::Done
        );
        for change in [
            AdminStatusChange::Suspend,
            AdminStatusChange::Remove,
            AdminStatusChange::Reactivate,
        ] {
            assert_eq!(
                db.change_admin_status(ada, root, change, t0())
                    .await
                    .unwrap(),
                AdminChange::Refused(AdminChangeError::ActorInactive),
                "{change:?}"
            );
        }
        assert_eq!(active_admins(&db).await, 1);
        // Reactivating an active admin is not a change.
        assert_eq!(
            db.change_admin_status(root, ada, AdminStatusChange::Reactivate, t0())
                .await
                .unwrap(),
            AdminChange::Done
        );
        assert_eq!(
            db.change_admin_status(root, ada, AdminStatusChange::Reactivate, t0())
                .await
                .unwrap(),
            AdminChange::InvalidState
        );
    }

    #[tokio::test]
    async fn the_database_refuses_to_lose_the_last_active_admin() {
        let db = Database::open_in_memory().await.unwrap();
        let (root, ada) = two_admins(&db).await;
        sqlx::query("UPDATE admin_account SET suspended_at = 'x' WHERE id = ?")
            .bind(ada)
            .execute(db.pool())
            .await
            .unwrap();
        for sql in [
            "UPDATE admin_account SET suspended_at = 'x' WHERE id = ?",
            "UPDATE admin_account SET removed_at = 'x' WHERE id = ?",
            "DELETE FROM admin_account WHERE id = ?",
        ] {
            let err = sqlx::query(sql)
                .bind(root)
                .execute(db.pool())
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("last_active_admin"),
                "{sql}: {err}"
            );
        }
    }

    #[tokio::test]
    async fn suspension_ends_sessions_including_member_view() {
        let db = Database::open_in_memory().await.unwrap();
        let (root, ada) = two_admins(&db).await;
        sqlx::query(
            "INSERT INTO session (token_hash, subject_kind, subject_id, created_at, last_seen_at, expires_at, origin_admin_id)
             VALUES (X'01', 'admin', ?1, 't', 't', 't', NULL), (X'02', 'member', 99, 't', 't', 't', ?1),
                    (X'03', 'admin', ?2, 't', 't', 't', NULL)",
        )
        .bind(ada)
        .bind(root)
        .execute(db.pool())
        .await
        .unwrap();
        db.change_admin_status(root, ada, AdminStatusChange::Suspend, t0())
            .await
            .unwrap();
        let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(left, 1, "only root's own session stays");
    }

    #[tokio::test]
    async fn totp_steps_are_accepted_once() {
        let db = Database::open_in_memory().await.unwrap();
        let (root, _) = two_admins(&db).await;
        assert!(!db.accept_totp_step(root, 10).await.unwrap(), "TOTP off");
        assert!(db.set_totp_pending(root, b"sealed").await.unwrap());
        assert!(
            !db.enable_totp(root, b"other", 10, &[], t0()).await.unwrap(),
            "only the verified enrollment is enabled"
        );
        assert!(
            db.enable_totp(root, b"sealed", 10, &[[7; 32]], t0())
                .await
                .unwrap()
        );
        assert!(!db.set_totp_pending(root, b"again").await.unwrap());
        assert!(
            !db.accept_totp_step(root, 10).await.unwrap(),
            "confirming step"
        );
        assert!(db.accept_totp_step(root, 11).await.unwrap());
        assert!(!db.accept_totp_step(root, 11).await.unwrap());
        assert!(!db.accept_totp_step(root, 9).await.unwrap());

        assert!(db.use_recovery_code(root, &[7; 32], t0()).await.unwrap());
        assert!(!db.use_recovery_code(root, &[7; 32], t0()).await.unwrap());
        assert_eq!(db.recovery_codes_remaining(root).await.unwrap(), 0);

        db.disable_totp(root).await.unwrap();
        assert_eq!(db.totp_secrets(root).await.unwrap(), TotpSecrets::default());
    }

    #[tokio::test]
    async fn pending_logins_are_single_use_and_expire() {
        let db = Database::open_in_memory().await.unwrap();
        let (root, _) = two_admins(&db).await;
        db.set_totp_pending(root, b"s").await.unwrap();
        db.enable_totp(root, b"s", 1, &[], t0()).await.unwrap();
        let expires = t0() + TimeDelta::minutes(5);
        db.create_pending_login(&[9; 32], root, expires, t0())
            .await
            .unwrap();
        assert_eq!(
            db.claim_pending_login(&[9; 32], expires).await.unwrap(),
            None,
            "expired"
        );
        let claimed = db
            .claim_pending_login(&[9; 32], t0())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.admin_id, root);
        assert_eq!(db.claim_pending_login(&[9; 32], t0()).await.unwrap(), None);
        db.release_pending_login(&[9; 32], &claimed).await.unwrap();
        assert!(
            db.claim_pending_login(&[9; 32], t0())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_member_is_linked_to_at_most_one_admin() {
        let db = Database::open_in_memory().await.unwrap();
        let (root, ada) = two_admins(&db).await;
        let mia = db
            .create_member(
                &crate::members::NewMember {
                    username: "mia",
                    email: "mia@example.org",
                    display_name: "Mia",
                    password_hash: None,
                },
                &[],
                t0(),
            )
            .await
            .unwrap();
        db.set_linked_member(root, Some(mia)).await.unwrap();
        assert!(matches!(
            db.set_linked_member(ada, Some(mia)).await,
            Err(DbError::MemberAlreadyLinked)
        ));
        let linked = db.get_admin(root).await.unwrap().unwrap().linked_member;
        assert_eq!(linked.map(|m| m.username), Some("mia".to_owned()));
        db.set_linked_member(root, None).await.unwrap();
        db.set_linked_member(ada, Some(mia)).await.unwrap();
    }

    #[tokio::test]
    async fn recovery_never_revives_a_tombstone() {
        let db = Database::open_in_memory().await.unwrap();
        let (root, ada) = two_admins(&db).await;
        db.change_admin_status(root, ada, AdminStatusChange::Remove, t0())
            .await
            .unwrap();
        let request = |username, create| RecoverAdmin {
            username,
            reset_totp: false,
            unusable_password_hash: HASH,
            create,
            token_hash: &[3; 32],
            token_expires_at: t0() + TimeDelta::days(1),
        };
        assert_eq!(
            db.recover_admin(&request("ada", Some(admin("ada"))), t0())
                .await
                .unwrap(),
            Recovery::Tombstone
        );
        assert_eq!(
            db.recover_admin(&request("nobody", None), t0())
                .await
                .unwrap(),
            Recovery::NotFound
        );
        assert!(matches!(
            db.recover_admin(&request("eve", Some(admin("eve"))), t0())
                .await
                .unwrap(),
            Recovery::Created { .. }
        ));
    }
}
