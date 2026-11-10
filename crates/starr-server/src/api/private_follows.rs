//! Performer and studio follows in the private area (spec 0007 slice 5,
//! Q4, ADR 0014).
//!
//! A member follows a performer or a studio from its page: `follows.check`
//! (worker, daily per follow) then requests the scenes released after the
//! follow date in the member's name and mode, oldest first and at most the
//! private category's weekly cap per follow; the rest stay new and show as
//! "N more new scenes this week" on the follow (`cappedScenes`). Follows
//! are personal, like every follow.
//!
//! Every handler asks the privacy gate first (404 `private.not_found`
//! while closed). The member's private follows are listed here only, never
//! in the general `/api/v1/follows`; unfollowing uses the general
//! `DELETE /api/v1/follows/{id}`, which answers for a private follow only
//! through the gate. The audit log gets the follow id and the category,
//! never a name or a `StashDB` UUID.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::adapters::AdapterError;
use starr_core::catalog::{Catalog, MediaKind};
use starr_core::follows::{CAP_WINDOW, DEFAULT_CAP_PER_WEEK};
use starr_core::members::Category;
use starr_db::catalog::{ItemKind, Provenance};
use starr_db::follows::{Cutoff, FollowRecord, Restored};
use utoipa::ToSchema;

use super::catalog::adapter_error;
use super::common::audit_session;
use super::error::{ApiError, ErrorBody};
use super::privacy;
use super::private_catalog::{enter, image_ref, private_catalog};
use super::requests::{entitled, ready_source, title_ref};
use crate::AppState;
use crate::auth::{MemberSession, Session};

/// Language passed to the catalog (`StashDB` has one).
const LANGUAGE: &str = "en-US";
/// How long a recorded performer or studio is kept before it counts as
/// stale.
const METADATA_TTL: chrono::TimeDelta = chrono::TimeDelta::days(30);
/// How long after an unfollow its undo still brings the follow back
/// (spec 0008 F8: the undo toast offers it for 8 s, longer while it has
/// focus or the pointer).
const RESTORE_WINDOW: chrono::TimeDelta = chrono::TimeDelta::hours(1);

/// What a private follow follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = PrivateFollowKind)]
pub enum PrivateFollowKindDto {
    Performer,
    Studio,
}

impl PrivateFollowKindDto {
    const fn media_kind(self) -> MediaKind {
        match self {
            Self::Performer => MediaKind::Performer,
            Self::Studio => MediaKind::Studio,
        }
    }

    const fn of(kind: ItemKind) -> Option<Self> {
        match kind {
            ItemKind::Performer => Some(Self::Performer),
            ItemKind::Studio => Some(Self::Studio),
            _ => None,
        }
    }
}

/// A performer or studio to follow.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatePrivateFollow {
    kind: PrivateFollowKindDto,
    /// The `StashDB` UUID (lower case), as in `PrivateEntity.id`.
    id: String,
}

/// A performer or studio follow of the logged-in member. Carries no other
/// member's data.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrivateFollow {
    id: i64,
    kind: PrivateFollowKindDto,
    /// The `StashDB` UUID; with `kind` it opens the performer or studio.
    entity_id: Option<String>,
    /// The performer's or studio's name.
    name: String,
    /// Image id for `/api/v1/images/stashdb/{size}/{ref}`.
    image: Option<String>,
    /// Scenes released after this day (the day following started) are
    /// requested.
    known_through: Option<NaiveDate>,
    created_at: DateTime<Utc>,
    /// The last daily check of this follow.
    checked_at: Option<DateTime<Utc>>,
    /// New scenes the last check (within the past week) left beyond the
    /// weekly cap: "N more new scenes this week — request them yourself".
    /// Absent when none.
    capped_scenes: Option<u32>,
    /// The private category's weekly cap per follow (spec 0008 PRIV-09:
    /// "up to N new scenes per week").
    weekly_cap: u32,
    /// Scenes this follow may still request in the current rolling week
    /// ("M left this week"); requests the member made by hand do not count.
    remaining_this_week: u32,
}

/// The weekly cap per follow: the private catalog's setting, else the
/// default (as `follows.check` reads it).
async fn weekly_cap(state: &AppState) -> Result<u32, ApiError> {
    Ok(state
        .db
        .default_catalog(Category::Adult)
        .await?
        .and_then(|role| role.follow_cap_per_week)
        .unwrap_or(DEFAULT_CAP_PER_WEEK))
}

impl PrivateFollow {
    /// The DTO of a performer or studio follow under the weekly `cap`;
    /// `None` for other kinds.
    async fn of(
        state: &AppState,
        f: FollowRecord,
        cap: u32,
        now: DateTime<Utc>,
    ) -> Result<Option<Self>, ApiError> {
        let Some(kind) = PrivateFollowKindDto::of(f.kind) else {
            return Ok(None);
        };
        let used = state
            .db
            .follow_requests_since(f.id, now - CAP_WINDOW)
            .await?;
        // A hint from a check older than the week no longer says anything.
        let fresh = f.checked_at.is_some_and(|at| at > now - CAP_WINDOW);
        Ok(Some(Self {
            id: f.id,
            kind,
            entity_id: f.stash_id.map(|u| u.hyphenated().to_string()),
            image: image_ref(f.poster_path.as_deref()),
            name: f.title,
            known_through: f.cutoff.date(),
            created_at: f.created_at,
            checked_at: f.checked_at,
            capped_scenes: f.capped_scenes.filter(|_| fresh),
            weekly_cap: cap,
            remaining_this_week: cap.saturating_sub(used),
        }))
    }

    /// [`Self::of`] with the category's cap, for one follow.
    async fn one(state: &AppState, f: FollowRecord) -> Result<Self, ApiError> {
        let cap = weekly_cap(state).await?;
        Self::of(state, f, cap, state.clock.now())
            .await?
            .ok_or(ApiError::Internal)
    }
}

/// The member's private follows (performers and studios), newest first.
/// Call only behind the gate.
async fn member_private_follows(
    state: &AppState,
    member_id: i64,
) -> Result<Vec<PrivateFollow>, ApiError> {
    let now = state.clock.now();
    let cap = weekly_cap(state).await?;
    let mut out = Vec::new();
    for f in state.db.member_follows(member_id).await? {
        if f.category != Category::Adult {
            continue;
        }
        out.extend(PrivateFollow::of(state, f, cap, now).await?);
    }
    Ok(out)
}

/// The member's follow of the performer or studio `id`, if any. Call only
/// behind the gate.
pub(crate) async fn follow_of(
    state: &AppState,
    member_id: i64,
    kind: MediaKind,
    id: &str,
) -> Result<Option<PrivateFollow>, ApiError> {
    Ok(member_private_follows(state, member_id)
        .await?
        .into_iter()
        .find(|f| f.kind.media_kind() == kind && f.entity_id.as_deref() == Some(id)))
}

/// The member's performer and studio follows, newest first (spec 0007
/// story 7: the private area's own tab). Listing is no activity: it does
/// not keep the unlock open.
#[utoipa::path(get, path = "/api/v1/private/follows", tag = "private",
    responses(
        (status = 200, body = Vec<PrivateFollow>),
        (status = 404, body = ErrorBody, description = "`private.not_found` (the gate is closed)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_private_follows(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<PrivateFollow>>, ApiError> {
    if !privacy::adult_open(&state, &session).await? {
        return Err(ApiError::NotFound("private.not_found"));
    }
    Ok(Json(
        member_private_follows(&state, session.subject.id).await?,
    ))
}

/// Follow a performer or studio (spec 0007 Q4): every scene released after
/// today is requested in the member's name with their request mode, at
/// most the category's weekly cap per follow (default 10); the rest are
/// shown on the follow for the member to request. Following one already
/// followed returns that follow (200). Performers and studios themselves
/// are never requested (Q3 A).
#[utoipa::path(post, path = "/api/v1/private/follows", tag = "private",
    request_body = CreatePrivateFollow,
    responses(
        (status = 201, body = PrivateFollow, description = "Now following"),
        (status = 200, body = PrivateFollow, description = "Already following"),
        (status = 404, body = ErrorBody, description = "`private.not_found` (the gate is closed), `catalog.not_found`"),
        (status = 409, body = ErrorBody, description = "`requests.category_not_ready`"),
        (status = 422, body = ErrorBody, description = "`requests.not_entitled`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn create_private_follow(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Json(body): Json<CreatePrivateFollow>,
) -> Result<(StatusCode, Json<PrivateFollow>), ApiError> {
    enter(&state, &session).await?;
    let member_id = session.subject.id;
    entitled(&state, member_id, Category::Adult).await?;
    // New scenes become requests: the category must take them.
    ready_source(&state, Category::Adult, MediaKind::Scene).await?;
    let kind = body.kind.media_kind();
    let reference = title_ref(kind, &body.id).ok_or(ApiError::NotFound("catalog.not_found"))?;
    let role = state
        .db
        .default_catalog(Category::Adult)
        .await?
        .ok_or(ApiError::Conflict("requests.category_not_ready"))?;
    let catalog = private_catalog(&state)
        .await?
        .ok_or(ApiError::Conflict("requests.category_not_ready"))?;
    let details = match catalog.details(&reference, LANGUAGE).await {
        Ok(details) => details,
        Err(AdapterError::NotFound(_)) => return Err(ApiError::NotFound("catalog.not_found")),
        Err(err) => return Err(adapter_error(&err)),
    };
    let now = state.clock.now();
    let provenance = Provenance {
        adapter_type: &role.adapter_type,
        instance_id: role.service_instance_id,
        fetched_at: now,
        expires_at: now + METADATA_TTL,
    };
    let item = state
        .db
        .upsert_catalog_item(&details, &provenance, now)
        .await?;
    let (follow, created) = follow_entity(&state, &session, item.id).await?;
    let follow = PrivateFollow::one(&state, follow).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(follow)))
}

/// Follow performer or studio `item` for the session's member from today
/// on (scenes released after the follow date are new), or keep following
/// it with its date; audited with ids only when new.
async fn follow_entity(
    state: &AppState,
    session: &Session,
    item: i64,
) -> Result<(FollowRecord, bool), ApiError> {
    let now = state.clock.now();
    let (follow, created) = state
        .db
        .follow(
            session.subject.id,
            item,
            Category::Adult,
            Cutoff::Date(now.date_naive()),
            now,
        )
        .await?;
    if created {
        audit_session(
            state,
            session,
            "follow.create",
            None,
            Some(json!({ "followId": follow.id, "category": Category::Adult.as_str() })),
        )
        .await?;
    }
    Ok((follow, created))
}

/// Undo an unfollow (spec 0008 F8, PRIV-17): the member's private follow
/// `id`, ended within the last hour, follows again exactly as before: the
/// same cutoff (scenes released after the original follow date), start
/// and hint. An active follow is returned as it is. Audited with ids only.
#[utoipa::path(post, path = "/api/v1/private/follows/{id}/restore", tag = "private",
    params(("id" = i64, Path, description = "Follow id, as unfollowed")),
    responses(
        (status = 200, body = PrivateFollow, description = "Following again, as before"),
        (status = 404, body = ErrorBody, description = "`private.not_found` (the gate is closed), `follows.not_found` (not the member's private follow, or ended over an hour ago)"),
        (status = 409, body = ErrorBody, description = "`follows.followed_again` (the member follows it anew meanwhile)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn restore_private_follow(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<PrivateFollow>, ApiError> {
    enter(&state, &session).await?;
    // As when following: the member may still request here, and the
    // category still takes requests.
    entitled(&state, session.subject.id, Category::Adult).await?;
    ready_source(&state, Category::Adult, MediaKind::Scene).await?;
    let now = state.clock.now();
    let follow = match state
        .db
        .restore_follow(
            session.subject.id,
            id,
            Category::Adult,
            now - RESTORE_WINDOW,
        )
        .await?
    {
        Restored::Back(follow) => follow,
        Restored::Active(follow) => return Ok(Json(PrivateFollow::one(&state, follow).await?)),
        Restored::Superseded => return Err(ApiError::Conflict("follows.followed_again")),
        Restored::NotFound => return Err(ApiError::NotFound("follows.not_found")),
    };
    audit_session(
        &state,
        &session,
        "follow.restore",
        None,
        Some(json!({ "followId": follow.id, "category": Category::Adult.as_str() })),
    )
    .await?;
    Ok(Json(PrivateFollow::one(&state, follow).await?))
}
