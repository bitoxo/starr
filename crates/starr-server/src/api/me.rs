//! Member self-service: `/api/v1/me` (profile), `/me/password`, `/me/sessions`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum_extra::extract::cookie::CookieJar;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::auth::SubjectKind;
use utoipa::ToSchema;

use super::common::{self, EntitlementDto, SessionDto, to_dtos};
use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::{Client, CookieScheme, MemberSession, Session, end_session};

/// The member's own profile.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    username: String,
    display_name: String,
    email: Option<String>,
    /// What the member may request, one entry per category. Read-only here.
    entitlements: Vec<EntitlementDto>,
}

/// Fields a member may change. Unknown fields (e.g. entitlements) are rejected.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateProfileRequest {
    /// Empty resets it to the username.
    display_name: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChangePasswordRequest {
    #[schema(value_type = String, format = Password)]
    pub(super) current_password: SecretString,
    #[schema(value_type = String, format = Password)]
    pub(super) new_password: SecretString,
}

async fn profile(state: &AppState, session: &Session) -> Result<Profile, ApiError> {
    let member = state
        .db
        .get_member(session.subject.id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    // The private category is named only through the gate (spec 0007).
    let adult = super::privacy::adult_open(state, session).await?;
    let entitlements = state
        .db
        .entitlements(member.id)
        .await?
        .into_iter()
        .filter(|e| starr_core::categories::category_visible(e.category, adult))
        .collect();
    Ok(Profile {
        entitlements: to_dtos(entitlements),
        username: member.username,
        display_name: member.display_name,
        email: member.email,
    })
}

/// The logged-in member's profile.
#[utoipa::path(get, path = "/api/v1/me", tag = "me",
    responses(
        (status = 200, body = Profile),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
    ))]
pub async fn get_profile(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
) -> Result<Json<Profile>, ApiError> {
    Ok(Json(profile(&state, &session).await?))
}

/// Change the display name.
#[utoipa::path(patch, path = "/api/v1/me", tag = "me",
    request_body = UpdateProfileRequest,
    responses(
        (status = 200, body = Profile),
        (status = 422, description = "`validation.display_name_invalid`, unknown fields", body = ErrorBody),
    ))]
pub async fn update_profile(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Json(body): Json<UpdateProfileRequest>,
) -> Result<Json<Profile>, ApiError> {
    let name = common::display_name(Some(&body.display_name), &session.subject.username)?;
    state
        .db
        .set_member_display_name(session.subject.id, &name)
        .await?;
    common::audit_session(&state, &session, "member.profile_updated", None, None).await?;
    Ok(Json(profile(&state, &session).await?))
}

/// Change the password. Requires the current one; ends all other sessions.
/// Wrong current passwords count against the login throttle. Accounts in
/// services that use the Starr password get the new one (ADR 0010; signs
/// out their devices); see `GET /api/v1/me/services`.
#[utoipa::path(post, path = "/api/v1/me/password", tag = "me",
    request_body = ChangePasswordRequest,
    responses(
        (status = 204, description = "Password changed; other sessions ended"),
        (status = 422, description = "`auth.current_password_invalid`, `auth.password_too_short`, `auth.password_too_long`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn change_password(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    client: Client,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<StatusCode, ApiError> {
    let hash = state.db.member_password_hash(session.subject.id).await?;
    if !common::verify_password(
        &state,
        &client,
        &session.subject.username,
        hash,
        body.current_password,
    )
    .await?
    {
        return Err(ApiError::Validation("auth.current_password_invalid"));
    }

    common::new_password(&body.new_password)?;
    let (new_hash, password) = common::hash_keeping(&state, body.new_password).await?;
    let generation = state
        .db
        .change_member_password(session.subject.id, &new_hash, &session.token_hash)
        .await?;
    super::my_services::offer_after_set(&state, session.subject.id, generation, &password).await;
    drop(password);
    common::audit_session(
        &state,
        &session,
        "member.password_changed",
        None,
        Some(json!({ "ip": client.ip })),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Where the member is logged in, most recently used first.
#[utoipa::path(get, path = "/api/v1/me/sessions", tag = "me",
    responses((status = 200, body = Vec<SessionDto>)))]
pub async fn list_sessions(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<SessionDto>>, ApiError> {
    let sessions = state
        .db
        .subject_sessions(SubjectKind::Member, session.subject.id)
        .await?;
    Ok(Json(
        sessions
            .into_iter()
            .map(|s| SessionDto::new(s, &session.token_hash))
            .collect(),
    ))
}

/// End one of the member's sessions.
#[utoipa::path(delete, path = "/api/v1/me/sessions/{id}", tag = "me",
    params(("id" = i64, Path)),
    responses(
        (status = 204, description = "Session ended"),
        (status = 404, description = "`session.not_found`", body = ErrorBody),
    ))]
pub async fn revoke_session(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    if !state
        .db
        .delete_subject_session(SubjectKind::Member, session.subject.id, id)
        .await?
    {
        return Err(ApiError::NotFound("session.not_found"));
    }
    common::audit_session(&state, &session, "member.session_revoked", None, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Log out everywhere: ends all of the member's sessions, this one included,
/// and clears the session cookie.
#[utoipa::path(delete, path = "/api/v1/me/sessions", tag = "me",
    responses((status = 204, description = "All sessions ended; session cookie cleared")))]
pub async fn revoke_all_sessions(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Result<(StatusCode, CookieJar), ApiError> {
    state
        .db
        .delete_subject_sessions(SubjectKind::Member, session.subject.id)
        .await?;
    common::audit_session(&state, &session, "member.logged_out_everywhere", None, None).await?;
    let scheme = CookieScheme::for_request(&headers, state.settings.behind_proxy);
    let (jar, _) = end_session(&state, jar, &headers, scheme).await?;
    Ok((StatusCode::NO_CONTENT, jar))
}
