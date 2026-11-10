//! Follows (spec 0003 D8, user story 4; slice 7; spec 0006 slice 6, user
//! stories 3, 5 and 7).
//!
//! Members (`/api/v1/follows…`) follow a series or an artist without
//! requesting anything (from the detail sheet), list their follows and
//! unfollow. Requesting with "also get new seasons" or "follow the artist"
//! follows too (`requests.rs`, `music.rs`). Follows are personal: a member
//! only ever sees and ends their own.
//!
//! `follows.check` (worker, daily per follow) turns new seasons and albums
//! into requests in the member's name; this side only records the follow
//! and its cutoff: the seasons the catalog lists with an air date right
//! now, or today's date for an artist (albums first released after it are
//! new, spec 0006 Q3).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::catalog::{CatalogDetails, MediaKind};
use starr_core::categories::is_private;
use starr_core::follows;
use starr_core::members::Category;
use starr_db::catalog::{CatalogItem, ItemKind};
use starr_db::follows::{Cutoff, FollowRecord};
use utoipa::ToSchema;

use super::catalog::{MediaKindDto, file_name};
use super::common::{CategoryDto, audit_session};
use super::error::{ApiError, ErrorBody};
use super::privacy;
use super::requests::{
    Record, category_of, entitled, ready_source, record_title, title_ref_or_404,
};
use crate::AppState;
use crate::auth::{MemberSession, Session};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateFollow {
    /// `series` (new seasons, D8) or `artist` (new albums, spec 0006).
    kind: MediaKindDto,
    /// The title's id in the category's catalog (TMDB for series, the
    /// artist MBID for artists), as in `TitleCard.id`.
    title_id: String,
}

/// A follow of the logged-in member. Carries no other member's data.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Follow {
    id: i64,
    /// `series` or `artist`.
    kind: MediaKindDto,
    /// The title's id in the category's catalog; with `kind` it opens the
    /// title.
    title_id: Option<String>,
    title: String,
    year: Option<i32>,
    /// Poster file (series: `/api/v1/images/tmdb/{size}/{file}`) or cover
    /// (artists: `/api/v1/images/caa/{size}/{ref}`).
    poster: Option<String>,
    category: CategoryDto,
    /// Series: seasons up to this number were known when following started
    /// (or were handled since); later seasons are asked for once they have
    /// an air date. Absent for artists.
    seasons_through: Option<u32>,
    /// Artists: albums first released after this day (the day following
    /// started) are asked for, of the category's release types. Absent for
    /// series.
    known_through: Option<NaiveDate>,
    created_at: DateTime<Utc>,
    /// The last daily check of this follow.
    checked_at: Option<DateTime<Utc>>,
}

impl From<FollowRecord> for Follow {
    fn from(f: FollowRecord) -> Self {
        let (kind, title_id) = match f.kind {
            ItemKind::Artist => (
                MediaKindDto::Artist,
                f.mb_artist.map(|id| id.hyphenated().to_string()),
            ),
            _ => (MediaKindDto::Series, f.tmdb_id.map(|v| v.to_string())),
        };
        Self {
            id: f.id,
            kind,
            title_id,
            title: f.title,
            year: f.year,
            poster: file_name(f.poster_path),
            category: f.category.into(),
            seasons_through: f.cutoff.seasons(),
            known_through: f.cutoff.date(),
            created_at: f.created_at,
            checked_at: f.checked_at,
        }
    }
}

/// The logged-in member's follows, newest first (user story 4).
#[utoipa::path(get, path = "/api/v1/follows", tag = "follows",
    responses(
        (status = 200, body = Vec<Follow>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_follows(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<Follow>>, ApiError> {
    let follows = state.db.member_follows(session.subject.id).await?;
    // Private follows (performers, studios) are listed in the private area
    // only, through its gate (spec 0007 story 7, slice 5).
    Ok(Json(
        follows
            .into_iter()
            .filter(|f| !is_private(f.category))
            .map(Follow::from)
            .collect(),
    ))
}

/// Follow a series or an artist without requesting anything. A series
/// (D8): its seasons listed with an air date now are known; every later
/// one is asked for in the member's name with their request mode once the
/// catalog lists it with an air date. An artist (spec 0006 Q3): every
/// album of the category's release types first released after today is
/// asked for the same way. Following a title already followed returns
/// that follow (200).
#[utoipa::path(post, path = "/api/v1/follows", tag = "follows",
    request_body = CreateFollow,
    responses(
        (status = 201, body = Follow, description = "Now following"),
        (status = 200, body = Follow, description = "Already following"),
        (status = 404, body = ErrorBody, description = "`catalog.not_found`"),
        (status = 409, body = ErrorBody, description = "`requests.category_not_ready`"),
        (status = 422, body = ErrorBody, description = "`requests.not_entitled`, `follows.not_followable` (only series and artists), `requests.no_tvdb_id`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn create_follow(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Json(body): Json<CreateFollow>,
) -> Result<(StatusCode, Json<Follow>), ApiError> {
    let kind: MediaKind = body.kind.into();
    if !matches!(kind, MediaKind::Series | MediaKind::Artist) {
        return Err(ApiError::Validation("follows.not_followable"));
    }
    let category = category_of(kind);
    let member_id = session.subject.id;
    entitled(&state, member_id, category).await?;
    // New seasons and albums become requests: the category must take them.
    ready_source(&state, category, kind).await?;
    let title_ref = title_ref_or_404(kind, &body.title_id)?;
    let (details, title) = record_title(&state, category, &title_ref, Record::IfStale).await?;
    let (follow, created) = if kind == MediaKind::Artist {
        follow_artist(&state, &session, &title).await?
    } else {
        // Sonarr keeps series by TVDB id: without one no season could be
        // asked for.
        if title.tvdb_id.is_none() {
            return Err(ApiError::Validation("requests.no_tvdb_id"));
        }
        follow_series(&state, &session, &title, &details).await?
    };
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(follow.into())))
}

/// Follow the series `title` for the session's member from the seasons that
/// have aired today on (D8, maintainer 2026-11-02), or keep following it;
/// audited when new. Returns the follow and whether it is new.
pub(crate) async fn follow_series(
    state: &AppState,
    session: &Session,
    title: &CatalogItem,
    details: &CatalogDetails,
) -> Result<(FollowRecord, bool), ApiError> {
    let today = state.clock.now().date_naive();
    let cutoff = Cutoff::Seasons(follows::seasons_through(details.seasons(), today));
    follow_title(state, session, title, Category::Series, cutoff).await
}

/// Follow the artist `title` for the session's member from today on (spec
/// 0006 Q3: albums first released after the follow date are new), or keep
/// following it with its date; audited when new.
pub(crate) async fn follow_artist(
    state: &AppState,
    session: &Session,
    title: &CatalogItem,
) -> Result<(FollowRecord, bool), ApiError> {
    let cutoff = artist_cutoff(state);
    follow_title(state, session, title, Category::Music, cutoff).await
}

/// Where an artist follow starting now starts: today.
pub(crate) fn artist_cutoff(state: &AppState) -> Cutoff {
    Cutoff::Date(state.clock.now().date_naive())
}

async fn follow_title(
    state: &AppState,
    session: &Session,
    title: &CatalogItem,
    category: Category,
    cutoff: Cutoff,
) -> Result<(FollowRecord, bool), ApiError> {
    let now = state.clock.now();
    let (follow, created) = state
        .db
        .follow(session.subject.id, title.id, category, cutoff, now)
        .await?;
    if created {
        let detail = match follow.cutoff {
            Cutoff::Seasons(n) => json!({ "followId": follow.id, "seasonsThrough": n }),
            Cutoff::Date(d) => json!({ "followId": follow.id, "knownThrough": d }),
        };
        audit_session(
            state,
            session,
            "follow.create",
            Some(&title.title),
            Some(detail),
        )
        .await?;
    }
    Ok((follow, created))
}

/// Stop following (new seasons or albums are no longer asked for).
/// Requests made so far stay.
#[utoipa::path(delete, path = "/api/v1/follows/{id}", tag = "follows",
    params(("id" = i64, Path, description = "Follow id")),
    responses(
        (status = 204, description = "Unfollowed"),
        (status = 404, body = ErrorBody, description = "`follows.not_found` (also for other members' follows)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn delete_follow(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let member_id = session.subject.id;
    let follow = state
        .db
        .member_follow(member_id, id)
        .await?
        .ok_or(ApiError::NotFound("follows.not_found"))?;
    if !privacy::visible(&state, &session, follow.category).await? {
        return Err(ApiError::NotFound("follows.not_found"));
    }
    if !state.db.unfollow(member_id, id, state.clock.now()).await? {
        return Err(ApiError::NotFound("follows.not_found"));
    }
    audit_session(
        &state,
        &session,
        "follow.end",
        privacy::audit_title(follow.category, &follow.title),
        Some(json!({ "followId": id, "category": follow.category.as_str() })),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
