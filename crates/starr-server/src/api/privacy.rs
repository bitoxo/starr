//! The privacy gate of the adult category (spec 0007 "Privacy and
//! visibility model", ADR 0014). [`adult_open`] answers
//! [`starr_core::categories::adult_visible`] for the session of a request;
//! every handler that lists or serves something of a category asks it.
//! Closed means absent: lists leave adult out, single resources answer
//! 404, counts do not include it.
//!
//! The session unlock (Q7 B) lives on the session row: `PUT
//! /api/v1/auth/private` opens or closes it; it closes by itself 30 minutes
//! after the last adult activity and on logout. Admins count as entitled
//! (Q9 B) and see neutral rows until they unlock.

use axum::Json;
use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use starr_core::auth::{PRIVATE_UNLOCK_IDLE, SubjectKind, private_unlocked};
use starr_core::categories::{adult_visible, category_visible, is_private};
use starr_core::members::Category;
use starr_db::requests::{AcquisitionRecord, RequestEvent, RequestRecord};
use utoipa::ToSchema;

use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::{AnySession, Session};

/// The adult category's gate as far as it does not depend on the session:
/// switched on with the operator statement, and the account may see it
/// (a member with the adult entitlement switched on, or an admin).
pub(crate) async fn adult_available(
    state: &AppState,
    kind: SubjectKind,
    id: i64,
) -> Result<bool, ApiError> {
    let (enabled, entitled) = gate_inputs(state, kind, id).await?;
    Ok(enabled && entitled)
}

/// The account-side inputs of the gate: the category is switched on with
/// a valid operator statement, and the account counts as entitled.
async fn gate_inputs(
    state: &AppState,
    kind: SubjectKind,
    id: i64,
) -> Result<(bool, bool), ApiError> {
    let enabled = state
        .db
        .category_state(Category::Adult)
        .await?
        .enabled_with_statement();
    if !enabled {
        return Ok((false, false));
    }
    let entitled = match kind {
        SubjectKind::Admin => true,
        SubjectKind::Member => state
            .db
            .entitlements(id)
            .await?
            .iter()
            .any(|e| e.category == Category::Adult && e.enabled),
    };
    Ok((enabled, entitled))
}

/// Whether adult content is visible to `session` now: the privacy gate.
pub(crate) async fn adult_open(state: &AppState, session: &Session) -> Result<bool, ApiError> {
    let unlocked = private_unlocked(session.private_unlocked_until, state.clock.now());
    if !unlocked {
        // Closed whatever the rest says; no need to read it.
        return Ok(false);
    }
    let (enabled, entitled) = gate_inputs(state, session.subject.kind, session.subject.id).await?;
    Ok(adult_visible(enabled, entitled, unlocked))
}

/// Whether something of `category` is visible to `session`: other
/// categories are not gated here.
pub(crate) async fn visible(
    state: &AppState,
    session: &Session,
    category: Category,
) -> Result<bool, ApiError> {
    Ok(category_visible(
        category,
        category != Category::Adult || adult_open(state, session).await?,
    ))
}

/// Adult activity in `session`: an open unlock lasts another 30 minutes
/// from now. Call only where adult content was actually served.
pub(crate) async fn touch(state: &AppState, session: &Session) -> Result<(), ApiError> {
    let now = state.clock.now();
    state
        .db
        .extend_private_unlock(&session.token_hash, now, now + PRIVATE_UNLOCK_IDLE)
        .await?;
    Ok(())
}

/// Delete the private category's cached catalog answers and images (spec
/// 0007 Q13: when the category is switched off).
pub(crate) async fn purge_private_caches(state: &AppState) -> Result<(), ApiError> {
    state
        .db
        .catalog_cache_purge_prefix(starr_adapter_stashdb::CACHE_PREFIX)
        .await?;
    if let Some(images) = state.images.clone() {
        let purged = tokio::task::spawn_blocking(move || images.purge_adult()).await?;
        if let Err(err) = purged {
            // Housekeeping tries again while the category stays off.
            tracing::warn!(error = %err, "purging the adult image cache failed");
        }
    }
    Ok(())
}

/// The private category as the session sees it. Only present (in `me`)
/// for accounts that may unlock it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrivateCategory {
    /// Unlocked in this session.
    unlocked: bool,
    /// When the unlock closes unless there is activity in the private
    /// area before.
    unlocked_until: Option<DateTime<Utc>>,
}

impl PrivateCategory {
    pub(crate) fn of(until: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Self {
        let unlocked = private_unlocked(until, now);
        Self {
            unlocked,
            unlocked_until: until.filter(|_| unlocked),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetPrivateUnlock {
    unlocked: bool,
}

/// Unlock the private category for this session, or lock it again (spec
/// 0007 Q7). The unlock closes by itself 30 minutes after the last activity
/// in the private area, and on logout. Accounts that may not see the
/// private category get 404 when they unlock, as if it did not exist;
/// locking always works.
#[utoipa::path(put, path = "/api/v1/auth/private", tag = "auth",
    request_body = SetPrivateUnlock,
    responses(
        (status = 200, body = PrivateCategory),
        (status = 404, description = "`private.not_found`", body = ErrorBody),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn set_private_unlock(
    AnySession(session): AnySession,
    State(state): State<AppState>,
    Json(body): Json<SetPrivateUnlock>,
) -> Result<Json<PrivateCategory>, ApiError> {
    // Locking always works; unlocking only for accounts that may see it.
    if body.unlocked && !adult_available(&state, session.subject.kind, session.subject.id).await? {
        return Err(ApiError::NotFound("private.not_found"));
    }
    let now = state.clock.now();
    let until = body.unlocked.then(|| now + PRIVATE_UNLOCK_IDLE);
    if !state
        .db
        .set_private_unlock(&session.token_hash, until)
        .await?
    {
        return Err(ApiError::Unauthenticated);
    }
    Ok(Json(PrivateCategory::of(until, now)))
}

/// Leave out of `r` everything that names its title (Q9 B neutral rows):
/// title, year, poster, catalog ids, member names, reasons, albums, the
/// source's messages and the library name. Ids, kind, category, states
/// and times stay.
pub(crate) fn redact_request(r: &mut RequestRecord) {
    r.member_username = String::new();
    r.member_display_name = String::new();
    r.title = String::new();
    r.year = None;
    r.poster_path = None;
    r.tmdb_id = None;
    r.mb_artist = None;
    r.stash_scene = None;
    r.reject_reason = None;
    r.revoke_reason = None;
    for item in &mut r.items {
        item.album = None;
        if let Some(a) = item.acquisition.as_mut() {
            a.source_message = None;
            a.last_error = None;
            a.library_name = None;
        }
    }
}

/// Leave out who changed a private request and why (names, reasons).
pub(crate) fn redact_events(events: &mut [RequestEvent]) {
    for event in events {
        event.actor = None;
        event.detail = None;
    }
}

/// [`redact_request`] for an acquisition.
pub(crate) fn redact_acquisition(a: &mut AcquisitionRecord) {
    a.title = String::new();
    a.year = None;
    a.poster_path = None;
    a.album = None;
    a.library_name = None;
    a.source_message = None;
    a.last_error = None;
}

/// Whether something of `category` is shown as a neutral row (Q9 B) to a
/// session for which [`adult_open`] said `adult`: the attention list, the
/// held titles of a paused source, admin rows.
pub(crate) fn neutral(category: Category, adult: bool) -> bool {
    !category_visible(category, adult)
}

/// [`redact_request`] for a held title: title, year, season and album.
pub(crate) fn redact_held(t: &mut starr_db::requests::HeldTitle) {
    t.title = String::new();
    t.year = None;
    t.season = None;
    t.album = None;
}

/// The audit `target` for a title of `category`: never a private title
/// (the audit log keeps ids and the category only, spec 0007).
pub(crate) fn audit_title(category: Category, title: &str) -> Option<&str> {
    (!is_private(category)).then_some(title)
}
