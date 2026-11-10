//! Member administration: `/api/v1/members` (admins only).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::auth::SubjectKind;
use starr_core::members::{MemberStatus, PASSWORD_TOKEN_LIFETIME, PasswordTokenPurpose};
use starr_db::members::{MemberFilter, MemberRecord, NewMember};
use utoipa::{IntoParams, ToSchema};

use super::common::{self, EntitlementDto, LinkDto, SessionDto, to_dtos, to_entitlements};
use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::{AdminSession, crypto};

use super::common::SET_PASSWORD_PATH;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = MemberStatus)]
pub enum MemberStatusDto {
    Active,
    Suspended,
    /// Deletion pending; restorable until `deletion.graceUntil`.
    Retired,
}

impl From<MemberStatus> for MemberStatusDto {
    fn from(s: MemberStatus) -> Self {
        match s {
            MemberStatus::Active => Self::Active,
            MemberStatus::Suspended => Self::Suspended,
            MemberStatus::Retired => Self::Retired,
        }
    }
}

impl From<MemberStatusDto> for MemberStatus {
    fn from(s: MemberStatusDto) -> Self {
        match s {
            MemberStatusDto::Active => Self::Active,
            MemberStatusDto::Suspended => Self::Suspended,
            MemberStatusDto::Retired => Self::Retired,
        }
    }
}

/// A pending deletion order.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeletionDto {
    requested_by: String,
    requested_at: DateTime<Utc>,
    /// Until then the member can be restored; afterwards only a tombstone remains.
    grace_until: DateTime<Utc>,
}

/// A member as admins see it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemberDto {
    id: i64,
    username: String,
    email: Option<String>,
    display_name: String,
    status: MemberStatusDto,
    /// False until the member set a password via link or invitation.
    password_set: bool,
    created_at: DateTime<Utc>,
    deletion: Option<DeletionDto>,
    /// One entry per category.
    entitlements: Vec<EntitlementDto>,
}

/// A member with their sessions.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemberDetail {
    #[serde(flatten)]
    member: MemberDto,
    sessions: Vec<SessionDto>,
    /// Since when the NSFW (`adult`) entitlement is on; absent while it is off.
    adult_since: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct MemberQuery {
    status: Option<MemberStatusDto>,
    /// Case-insensitive search in username, display name and email.
    q: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateMemberRequest {
    username: String,
    email: String,
    /// Defaults to the username.
    display_name: Option<String>,
    /// Missing categories are off with request mode `approval`.
    #[serde(default)]
    entitlements: Vec<EntitlementDto>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatedMember {
    member: MemberDto,
    /// One-time set-password link to hand to the member.
    set_password_link: LinkDto,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EntitlementsRequest {
    entitlements: Vec<EntitlementDto>,
}

/// Irreversible: the request names the member it means (lesson 4).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeleteMemberRequest {
    /// Must equal the member's username.
    username: String,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DeleteOutcome {
    /// The member never used Starr and is gone; the username is free.
    Deleted,
    /// The member is retired and restorable until `graceUntil`.
    Scheduled,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeleteMemberResponse {
    outcome: DeleteOutcome,
    grace_until: Option<DateTime<Utc>>,
}

async fn member_dto(state: &AppState, record: MemberRecord) -> Result<MemberDto, ApiError> {
    let entitlements = to_dtos(state.db.entitlements(record.id).await?);
    Ok(MemberDto {
        id: record.id,
        username: record.username,
        email: record.email,
        display_name: record.display_name,
        status: record.status.into(),
        password_set: record.password_set,
        created_at: record.created_at,
        deletion: record.deletion.map(|d| DeletionDto {
            requested_by: d.requested_by,
            requested_at: d.requested_at,
            grace_until: d.grace_until,
        }),
        entitlements,
    })
}

async fn find(state: &AppState, id: i64) -> Result<MemberRecord, ApiError> {
    state
        .db
        .get_member(id)
        .await?
        .ok_or(ApiError::NotFound("member.not_found"))
}

async fn detail(state: &AppState, id: i64) -> Result<MemberDetail, ApiError> {
    let record = find(state, id).await?;
    let sessions = state
        .db
        .subject_sessions(SubjectKind::Member, id)
        .await?
        .into_iter()
        .map(|s| SessionDto::new(s, &[]))
        .collect();
    let adult_since = state
        .db
        .entitlement_enabled_since(id, starr_core::members::Category::Adult)
        .await?;
    Ok(MemberDetail {
        member: member_dto(state, record).await?,
        sessions,
        adult_since,
    })
}

/// Issue a new password link for a member (older unused links stop working).
async fn issue_password_link(
    state: &AppState,
    member_id: i64,
    purpose: PasswordTokenPurpose,
) -> Result<LinkDto, ApiError> {
    let now = state.clock.now();
    let token = crypto::new_token();
    let expires_at = now + PASSWORD_TOKEN_LIFETIME;
    state
        .db
        .create_password_token(
            &crypto::hash_token(secrecy::ExposeSecret::expose_secret(&token)),
            member_id,
            purpose,
            expires_at,
            now,
        )
        .await?;
    Ok(LinkDto::new(state, SET_PASSWORD_PATH, &token, expires_at))
}

/// Members, oldest first. Deleted members (tombstones) are not listed.
#[utoipa::path(get, path = "/api/v1/members", tag = "members",
    params(MemberQuery),
    responses(
        (status = 200, body = Vec<MemberDto>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden`", body = ErrorBody),
    ))]
pub async fn list_members(
    _: AdminSession,
    State(state): State<AppState>,
    Query(query): Query<MemberQuery>,
) -> Result<Json<Vec<MemberDto>>, ApiError> {
    let records = state
        .db
        .list_members(MemberFilter {
            status: query.status.map(Into::into),
            query: query.q.as_deref().filter(|q| !q.trim().is_empty()),
        })
        .await?;
    let mut members = Vec::with_capacity(records.len());
    for record in records {
        members.push(member_dto(&state, record).await?);
    }
    Ok(Json(members))
}

/// Create a member. Returns a one-time set-password link (valid 7 days).
#[utoipa::path(post, path = "/api/v1/members", tag = "members",
    request_body = CreateMemberRequest,
    responses(
        (status = 201, body = CreatedMember),
        (status = 409, description = "`validation.username_taken`, `validation.email_taken`", body = ErrorBody),
        (status = 422, description = "`validation.username_invalid`, `validation.email_invalid`, `validation.display_name_invalid`", body = ErrorBody),
    ))]
pub async fn create_member(
    AdminSession(admin): AdminSession,
    State(state): State<AppState>,
    Json(body): Json<CreateMemberRequest>,
) -> Result<(StatusCode, Json<CreatedMember>), ApiError> {
    let username = common::username(&body.username)?;
    let email = common::email(&body.email)?;
    let display_name = common::display_name(body.display_name.as_deref(), &username)?;
    let entitlements = to_entitlements(&body.entitlements);
    let id = state
        .db
        .create_member(
            &NewMember {
                username: &username,
                email: &email,
                display_name: &display_name,
                password_hash: None,
            },
            &entitlements,
            state.clock.now(),
        )
        .await?;
    super::service_accounts::new_member_accounts(&state, id, &username, &[]).await;
    let link = issue_password_link(&state, id, PasswordTokenPurpose::Set).await?;
    common::audit(
        &state,
        &admin.subject,
        "member.created",
        Some(&username),
        Some(json!({ "entitlements": body.entitlements })),
    )
    .await?;
    let member = member_dto(&state, find(&state, id).await?).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedMember {
            member,
            set_password_link: link,
        }),
    ))
}

/// One member with entitlements and sessions.
#[utoipa::path(get, path = "/api/v1/members/{id}", tag = "members",
    params(("id" = i64, Path)),
    responses(
        (status = 200, body = MemberDetail),
        (status = 404, description = "`member.not_found`", body = ErrorBody),
    ))]
pub async fn get_member(
    _: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<MemberDetail>, ApiError> {
    Ok(Json(detail(&state, id).await?))
}

/// Suspend an active member. All their sessions end immediately.
#[utoipa::path(post, path = "/api/v1/members/{id}/suspend", tag = "members",
    params(("id" = i64, Path)),
    responses(
        (status = 200, body = MemberDetail),
        (status = 404, description = "`member.not_found`", body = ErrorBody),
        (status = 409, description = "`member.not_active`", body = ErrorBody),
    ))]
pub async fn suspend_member(
    AdminSession(admin): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<MemberDetail>, ApiError> {
    let record = find(&state, id).await?;
    if !state.db.suspend_member(id).await? {
        return Err(ApiError::Conflict("member.not_active"));
    }
    common::audit(
        &state,
        &admin.subject,
        "member.suspended",
        Some(&record.username),
        None,
    )
    .await?;
    super::service_accounts::accounts_changed(&state).await?;
    Ok(Json(detail(&state, id).await?))
}

/// Reactivate a suspended member.
#[utoipa::path(post, path = "/api/v1/members/{id}/reactivate", tag = "members",
    params(("id" = i64, Path)),
    responses(
        (status = 200, body = MemberDetail),
        (status = 404, description = "`member.not_found`", body = ErrorBody),
        (status = 409, description = "`member.not_suspended`", body = ErrorBody),
    ))]
pub async fn reactivate_member(
    AdminSession(admin): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<MemberDetail>, ApiError> {
    let record = find(&state, id).await?;
    if !state.db.reactivate_member(id).await? {
        return Err(ApiError::Conflict("member.not_suspended"));
    }
    common::audit(
        &state,
        &admin.subject,
        "member.reactivated",
        Some(&record.username),
        None,
    )
    .await?;
    super::service_accounts::accounts_changed(&state).await?;
    Ok(Json(detail(&state, id).await?))
}

/// Issue a new set-password link (password reset). Earlier links stop
/// working; the current password keeps working until the link is used.
#[utoipa::path(post, path = "/api/v1/members/{id}/password-reset", tag = "members",
    params(("id" = i64, Path)),
    responses(
        (status = 200, body = LinkDto),
        (status = 404, description = "`member.not_found`", body = ErrorBody),
        (status = 409, description = "`member.not_active`", body = ErrorBody),
    ))]
pub async fn reset_password(
    AdminSession(admin): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<LinkDto>, ApiError> {
    let record = find(&state, id).await?;
    // Suspension voids links and suspended members cannot use new ones.
    if record.status != MemberStatus::Active {
        return Err(ApiError::Conflict("member.not_active"));
    }
    let purpose = if record.password_set {
        PasswordTokenPurpose::Reset
    } else {
        PasswordTokenPurpose::Set
    };
    let link = issue_password_link(&state, id, purpose).await?;
    common::audit(
        &state,
        &admin.subject,
        "member.password_link_issued",
        Some(&record.username),
        Some(json!({ "purpose": purpose.as_str() })),
    )
    .await?;
    Ok(Json(link))
}

/// Delete a member. A member without own audit entries (never logged in or
/// acted) and without an account Starr created in a service is deleted at
/// once and the username is freed. Everyone else is retired: sessions end,
/// their service accounts are disabled by the next reconcile, and after
/// `STARR_MEMBER_DELETE_GRACE_DAYS` the accounts Starr manages are deleted
/// in the services (never ignored, hands-off or administrator accounts,
/// spec 0005 D16) and only a tombstone remains that keeps the username
/// reserved. A member who owns service accounts can only be deleted after
/// a fresh elevation (M10 2).
#[utoipa::path(delete, path = "/api/v1/members/{id}", tag = "members",
    params(("id" = i64, Path)),
    request_body = DeleteMemberRequest,
    responses(
        (status = 200, body = DeleteMemberResponse),
        (status = 403, description = "`auth.elevation_required` (the member owns service accounts)", body = ErrorBody),
        (status = 404, description = "`member.not_found`", body = ErrorBody),
        (status = 409, description = "`member.already_retired`", body = ErrorBody),
        (status = 422, description = "`validation.confirmation_mismatch`", body = ErrorBody),
    ))]
pub async fn delete_member(
    AdminSession(admin): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<DeleteMemberRequest>,
) -> Result<Json<DeleteMemberResponse>, ApiError> {
    let record = find(&state, id).await?;
    if body.username.trim().to_lowercase() != record.username {
        return Err(ApiError::Validation("validation.confirmation_mismatch"));
    }
    if record.status == MemberStatus::Retired {
        return Err(ApiError::Conflict("member.already_retired"));
    }
    // A deletion order deletes accounts in the services (D16, M10 2).
    if state.db.member_owns_accounts(id).await? {
        common::require_elevation(&state, &admin)?;
    }
    if state.db.delete_unused_member(id).await? {
        common::audit(
            &state,
            &admin.subject,
            "member.deleted",
            Some(&record.username),
            None,
        )
        .await?;
        return Ok(Json(DeleteMemberResponse {
            outcome: DeleteOutcome::Deleted,
            grace_until: None,
        }));
    }

    let now = state.clock.now();
    let grace_until = now + TimeDelta::days(i64::from(state.settings.member_delete_grace_days));
    if !state
        .db
        .schedule_member_deletion(id, &admin.subject.username, grace_until, now)
        .await?
    {
        return Err(ApiError::Conflict("member.already_retired"));
    }
    common::audit(
        &state,
        &admin.subject,
        "member.deletion_scheduled",
        Some(&record.username),
        Some(json!({ "graceUntil": grace_until })),
    )
    .await?;
    super::service_accounts::accounts_changed(&state).await?;
    Ok(Json(DeleteMemberResponse {
        outcome: DeleteOutcome::Scheduled,
        grace_until: Some(grace_until),
    }))
}

/// Cancel a pending deletion. The member returns suspended.
#[utoipa::path(post, path = "/api/v1/members/{id}/restore", tag = "members",
    params(("id" = i64, Path)),
    responses(
        (status = 200, body = MemberDetail),
        (status = 404, description = "`member.not_found`", body = ErrorBody),
        (status = 409, description = "`member.not_restorable`", body = ErrorBody),
    ))]
pub async fn restore_member(
    AdminSession(admin): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<MemberDetail>, ApiError> {
    let record = find(&state, id).await?;
    if !state.db.restore_member(id, state.clock.now()).await? {
        return Err(ApiError::Conflict("member.not_restorable"));
    }
    common::audit(
        &state,
        &admin.subject,
        "member.restored",
        Some(&record.username),
        None,
    )
    .await?;
    super::service_accounts::accounts_changed(&state).await?;
    Ok(Json(detail(&state, id).await?))
}

/// Replace the member's entitlements. Categories left out are switched off.
/// Only admins can change entitlements, including `adult`.
#[utoipa::path(put, path = "/api/v1/members/{id}/entitlements", tag = "members",
    params(("id" = i64, Path)),
    request_body = EntitlementsRequest,
    responses(
        (status = 200, body = Vec<EntitlementDto>),
        (status = 404, description = "`member.not_found`", body = ErrorBody),
        (status = 409, description = "`member.retired`", body = ErrorBody),
    ))]
pub async fn put_entitlements(
    AdminSession(admin): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<EntitlementsRequest>,
) -> Result<Json<Vec<EntitlementDto>>, ApiError> {
    let record = find(&state, id).await?;
    if record.status == MemberStatus::Retired {
        return Err(ApiError::Conflict("member.retired"));
    }
    state
        .db
        .replace_entitlements(id, &to_entitlements(&body.entitlements), state.clock.now())
        .await?;
    let adult_on = |list: &[starr_core::members::Entitlement]| {
        list.iter()
            .any(|e| e.category == starr_core::members::Category::Adult && e.enabled)
    };
    let stored = state.db.entitlements(id).await?;
    if !adult_on(&stored) {
        // Without the adult entitlement the member's unlocks close at once,
        // so granting it again later does not reopen an old session.
        state.db.lock_private_unlocks(Some(id)).await?;
    }
    let stored = to_dtos(stored);
    common::audit(
        &state,
        &admin.subject,
        "member.entitlements_updated",
        Some(&record.username),
        Some(json!({ "entitlements": stored })),
    )
    .await?;
    super::service_accounts::accounts_changed(&state).await?;
    Ok(Json(stored))
}
