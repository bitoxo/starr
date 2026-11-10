//! Invitation links (ADR 0006). Only token hashes are stored.

use chrono::{DateTime, Utc};
use starr_core::members::Entitlement;

use crate::members::{NewMember, insert_member};
use crate::service_accounts::PresetAccount;
use crate::{Database, DbError, parse_ts, ts};

/// Data for a new invitation.
#[derive(Debug)]
pub struct NewInvitation<'a> {
    pub token_hash: &'a [u8],
    pub created_by: i64,
    pub expires_at: DateTime<Utc>,
    pub preset_username: Option<&'a str>,
    pub preset_email: Option<&'a str>,
    pub preset_entitlements: &'a [Entitlement],
    /// Existing accounts the new member is linked to on acceptance (M3).
    pub preset_accounts: &'a [PresetAccount],
}

/// A stored invitation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvitationRecord {
    pub id: i64,
    /// Username of the creating admin (None if that admin row is gone).
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    /// Username of the member who accepted it.
    pub used_by: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub preset_username: Option<String>,
    pub preset_email: Option<String>,
    pub preset_entitlements: Vec<Entitlement>,
    pub preset_accounts: Vec<PresetAccount>,
}

type InvitationRow = (
    i64,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

const INVITATION_SELECT: &str = "
    SELECT i.id, a.username, i.created_at, i.expires_at, i.used_at, m.username,
           i.revoked_at, i.preset_username, i.preset_email, i.preset_entitlements,
           i.preset_accounts
    FROM invitation i
    LEFT JOIN admin_account a ON a.id = i.created_by
    LEFT JOIN member m ON m.id = i.used_by_member_id";

fn opt_ts(value: Option<&str>) -> Result<Option<DateTime<Utc>>, DbError> {
    value.map(parse_ts).transpose()
}

fn invitation_record(row: InvitationRow) -> Result<InvitationRecord, DbError> {
    let (
        id,
        created_by,
        created,
        expires,
        used,
        used_by,
        revoked,
        username,
        email,
        presets,
        accounts,
    ) = row;
    Ok(InvitationRecord {
        id,
        created_by,
        created_at: parse_ts(&created)?,
        expires_at: parse_ts(&expires)?,
        used_at: opt_ts(used.as_deref())?,
        used_by,
        revoked_at: opt_ts(revoked.as_deref())?,
        preset_username: username,
        preset_email: email,
        preset_entitlements: serde_json::from_str(&presets)?,
        preset_accounts: accounts
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default(),
    })
}

/// Result of accepting an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acceptance {
    /// The member was created with this id.
    Created(i64),
    /// The invitation was used, revoked or expired in the meantime.
    Gone,
}

impl Database {
    /// Store a new invitation and return its id.
    pub async fn create_invitation(
        &self,
        invitation: &NewInvitation<'_>,
        now: DateTime<Utc>,
    ) -> Result<i64, DbError> {
        let presets = serde_json::to_string(invitation.preset_entitlements)?;
        let accounts = if invitation.preset_accounts.is_empty() {
            None
        } else {
            Some(serde_json::to_string(invitation.preset_accounts)?)
        };
        let result = sqlx::query(
            "INSERT INTO invitation
                (token_hash, created_by, created_at, expires_at, preset_username, preset_email,
                 preset_entitlements, preset_accounts)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(invitation.token_hash)
        .bind(invitation.created_by)
        .bind(ts(now))
        .bind(ts(invitation.expires_at))
        .bind(invitation.preset_username)
        .bind(invitation.preset_email)
        .bind(presets)
        .bind(accounts)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    /// All invitations, newest first.
    pub async fn list_invitations(&self) -> Result<Vec<InvitationRecord>, DbError> {
        let sql = format!("{INVITATION_SELECT} ORDER BY i.id DESC");
        let rows: Vec<InvitationRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(invitation_record).collect()
    }

    /// One invitation by id.
    pub async fn get_invitation(&self, id: i64) -> Result<Option<InvitationRecord>, DbError> {
        let sql = format!("{INVITATION_SELECT} WHERE i.id = ?");
        let row: Option<InvitationRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(invitation_record).transpose()
    }

    /// One invitation by token hash.
    pub async fn find_invitation(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<InvitationRecord>, DbError> {
        let sql = format!("{INVITATION_SELECT} WHERE i.token_hash = ?");
        let row: Option<InvitationRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(token_hash)
            .fetch_optional(&self.pool)
            .await?;
        row.map(invitation_record).transpose()
    }

    /// Revoke an invitation that is neither used nor revoked. False otherwise.
    pub async fn revoke_invitation(&self, id: i64, now: DateTime<Utc>) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE invitation SET revoked_at = ?
             WHERE id = ? AND used_at IS NULL AND revoked_at IS NULL",
        )
        .bind(ts(now))
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Accept an invitation: create the member with its entitlements and mark
    /// the invitation used, all or nothing. Concurrent accepts of one link
    /// create at most one member.
    pub async fn accept_invitation(
        &self,
        id: i64,
        member: &NewMember<'_>,
        entitlements: &[Entitlement],
        now: DateTime<Utc>,
    ) -> Result<Acceptance, DbError> {
        let mut tx = self.begin_write().await?;
        let claimed = sqlx::query(
            "UPDATE invitation SET used_at = ?
             WHERE id = ? AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?",
        )
        .bind(ts(now))
        .bind(id)
        .bind(ts(now))
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !claimed {
            tx.rollback().await?;
            return Ok(Acceptance::Gone);
        }
        let member_id = insert_member(&mut tx, member, entitlements, now).await?;
        sqlx::query("UPDATE invitation SET used_by_member_id = ? WHERE id = ?")
            .bind(member_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Acceptance::Created(member_id))
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use starr_core::members::{Category, RequestMode};

    use super::*;
    use crate::auth::NewAdmin;
    use crate::test_support::t0;

    async fn db_with_admin() -> (Database, i64) {
        let db = Database::open_in_memory().await.unwrap();
        let admin = db
            .create_first_admin(
                &NewAdmin {
                    username: "root",
                    display_name: "Root",
                    password_hash: "x",
                },
                t0(),
            )
            .await
            .unwrap()
            .unwrap();
        (db, admin)
    }

    fn member<'a>(username: &'a str, email: &'a str) -> NewMember<'a> {
        NewMember {
            username,
            email,
            display_name: username,
            password_hash: Some("hash"),
        }
    }

    #[tokio::test]
    async fn accepting_creates_the_member_once() {
        let (db, admin) = db_with_admin().await;
        let presets = [Entitlement {
            category: Category::Series,
            enabled: true,
            request_mode: RequestMode::Auto,
        }];
        let accounts = [PresetAccount {
            service_id: 4,
            remote_id: "r-mia".to_owned(),
            username: Some("Mia".to_owned()),
            confirmed: true,
        }];
        let id = db
            .create_invitation(
                &NewInvitation {
                    token_hash: &[1; 32],
                    created_by: admin,
                    expires_at: t0() + TimeDelta::days(7),
                    preset_username: Some("mia"),
                    preset_email: None,
                    preset_entitlements: &presets,
                    preset_accounts: &accounts,
                },
                t0(),
            )
            .await
            .unwrap();
        let found = db.find_invitation(&[1; 32]).await.unwrap().unwrap();
        assert_eq!(found.id, id);
        assert_eq!(found.created_by.as_deref(), Some("root"));
        assert_eq!(found.preset_entitlements, presets);
        assert_eq!(found.preset_accounts, accounts);

        let Acceptance::Created(mia) = db
            .accept_invitation(id, &member("mia", "mia@example.org"), &presets, t0())
            .await
            .unwrap()
        else {
            panic!("not accepted");
        };
        assert_eq!(db.entitlements(mia).await.unwrap()[1], presets[0]);
        assert_eq!(
            db.accept_invitation(id, &member("noah", "noah@example.org"), &[], t0())
                .await
                .unwrap(),
            Acceptance::Gone
        );
        let used = db.get_invitation(id).await.unwrap().unwrap();
        assert_eq!(used.used_by.as_deref(), Some("mia"));
        assert!(
            !db.revoke_invitation(id, t0()).await.unwrap(),
            "used is final"
        );
    }

    #[tokio::test]
    async fn revoked_and_expired_invitations_cannot_be_accepted() {
        let (db, admin) = db_with_admin().await;
        let new = |hash: &'static [u8]| NewInvitation {
            token_hash: hash,
            created_by: admin,
            expires_at: t0() + TimeDelta::days(1),
            preset_username: None,
            preset_email: None,
            preset_entitlements: &[],
            preset_accounts: &[],
        };
        let revoked = db.create_invitation(&new(&[1; 32]), t0()).await.unwrap();
        let expiring = db.create_invitation(&new(&[2; 32]), t0()).await.unwrap();
        assert!(db.revoke_invitation(revoked, t0()).await.unwrap());
        assert_eq!(
            db.accept_invitation(revoked, &member("mia", "m@example.org"), &[], t0())
                .await
                .unwrap(),
            Acceptance::Gone
        );
        let later = t0() + TimeDelta::days(1);
        assert_eq!(
            db.accept_invitation(expiring, &member("mia", "m@example.org"), &[], later)
                .await
                .unwrap(),
            Acceptance::Gone
        );
        assert!(!db.username_exists("mia").await.unwrap(), "rolled back");
        assert_eq!(db.list_invitations().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_failed_member_insert_leaves_the_invitation_unused() {
        let (db, admin) = db_with_admin().await;
        let id = db
            .create_invitation(
                &NewInvitation {
                    token_hash: &[1; 32],
                    created_by: admin,
                    expires_at: t0() + TimeDelta::days(1),
                    preset_username: None,
                    preset_email: None,
                    preset_entitlements: &[],
                    preset_accounts: &[],
                },
                t0(),
            )
            .await
            .unwrap();
        let err = db
            .accept_invitation(id, &member("root", "r@example.org"), &[], t0())
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::UsernameTaken), "{err:?}");
        assert!(
            db.get_invitation(id)
                .await
                .unwrap()
                .unwrap()
                .used_at
                .is_none()
        );
    }
}
