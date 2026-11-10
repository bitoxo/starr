//! Pieces shared by the member, invitation and profile endpoints: DTOs for
//! entitlements and sessions, input validation mapped to error codes, audit
//! helpers and throttling of token endpoints.

use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use starr_core::auth::{
    DISPLAY_NAME_MAX_CHARS, PasswordError, Subject, normalize_username, parse_username,
    validate_password,
};
use starr_core::members::{Category, Entitlement, RequestMode, parse_email};
use starr_db::audit::AuditEntry;
use starr_db::auth::{AttemptKey, LoginAttempt};
use starr_db::members::SessionInfo;
use utoipa::ToSchema;

use super::auth::actor_kind;
use super::error::ApiError;
use crate::AppState;
use crate::auth::{Client, Session, crypto, request};

/// Category of media (fixed in code).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = Category)]
pub enum CategoryDto {
    Movies,
    Series,
    Music,
    /// Shown as "NSFW". Off by default; only admins can enable it.
    Adult,
}

impl From<Category> for CategoryDto {
    fn from(c: Category) -> Self {
        match c {
            Category::Movies => Self::Movies,
            Category::Series => Self::Series,
            Category::Music => Self::Music,
            Category::Adult => Self::Adult,
        }
    }
}

impl From<CategoryDto> for Category {
    fn from(c: CategoryDto) -> Self {
        match c {
            CategoryDto::Movies => Self::Movies,
            CategoryDto::Series => Self::Series,
            CategoryDto::Music => Self::Music,
            CategoryDto::Adult => Self::Adult,
        }
    }
}

/// A consequence of an admin action beyond the record it changes (spec 0008
/// F8): the confirmation dialog and the toast name these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Effect {
    /// A new address of the category's source: its quality profile, root
    /// folder and metadata profile stay, and the lists read from the new
    /// address decide; what that server does not offer is cleared and the
    /// category's requests close until an admin picks again (ADM-01).
    SourceSettingsRechecked { category: CategoryDto },
    /// The category stops taking requests (it has no complete source any
    /// more).
    RequestsClose { category: CategoryDto },
    /// Requests already sent to the source keep finishing there.
    OpenRequestsFinish { count: usize },
    /// The source's settings are kept and come back when the same instance
    /// is the category's source again (ADM-02).
    SettingsKept { category: CategoryDto },
}

/// The action has consequences (`effects`) and was not carried out: send
/// it again with `confirm` once the admin agreed (409 `*.confirm_required`).
#[derive(Debug, Serialize, ToSchema)]
pub struct ConfirmRequired {
    #[schema(example = "categories.confirm_required")]
    pub code: &'static str,
    /// Present with `*.confirm_required`; other 409 codes of the same
    /// endpoint (e.g. `services.name_taken`) answer the plain error body.
    #[schema(required = false)]
    pub effects: Vec<Effect>,
}

impl axum::response::IntoResponse for ConfirmRequired {
    fn into_response(self) -> axum::response::Response {
        (axum::http::StatusCode::CONFLICT, axum::Json(self)).into_response()
    }
}

/// Whether requests need an admin's approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = RequestMode)]
pub enum RequestModeDto {
    Auto,
    Approval,
}

impl From<RequestMode> for RequestModeDto {
    fn from(m: RequestMode) -> Self {
        match m {
            RequestMode::Auto => Self::Auto,
            RequestMode::Approval => Self::Approval,
        }
    }
}

impl From<RequestModeDto> for RequestMode {
    fn from(m: RequestModeDto) -> Self {
        match m {
            RequestModeDto::Auto => Self::Auto,
            RequestModeDto::Approval => Self::Approval,
        }
    }
}

/// A member's entitlement for one whole category.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = Entitlement)]
pub struct EntitlementDto {
    pub category: CategoryDto,
    pub enabled: bool,
    pub request_mode: RequestModeDto,
}

impl From<Entitlement> for EntitlementDto {
    fn from(e: Entitlement) -> Self {
        Self {
            category: e.category.into(),
            enabled: e.enabled,
            request_mode: e.request_mode.into(),
        }
    }
}

impl From<EntitlementDto> for Entitlement {
    fn from(e: EntitlementDto) -> Self {
        Self {
            category: e.category.into(),
            enabled: e.enabled,
            request_mode: e.request_mode.into(),
        }
    }
}

pub fn to_entitlements(dtos: &[EntitlementDto]) -> Vec<Entitlement> {
    dtos.iter().copied().map(Entitlement::from).collect()
}

pub fn to_dtos(entitlements: Vec<Entitlement>) -> Vec<EntitlementDto> {
    entitlements.into_iter().map(EntitlementDto::from).collect()
}

/// One login session of an account.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    id: i64,
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    ip: Option<String>,
    user_agent: Option<String>,
    /// The session making this request.
    current: bool,
}

impl SessionDto {
    pub fn new(session: SessionInfo, current: &[u8]) -> Self {
        Self {
            current: session.token_hash == current,
            id: session.id,
            created_at: session.created_at,
            last_seen_at: session.last_seen_at,
            ip: session.ip,
            user_agent: session.user_agent,
        }
    }
}

/// Web UI page that consumes password tokens.
pub const SET_PASSWORD_PATH: &str = "/set-password";

/// A one-time link to show to an admin.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkDto {
    /// Absolute when `STARR_PUBLIC_URL` is set, otherwise a path.
    url: String,
    expires_at: DateTime<Utc>,
}

impl LinkDto {
    /// Link to the web UI page `path` (starting with `/`) carrying `token`.
    pub fn new(
        state: &AppState,
        path: &str,
        token: &SecretString,
        expires_at: DateTime<Utc>,
    ) -> Self {
        Self {
            url: link_url(state.settings.public_url.as_ref(), path, token),
            expires_at,
        }
    }
}

/// Link to the web UI page `path` (starting with `/`) carrying `token`:
/// absolute under `public_url`, otherwise just the path.
pub fn link_url(public_url: Option<&url::Url>, path: &str, token: &SecretString) -> String {
    let path = format!("{path}/{}", token.expose_secret());
    match public_url {
        Some(base) => format!("{}{}", base.as_str().trim_end_matches('/'), path),
        None => path,
    }
}

/// Check the password of the logged-in account (re-authentication),
/// counted against the login throttle of `username` like a login. `hash`
/// is `None` if the account has no usable password.
pub async fn verify_password(
    state: &AppState,
    client: &Client,
    username: &str,
    hash: Option<String>,
    password: SecretString,
) -> Result<bool, ApiError> {
    Ok(check_password(state, client, username, hash, password)
        .await?
        .is_some())
}

/// [`verify_password`], handing the password back when it is right (for a
/// password handoff, ADR 0010).
pub async fn check_password(
    state: &AppState,
    client: &Client,
    username: &str,
    hash: Option<String>,
    password: SecretString,
) -> Result<Option<SecretString>, ApiError> {
    let attempt = AttemptKey {
        username_hash: &crypto::username_key(&normalize_username(username)),
        ip: &request::throttle_key(client.ip),
        device: None,
    };
    let LoginAttempt::Allowed(attempt) = state
        .db
        .begin_login_attempt(&attempt, state.clock.now())
        .await?
    else {
        return Err(ApiError::TooManyAttempts);
    };
    let hash = hash.unwrap_or_else(|| crypto::DUMMY_HASH.to_owned());
    let (verified, password) = state
        .run_hashing(move || (crypto::verify_password(&password, &hash), password))
        .await?;
    if verified {
        state.db.finish_login_attempt(attempt, true).await?;
    }
    Ok(verified.then_some(password))
}

pub fn username(input: &str) -> Result<String, ApiError> {
    parse_username(input).map_err(|_| ApiError::Validation("validation.username_invalid"))
}

pub fn email(input: &str) -> Result<String, ApiError> {
    parse_email(input).map_err(|_| ApiError::Validation("validation.email_invalid"))
}

/// Trimmed display name; `None`/empty falls back to `fallback`.
pub fn display_name(input: Option<&str>, fallback: &str) -> Result<String, ApiError> {
    match input.map(str::trim) {
        None | Some("") => Ok(fallback.to_owned()),
        Some(name) if name.chars().count() <= DISPLAY_NAME_MAX_CHARS => Ok(name.to_owned()),
        Some(_) => Err(ApiError::Validation("validation.display_name_invalid")),
    }
}

pub fn new_password(password: &SecretString) -> Result<(), ApiError> {
    validate_password(password.expose_secret()).map_err(|err| {
        ApiError::Validation(match err {
            PasswordError::TooShort => "auth.password_too_short",
            PasswordError::TooLong => "auth.password_too_long",
        })
    })
}

/// Hash a validated password on the hashing pool, handing the password
/// back (for a password handoff, ADR 0010).
pub async fn hash_keeping(
    state: &AppState,
    password: SecretString,
) -> Result<(String, SecretString), ApiError> {
    let (hashed, password) = state
        .run_hashing(move || (crypto::hash_password(&password), password))
        .await?;
    let hashed = hashed.map_err(|err| {
        tracing::error!(error = %err, "hashing password failed");
        ApiError::Internal
    })?;
    Ok((hashed, password))
}

/// Hash a validated password on the hashing pool.
pub async fn hash(state: &AppState, password: SecretString) -> Result<String, ApiError> {
    state
        .run_hashing(move || crypto::hash_password(&password))
        .await?
        .map_err(|err| {
            tracing::error!(error = %err, "hashing password failed");
            ApiError::Internal
        })
}

/// M10 1 (maintainer decision 2026-11-03): after `admin` did something
/// that can leave an active admin without TOTP (`trigger`: TOTP off, admin
/// created or reactivated), account writes pause on every live instance
/// while that is so; each pause is audited with the reason.
pub async fn pause_writes_for_totp_gap(
    state: &AppState,
    admin: &Subject,
    trigger: &str,
) -> Result<(), ApiError> {
    starr_worker::provisioning::pause_for_totp_gap(
        &state.db,
        (starr_db::audit::ActorKind::Admin, Some(&admin.username)),
        trigger,
    )
    .await?;
    Ok(())
}

/// Actions that need the admin to have re-authenticated within the last
/// five minutes (`POST /api/v1/auth/elevate`, spec 0005 M10 2).
pub fn require_elevation(state: &AppState, session: &Session) -> Result<(), ApiError> {
    if session
        .elevated_until
        .is_some_and(|until| until > state.clock.now())
    {
        Ok(())
    } else {
        Err(ApiError::ElevationRequired)
    }
}

/// Write an audit entry for an action of `actor`.
pub async fn audit(
    state: &AppState,
    actor: &Subject,
    action: &str,
    target: Option<&str>,
    details: Option<serde_json::Value>,
) -> Result<(), ApiError> {
    let details = details.map(|d| d.to_string());
    state
        .db
        .audit(
            &AuditEntry {
                actor_kind: actor_kind(actor.kind),
                actor_name: Some(&actor.username),
                action,
                target,
                details: details.as_deref(),
            },
            state.clock.now(),
        )
        .await?;
    Ok(())
}

/// Write an audit entry for an action of the session's subject. In an
/// admin's member view the entry also names the admin (`viaAdmin`), so
/// actions taken as a member stay attributable to the admin behind them.
pub async fn audit_session(
    state: &AppState,
    session: &crate::auth::Session,
    action: &str,
    target: Option<&str>,
    details: Option<serde_json::Value>,
) -> Result<(), ApiError> {
    let mut details = details;
    if let Some(origin) = session.origin_admin_id {
        let admin = state
            .db
            .get_admin(origin)
            .await?
            .map_or_else(|| origin.to_string(), |a| a.username);
        let mut object = match details {
            Some(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        object.insert("viaAdmin".to_owned(), serde_json::Value::String(admin));
        details = Some(serde_json::Value::Object(object));
    }
    audit(state, &session.subject, action, target, details).await
}

/// Browsers mark requests a foreign page triggers (e.g. `<img src>`); such
/// requests have no business with token links (invitations, set-password links).
pub fn refuse_cross_site(headers: &HeaderMap) -> Result<(), ApiError> {
    let cross_site = headers
        .get("sec-fetch-site")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"cross-site"));
    if cross_site {
        Err(ApiError::Forbidden)
    } else {
        Ok(())
    }
}

/// Count a use of a public token endpoint against its own throttle: per
/// client IP and per token, apart from logins (see
/// [`starr_db::Database::begin_token_attempt`]). Returns the attempt id to
/// mark successful with [`token_attempt_succeeded`].
pub async fn begin_token_attempt(
    state: &AppState,
    client: &Client,
    token: &str,
) -> Result<i64, ApiError> {
    // A hash of the token, never the token itself, and never equal to the
    // stored token hash.
    let key = crypto::username_key(&format!("token:{token}"));
    match state
        .db
        .begin_token_attempt(&key, &request::throttle_key(client.ip), state.clock.now())
        .await?
    {
        LoginAttempt::Allowed(id) => Ok(id),
        LoginAttempt::Throttled => {
            tracing::warn!(ip = %client.ip, "token endpoint throttled");
            Err(ApiError::TooManyAttempts)
        }
    }
}

pub async fn token_attempt_succeeded(state: &AppState, attempt: i64) -> Result<(), ApiError> {
    state.db.finish_login_attempt(attempt, true).await?;
    Ok(())
}

/// Where an available title can be watched (O3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpenInLink {
    /// Adapter type, e.g. `jellyfin` or `emby`.
    service: String,
    /// The instance's name as the admin set it.
    name: String,
    /// The title's page in the service's web client.
    url: String,
}

/// "Open in" links from a title's present inventory rows; services whose
/// adapter builds no URL give none.
pub fn open_in_links(links: Vec<starr_db::inventory::TitleLink>) -> Vec<OpenInLink> {
    links
        .into_iter()
        .filter_map(|link| {
            let url = starr_worker::adapters::item_url(
                &link.adapter_type,
                &starr_core::library::ItemLink {
                    public_url: &link.web_url,
                    server_id: link.server_id.as_deref(),
                    remote_id: &link.remote_id,
                },
            )?;
            Some(OpenInLink {
                service: link.adapter_type,
                name: link.service_name,
                url,
            })
        })
        .collect()
}
