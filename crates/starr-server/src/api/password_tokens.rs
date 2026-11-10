//! Set-password links: `GET /api/v1/password-tokens/{token}` checks one,
//! `POST` uses it (both public and rate-limited; single use). Issued by admins for new members, new admins
//! and resets, and by `starr admin recover`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::CookieJar;
use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::auth::{Subject, SubjectKind};
use starr_core::members::MemberStatus;
use starr_db::members::{MemberTokenUse, PasswordTokenRecord, TokenUse};
use utoipa::ToSchema;

use super::auth::{MeResponse, TotpChallenge, me_response, totp_challenge};
use super::common;
use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::{Client, crypto, start_session};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetPasswordRequest {
    #[schema(value_type = String, format = Password)]
    password: SecretString,
}

/// A set-password link that can still be used.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasswordTokenPreview {
    expires_at: DateTime<Utc>,
}

/// Check a set-password link before choosing a password (spec 0008
/// AUTH-03): the same answers as using it, without using it, and nothing
/// about whose link it is. Public, rate-limited like using it.
#[utoipa::path(get, path = "/api/v1/password-tokens/{token}", tag = "auth",
    params(("token" = String, Path)),
    responses(
        (status = 200, description = "The link can be used", body = PasswordTokenPreview),
        (status = 403, description = "`auth.forbidden` (cross-site request)", body = ErrorBody),
        (status = 404, description = "`password_token.not_found`", body = ErrorBody),
        (status = 409, description = "`password_token.used`, `password_token.expired`, `password_token.account_inactive`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn check_password_token(
    State(state): State<AppState>,
    client: Client,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Result<Json<PasswordTokenPreview>, ApiError> {
    common::refuse_cross_site(&headers)?;
    let attempt = common::begin_token_attempt(&state, &client, &token).await?;
    let record = state
        .db
        .find_password_token(&crypto::hash_token(&token))
        .await?
        .ok_or(ApiError::NotFound("password_token.not_found"))?;
    // A real token, even a used or expired one, is not guessing.
    common::token_attempt_succeeded(&state, attempt).await?;
    usable(&state, &record).await?;
    Ok(Json(PasswordTokenPreview {
        expires_at: record.expires_at,
    }))
}

/// Set a password with a one-time link and log in. All other sessions of
/// the account end. An admin with TOTP gets `202` and a pending token for
/// the second login step instead of a session: a link never bypasses TOTP.
#[utoipa::path(post, path = "/api/v1/password-tokens/{token}", tag = "auth",
    params(("token" = String, Path)),
    request_body = SetPasswordRequest,
    responses(
        (status = 200, description = "Password set; logged in, session cookie set", body = MeResponse),
        (status = 202, description = "Password set (admin with TOTP): continue with `POST /api/v1/auth/login/totp`", body = TotpChallenge),
        (status = 403, description = "`auth.origin_mismatch`", body = ErrorBody),
        (status = 404, description = "`password_token.not_found`", body = ErrorBody),
        (status = 409, description = "`password_token.used`, `password_token.expired`, `password_token.account_inactive`", body = ErrorBody),
        (status = 422, description = "`auth.password_too_short`, `auth.password_too_long`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn use_password_token(
    State(state): State<AppState>,
    client: Client,
    headers: HeaderMap,
    jar: CookieJar,
    Path(token): Path<String>,
    Json(body): Json<SetPasswordRequest>,
) -> Result<Response, ApiError> {
    let attempt = common::begin_token_attempt(&state, &client, &token).await?;
    let record = state
        .db
        .find_password_token(&crypto::hash_token(&token))
        .await?
        .ok_or(ApiError::NotFound("password_token.not_found"))?;
    common::token_attempt_succeeded(&state, attempt).await?;
    let (subject, totp_enabled) = usable(&state, &record).await?;

    common::new_password(&body.password)?;
    let (password_hash, password) = common::hash_keeping(&state, body.password).await?;
    let now = state.clock.now();
    let used = match subject.kind {
        SubjectKind::Member => {
            match state
                .db
                .use_password_token(record.id, subject.id, &password_hash, now)
                .await?
            {
                MemberTokenUse::Done { generation } => {
                    // Accounts using the Starr password get it (ADR 0010).
                    super::my_services::offer_after_set(&state, subject.id, generation, &password)
                        .await;
                    TokenUse::Done
                }
                MemberTokenUse::Rejected => TokenUse::Rejected,
            }
        }
        SubjectKind::Admin => {
            state
                .db
                .use_admin_password_token(record.id, subject.id, &password_hash, now)
                .await?
        }
    };
    drop(password);
    if used == TokenUse::Rejected {
        return Err(ApiError::Conflict("password_token.used"));
    }

    let action = match subject.kind {
        SubjectKind::Member => "member.password_set",
        SubjectKind::Admin => "admin.password_set",
    };
    common::audit(
        &state,
        &subject,
        action,
        None,
        Some(json!({ "purpose": record.purpose.as_str(), "ip": client.ip })),
    )
    .await?;
    if totp_enabled {
        let challenge = totp_challenge(&state, subject.id).await?;
        return Ok((StatusCode::ACCEPTED, Json(challenge)).into_response());
    }
    let jar = start_session(&state, jar, &headers, &client, &subject).await?;
    let me = me_response(&state, subject, None).await?;
    Ok((jar, Json(me)).into_response())
}

/// The account a link sets the password of, and whether it has TOTP: the
/// link must be unused and unexpired, the account active.
async fn usable(
    state: &AppState,
    record: &PasswordTokenRecord,
) -> Result<(Subject, bool), ApiError> {
    if record.used {
        return Err(ApiError::Conflict("password_token.used"));
    }
    if record.expires_at <= state.clock.now() {
        return Err(ApiError::Conflict("password_token.expired"));
    }
    match record.subject_kind {
        SubjectKind::Member => {
            let member = state
                .db
                .get_member(record.subject_id)
                .await?
                .filter(|m| m.status == MemberStatus::Active)
                .ok_or(ApiError::Conflict("password_token.account_inactive"))?;
            let subject = Subject {
                kind: SubjectKind::Member,
                id: member.id,
                username: member.username,
                display_name: member.display_name,
            };
            Ok((subject, false))
        }
        SubjectKind::Admin => {
            let admin = state
                .db
                .get_admin(record.subject_id)
                .await?
                .filter(|a| !a.suspended && !a.removed)
                .ok_or(ApiError::Conflict("password_token.account_inactive"))?;
            let subject = Subject {
                kind: SubjectKind::Admin,
                id: admin.id,
                username: admin.username,
                display_name: admin.display_name,
            };
            Ok((subject, admin.totp_enabled))
        }
    }
}
