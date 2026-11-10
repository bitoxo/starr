//! Notifications (spec 0003 O1, O2): the member's own stream and email
//! preferences under `/api/v1/notifications` and `/api/v1/me/...`; the
//! admin stream and the channels under `/api/v1/admin/...`.
//!
//! Streams are personal: member endpoints only ever touch the session's
//! own stream, admin endpoints only the admin stream (admins never read a
//! member's stream). Channel secrets (SMTP password, webhook URL) are
//! sealed for the worker on entry and never returned or opened here (ADR
//! 0003 amendment 1); a test only queues `notifications.deliver`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use starr_core::auth::SubjectKind;
use starr_core::jobs::{JobKind, Trigger};
use starr_core::members::Category;
use starr_core::notifications::{ChannelKind, EmailSettings, NotificationKind, SmtpSecurity};
use starr_db::audit::{ActorKind, AuditEntry};
use starr_db::jobs::NewJob;
use starr_db::notifications::{
    ChannelRecord, ChannelUpdate, NewChannel, NotificationRecord, Stream,
};
use starr_db::services::StoredCredential;
use utoipa::{IntoParams, ToSchema};

use super::common::CategoryDto;
use super::error::{ApiError, ErrorBody};
use super::privacy;
use super::requests::RequestStateDto;
use crate::AppState;
use crate::auth::{AdminSession, MemberSession, Session};

const DEFAULT_LIMIT: u32 = 30;
const MAX_LIMIT: u32 = 100;
const NAME_MAX_CHARS: usize = 100;
const FIELD_MAX_CHARS: usize = 255;
const SECRET_MAX_CHARS: usize = 2048;

macro_rules! dto_enum {
    ($(#[$meta:meta])* $name:ident as $schema:ident from $core:ty { $($variant:ident),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
        #[serde(rename_all = "snake_case")]
        #[schema(as = $schema)]
        pub enum $name {
            $($variant),+
        }

        impl From<$core> for $name {
            fn from(v: $core) -> Self {
                match v {
                    $(<$core>::$variant => Self::$variant),+
                }
            }
        }

        impl From<$name> for $core {
            fn from(v: $name) -> Self {
                match v {
                    $($name::$variant => Self::$variant),+
                }
            }
        }
    };
}

dto_enum!(
    /// What a notification is about. `approval_pending`,
    /// `acquisition_failed`, `import_blocked`, `account_privileged` and
    /// `account_locked` belong to the admin stream.
    NotificationKindDto as NotificationKind from NotificationKind {
        RequestApproved, RequestRejected, RequestRevoked, RequestPartiallyAvailable, RequestAvailable,
        RequestFailed, FollowRequested, FollowCapped, AccountActivate, ApprovalPending, AcquisitionFailed,
        ImportBlocked, AccountPrivileged, AccountLocked,
    }
);

dto_enum!(
    /// Kind of a notification channel.
    ChannelKindDto as NotificationChannelKind from ChannelKind {
        Email, Webhook, Discord,
    }
);

dto_enum!(
    /// How the SMTP connection is secured.
    SmtpSecurityDto as SmtpSecurity from SmtpSecurity {
        Starttls, Tls, None,
    }
);

/// One notification.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotificationDto {
    id: i64,
    kind: NotificationKindDto,
    created_at: String,
    read: bool,
    title: String,
    year: Option<i64>,
    category: String,
    /// The request it is about (request kinds, `approval_pending`).
    request_id: Option<i64>,
    /// The acquisition it is about (`acquisition_failed`, `import_blocked`).
    acquisition_id: Option<i64>,
    seasons: Vec<u32>,
    /// Reject reason, why an acquisition failed, or the source's message
    /// about a blocked import.
    reason: Option<String>,
    /// Admin stream only: who asked.
    member: Option<String>,
    /// Admin stream, about the private category while the session has not
    /// unlocked it (spec 0007 Q9): title, year, seasons, reason and member
    /// are left out; show the neutral line ("A request in a private
    /// category was approved"). Members never get these: their private
    /// notifications are left out until they unlock (spec 0008 Q24 A).
    redacted: bool,
    /// Admin stream, `approval_pending`: the request's state now, so the
    /// bell can say how it turned out (spec 0008 slice 12). Absent when the
    /// request no longer exists and on member streams.
    request_state: Option<RequestStateDto>,
}

/// How a page shows notifications of the private category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrivateShown {
    /// The gate is open: as they are.
    Open,
    /// Neutral lines (Q9: a locked admin), counted like the others.
    Neutral,
    /// Not at all: a member who has not unlocked (spec 0008 Q24 A), has no
    /// entitlement, or the category is off.
    Absent,
}

impl NotificationDto {
    fn new(mut n: NotificationRecord, shown: PrivateShown) -> Self {
        let redacted = shown != PrivateShown::Open && n.payload.is_private();
        if redacted {
            n.payload = n.payload.neutral();
        }
        Self {
            redacted,
            id: n.id,
            kind: n.kind.into(),
            created_at: n.created_at.to_rfc3339(),
            read: n.read_at.is_some(),
            title: n.payload.title,
            year: n.payload.year,
            category: n.payload.category,
            request_id: n.payload.request_id,
            acquisition_id: n.payload.acquisition_id,
            seasons: n.payload.seasons,
            reason: n.payload.reason,
            member: n.payload.member,
            request_state: None,
        }
    }
}

/// A page of a stream, newest first.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotificationPage {
    items: Vec<NotificationDto>,
    /// Unread notifications in the whole stream.
    unread: u64,
    /// Pass as `before` for the next page; absent on the last one.
    next_before: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct PageQuery {
    /// Page size (1–100, default 30).
    limit: Option<u32>,
    /// Only notifications with an id below this (from `nextBefore`).
    before: Option<i64>,
}

/// Which notifications to mark read.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarkRead {
    /// These ids; absent: all of the stream.
    ids: Option<Vec<i64>>,
}

/// Unread count after marking.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnreadCount {
    unread: u64,
}

/// Queue `notifications.deliver` right away (deduplicated with a pending
/// one) when a change left deliveries due, so emails and webhooks do not
/// wait for the minute schedule.
pub(crate) async fn enqueue_due(state: &AppState) -> Result<(), ApiError> {
    let now = state.clock.now();
    if !state.db.has_due_deliveries(now).await? {
        return Ok(());
    }
    let kind = JobKind::NotificationsDeliver;
    let key = kind.unique_key();
    state
        .db
        .enqueue_job(
            &NewJob {
                kind: kind.name(),
                payload: "{}",
                unique_key: Some(&key),
                trigger: Trigger::Manual,
                run_after: now,
                max_attempts: kind.spec().max_attempts,
            },
            now,
        )
        .await?;
    state.wake.notify_one();
    Ok(())
}

/// The category a page leaves out entirely: not listed, not counted and
/// not marked read by "mark all" (spec 0008 Q24 A).
const fn left_out(shown: PrivateShown) -> Option<Category> {
    match shown {
        PrivateShown::Open | PrivateShown::Neutral => None,
        PrivateShown::Absent => Some(Category::Adult),
    }
}

/// How `session` sees the private category's notifications.
async fn private_shown(state: &AppState, session: &Session) -> Result<PrivateShown, ApiError> {
    if privacy::adult_open(state, session).await? {
        return Ok(PrivateShown::Open);
    }
    Ok(match session.subject.kind {
        // Admins see neutral rows until they unlock (Q9 B).
        SubjectKind::Admin => PrivateShown::Neutral,
        // Members: neither listed nor counted until they unlock (spec 0008
        // Q24 A), so the badge and the list always agree.
        SubjectKind::Member => PrivateShown::Absent,
    })
}

async fn page(
    state: &AppState,
    stream: Stream,
    query: &PageQuery,
    shown: PrivateShown,
) -> Result<NotificationPage, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let items = state
        .db
        .list_notifications_without(stream, query.before, limit, left_out(shown))
        .await?;
    let next_before = (items.len() == limit as usize)
        .then(|| items.last().map(|n| n.id))
        .flatten();
    // The admin bell names how each approval turned out (one query per page).
    let approval_ids: Vec<i64> = match stream {
        Stream::Admin => items
            .iter()
            .filter(|n| n.kind == NotificationKind::ApprovalPending)
            .filter_map(|n| n.payload.request_id)
            .collect(),
        Stream::Member(_) => Vec::new(),
    };
    let states = state.db.request_states(&approval_ids).await?;
    Ok(NotificationPage {
        unread: state
            .db
            .unread_notifications_without(stream, left_out(shown))
            .await?,
        items: items
            .into_iter()
            .map(|n| {
                let request_state = (n.kind == NotificationKind::ApprovalPending)
                    .then(|| n.payload.request_id.and_then(|id| states.get(&id)))
                    .flatten()
                    .map(|s| RequestStateDto::from(*s));
                NotificationDto {
                    request_state,
                    ..NotificationDto::new(n, shown)
                }
            })
            .collect(),
        next_before,
    })
}

async fn mark(
    state: &AppState,
    stream: Stream,
    body: &MarkRead,
    shown: PrivateShown,
) -> Result<UnreadCount, ApiError> {
    if body.ids.as_ref().is_some_and(|ids| ids.len() > 500) {
        return Err(ApiError::Validation("validation.notification_ids"));
    }
    state
        .db
        .mark_notifications_read_without(
            stream,
            body.ids.as_deref(),
            left_out(shown),
            state.clock.now(),
        )
        .await?;
    Ok(UnreadCount {
        unread: state
            .db
            .unread_notifications_without(stream, left_out(shown))
            .await?,
    })
}

/// The logged-in member's notifications, newest first (user story 6).
#[utoipa::path(get, path = "/api/v1/notifications", tag = "notifications",
    params(PageQuery),
    responses(
        (status = 200, body = NotificationPage),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_my_notifications(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> Result<Json<NotificationPage>, ApiError> {
    Ok(Json(
        page(
            &state,
            Stream::Member(session.subject.id),
            &query,
            private_shown(&state, &session).await?,
        )
        .await?,
    ))
}

/// Mark some or all of the member's notifications read. Ids of other
/// streams are ignored.
#[utoipa::path(post, path = "/api/v1/notifications/read", tag = "notifications",
    request_body = MarkRead,
    responses(
        (status = 200, body = UnreadCount),
        (status = 422, description = "`validation.notification_ids` (more than 500)", body = ErrorBody),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn mark_my_notifications_read(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Json(body): Json<MarkRead>,
) -> Result<Json<UnreadCount>, ApiError> {
    Ok(Json(
        mark(
            &state,
            Stream::Member(session.subject.id),
            &body,
            private_shown(&state, &session).await?,
        )
        .await?,
    ))
}

/// The member's email choices.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotificationPreferences {
    /// The member kinds that apply to this member now, in display order:
    /// only kinds of the categories the member may see and, for the
    /// private category, only while the session has unlocked it (spec 0008
    /// F2, PRV-01). The page lists exactly these.
    kinds: Vec<NotificationKindDto>,
    /// Categories whose follows request in the member's name (`series`,
    /// `music`), for the wording of `follow_requested`. Never the private
    /// category.
    follows: Vec<CategoryDto>,
    /// Kinds of `kinds` also sent by email. In-app is always on.
    email: Vec<NotificationKindDto>,
    /// An admin set up an email server (otherwise email cannot be chosen).
    email_available: bool,
    /// The address emails go to.
    email_address: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateNotificationPreferences {
    /// Member kinds to send by email (opt-in); admin kinds are refused.
    email: Vec<NotificationKindDto>,
}

/// What applies to a member: the member kinds to offer and the
/// categories their follows request in.
struct Applicable {
    kinds: Vec<NotificationKind>,
    follows: Vec<Category>,
}

/// The member kinds that apply to `session` now (spec 0008 F2, PRV-01):
/// follows only with a category that has them (series, music, or the
/// private category while unlocked), the weekly cap only while the private
/// category is open. Everything else applies to every member.
async fn applicable(state: &AppState, session: &Session) -> Result<Applicable, ApiError> {
    let entitled = state.db.entitlements(session.subject.id).await?;
    let switched_on = state.db.list_categories().await?;
    let follows: Vec<Category> = [Category::Series, Category::Music]
        .into_iter()
        .filter(|c| {
            entitled.iter().any(|e| e.category == *c && e.enabled)
                && switched_on.iter().any(|s| s.category == *c && s.enabled)
        })
        .collect();
    let private_open = privacy::adult_open(state, session).await?;
    let kinds = NotificationKind::MEMBER
        .into_iter()
        .filter(|kind| match kind {
            NotificationKind::FollowRequested => !follows.is_empty() || private_open,
            NotificationKind::FollowCapped => private_open,
            _ => true,
        })
        .collect();
    Ok(Applicable { kinds, follows })
}

async fn preferences(
    state: &AppState,
    session: &Session,
) -> Result<NotificationPreferences, ApiError> {
    let member_id = session.subject.id;
    let member = state
        .db
        .get_member(member_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    let applicable = applicable(state, session).await?;
    Ok(NotificationPreferences {
        email: state
            .db
            .email_preferences(member_id)
            .await?
            .into_iter()
            .filter(|k| applicable.kinds.contains(k))
            .map(Into::into)
            .collect(),
        kinds: applicable.kinds.into_iter().map(Into::into).collect(),
        follows: applicable.follows.into_iter().map(Into::into).collect(),
        email_available: state.db.email_channel_enabled().await?,
        email_address: member.email,
    })
}

/// Which notifications the member also gets by email.
#[utoipa::path(get, path = "/api/v1/me/notification-preferences", tag = "notifications",
    responses(
        (status = 200, body = NotificationPreferences),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn get_notification_preferences(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
) -> Result<Json<NotificationPreferences>, ApiError> {
    Ok(Json(preferences(&state, &session).await?))
}

/// Choose which notifications also come by email. Only kinds that apply
/// now (`kinds` of the answer) can be chosen; choices for kinds that do not
/// apply at the moment (the private category while locked) stay as they
/// are.
#[utoipa::path(put, path = "/api/v1/me/notification-preferences", tag = "notifications",
    request_body = UpdateNotificationPreferences,
    responses(
        (status = 200, body = NotificationPreferences),
        (status = 422, description = "`validation.notification_kind` (an admin kind, or one that does not apply to the member now)", body = ErrorBody),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn put_notification_preferences(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Json(body): Json<UpdateNotificationPreferences>,
) -> Result<Json<NotificationPreferences>, ApiError> {
    let mut kinds: Vec<NotificationKind> = body.email.into_iter().map(Into::into).collect();
    let applicable = applicable(&state, &session).await?;
    if kinds.iter().any(|k| !applicable.kinds.contains(k)) {
        return Err(ApiError::Validation("validation.notification_kind"));
    }
    // Choices the member cannot see now are kept, not dropped.
    kinds.extend(
        state
            .db
            .email_preferences(session.subject.id)
            .await?
            .into_iter()
            .filter(|k| !applicable.kinds.contains(k)),
    );
    state
        .db
        .set_email_preferences(session.subject.id, &kinds)
        .await?;
    Ok(Json(preferences(&state, &session).await?))
}

/// The admin stream (pending approvals, failed acquisitions), newest
/// first. Shared by all admins.
#[utoipa::path(get, path = "/api/v1/admin/notifications", tag = "notifications",
    params(PageQuery),
    responses(
        (status = 200, body = NotificationPage),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_admin_notifications(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> Result<Json<NotificationPage>, ApiError> {
    let shown = private_shown(&state, &session).await?;
    Ok(Json(page(&state, Stream::Admin, &query, shown).await?))
}

/// Mark some or all of the admin stream read (for every admin).
#[utoipa::path(post, path = "/api/v1/admin/notifications/read", tag = "notifications",
    request_body = MarkRead,
    responses(
        (status = 200, body = UnreadCount),
        (status = 422, description = "`validation.notification_ids` (more than 500)", body = ErrorBody),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn mark_admin_notifications_read(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Json(body): Json<MarkRead>,
) -> Result<Json<UnreadCount>, ApiError> {
    let shown = private_shown(&state, &session).await?;
    Ok(Json(mark(&state, Stream::Admin, &body, shown).await?))
}

// ---------------------------------------------------------------- channels

/// SMTP server settings (the password is set separately and never shown).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmailSettingsDto {
    host: String,
    port: u16,
    security: SmtpSecurityDto,
    username: Option<String>,
    from_address: String,
    from_name: Option<String>,
    /// Where admin notifications go by email; absent: none.
    admin_address: Option<String>,
}

impl From<EmailSettings> for EmailSettingsDto {
    fn from(s: EmailSettings) -> Self {
        Self {
            host: s.host,
            port: s.port,
            security: s.security.into(),
            username: s.username,
            from_address: s.from_address,
            from_name: s.from_name,
            admin_address: s.admin_address,
        }
    }
}

/// A notification channel. Secrets are never returned.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelDto {
    id: i64,
    kind: ChannelKindDto,
    name: String,
    enabled: bool,
    /// Email channels: the SMTP server.
    email: Option<EmailSettingsDto>,
    /// Webhooks: the host the URL points at (the URL itself is secret).
    host: Option<String>,
    /// A password (email) or URL (webhooks) is stored.
    has_secret: bool,
    /// Admin notifications sent here.
    admin_events: Vec<NotificationKindDto>,
    last_attempt_at: Option<String>,
    /// Outcome of the last delivery or test.
    last_ok: Option<bool>,
    last_error_code: Option<String>,
    last_error: Option<String>,
}

impl From<ChannelRecord> for ChannelDto {
    fn from(c: ChannelRecord) -> Self {
        let email = (c.kind == ChannelKind::Email)
            .then(|| serde_json::from_value::<EmailSettings>(c.settings.clone()).ok())
            .flatten()
            .map(Into::into);
        let host = c
            .settings
            .get("host")
            .and_then(Value::as_str)
            .filter(|_| c.kind != ChannelKind::Email)
            .map(str::to_owned);
        Self {
            id: c.id,
            kind: c.kind.into(),
            name: c.name,
            enabled: c.enabled,
            email,
            host,
            has_secret: c.has_secret,
            admin_events: c.admin_events.into_iter().map(Into::into).collect(),
            last_attempt_at: c.last_attempt_at.map(|t| t.to_rfc3339()),
            last_ok: c.last_ok,
            last_error_code: c.last_error_code,
            last_error: c.last_error,
        }
    }
}

/// A channel to create.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateChannel {
    kind: ChannelKindDto,
    name: String,
    #[serde(default = "enabled_default")]
    enabled: bool,
    /// Required for email.
    email: Option<EmailSettingsDto>,
    /// SMTP password (optional) or webhook URL (required).
    #[schema(value_type = Option<String>, format = Password)]
    secret: Option<SecretString>,
    /// Admin notifications to send here (admin kinds only).
    #[serde(default)]
    admin_events: Vec<NotificationKindDto>,
}

const fn enabled_default() -> bool {
    true
}

/// Changes to a channel; absent fields stay.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateChannel {
    name: Option<String>,
    enabled: Option<bool>,
    /// Email channels only.
    email: Option<EmailSettingsDto>,
    /// New SMTP password or webhook URL; empty removes the SMTP password.
    #[schema(value_type = Option<String>, format = Password)]
    secret: Option<SecretString>,
    admin_events: Option<Vec<NotificationKindDto>>,
}

/// A queued test.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestQueued {
    /// Follow it via `/api/v1/runs?jobId=`; the channel's `lastOk` shows
    /// the result.
    job_id: i64,
}

fn validate_name(raw: &str) -> Result<&str, ApiError> {
    let name = raw.trim();
    let len = name.chars().count();
    if len == 0 || len > NAME_MAX_CHARS || name.chars().any(char::is_control) {
        return Err(ApiError::Validation("validation.channel_name"));
    }
    Ok(name)
}

fn field(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn is_address(raw: &str) -> bool {
    let mut parts = raw.split('@');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(local), Some(domain), None)
            if !local.is_empty() && domain.contains('.') && !raw.chars().any(|c| c.is_whitespace() || c.is_control())
    ) && raw.len() <= FIELD_MAX_CHARS
}

/// Validate and normalise the SMTP settings.
fn validate_email(dto: &EmailSettingsDto) -> Result<EmailSettings, ApiError> {
    let host = dto.host.trim();
    if host.is_empty()
        || host.len() > FIELD_MAX_CHARS
        || host
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '@' | '?' | '#'))
    {
        return Err(ApiError::Validation("validation.smtp_host"));
    }
    if dto.port == 0 {
        return Err(ApiError::Validation("validation.smtp_port"));
    }
    let from_address = dto.from_address.trim();
    if !is_address(from_address) {
        return Err(ApiError::Validation("validation.from_address"));
    }
    let admin_address = field(dto.admin_address.as_deref());
    if admin_address.as_deref().is_some_and(|a| !is_address(a)) {
        return Err(ApiError::Validation("validation.admin_address"));
    }
    let username = field(dto.username.as_deref());
    let from_name = field(dto.from_name.as_deref());
    if [&username, &from_name].iter().any(|v| {
        v.as_ref()
            .is_some_and(|v| v.len() > FIELD_MAX_CHARS || v.chars().any(char::is_control))
    }) {
        return Err(ApiError::Validation("validation.smtp_field"));
    }
    Ok(EmailSettings {
        host: host.to_owned(),
        port: dto.port,
        security: dto.security.into(),
        username,
        from_address: from_address.to_owned(),
        from_name,
        admin_address,
    })
}

/// Validate a webhook URL; returns its host for display.
fn validate_webhook(kind: ChannelKind, raw: &SecretString) -> Result<String, ApiError> {
    use secrecy::ExposeSecret;
    let invalid = ApiError::Validation("validation.webhook_url");
    let raw = raw.expose_secret().trim();
    if raw.len() > SECRET_MAX_CHARS {
        return Err(invalid);
    }
    let url = url::Url::parse(raw).map_err(|_| invalid)?;
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() {
        return Err(invalid);
    }
    let host = url.host_str().ok_or(invalid)?.to_owned();
    if kind == ChannelKind::Discord && !url.path().contains("/api/webhooks/") {
        return Err(ApiError::Validation("validation.discord_url"));
    }
    Ok(host)
}

fn admin_events(events: &[NotificationKindDto]) -> Result<Vec<NotificationKind>, ApiError> {
    let kinds: Vec<NotificationKind> = events.iter().map(|&k| k.into()).collect();
    if kinds.iter().any(|k| !k.is_admin()) {
        return Err(ApiError::Validation("validation.notification_kind"));
    }
    Ok(kinds)
}

async fn seal(state: &AppState, secret: &SecretString) -> Result<StoredCredential, ApiError> {
    use secrecy::ExposeSecret;
    let trimmed = SecretString::from(secret.expose_secret().trim().to_owned());
    let (recipient, ciphertext) = state
        .credential_sealer()
        .await?
        .seal(&trimmed)
        .map_err(|err| {
            tracing::error!(error = %err, "sealing a channel secret failed");
            ApiError::Internal
        })?
        .into_parts();
    Ok(StoredCredential {
        ciphertext,
        recipient,
        class: starr_core::adapters::SecretClass::ServiceCredential,
    })
}

fn has_username(settings: &Value) -> bool {
    settings
        .get("username")
        .and_then(Value::as_str)
        .is_some_and(|u| !u.is_empty())
}

/// SMTP login needs both parts; one without the other would silently send
/// without logging in.
fn check_login(username: bool, password: bool) -> Result<(), ApiError> {
    if username == password {
        Ok(())
    } else {
        Err(ApiError::Validation("validation.smtp_login_incomplete"))
    }
}

fn is_blank(secret: &SecretString) -> bool {
    use secrecy::ExposeSecret;
    secret.expose_secret().trim().is_empty()
}

async fn audit(
    state: &AppState,
    admin: &AdminSession,
    action: &str,
    target: &str,
    details: &Value,
) -> Result<(), ApiError> {
    state
        .db
        .audit(
            &AuditEntry {
                actor_kind: ActorKind::Admin,
                actor_name: Some(&admin.0.subject.username),
                action,
                target: Some(target),
                details: Some(&details.to_string()),
            },
            state.clock.now(),
        )
        .await?;
    Ok(())
}

fn conflict(err: starr_db::DbError) -> ApiError {
    match err {
        starr_db::DbError::ChannelTaken => ApiError::Conflict("notifications.channel_taken"),
        other => other.into(),
    }
}

/// The notification channels.
#[utoipa::path(get, path = "/api/v1/admin/notification-channels", tag = "notifications",
    responses(
        (status = 200, body = Vec<ChannelDto>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_channels(
    _admin: AdminSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<ChannelDto>>, ApiError> {
    Ok(Json(
        state
            .db
            .list_channels()
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

/// Add a channel: the SMTP server (one) or a webhook / Discord webhook for
/// admin notifications (needs a fresh elevation). The secret is sealed for
/// the worker.
#[utoipa::path(post, path = "/api/v1/admin/notification-channels", tag = "notifications",
    request_body = CreateChannel,
    responses(
        (status = 201, body = ChannelDto),
        (status = 409, body = ErrorBody, description = "`notifications.channel_taken` (name used, or an email channel exists)"),
        (status = 422, body = ErrorBody, description = "`validation.channel_name`, `validation.smtp_settings_required`, `validation.smtp_host`, `validation.smtp_port`, `validation.from_address`, `validation.admin_address`, `validation.smtp_field`, `validation.smtp_login_incomplete` (username without password or the other way round), `validation.webhook_url`, `validation.discord_url`, `validation.notification_kind`"),
        (status = 503, body = ErrorBody, description = "`services.secrets_unavailable` (worker has not initialised the key yet)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin), `auth.elevation_required`", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn create_channel(
    admin: AdminSession,
    State(state): State<AppState>,
    Json(body): Json<CreateChannel>,
) -> Result<(StatusCode, Json<ChannelDto>), ApiError> {
    // A new channel is a new place messages go: as for changing one.
    super::common::require_elevation(&state, &admin.0)?;
    let kind: ChannelKind = body.kind.into();
    let name = validate_name(&body.name)?;
    let events = admin_events(&body.admin_events)?;
    let secret = body.secret.filter(|s| !is_blank(s));
    let settings = match kind {
        ChannelKind::Email => {
            let dto = body
                .email
                .as_ref()
                .ok_or(ApiError::Validation("validation.smtp_settings_required"))?;
            serde_json::to_value(validate_email(dto)?).map_err(|_| ApiError::Internal)?
        }
        ChannelKind::Webhook | ChannelKind::Discord => {
            let url = secret
                .as_ref()
                .ok_or(ApiError::Validation("validation.webhook_url"))?;
            json!({ "host": validate_webhook(kind, url)? })
        }
    };
    if kind == ChannelKind::Email {
        check_login(has_username(&settings), secret.is_some())?;
    }
    let sealed = match &secret {
        Some(secret) => Some(seal(&state, secret).await?),
        None => None,
    };
    let created = state
        .db
        .create_channel(
            &NewChannel {
                kind,
                name,
                enabled: body.enabled,
                settings: &settings,
                secret: sealed.as_ref(),
                admin_events: &events,
            },
            state.clock.now(),
        )
        .await
        .map_err(conflict)?;
    audit(
        &state,
        &admin,
        "notifications.channel_create",
        &created.name,
        &json!({ "id": created.id, "kind": kind.as_str() }),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(created.into())))
}

/// Change a channel. A new secret replaces the stored one; an empty one
/// removes the SMTP password. Changing where messages go (webhook or Discord
/// URL, SMTP settings or password) needs a fresh elevation.
#[utoipa::path(patch, path = "/api/v1/admin/notification-channels/{id}", tag = "notifications",
    params(("id" = i64, Path, description = "Channel id")),
    request_body = UpdateChannel,
    responses(
        (status = 200, body = ChannelDto),
        (status = 404, body = ErrorBody, description = "`notifications.channel_not_found`"),
        (status = 409, body = ErrorBody, description = "`notifications.channel_taken`"),
        (status = 422, body = ErrorBody, description = "As for creating; `validation.smtp_settings_required` (email settings on a webhook), `validation.credential_required_for_new_origin` (another SMTP host or port while a password is stored, without the password)"),
        (status = 503, body = ErrorBody, description = "`services.secrets_unavailable`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin), `auth.elevation_required` (target change without a fresh re-authentication)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn update_channel(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateChannel>,
) -> Result<Json<ChannelDto>, ApiError> {
    let current = state
        .db
        .get_channel(id)
        .await?
        .ok_or(ApiError::NotFound("notifications.channel_not_found"))?;
    let name = body.name.as_deref().map(validate_name).transpose()?;
    let events = body.admin_events.as_deref().map(admin_events).transpose()?;
    let email = match (current.kind, &body.email) {
        (_, None) => None,
        (ChannelKind::Email, Some(dto)) => Some(validate_email(dto)?),
        (_, Some(_)) => return Err(ApiError::Validation("validation.smtp_settings_required")),
    };
    let stored = serde_json::from_value::<EmailSettings>(current.settings.clone()).ok();
    if let Some(new) = &email
        && current.has_secret
        && body.secret.is_none()
        && stored
            .as_ref()
            .is_none_or(|old| (old.host.as_str(), old.port) != (new.host.as_str(), new.port))
    {
        // The stored SMTP password was entered for the old server: like a
        // service key (M1), it never follows the settings to another one.
        return Err(ApiError::Validation(
            "validation.credential_required_for_new_origin",
        ));
    }
    // Where messages go: a new secret (webhook URL, SMTP password) or other
    // SMTP settings. Re-sending the same settings needs nothing.
    if body.secret.is_some()
        || email
            .as_ref()
            .is_some_and(|new| stored.as_ref() != Some(new))
    {
        super::common::require_elevation(&state, &admin.0)?;
    }
    let mut settings = email
        .map(|e| serde_json::to_value(e).map_err(|_| ApiError::Internal))
        .transpose()?;
    let sealed = match (&body.secret, current.kind) {
        (None, _) => None,
        (Some(secret), ChannelKind::Email) if is_blank(secret) => Some(None),
        (Some(secret), ChannelKind::Email) => Some(Some(seal(&state, secret).await?)),
        (Some(url), kind) => {
            let host = validate_webhook(kind, url)?;
            settings = Some(json!({ "host": host }));
            Some(Some(seal(&state, url).await?))
        }
    };
    if current.kind == ChannelKind::Email {
        let username = has_username(settings.as_ref().unwrap_or(&current.settings));
        let password = match &sealed {
            None => current.has_secret,
            Some(secret) => secret.is_some(),
        };
        check_login(username, password)?;
    }
    let updated = state
        .db
        .update_channel(
            id,
            &ChannelUpdate {
                name,
                enabled: body.enabled,
                settings: settings.as_ref(),
                secret: sealed.as_ref().map(Option::as_ref),
                admin_events: events.as_deref(),
            },
            state.clock.now(),
        )
        .await
        .map_err(conflict)?
        .ok_or(ApiError::NotFound("notifications.channel_not_found"))?;
    audit(
        &state,
        &admin,
        "notifications.channel_update",
        &updated.name,
        &json!({
            "id": id,
            "enabled": body.enabled,
            "secretChanged": body.secret.is_some(),
            "settingsChanged": settings.is_some(),
        }),
    )
    .await?;
    Ok(Json(updated.into()))
}

/// Remove a channel and its pending deliveries.
#[utoipa::path(delete, path = "/api/v1/admin/notification-channels/{id}", tag = "notifications",
    params(("id" = i64, Path, description = "Channel id")),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, body = ErrorBody, description = "`notifications.channel_not_found`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn delete_channel(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let current = state
        .db
        .get_channel(id)
        .await?
        .ok_or(ApiError::NotFound("notifications.channel_not_found"))?;
    if !state.db.delete_channel(id).await? {
        return Err(ApiError::NotFound("notifications.channel_not_found"));
    }
    audit(
        &state,
        &admin,
        "notifications.channel_delete",
        &current.name,
        &json!({ "id": id }),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Send a test message on a channel (queued for the worker, which alone
/// can open the secret).
#[utoipa::path(post, path = "/api/v1/admin/notification-channels/{id}/test", tag = "notifications",
    params(("id" = i64, Path, description = "Channel id")),
    responses(
        (status = 202, body = TestQueued),
        (status = 404, body = ErrorBody, description = "`notifications.channel_not_found`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn test_channel(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<(StatusCode, Json<TestQueued>), ApiError> {
    if state.db.get_channel(id).await?.is_none() {
        return Err(ApiError::NotFound("notifications.channel_not_found"));
    }
    let now = state.clock.now();
    let kind = JobKind::NotificationsDeliver;
    let key = format!("{}:test:{id}", kind.name());
    let payload = json!({ "testChannelId": id }).to_string();
    let job_id = state
        .db
        .enqueue_job(
            &NewJob {
                kind: kind.name(),
                payload: &payload,
                unique_key: Some(&key),
                trigger: Trigger::Manual,
                run_after: now,
                max_attempts: 1,
            },
            now,
        )
        .await?;
    state.wake.notify_one();
    Ok((StatusCode::ACCEPTED, Json(TestQueued { job_id })))
}
