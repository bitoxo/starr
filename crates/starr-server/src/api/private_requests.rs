//! Scene requests in the private area (spec 0007 slice 4, ADR 0014).
//!
//! Members request scenes (Q3 A: never performers or studios) from a scene
//! page or several at once from a performer or studio page. Each scene is
//! its own request with one item, like a movie: scenes have no parent in
//! the catalog, and one request per scene keeps attach, withdraw, cancel,
//! revoke and "removed" per scene (C2, C10). Approval and entitlement work
//! as for every category (Q10 A): the member's mode, an earlier refusal
//! waits for an admin, a scene already being acquired for someone else is
//! attached (D10).
//!
//! Availability (Q5 A): a scene is available when Whisparr has its file;
//! the media servers' adult libraries are not asked. "Open in" leads to
//! those libraries (the member's account sees them by C11), not to the
//! scene.
//!
//! Every handler asks the privacy gate first (404 `private.not_found`
//! while closed) and keeps the session's unlock open. The audit log gets
//! request ids and the category only; nothing here logs a title or a
//! `StashDB` UUID.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::adapters::AdapterError;
use starr_core::catalog::{Catalog, CatalogRef, ExternalId, MediaKind};
use starr_core::journey::Audience;
use starr_core::members::Category;
use starr_core::members::RequestMode;
use starr_core::requests::{Availability, Decision};
use starr_db::catalog::{CatalogItem, Provenance};
use starr_db::requests::{
    Actor, EventTrigger, NewRequest, RequestFilter, RequestFollow, SourceTarget, TitlePresence,
    Unit,
};
use utoipa::{IntoParams, ToSchema};

use super::catalog::adapter_error;
use super::common::{OpenInLink, audit_session, open_in_links};
use super::error::{ApiError, ErrorBody};
use super::journey::{JourneyDto, item_journey};
use super::notifications;
use super::privacy;
use super::private_catalog::{enter, private_catalog};
use super::requests::{
    AvailabilityDto, MyRequest, SkipReason, availability, enqueue_submit, entitled, my_requests,
    ready_source, title_ref,
};
use crate::AppState;
use crate::auth::MemberSession;

/// Language passed to the catalog (`StashDB` has one).
const LANGUAGE: &str = "en-US";
/// How long a recorded scene is kept before it counts as stale.
const METADATA_TTL: chrono::TimeDelta = chrono::TimeDelta::days(30);
/// Most scenes one call requests: each is read from `StashDB`, which Starr
/// asks at most 2 times a second (spec 0007 Q1).
pub(crate) const MAX_SCENES: usize = 20;

/// Scenes to request.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatePrivateRequests {
    /// `StashDB` scene UUIDs (lower case, 1 to 20; duplicates count once).
    scenes: Vec<String>,
}

/// A wanted scene left out of the new requests.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkippedScene {
    /// The `StashDB` scene UUID.
    id: String,
    reason: SkipReason,
}

/// The new requests (one per scene), and the scenes left out.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatedPrivateRequests {
    requests: Vec<MyRequest>,
    skipped: Vec<SkippedScene>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct PrivateRequestsQuery {
    /// Also list the requests the member hid.
    #[serde(default)]
    hidden: bool,
}

/// What a member sees of a scene before requesting: never who else
/// requested it (D10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SceneStatus {
    pub status: Availability,
    pub requested_by_me: bool,
}

impl SceneStatus {
    const NEW: Self = Self {
        status: Availability::New,
        requested_by_me: false,
    };

    pub(crate) fn dto(self) -> AvailabilityDto {
        self.status.into()
    }
}

/// The scene `id` names, as recorded; `None` before anyone requested it.
async fn recorded_scene(state: &AppState, id: &str) -> Result<Option<CatalogItem>, ApiError> {
    let Ok(uuid) = uuid::Uuid::parse_str(id) else {
        return Ok(None);
    };
    Ok(state
        .db
        .resolve_catalog_item(MediaKind::Scene, &[ExternalId::StashScene(uuid)])
        .await?)
}

/// What `member_id` sees of scene `id`: from its acquisition and anyone's
/// open requests (Whisparr's files decide availability, Q5 A).
pub(crate) async fn scene_status(
    state: &AppState,
    member_id: i64,
    id: &str,
) -> Result<(SceneStatus, Option<CatalogItem>), ApiError> {
    let Some(item) = recorded_scene(state, id).await? else {
        return Ok((SceneStatus::NEW, None));
    };
    let facts = state.db.unit_facts(item.id, member_id).await?;
    let found = facts.first().map_or(SceneStatus::NEW, |f| SceneStatus {
        status: availability(f, &TitlePresence::NoService),
        requested_by_me: f.mine,
    });
    Ok((found, Some(item)))
}

/// The journey of the member's open request for scene `item`, if any.
pub(crate) async fn scene_journey(
    state: &AppState,
    member_id: i64,
    item: &CatalogItem,
) -> Result<Option<JourneyDto>, ApiError> {
    let requests = state
        .db
        .list_requests(RequestFilter {
            member_id: Some(member_id),
            category: Some(Category::Adult),
            catalog_item_id: Some(item.id),
            open: true,
            ..RequestFilter::default()
        })
        .await?;
    let Some(request) = requests.first() else {
        return Ok(None);
    };
    let events = state.db.request_events(request.id).await?;
    Ok(request
        .items
        .iter()
        .find(|i| matches!(i.decision, Decision::PendingApproval | Decision::Approved))
        .map(|i| item_journey(Audience::Member, request, i, &events)))
}

/// Where an available scene opens: the private category's libraries.
pub(crate) async fn scene_open_in(
    state: &AppState,
    status: SceneStatus,
) -> Result<Vec<OpenInLink>, ApiError> {
    if status.status != Availability::Available {
        return Ok(Vec::new());
    }
    Ok(open_in_links(
        state.db.category_library_links(Category::Adult).await?,
    ))
}

/// Whether members can request scenes right now: the category is on with
/// its operator statement and its source is set up completely, and a
/// `StashDB` catalog passed its check. Call only behind the gate.
pub(crate) async fn requestable(state: &AppState) -> Result<bool, ApiError> {
    match ready_source(state, Category::Adult, MediaKind::Scene).await {
        Ok(_) => Ok(private_catalog(state).await?.is_some()),
        Err(ApiError::Conflict("requests.category_not_ready")) => Ok(false),
        Err(other) => Err(other),
    }
}

fn unknown_scene() -> ApiError {
    ApiError::Validation("requests.unknown_scene")
}

/// The scenes a body names, as catalog refs (deduplicated, in order).
fn wanted_scenes(body: &CreatePrivateRequests) -> Result<Vec<CatalogRef>, ApiError> {
    if body.scenes.is_empty() || body.scenes.len() > MAX_SCENES {
        return Err(ApiError::Validation("requests.scenes_required"));
    }
    let mut seen = BTreeSet::new();
    body.scenes
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .map(|id| title_ref(MediaKind::Scene, id).ok_or_else(unknown_scene))
        .collect()
}

/// Read and record every scene of `wanted` from the private category's
/// catalog: an unknown one refuses the whole call before anything is
/// recorded.
async fn record_scenes(
    state: &AppState,
    wanted: &[CatalogRef],
) -> Result<Vec<CatalogItem>, ApiError> {
    let role = state
        .db
        .default_catalog(Category::Adult)
        .await?
        .ok_or(ApiError::Conflict("requests.category_not_ready"))?;
    let catalog = private_catalog(state)
        .await?
        .ok_or(ApiError::Conflict("requests.category_not_ready"))?;
    let mut details = Vec::with_capacity(wanted.len());
    for scene in wanted {
        match catalog.details(scene, LANGUAGE).await {
            Ok(found) => details.push(found),
            Err(AdapterError::NotFound(_)) => return Err(unknown_scene()),
            Err(err) => return Err(adapter_error(&err)),
        }
    }
    let now = state.clock.now();
    let provenance = Provenance {
        adapter_type: &role.adapter_type,
        instance_id: role.service_instance_id,
        fetched_at: now,
        expires_at: now + METADATA_TTL,
    };
    let mut items = Vec::with_capacity(details.len());
    for found in &details {
        items.push(
            state
                .db
                .upsert_catalog_item(found, &provenance, now)
                .await?,
        );
    }
    Ok(items)
}

/// What became of one wanted scene.
enum Outcome {
    Created(starr_db::requests::Created),
    Skipped(SkipReason),
}

/// Request scene `item` (`id` its `StashDB` UUID) for the member of
/// `session` in `request_mode`, unless it is available or theirs already.
async fn request_scene(
    state: &AppState,
    session: &crate::auth::Session,
    (id, item): (&str, &CatalogItem),
    request_mode: RequestMode,
    source: &SourceTarget,
) -> Result<Outcome, ApiError> {
    let member_id = session.subject.id;
    let (status, _) = scene_status(state, member_id, id).await?;
    if status.status == Availability::Available {
        return Ok(Outcome::Skipped(SkipReason::Available));
    }
    if status.requested_by_me {
        return Ok(Outcome::Skipped(SkipReason::AlreadyRequested));
    }
    let result = state
        .db
        .create_request(
            &NewRequest {
                member_id,
                catalog_item_id: item.id,
                follow: RequestFollow::No,
                units: &[Unit {
                    catalog_item_id: item.id,
                    season_number: None,
                }],
                mode: request_mode,
                source,
            },
            Actor {
                trigger: EventTrigger::Member,
                name: &session.subject.username,
            },
            state.clock.now(),
        )
        .await;
    let created = match result {
        Ok(created) => created,
        // Requested meanwhile (another tab) or available through its
        // acquisition: left out like the scenes above.
        Err(starr_db::DbError::RequestOpen) => {
            return Ok(Outcome::Skipped(SkipReason::AlreadyRequested));
        }
        Err(starr_db::DbError::AlreadyAvailable) => {
            return Ok(Outcome::Skipped(SkipReason::Available));
        }
        Err(other) => return Err(other.into()),
    };
    // Ids and the category only (spec 0007, audit log).
    audit_session(
        state,
        session,
        "request.create",
        None,
        Some(json!({
            "requestId": created.request_id,
            "category": Category::Adult.as_str(),
            "state": created.state.as_str(),
        })),
    )
    .await?;
    Ok(Outcome::Created(created))
}

/// Request scenes (spec 0007 slice 4): one request per scene, each
/// admitted by the member's mode, an earlier refusal and a shared
/// acquisition as for movies (Q10 A). Scenes that are available or that
/// the member requested already are skipped and listed; refused only if
/// nothing is left. Performers and studios are never requestable (Q3 A).
#[utoipa::path(post, path = "/api/v1/private/requests", tag = "private",
    request_body = CreatePrivateRequests,
    responses(
        (status = 201, body = CreatedPrivateRequests),
        (status = 404, body = ErrorBody, description = "`private.not_found` (the gate is closed)"),
        (status = 409, body = ErrorBody, description = "`requests.category_not_ready`, `requests.already_requested`, `requests.already_available`"),
        (status = 422, body = ErrorBody, description = "`requests.scenes_required` (1 to 20), `requests.unknown_scene`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn create_private_requests(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Json(body): Json<CreatePrivateRequests>,
) -> Result<(StatusCode, Json<CreatedPrivateRequests>), ApiError> {
    enter(&state, &session).await?;
    let member_id = session.subject.id;
    let request_mode = entitled(&state, member_id, Category::Adult).await?;
    let wanted = wanted_scenes(&body)?;
    let source = ready_source(&state, Category::Adult, MediaKind::Scene).await?;
    let items = record_scenes(&state, &wanted).await?;
    let mut created = Vec::new();
    let mut skipped = Vec::new();
    let mut submit = false;
    for (scene, item) in wanted.iter().zip(&items) {
        match request_scene(&state, &session, (&scene.id, item), request_mode, &source).await? {
            Outcome::Created(made) => {
                submit |= made.submit;
                created.push(made.request_id);
            }
            Outcome::Skipped(reason) => skipped.push(SkippedScene {
                id: scene.id.clone(),
                reason,
            }),
        }
    }
    if created.is_empty() {
        let all_available = skipped.iter().all(|s| s.reason == SkipReason::Available);
        return Err(ApiError::Conflict(if all_available {
            "requests.already_available"
        } else {
            "requests.already_requested"
        }));
    }
    if submit {
        enqueue_submit(&state).await?;
    }
    notifications::enqueue_due(&state).await?;
    let requests = state
        .db
        .list_requests(RequestFilter {
            member_id: Some(member_id),
            category: Some(Category::Adult),
            ..RequestFilter::default()
        })
        .await?
        .into_iter()
        .filter(|r| created.contains(&r.id))
        .collect();
    Ok((
        StatusCode::CREATED,
        Json(CreatedPrivateRequests {
            requests: my_requests(&state, requests).await?,
            skipped,
        }),
    ))
}

/// The member's requests in the private category, newest first (spec 0007
/// story 7: the private area's own tab); hidden ones only with
/// `hidden=true`. Withdraw, cancel and hide use the general request
/// routes, which answer for private requests only through the gate.
/// Listing is no activity: it does not keep the unlock open.
#[utoipa::path(get, path = "/api/v1/private/requests", tag = "private",
    params(PrivateRequestsQuery),
    responses(
        (status = 200, body = Vec<MyRequest>),
        (status = 404, body = ErrorBody, description = "`private.not_found` (the gate is closed)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_private_requests(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Query(query): Query<PrivateRequestsQuery>,
) -> Result<Json<Vec<MyRequest>>, ApiError> {
    // The gate, without extending the unlock: the tab polls this list, and
    // polling is no activity (ADR 0014), so a forgotten device still closes.
    if !privacy::adult_open(&state, &session).await? {
        return Err(ApiError::NotFound("private.not_found"));
    }
    let requests = state
        .db
        .list_requests(RequestFilter {
            member_id: Some(session.subject.id),
            category: Some(Category::Adult),
            without_hidden: !query.hidden,
            ..RequestFilter::default()
        })
        .await?;
    Ok(Json(my_requests(&state, requests).await?))
}
