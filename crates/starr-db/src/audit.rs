//! Audit log. Each entry is written on its own (autocommit) so it survives a
//! failed or rolled-back operation.

use chrono::{DateTime, Utc};

use crate::{Database, DbError, ts};

/// Who performed an audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    Admin,
    Member,
    System,
    /// Not authenticated (e.g. a failed login).
    Anonymous,
}

impl ActorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
            Self::System => "system",
            Self::Anonymous => "anonymous",
        }
    }
}

/// One audit log entry. `details` is a JSON object and must never contain secrets.
#[derive(Debug)]
pub struct AuditEntry<'a> {
    pub actor_kind: ActorKind,
    pub actor_name: Option<&'a str>,
    pub action: &'a str,
    pub target: Option<&'a str>,
    pub details: Option<&'a str>,
}

/// A stored audit entry as read back.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct AuditRecord {
    pub at: String,
    pub actor_kind: String,
    pub actor_name: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub details: Option<String>,
}

impl Database {
    /// Append an entry to the audit log.
    pub async fn audit(&self, entry: &AuditEntry<'_>, now: DateTime<Utc>) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO audit_log (at, actor_kind, actor_name, action, target, details)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(ts(now))
        .bind(entry.actor_kind.as_str())
        .bind(entry.actor_name)
        .bind(entry.action)
        .bind(entry.target)
        .bind(entry.details)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The most recent audit entries, newest first.
    pub async fn recent_audit(&self, limit: i64) -> Result<Vec<AuditRecord>, DbError> {
        Ok(sqlx::query_as(
            "SELECT at, actor_kind, actor_name, action, target, details
             FROM audit_log ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn entries_are_appended_and_read_back() {
        let db = Database::open_in_memory().await.unwrap();
        let now = Utc::now();
        db.audit(
            &AuditEntry {
                actor_kind: ActorKind::Anonymous,
                actor_name: None,
                action: "auth.login_failed",
                target: Some("root"),
                details: Some(r#"{"ip":"203.0.113.1"}"#),
            },
            now,
        )
        .await
        .unwrap();
        let entries = db.recent_audit(10).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "auth.login_failed");
        assert_eq!(entries[0].actor_kind, "anonymous");
        assert_eq!(entries[0].target.as_deref(), Some("root"));
    }
}
