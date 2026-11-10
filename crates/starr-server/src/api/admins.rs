//! Admin accounts: `/api/v1/admins` (admins only). An admin never suspends
//! or removes themselves, the acting admin must be active, and one active
//! admin always remains (domain model §1). Removal leaves a tombstone.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::auth::{AdminChangeError, SubjectKind};
use starr_core::members::PASSWORD_TOKEN_LIFETIME;
use starr_db::admins::{AdminChange, AdminRecord, AdminStatusChange};
use starr_db::auth::NewAdmin;
use utoipa::ToSchema;

use super::common::{self, LinkDto, SET_PASSWORD_PATH, SessionDto};
use super::error::{ApiError, ErrorBody};
use super::me::ChangePasswordRequest;
use crate::AppState;
use crate::auth::{AdminSession, Client, crypto};

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AdminStatus {
    Active,
    Suspended,
}

/// An admin account as admins see it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdminDto {
    id: i64,
    username: String,
    display_name: String,
    status: AdminStatus,
    totp_enabled: bool,
    /// Username of the linked member profile.
    linked_member: Option<String>,
    /// Username of the admin who created this account.
    created_by: Option<String>,
    created_at: DateTime<Utc>,
    /// The account making this request.
    current: bool,
}

impl AdminDto {
    fn new(record: AdminRecord, current_id: i64) -> Self {
        Self {
            current: record.id == current_id,
            id: record.id,
            username: record.username,
            display_name: record.display_name,
            status: if record.suspended {
                AdminStatus::Suspended
            } else {
                AdminStatus::Active
            },
            totp_enabled: record.totp_enabled,
            linked_member: record.linked_member.map(|m| m.username),
            created_by: record.created_by,
            created_at: record.created_at,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateAdminRequest {
    username: String,
    /// Defaults to the username.
    display_name: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatedAdmin {
    admin: AdminDto,
    /// One-time set-password link to hand to the new admin.
    set_password_link: LinkDto,
}

/// Irreversible: the request names the admin it means.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RemoveAdminRequest {
    /// Must equal the admin's username.
    username: String,
}

async fn find(state: &AppState, id: i64) -> Result<AdminRecord, ApiError> {
    state
        .db
        .get_admin(id)
        .await?
        .filter(|a| !a.removed)
        .ok_or(ApiError::NotFound("admin.not_found"))
}

fn change_error(change: AdminStatusChange, outcome: AdminChange) -> Option<ApiError> {
    match outcome {
        AdminChange::Done => None,
        AdminChange::NotFound => Some(ApiError::NotFound("admin.not_found")),
        AdminChange::InvalidState => Some(ApiError::Conflict(match change {
            AdminStatusChange::Suspend => "admin.not_active",
            AdminStatusChange::Reactivate => "admin.not_suspended",
            AdminStatusChange::Remove => "admin.not_found",
        })),
        AdminChange::Refused(AdminChangeError::SelfTarget) => {
            Some(ApiError::Conflict("admin.self"))
        }
        AdminChange::Refused(AdminChangeError::ActorInactive) => Some(ApiError::Forbidden),
        AdminChange::Refused(AdminChangeError::LastActiveAdmin) => {
            Some(ApiError::Conflict("admin.last_active"))
        }
    }
}

async fn change_status(
    state: &AppState,
    session: &crate::auth::Session,
    id: i64,
    change: AdminStatusChange,
) -> Result<AdminRecord, ApiError> {
    let target = find(state, id).await?;
    let outcome = state
        .db
        .change_admin_status(session.subject.id, id, change, state.clock.now())
        .await?;
    if let Some(err) = change_error(change, outcome) {
        return Err(err);
    }
    let action = match change {
        AdminStatusChange::Suspend => "admin.suspended",
        AdminStatusChange::Reactivate => "admin.reactivated",
        AdminStatusChange::Remove => "admin.removed",
    };
    common::audit(
        state,
        &session.subject,
        action,
        Some(&target.username),
        None,
    )
    .await?;
    if change == AdminStatusChange::Reactivate {
        // Active again, possibly without TOTP (M10 1).
        common::pause_writes_for_totp_gap(state, &session.subject, action).await?;
    }
    Ok(target)
}

/// Admin accounts, oldest first. Removed admins are not listed.
#[utoipa::path(get, path = "/api/v1/admins", tag = "admins",
    responses(
        (status = 200, body = Vec<AdminDto>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden`, `auth.totp_enrollment_required`", body = ErrorBody),
    ))]
pub async fn list_admins(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<AdminDto>>, ApiError> {
    let admins = state.db.list_admins().await?;
    Ok(Json(
        admins
            .into_iter()
            .map(|a| AdminDto::new(a, session.subject.id))
            .collect(),
    ))
}

/// Create an admin account (needs a fresh elevation). Returns a one-time
/// set-password link (valid 7 days); the account cannot log in before it is
/// used. The new admin has
/// no TOTP yet, so account writes on live instances pause until they set it
/// up (spec 0005 M10 1).
#[utoipa::path(post, path = "/api/v1/admins", tag = "admins",
    request_body = CreateAdminRequest,
    responses(
        (status = 201, body = CreatedAdmin),
        (status = 403, description = "`auth.elevation_required`", body = ErrorBody),
        (status = 409, description = "`validation.username_taken`", body = ErrorBody),
        (status = 422, description = "`validation.username_invalid`, `validation.display_name_invalid`", body = ErrorBody),
    ))]
pub async fn create_admin_account(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Json(body): Json<CreateAdminRequest>,
) -> Result<(StatusCode, Json<CreatedAdmin>), ApiError> {
    common::require_elevation(&state, &session)?;
    let username = common::username(&body.username)?;
    let display_name = common::display_name(body.display_name.as_deref(), &username)?;
    // Unusable until the link is used: the hash of a random, discarded
    // password (so logins against it take as long as against real ones).
    let placeholder = common::hash(&state, crypto::new_token()).await?;
    let now = state.clock.now();
    let token = crypto::new_token();
    let expires_at = now + PASSWORD_TOKEN_LIFETIME;
    let id = state
        .db
        .create_admin(
            &NewAdmin {
                username: &username,
                display_name: &display_name,
                password_hash: &placeholder,
            },
            session.subject.id,
            &crypto::hash_token(token.expose_secret()),
            expires_at,
            now,
        )
        .await?;
    common::audit(
        &state,
        &session.subject,
        "admin.created",
        Some(&username),
        None,
    )
    .await?;
    // A new admin has no TOTP yet: live account writes pause (M10 1).
    common::pause_writes_for_totp_gap(&state, &session.subject, "admin.created").await?;
    let admin = AdminDto::new(find(&state, id).await?, session.subject.id);
    Ok((
        StatusCode::CREATED,
        Json(CreatedAdmin {
            admin,
            set_password_link: LinkDto::new(&state, SET_PASSWORD_PATH, &token, expires_at),
        }),
    ))
}

/// Suspend another admin (needs a fresh elevation). Their sessions
/// (including a member view) end.
#[utoipa::path(post, path = "/api/v1/admins/{id}/suspend", tag = "admins",
    params(("id" = i64, Path)),
    responses(
        (status = 200, body = AdminDto),
        (status = 403, description = "`auth.elevation_required`", body = ErrorBody),
        (status = 404, description = "`admin.not_found`", body = ErrorBody),
        (status = 409, description = "`admin.self`, `admin.not_active`, `admin.last_active`", body = ErrorBody),
    ))]
pub async fn suspend_admin(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<AdminDto>, ApiError> {
    common::require_elevation(&state, &session)?;
    change_status(&state, &session, id, AdminStatusChange::Suspend).await?;
    Ok(Json(AdminDto::new(
        find(&state, id).await?,
        session.subject.id,
    )))
}

/// Reactivate a suspended admin (needs a fresh elevation, like creating one).
#[utoipa::path(post, path = "/api/v1/admins/{id}/reactivate", tag = "admins",
    params(("id" = i64, Path)),
    responses(
        (status = 200, body = AdminDto),
        (status = 403, description = "`auth.elevation_required`", body = ErrorBody),
        (status = 404, description = "`admin.not_found`", body = ErrorBody),
        (status = 409, description = "`admin.not_suspended`", body = ErrorBody),
    ))]
pub async fn reactivate_admin(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<AdminDto>, ApiError> {
    common::require_elevation(&state, &session)?;
    change_status(&state, &session, id, AdminStatusChange::Reactivate).await?;
    Ok(Json(AdminDto::new(
        find(&state, id).await?,
        session.subject.id,
    )))
}

/// Remove another admin (needs a fresh elevation). A tombstone stays: the
/// username remains reserved because logs name it. Sessions, second factor
/// and member link go.
#[utoipa::path(delete, path = "/api/v1/admins/{id}", tag = "admins",
    params(("id" = i64, Path)),
    request_body = RemoveAdminRequest,
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = "`auth.elevation_required`", body = ErrorBody),
        (status = 404, description = "`admin.not_found`", body = ErrorBody),
        (status = 409, description = "`admin.self`, `admin.last_active`", body = ErrorBody),
        (status = 422, description = "`validation.confirmation_mismatch`", body = ErrorBody),
    ))]
pub async fn remove_admin(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<RemoveAdminRequest>,
) -> Result<StatusCode, ApiError> {
    common::require_elevation(&state, &session)?;
    let target = find(&state, id).await?;
    if body.username.trim().to_lowercase() != target.username {
        return Err(ApiError::Validation("validation.confirmation_mismatch"));
    }
    change_status(&state, &session, id, AdminStatusChange::Remove).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Where the signed-in admin is logged in, most recently used first
/// (spec 0008 ADM-28; members use `/api/v1/me/sessions`).
#[utoipa::path(get, path = "/api/v1/admin/me/sessions", tag = "admins",
    responses((status = 200, body = Vec<SessionDto>)))]
pub async fn list_own_sessions(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<SessionDto>>, ApiError> {
    let sessions = state
        .db
        .subject_sessions(SubjectKind::Admin, session.subject.id)
        .await?;
    Ok(Json(
        sessions
            .into_iter()
            .map(|s| SessionDto::new(s, &session.token_hash))
            .collect(),
    ))
}

/// End one of the signed-in admin's other sessions.
#[utoipa::path(delete, path = "/api/v1/admin/me/sessions/{id}", tag = "admins",
    params(("id" = i64, Path)),
    responses(
        (status = 204, description = "Session ended"),
        (status = 404, description = "`session.not_found`", body = ErrorBody),
        (status = 409, description = "`session.current` (sign out instead)", body = ErrorBody),
    ))]
pub async fn revoke_own_session(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    // This session ends through sign-out, which also clears the cookie.
    let current = state
        .db
        .subject_sessions(SubjectKind::Admin, session.subject.id)
        .await?
        .into_iter()
        .any(|s| s.id == id && s.token_hash == session.token_hash);
    if current {
        return Err(ApiError::Conflict("session.current"));
    }
    if !state
        .db
        .delete_subject_session(SubjectKind::Admin, session.subject.id, id)
        .await?
    {
        return Err(ApiError::NotFound("session.not_found"));
    }
    common::audit_session(&state, &session, "admin.session_revoked", None, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Change the signed-in admin's password. Requires the current one (wrong
/// ones count against the login throttle); ends every other session of the
/// admin and voids unused password links.
#[utoipa::path(post, path = "/api/v1/admin/me/password", tag = "admins",
    request_body = ChangePasswordRequest,
    responses(
        (status = 204, description = "Password changed; other sessions ended"),
        (status = 422, description = "`auth.current_password_invalid`, `auth.password_too_short`, `auth.password_too_long`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn change_own_password(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    client: Client,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<StatusCode, ApiError> {
    let hash = state.db.admin_password_hash(session.subject.id).await?;
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
    let new_hash = common::hash(&state, body.new_password).await?;
    state
        .db
        .change_admin_password(session.subject.id, &new_hash, &session.token_hash)
        .await?;
    common::audit_session(
        &state,
        &session,
        "admin.password_changed",
        None,
        Some(json!({ "ip": client.ip })),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
