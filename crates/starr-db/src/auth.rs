//! Accounts, sessions and login attempts (ADR 0005).
//!
//! Callers pass `now` explicitly so lifetimes and throttling are testable with
//! an injected clock. Timestamps are stored as fixed-width UTC text.

use chrono::{DateTime, Utc};
use starr_core::auth::{
    MAX_FAILURES_PER_DEVICE, MAX_FAILURES_PER_IP, MAX_FAILURES_PER_TOKEN,
    MAX_FAILURES_PER_USERNAME, MAX_TOKEN_FAILURES_PER_IP, Subject, SubjectKind, THROTTLE_WINDOW,
};

use crate::{Database, DbError, parse_ts, ts};

/// Map the cross-table username trigger and unique constraints to a typed error.
pub(crate) fn username_conflict(err: sqlx::Error) -> DbError {
    if let sqlx::Error::Database(db) = &err {
        if db.message().contains("member.email") {
            return DbError::EmailTaken;
        }
        if db.is_unique_violation() || db.message().contains("username_taken") {
            return DbError::UsernameTaken;
        }
    }
    DbError::Sqlx(err)
}

/// Data for a new admin account. `password_hash` is a PHC string.
#[derive(Debug)]
pub struct NewAdmin<'a> {
    pub username: &'a str,
    pub display_name: &'a str,
    pub password_hash: &'a str,
}

/// What login needs to check a password. Never log `password_hash`.
#[derive(Debug)]
pub struct Credentials {
    pub subject: Subject,
    pub password_hash: String,
    /// False for suspended or removed accounts: they cannot log in.
    pub active: bool,
    /// Admins with TOTP on need a second step before a session starts.
    pub totp_enabled: bool,
}

/// A stored session together with its (active) subject.
#[derive(Debug)]
pub struct SessionRecord {
    pub subject: Subject,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Until then an admin in member view may switch back (re-authenticated).
    pub elevated_until: Option<DateTime<Utc>>,
    /// Set when an admin switched into the member view of their linked member.
    pub origin_admin_id: Option<i64>,
    /// Admin subjects: TOTP is on.
    pub totp_enabled: bool,
    /// The private (adult) category is unlocked until then (spec 0007 Q7).
    pub private_unlocked_until: Option<DateTime<Utc>>,
}

/// Who a rotated session belongs to (switch, elevation).
#[derive(Debug, Clone, Copy)]
pub struct RotatedSession<'a> {
    /// Hash of the new token.
    pub token_hash: &'a [u8],
    pub subject_kind: SubjectKind,
    pub subject_id: i64,
    pub origin_admin_id: Option<i64>,
    pub elevated_until: Option<DateTime<Utc>>,
}

/// Data for a new session.
#[derive(Debug)]
pub struct NewSession<'a> {
    pub token_hash: &'a [u8],
    pub subject_kind: SubjectKind,
    pub subject_id: i64,
    pub expires_at: DateTime<Utc>,
    pub ip: &'a str,
    pub user_agent: Option<&'a str>,
}

/// Who a login attempt is counted against.
#[derive(Debug, Clone, Copy)]
pub struct AttemptKey<'a> {
    /// Hash of the normalized username (never the plain text).
    pub username_hash: &'a str,
    /// Client IP (IPv6 grouped by /64).
    pub ip: &'a str,
    /// Device cookie id, if a valid one for this username was presented.
    pub device: Option<&'a str>,
}

/// Result of registering a login attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginAttempt {
    /// Within limits; finish the attempt with [`Database::finish_login_attempt`].
    Allowed(i64),
    /// Too many recent failures for this username or IP; nothing was recorded.
    Throttled,
}

type SubjectRow = (i64, String, String, String, bool, bool);

type SessionRow = (
    String,
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    bool,
    Option<String>,
);

impl Database {
    /// Whether any admin account exists (setup is complete).
    pub async fn admin_exists(&self) -> Result<bool, DbError> {
        let (exists,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM admin_account)")
            .fetch_one(&self.pool)
            .await?;
        Ok(exists)
    }

    /// Create the first admin account. Returns `None` (and changes nothing)
    /// if an admin already exists, so concurrent setups cannot both succeed.
    pub async fn create_first_admin(
        &self,
        admin: &NewAdmin<'_>,
        now: DateTime<Utc>,
    ) -> Result<Option<i64>, DbError> {
        let result = sqlx::query(
            "INSERT INTO admin_account (username, display_name, password_hash, created_at)
             SELECT ?, ?, ?, ? WHERE NOT EXISTS (SELECT 1 FROM admin_account)",
        )
        .bind(admin.username)
        .bind(admin.display_name)
        .bind(admin.password_hash)
        .bind(ts(now))
        .execute(&self.pool)
        .await
        .map_err(username_conflict)?;
        Ok((result.rows_affected() == 1).then(|| result.last_insert_rowid()))
    }

    /// Look up login credentials by normalized username across admins and
    /// members. Members without a password yet are treated as unknown.
    pub async fn find_credentials(&self, username: &str) -> Result<Option<Credentials>, DbError> {
        let admin: Option<SubjectRow> = sqlx::query_as(
            "SELECT id, username, display_name, password_hash,
                    suspended_at IS NULL AND removed_at IS NULL,
                    totp_secret_sealed IS NOT NULL
             FROM admin_account WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(row) = admin {
            return Ok(Some(credentials(SubjectKind::Admin, row)));
        }
        let member: Option<SubjectRow> = sqlx::query_as(
            "SELECT id, username, display_name, password_hash, status = 'active', FALSE
             FROM member WHERE username = ? AND password_hash IS NOT NULL",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        Ok(member.map(|row| credentials(SubjectKind::Member, row)))
    }

    /// Store a new session (created and last seen `now`).
    pub async fn create_session(
        &self,
        session: &NewSession<'_>,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO session
                (token_hash, subject_kind, subject_id, created_at, last_seen_at, expires_at, ip, user_agent)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(session.token_hash)
        .bind(session.subject_kind.as_str())
        .bind(session.subject_id)
        .bind(ts(now))
        .bind(ts(now))
        .bind(ts(session.expires_at))
        .bind(session.ip)
        .bind(session.user_agent)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Find a session by token hash. Sessions whose subject is suspended,
    /// removed or retired are not returned, nor member-view sessions whose
    /// admin is no longer active or no longer linked to the member.
    /// Lifetimes are checked by the caller.
    pub async fn find_session(&self, token_hash: &[u8]) -> Result<Option<SessionRecord>, DbError> {
        let row: Option<SessionRow> = sqlx::query_as(
            "SELECT s.subject_kind, s.subject_id, a.username, a.display_name, s.last_seen_at,
                    s.expires_at, s.elevated_until, NULL, a.totp_secret_sealed IS NOT NULL,
                    s.private_unlocked_until
             FROM session s JOIN admin_account a ON a.id = s.subject_id
             WHERE s.token_hash = ? AND s.subject_kind = 'admin'
               AND a.suspended_at IS NULL AND a.removed_at IS NULL
             UNION ALL
             SELECT s.subject_kind, s.subject_id, m.username, m.display_name, s.last_seen_at,
                    s.expires_at, s.elevated_until, s.origin_admin_id, FALSE,
                    s.private_unlocked_until
             FROM session s JOIN member m ON m.id = s.subject_id
             WHERE s.token_hash = ? AND s.subject_kind = 'member' AND m.status = 'active'
               AND (s.origin_admin_id IS NULL OR EXISTS (
                   SELECT 1 FROM admin_account o
                   WHERE o.id = s.origin_admin_id AND o.linked_member_id = m.id
                     AND o.suspended_at IS NULL AND o.removed_at IS NULL))",
        )
        .bind(token_hash)
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;
        let Some((
            kind,
            id,
            username,
            display_name,
            last_seen,
            expires,
            elevated,
            origin,
            totp,
            unlocked,
        )) = row
        else {
            return Ok(None);
        };
        let kind =
            SubjectKind::parse(&kind).ok_or_else(|| crate::unknown("subject kind", &kind))?;
        Ok(Some(SessionRecord {
            subject: Subject {
                kind,
                id,
                username,
                display_name,
            },
            last_seen_at: parse_ts(&last_seen)?,
            expires_at: parse_ts(&expires)?,
            elevated_until: elevated.as_deref().map(parse_ts).transpose()?,
            origin_admin_id: origin,
            totp_enabled: totp,
            private_unlocked_until: unlocked.as_deref().map(parse_ts).transpose()?,
        }))
    }

    /// Unlock the private category for session `token_hash` until `until`,
    /// or lock it (`None`). False if the session does not exist.
    pub async fn set_private_unlock(
        &self,
        token_hash: &[u8],
        until: Option<DateTime<Utc>>,
    ) -> Result<bool, DbError> {
        let done =
            sqlx::query("UPDATE session SET private_unlocked_until = ? WHERE token_hash = ?")
                .bind(until.map(ts))
                .bind(token_hash)
                .execute(&self.pool)
                .await?
                .rows_affected()
                == 1;
        Ok(done)
    }

    /// Close every session's private unlock (the adult category was
    /// switched off or on: the gate closes for everyone at once), or only
    /// those of member `member_id` (their adult entitlement changed).
    pub async fn lock_private_unlocks(&self, member_id: Option<i64>) -> Result<u64, DbError> {
        let done = sqlx::query(
            "UPDATE session SET private_unlocked_until = NULL
             WHERE private_unlocked_until IS NOT NULL
               AND (?1 IS NULL OR (subject_kind = 'member' AND subject_id = ?1))",
        )
        .bind(member_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(done)
    }

    /// Adult activity: move an unlock that is still open at `now` on to
    /// `until`. A closed unlock stays closed.
    pub async fn extend_private_unlock(
        &self,
        token_hash: &[u8],
        now: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE session SET private_unlocked_until = ?
             WHERE token_hash = ? AND private_unlocked_until > ?",
        )
        .bind(ts(until))
        .bind(token_hash)
        .bind(ts(now))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Replace a session by a new token for `next` (subject switch or
    /// elevation), keeping its absolute expiry and client details. False if
    /// the old session no longer exists.
    pub async fn rotate_session(
        &self,
        old_hash: &[u8],
        next: &RotatedSession<'_>,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let inserted = sqlx::query(
            "INSERT INTO session
                (token_hash, subject_kind, subject_id, created_at, last_seen_at, expires_at,
                 ip, user_agent, elevated_until, origin_admin_id)
             SELECT ?, ?, ?, created_at, ?, expires_at, ip, user_agent, ?, ?
             FROM session WHERE token_hash = ?",
        )
        .bind(next.token_hash)
        .bind(next.subject_kind.as_str())
        .bind(next.subject_id)
        .bind(ts(now))
        .bind(next.elevated_until.map(ts))
        .bind(next.origin_admin_id)
        .bind(old_hash)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !inserted {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query("DELETE FROM session WHERE token_hash = ?")
            .bind(old_hash)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Record activity on a session.
    pub async fn touch_session(
        &self,
        token_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE session SET last_seen_at = ? WHERE token_hash = ?")
            .bind(ts(now))
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Delete one session (logout, expiry, rotation).
    pub async fn delete_session(&self, token_hash: &[u8]) -> Result<(), DbError> {
        sqlx::query("DELETE FROM session WHERE token_hash = ?")
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Delete sessions past their absolute expiry or idle since before `idle_before`.
    pub async fn delete_expired_sessions(
        &self,
        now: DateTime<Utc>,
        idle_before: DateTime<Utc>,
    ) -> Result<u64, DbError> {
        let result = sqlx::query("DELETE FROM session WHERE expires_at <= ? OR last_seen_at <= ?")
            .bind(ts(now))
            .bind(ts(idle_before))
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// Register a login attempt as a provisional failure, unless it is over
    /// a throttle limit within the window: per IP always; per device cookie
    /// when `device` is given (a valid device cookie for this username),
    /// otherwise per username, counting only attempts without device cookie.
    ///
    /// Insert and count happen in one write transaction, so concurrent
    /// attempts cannot slip past the limit.
    pub async fn begin_login_attempt(
        &self,
        attempt: &AttemptKey<'_>,
        now: DateTime<Utc>,
    ) -> Result<LoginAttempt, DbError> {
        let since = ts(now - THROTTLE_WINDOW);
        let mut tx = self.begin_write().await?;
        let id = sqlx::query(
            "INSERT INTO login_attempt (username_hash, ip, device, at, success) VALUES (?, ?, ?, ?, 0)",
        )
        .bind(attempt.username_hash)
        .bind(attempt.ip)
        .bind(attempt.device)
        .bind(ts(now))
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
        let (by_ip,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM login_attempt
             WHERE ip = ? AND at > ? AND success = 0 AND username_hash NOT LIKE 'token:%'",
        )
        .bind(attempt.ip)
        .bind(&since)
        .fetch_one(&mut *tx)
        .await?;
        // Trusted device: its own bucket. Otherwise: the username bucket of
        // attempts without device cookie.
        let (sql, identity, limit) = if let Some(device) = attempt.device {
            (
                "SELECT COUNT(*) FROM login_attempt WHERE device = ? AND at > ? AND success = 0",
                device,
                MAX_FAILURES_PER_DEVICE,
            )
        } else {
            (
                "SELECT COUNT(*) FROM login_attempt
                 WHERE username_hash = ? AND device IS NULL AND at > ? AND success = 0",
                attempt.username_hash,
                MAX_FAILURES_PER_USERNAME,
            )
        };
        let (by_identity,): (i64,) = sqlx::query_as(sql)
            .bind(identity)
            .bind(&since)
            .fetch_one(&mut *tx)
            .await?;
        let over_identity = by_identity > limit;
        if over_identity || by_ip > MAX_FAILURES_PER_IP {
            tx.rollback().await?;
            return Ok(LoginAttempt::Throttled);
        }
        tx.commit().await?;
        Ok(LoginAttempt::Allowed(id))
    }

    /// Register a use of a public token endpoint (invitation, password link)
    /// as a provisional failure, unless over a limit: per token key and per
    /// IP, counted separately from logins so that failed token lookups (or a
    /// foreign page embedding token URLs) cannot lock anyone out of logging
    /// in. `token_key` is a hash of the token; it gets a `token:` prefix.
    pub async fn begin_token_attempt(
        &self,
        token_key: &str,
        ip: &str,
        now: DateTime<Utc>,
    ) -> Result<LoginAttempt, DbError> {
        let key = format!("token:{token_key}");
        let since = ts(now - THROTTLE_WINDOW);
        let mut tx = self.begin_write().await?;
        let id = sqlx::query(
            "INSERT INTO login_attempt (username_hash, ip, at, success) VALUES (?, ?, ?, 0)",
        )
        .bind(&key)
        .bind(ip)
        .bind(ts(now))
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
        let (by_ip, by_token): (i64, i64) = sqlx::query_as(
            "SELECT
                 (SELECT COUNT(*) FROM login_attempt
                  WHERE ip = ? AND at > ? AND success = 0 AND username_hash LIKE 'token:%'),
                 (SELECT COUNT(*) FROM login_attempt
                  WHERE username_hash = ? AND at > ? AND success = 0)",
        )
        .bind(ip)
        .bind(&since)
        .bind(&key)
        .bind(&since)
        .fetch_one(&mut *tx)
        .await?;
        if by_ip > MAX_TOKEN_FAILURES_PER_IP || by_token > MAX_FAILURES_PER_TOKEN {
            tx.rollback().await?;
            return Ok(LoginAttempt::Throttled);
        }
        tx.commit().await?;
        Ok(LoginAttempt::Allowed(id))
    }

    /// Value of an `app_meta` entry, created with `initial` if missing (the
    /// first writer wins when several processes race).
    pub async fn meta_get_or_insert(&self, key: &str, initial: &str) -> Result<String, DbError> {
        sqlx::query("INSERT OR IGNORE INTO app_meta (key, value) VALUES (?, ?)")
            .bind(key)
            .bind(initial)
            .execute(&self.pool)
            .await?;
        let (value,): (String,) = sqlx::query_as("SELECT value FROM app_meta WHERE key = ?")
            .bind(key)
            .fetch_one(&self.pool)
            .await?;
        Ok(value)
    }

    /// Insert or replace an `app_meta` value.
    pub async fn meta_set(&self, key: &str, value: &str) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO app_meta (key, value) VALUES (?, ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// An `app_meta` value, if set.
    pub async fn meta_get(&self, key: &str) -> Result<Option<String>, DbError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM app_meta WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(v,)| v))
    }

    /// Mark an allowed attempt as successful (failures need no update).
    pub async fn finish_login_attempt(&self, id: i64, success: bool) -> Result<(), DbError> {
        if success {
            sqlx::query("UPDATE login_attempt SET success = 1 WHERE id = ?")
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Forget login attempts older than `before`.
    pub async fn prune_login_attempts(&self, before: DateTime<Utc>) -> Result<u64, DbError> {
        let result = sqlx::query("DELETE FROM login_attempt WHERE at < ?")
            .bind(ts(before))
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

fn credentials(kind: SubjectKind, row: SubjectRow) -> Credentials {
    let (id, username, display_name, password_hash, active, totp_enabled) = row;
    Credentials {
        subject: Subject {
            kind,
            id,
            username,
            display_name,
        },
        password_hash,
        active,
        totp_enabled,
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    const ADMIN: NewAdmin<'static> = NewAdmin {
        username: "root",
        display_name: "Root",
        password_hash: "$argon2id$dummy",
    };

    async fn insert_member(db: &Database, username: &str) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO member (username, display_name, password_hash, created_at)
             VALUES (?, ?, 'hash', '2026-11-01T00:00:00.000Z')",
        )
        .bind(username)
        .bind(username)
        .execute(db.pool())
        .await
        .map_err(username_conflict)?;
        Ok(())
    }

    /// Suspend `root`, keeping another admin active (one always remains).
    async fn suspend_root(db: &Database) {
        sqlx::query(
            "INSERT INTO admin_account (username, display_name, password_hash, created_at)
             VALUES ('spare', 'Spare', 'hash', '2026-11-01T00:00:00.000Z')",
        )
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query("UPDATE admin_account SET suspended_at = 'x' WHERE username = 'root'")
            .execute(db.pool())
            .await
            .unwrap();
    }

    fn key<'a>(username: &'a str, ip: &'a str, device: Option<&'a str>) -> AttemptKey<'a> {
        AttemptKey {
            username_hash: username,
            ip,
            device,
        }
    }

    #[tokio::test]
    async fn device_cookie_attempts_bypass_the_username_limit_but_have_their_own() {
        let db = Database::open_in_memory().await.unwrap();
        for i in 0..5 {
            let ip = format!("198.51.100.{i}");
            assert!(matches!(
                db.begin_login_attempt(&key("root", &ip, None), t0())
                    .await
                    .unwrap(),
                LoginAttempt::Allowed(_)
            ));
        }
        // Username locked for untrusted attempts ...
        assert_eq!(
            db.begin_login_attempt(&key("root", "198.51.100.50", None), t0())
                .await
                .unwrap(),
            LoginAttempt::Throttled
        );
        // ... but the owner's device still gets its own five tries.
        for _ in 0..5 {
            assert!(matches!(
                db.begin_login_attempt(&key("root", "203.0.113.5", Some("dev1")), t0())
                    .await
                    .unwrap(),
                LoginAttempt::Allowed(_)
            ));
        }
        assert_eq!(
            db.begin_login_attempt(&key("root", "203.0.113.6", Some("dev1")), t0())
                .await
                .unwrap(),
            LoginAttempt::Throttled
        );
        // Device failures do not feed the untrusted username bucket, and vice versa.
        assert!(matches!(
            db.begin_login_attempt(&key("root", "203.0.113.7", Some("dev2")), t0())
                .await
                .unwrap(),
            LoginAttempt::Allowed(_)
        ));
    }

    #[tokio::test]
    async fn token_attempts_have_their_own_buckets() {
        let db = Database::open_in_memory().await.unwrap();
        // Five failures for one token, then that token is throttled ...
        for _ in 0..5 {
            assert!(matches!(
                db.begin_token_attempt("t1", "203.0.113.1", t0())
                    .await
                    .unwrap(),
                LoginAttempt::Allowed(_)
            ));
        }
        assert_eq!(
            db.begin_token_attempt("t1", "203.0.113.2", t0())
                .await
                .unwrap(),
            LoginAttempt::Throttled
        );
        // ... 20 failures per IP across tokens ...
        for i in 0..15 {
            assert!(matches!(
                db.begin_token_attempt(&format!("x{i}"), "203.0.113.1", t0())
                    .await
                    .unwrap(),
                LoginAttempt::Allowed(_)
            ));
        }
        assert_eq!(
            db.begin_token_attempt("y", "203.0.113.1", t0())
                .await
                .unwrap(),
            LoginAttempt::Throttled
        );
        // ... and none of it counts against logins from that IP.
        assert!(matches!(
            db.begin_login_attempt(&key("root", "203.0.113.1", None), t0())
                .await
                .unwrap(),
            LoginAttempt::Allowed(_)
        ));
    }

    #[tokio::test]
    async fn meta_values_are_created_once() {
        let db = Database::open_in_memory().await.unwrap();
        assert_eq!(db.meta_get_or_insert("k", "first").await.unwrap(), "first");
        assert_eq!(db.meta_get_or_insert("k", "second").await.unwrap(), "first");
        db.meta_set("k", "third").await.unwrap();
        assert_eq!(db.meta_get("k").await.unwrap().as_deref(), Some("third"));
        assert_eq!(db.meta_get("missing").await.unwrap(), None);
    }

    #[test]
    fn timestamps_are_fixed_width() {
        assert_eq!(ts(t0()), "2026-11-01T12:00:00.000Z");
        assert_eq!(parse_ts(&ts(t0())).unwrap(), t0());
    }

    #[tokio::test]
    async fn first_admin_is_created_only_once() {
        let db = Database::open_in_memory().await.unwrap();
        assert!(!db.admin_exists().await.unwrap());
        let id = db.create_first_admin(&ADMIN, t0()).await.unwrap();
        assert!(id.is_some());
        assert!(db.admin_exists().await.unwrap());
        let second = NewAdmin {
            username: "other",
            ..ADMIN
        };
        assert_eq!(db.create_first_admin(&second, t0()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn usernames_are_unique_across_admins_and_members() {
        let db = Database::open_in_memory().await.unwrap();
        db.create_first_admin(&ADMIN, t0()).await.unwrap();
        assert!(matches!(
            insert_member(&db, "ROOT").await,
            Err(DbError::UsernameTaken)
        ));

        let db = Database::open_in_memory().await.unwrap();
        insert_member(&db, "root").await.unwrap();
        assert!(matches!(
            db.create_first_admin(
                &NewAdmin {
                    username: "Root",
                    ..ADMIN
                },
                t0()
            )
            .await,
            Err(DbError::UsernameTaken)
        ));
        assert!(matches!(
            insert_member(&db, "Root").await,
            Err(DbError::UsernameTaken)
        ));
    }

    #[tokio::test]
    async fn credentials_cover_admins_and_members() {
        let db = Database::open_in_memory().await.unwrap();
        db.create_first_admin(&ADMIN, t0()).await.unwrap();
        insert_member(&db, "mia").await.unwrap();

        let admin = db.find_credentials("root").await.unwrap().unwrap();
        assert_eq!(admin.subject.kind, SubjectKind::Admin);
        assert!(admin.active);
        let member = db.find_credentials("mia").await.unwrap().unwrap();
        assert_eq!(member.subject.kind, SubjectKind::Member);
        assert!(db.find_credentials("nobody").await.unwrap().is_none());

        suspend_root(&db).await;
        assert!(!db.find_credentials("root").await.unwrap().unwrap().active);
    }

    #[tokio::test]
    async fn sessions_round_trip_and_hide_inactive_subjects() {
        let db = Database::open_in_memory().await.unwrap();
        let id = db.create_first_admin(&ADMIN, t0()).await.unwrap().unwrap();
        let hash = [7u8; 32];
        let new = NewSession {
            token_hash: &hash,
            subject_kind: SubjectKind::Admin,
            subject_id: id,
            expires_at: t0() + TimeDelta::days(30),
            ip: "203.0.113.9",
            user_agent: Some("test"),
        };
        db.create_session(&new, t0()).await.unwrap();

        let found = db.find_session(&hash).await.unwrap().unwrap();
        assert_eq!(found.subject.username, "root");
        assert_eq!(found.subject.kind, SubjectKind::Admin);
        assert_eq!(found.last_seen_at, t0());
        assert_eq!(found.expires_at, t0() + TimeDelta::days(30));

        db.touch_session(&hash, t0() + TimeDelta::hours(1))
            .await
            .unwrap();
        let found = db.find_session(&hash).await.unwrap().unwrap();
        assert_eq!(found.last_seen_at, t0() + TimeDelta::hours(1));

        suspend_root(&db).await;
        assert!(db.find_session(&hash).await.unwrap().is_none());

        db.delete_session(&hash).await.unwrap();
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn private_unlock_is_per_session_extends_only_while_open_and_not_rotated() {
        let db = Database::open_in_memory().await.unwrap();
        let id = db.create_first_admin(&ADMIN, t0()).await.unwrap().unwrap();
        let (one, two) = ([1u8; 32], [2u8; 32]);
        for hash in [&one, &two] {
            let new = NewSession {
                token_hash: hash,
                subject_kind: SubjectKind::Admin,
                subject_id: id,
                expires_at: t0() + TimeDelta::days(30),
                ip: "203.0.113.9",
                user_agent: None,
            };
            db.create_session(&new, t0()).await.unwrap();
        }
        let until = |m| t0() + TimeDelta::minutes(m);
        let unlocked = async |db: &Database, hash: &[u8]| {
            db.find_session(hash)
                .await
                .unwrap()
                .unwrap()
                .private_unlocked_until
        };
        assert_eq!(unlocked(&db, &one).await, None);
        assert!(db.set_private_unlock(&one, Some(until(30))).await.unwrap());
        assert_eq!(unlocked(&db, &one).await, Some(until(30)));
        // The other session of the same admin stays locked.
        assert_eq!(unlocked(&db, &two).await, None);

        // Activity moves an open unlock on, never reopens a closed one.
        db.extend_private_unlock(&one, until(10), until(40))
            .await
            .unwrap();
        assert_eq!(unlocked(&db, &one).await, Some(until(40)));
        db.extend_private_unlock(&one, until(41), until(71))
            .await
            .unwrap();
        assert_eq!(unlocked(&db, &one).await, Some(until(40)));
        db.extend_private_unlock(&two, until(0), until(30))
            .await
            .unwrap();
        assert_eq!(unlocked(&db, &two).await, None);

        // A rotated session starts locked.
        db.set_private_unlock(&one, Some(until(60))).await.unwrap();
        let rotated = [3u8; 32];
        assert!(
            db.rotate_session(
                &one,
                &RotatedSession {
                    token_hash: &rotated,
                    subject_kind: SubjectKind::Admin,
                    subject_id: id,
                    origin_admin_id: None,
                    elevated_until: None,
                },
                t0(),
            )
            .await
            .unwrap()
        );
        assert_eq!(unlocked(&db, &rotated).await, None);

        db.set_private_unlock(&two, Some(until(30))).await.unwrap();
        assert!(db.set_private_unlock(&two, None).await.unwrap());
        assert_eq!(unlocked(&db, &two).await, None);
        assert!(!db.set_private_unlock(&[9u8; 32], None).await.unwrap());
    }

    #[tokio::test]
    async fn username_throttle_allows_five_failures_in_window() {
        let db = Database::open_in_memory().await.unwrap();
        for i in 0..5 {
            let ip = format!("198.51.100.{i}");
            assert!(matches!(
                db.begin_login_attempt(&key("root", &ip, None), t0())
                    .await
                    .unwrap(),
                LoginAttempt::Allowed(_)
            ));
        }
        assert_eq!(
            db.begin_login_attempt(&key("root", "198.51.100.99", None), t0())
                .await
                .unwrap(),
            LoginAttempt::Throttled
        );
        // Throttled attempts are not recorded; the window slides.
        let later = t0() + THROTTLE_WINDOW;
        assert!(matches!(
            db.begin_login_attempt(&key("root", "198.51.100.99", None), later)
                .await
                .unwrap(),
            LoginAttempt::Allowed(_)
        ));
    }

    #[tokio::test]
    async fn ip_throttle_allows_twenty_failures_in_window() {
        let db = Database::open_in_memory().await.unwrap();
        for i in 0..20 {
            assert!(matches!(
                db.begin_login_attempt(&key(&format!("user{i}"), "203.0.113.1", None), t0())
                    .await
                    .unwrap(),
                LoginAttempt::Allowed(_)
            ));
        }
        assert_eq!(
            db.begin_login_attempt(&key("fresh", "203.0.113.1", None), t0())
                .await
                .unwrap(),
            LoginAttempt::Throttled
        );
        assert!(matches!(
            db.begin_login_attempt(&key("fresh", "203.0.113.2", None), t0())
                .await
                .unwrap(),
            LoginAttempt::Allowed(_)
        ));
    }

    #[tokio::test]
    async fn successful_attempts_do_not_count_as_failures() {
        let db = Database::open_in_memory().await.unwrap();
        for _ in 0..10 {
            let LoginAttempt::Allowed(id) = db
                .begin_login_attempt(&key("root", "203.0.113.1", None), t0())
                .await
                .unwrap()
            else {
                panic!("throttled");
            };
            db.finish_login_attempt(id, true).await.unwrap();
        }
        assert!(matches!(
            db.begin_login_attempt(&key("root", "203.0.113.1", None), t0())
                .await
                .unwrap(),
            LoginAttempt::Allowed(_)
        ));
    }
}
