//! The private area's catalog (spec 0007 slice 2, ADR 0014): Discover
//! lanes, search and the scene, performer and studio pages of the adult
//! category, from its `StashDB` catalog. Separate from the global Discover
//! and search (Q11 A): nothing here is reachable from them, and they never
//! open this catalog.
//!
//! Every handler asks the privacy gate first. Closed (category off, no
//! entitlement, session locked) means absent: 404 `private.not_found`
//! before anything else is read, as if the area did not exist. Serving
//! something here is adult activity and keeps the session's unlock open.
//! Paths and answers stay neutral (`/api/v1/private/...`), and nothing of
//! an answer is logged.
//!
//! Scenes carry the member's status (new, requested, available; never who
//! else requested them) and, on the scene page, the journey of the
//! member's request; requests themselves are in `private_requests` (spec
//! 0007 slice 4).

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use starr_adapter_stashdb::StashDb;
use starr_core::catalog::{
    Catalog, CatalogEntry, CatalogRef, Child, ListKind, ListQuery, MediaKind, NamedRef, Relevance,
    SearchQuery,
};
use starr_core::members::Category;
use starr_worker::catalog::AnyCatalog;
use utoipa::{IntoParams, ToSchema};

use super::catalog::{adapter_error, open_catalog};
use super::common::OpenInLink;
use super::error::{ApiError, ErrorBody};
use super::journey::JourneyDto;
use super::privacy;
use super::private_follows::{PrivateFollow, follow_of};
use super::private_requests::{requestable, scene_journey, scene_open_in, scene_status};
use super::requests::AvailabilityDto;
use crate::AppState;
use crate::auth::{MemberSession, Session};

/// Language passed to the catalog (`StashDB` has one).
const LANGUAGE: &str = "en-US";
/// Longest search text accepted.
const SEARCH_MAX_CHARS: usize = 200;
/// Pages a lane or search offers (`StashDB` adapter cap).
const MAX_PAGE: u32 = starr_adapter_stashdb::PAGE_CAP;

/// What a private title is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = PrivateKind)]
pub enum PrivateKindDto {
    Scene,
    Performer,
    Studio,
}

impl PrivateKindDto {
    const fn of(kind: MediaKind) -> Option<Self> {
        match kind {
            MediaKind::Scene => Some(Self::Scene),
            MediaKind::Performer => Some(Self::Performer),
            MediaKind::Studio => Some(Self::Studio),
            MediaKind::Movie | MediaKind::Series | MediaKind::Artist | MediaKind::Album => None,
        }
    }
}

/// A lane of the private area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = PrivateList)]
pub enum PrivateListDto {
    /// Newest scenes by release date.
    New,
    /// The catalog's own trend.
    Trending,
}

impl PrivateListDto {
    const fn of(list: ListKind) -> Option<Self> {
        match list {
            ListKind::New => Some(Self::New),
            ListKind::Trending => Some(Self::Trending),
            _ => None,
        }
    }

    const fn list(self) -> ListKind {
        match self {
            Self::New => ListKind::New,
            Self::Trending => ListKind::Trending,
        }
    }
}

/// The private area as the member sees it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrivateArea {
    /// A catalog is set up: lanes, search and pages work.
    ready: bool,
    /// Scenes can be requested: the category's source is set up completely.
    requestable: bool,
    /// The lanes, in display order.
    lists: Vec<PrivateListDto>,
}

/// A performer or studio a scene names.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = PrivateRef)]
pub struct PrivateRefDto {
    kind: PrivateKindDto,
    id: String,
    name: String,
}

impl PrivateRefDto {
    fn of(named: NamedRef) -> Option<Self> {
        Some(Self {
            kind: PrivateKindDto::of(named.item.kind)?,
            id: named.item.id,
            name: named.name,
        })
    }
}

/// A scene, performer or studio in a lane, a search result or a page.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrivateCard {
    kind: PrivateKindDto,
    /// The catalog's id (a `StashDB` UUID).
    id: String,
    /// Title of a scene, name of a performer or studio.
    title: String,
    /// Scenes: release date (partial dates as their first day).
    date: Option<NaiveDate>,
    /// Image id for `/api/v1/images/stashdb/{size}/{ref}` (16:9).
    image: Option<String>,
    /// Scenes: the studio.
    studio: Option<PrivateRefDto>,
    /// Scenes: the performers, in the catalog's order (empty otherwise).
    performers: Vec<PrivateRefDto>,
    /// Scenes: running time in seconds.
    duration_seconds: Option<u32>,
    /// Scenes: new, requested (by anyone) or available for the member.
    status: Option<AvailabilityDto>,
    /// Scenes: the member has an open request for it.
    requested_by_me: bool,
}

impl PrivateCard {
    fn of(entry: CatalogEntry) -> Option<Self> {
        let kind = PrivateKindDto::of(entry.item.kind)?;
        let facts = entry.scene.unwrap_or_default();
        Some(Self {
            kind,
            id: entry.item.id,
            title: entry.title,
            date: entry.release_date,
            image: image_ref(entry.poster.as_deref()),
            studio: facts.studio.and_then(PrivateRefDto::of),
            performers: facts
                .performers
                .into_iter()
                .filter_map(PrivateRefDto::of)
                .collect(),
            duration_seconds: facts.duration_seconds,
            status: None,
            requested_by_me: false,
        })
    }
}

/// Cards of `entries`, scenes with the member's status.
async fn cards(
    state: &AppState,
    member_id: i64,
    entries: Vec<CatalogEntry>,
) -> Result<Vec<PrivateCard>, ApiError> {
    let mut out: Vec<PrivateCard> = entries.into_iter().filter_map(PrivateCard::of).collect();
    for card in out.iter_mut().filter(|c| c.kind == PrivateKindDto::Scene) {
        let (found, _) = scene_status(state, member_id, &card.id).await?;
        card.status = Some(found.dto());
        card.requested_by_me = found.requested_by_me;
    }
    Ok(out)
}

/// The image id of a `stashdb:<id>` reference; nothing else is ever
/// handed out here.
pub(crate) fn image_ref(poster: Option<&str>) -> Option<String> {
    poster
        .and_then(|p| p.strip_prefix("stashdb:"))
        .filter(|id| starr_adapter_stashdb::valid_image_ref(id))
        .map(str::to_owned)
}

/// One page of cards; `next` is the page to ask for next.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrivatePage {
    items: Vec<PrivateCard>,
    next: Option<u32>,
}

/// Search results by kind.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrivateSearch {
    scenes: Vec<PrivateCard>,
    performers: Vec<PrivateCard>,
    studios: Vec<PrivateCard>,
    next: Option<u32>,
    /// How many of each kind the catalog found in all (spec 0008 PRIV-14,
    /// the group headers); with a studio or performer filter, `scenes`
    /// counts the narrowed scenes.
    counts: PrivateSearchCounts,
}

/// Totals of a private search per kind.
#[derive(Debug, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrivateSearchCounts {
    scenes: u64,
    performers: u64,
    studios: u64,
}

/// A scene page (spec 0007 story 6).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrivateScene {
    id: String,
    title: String,
    date: Option<NaiveDate>,
    duration_seconds: Option<u32>,
    image: Option<String>,
    studio: Option<PrivateRefDto>,
    performers: Vec<PrivateRefDto>,
    /// New, requested (by anyone, never by whom) or available for the
    /// member: available when Whisparr has the file (spec 0007 Q5 A).
    status: AvailabilityDto,
    /// The member has an open request for it.
    requested_by_me: bool,
    /// The journey of the member's open request; absent without one.
    journey: Option<JourneyDto>,
    /// Available: the private category's libraries to open (not the scene
    /// itself, spec 0007 Q5 A).
    open_in: Vec<OpenInLink>,
    /// It can be requested: the category's source is set up completely.
    requestable: bool,
    /// The scene's description as plain text, at most 2,000 characters
    /// (spec 0008 Q14): render as text, never as HTML or Markdown. Like
    /// the title, it exists only behind the privacy gate.
    details: Option<String>,
    /// The studio's newest other scenes ("More from \<studio\>", the first
    /// page of the studio page, at most 12).
    more_from_studio: Vec<PrivateCard>,
}

/// Scenes in "More from \<studio\>".
const MORE_FROM_STUDIO: usize = 12;

/// A performer or studio page with its scenes, newest first.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrivateEntity {
    kind: PrivateKindDto,
    id: String,
    name: String,
    image: Option<String>,
    scenes: Vec<PrivateCard>,
    /// Its scenes can be requested (several at once); the performer or
    /// studio itself never is (spec 0007 Q3 A).
    requestable: bool,
    /// The member's follow of it (spec 0007 Q4), with the scenes beyond
    /// the weekly cap; absent when not followed.
    follow: Option<PrivateFollow>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PrivatePageQuery {
    /// 1-based page from `next` (default 1).
    page: Option<u32>,
}

/// A search of the private area. Sent as a body, never in a URL: search
/// text names titles and must not reach browser history or access logs.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivateSearchRequest {
    /// What to look for (at most 200 characters; blank finds nothing).
    #[serde(default)]
    query: String,
    /// 1-based page from `next` (default 1).
    #[serde(default)]
    page: Option<u32>,
    /// Narrow the scenes to one studio (`StashDB` UUID, from the search's
    /// studio lane; spec 0008 Q7 C). Performers and studios are searched
    /// as without it.
    #[serde(default)]
    studio: Option<String>,
    /// Narrow the scenes to one performer instead.
    #[serde(default)]
    performer: Option<String>,
}

impl PrivateSearchRequest {
    /// The entity the scenes are narrowed to: at most one, by canonical
    /// UUID.
    fn entity(&self) -> Result<Option<CatalogRef>, ApiError> {
        let invalid = || ApiError::Validation("validation.search_filter");
        let (kind, id) = match (&self.studio, &self.performer) {
            (None, None) => return Ok(None),
            (Some(id), None) => (MediaKind::Studio, id),
            (None, Some(id)) => (MediaKind::Performer, id),
            (Some(_), Some(_)) => return Err(invalid()),
        };
        let canonical = uuid::Uuid::parse_str(id)
            .ok()
            .filter(|u| u.hyphenated().to_string() == *id)
            .ok_or_else(invalid)?;
        Ok(Some(CatalogRef {
            kind,
            id: canonical.hyphenated().to_string(),
        }))
    }
}

/// The gate: open, the session's unlock is extended (activity in the
/// private area); closed, the area does not exist.
pub(crate) async fn enter(state: &AppState, session: &Session) -> Result<(), ApiError> {
    if !privacy::adult_open(state, session).await? {
        return Err(ApiError::NotFound("private.not_found"));
    }
    privacy::touch(state, session).await
}

/// The private category's catalog: its default catalog role, a `StashDB`
/// whose last check succeeded. Every successful check proved the key's
/// account can only read (spec 0007 Q14), so a key that was refused stays
/// unused until a check passes again, also across checks that failed for
/// other reasons. Call only behind the gate.
pub(crate) async fn private_catalog(state: &AppState) -> Result<Option<StashDb>, ApiError> {
    let Some(role) = state.db.default_catalog(Category::Adult).await? else {
        return Ok(None);
    };
    if role.adapter_type != starr_adapter_stashdb::DESCRIPTOR.type_id {
        return Ok(None);
    }
    let checked = state
        .db
        .get_service(role.service_instance_id)
        .await?
        .is_some_and(|s| s.last_check_at.is_some() && s.last_error.is_none());
    if !checked {
        return Ok(None);
    }
    match open_catalog(state, role.service_instance_id).await? {
        AnyCatalog::StashDb(catalog) => Ok(Some(catalog)),
        AnyCatalog::Tmdb(_) | AnyCatalog::MusicBrainz(_) => Ok(None),
    }
}

async fn ready_catalog(state: &AppState) -> Result<StashDb, ApiError> {
    private_catalog(state)
        .await?
        .ok_or(ApiError::NotFound("discover.unavailable"))
}

fn page_of(page: Option<u32>) -> Result<u32, ApiError> {
    match page.unwrap_or(1) {
        n @ 1..=MAX_PAGE => Ok(n),
        _ => Err(ApiError::Validation("validation.page")),
    }
}

fn lists() -> Vec<PrivateListDto> {
    starr_adapter_stashdb::DESCRIPTOR
        .lists
        .iter()
        .copied()
        .filter_map(PrivateListDto::of)
        .collect()
}

/// The private area: whether its catalog is set up, and its lanes. Absent
/// unless the privacy gate is open.
#[utoipa::path(get, path = "/api/v1/private", tag = "private",
    responses(
        (status = 200, body = PrivateArea),
        (status = 404, body = ErrorBody, description = "`private.not_found` (the gate is closed)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn private_area(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
) -> Result<Json<PrivateArea>, ApiError> {
    enter(&state, &session).await?;
    let ready = private_catalog(&state).await?.is_some();
    Ok(Json(PrivateArea {
        ready,
        requestable: ready && requestable(&state).await?,
        lists: if ready { lists() } else { Vec::new() },
    }))
}

/// One page of a lane of the private area.
#[utoipa::path(get, path = "/api/v1/private/discover/{list}", tag = "private",
    params(
        ("list" = String, Path, description = "`new` or `trending`"),
        PrivatePageQuery,
    ),
    responses(
        (status = 200, body = PrivatePage),
        (status = 404, body = ErrorBody, description = "`private.not_found` (the gate is closed), `discover.unavailable` (no catalog), `discover.unknown_list`"),
        (status = 422, body = ErrorBody, description = "`validation.page` (1 to 20)"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn private_lane(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path(list): Path<String>,
    Query(query): Query<PrivatePageQuery>,
) -> Result<Json<PrivatePage>, ApiError> {
    enter(&state, &session).await?;
    let list = lists()
        .into_iter()
        .find(|l| l.list().as_str() == list)
        .ok_or(ApiError::NotFound("discover.unknown_list"))?;
    let page = page_of(query.page)?;
    let catalog = ready_catalog(&state).await?;
    let found = catalog
        .list(&ListQuery {
            kind: MediaKind::Scene,
            list: list.list(),
            page,
            language: LANGUAGE,
            relevance: &Relevance::default(),
            filter: None,
        })
        .await
        .map_err(|e| adapter_error(&e))?;
    Ok(Json(PrivatePage {
        items: cards(&state, session.subject.id, found.items).await?,
        next: found.next,
    }))
}

/// Search the private area's catalog: scenes, performers and studios
/// (Q11 A: the global search never does). A `POST` so the text never
/// appears in a URL (browser history, reverse-proxy access logs).
#[utoipa::path(post, path = "/api/v1/private/search", tag = "private",
    request_body = PrivateSearchRequest,
    responses(
        (status = 200, body = PrivateSearch, description = "Empty for blank text"),
        (status = 404, body = ErrorBody, description = "`private.not_found` (the gate is closed), `discover.unavailable` (no catalog)"),
        (status = 422, body = ErrorBody, description = "`validation.page` (1 to 20), `validation.search_query`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn private_search(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Json(params): Json<PrivateSearchRequest>,
) -> Result<Json<PrivateSearch>, ApiError> {
    enter(&state, &session).await?;
    let page = page_of(params.page)?;
    let entity = params.entity()?;
    let text = params.query.trim();
    if text.chars().count() > SEARCH_MAX_CHARS {
        return Err(ApiError::Validation("validation.search_query"));
    }
    let mut out = PrivateSearch {
        scenes: Vec::new(),
        performers: Vec::new(),
        studios: Vec::new(),
        next: None,
        counts: PrivateSearchCounts::default(),
    };
    if text.is_empty() {
        return Ok(Json(out));
    }
    let catalog = ready_catalog(&state).await?;
    // With an entity filter, scenes come from a filtered query and the
    // lanes from the search as before.
    // With an entity filter the scenes come from their own query; after the
    // first page only scenes are wanted (the lanes come from the first).
    let kinds: &[MediaKind] = match (entity.is_some(), page > 1) {
        (true, true) => &[],
        (true, false) => &[MediaKind::Performer, MediaKind::Studio],
        (false, true) => &[MediaKind::Scene],
        (false, false) => &[MediaKind::Scene, MediaKind::Performer, MediaKind::Studio],
    };
    let (mut found, counts) = catalog
        .search_counted(&SearchQuery {
            text,
            kinds,
            page,
            language: LANGUAGE,
        })
        .await
        .map_err(|e| adapter_error(&e))?;
    out.counts = PrivateSearchCounts {
        scenes: counts.scenes,
        performers: counts.performers,
        studios: counts.studios,
    };
    if let Some(entity) = &entity {
        let (scenes, count) = catalog
            .search_scenes_of(text, entity, page)
            .await
            .map_err(|e| adapter_error(&e))?;
        found.items.extend(scenes.items);
        found.next = found.next.max(scenes.next);
        out.counts.scenes = count;
    }
    out.next = found.next;
    for card in cards(&state, session.subject.id, found.items).await? {
        match card.kind {
            PrivateKindDto::Scene => out.scenes.push(card),
            PrivateKindDto::Performer => out.performers.push(card),
            PrivateKindDto::Studio => out.studios.push(card),
        }
    }
    Ok(Json(out))
}

/// A scene: image, studio, performers, date and running time.
#[utoipa::path(get, path = "/api/v1/private/scenes/{sceneId}", tag = "private",
    params(("sceneId" = String, Path, description = "`StashDB` scene UUID")),
    responses(
        (status = 200, body = PrivateScene),
        (status = 404, body = ErrorBody, description = "`private.not_found` (the gate is closed), `discover.unavailable` (no catalog), `catalog.not_found`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn private_scene(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<PrivateScene>, ApiError> {
    enter(&state, &session).await?;
    let catalog = ready_catalog(&state).await?;
    let details = catalog
        .details(
            &CatalogRef {
                kind: MediaKind::Scene,
                id,
            },
            LANGUAGE,
        )
        .await
        .map_err(|e| adapter_error(&e))?;
    let entry = details.entry;
    let facts = entry.scene.unwrap_or_default();
    let member_id = session.subject.id;
    let (found, item) = scene_status(&state, member_id, &entry.item.id).await?;
    let journey = match &item {
        Some(item) => scene_journey(&state, member_id, item).await?,
        None => None,
    };
    // "More from <studio>": the studio page's first page (one request,
    // shared with the studio page's cache).
    // A failure there leaves the list empty instead of failing the page;
    // the error names nothing (private-content client).
    let more = match &facts.studio {
        Some(studio) => match catalog.studio_scenes(&studio.item).await {
            Ok(scenes) => scenes
                .into_iter()
                .filter(|s| s.item.id != entry.item.id)
                .take(MORE_FROM_STUDIO)
                .collect(),
            Err(err) => {
                tracing::warn!(code = err.code(), "more from the studio unavailable");
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    Ok(Json(PrivateScene {
        more_from_studio: cards(&state, member_id, more).await?,
        details: facts.details,
        status: found.dto(),
        requested_by_me: found.requested_by_me,
        journey,
        open_in: scene_open_in(&state, found).await?,
        requestable: requestable(&state).await?,
        id: entry.item.id,
        title: entry.title,
        date: entry.release_date,
        duration_seconds: facts.duration_seconds,
        image: image_ref(entry.poster.as_deref()),
        studio: facts.studio.and_then(PrivateRefDto::of),
        performers: facts
            .performers
            .into_iter()
            .filter_map(PrivateRefDto::of)
            .collect(),
    }))
}

async fn entity(
    state: &AppState,
    session: &Session,
    kind: MediaKind,
    id: String,
) -> Result<Json<PrivateEntity>, ApiError> {
    enter(state, session).await?;
    let catalog = ready_catalog(state).await?;
    let details = catalog
        .details(&CatalogRef { kind, id }, LANGUAGE)
        .await
        .map_err(|e| adapter_error(&e))?;
    let entry = details.entry;
    let kind =
        PrivateKindDto::of(entry.item.kind).ok_or(ApiError::NotFound("catalog.not_found"))?;
    let scenes: Vec<CatalogEntry> = details
        .children
        .into_iter()
        .filter_map(|child| match child {
            Child::Scene(scene) => Some(*scene),
            Child::Season(_) | Child::Album(_) => None,
        })
        .collect();
    let follow = follow_of(state, session.subject.id, entry.item.kind, &entry.item.id).await?;
    Ok(Json(PrivateEntity {
        follow,
        kind,
        id: entry.item.id,
        name: entry.title,
        image: image_ref(entry.poster.as_deref()),
        scenes: cards(state, session.subject.id, scenes).await?,
        requestable: requestable(state).await?,
    }))
}

/// A performer with their scenes, newest first.
#[utoipa::path(get, path = "/api/v1/private/performers/{performerId}", tag = "private",
    params(("performerId" = String, Path, description = "`StashDB` performer UUID")),
    responses(
        (status = 200, body = PrivateEntity),
        (status = 404, body = ErrorBody, description = "`private.not_found` (the gate is closed), `discover.unavailable` (no catalog), `catalog.not_found`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn private_performer(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<PrivateEntity>, ApiError> {
    entity(&state, &session, MediaKind::Performer, id).await
}

/// A studio with its scenes, newest first.
#[utoipa::path(get, path = "/api/v1/private/studios/{studioId}", tag = "private",
    params(("studioId" = String, Path, description = "`StashDB` studio UUID")),
    responses(
        (status = 200, body = PrivateEntity),
        (status = 404, body = ErrorBody, description = "`private.not_found` (the gate is closed), `discover.unavailable` (no catalog), `catalog.not_found`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn private_studio(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<PrivateEntity>, ApiError> {
    entity(&state, &session, MediaKind::Studio, id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_stashdb_image_ids_are_handed_out() {
        let id = "1a6e0000-0000-4000-8000-000000000002";
        assert_eq!(
            image_ref(Some(&format!("stashdb:{id}"))).as_deref(),
            Some(id)
        );
        for other in [
            "/abc.jpg",
            "caa:1a6e0000-0000-4000-8000-000000000002",
            "stashdb:../x",
        ] {
            assert_eq!(image_ref(Some(other)), None, "{other}");
        }
        assert_eq!(image_ref(None), None);
    }
}
