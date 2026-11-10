//! Invitations (ADR 0006): admins create and revoke links at
//! `/api/v1/invitations`; invited people look at and accept them through the
//! two public, rate-limited `/api/v1/invitations/{token}` endpoints.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum_extra::extract::cookie::CookieJar;
use chrono::{DateTime, TimeDelta, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::auth::{Subject, SubjectKind};
use starr_core::members::{
    INVITATION_DEFAULT_DAYS, INVITATION_MAX_DAYS, InvitationState, invitation_state,
};
use starr_db::invitations::{Acceptance, InvitationRecord, NewInvitation};
use starr_db::members::NewMember;
use utoipa::ToSchema;

use super::auth::{MeResponse, me_response};
use super::common::{self, EntitlementDto, LinkDto, to_dtos, to_entitlements};
use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::{AdminSession, Client, crypto, start_session};

/// Web UI page that accepts invitations.
const INVITE_PATH: &str = "/invite";

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = InvitationState)]
pub enum InvitationStateDto {
    Pending,
    Accepted,
    Revoked,
    Expired,
}

impl From<InvitationState> for InvitationStateDto {
    fn from(s: InvitationState) -> Self {
        match s {
            InvitationState::Pending => Self::Pending,
            InvitationState::Accepted => Self::Accepted,
            InvitationState::Revoked => Self::Revoked,
            InvitationState::Expired => Self::Expired,
        }
    }
}

/// An invitation as admins see it. The link itself is shown only once, at
/// creation: Starr stores only a hash of the token.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InvitationDto {
    id: i64,
    state: InvitationStateDto,
    created_by: Option<String>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    used_at: Option<DateTime<Utc>>,
    /// Username of the member who accepted it.
    used_by: Option<String>,
    revoked_at: Option<DateTime<Utc>>,
    preset_username: Option<String>,
    preset_email: Option<String>,
    /// One entry per category.
    preset_entitlements: Vec<EntitlementDto>,
    /// Existing accounts the new member is linked to on acceptance (M3).
    preset_accounts: Vec<PresetAccountDto>,
}

/// An existing account an invitation names.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PresetAccountDto {
    service_id: i64,
    /// The service's account ID.
    remote_id: String,
    /// The name there when the invitation was created; ignored on input.
    #[serde(default)]
    username: Option<String>,
}

fn state_of(record: &InvitationRecord, now: DateTime<Utc>) -> InvitationState {
    invitation_state(
        record.used_at.is_some(),
        record.revoked_at.is_some(),
        record.expires_at <= now,
    )
}

fn invitation_dto(record: InvitationRecord, now: DateTime<Utc>) -> InvitationDto {
    InvitationDto {
        id: record.id,
        state: state_of(&record, now).into(),
        created_by: record.created_by,
        created_at: record.created_at,
        expires_at: record.expires_at,
        used_at: record.used_at,
        used_by: record.used_by,
        revoked_at: record.revoked_at,
        preset_username: record.preset_username,
        preset_email: record.preset_email,
        preset_entitlements: to_dtos(starr_core::members::complete_entitlements(
            &record.preset_entitlements,
        )),
        preset_accounts: record
            .preset_accounts
            .into_iter()
            .map(|p| PresetAccountDto {
                service_id: p.service_id,
                remote_id: p.remote_id,
                username: p.username,
            })
            .collect(),
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateInvitationRequest {
    /// 1 to 30, default 7.
    expires_in_days: Option<u32>,
    /// Preset username; the invited person cannot change it.
    username: Option<String>,
    /// Preset email; the invited person cannot change it.
    email: Option<String>,
    /// Entitlements the new member gets. Missing categories are off.
    #[serde(default)]
    entitlements: Vec<EntitlementDto>,
    /// The friend's existing accounts (at most one per service): accepting
    /// links them to the new member, password deferred (spec 0005 M3).
    /// Elsewhere the member gets new accounts.
    #[serde(default)]
    accounts: Vec<PresetAccountDto>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatedInvitation {
    invitation: InvitationDto,
    /// The invitation link. Shown only now.
    link: LinkDto,
}

/// What the accept page needs to know about an invitation.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InvitationPreview {
    preset_username: Option<String>,
    preset_email: Option<String>,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AcceptInvitationRequest {
    /// Required unless the invitation presets it (then ignored).
    username: Option<String>,
    /// Required unless the invitation presets it (then ignored).
    email: Option<String>,
    /// Defaults to the username.
    display_name: Option<String>,
    #[schema(value_type = String, format = Password)]
    password: SecretString,
}

/// Normalize an optional text field: missing or blank is `None`.
fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// All invitations, newest first.
#[utoipa::path(get, path = "/api/v1/invitations", tag = "invitations",
    responses(
        (status = 200, body = Vec<InvitationDto>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden`", body = ErrorBody),
    ))]
pub async fn list_invitations(
    _: AdminSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<InvitationDto>>, ApiError> {
    let now = state.clock.now();
    let invitations = state.db.list_invitations().await?;
    Ok(Json(
        invitations
            .into_iter()
            .map(|i| invitation_dto(i, now))
            .collect(),
    ))
}

/// Create an invitation link (single use, default expiry 7 days).
#[utoipa::path(post, path = "/api/v1/invitations", tag = "invitations",
    request_body = CreateInvitationRequest,
    responses(
        (status = 201, body = CreatedInvitation),
        (status = 404, description = "`services.not_found`, `accounts.not_found` (a named account)", body = ErrorBody),
        (status = 403, description = "`auth.forbidden`, `auth.elevation_required` (naming existing accounts adopts them: re-authenticate first)", body = ErrorBody),
        (status = 409, description = "`validation.username_taken`, `validation.email_taken`, `accounts.taken` (a named account is linked to a member)", body = ErrorBody),
        (status = 422, description = "`validation.username_invalid`, `validation.email_invalid`, `validation.expiry_invalid`, `accounts.unsupported`, `accounts.privileged`, `accounts.ignored`, `accounts.gone`, `invitations.accounts_duplicate_service`, `invitations.accounts_not_entitled` (no category of the invitation is served there)", body = ErrorBody),
    ))]
pub async fn create_invitation(
    AdminSession(admin): AdminSession,
    State(state): State<AppState>,
    Json(body): Json<CreateInvitationRequest>,
) -> Result<(StatusCode, Json<CreatedInvitation>), ApiError> {
    // Naming existing accounts adopts them on acceptance (M10 2).
    if !body.accounts.is_empty() {
        common::require_elevation(&state, &admin)?;
    }
    let days = body.expires_in_days.unwrap_or(INVITATION_DEFAULT_DAYS);
    if !(1..=INVITATION_MAX_DAYS).contains(&days) {
        return Err(ApiError::Validation("validation.expiry_invalid"));
    }
    let username = present(body.username.as_deref())
        .map(common::username)
        .transpose()?;
    let email = present(body.email.as_deref())
        .map(common::email)
        .transpose()?;
    if let Some(name) = &username
        && state.db.username_exists(name).await?
    {
        return Err(ApiError::Conflict("validation.username_taken"));
    }
    if let Some(email) = &email
        && state.db.email_exists(email).await?
    {
        return Err(ApiError::Conflict("validation.email_taken"));
    }

    let entitlements = to_entitlements(&body.entitlements);
    let accounts = super::service_accounts::preset_accounts(
        &state,
        &body
            .accounts
            .iter()
            .map(|a| (a.service_id, a.remote_id.trim()))
            .collect::<Vec<_>>(),
        &entitlements,
    )
    .await?;

    let now = state.clock.now();
    let token = crypto::new_token();
    let expires_at = now + TimeDelta::days(i64::from(days));
    let id = state
        .db
        .create_invitation(
            &NewInvitation {
                token_hash: &crypto::hash_token(token.expose_secret()),
                created_by: admin.subject.id,
                expires_at,
                preset_username: username.as_deref(),
                preset_email: email.as_deref(),
                preset_entitlements: &entitlements,
                preset_accounts: &accounts,
            },
            now,
        )
        .await?;
    common::audit(
        &state,
        &admin.subject,
        "invitation.created",
        Some(&format!("invitation:{id}")),
        Some(json!({
            "expiresAt": expires_at,
            "presetUsername": username,
            "entitlements": body.entitlements,
            "accounts": accounts.iter().map(|a| json!({ "serviceId": a.service_id, "remoteId": a.remote_id })).collect::<Vec<_>>(),
        })),
    )
    .await?;
    let record = state
        .db
        .get_invitation(id)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedInvitation {
            invitation: invitation_dto(record, now),
            link: LinkDto::new(&state, INVITE_PATH, &token, expires_at),
        }),
    ))
}

/// Revoke a pending invitation; its link stops working.
#[utoipa::path(post, path = "/api/v1/invitations/{id}/revoke", tag = "invitations",
    params(("id" = i64, Path)),
    responses(
        (status = 200, body = InvitationDto),
        (status = 404, description = "`invitation.not_found`", body = ErrorBody),
        (status = 409, description = "`invitation.not_pending`", body = ErrorBody),
    ))]
pub async fn revoke_invitation(
    AdminSession(admin): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<InvitationDto>, ApiError> {
    let now = state.clock.now();
    if state.db.get_invitation(id).await?.is_none() {
        return Err(ApiError::NotFound("invitation.not_found"));
    }
    if !state.db.revoke_invitation(id, now).await? {
        return Err(ApiError::Conflict("invitation.not_pending"));
    }
    common::audit(
        &state,
        &admin.subject,
        "invitation.revoked",
        Some(&format!("invitation:{id}")),
        None,
    )
    .await?;
    let record = state
        .db
        .get_invitation(id)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok(Json(invitation_dto(record, now)))
}

/// Why an invitation cannot be used, if it cannot.
fn ensure_pending(record: &InvitationRecord, now: DateTime<Utc>) -> Result<(), ApiError> {
    match state_of(record, now) {
        InvitationState::Pending => Ok(()),
        InvitationState::Accepted => Err(ApiError::Conflict("invitation.used")),
        InvitationState::Revoked => Err(ApiError::Conflict("invitation.revoked")),
        InvitationState::Expired => Err(ApiError::Conflict("invitation.expired")),
    }
}

async fn find_by_token(state: &AppState, token: &str) -> Result<InvitationRecord, ApiError> {
    state
        .db
        .find_invitation(&crypto::hash_token(token))
        .await?
        .ok_or(ApiError::NotFound("invitation.not_found"))
}

/// Look at an invitation before accepting it. Public, rate-limited.
#[utoipa::path(get, path = "/api/v1/invitations/{token}", tag = "invitations",
    params(("token" = String, Path)),
    responses(
        (status = 200, body = InvitationPreview),
        (status = 403, description = "`auth.forbidden` (cross-site request)", body = ErrorBody),
        (status = 404, description = "`invitation.not_found`", body = ErrorBody),
        (status = 409, description = "`invitation.used`, `invitation.revoked`, `invitation.expired`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn preview_invitation(
    State(state): State<AppState>,
    client: Client,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Result<Json<InvitationPreview>, ApiError> {
    common::refuse_cross_site(&headers)?;
    let attempt = common::begin_token_attempt(&state, &client, &token).await?;
    let record = find_by_token(&state, &token).await?;
    // A real token, even a used or expired one, is not guessing.
    common::token_attempt_succeeded(&state, attempt).await?;
    ensure_pending(&record, state.clock.now())?;
    Ok(Json(InvitationPreview {
        preset_username: record.preset_username,
        preset_email: record.preset_email,
        expires_at: record.expires_at,
    }))
}

/// Accept an invitation: creates the member with the preset entitlements,
/// sets the password and logs in. Public, rate-limited, single use.
#[utoipa::path(post, path = "/api/v1/invitations/{token}/accept", tag = "invitations",
    params(("token" = String, Path)),
    request_body = AcceptInvitationRequest,
    responses(
        (status = 201, description = "Member created and logged in; session cookie set", body = MeResponse),
        (status = 403, description = "`auth.origin_mismatch`", body = ErrorBody),
        (status = 404, description = "`invitation.not_found`", body = ErrorBody),
        (status = 409, description = "`invitation.used`, `invitation.revoked`, `invitation.expired`, `validation.username_taken`, `validation.email_taken`", body = ErrorBody),
        (status = 422, description = "`validation.username_invalid`, `validation.email_invalid`, `validation.display_name_invalid`, `auth.password_too_short`, `auth.password_too_long`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn accept_invitation(
    State(state): State<AppState>,
    client: Client,
    headers: HeaderMap,
    jar: CookieJar,
    Path(token): Path<String>,
    Json(body): Json<AcceptInvitationRequest>,
) -> Result<(StatusCode, CookieJar, Json<MeResponse>), ApiError> {
    // The attempt stays a failure until the member exists: a link holder
    // cannot loop through Argon2 hashes with conflicting input unthrottled.
    let attempt = common::begin_token_attempt(&state, &client, &token).await?;
    let invitation = find_by_token(&state, &token).await?;
    ensure_pending(&invitation, state.clock.now())?;

    let username = common::username(
        invitation
            .preset_username
            .as_deref()
            .or(present(body.username.as_deref()))
            .unwrap_or_default(),
    )?;
    let email = common::email(
        invitation
            .preset_email
            .as_deref()
            .or(present(body.email.as_deref()))
            .unwrap_or_default(),
    )?;
    let display_name = common::display_name(body.display_name.as_deref(), &username)?;
    common::new_password(&body.password)?;
    // Cheap conflict checks before the expensive hash; the insert still
    // catches races.
    if state.db.username_exists(&username).await? {
        return Err(ApiError::Conflict("validation.username_taken"));
    }
    if state.db.email_exists(&email).await? {
        return Err(ApiError::Conflict("validation.email_taken"));
    }
    let (password_hash, password) = common::hash_keeping(&state, body.password).await?;

    let now = state.clock.now();
    let member_id = match state
        .db
        .accept_invitation(
            invitation.id,
            &NewMember {
                username: &username,
                email: &email,
                display_name: &display_name,
                password_hash: Some(&password_hash),
            },
            &invitation.preset_entitlements,
            now,
        )
        .await?
    {
        Acceptance::Created(id) => id,
        Acceptance::Gone => {
            // Used, revoked or expired meanwhile: say which.
            let current = find_by_token(&state, &token).await?;
            ensure_pending(&current, state.clock.now())?;
            return Err(ApiError::Conflict("invitation.used"));
        }
    };
    common::token_attempt_succeeded(&state, attempt).await?;
    super::service_accounts::new_member_accounts(
        &state,
        member_id,
        &username,
        &invitation.preset_accounts,
    )
    .await;
    // The new accounts are created with this password (ADR 0010): a password
    // set at creation is generation 0.
    super::my_services::offer_after_set(&state, member_id, 0, &password).await;
    drop(password);
    let subject = Subject {
        kind: SubjectKind::Member,
        id: member_id,
        username,
        display_name,
    };
    common::audit(
        &state,
        &subject,
        "invitation.accepted",
        Some(&format!("invitation:{}", invitation.id)),
        Some(json!({ "ip": client.ip })),
    )
    .await?;
    let jar = start_session(&state, jar, &headers, &client, &subject).await?;
    let me = me_response(&state, subject, None).await?;
    Ok((StatusCode::CREATED, jar, Json(me)))
}
