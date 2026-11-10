//! Operator recovery when every admin is locked out: `starr admin recover
//! <username> [--reset-totp] [--create]`, run inside the container with
//! `docker exec` (it shares the database with the running server).
//!
//! Reactivates the admin and returns a one-time set-password link. Without
//! `--reset-totp` the current password (plus TOTP) keeps working until the
//! link is used. With `--reset-totp` TOTP goes off, the password is replaced
//! by an unusable one and all sessions end: only the link restores access,
//! so a stolen password does not become enough on its own. A removed admin
//! (tombstone) is never revived: its username stays reserved. `--create`
//! makes a new admin only when no account has the name.

use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use starr_core::auth::parse_username;
use starr_core::members::PASSWORD_TOKEN_LIFETIME;
use starr_db::admins::{RecoverAdmin, Recovery};
use starr_db::audit::{ActorKind, AuditEntry};
use starr_db::auth::NewAdmin;
use starr_db::{Database, DbError};
use url::Url;

use crate::api::common::link_url;
use crate::auth::crypto;

/// What to recover.
#[derive(Debug, Clone)]
pub struct RecoverRequest {
    pub username: String,
    pub reset_totp: bool,
    pub create: bool,
}

/// A recovered (or created) admin and the link to hand over.
#[derive(Debug)]
pub struct Recovered {
    pub username: String,
    pub created: bool,
    /// One-time set-password link; absolute when `STARR_PUBLIC_URL` is set.
    pub link: SecretString,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecoverError {
    #[error("`{0}` is not a valid username")]
    InvalidUsername(String),
    #[error("no admin account `{0}`; pass --create to create it")]
    NotFound(String),
    #[error(
        "admin `{0}` was removed; the username stays reserved. Use --create with a different username"
    )]
    Tombstone(String),
    #[error("`{0}` is a member's username; choose another one")]
    UsernameTaken(String),
    #[error("password hashing failed: {0}")]
    Hash(#[from] crypto::HashError),
    #[error(transparent)]
    Db(DbError),
}

/// Run the recovery against `db`.
pub async fn recover_admin(
    db: &Database,
    public_url: Option<&Url>,
    request: &RecoverRequest,
    now: DateTime<Utc>,
) -> Result<Recovered, RecoverError> {
    let username = parse_username(&request.username)
        .map_err(|_| RecoverError::InvalidUsername(request.username.clone()))?;
    // Unusable until the link is used (hash of a discarded password).
    let placeholder = crypto::hash_password(&crypto::new_token())?;
    let token = crypto::new_token();
    let expires_at = now + PASSWORD_TOKEN_LIFETIME;
    let outcome = db
        .recover_admin(
            &RecoverAdmin {
                username: &username,
                reset_totp: request.reset_totp,
                unusable_password_hash: &placeholder,
                create: request.create.then_some(NewAdmin {
                    username: &username,
                    display_name: &username,
                    password_hash: &placeholder,
                }),
                token_hash: &crypto::hash_token(token.expose_secret()),
                token_expires_at: expires_at,
            },
            now,
        )
        .await
        .map_err(|err| match err {
            DbError::UsernameTaken => RecoverError::UsernameTaken(username.clone()),
            other => RecoverError::Db(other),
        })?;
    let created = match outcome {
        Recovery::Recovered { .. } => false,
        Recovery::Created { .. } => true,
        Recovery::NotFound => return Err(RecoverError::NotFound(username)),
        Recovery::Tombstone => return Err(RecoverError::Tombstone(username)),
    };
    let details = json!({ "resetTotp": request.reset_totp, "created": created }).to_string();
    db.audit(
        &AuditEntry {
            actor_kind: ActorKind::System,
            actor_name: Some("recovery-cli"),
            action: "admin.recovered",
            target: Some(&username),
            details: Some(&details),
        },
        now,
    )
    .await
    .map_err(RecoverError::Db)?;
    // TOTP reset or a new admin: live account writes pause (M10 1).
    starr_worker::provisioning::pause_for_totp_gap(
        db,
        (ActorKind::System, Some("recovery-cli")),
        "admin.recovered",
    )
    .await
    .map_err(RecoverError::Db)?;
    let link = link_url(public_url, crate::api::common::SET_PASSWORD_PATH, &token);
    Ok(Recovered {
        username,
        created,
        link: SecretString::from(link),
        expires_at,
    })
}
