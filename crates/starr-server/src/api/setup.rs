//! First-run setup: create the first admin with the one-time setup token.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum_extra::extract::cookie::CookieJar;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use starr_core::auth::{
    DISPLAY_NAME_MAX_CHARS, PasswordError, Subject, SubjectKind, parse_username, validate_password,
};
use starr_db::audit::{ActorKind, AuditEntry};
use starr_db::auth::NewAdmin;
use utoipa::ToSchema;

use super::auth::{MeResponse, me_response};
use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::{Client, crypto, start_session};

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetupState {
    /// No admin exists yet: the UI shows the setup page.
    needs_setup: bool,
    /// `STARR_PUBLIC_URL` is set; setup cannot finish without it.
    public_url_configured: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetupAdminRequest {
    /// The one-time setup token printed in the server log.
    #[schema(value_type = String, format = Password)]
    token: SecretString,
    username: String,
    /// Defaults to the username.
    display_name: Option<String>,
    #[schema(value_type = String, format = Password)]
    password: SecretString,
}

/// Whether first-run setup is needed.
#[utoipa::path(get, path = "/api/v1/setup/state", tag = "setup",
    responses((status = 200, body = SetupState)))]
pub async fn state(State(state): State<AppState>) -> Result<Json<SetupState>, ApiError> {
    Ok(Json(SetupState {
        needs_setup: !state.db.admin_exists().await?,
        public_url_configured: state.settings.public_url.is_some(),
    }))
}

/// Create the first admin account and log in as it.
#[utoipa::path(post, path = "/api/v1/setup/admin", tag = "setup",
    request_body = SetupAdminRequest,
    responses(
        (status = 201, description = "Admin created; session cookie set", body = MeResponse),
        (status = 403, description = "`setup.invalid_token`, `auth.origin_mismatch`", body = ErrorBody),
        (status = 409, description = "`setup.already_completed`, `setup.public_url_required`", body = ErrorBody),
        (status = 422, description = "`validation.username_invalid`, `validation.display_name_invalid`, `auth.password_too_short`, `auth.password_too_long`", body = ErrorBody),
    ))]
pub async fn create_admin(
    State(state): State<AppState>,
    client: Client,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<SetupAdminRequest>,
) -> Result<(StatusCode, CookieJar, Json<MeResponse>), ApiError> {
    if state.db.admin_exists().await? {
        return Err(ApiError::SetupAlreadyCompleted);
    }
    if state.settings.public_url.is_none() {
        return Err(ApiError::SetupPublicUrlRequired);
    }
    if !state.setup_token.matches(&body.token) {
        tracing::warn!(ip = %client.ip, "setup attempt with invalid token");
        return Err(ApiError::SetupInvalidToken);
    }

    let username = parse_username(&body.username)
        .map_err(|_| ApiError::Validation("validation.username_invalid"))?;
    let display_name = match body.display_name.as_deref().map(str::trim) {
        None | Some("") => username.clone(),
        Some(name) if name.chars().count() <= DISPLAY_NAME_MAX_CHARS => name.to_owned(),
        Some(_) => return Err(ApiError::Validation("validation.display_name_invalid")),
    };
    validate_password(body.password.expose_secret()).map_err(|err| {
        ApiError::Validation(match err {
            PasswordError::TooShort => "auth.password_too_short",
            PasswordError::TooLong => "auth.password_too_long",
        })
    })?;

    let password = body.password;
    let password_hash = state
        .run_hashing(move || crypto::hash_password(&password))
        .await?
        .map_err(|err| {
            tracing::error!(error = %err, "hashing password failed");
            ApiError::Internal
        })?;

    let now = state.clock.now();
    let id = state
        .db
        .create_first_admin(
            &NewAdmin {
                username: &username,
                display_name: &display_name,
                password_hash: &password_hash,
            },
            now,
        )
        .await?
        .ok_or(ApiError::SetupAlreadyCompleted)?;
    state.setup_token.consume();

    state
        .db
        .audit(
            &AuditEntry {
                actor_kind: ActorKind::System,
                actor_name: None,
                action: "setup.admin_created",
                target: Some(&username),
                details: Some(&serde_json::json!({ "ip": client.ip }).to_string()),
            },
            now,
        )
        .await?;
    tracing::info!(%username, "first admin created; setup complete");

    let subject = Subject {
        kind: SubjectKind::Admin,
        id,
        username,
        display_name,
    };
    let jar = start_session(&state, jar, &headers, &client, &subject).await?;
    let me = me_response(&state, subject, None).await?;
    Ok((StatusCode::CREATED, jar, Json(me)))
}
