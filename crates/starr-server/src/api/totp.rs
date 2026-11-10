//! TOTP for admins (ADR 0005): enroll, confirm, disable, status, and the
//! second-factor check shared by login, elevation and disabling.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum_extra::extract::cookie::CookieJar;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::auth::{SecondFactor, Subject, SubjectKind, parse_second_factor};
use starr_db::auth::{AttemptKey, LoginAttempt};
use utoipa::ToSchema;

use super::common;
use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::totp::{TotpSecret, hash_recovery_code, new_recovery_codes, qr_svg_base64};
use crate::auth::{
    AdminSession, Client, CookieScheme, NextSession, crypto, request, rotate_session,
};

/// Which second factor passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorKind {
    Totp,
    RecoveryCode,
}

impl FactorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Totp => "totp",
            Self::RecoveryCode => "recovery_code",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecondFactorCheck {
    Passed(FactorKind),
    /// Wrong, replayed or used-up code (audited).
    Failed,
}

/// Throttle key for code attempts of `username`: its own bucket next to the
/// password bucket, counted per IP together with logins.
fn code_attempt_key(username: &str) -> String {
    crypto::username_key(&format!("totp:{username}"))
}

/// Open the admin's confirmed TOTP secret.
async fn enabled_secret(state: &AppState, admin_id: i64) -> Result<Option<TotpSecret>, ApiError> {
    let Some(sealed) = state.db.totp_secrets(admin_id).await?.enabled else {
        return Ok(None);
    };
    let secret = state.totp_box().open(admin_id, &sealed);
    if secret.is_none() {
        tracing::error!(admin_id, "stored TOTP secret does not open");
        return Err(ApiError::Internal);
    }
    Ok(secret)
}

/// Check a TOTP code (each time step accepted once) or a recovery code
/// (each works once) of `admin`. Attempts are throttled like logins
/// (5 failures per admin, 20 per IP in 15 minutes); failures are audited.
pub async fn check_second_factor(
    state: &AppState,
    client: &Client,
    admin: &Subject,
    input: &SecretString,
) -> Result<SecondFactorCheck, ApiError> {
    let now = state.clock.now();
    let key = code_attempt_key(&admin.username);
    let attempt = AttemptKey {
        username_hash: &key,
        ip: &request::throttle_key(client.ip),
        device: None,
    };
    let LoginAttempt::Allowed(attempt) = state.db.begin_login_attempt(&attempt, now).await? else {
        tracing::warn!(ip = %client.ip, "second factor throttled");
        return Err(ApiError::TooManyAttempts);
    };

    let mut replay = false;
    let passed = match parse_second_factor(input.expose_secret()) {
        Some(SecondFactor::Totp(code)) => match enabled_secret(state, admin.id).await? {
            Some(secret) => match secret.matching_step(&code, now) {
                Some(step) => {
                    let fresh = state.db.accept_totp_step(admin.id, step).await?;
                    replay = !fresh;
                    fresh.then_some(FactorKind::Totp)
                }
                None => None,
            },
            None => None,
        },
        Some(SecondFactor::RecoveryCode(code)) => state
            .db
            .use_recovery_code(admin.id, &hash_recovery_code(admin.id, &code), now)
            .await?
            .then_some(FactorKind::RecoveryCode),
        None => None,
    };

    let Some(factor) = passed else {
        common::audit(
            state,
            admin,
            "auth.second_factor_failed",
            None,
            Some(json!({ "ip": client.ip, "replay": replay })),
        )
        .await?;
        return Ok(SecondFactorCheck::Failed);
    };
    state.db.finish_login_attempt(attempt, true).await?;
    if factor == FactorKind::RecoveryCode {
        let remaining = state.db.recovery_codes_remaining(admin.id).await?;
        common::audit(
            state,
            admin,
            "auth.recovery_code_used",
            None,
            Some(json!({ "ip": client.ip, "remaining": remaining })),
        )
        .await?;
    }
    Ok(SecondFactorCheck::Passed(factor))
}

/// The admin's TOTP state.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TotpStatus {
    enabled: bool,
    /// `STARR_ADMIN_TOTP_REQUIRED` is set.
    required: bool,
    /// Unused recovery codes (0 while TOTP is off).
    recovery_codes_remaining: i64,
}

/// TOTP state of the logged-in admin.
#[utoipa::path(get, path = "/api/v1/auth/totp", tag = "auth",
    responses(
        (status = 200, body = TotpStatus),
        (status = 403, description = "`auth.forbidden`", body = ErrorBody),
    ))]
pub async fn totp_status(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
) -> Result<Json<TotpStatus>, ApiError> {
    Ok(Json(TotpStatus {
        enabled: session.totp_enabled,
        required: state.settings.admin_totp_required,
        recovery_codes_remaining: state
            .db
            .recovery_codes_remaining(session.subject.id)
            .await?,
    }))
}

/// A new TOTP secret to add to an authenticator app. Shown once.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TotpEnrollment {
    /// Base32, for typing into the app by hand.
    secret: String,
    /// `otpauth://totp/...` URI (what the QR code contains).
    otpauth_uri: String,
    /// The QR code as an SVG image, base64 (for a `data:image/svg+xml` URL).
    qr_svg: String,
}

/// Start TOTP enrollment: a fresh secret (replacing an unconfirmed one).
/// TOTP is on only after `POST /api/v1/auth/totp/confirm`.
#[utoipa::path(post, path = "/api/v1/auth/totp/enroll", tag = "auth",
    responses(
        (status = 200, body = TotpEnrollment),
        (status = 403, description = "`auth.forbidden`", body = ErrorBody),
        (status = 409, description = "`totp.already_enabled`", body = ErrorBody),
    ))]
pub async fn enroll(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
) -> Result<Json<TotpEnrollment>, ApiError> {
    let admin = &session.subject;
    let secret = TotpSecret::generate();
    let sealed = state
        .totp_box()
        .seal(admin.id, &secret)
        .ok_or(ApiError::Internal)?;
    if !state.db.set_totp_pending(admin.id, &sealed).await? {
        return Err(ApiError::Conflict("totp.already_enabled"));
    }
    let uri = secret
        .otpauth_uri(&admin.username)
        .ok_or(ApiError::Internal)?;
    let qr_svg = qr_svg_base64(&uri).ok_or(ApiError::Internal)?;
    common::audit(&state, admin, "auth.totp_enrollment_started", None, None).await?;
    Ok(Json(TotpEnrollment {
        secret: secret.base32().expose_secret().to_owned(),
        otpauth_uri: uri.expose_secret().to_owned(),
        qr_svg,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmTotpRequest {
    /// Current six-digit code from the app.
    #[schema(value_type = String, format = Password)]
    code: SecretString,
}

/// One-time recovery codes. Shown once; only hashes are stored.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCodes {
    codes: Vec<String>,
}

/// Confirm enrollment with a code from the app: TOTP is on from now on.
/// All other sessions of the admin end (they never proved the second
/// factor) and the current one is rotated. Returns ten recovery codes, each
/// usable once instead of a code.
#[utoipa::path(post, path = "/api/v1/auth/totp/confirm", tag = "auth",
    request_body = ConfirmTotpRequest,
    responses(
        (status = 200, description = "TOTP on; new session cookie set", body = RecoveryCodes),
        (status = 403, description = "`auth.forbidden`", body = ErrorBody),
        (status = 409, description = "`totp.no_enrollment` (enroll first)", body = ErrorBody),
        (status = 422, description = "`auth.invalid_code`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn confirm(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    client: Client,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<ConfirmTotpRequest>,
) -> Result<(CookieJar, Json<RecoveryCodes>), ApiError> {
    let admin = &session.subject;
    let now = state.clock.now();
    let Some(pending) = state.db.totp_secrets(admin.id).await?.pending else {
        return Err(ApiError::Conflict("totp.no_enrollment"));
    };
    let secret = state
        .totp_box()
        .open(admin.id, &pending)
        .ok_or(ApiError::Internal)?;

    let key = code_attempt_key(&admin.username);
    let attempt = AttemptKey {
        username_hash: &key,
        ip: &request::throttle_key(client.ip),
        device: None,
    };
    let LoginAttempt::Allowed(attempt) = state.db.begin_login_attempt(&attempt, now).await? else {
        return Err(ApiError::TooManyAttempts);
    };
    let step = match parse_second_factor(body.code.expose_secret()) {
        Some(SecondFactor::Totp(code)) => secret.matching_step(&code, now),
        _ => None,
    };
    let Some(step) = step else {
        common::audit(
            &state,
            admin,
            "auth.second_factor_failed",
            None,
            Some(json!({ "ip": client.ip, "during": "enrollment" })),
        )
        .await?;
        return Err(ApiError::Validation("auth.invalid_code"));
    };
    state.db.finish_login_attempt(attempt, true).await?;

    let (codes, hashes) = new_recovery_codes(admin.id);
    if !state
        .db
        .enable_totp(admin.id, &pending, step, &hashes, now)
        .await?
    {
        return Err(ApiError::Conflict("totp.no_enrollment"));
    }
    let ended = state
        .db
        .delete_other_admin_sessions(admin.id, &session.token_hash)
        .await?;
    let scheme = CookieScheme::for_request(&headers, state.settings.behind_proxy);
    let jar = rotate_session(
        &state,
        jar,
        scheme,
        &session,
        NextSession {
            kind: SubjectKind::Admin,
            id: admin.id,
            origin_admin_id: None,
            elevated_until: None,
        },
    )
    .await?;
    common::audit(
        &state,
        admin,
        "auth.totp_enabled",
        None,
        Some(json!({ "ip": client.ip, "sessionsEnded": ended })),
    )
    .await?;
    Ok((jar, Json(display_codes(&codes))))
}

fn display_codes(codes: &[SecretString]) -> RecoveryCodes {
    RecoveryCodes {
        codes: codes.iter().map(|c| c.expose_secret().to_owned()).collect(),
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegenerateRecoveryCodesRequest {
    #[schema(value_type = String, format = Password)]
    password: SecretString,
    /// Current six-digit TOTP code (not a recovery code).
    #[schema(value_type = String, format = Password)]
    code: SecretString,
}

/// Replace all recovery codes with ten new ones (the old ones stop working).
/// Needs the password and a current TOTP code. Shown once.
#[utoipa::path(post, path = "/api/v1/auth/totp/recovery-codes", tag = "auth",
    request_body = RegenerateRecoveryCodesRequest,
    responses(
        (status = 200, body = RecoveryCodes),
        (status = 403, description = "`auth.forbidden`", body = ErrorBody),
        (status = 409, description = "`totp.not_enabled`", body = ErrorBody),
        (status = 422, description = "`auth.current_password_invalid`, `auth.invalid_code`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn regenerate_recovery_codes(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    client: Client,
    Json(body): Json<RegenerateRecoveryCodesRequest>,
) -> Result<Json<RecoveryCodes>, ApiError> {
    let admin = &session.subject;
    if !session.totp_enabled {
        return Err(ApiError::Conflict("totp.not_enabled"));
    }
    let hash = state.db.admin_password_hash(admin.id).await?;
    if !common::verify_password(&state, &client, &admin.username, hash, body.password).await? {
        return Err(ApiError::Validation("auth.current_password_invalid"));
    }
    // Only a TOTP code: a recovery code here would be spent on replacing itself.
    if !matches!(
        parse_second_factor(body.code.expose_secret()),
        Some(SecondFactor::Totp(_))
    ) || check_second_factor(&state, &client, admin, &body.code).await?
        == SecondFactorCheck::Failed
    {
        return Err(ApiError::Validation("auth.invalid_code"));
    }
    let now = state.clock.now();
    let (codes, hashes) = new_recovery_codes(admin.id);
    if !state
        .db
        .regenerate_recovery_codes(admin.id, &hashes, now)
        .await?
    {
        return Err(ApiError::Conflict("totp.not_enabled"));
    }
    common::audit(
        &state,
        admin,
        "auth.recovery_codes_regenerated",
        None,
        Some(json!({ "ip": client.ip })),
    )
    .await?;
    Ok(Json(display_codes(&codes)))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DisableTotpRequest {
    #[schema(value_type = String, format = Password)]
    password: SecretString,
    /// TOTP code or a recovery code.
    #[schema(value_type = String, format = Password)]
    code: SecretString,
}

/// Turn TOTP off. Needs the password and a current code (or a recovery
/// code). Recovery codes are deleted. Refused while `STARR_ADMIN_TOTP_REQUIRED`
/// is set (then only `starr admin recover --reset-totp` turns it off).
/// Account writes on every live instance pause automatically (audited as
/// `accounts.pause`); resuming needs every active admin to have TOTP again
/// (spec 0005 M10 1).
#[utoipa::path(post, path = "/api/v1/auth/totp/disable", tag = "auth",
    request_body = DisableTotpRequest,
    responses(
        (status = 204, description = "TOTP is off"),
        (status = 403, description = "`auth.forbidden`", body = ErrorBody),
        (status = 409, description = "`totp.not_enabled`, `totp.required`", body = ErrorBody),
        (status = 422, description = "`auth.current_password_invalid`, `auth.invalid_code`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn disable(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    client: Client,
    Json(body): Json<DisableTotpRequest>,
) -> Result<StatusCode, ApiError> {
    let admin = &session.subject;
    if !session.totp_enabled {
        return Err(ApiError::Conflict("totp.not_enabled"));
    }
    // Off would let a password alone enroll a new authenticator.
    if state.settings.admin_totp_required {
        return Err(ApiError::Conflict("totp.required"));
    }
    let hash = state.db.admin_password_hash(admin.id).await?;
    if !common::verify_password(&state, &client, &admin.username, hash, body.password).await? {
        return Err(ApiError::Validation("auth.current_password_invalid"));
    }
    if check_second_factor(&state, &client, admin, &body.code).await? == SecondFactorCheck::Failed {
        return Err(ApiError::Validation("auth.invalid_code"));
    }
    state.db.disable_totp(admin.id).await?;
    common::audit(
        &state,
        admin,
        "auth.totp_disabled",
        None,
        Some(json!({ "ip": client.ip })),
    )
    .await?;
    // Allowed, but live account writes pause until TOTP is back (M10 1).
    common::pause_writes_for_totp_gap(&state, admin, "auth.totp_disabled").await?;
    Ok(StatusCode::NO_CONTENT)
}
