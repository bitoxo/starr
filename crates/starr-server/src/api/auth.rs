//! Login (one or two steps), logout, the current account, and the admin's
//! member view: linked member, switch and re-authentication (ADR 0005).

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::CookieJar;
use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::auth::{
    ELEVATION_WINDOW, PENDING_LOGIN_LIFETIME, Subject, SubjectKind, normalize_username,
    parse_username,
};
use starr_core::members::MemberStatus;
use starr_db::audit::{ActorKind, AuditEntry};
use starr_db::auth::{AttemptKey, LoginAttempt};
use utoipa::ToSchema;

use super::common;
use super::error::{ApiError, ErrorBody};
use super::privacy::{self, PrivateCategory};
use super::totp::{SecondFactorCheck, check_second_factor};
use crate::AppState;
use crate::auth::{
    AdminSession, AnySession, Client, CookieScheme, NextSession, crypto, end_session,
    presented_device, request, rotate_session, start_session,
};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    username: String,
    #[schema(value_type = String, format = Password)]
    password: SecretString,
}

/// Kind of account behind a session.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    Admin,
    Member,
}

/// The member profile linked to an admin account.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkedMemberDto {
    id: i64,
    username: String,
    display_name: String,
    /// Only an active member can be switched into.
    active: bool,
}

/// The admin behind a member view.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwitchedFromDto {
    username: String,
    display_name: String,
    /// Switching back asks for a TOTP code too.
    totp_enabled: bool,
}

/// The logged-in account.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeResponse {
    kind: AccountKind,
    username: String,
    display_name: String,
    /// Admins: TOTP is on.
    totp_enabled: bool,
    /// Admins: `STARR_ADMIN_TOTP_REQUIRED` is set and TOTP is off. Until it
    /// is on, only `/auth/me` and the TOTP enrollment endpoints work.
    totp_enrollment_required: bool,
    /// Admins: the member profile for "switch to member view".
    linked_member: Option<LinkedMemberDto>,
    /// Member view of an admin: who switching back returns to.
    switched_from: Option<SwitchedFromDto>,
    /// The private category (spec 0007 Q7): present only for accounts
    /// that may unlock it (the category is on, and a member has the
    /// entitlement, or an admin). Absent for everybody else.
    private_category: Option<PrivateCategory>,
    /// Admins: until when actions that need a fresh re-authentication run
    /// without asking again (spec 0005 M10 2); absent when not elevated.
    /// The UI asks before opening such an action's dialog (spec 0008 ADM-34).
    elevated_until: Option<DateTime<Utc>>,
}

impl MeResponse {
    fn plain(subject: Subject) -> Self {
        Self {
            kind: match subject.kind {
                SubjectKind::Admin => AccountKind::Admin,
                SubjectKind::Member => AccountKind::Member,
            },
            username: subject.username,
            display_name: subject.display_name,
            totp_enabled: false,
            totp_enrollment_required: false,
            linked_member: None,
            switched_from: None,
            private_category: None,
            elevated_until: None,
        }
    }
}

/// The account `subject` as the UI sees it. `origin_admin_id` is the admin
/// behind a member view.
pub(crate) async fn me_response(
    state: &AppState,
    subject: Subject,
    origin_admin_id: Option<i64>,
) -> Result<MeResponse, ApiError> {
    let (kind, id) = (subject.kind, subject.id);
    let mut me = match subject.kind {
        SubjectKind::Admin => {
            let admin = state.db.get_admin(subject.id).await?;
            let mut me = MeResponse::plain(subject);
            if let Some(admin) = admin {
                me.totp_enabled = admin.totp_enabled;
                me.totp_enrollment_required =
                    state.settings.admin_totp_required && !admin.totp_enabled;
                me.linked_member = admin.linked_member.map(|m| LinkedMemberDto {
                    id: m.id,
                    username: m.username,
                    display_name: m.display_name,
                    active: m.active,
                });
            }
            me
        }
        SubjectKind::Member => MeResponse::plain(subject),
    };
    if let Some(origin) = origin_admin_id
        && let Some(admin) = state.db.get_admin(origin).await?
    {
        me.switched_from = Some(SwitchedFromDto {
            username: admin.username,
            display_name: admin.display_name,
            totp_enabled: admin.totp_enabled,
        });
    }
    // A session this response starts (login, switch) is locked.
    me.private_category = privacy::adult_available(state, kind, id)
        .await?
        .then(|| PrivateCategory::of(None, state.clock.now()));
    Ok(me)
}

/// The password step passed; the admin has TOTP on. Send the pending token
/// with the code to `POST /api/v1/auth/login/totp`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TotpChallenge {
    /// Single use; not a session.
    pending_token: String,
    expires_at: DateTime<Utc>,
}

/// Start the second login step for an admin with TOTP. No session yet.
pub(crate) async fn totp_challenge(
    state: &AppState,
    admin_id: i64,
) -> Result<TotpChallenge, ApiError> {
    let now = state.clock.now();
    let token = crypto::new_token();
    let expires_at = now + PENDING_LOGIN_LIFETIME;
    state
        .db
        .create_pending_login(
            &crypto::hash_token(secrecy::ExposeSecret::expose_secret(&token)),
            admin_id,
            expires_at,
            now,
        )
        .await?;
    Ok(TotpChallenge {
        pending_token: secrecy::ExposeSecret::expose_secret(&token).to_owned(),
        expires_at,
    })
}

pub(crate) fn actor_kind(kind: SubjectKind) -> ActorKind {
    match kind {
        SubjectKind::Admin => ActorKind::Admin,
        SubjectKind::Member => ActorKind::Member,
    }
}

fn ip_details(ip: std::net::IpAddr) -> String {
    serde_json::json!({ "ip": ip }).to_string()
}

/// Log in with username and password. Sets the session and device cookies,
/// or, for an admin with TOTP, answers `202` with a pending token for the
/// second step and sets nothing.
///
/// Unknown usernames, wrong passwords and suspended accounts get the same
/// response, after the same amount of work.
#[utoipa::path(post, path = "/api/v1/auth/login", tag = "auth",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Logged in; session and device cookies set", body = MeResponse),
        (status = 202, description = "Password correct, TOTP code needed: no session yet", body = TotpChallenge),
        (status = 401, description = "`auth.invalid_credentials`", body = ErrorBody),
        (status = 403, description = "`auth.origin_mismatch`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn login(
    State(state): State<AppState>,
    client: Client,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let now = state.clock.now();
    let username = normalize_username(&body.username);
    let device = presented_device(&state, &jar, client.cookies, &username).await?;

    let attempt = AttemptKey {
        username_hash: &crypto::username_key(&username),
        ip: &request::throttle_key(client.ip),
        device: device.as_deref(),
    };
    let LoginAttempt::Allowed(attempt) = state.db.begin_login_attempt(&attempt, now).await? else {
        tracing::warn!(ip = %client.ip, "login throttled");
        return Err(ApiError::TooManyAttempts);
    };

    let (hash, account) = match state.db.find_credentials(&username).await? {
        Some(found) => (
            found.password_hash,
            found.active.then_some((found.subject, found.totp_enabled)),
        ),
        None => (crypto::DUMMY_HASH.to_owned(), None),
    };
    let password = body.password;
    let verified = state
        .run_hashing(move || crypto::verify_password(&password, &hash))
        .await?;

    let Some((subject, totp_enabled)) = account.filter(|_| verified) else {
        // Only well-formed usernames are recorded: people sometimes type
        // their password into the username field.
        let target = parse_username(&username).ok();
        state
            .db
            .audit(
                &AuditEntry {
                    actor_kind: ActorKind::Anonymous,
                    actor_name: None,
                    action: "auth.login_failed",
                    target: Some(target.as_deref().unwrap_or("<invalid>")),
                    details: Some(&ip_details(client.ip)),
                },
                now,
            )
            .await?;
        return Err(ApiError::InvalidCredentials);
    };

    state.db.finish_login_attempt(attempt, true).await?;
    if totp_enabled {
        let challenge = totp_challenge(&state, subject.id).await?;
        common::audit(
            &state,
            &subject,
            "auth.login_second_factor_required",
            None,
            Some(json!({ "ip": client.ip })),
        )
        .await?;
        return Ok((StatusCode::ACCEPTED, Json(challenge)).into_response());
    }
    common::audit(
        &state,
        &subject,
        "auth.login",
        None,
        Some(json!({ "ip": client.ip })),
    )
    .await?;
    let jar = start_session(&state, jar, &headers, &client, &subject).await?;
    let me = me_response(&state, subject, None).await?;
    Ok((jar, Json(me)).into_response())
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoginTotpRequest {
    /// From the `202` answer of `POST /api/v1/auth/login`.
    #[schema(value_type = String, format = Password)]
    pending_token: SecretString,
    /// Six-digit TOTP code or a recovery code.
    #[schema(value_type = String, format = Password)]
    code: SecretString,
}

/// Second login step: the TOTP code (or a recovery code) for a pending
/// login. Starts the session. The pending token works once; wrong codes are
/// throttled like logins.
#[utoipa::path(post, path = "/api/v1/auth/login/totp", tag = "auth",
    request_body = LoginTotpRequest,
    responses(
        (status = 200, description = "Logged in; session and device cookies set", body = MeResponse),
        (status = 401, description = "`auth.invalid_code`, `auth.login_expired` (start again with the password)", body = ErrorBody),
        (status = 403, description = "`auth.origin_mismatch`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn login_totp(
    State(state): State<AppState>,
    client: Client,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<LoginTotpRequest>,
) -> Result<(CookieJar, Json<MeResponse>), ApiError> {
    let now = state.clock.now();
    let token_hash = crypto::hash_token(secrecy::ExposeSecret::expose_secret(&body.pending_token));
    // Claim the pending login before any factor is looked at: an unknown,
    // expired or already used token never checks a code or burns a
    // recovery code, and of two racing requests only one gets this far.
    let Some(pending) = state.db.claim_pending_login(&token_hash, now).await? else {
        return Err(ApiError::LoginExpired);
    };
    let Some(admin) = state.db.get_admin(pending.admin_id).await? else {
        return Err(ApiError::LoginExpired);
    };
    let subject = Subject {
        kind: SubjectKind::Admin,
        id: admin.id,
        username: admin.username,
        display_name: admin.display_name,
    };
    let checked = check_second_factor(&state, &client, &subject, &body.code).await;
    let factor = match checked {
        Ok(SecondFactorCheck::Passed(factor)) => factor,
        Ok(SecondFactorCheck::Failed) => {
            state
                .db
                .release_pending_login(&token_hash, &pending)
                .await?;
            return Err(ApiError::InvalidCode);
        }
        Err(err) => {
            // Throttled or failed: the admin may retry until expiry.
            state
                .db
                .release_pending_login(&token_hash, &pending)
                .await?;
            return Err(err);
        }
    };
    common::audit(
        &state,
        &subject,
        "auth.login",
        None,
        Some(json!({ "ip": client.ip, "secondFactor": factor.as_str() })),
    )
    .await?;
    let jar = start_session(&state, jar, &headers, &client, &subject).await?;
    let me = me_response(&state, subject, None).await?;
    Ok((jar, Json(me)))
}

/// End the current session, if any, and clear the session cookie. Public so
/// that it always clears the cookie, even for an expired session.
#[utoipa::path(post, path = "/api/v1/auth/logout", tag = "auth",
    responses(
        (status = 204, description = "Logged out; session cookie cleared"),
        (status = 403, description = "`auth.origin_mismatch`", body = ErrorBody),
    ))]
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Result<(StatusCode, CookieJar), ApiError> {
    let scheme = CookieScheme::for_request(&headers, state.settings.behind_proxy);
    let (jar, subject) = end_session(&state, jar, &headers, scheme).await?;
    if let Some(subject) = subject {
        state
            .db
            .audit(
                &AuditEntry {
                    actor_kind: actor_kind(subject.kind),
                    actor_name: Some(&subject.username),
                    action: "auth.logout",
                    target: None,
                    details: None,
                },
                state.clock.now(),
            )
            .await?;
    }
    Ok((StatusCode::NO_CONTENT, jar))
}

/// The account behind the current session.
#[utoipa::path(get, path = "/api/v1/auth/me", tag = "auth",
    responses(
        (status = 200, body = MeResponse),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
    ))]
pub async fn me(
    AnySession(session): AnySession,
    State(state): State<AppState>,
) -> Result<Json<MeResponse>, ApiError> {
    let unlocked_until = session.private_unlocked_until;
    let is_admin = session.subject.kind == SubjectKind::Admin;
    let elevated_until = session.elevated_until;
    let mut me = me_response(&state, session.subject, session.origin_admin_id).await?;
    if let Some(private) = me.private_category.as_mut() {
        *private = PrivateCategory::of(unlocked_until, state.clock.now());
    }
    if is_admin {
        me.elevated_until = elevated_until.filter(|until| *until > state.clock.now());
    }
    Ok(Json(me))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkedMemberRequest {
    /// `null` unlinks.
    member_id: Option<i64>,
    /// The member's password: proof that the profile is the admin's own.
    /// Required when linking.
    #[schema(value_type = Option<String>, format = Password)]
    member_password: Option<SecretString>,
}

/// Link the admin's own member profile (or unlink it). Linking needs the
/// member's password, so an admin cannot take over someone else's profile.
/// A member is linked to at most one admin; retired members cannot be linked.
#[utoipa::path(put, path = "/api/v1/auth/linked-member", tag = "auth",
    request_body = LinkedMemberRequest,
    responses(
        (status = 200, body = MeResponse),
        (status = 403, description = "`auth.forbidden`", body = ErrorBody),
        (status = 404, description = "`member.not_found`", body = ErrorBody),
        (status = 409, description = "`member.already_linked`, `member.retired`", body = ErrorBody),
        (status = 422, description = "`auth.member_password_invalid`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn put_linked_member(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    client: Client,
    Json(body): Json<LinkedMemberRequest>,
) -> Result<Json<MeResponse>, ApiError> {
    let member = match body.member_id {
        Some(id) => {
            let member = state
                .db
                .get_member(id)
                .await?
                .ok_or(ApiError::NotFound("member.not_found"))?;
            if member.status == MemberStatus::Retired {
                return Err(ApiError::Conflict("member.retired"));
            }
            let password = body
                .member_password
                .ok_or(ApiError::Validation("auth.member_password_invalid"))?;
            let hash = state.db.member_password_hash(member.id).await?;
            if !common::verify_password(&state, &client, &member.username, hash, password).await? {
                return Err(ApiError::Validation("auth.member_password_invalid"));
            }
            Some(member)
        }
        None => None,
    };
    state
        .db
        .set_linked_member(session.subject.id, member.as_ref().map(|m| m.id))
        .await?;
    let (action, target) = match &member {
        Some(m) => ("admin.member_linked", Some(m.username.as_str())),
        None => ("admin.member_unlinked", None),
    };
    common::audit(&state, &session.subject, action, target, None).await?;
    Ok(Json(me_response(&state, session.subject, None).await?))
}

/// Switch between the admin account and its linked member, rotating the
/// session token. Admin → member needs an active linked member. Member view
/// → admin needs `POST /api/v1/auth/elevate` within the last five minutes.
#[utoipa::path(post, path = "/api/v1/auth/switch", tag = "auth",
    responses(
        (status = 200, description = "Switched; new session cookie set", body = MeResponse),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (a member's own session), `auth.elevation_required`", body = ErrorBody),
        (status = 409, description = "`auth.no_linked_member`, `auth.linked_member_inactive`", body = ErrorBody),
    ))]
pub async fn switch(
    AnySession(session): AnySession,
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Result<(CookieJar, Json<MeResponse>), ApiError> {
    let scheme = CookieScheme::for_request(&headers, state.settings.behind_proxy);
    let now = state.clock.now();
    match (session.subject.kind, session.origin_admin_id) {
        (SubjectKind::Admin, _) => {
            let linked = state
                .db
                .get_admin(session.subject.id)
                .await?
                .and_then(|a| a.linked_member)
                .ok_or(ApiError::Conflict("auth.no_linked_member"))?;
            if !linked.active {
                return Err(ApiError::Conflict("auth.linked_member_inactive"));
            }
            let jar = rotate_session(
                &state,
                jar,
                scheme,
                &session,
                NextSession {
                    kind: SubjectKind::Member,
                    id: linked.id,
                    origin_admin_id: Some(session.subject.id),
                    elevated_until: None,
                },
            )
            .await?;
            common::audit(
                &state,
                &session.subject,
                "auth.switched_to_member",
                Some(&linked.username),
                None,
            )
            .await?;
            let member = Subject {
                kind: SubjectKind::Member,
                id: linked.id,
                username: linked.username,
                display_name: linked.display_name,
            };
            Ok((
                jar,
                Json(me_response(&state, member, Some(session.subject.id)).await?),
            ))
        }
        (SubjectKind::Member, Some(admin_id)) => {
            if session.elevated_until.is_none_or(|until| until <= now) {
                return Err(ApiError::ElevationRequired);
            }
            let admin = state
                .db
                .get_admin(admin_id)
                .await?
                .filter(|a| !a.suspended && !a.removed)
                .ok_or(ApiError::Unauthenticated)?;
            let jar = rotate_session(
                &state,
                jar,
                scheme,
                &session,
                NextSession {
                    kind: SubjectKind::Admin,
                    id: admin_id,
                    origin_admin_id: None,
                    elevated_until: None,
                },
            )
            .await?;
            let subject = Subject {
                kind: SubjectKind::Admin,
                id: admin.id,
                username: admin.username,
                display_name: admin.display_name,
            };
            common::audit(
                &state,
                &subject,
                "auth.switched_to_admin",
                Some(&session.subject.username),
                None,
            )
            .await?;
            Ok((jar, Json(me_response(&state, subject, None).await?)))
        }
        (SubjectKind::Member, None) => Err(ApiError::Forbidden),
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ElevateRequest {
    /// The admin's password.
    #[schema(value_type = String, format = Password)]
    password: SecretString,
    /// TOTP or recovery code; required when the admin has TOTP on.
    #[schema(value_type = Option<String>, format = Password)]
    code: Option<SecretString>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ElevateResponse {
    /// Elevated until then: from a member view, `POST /api/v1/auth/switch`
    /// back to admin works; in an admin session, the actions that need a
    /// fresh re-authentication do (spec 0005 M10 2).
    elevated_until: DateTime<Utc>,
}

/// Re-authenticate as the admin (password, plus TOTP when on), rotating the
/// session token, for five minutes. From a member view this allows
/// switching back to the admin account; in an admin session it allows the
/// actions that need a fresh re-authentication (spec 0005 M10 2: adopting
/// an existing service account, switching account writes live, resuming
/// them, approving a held plan).
#[utoipa::path(post, path = "/api/v1/auth/elevate", tag = "auth",
    request_body = ElevateRequest,
    responses(
        (status = 200, description = "Re-authenticated; new session cookie set", body = ElevateResponse),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (a member's own session)", body = ErrorBody),
        (status = 422, description = "`auth.current_password_invalid`, `auth.invalid_code`, `auth.code_required`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
    ))]
pub async fn elevate(
    AnySession(session): AnySession,
    State(state): State<AppState>,
    client: Client,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<ElevateRequest>,
) -> Result<(CookieJar, Json<ElevateResponse>), ApiError> {
    let admin_id = match (session.subject.kind, session.origin_admin_id) {
        (SubjectKind::Admin, _) => session.subject.id,
        (SubjectKind::Member, Some(admin_id)) => admin_id,
        (SubjectKind::Member, None) => return Err(ApiError::Forbidden),
    };
    let admin = state
        .db
        .get_admin(admin_id)
        .await?
        .filter(|a| !a.suspended && !a.removed)
        .ok_or(ApiError::Unauthenticated)?;
    let subject = Subject {
        kind: SubjectKind::Admin,
        id: admin.id,
        username: admin.username,
        display_name: admin.display_name,
    };
    let hash = state.db.admin_password_hash(admin_id).await?;
    if !common::verify_password(&state, &client, &subject.username, hash, body.password).await? {
        common::audit(
            &state,
            &subject,
            "auth.elevate_failed",
            None,
            Some(json!({ "ip": client.ip, "reason": "password" })),
        )
        .await?;
        return Err(ApiError::Validation("auth.current_password_invalid"));
    }
    if admin.totp_enabled {
        let code = body
            .code
            .ok_or(ApiError::Validation("auth.code_required"))?;
        if check_second_factor(&state, &client, &subject, &code).await? == SecondFactorCheck::Failed
        {
            return Err(ApiError::Validation("auth.invalid_code"));
        }
    }
    let elevated_until = state.clock.now() + ELEVATION_WINDOW;
    let scheme = CookieScheme::for_request(&headers, state.settings.behind_proxy);
    let jar = rotate_session(
        &state,
        jar,
        scheme,
        &session,
        NextSession {
            kind: session.subject.kind,
            id: session.subject.id,
            origin_admin_id: session.origin_admin_id,
            elevated_until: Some(elevated_until),
        },
    )
    .await?;
    common::audit(
        &state,
        &subject,
        "auth.elevated",
        None,
        Some(json!({ "ip": client.ip })),
    )
    .await?;
    Ok((jar, Json(ElevateResponse { elevated_until })))
}
