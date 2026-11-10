//! Members, entitlements, member deletion and password tokens (domain model
//! §1–2, ADR 0006). Callers pass `now` so lifetimes are testable.

use chrono::{DateTime, Utc};
use sqlx::{Sqlite, Transaction};
use starr_core::auth::SubjectKind;
use starr_core::members::{
    Category, Entitlement, MemberStatus, PasswordTokenPurpose, RequestMode, complete_entitlements,
};

use crate::auth::username_conflict;
use crate::{Database, DbError, parse_ts, ts};

/// Data for a new member. `password_hash` is a PHC string, or `None` until
/// the member sets a password through a link.
#[derive(Debug)]
pub struct NewMember<'a> {
    pub username: &'a str,
    pub email: &'a str,
    pub display_name: &'a str,
    pub password_hash: Option<&'a str>,
}

/// A pending or completed deletion order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deletion {
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
    pub grace_until: DateTime<Utc>,
}

/// A member as listed and shown to admins. Tombstones are never returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRecord {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
    pub display_name: String,
    pub status: MemberStatus,
    /// False until the member used a set-password link (or accepted an invitation).
    pub password_set: bool,
    pub created_at: DateTime<Utc>,
    /// Set while a deletion order is in its grace period.
    pub deletion: Option<Deletion>,
}

/// Filter for [`Database::list_members`].
#[derive(Debug, Default, Clone, Copy)]
pub struct MemberFilter<'a> {
    pub status: Option<MemberStatus>,
    /// Case-insensitive substring of username, display name or email.
    pub query: Option<&'a str>,
}

/// One session of an account, for "where am I logged in".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// Row id; stable while the session lives, never the token.
    pub id: i64,
    pub token_hash: Vec<u8>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

/// A stored password token (only its hash is known).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordTokenRecord {
    pub id: i64,
    pub subject_kind: SubjectKind,
    pub subject_id: i64,
    pub purpose: PasswordTokenPurpose,
    pub expires_at: DateTime<Utc>,
    pub used: bool,
}

/// Result of using a password token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenUse {
    /// Password stored; the member's sessions were ended.
    Done,
    /// Token unknown, used, expired, or the member is not active.
    Rejected,
}

/// Result of using a member's password token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberTokenUse {
    /// Password stored with this password generation (ADR 0010); the
    /// member's sessions were ended.
    Done { generation: i64 },
    /// Token unknown, used, expired, or the member is not active.
    Rejected,
}

type MemberRow = (
    i64,
    String,
    Option<String>,
    String,
    String,
    bool,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

type SessionRow = (i64, Vec<u8>, String, String, Option<String>, Option<String>);

const MEMBER_SELECT: &str = "
    SELECT m.id, m.username, m.email, m.display_name, m.status,
           m.password_hash IS NOT NULL, m.created_at,
           d.requested_by, d.requested_at, d.grace_until
    FROM member m LEFT JOIN member_deletion d ON d.member_id = m.id
    WHERE d.tombstoned_at IS NULL";

fn member_record(row: MemberRow) -> Result<MemberRecord, DbError> {
    let (id, username, email, display_name, status, password_set, created_at, by, at, until) = row;
    let status =
        MemberStatus::parse(&status).ok_or_else(|| crate::unknown("member status", &status))?;
    let deletion = match (by, at, until) {
        (Some(requested_by), Some(at), Some(until)) => Some(Deletion {
            requested_by,
            requested_at: parse_ts(&at)?,
            grace_until: parse_ts(&until)?,
        }),
        _ => None,
    };
    Ok(MemberRecord {
        id,
        username,
        email,
        display_name,
        status,
        password_set,
        created_at: parse_ts(&created_at)?,
        deletion,
    })
}

/// Insert a member and its entitlements inside `tx`.
pub(crate) async fn insert_member(
    tx: &mut Transaction<'_, Sqlite>,
    member: &NewMember<'_>,
    entitlements: &[Entitlement],
    now: DateTime<Utc>,
) -> Result<i64, DbError> {
    // A password set at creation (invitation) is generation 0 (ADR 0010).
    let id = sqlx::query(
        "INSERT INTO member (username, email, display_name, password_hash, created_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(member.username)
    .bind(member.email)
    .bind(member.display_name)
    .bind(member.password_hash)
    .bind(ts(now))
    .execute(&mut **tx)
    .await
    .map_err(username_conflict)?
    .last_insert_rowid();
    write_entitlements(tx, id, entitlements, now).await?;
    Ok(id)
}

/// Replace the whole-category entitlements of `member_id`. Stores one row
/// per category so a later default change does not alter existing members.
/// `updated_at` is when `enabled` last changed: a row whose switch stays as
/// it was keeps its time (admins see "On since …", spec 0008 ADM-12).
async fn write_entitlements(
    tx: &mut Transaction<'_, Sqlite>,
    member_id: i64,
    entitlements: &[Entitlement],
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    let before: Vec<(String, bool, String)> = sqlx::query_as(
        "SELECT category, enabled, updated_at FROM entitlement
         WHERE member_id = ? AND service_instance_id IS NULL",
    )
    .bind(member_id)
    .fetch_all(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM entitlement WHERE member_id = ? AND service_instance_id IS NULL")
        .bind(member_id)
        .execute(&mut **tx)
        .await?;
    let now = ts(now);
    for e in complete_entitlements(entitlements) {
        let since = before
            .iter()
            .find(|(category, enabled, _)| category == e.category.as_str() && *enabled == e.enabled)
            .map_or(now.as_str(), |(_, _, at)| at.as_str());
        sqlx::query(
            "INSERT INTO entitlement (member_id, category, enabled, request_mode, updated_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(member_id)
        .bind(e.category.as_str())
        .bind(e.enabled)
        .bind(e.request_mode.as_str())
        .bind(since)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn delete_sessions_in(
    tx: &mut Transaction<'_, Sqlite>,
    member_id: i64,
) -> Result<(), DbError> {
    sqlx::query("DELETE FROM session WHERE subject_kind = 'member' AND subject_id = ?")
        .bind(member_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn delete_password_tokens_in(
    tx: &mut Transaction<'_, Sqlite>,
    member_id: i64,
) -> Result<(), DbError> {
    sqlx::query("DELETE FROM password_token WHERE subject_kind = 'member' AND subject_id = ?")
        .bind(member_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

impl Database {
    /// Create a member with its entitlements (missing categories get defaults).
    pub async fn create_member(
        &self,
        member: &NewMember<'_>,
        entitlements: &[Entitlement],
        now: DateTime<Utc>,
    ) -> Result<i64, DbError> {
        let mut tx = self.begin_write().await?;
        let id = insert_member(&mut tx, member, entitlements, now).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Whether `username` is used by any account, including tombstones.
    pub async fn username_exists(&self, username: &str) -> Result<bool, DbError> {
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM member WHERE username = ?)
                 OR EXISTS (SELECT 1 FROM admin_account WHERE username = ?)",
        )
        .bind(username)
        .bind(username)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    /// Whether a (non-tombstoned) member uses `email`.
    pub async fn email_exists(&self, email: &str) -> Result<bool, DbError> {
        let (exists,): (bool,) =
            sqlx::query_as("SELECT EXISTS (SELECT 1 FROM member WHERE email = ?)")
                .bind(email)
                .fetch_one(&self.pool)
                .await?;
        Ok(exists)
    }

    /// Members matching `filter`, oldest first. Tombstones are not listed.
    pub async fn list_members(
        &self,
        filter: MemberFilter<'_>,
    ) -> Result<Vec<MemberRecord>, DbError> {
        let pattern = filter.query.map(|q| {
            let escaped = q
                .trim()
                .to_lowercase()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            format!("%{escaped}%")
        });
        let sql = format!(
            "{MEMBER_SELECT}
               AND (?1 IS NULL OR m.status = ?1)
               AND (?2 IS NULL OR lower(m.username) LIKE ?2 ESCAPE '\\'
                    OR lower(m.display_name) LIKE ?2 ESCAPE '\\'
                    OR lower(ifnull(m.email, '')) LIKE ?2 ESCAPE '\\')
             ORDER BY m.id"
        );
        let rows: Vec<MemberRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(filter.status.map(MemberStatus::as_str))
            .bind(pattern)
            .fetch_all(&self.pool)
            .await?;
        crate::readable(rows.into_iter().map(member_record))
    }

    /// One member, unless unknown or a tombstone.
    pub async fn get_member(&self, id: i64) -> Result<Option<MemberRecord>, DbError> {
        let sql = format!("{MEMBER_SELECT} AND m.id = ?");
        let row: Option<MemberRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(member_record).transpose()
    }

    /// The member's whole-category entitlements, one per category.
    pub async fn entitlements(&self, member_id: i64) -> Result<Vec<Entitlement>, DbError> {
        let rows: Vec<(String, bool, String)> = sqlx::query_as(
            "SELECT category, enabled, request_mode FROM entitlement
             WHERE member_id = ? AND service_instance_id IS NULL",
        )
        .bind(member_id)
        .fetch_all(&self.pool)
        .await?;
        let stored = crate::readable(rows.into_iter().map(|(category, enabled, mode)| {
            Ok(Entitlement {
                category: Category::parse(&category)
                    .ok_or_else(|| crate::unknown("category", &category))?,
                enabled,
                request_mode: RequestMode::parse(&mode)
                    .ok_or_else(|| crate::unknown("request mode", &mode))?,
            })
        }))?;
        Ok(complete_entitlements(&stored))
    }

    /// Since when the member's whole-category entitlement for `category` is
    /// switched on; `None` while it is off.
    pub async fn entitlement_enabled_since(
        &self,
        member_id: i64,
        category: Category,
    ) -> Result<Option<DateTime<Utc>>, DbError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT updated_at FROM entitlement
             WHERE member_id = ? AND category = ? AND service_instance_id IS NULL AND enabled = 1",
        )
        .bind(member_id)
        .bind(category.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|(at,)| parse_ts(&at)).transpose()
    }

    /// Replace the member's whole-category entitlements.
    pub async fn replace_entitlements(
        &self,
        member_id: i64,
        entitlements: &[Entitlement],
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        let mut tx = self.begin_write().await?;
        write_entitlements(&mut tx, member_id, entitlements, now).await?;
        tx.commit().await?;
        Ok(())
    }

    /// `active` → `suspended`, ending all of the member's sessions in the
    /// same transaction. False if the member was not active.
    pub async fn suspend_member(&self, id: i64) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let changed = sqlx::query(
            "UPDATE member SET status = 'suspended' WHERE id = ? AND status = 'active'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if changed {
            // No password is written for a suspended member (D9).
            sqlx::query("DELETE FROM password_handoff WHERE member_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            // A leaked link must not work again after reactivation.
            delete_sessions_in(&mut tx, id).await?;
            delete_password_tokens_in(&mut tx, id).await?;
        }
        tx.commit().await?;
        Ok(changed)
    }

    /// `suspended` → `active`. False if the member was not suspended.
    pub async fn reactivate_member(&self, id: i64) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE member SET status = 'active' WHERE id = ? AND status = 'suspended'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Change the member's display name.
    pub async fn set_member_display_name(&self, id: i64, name: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE member SET display_name = ? WHERE id = ?")
            .bind(name)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The member's password hash, if one is set.
    pub async fn member_password_hash(&self, id: i64) -> Result<Option<String>, DbError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT password_hash FROM member WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|(hash,)| hash))
    }

    /// The member's password generation (ADR 0010): incremented on every
    /// Starr password set. `None` for an unknown member.
    pub async fn member_password_generation(&self, id: i64) -> Result<Option<i64>, DbError> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT password_generation FROM member WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(g,)| g))
    }

    /// Store a new password, increment the password generation and end
    /// every session of the member except `keep` (the one changing the
    /// password). Returns the new generation (0 for an unknown member,
    /// where nothing changes).
    pub async fn change_member_password(
        &self,
        id: i64,
        password_hash: &str,
        keep: &[u8],
    ) -> Result<i64, DbError> {
        let mut tx = self.begin_write().await?;
        let row: Option<(i64,)> = sqlx::query_as(
            "UPDATE member SET password_hash = ?, password_generation = password_generation + 1
             WHERE id = ? RETURNING password_generation",
        )
        .bind(password_hash)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let generation = row.map_or(0, |(g,)| g);
        sqlx::query(
            "DELETE FROM session WHERE subject_kind = 'member' AND subject_id = ? AND token_hash <> ?",
        )
        .bind(id)
        .bind(keep)
        .execute(&mut *tx)
        .await?;
        // An outstanding reset link would bypass the new password.
        delete_password_tokens_in(&mut tx, id).await?;
        tx.commit().await?;
        Ok(generation)
    }

    /// Whether the audit log has an entry the member performed (login,
    /// profile change, …). Without one the member may be deleted outright.
    pub async fn member_has_own_audit_entries(&self, username: &str) -> Result<bool, DbError> {
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM audit_log WHERE actor_kind = 'member' AND actor_name = ?)",
        )
        .bind(username)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    /// Delete a member outright (fast path) if the audit log has no entry
    /// the member performed: sessions, tokens, entitlements and the row; the
    /// username is free again. The check runs in the same transaction, so a
    /// member who just started using Starr is not hard-deleted. False if the
    /// member is unknown, has own audit entries or owns an account Starr
    /// created in a service (then schedule a deletion).
    pub async fn delete_unused_member(&self, id: i64) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        // An account Starr created in a service would be left behind
        // enabled and unmanaged: such a member goes through a deletion
        // order instead (disable now, delete after the grace period, D16).
        if crate::account_lifecycle::owns_provisioned_in(&mut tx, id).await? {
            tx.rollback().await?;
            return Ok(false);
        }
        // Their request items go with them: acquisitions nobody else wants
        // are released afterwards (spec 0004 R1).
        let linked: Vec<(i64,)> = sqlx::query_as(
            "SELECT DISTINCT acquisition_id FROM request_item
             WHERE member_id = ? AND decision = 'approved' AND acquisition_id IS NOT NULL",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        let deleted = sqlx::query(
            "DELETE FROM member WHERE id = ?1 AND NOT EXISTS (
                 SELECT 1 FROM audit_log
                 WHERE actor_kind = 'member'
                   AND actor_name = (SELECT username FROM member WHERE id = ?1))",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !deleted {
            tx.rollback().await?;
            return Ok(false);
        }
        delete_sessions_in(&mut tx, id).await?;
        delete_password_tokens_in(&mut tx, id).await?;
        let now = Utc::now();
        for (acquisition,) in linked {
            crate::requests::release_acquisition(&mut tx, acquisition, now).await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Start a deletion order: the member becomes `retired`, sessions and
    /// password links end, and a tombstone follows after `grace_until`.
    /// False if the member is unknown or already retired.
    pub async fn schedule_member_deletion(
        &self,
        id: i64,
        requested_by: &str,
        grace_until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let changed = sqlx::query(
            "UPDATE member SET status = 'retired' WHERE id = ? AND status IN ('active', 'suspended')",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !changed {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO member_deletion (member_id, requested_by, requested_at, grace_until)
             VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(requested_by)
        .bind(ts(now))
        .bind(ts(grace_until))
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM password_handoff WHERE member_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        delete_sessions_in(&mut tx, id).await?;
        delete_password_tokens_in(&mut tx, id).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Cancel a deletion order within its grace period. The member returns
    /// `suspended` (domain model §1). False if there is nothing to restore.
    pub async fn restore_member(&self, id: i64, now: DateTime<Utc>) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let removed = sqlx::query(
            "DELETE FROM member_deletion
             WHERE member_id = ? AND tombstoned_at IS NULL AND grace_until > ?",
        )
        .bind(id)
        .bind(ts(now))
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !removed {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query("UPDATE member SET status = 'suspended' WHERE id = ? AND status = 'retired'")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Turn every member whose grace period ended into a tombstone: only
    /// username and the deletion order remain; the username stays reserved,
    /// the email is freed. Returns the usernames tombstoned.
    pub async fn tombstone_due_members(&self, now: DateTime<Utc>) -> Result<Vec<String>, DbError> {
        let mut tx = self.begin_write().await?;
        let due: Vec<(i64, String)> = sqlx::query_as(
            "SELECT m.id, m.username FROM member_deletion d JOIN member m ON m.id = d.member_id
             WHERE d.tombstoned_at IS NULL AND d.grace_until <= ?",
        )
        .bind(ts(now))
        .fetch_all(&mut *tx)
        .await?;
        for (id, _) in &due {
            sqlx::query(
                "UPDATE member SET email = NULL, password_hash = NULL, display_name = username,
                        status = 'retired'
                 WHERE id = ?",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM entitlement WHERE member_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            delete_sessions_in(&mut tx, *id).await?;
            delete_password_tokens_in(&mut tx, *id).await?;
            // Personal data elsewhere: the email preset of the invitation they used.
            sqlx::query("UPDATE invitation SET preset_email = NULL WHERE used_by_member_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE member_deletion SET tombstoned_at = ? WHERE member_id = ?")
                .bind(ts(now))
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(due.into_iter().map(|(_, name)| name).collect())
    }

    /// Live sessions of one account, most recently used first.
    pub async fn subject_sessions(
        &self,
        kind: SubjectKind,
        id: i64,
    ) -> Result<Vec<SessionInfo>, DbError> {
        let rows: Vec<SessionRow> = sqlx::query_as(
            "SELECT rowid, token_hash, created_at, last_seen_at, ip, user_agent FROM session
                 WHERE subject_kind = ? AND subject_id = ?
                 ORDER BY last_seen_at DESC, rowid DESC",
        )
        .bind(kind.as_str())
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, token_hash, created, seen, ip, user_agent)| {
                Ok(SessionInfo {
                    id,
                    token_hash,
                    created_at: parse_ts(&created)?,
                    last_seen_at: parse_ts(&seen)?,
                    ip,
                    user_agent,
                })
            })
            .collect()
    }

    /// Delete one session of an account by row id. False if it does not
    /// exist or belongs to someone else.
    pub async fn delete_subject_session(
        &self,
        kind: SubjectKind,
        id: i64,
        session_id: i64,
    ) -> Result<bool, DbError> {
        let result = sqlx::query(
            "DELETE FROM session WHERE rowid = ? AND subject_kind = ? AND subject_id = ?",
        )
        .bind(session_id)
        .bind(kind.as_str())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Delete all sessions of an account ("log out everywhere").
    pub async fn delete_subject_sessions(
        &self,
        kind: SubjectKind,
        id: i64,
    ) -> Result<u64, DbError> {
        let result = sqlx::query("DELETE FROM session WHERE subject_kind = ? AND subject_id = ?")
            .bind(kind.as_str())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// Store a new password token for a member. Earlier unused tokens of the
    /// member are deleted: only the newest link works.
    pub async fn create_password_token(
        &self,
        token_hash: &[u8],
        member_id: i64,
        purpose: PasswordTokenPurpose,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        let mut tx = self.begin_write().await?;
        sqlx::query(
            "DELETE FROM password_token
             WHERE subject_kind = 'member' AND subject_id = ? AND used_at IS NULL",
        )
        .bind(member_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO password_token
                (token_hash, subject_kind, subject_id, purpose, created_at, expires_at)
             VALUES (?, 'member', ?, ?, ?, ?)",
        )
        .bind(token_hash)
        .bind(member_id)
        .bind(purpose.as_str())
        .bind(ts(now))
        .bind(ts(expires_at))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Look up a password token by hash.
    pub async fn find_password_token(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<PasswordTokenRecord>, DbError> {
        let row: Option<(i64, String, i64, String, String, bool)> = sqlx::query_as(
            "SELECT id, subject_kind, subject_id, purpose, expires_at, used_at IS NOT NULL
             FROM password_token WHERE token_hash = ?",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;
        let Some((id, kind, subject_id, purpose, expires_at, used)) = row else {
            return Ok(None);
        };
        Ok(Some(PasswordTokenRecord {
            id,
            subject_kind: SubjectKind::parse(&kind)
                .ok_or_else(|| crate::unknown("subject kind", &kind))?,
            subject_id,
            purpose: PasswordTokenPurpose::parse(&purpose)
                .ok_or_else(|| crate::unknown("token purpose", &purpose))?,
            expires_at: parse_ts(&expires_at)?,
            used,
        }))
    }

    /// Use a member's password token: mark it used, store the password and
    /// end the member's sessions, all or nothing. Rejected when the token is
    /// used or expired at `now` or the member is not active.
    pub async fn use_password_token(
        &self,
        token_id: i64,
        member_id: i64,
        password_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<MemberTokenUse, DbError> {
        let mut tx = self.begin_write().await?;
        let claimed = sqlx::query(
            "UPDATE password_token SET used_at = ?
             WHERE id = ? AND subject_kind = 'member' AND subject_id = ?
               AND used_at IS NULL AND expires_at > ?",
        )
        .bind(ts(now))
        .bind(token_id)
        .bind(member_id)
        .bind(ts(now))
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        let stored: Option<(i64,)> = if claimed {
            sqlx::query_as(
                "UPDATE member SET password_hash = ?, password_generation = password_generation + 1
                 WHERE id = ? AND status = 'active' RETURNING password_generation",
            )
            .bind(password_hash)
            .bind(member_id)
            .fetch_optional(&mut *tx)
            .await?
        } else {
            None
        };
        let Some((generation,)) = stored else {
            tx.rollback().await?;
            return Ok(MemberTokenUse::Rejected);
        };
        delete_sessions_in(&mut tx, member_id).await?;
        tx.commit().await?;
        Ok(MemberTokenUse::Done { generation })
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;
    use crate::audit::{ActorKind, AuditEntry};
    use crate::auth::NewSession;
    use crate::test_support::t0;

    fn new_member<'a>(username: &'a str, email: &'a str) -> NewMember<'a> {
        NewMember {
            username,
            email,
            display_name: username,
            password_hash: Some("$argon2id$dummy"),
        }
    }

    async fn two_members(db: &Database) -> (i64, i64) {
        let mia = db
            .create_member(&new_member("mia", "mia@example.org"), &[], t0())
            .await
            .unwrap();
        let noah = db
            .create_member(&new_member("noah", "noah@example.org"), &[], t0())
            .await
            .unwrap();
        (mia, noah)
    }

    async fn add_session(db: &Database, member: i64, byte: u8) {
        db.create_session(
            &NewSession {
                token_hash: &[byte; 32],
                subject_kind: SubjectKind::Member,
                subject_id: member,
                expires_at: t0() + TimeDelta::days(30),
                ip: "203.0.113.1",
                user_agent: None,
            },
            t0(),
        )
        .await
        .unwrap();
    }

    async fn session_count(db: &Database, member: i64) -> usize {
        db.subject_sessions(SubjectKind::Member, member)
            .await
            .unwrap()
            .len()
    }

    /// Lesson 4: every unique index on a table with a `member_id` column
    /// includes the owner.
    #[tokio::test]
    async fn unique_indexes_on_personal_tables_include_the_owner() {
        let db = Database::open_in_memory().await.unwrap();
        let tables: Vec<(String,)> = sqlx::query_as(
            "SELECT m.name FROM sqlite_master m
             WHERE m.type = 'table'
               AND EXISTS (SELECT 1 FROM pragma_table_info(m.name) WHERE name = 'member_id')",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        assert!(tables.len() >= 2, "{tables:?}");
        for (table,) in tables {
            let indexes: Vec<(String, bool)> =
                sqlx::query_as("SELECT name, \"unique\" FROM pragma_index_list(?)")
                    .bind(&table)
                    .fetch_all(db.pool())
                    .await
                    .unwrap();
            for (index, unique) in indexes {
                // Deliberately across members: a service account belongs to
                // at most one member (spec 0005 D4).
                if !unique || index == "service_account_remote" {
                    continue;
                }
                let columns: Vec<(Option<String>,)> =
                    sqlx::query_as("SELECT name FROM pragma_index_info(?)")
                        .bind(&index)
                        .fetch_all(db.pool())
                        .await
                        .unwrap();
                assert!(
                    columns.iter().any(|(c,)| c.as_deref() == Some("member_id")),
                    "unique index {index} on {table} lacks member_id"
                );
            }
        }
    }

    #[tokio::test]
    async fn emails_are_unique_case_insensitively() {
        let db = Database::open_in_memory().await.unwrap();
        two_members(&db).await;
        let err = db
            .create_member(&new_member("other", "MIA@example.org"), &[], t0())
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::EmailTaken), "{err:?}");
        let err = db
            .create_member(&new_member("MIA", "x@example.org"), &[], t0())
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::UsernameTaken), "{err:?}");
    }

    #[tokio::test]
    async fn entitlements_belong_to_one_member() {
        let db = Database::open_in_memory().await.unwrap();
        let (mia, noah) = two_members(&db).await;
        let movies = Entitlement {
            category: Category::Movies,
            enabled: true,
            request_mode: RequestMode::Auto,
        };
        db.replace_entitlements(mia, &[movies], t0()).await.unwrap();
        // Same category for the second member does not collide (lesson 4).
        db.replace_entitlements(noah, &[movies], t0())
            .await
            .unwrap();
        db.replace_entitlements(noah, &[], t0()).await.unwrap();

        assert_eq!(db.entitlements(mia).await.unwrap()[0], movies);
        let noahs = db.entitlements(noah).await.unwrap();
        assert_eq!(noahs.len(), 4);
        assert!(noahs.iter().all(|e| !e.enabled));
    }

    #[tokio::test]
    async fn enabled_since_moves_only_when_the_switch_changes() {
        let db = Database::open_in_memory().await.unwrap();
        let (mia, _) = two_members(&db).await;
        let adult = |enabled, request_mode| Entitlement {
            category: Category::Adult,
            enabled,
            request_mode,
        };
        assert_eq!(
            db.entitlement_enabled_since(mia, Category::Adult)
                .await
                .unwrap(),
            None,
            "off by default"
        );

        let t1 = t0() + chrono::TimeDelta::days(1);
        db.replace_entitlements(mia, &[adult(true, RequestMode::Approval)], t1)
            .await
            .unwrap();
        assert_eq!(
            db.entitlement_enabled_since(mia, Category::Adult)
                .await
                .unwrap(),
            Some(t1)
        );

        // Saving again, even with another request mode, keeps the time.
        let t2 = t1 + chrono::TimeDelta::days(1);
        db.replace_entitlements(mia, &[adult(true, RequestMode::Auto)], t2)
            .await
            .unwrap();
        assert_eq!(
            db.entitlement_enabled_since(mia, Category::Adult)
                .await
                .unwrap(),
            Some(t1)
        );

        db.replace_entitlements(mia, &[adult(false, RequestMode::Auto)], t2)
            .await
            .unwrap();
        assert_eq!(
            db.entitlement_enabled_since(mia, Category::Adult)
                .await
                .unwrap(),
            None
        );
        let t3 = t2 + chrono::TimeDelta::days(1);
        db.replace_entitlements(mia, &[adult(true, RequestMode::Auto)], t3)
            .await
            .unwrap();
        assert_eq!(
            db.entitlement_enabled_since(mia, Category::Adult)
                .await
                .unwrap(),
            Some(t3),
            "switched on again: a new time"
        );
    }

    #[tokio::test]
    async fn suspension_ends_only_that_members_sessions() {
        let db = Database::open_in_memory().await.unwrap();
        let (mia, noah) = two_members(&db).await;
        add_session(&db, mia, 1).await;
        add_session(&db, mia, 2).await;
        add_session(&db, noah, 3).await;

        assert!(db.suspend_member(mia).await.unwrap());
        assert!(!db.suspend_member(mia).await.unwrap(), "already suspended");
        assert_eq!(session_count(&db, mia).await, 0);
        assert_eq!(session_count(&db, noah).await, 1);

        assert!(db.reactivate_member(mia).await.unwrap());
        assert!(!db.reactivate_member(noah).await.unwrap(), "not suspended");
    }

    #[tokio::test]
    async fn session_revocation_is_scoped_to_the_owner() {
        let db = Database::open_in_memory().await.unwrap();
        let (mia, noah) = two_members(&db).await;
        add_session(&db, mia, 1).await;
        add_session(&db, noah, 2).await;
        let noahs = db
            .subject_sessions(SubjectKind::Member, noah)
            .await
            .unwrap();
        // Mia cannot delete Noah's session by guessing its id.
        assert!(
            !db.delete_subject_session(SubjectKind::Member, mia, noahs[0].id)
                .await
                .unwrap()
        );
        assert_eq!(session_count(&db, noah).await, 1);
        assert_eq!(
            db.delete_subject_sessions(SubjectKind::Member, mia)
                .await
                .unwrap(),
            1
        );
        assert_eq!(session_count(&db, noah).await, 1);
    }

    #[tokio::test]
    async fn password_change_keeps_only_the_current_session() {
        let db = Database::open_in_memory().await.unwrap();
        let (mia, noah) = two_members(&db).await;
        add_session(&db, mia, 1).await;
        add_session(&db, mia, 2).await;
        add_session(&db, noah, 3).await;
        db.change_member_password(mia, "new", &[1; 32])
            .await
            .unwrap();
        let left = db.subject_sessions(SubjectKind::Member, mia).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].token_hash, vec![1; 32]);
        assert_eq!(session_count(&db, noah).await, 1);
        assert_eq!(
            db.member_password_hash(mia).await.unwrap().as_deref(),
            Some("new")
        );
    }

    #[tokio::test]
    async fn fast_path_depends_on_the_members_own_audit_entries() {
        let db = Database::open_in_memory().await.unwrap();
        let (mia, _) = two_members(&db).await;
        assert!(!db.member_has_own_audit_entries("mia").await.unwrap());
        // An admin acting on the member does not count.
        db.audit(
            &AuditEntry {
                actor_kind: ActorKind::Admin,
                actor_name: Some("root"),
                action: "member.created",
                target: Some("mia"),
                details: None,
            },
            t0(),
        )
        .await
        .unwrap();
        assert!(!db.member_has_own_audit_entries("mia").await.unwrap());
        db.audit(
            &AuditEntry {
                actor_kind: ActorKind::Member,
                actor_name: Some("mia"),
                action: "auth.login",
                target: None,
                details: None,
            },
            t0(),
        )
        .await
        .unwrap();
        assert!(db.member_has_own_audit_entries("mia").await.unwrap());
        assert!(!db.member_has_own_audit_entries("noah").await.unwrap());

        assert!(
            !db.delete_unused_member(mia).await.unwrap(),
            "mia has own entries"
        );
        sqlx::query("DELETE FROM audit_log")
            .execute(db.pool())
            .await
            .unwrap();
        assert!(db.delete_unused_member(mia).await.unwrap());
        assert!(db.get_member(mia).await.unwrap().is_none());
        assert!(
            !db.username_exists("mia").await.unwrap(),
            "name is free again"
        );
    }

    #[tokio::test]
    async fn deletion_grace_restore_and_tombstone() {
        let db = Database::open_in_memory().await.unwrap();
        let (mia, noah) = two_members(&db).await;
        add_session(&db, mia, 1).await;
        add_session(&db, noah, 2).await;
        let grace = t0() + TimeDelta::days(30);

        assert!(
            db.schedule_member_deletion(mia, "root", grace, t0())
                .await
                .unwrap()
        );
        assert!(
            !db.schedule_member_deletion(mia, "root", grace, t0())
                .await
                .unwrap()
        );
        let record = db.get_member(mia).await.unwrap().unwrap();
        assert_eq!(record.status, MemberStatus::Retired);
        assert_eq!(record.deletion.unwrap().grace_until, grace);
        assert_eq!(session_count(&db, mia).await, 0);
        assert_eq!(session_count(&db, noah).await, 1);

        // Restore within the grace period: back as suspended.
        assert!(
            db.restore_member(mia, t0() + TimeDelta::days(1))
                .await
                .unwrap()
        );
        let record = db.get_member(mia).await.unwrap().unwrap();
        assert_eq!(record.status, MemberStatus::Suspended);
        assert!(record.deletion.is_none());

        // Nothing is due before the grace period ends.
        db.schedule_member_deletion(mia, "root", grace, t0())
            .await
            .unwrap();
        assert_eq!(
            db.tombstone_due_members(grace - TimeDelta::seconds(1))
                .await
                .unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(db.tombstone_due_members(grace).await.unwrap(), vec!["mia"]);
        assert!(!db.restore_member(mia, grace).await.unwrap(), "too late");

        // Tombstone: unlisted, name reserved, email freed, second member untouched.
        assert!(db.get_member(mia).await.unwrap().is_none());
        let listed = db.list_members(MemberFilter::default()).await.unwrap();
        assert_eq!(listed.iter().map(|m| m.id).collect::<Vec<_>>(), vec![noah]);
        assert!(db.username_exists("mia").await.unwrap());
        assert!(db.find_credentials("mia").await.unwrap().is_none());
        db.create_member(&new_member("mia2", "mia@example.org"), &[], t0())
            .await
            .unwrap();
        assert!(matches!(
            db.create_member(&new_member("mia", "m@example.org"), &[], t0())
                .await,
            Err(DbError::UsernameTaken)
        ));
        assert_eq!(session_count(&db, noah).await, 1);
    }

    #[tokio::test]
    async fn list_filters_by_status_and_query() {
        let db = Database::open_in_memory().await.unwrap();
        let (mia, _) = two_members(&db).await;
        db.suspend_member(mia).await.unwrap();
        let by_status = db
            .list_members(MemberFilter {
                status: Some(MemberStatus::Suspended),
                query: None,
            })
            .await
            .unwrap();
        assert_eq!(by_status.len(), 1);
        assert_eq!(by_status[0].username, "mia");
        let by_query = db
            .list_members(MemberFilter {
                status: None,
                query: Some("NOAH@"),
            })
            .await
            .unwrap();
        assert_eq!(by_query.len(), 1);
        assert_eq!(by_query[0].username, "noah");
        let wildcard = db
            .list_members(MemberFilter {
                status: None,
                query: Some("%"),
            })
            .await
            .unwrap();
        assert!(wildcard.is_empty(), "LIKE wildcards are escaped");
    }

    #[tokio::test]
    async fn suspension_and_password_change_void_outstanding_links() {
        let db = Database::open_in_memory().await.unwrap();
        let (mia, noah) = two_members(&db).await;
        let expires = t0() + TimeDelta::days(7);
        for (hash, member) in [([1; 32], mia), ([2; 32], noah)] {
            db.create_password_token(&hash, member, PasswordTokenPurpose::Reset, expires, t0())
                .await
                .unwrap();
        }
        db.suspend_member(mia).await.unwrap();
        assert!(db.find_password_token(&[1; 32]).await.unwrap().is_none());
        db.change_member_password(noah, "new", &[9; 32])
            .await
            .unwrap();
        assert!(db.find_password_token(&[2; 32]).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn password_tokens_are_single_use_newest_only_and_expire() {
        let db = Database::open_in_memory().await.unwrap();
        let (mia, noah) = two_members(&db).await;
        let expires = t0() + TimeDelta::days(7);
        db.create_password_token(&[1; 32], mia, PasswordTokenPurpose::Set, expires, t0())
            .await
            .unwrap();
        db.create_password_token(&[2; 32], mia, PasswordTokenPurpose::Reset, expires, t0())
            .await
            .unwrap();
        db.create_password_token(&[3; 32], noah, PasswordTokenPurpose::Set, expires, t0())
            .await
            .unwrap();
        assert!(
            db.find_password_token(&[1; 32]).await.unwrap().is_none(),
            "replaced"
        );
        let token = db.find_password_token(&[2; 32]).await.unwrap().unwrap();
        assert_eq!(token.subject_id, mia);
        assert_eq!(token.purpose, PasswordTokenPurpose::Reset);

        // A token cannot set another member's password.
        assert_eq!(
            db.use_password_token(token.id, noah, "x", t0())
                .await
                .unwrap(),
            MemberTokenUse::Rejected
        );
        add_session(&db, mia, 9).await;
        assert_eq!(
            db.use_password_token(token.id, mia, "new", t0())
                .await
                .unwrap(),
            MemberTokenUse::Done { generation: 1 }
        );
        assert_eq!(session_count(&db, mia).await, 0);
        assert_eq!(
            db.use_password_token(token.id, mia, "again", t0())
                .await
                .unwrap(),
            MemberTokenUse::Rejected
        );
        assert_eq!(
            db.member_password_hash(mia).await.unwrap().as_deref(),
            Some("new")
        );

        let noahs = db.find_password_token(&[3; 32]).await.unwrap().unwrap();
        assert_eq!(
            db.use_password_token(noahs.id, noah, "late", expires)
                .await
                .unwrap(),
            MemberTokenUse::Rejected,
            "expired"
        );
    }
}
