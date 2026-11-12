//! Requests (spec 0003 slice 4).
//!
//! Members (`/api/v1/requests…`): create a request for a movie, chosen
//! seasons or albums (spec 0006 slice 4), list their own, withdraw pending ones, cancel approved ones
//! whose download has not started, hide finished ones (spec 0004), and look
//! up what is available or requested. Members only ever see their own
//! requests; for others' they learn "requested", never by whom (D10).
//!
//! Admins (`/api/v1/admin/…`): the approval inbox ("Needs you", D16),
//! approve, reject and revoke in bulk naming each request's owner (lesson
//! 4), all requests with their owner (hidden ones too), and every
//! acquisition with the source's reason when it failed, whether nobody
//! wants it any more and whether a stop waits.
//!
//! Submission happens in the worker (`requests.submit`); the web side only
//! enqueues it (ADR 0004) and never opens source credentials (ADR 0003).
//! The catalog (TMDB, `MusicBrainz`) is read here with the web key
//! (amendment 1) to record the requested title.

use std::collections::{BTreeMap, BTreeSet};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::catalog::{Catalog, CatalogDetails, CatalogRef, ExternalId, MediaKind};
use starr_core::categories::{category_visible, is_private};
use starr_core::follows;
use starr_core::jobs::{JobKind, Trigger};
use starr_core::journey::Audience;
use starr_core::members::Category;
use starr_core::requests::{
    self as rules, AcquisitionState, Availability, Decision, DownloadStage, RequestState,
};
use starr_db::catalog::{CatalogItem, ItemKind, Provenance};
use starr_db::follows::Cutoff;
use starr_db::jobs::NewJob;
use starr_db::requests::{
    AcquisitionRecord, Actor, CancelOutcome, DecisionOutcome, DecisionTarget, EventTrigger,
    HideOutcome, ItemAlbum, NewRequest, RequestEvent, RequestFilter, RequestFollow,
    RequestItemRecord, RequestRecord, RetryOutcome, RevokeOutcome, SourceTarget, TitlePresence,
    Unit, UnitFacts, WithdrawOutcome,
};
use utoipa::{IntoParams, ToSchema};

use super::catalog::{MediaKindDto, adapter_error, file_name, open_catalog};
use super::common::{CategoryDto, OpenInLink, audit, audit_session, open_in_links};
use super::error::{ApiError, ErrorBody};
use super::follows::follow_series;
use super::journey::{JourneyDto, acquisition_journey, item_journey};
use super::notifications;
use super::privacy;
use crate::AppState;
use crate::auth::{AdminSession, MemberSession, Session};

/// Language of recorded titles (D5: English in v1).
const LANGUAGE: &str = "en-US";
/// How long recorded catalog metadata is kept before it counts as stale.
const METADATA_TTL: TimeDelta = TimeDelta::days(30);
/// Most titles one status lookup answers.
const STATUS_MAX_IDS: usize = 50;
/// Most requests one bulk decision takes.
const DECISION_MAX: usize = 200;

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
    /// State of a request or one of its items (user story 4).
    RequestStateDto as RequestState from RequestState {
        PendingApproval, Approved, Submitted, InProgress, PartiallyAvailable, Available,
        Rejected, Cancelled, Failed, Revoked, Removed,
    }
);

dto_enum!(
    /// What was decided about an item. `cancelled`: withdrawn before or
    /// cancelled after approval by the member; `revoked`: an admin revoked
    /// it; `removed`: the title left the library.
    DecisionDto as RequestDecision from Decision {
        PendingApproval, Approved, Rejected, Cancelled, Revoked, Removed,
    }
);

dto_enum!(
    /// State of a shared acquisition at a source. `cancelled`: nobody wants
    /// it any more; nothing is sent for it.
    AcquisitionStateDto as AcquisitionState from AcquisitionState {
        Pending, Submitted, Downloading, PartiallyAvailable, Available, Failed, Cancelled,
    }
);

dto_enum!(
    /// How far a download got once nothing is left to load: `importing`
    /// (the source imports the finished download) or `arriving` (the source
    /// has it, the media server does not list it yet). Only while in
    /// progress (downloading).
    DownloadStageDto as DownloadStage from DownloadStage {
        Importing, Arriving,
    }
);

/// Where an arriving title will show up: the category's media servers by
/// name, only for the `arriving` stage.
pub(crate) fn arriving_in(stage: Option<DownloadStage>, library: Option<String>) -> Option<String> {
    library.filter(|_| stage == Some(DownloadStage::Arriving))
}

dto_enum!(
    /// What a member sees for a title or season before requesting.
    AvailabilityDto as Availability from Availability {
        New, Requested, PartiallyAvailable, Available,
    }
);

dto_enum!(
    /// Who caused a change of a request.
    EventTriggerDto as RequestEventTrigger from EventTrigger {
        Member, Admin, System,
    }
);

// ---------------------------------------------------------------- member

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRequest {
    pub(crate) kind: MediaKindDto,
    /// The title's id in the category's catalog (TMDB for movies and
    /// series, `MusicBrainz` for artists and albums), as in `TitleCard.id`.
    /// `album` requests that one album under its artist.
    pub(crate) title_id: String,
    /// Series: the seasons to request (at least one). Otherwise absent.
    #[serde(default)]
    pub(crate) seasons: Option<Vec<u32>>,
    /// Artists: the albums to request (release-group MBIDs of the
    /// artist's discography, at least one). Otherwise absent.
    #[serde(default)]
    pub(crate) albums: Option<Vec<String>>,
    /// Series: also follow new seasons (D8; `follows.check`). Music: also
    /// follow the artist (new albums, spec 0006 slice 6). Followed even
    /// when nothing is left to request.
    #[serde(default)]
    pub(crate) follow: bool,
}

/// The album a music request item asks for.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestAlbum {
    /// Release-group MBID (opens `/api/v1/albums/{id}`).
    id: String,
    title: String,
    year: Option<i32>,
    /// Release-group MBID for `/api/v1/images/caa/{size}/{ref}`.
    cover: Option<String>,
}

impl RequestAlbum {
    pub(crate) fn of(album: Option<ItemAlbum>) -> Option<Self> {
        album.map(|a| Self {
            id: a.release_group.hyphenated().to_string(),
            title: a.title,
            year: a.year,
            cover: file_name(a.poster_path),
        })
    }
}

/// One unit of the member's request.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyRequestItem {
    /// Series: the season; absent otherwise.
    season: Option<u32>,
    /// Music: the album; absent otherwise.
    album: Option<RequestAlbum>,
    state: RequestStateDto,
    /// Downloaded fraction 0–1 while in progress.
    progress: Option<f64>,
    /// In progress: importing, or arriving in the media server.
    stage: Option<DownloadStageDto>,
    /// The steps behind the item's status chip.
    journey: JourneyDto,
    /// Music: where the album can be listened to once (partly) available;
    /// absent otherwise.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schema(required = false)]
    open_in: Vec<OpenInLink>,
}

/// A request of the logged-in member. Carries no other member's data.
#[allow(clippy::struct_excessive_bools)] // independent flags of a response
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyRequest {
    id: i64,
    kind: MediaKindDto,
    /// The title's id in the category's catalog; with `kind` it opens the
    /// title (`/api/v1/titles/{kind}/{id}`).
    title_id: Option<String>,
    title: String,
    year: Option<i32>,
    /// Poster file for `/api/v1/images/tmdb/{size}/{file}`; music: the
    /// first album's release-group MBID for `/api/v1/images/caa/{size}/{ref}`.
    poster: Option<String>,
    category: CategoryDto,
    state: RequestStateDto,
    /// In progress as a whole: the least advanced stage of its downloads
    /// (absent while any still loads).
    stage: Option<DownloadStageDto>,
    /// The request or one of its items arriving: the media server(s) it
    /// will show up in.
    arriving_in: Option<String>,
    /// Set when an admin rejected (part of) the request.
    reject_reason: Option<String>,
    /// Set when an admin revoked the request.
    revoke_reason: Option<String>,
    follow: bool,
    /// Some items still wait for approval (D11: withdraw until approved).
    can_withdraw: bool,
    /// Something can still be cancelled: waiting for approval, or approved
    /// and not downloading yet (spec 0004).
    can_cancel: bool,
    /// Finished: it can be hidden from the list.
    can_hide: bool,
    /// The member hid it (listed only with `hidden=true`).
    hidden: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    items: Vec<MyRequestItem>,
    /// Something is watchable: where to open it (O3).
    open_in: Vec<OpenInLink>,
}

/// Why a wanted unit was left out of a new request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// It is available already.
    Available,
    /// The member has an open request for it.
    AlreadyRequested,
}

/// A wanted unit that was left out.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkippedUnit {
    /// The season; absent otherwise.
    season: Option<u32>,
    /// The album's release-group MBID; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    album: Option<String>,
    pub(crate) reason: SkipReason,
}

/// The new request, and the wanted units left out of it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatedRequest {
    pub(crate) request: MyRequest,
    pub(crate) skipped: Vec<SkippedUnit>,
}

/// The media kind of a request's title; `None` for kinds requests do not
/// exist for in this release (written by a newer one before a rollback:
/// such requests are left out, ADR 0012). Music requests are the artist's
/// (one item per album); a scene is its own request (spec 0007 slice 4).
pub(crate) fn media(kind: ItemKind) -> Option<MediaKindDto> {
    match kind {
        ItemKind::Movie => Some(MediaKindDto::Movie),
        ItemKind::Series | ItemKind::Season => Some(MediaKindDto::Series),
        ItemKind::Artist => Some(MediaKindDto::Artist),
        ItemKind::Album => Some(MediaKindDto::Album),
        ItemKind::Scene => Some(MediaKindDto::Scene),
        ItemKind::Performer | ItemKind::Studio => None,
    }
}

/// The poster a request shows: the title's, for music the first album's
/// cover (an artist has no picture of its own, spec 0006 Q8).
fn request_poster(r: &RequestRecord) -> Option<String> {
    let album = r
        .items
        .iter()
        .find_map(|i| i.album.as_ref())
        .and_then(|a| a.poster_path.clone());
    file_name(album.or_else(|| r.poster_path.clone()))
}

impl MyRequest {
    /// `open_in`: the title's links (movies, series); `albums`: the albums'
    /// links by release group (music).
    fn new(
        r: RequestRecord,
        events: &[RequestEvent],
        open_in: Vec<OpenInLink>,
        albums: &BTreeMap<uuid::Uuid, Vec<starr_db::inventory::TitleLink>>,
    ) -> Option<Self> {
        let kind = media(r.kind)?;
        let album_links = |item: &RequestItemRecord| -> Vec<OpenInLink> {
            match &item.album {
                Some(album) if listenable(item) => albums
                    .get(&album.release_group)
                    .cloned()
                    .map_or_default(open_in_links),
                _ => Vec::new(),
            }
        };
        let item_links: Vec<Vec<OpenInLink>> = r.items.iter().map(album_links).collect();
        // A request of one album opens it from the request card too.
        let open_in = match item_links.as_slice() {
            [only] if open_in.is_empty() => only.clone(),
            _ => open_in,
        };
        let (request_stage, arriving_in) = request_stage_of(r.state, &r.items);
        let journeys: Vec<JourneyDto> = r
            .items
            .iter()
            .map(|i| item_journey(Audience::Member, &r, i, events))
            .collect();
        let poster = request_poster(&r);
        let title_id = title_id_of(&r);
        Some(Self {
            open_in,
            stage: request_stage.map(Into::into),
            arriving_in,
            can_withdraw: r
                .items
                .iter()
                .any(|i| i.decision == Decision::PendingApproval),
            can_cancel: r.items.iter().any(|i| {
                rules::member_end(i.decision, i.acquisition.as_ref().map(|a| a.state)).is_some()
            }),
            can_hide: r.state.can_hide(),
            hidden: r.hidden_at.is_some(),
            revoke_reason: r.revoke_reason,
            items: r
                .items
                .into_iter()
                .zip(journeys)
                .zip(item_links)
                .map(|((i, journey), open_in)| MyRequestItem {
                    season: i.season_number,
                    state: i.state.into(),
                    progress: progress(&i),
                    stage: stage(&i).map(Into::into),
                    album: RequestAlbum::of(i.album),
                    journey,
                    open_in,
                })
                .collect(),
            id: r.id,
            kind,
            title_id,
            title: r.title,
            year: r.year,
            poster,
            category: r.category.into(),
            state: r.state.into(),
            reject_reason: r.reject_reason,
            follow: r.follow,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

fn progress(item: &RequestItemRecord) -> Option<f64> {
    item.acquisition.as_ref().and_then(|a| a.progress)
}

/// A request's stage ([`rules::request_stage`]) and, when any of its items
/// is arriving, the media server(s) it will show up in. One rule for the
/// member's and the admin's view.
fn request_stage_of(
    state: RequestState,
    items: &[RequestItemRecord],
) -> (Option<DownloadStage>, Option<String>) {
    let stages: Vec<(RequestState, Option<DownloadStage>)> =
        items.iter().map(|i| (i.state, stage(i))).collect();
    let arriving = stages
        .iter()
        .any(|(_, s)| *s == Some(DownloadStage::Arriving));
    let library = items
        .iter()
        .find_map(|i| i.acquisition.as_ref()?.library_name.as_deref())
        .filter(|_| arriving)
        .map(str::to_owned);
    (rules::request_stage(state, &stages), library)
}

fn stage(item: &RequestItemRecord) -> Option<DownloadStage> {
    item.acquisition.as_ref().and_then(|a| a.stage)
}

pub(crate) fn category_of(kind: MediaKind) -> Category {
    match kind {
        MediaKind::Movie => Category::Movies,
        MediaKind::Series => Category::Series,
        MediaKind::Artist | MediaKind::Album => Category::Music,
        MediaKind::Scene | MediaKind::Performer | MediaKind::Studio => Category::Adult,
    }
}

fn member_actor(session: &Session) -> Actor<'_> {
    Actor {
        trigger: EventTrigger::Member,
        name: &session.subject.username,
    }
}

fn admin_actor(session: &Session) -> Actor<'_> {
    Actor {
        trigger: EventTrigger::Admin,
        name: &session.subject.username,
    }
}

/// The member may request in `category` (entitlement switched on, D6).
pub(crate) async fn entitled(
    state: &AppState,
    member_id: i64,
    category: Category,
) -> Result<starr_core::members::RequestMode, ApiError> {
    state
        .db
        .entitlements(member_id)
        .await?
        .into_iter()
        .find(|e| e.category == category && e.enabled)
        .map(|e| e.request_mode)
        .ok_or(ApiError::Validation("requests.not_entitled"))
}

/// The category's source with complete settings, as a new acquisition
/// takes them along (D9); `None` while the category is not ready.
pub(crate) async fn source_target(
    state: &AppState,
    category: Category,
) -> Result<Option<SourceTarget>, ApiError> {
    Ok(state.db.source_target(category).await?)
}

/// The source of `category` when members can request `kind` there: the
/// category is switched on and its source settings are complete.
pub(crate) async fn ready_source(
    state: &AppState,
    category: Category,
    kind: MediaKind,
) -> Result<SourceTarget, ApiError> {
    let found = state.db.category_state(category).await?;
    // The private category counts as on only with a valid operator
    // statement (ADR 0014); provisioning keeps reading the plain switch.
    let enabled = if is_private(category) {
        found.enabled_with_statement()
    } else {
        found.enabled
    };
    match source_target(state, category).await? {
        Some(source) if enabled && category_of(kind) == category => Ok(source),
        _ => Err(ApiError::Conflict("requests.category_not_ready")),
    }
}

/// When [`record_title`] writes the catalog item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Record {
    /// Always (requests store the latest metadata with them).
    Always,
    /// Only when it is missing, expired or lacks a season the catalog
    /// lists now (viewing a title).
    IfStale,
}

/// The title the API addresses as `kind` plus the catalog's id `id`
/// (ADR 0012). Movies and series come from TMDB, so the id is a TMDB id;
/// anything else names no title.
pub(crate) fn title_ref(kind: MediaKind, id: &str) -> Option<CatalogRef> {
    // Canonical ids only (no sign, padding, blanks or upper case), so the
    // id a client sent is the id it gets back.
    let canonical = match kind {
        MediaKind::Movie | MediaKind::Series => id
            .parse::<u64>()
            .ok()
            .filter(|v| *v > 0)
            .is_some_and(|v| v.to_string() == id),
        // MusicBrainz MBIDs (spec 0006).
        MediaKind::Artist | MediaKind::Album => {
            uuid::Uuid::parse_str(id).is_ok_and(|v| v.hyphenated().to_string() == id)
        }
        // `StashDB` UUIDs: only the private area's routes take them (spec
        // 0007); the global routes refuse private kinds before this.
        MediaKind::Scene | MediaKind::Performer | MediaKind::Studio => {
            uuid::Uuid::parse_str(id).is_ok_and(|v| v.hyphenated().to_string() == id)
        }
    };
    canonical.then(|| CatalogRef {
        kind,
        id: id.to_owned(),
    })
}

/// [`title_ref`] for a path or body: an id that names no title is not
/// found.
pub(crate) fn title_ref_or_404(kind: MediaKind, id: &str) -> Result<CatalogRef, ApiError> {
    title_ref(kind, id).ok_or(ApiError::NotFound("catalog.not_found"))
}

/// The natural key the catalog's id is (TMDB for movies and series,
/// `MusicBrainz` MBIDs for artists and albums).
pub(crate) fn natural_key(title: &CatalogRef) -> Option<ExternalId> {
    match title.kind {
        MediaKind::Movie | MediaKind::Series => title.id.parse().ok().map(ExternalId::Tmdb),
        MediaKind::Artist => uuid::Uuid::parse_str(&title.id)
            .ok()
            .map(ExternalId::MbArtist),
        MediaKind::Album => uuid::Uuid::parse_str(&title.id)
            .ok()
            .map(ExternalId::MbReleaseGroup),
        MediaKind::Scene => uuid::Uuid::parse_str(&title.id)
            .ok()
            .map(ExternalId::StashScene),
        MediaKind::Performer => uuid::Uuid::parse_str(&title.id)
            .ok()
            .map(ExternalId::StashPerformer),
        MediaKind::Studio => uuid::Uuid::parse_str(&title.id)
            .ok()
            .map(ExternalId::StashStudio),
    }
}

/// A requested title's id in its catalog, as the API shows it: the TMDB id,
/// the artist's MBID for music, the `StashDB` UUID for a scene.
pub(crate) fn title_id_of(r: &RequestRecord) -> Option<String> {
    match r.kind {
        ItemKind::Artist => r.mb_artist.map(|id| id.hyphenated().to_string()),
        ItemKind::Scene => r.stash_scene.map(|id| id.hyphenated().to_string()),
        _ => r.tmdb_id.map(|v| v.to_string()),
    }
}

/// Read `title` from the category's catalog, record it (with its seasons)
/// as `record` says and return both.
pub(crate) async fn record_title(
    state: &AppState,
    category: Category,
    title: &CatalogRef,
    record: Record,
) -> Result<(CatalogDetails, CatalogItem), ApiError> {
    let kind = title.kind;
    let role = state
        .db
        .default_catalog(category)
        .await?
        .ok_or(ApiError::Conflict("requests.category_not_ready"))?;
    let catalog = open_catalog(state, role.service_instance_id).await?;
    let details = catalog
        .details(title, LANGUAGE)
        .await
        .map_err(|e| adapter_error(&e))?;
    let now = state.clock.now();
    if record == Record::IfStale
        && let Some(item) = state
            .db
            .resolve_catalog_item(kind, natural_key(title).as_slice())
            .await?
        && item.metadata_expires_at.is_some_and(|at| at > now)
    {
        let known = state.db.catalog_item_seasons(item.id).await?;
        let complete = details
            .seasons()
            .all(|s| known.iter().any(|k| k.season_number == Some(s.number)));
        if complete {
            return Ok((details, item));
        }
    }
    let item = state
        .db
        .upsert_catalog_item(
            &details,
            &Provenance {
                adapter_type: &role.adapter_type,
                instance_id: role.service_instance_id,
                fetched_at: now,
                expires_at: now + METADATA_TTL,
            },
            now,
        )
        .await?;
    Ok((details, item))
}

/// What the consume services have of `item`; an artist's albums (in
/// `facts`) by release group.
async fn presence_of(
    state: &AppState,
    category: Category,
    item: &CatalogItem,
    facts: &[UnitFacts],
) -> Result<TitlePresence, ApiError> {
    if matches!(item.kind, ItemKind::Artist | ItemKind::Album) {
        let albums: Vec<uuid::Uuid> = facts.iter().filter_map(|f| f.release_group).collect();
        return Ok(state.db.album_presence(category, &albums).await?);
    }
    Ok(state
        .db
        .title_presence(
            category,
            item.kind,
            item.tmdb_id,
            item.tvdb_id,
            item.imdb_id.as_deref(),
        )
        .await?)
}

/// A unit's download stage as members see it: only while it shows as
/// requested (the media server may list it before the next sync).
pub(crate) fn unit_stage(facts: &UnitFacts, presence: &TitlePresence) -> Option<DownloadStage> {
    facts
        .stage
        .filter(|_| availability(facts, presence) == Availability::Requested)
}

pub(crate) fn availability(facts: &UnitFacts, presence: &TitlePresence) -> Availability {
    rules::unit_availability(
        facts.acquisition,
        facts.open_requests,
        presence.unit(facts.season_number, facts.release_group),
        facts.expected_episodes,
    )
}

/// Enqueue `requests.submit` (deduplicated with a pending or running one)
/// and wake the worker.
pub(crate) async fn enqueue_submit(state: &AppState) -> Result<(), ApiError> {
    let now = Utc::now();
    let kind = JobKind::RequestsSubmit;
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

/// The units of `title` to request: the movie, or the wanted seasons.
async fn units_of(
    state: &AppState,
    kind: MediaKind,
    title: &CatalogItem,
    wanted: &BTreeSet<u32>,
) -> Result<Vec<Unit>, ApiError> {
    Ok(match kind {
        MediaKind::Movie => vec![Unit {
            catalog_item_id: title.id,
            season_number: None,
        }],
        MediaKind::Series => {
            if title.tvdb_id.is_none() {
                return Err(ApiError::Validation("requests.no_tvdb_id"));
            }
            let seasons = state.db.catalog_item_seasons(title.id).await?;
            wanted
                .iter()
                .map(|n| {
                    seasons
                        .iter()
                        .find(|s| s.season_number == Some(*n))
                        .map(|s| Unit {
                            catalog_item_id: s.id,
                            season_number: Some(*n),
                        })
                        .ok_or(ApiError::Validation("requests.unknown_season"))
                })
                .collect::<Result<_, _>>()?
        }
        // Music units are albums (`music::create_album_request`).
        MediaKind::Artist | MediaKind::Album => {
            return Err(ApiError::Validation("requests.albums_required"));
        }
        // Scenes are requested through the private area's own route.
        MediaKind::Scene | MediaKind::Performer | MediaKind::Studio => {
            return Err(ApiError::NotFound("catalog.not_found"));
        }
    })
}

/// Split `wanted` into the units to request and those left out: available
/// already (consume services or a finished acquisition) or asked for by
/// the member already.
pub(crate) async fn skip_known(
    state: &AppState,
    category: Category,
    title: &CatalogItem,
    member_id: i64,
    wanted: Vec<Unit>,
) -> Result<(Vec<Unit>, Vec<SkippedUnit>), ApiError> {
    let facts = state.db.unit_facts(title.id, member_id).await?;
    let presence = presence_of(state, category, title, &facts).await?;
    Ok(split_known(&facts, &presence, wanted))
}

/// [`skip_known`] with the units' facts at hand (music: an album is a unit
/// of the artist it is stored under, which may be another credited one).
pub(crate) fn split_known(
    facts: &[UnitFacts],
    presence: &TitlePresence,
    wanted: Vec<Unit>,
) -> (Vec<Unit>, Vec<SkippedUnit>) {
    let mut units = Vec::with_capacity(wanted.len());
    let mut skipped = Vec::new();
    for unit in wanted {
        let fact = facts
            .iter()
            .find(|f| f.catalog_item_id == unit.catalog_item_id);
        let reason = match fact {
            Some(f) if availability(f, presence) == Availability::Available => {
                Some(SkipReason::Available)
            }
            Some(f) if f.mine => Some(SkipReason::AlreadyRequested),
            _ => None,
        };
        match reason {
            Some(reason) => skipped.push(SkippedUnit {
                season: unit.season_number,
                album: fact
                    .and_then(|f| f.release_group)
                    .map(|rg| rg.hyphenated().to_string()),
                reason,
            }),
            None => units.push(unit),
        }
    }
    (units, skipped)
}

pub(crate) async fn my_request(
    state: &AppState,
    member_id: i64,
    id: i64,
) -> Result<MyRequest, ApiError> {
    let request = state
        .db
        .get_request(id)
        .await?
        .filter(|r| r.member_id == member_id)
        .ok_or(ApiError::NotFound("requests.not_found"))?;
    my_requests(state, vec![request])
        .await?
        .pop()
        .ok_or(ApiError::NotFound("requests.not_found"))
}

/// Whether something of the request can be watched (then the card offers
/// "Open in").
fn watchable(request: &RequestRecord) -> bool {
    request.items.iter().any(listenable)
}

/// The item is (partly) available.
fn listenable(item: &RequestItemRecord) -> bool {
    matches!(
        item.state,
        RequestState::Available | RequestState::PartiallyAvailable
    )
}

/// The member's requests as DTOs, with journeys and watch links (both
/// loaded for all requests at once). A watchable scene opens the private
/// category's libraries, not the item (spec 0007 Q5 A).
pub(crate) async fn my_requests(
    state: &AppState,
    requests: Vec<RequestRecord>,
) -> Result<Vec<MyRequest>, ApiError> {
    let ids: Vec<i64> = requests.iter().map(|r| r.id).collect();
    let mut events = state.db.requests_events(&ids).await?;
    let titles: Vec<(i64, Category)> = requests
        .iter()
        .filter(|r| watchable(r) && r.kind != ItemKind::Scene)
        .map(|r| (r.catalog_item_id, r.category))
        .collect();
    let links = state.db.titles_links(&titles).await?;
    let scene_links = if requests
        .iter()
        .any(|r| watchable(r) && r.kind == ItemKind::Scene)
    {
        open_in_links(state.db.category_library_links(Category::Adult).await?)
    } else {
        Vec::new()
    };
    let release_groups: Vec<uuid::Uuid> = requests
        .iter()
        .flat_map(|r| &r.items)
        .filter(|i| listenable(i))
        .filter_map(|i| i.album.as_ref().map(|a| a.release_group))
        .collect();
    let albums = state.db.album_links(&release_groups).await?;
    Ok(requests
        .into_iter()
        .filter_map(|request| {
            let history = events.remove(&request.id).unwrap_or_default();
            // Two requests (seasons of one series) may share a title's links.
            let open_in = if watchable(&request) && request.kind == ItemKind::Scene {
                scene_links.clone()
            } else if watchable(&request) {
                links
                    .get(&request.catalog_item_id)
                    .cloned()
                    .map_or_default(open_in_links)
            } else {
                Vec::new()
            };
            MyRequest::new(request, &history, open_in, &albums)
        })
        .collect())
}

/// Request a movie, chosen seasons of a series, or albums (spec 0006 slice
/// 4: `artist` with `albums` from the artist page, or one `album`; the
/// request is the artist's, one item per album). With request mode `auto`,
/// or when the title is being acquired already for someone else (D10), the
/// request is approved right away; after an earlier rejection it always
/// waits for an admin. Seasons and albums that are available or already
/// requested by the member are skipped and listed.
#[utoipa::path(post, path = "/api/v1/requests", tag = "requests",
    request_body = CreateRequest,
    responses(
        (status = 201, body = CreatedRequest),
        (status = 404, body = ErrorBody, description = "`catalog.not_found` (also for `kind=scene`: scenes are requested at `/api/v1/private/requests`)"),
        (status = 409, body = ErrorBody, description = "`requests.category_not_ready`, `requests.already_requested`, `requests.already_available` (with `follow`, the series or artist is followed even then)"),
        (status = 422, body = ErrorBody, description = "`requests.not_entitled`, `requests.seasons_required`, `requests.seasons_unexpected`, `requests.unknown_season`, `requests.follow_movie`, `requests.no_tvdb_id`, `requests.albums_required`, `requests.albums_unexpected`, `requests.unknown_album`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn create_request(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Json(body): Json<CreateRequest>,
) -> Result<(StatusCode, Json<CreatedRequest>), ApiError> {
    let kind: MediaKind = body.kind.into();
    let category = category_of(kind);
    // Scenes are requested in the private area only (spec 0007 slice 4):
    // here they name nothing, for every member alike.
    if is_private(category) {
        return Err(ApiError::NotFound("catalog.not_found"));
    }
    let member_id = session.subject.id;
    let mode = entitled(&state, member_id, category).await?;
    if category == Category::Music {
        return super::music::create_album_request(&state, &session, body, mode)
            .await
            .map(|created| (StatusCode::CREATED, Json(created)));
    }
    let wanted: BTreeSet<u32> = match (kind, body.seasons) {
        (MediaKind::Movie, Some(_)) => {
            return Err(ApiError::Validation("requests.seasons_unexpected"));
        }
        (MediaKind::Movie, None) if body.follow => {
            return Err(ApiError::Validation("requests.follow_movie"));
        }
        (MediaKind::Movie, None) => BTreeSet::new(),
        (MediaKind::Series, Some(seasons)) if !seasons.is_empty() => seasons.into_iter().collect(),
        (MediaKind::Series, _) => return Err(ApiError::Validation("requests.seasons_required")),
        // Music went its own way above.
        (MediaKind::Artist | MediaKind::Album, _) => {
            return Err(ApiError::Validation("requests.albums_required"));
        }
        // Refused above: the private area has its own route.
        (MediaKind::Scene | MediaKind::Performer | MediaKind::Studio, _) => {
            return Err(ApiError::NotFound("catalog.not_found"));
        }
    };
    if body.albums.is_some() {
        return Err(ApiError::Validation("requests.albums_unexpected"));
    }
    let source = ready_source(&state, category, kind).await?;
    let title_ref = title_ref_or_404(kind, &body.title_id)?;
    let (details, title) = record_title(&state, category, &title_ref, Record::Always).await?;
    let wanted_units = units_of(&state, kind, &title, &wanted).await?;
    // Units available through the consume services (D14: already in the
    // library) or a finished acquisition, and units the member asked for
    // already, are skipped and reported; refused only if nothing is left.
    let (units, skipped) = skip_known(&state, category, &title, member_id, wanted_units).await?;
    if units.is_empty() {
        if body.follow {
            // Nothing left to request, but "also get new seasons" holds.
            follow_series(&state, &session, &title, &details).await?;
        }
        let all_available = skipped.iter().all(|s| s.reason == SkipReason::Available);
        return Err(ApiError::Conflict(if all_available {
            "requests.already_available"
        } else {
            "requests.already_requested"
        }));
    }

    let created = state
        .db
        .create_request(
            &NewRequest {
                member_id,
                catalog_item_id: title.id,
                follow: if body.follow {
                    // The seasons known now; later ones come by the follow.
                    RequestFollow::Start {
                        cutoff: Cutoff::Seasons(follows::seasons_through(
                            details.seasons(),
                            state.clock.now().date_naive(),
                        )),
                    }
                } else {
                    RequestFollow::No
                },
                units: &units,
                mode,
                source: &source,
            },
            member_actor(&session),
            state.clock.now(),
        )
        .await
        .map_err(|err| match err {
            starr_db::DbError::RequestOpen => ApiError::Conflict("requests.already_requested"),
            starr_db::DbError::AlreadyAvailable => ApiError::Conflict("requests.already_available"),
            other => other.into(),
        })?;
    audit_session(
        &state,
        &session,
        "request.create",
        privacy::audit_title(category, &title.title),
        Some(json!({
            "requestId": created.request_id,
            "seasons": wanted,
            "state": created.state.as_str(),
        })),
    )
    .await?;
    if created.submit {
        enqueue_submit(&state).await?;
    }
    notifications::enqueue_due(&state).await?;
    let request = my_request(&state, member_id, created.request_id).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedRequest { request, skipped }),
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct MyRequestsQuery {
    /// Also list the requests the member hid.
    #[serde(default)]
    hidden: bool,
}

/// The logged-in member's requests, newest first (user story 4); hidden
/// ones only with `hidden=true`. Requests of the private category are
/// listed in the private area only (`/api/v1/private/requests`, spec 0007
/// story 7).
#[utoipa::path(get, path = "/api/v1/requests", tag = "requests",
    params(MyRequestsQuery),
    responses(
        (status = 200, body = Vec<MyRequest>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_my_requests(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Query(query): Query<MyRequestsQuery>,
) -> Result<Json<Vec<MyRequest>>, ApiError> {
    let requests = state
        .db
        .list_requests(RequestFilter {
            member_id: Some(session.subject.id),
            without_hidden: !query.hidden,
            // Private requests have their own tab in the private area.
            without_category: Some(Category::Adult),
            ..RequestFilter::default()
        })
        .await?;
    Ok(Json(my_requests(&state, requests).await?))
}

/// The member's own request `id` exists and the privacy gate lets
/// `session` see it; otherwise `requests.not_found`, as for other members'
/// requests. Checked before anything is changed.
async fn ensure_visible(state: &AppState, session: &Session, id: i64) -> Result<(), ApiError> {
    let request = state
        .db
        .get_request(id)
        .await?
        .filter(|r| r.member_id == session.subject.id)
        .ok_or(ApiError::NotFound("requests.not_found"))?;
    if privacy::visible(state, session, request.category).await? {
        Ok(())
    } else {
        Err(ApiError::NotFound("requests.not_found"))
    }
}

/// Cancel what the request still asks for and has not started downloading
/// (spec 0004): items waiting for approval are withdrawn, approved ones
/// cancelled; seasons already downloading or available stay. Nothing is
/// deleted in the source: when nobody else wants the title, Starr only
/// turns off the monitoring it switched on itself.
#[utoipa::path(post, path = "/api/v1/requests/{id}/cancel", tag = "requests",
    params(("id" = i64, Path, description = "Request id")),
    responses(
        (status = 200, body = MyRequest),
        (status = 404, body = ErrorBody, description = "`requests.not_found` (also for other members' requests)"),
        (status = 409, body = ErrorBody, description = "`requests.not_cancellable` (downloading, available or ended)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn cancel_request(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<MyRequest>, ApiError> {
    let member_id = session.subject.id;
    ensure_visible(&state, &session, id).await?;
    match state
        .db
        .cancel_request(member_id, id, member_actor(&session), state.clock.now())
        .await?
    {
        CancelOutcome::NotFound => Err(ApiError::NotFound("requests.not_found")),
        CancelOutcome::NotCancellable => Err(ApiError::Conflict("requests.not_cancellable")),
        CancelOutcome::Cancelled { state: after, stop } => {
            audit_session(
                &state,
                &session,
                "request.cancel",
                None,
                Some(json!({ "requestId": id, "state": after.as_str(), "stop": stop })),
            )
            .await?;
            if stop {
                enqueue_submit(&state).await?;
            }
            Ok(Json(my_request(&state, member_id, id).await?))
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetHidden {
    hidden: bool,
}

/// Hide a finished request from the member's own list, or show it again.
/// Admins and the audit log still see it.
#[utoipa::path(put, path = "/api/v1/requests/{id}/hidden", tag = "requests",
    params(("id" = i64, Path, description = "Request id")),
    request_body = SetHidden,
    responses(
        (status = 200, body = MyRequest),
        (status = 404, body = ErrorBody, description = "`requests.not_found` (also for other members' requests)"),
        (status = 409, body = ErrorBody, description = "`requests.not_finished` (only finished requests can be hidden)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn set_request_hidden(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<SetHidden>,
) -> Result<Json<MyRequest>, ApiError> {
    let member_id = session.subject.id;
    ensure_visible(&state, &session, id).await?;
    match state
        .db
        .hide_request(member_id, id, body.hidden, state.clock.now())
        .await?
    {
        HideOutcome::NotFound => Err(ApiError::NotFound("requests.not_found")),
        HideOutcome::NotFinished => Err(ApiError::Conflict("requests.not_finished")),
        HideOutcome::Done => {
            let action = if body.hidden {
                "request.hide"
            } else {
                "request.unhide"
            };
            audit_session(
                &state,
                &session,
                action,
                None,
                Some(json!({ "requestId": id })),
            )
            .await?;
            Ok(Json(my_request(&state, member_id, id).await?))
        }
    }
}

/// Withdraw what still waits for approval (D11). Approved parts stay.
#[utoipa::path(post, path = "/api/v1/requests/{id}/withdraw", tag = "requests",
    params(("id" = i64, Path, description = "Request id")),
    responses(
        (status = 200, body = MyRequest),
        (status = 404, body = ErrorBody, description = "`requests.not_found` (also for other members' requests)"),
        (status = 409, body = ErrorBody, description = "`requests.not_pending`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn withdraw_request(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<MyRequest>, ApiError> {
    let member_id = session.subject.id;
    ensure_visible(&state, &session, id).await?;
    match state
        .db
        .withdraw_request(member_id, id, member_actor(&session), state.clock.now())
        .await?
    {
        WithdrawOutcome::NotFound => Err(ApiError::NotFound("requests.not_found")),
        WithdrawOutcome::NotPending => Err(ApiError::Conflict("requests.not_pending")),
        WithdrawOutcome::Withdrawn(_) => {
            audit_session(
                &state,
                &session,
                "request.withdraw",
                None,
                Some(json!({ "requestId": id })),
            )
            .await?;
            Ok(Json(my_request(&state, member_id, id).await?))
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct StatusQuery {
    kind: MediaKindDto,
    /// Comma-separated title ids from the category's catalog (at most 50).
    ids: String,
}

/// Status of one season for the member.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeasonStatus {
    number: u32,
    status: AvailabilityDto,
    /// The member has an open request for it.
    requested_by_me: bool,
}

/// Status of one album of an artist for the member.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlbumStatus {
    /// Release-group MBID.
    id: String,
    status: AvailabilityDto,
    /// The member has an open request for it.
    requested_by_me: bool,
}

/// Status of one title for the member: never who else requested it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TitleStatus {
    /// The title's id in the category's catalog, as asked for.
    id: String,
    status: AvailabilityDto,
    requested_by_me: bool,
    /// Series: per season, known once the title was opened or requested.
    seasons: Vec<SeasonStatus>,
    /// Artists: per album someone requested.
    albums: Vec<AlbumStatus>,
}

/// What a member sees of one unit (the movie, a season or an album).
pub(crate) struct UnitStatus {
    pub season: Option<u32>,
    pub album: Option<uuid::Uuid>,
    pub status: Availability,
    pub mine: bool,
    /// Requested and downloading: importing, or arriving.
    pub stage: Option<DownloadStage>,
}

/// What a member sees of a title: never who else requested it (D10).
pub(crate) struct MemberTitleStatus {
    pub status: Availability,
    pub requested_by_me: bool,
    /// Movies: requested and downloading, importing or arriving.
    pub stage: Option<DownloadStage>,
    /// Series: per season, known once the title was opened or requested;
    /// artists: per album someone requested.
    pub seasons: Vec<UnitStatus>,
}

/// Availability of a title for `member_id`: from the consume services,
/// acquisitions and open requests (anyone's, without names).
pub(crate) async fn member_title_status(
    state: &AppState,
    member_id: i64,
    title: &CatalogRef,
) -> Result<MemberTitleStatus, ApiError> {
    let kind = title.kind;
    let series = match kind {
        MediaKind::Movie => false,
        MediaKind::Series | MediaKind::Artist => true,
        // An album is a unit of its artist.
        MediaKind::Album => {
            return super::music::album_status(state, member_id, title, None).await;
        }
        // Scenes have their status in the private area only.
        MediaKind::Scene | MediaKind::Performer | MediaKind::Studio => {
            return Err(ApiError::NotFound("catalog.not_found"));
        }
    };
    let category = category_of(kind);
    let key = natural_key(title);
    let item = state.db.resolve_catalog_item(kind, key.as_slice()).await?;
    let Some(item) = item else {
        // Never recorded: only the consume services can know it (an
        // artist's albums are looked up by the artist view).
        let presence = if kind == MediaKind::Artist {
            TitlePresence::Absent
        } else {
            state
                .db
                .title_presence(category, kind.into(), title.id.parse().ok(), None, None)
                .await?
        };
        let status = match (presence, kind) {
            (TitlePresence::Present(_), MediaKind::Movie) => Availability::Available,
            (TitlePresence::Present(_), MediaKind::Series) => Availability::PartiallyAvailable,
            _ => Availability::New,
        };
        return Ok(MemberTitleStatus {
            status,
            requested_by_me: false,
            stage: None,
            seasons: Vec::new(),
        });
    };
    let facts = state.db.unit_facts(item.id, member_id).await?;
    let presence = presence_of(state, category, &item, &facts).await?;
    let requested_by_me = facts.iter().any(|f| f.mine);
    let status = if kind == MediaKind::Artist {
        let albums: Vec<Availability> = facts
            .iter()
            .filter(|f| f.release_group.is_some())
            .map(|f| availability(f, &presence))
            .collect();
        rules::artist_availability(&albums)
    } else if series {
        let regular: Vec<Availability> = facts
            .iter()
            .filter(|f| f.season_number.is_some_and(|n| n > 0))
            .map(|f| availability(f, &presence))
            .collect();
        rules::series_availability(&regular)
    } else {
        facts
            .first()
            .map_or(Availability::New, |f| availability(f, &presence))
    };
    let seasons = facts
        .iter()
        .filter(|f| f.season_number.is_some() || f.release_group.is_some())
        .map(|f| UnitStatus {
            season: f.season_number,
            album: f.release_group,
            status: availability(f, &presence),
            mine: f.mine,
            stage: unit_stage(f, &presence),
        })
        .collect();
    let movie_stage = if series {
        None
    } else {
        facts.first().and_then(|f| unit_stage(f, &presence))
    };
    Ok(MemberTitleStatus {
        status,
        requested_by_me,
        stage: movie_stage,
        seasons,
    })
}

async fn title_status(
    state: &AppState,
    member_id: i64,
    title: CatalogRef,
) -> Result<TitleStatus, ApiError> {
    let found = member_title_status(state, member_id, &title).await?;
    Ok(TitleStatus {
        id: title.id,
        status: found.status.into(),
        requested_by_me: found.requested_by_me,
        albums: found
            .seasons
            .iter()
            .filter_map(|u| {
                u.album.map(|album| AlbumStatus {
                    id: album.hyphenated().to_string(),
                    status: u.status.into(),
                    requested_by_me: u.mine,
                })
            })
            .collect(),
        seasons: found
            .seasons
            .into_iter()
            .filter_map(|u| {
                u.season.map(|number| SeasonStatus {
                    number,
                    status: u.status.into(),
                    requested_by_me: u.mine,
                })
            })
            .collect(),
    })
}

/// Whether titles are available, requested (by anyone, without names) or
/// new, for search results and detail views (user story 2).
#[utoipa::path(get, path = "/api/v1/requests/status", tag = "requests",
    params(StatusQuery),
    responses(
        (status = 200, body = Vec<TitleStatus>),
        (status = 404, body = ErrorBody, description = "`catalog.not_found` (`kind=scene`: the private area has its own status)"),
        (status = 422, body = ErrorBody, description = "`requests.not_entitled`, `validation.title_ids`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn request_status(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Query(query): Query<StatusQuery>,
) -> Result<Json<Vec<TitleStatus>>, ApiError> {
    let kind: MediaKind = query.kind.into();
    // Private kinds have their status in the private area only, for every
    // member alike (spec 0007).
    if is_private(category_of(kind)) {
        return Err(ApiError::NotFound("catalog.not_found"));
    }
    entitled(&state, session.subject.id, category_of(kind)).await?;
    let titles: Vec<CatalogRef> = query
        .ids
        .split(',')
        .map(|id| title_ref(kind, id))
        .collect::<Option<_>>()
        .ok_or(ApiError::Validation("validation.title_ids"))?;
    if titles.is_empty() || titles.len() > STATUS_MAX_IDS {
        return Err(ApiError::Validation("validation.title_ids"));
    }
    let mut out = Vec::with_capacity(titles.len());
    for title in titles {
        out.push(title_status(&state, session.subject.id, title).await?);
    }
    Ok(Json(out))
}

// ----------------------------------------------------------------- admin

/// The member a request belongs to.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestOwner {
    id: i64,
    username: String,
    display_name: String,
}

/// One unit of a request, as admins see it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdminRequestItem {
    id: i64,
    season: Option<u32>,
    /// Music: the album.
    album: Option<RequestAlbum>,
    decision: DecisionDto,
    state: RequestStateDto,
    decided_at: Option<DateTime<Utc>>,
    acquisition_id: Option<i64>,
    progress: Option<f64>,
    /// Downloading: importing, or arriving in the media server.
    stage: Option<DownloadStageDto>,
    /// Stage `arriving`: the media server(s) it will show up in.
    arriving_in: Option<String>,
    /// The source's reason when the acquisition failed.
    last_error: Option<String>,
    /// The source's message about a stuck download.
    source_message: Option<String>,
    /// When it was withdrawn, cancelled, revoked or removed.
    ended_at: Option<DateTime<Utc>>,
    /// The steps behind the item's status chip, with technical labels.
    journey: JourneyDto,
}

/// A request with its owner (lesson 4: lists of others' records show the
/// owner per row).
#[allow(clippy::struct_excessive_bools)] // independent flags of a response
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdminRequest {
    pub(crate) id: i64,
    /// A request in the private category while the admin's session has
    /// not unlocked it (spec 0007 Q9): title, year, poster, catalog ids,
    /// member names, reasons, albums, messages and journey actors are left
    /// out; show "Private category · request #id".
    pub(crate) redacted: bool,
    member: RequestOwner,
    kind: MediaKindDto,
    /// The title's id in the category's catalog.
    title_id: Option<String>,
    title: String,
    year: Option<i32>,
    poster: Option<String>,
    category: CategoryDto,
    state: RequestStateDto,
    /// In progress as a whole: the least advanced stage of its downloads
    /// (absent while any still loads).
    stage: Option<DownloadStageDto>,
    /// The request or one of its items arriving: the media server(s) it
    /// will show up in.
    arriving_in: Option<String>,
    reject_reason: Option<String>,
    revoke_reason: Option<String>,
    /// The member hid it from their own list.
    hidden: bool,
    /// Something is still open (waiting or approved): it can be revoked.
    can_revoke: bool,
    follow: bool,
    pub(crate) created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    items: Vec<AdminRequestItem>,
}

impl AdminRequest {
    /// The admin row of `r`; neutral (Q9 B) when `redacted` (a private
    /// request the admin's session has not unlocked; `events` are redacted
    /// by the caller then).
    fn new(mut r: RequestRecord, events: &[RequestEvent], redacted: bool) -> Option<Self> {
        if redacted {
            privacy::redact_request(&mut r);
        }
        let kind = media(r.kind)?;
        let (request_stage, library) = request_stage_of(r.state, &r.items);
        let journeys: Vec<JourneyDto> = r
            .items
            .iter()
            .map(|i| item_journey(Audience::Admin, &r, i, events))
            .collect();
        let poster = request_poster(&r);
        let title_id = title_id_of(&r);
        Some(Self {
            redacted,
            stage: request_stage.map(Into::into),
            arriving_in: library,
            id: r.id,
            member: RequestOwner {
                id: r.member_id,
                username: r.member_username,
                display_name: r.member_display_name,
            },
            kind,
            title_id,
            title: r.title,
            year: r.year,
            poster,
            category: r.category.into(),
            state: r.state.into(),
            reject_reason: r.reject_reason,
            revoke_reason: r.revoke_reason,
            hidden: r.hidden_at.is_some(),
            can_revoke: r.items.iter().any(|i| i.decision.is_open()),
            follow: r.follow,
            created_at: r.created_at,
            updated_at: r.updated_at,
            items: r
                .items
                .into_iter()
                .zip(journeys)
                .map(|(i, journey)| {
                    let item_stage = stage(&i);
                    let item_progress = progress(&i);
                    let (acquisition_id, last_error, source_message, library) = match i.acquisition
                    {
                        Some(a) => (Some(a.id), a.last_error, a.source_message, a.library_name),
                        None => (None, None, None, None),
                    };
                    AdminRequestItem {
                        progress: item_progress,
                        stage: item_stage.map(Into::into),
                        arriving_in: arriving_in(item_stage, library),
                        id: i.id,
                        season: i.season_number,
                        album: RequestAlbum::of(i.album),
                        decision: i.decision.into(),
                        state: i.state.into(),
                        decided_at: i.decided_at,
                        ended_at: i.ended_at,
                        acquisition_id,
                        last_error,
                        source_message,
                        journey,
                    }
                })
                .collect(),
        })
    }
}

/// Requests as admins see them, with journeys; private ones neutral
/// unless `session` unlocked the private category (Q9 B).
pub(crate) async fn admin_requests(
    state: &AppState,
    session: &Session,
    requests: Vec<RequestRecord>,
) -> Result<Vec<AdminRequest>, ApiError> {
    let adult = privacy::adult_open(state, session).await?;
    let ids: Vec<i64> = requests.iter().map(|r| r.id).collect();
    let mut events = state.db.requests_events(&ids).await?;
    Ok(requests
        .into_iter()
        .filter_map(|r| {
            let mut history = events.remove(&r.id).unwrap_or_default();
            let redacted = !category_visible(r.category, adult);
            if redacted {
                privacy::redact_events(&mut history);
            }
            AdminRequest::new(r, &history, redacted)
        })
        .collect())
}

/// One change of a request's state.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestEventDto {
    at: DateTime<Utc>,
    /// Absent for the creation.
    from_state: Option<RequestStateDto>,
    to_state: RequestStateDto,
    trigger: EventTriggerDto,
    /// Username, or the job for the system.
    actor: Option<String>,
    detail: Option<String>,
}

impl From<RequestEvent> for RequestEventDto {
    fn from(e: RequestEvent) -> Self {
        Self {
            at: e.at,
            from_state: e.from_state.map(Into::into),
            to_state: e.to_state.into(),
            trigger: e.trigger.into(),
            actor: e.actor,
            detail: e.detail,
        }
    }
}

/// A request with its history.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdminRequestDetail {
    request: AdminRequest,
    events: Vec<RequestEventDto>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct RequestQuery {
    state: Option<RequestStateDto>,
    category: Option<CategoryDto>,
    member_id: Option<i64>,
}

/// A request named with its owner, for decisions on someone else's record.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestRef {
    id: i64,
    member_id: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApproveRequests {
    requests: Vec<RequestRef>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RejectRequests {
    requests: Vec<RequestRef>,
    /// Shown to the member (1–500 characters).
    reason: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevokeRequests {
    requests: Vec<RequestRef>,
    /// Shown to the member (1–500 characters).
    reason: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RevokeResult {
    /// Requests whose open items were revoked.
    revoked: Vec<i64>,
    /// Requests with nothing open (ended meanwhile).
    skipped: Vec<i64>,
}

impl From<RevokeOutcome> for RevokeResult {
    fn from(o: RevokeOutcome) -> Self {
        Self {
            revoked: o.revoked,
            skipped: o.skipped,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionResult {
    /// Requests whose pending items were decided.
    decided: Vec<i64>,
    /// Requests with nothing pending (decided or withdrawn meanwhile).
    skipped: Vec<i64>,
    /// Approvals: they can still be undone (`POST
    /// /api/v1/admin/approvals/undo`), because nothing went to a source for
    /// them (held at a paused source, joining an acquisition someone else
    /// wanted, or available already). Always false for rejections (D11).
    undoable: bool,
    /// Approvals that wait because the source's writes are paused.
    held: Vec<HeldDecision>,
}

/// An approved request held at a paused source.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeldDecision {
    request_id: i64,
    /// The source whose writes are paused.
    source: Option<String>,
}

impl From<DecisionOutcome> for DecisionResult {
    fn from(o: DecisionOutcome) -> Self {
        Self {
            decided: o.decided,
            skipped: o.skipped,
            undoable: false,
            held: vec![],
        }
    }
}

fn targets(refs: &[RequestRef]) -> Result<Vec<DecisionTarget>, ApiError> {
    if refs.is_empty() || refs.len() > DECISION_MAX {
        return Err(ApiError::Validation("requests.selection_invalid"));
    }
    Ok(refs
        .iter()
        .map(|r| DecisionTarget {
            request_id: r.id,
            member_id: r.member_id,
        })
        .collect())
}

fn decision_error(err: starr_db::DbError) -> ApiError {
    match err {
        starr_db::DbError::OwnerMismatch => ApiError::Conflict("requests.owner_mismatch"),
        starr_db::DbError::CategorySourceMissing => {
            ApiError::Conflict("requests.category_not_ready")
        }
        starr_db::DbError::UndoTooLate => ApiError::Conflict("requests.undo_too_late"),
        other => other.into(),
    }
}

/// Requests waiting for an admin, oldest first ("Needs you", D16): who
/// asked for what, and when.
#[utoipa::path(get, path = "/api/v1/admin/approvals", tag = "requests",
    responses(
        (status = 200, body = Vec<AdminRequest>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_approvals(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<AdminRequest>>, ApiError> {
    let requests = state
        .db
        .list_requests(RequestFilter {
            needs_decision: true,
            oldest_first: true,
            ..RequestFilter::default()
        })
        .await?;
    Ok(Json(admin_requests(&state, &session, requests).await?))
}

/// Approve the pending items of the given requests (bulk, all or
/// nothing). Each request names its owner.
#[utoipa::path(post, path = "/api/v1/admin/approvals/approve", tag = "requests",
    request_body = ApproveRequests,
    responses(
        (status = 200, body = DecisionResult),
        (status = 409, body = ErrorBody, description = "`requests.owner_mismatch`, `requests.category_not_ready`"),
        (status = 422, body = ErrorBody, description = "`requests.selection_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn approve_requests(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Json(body): Json<ApproveRequests>,
) -> Result<Json<DecisionResult>, ApiError> {
    let targets = targets(&body.requests)?;
    let mut sources = Vec::new();
    for category in Category::ALL {
        if let Some(source) = source_target(&state, category).await? {
            sources.push(source);
        }
    }
    let outcome = state
        .db
        .approve_requests(&targets, &sources, admin_actor(&session), state.clock.now())
        .await
        .map_err(decision_error)?;
    if !outcome.decided.is_empty() {
        audit(
            &state,
            &session.subject,
            "requests.approve",
            None,
            Some(json!({ "requestIds": outcome.decided })),
        )
        .await?;
    }
    let now = state.clock.now();
    // Checked before the submission is queued: what goes out right away
    // cannot be undone.
    let undoable =
        !outcome.decided.is_empty() && state.db.approvals_undoable(&outcome.decided, now).await?;
    let mut held = Vec::new();
    for id in &outcome.decided {
        let Some(request) = state.db.get_request(*id).await? else {
            continue;
        };
        if let Some(a) = request
            .items
            .iter()
            .filter_map(|i| i.acquisition.as_ref())
            .find(|a| a.held)
        {
            held.push(HeldDecision {
                request_id: *id,
                source: a.service_name.clone(),
            });
        }
    }
    if outcome.submit {
        enqueue_submit(&state).await?;
    }
    notifications::enqueue_due(&state).await?;
    Ok(Json(DecisionResult {
        undoable,
        held,
        ..outcome.into()
    }))
}

/// Take fresh approvals back (spec 0008 F-4, all or nothing): the
/// requests wait for approval again. Only while nothing went to a source
/// for them (`undoable` in the approval's answer) and within minutes of
/// the approval; a rejection is never undone (spec 0003 D11). The member's
/// "approved" notification is withdrawn unless it was delivered already.
#[utoipa::path(post, path = "/api/v1/admin/approvals/undo", tag = "requests",
    request_body = ApproveRequests,
    responses(
        (status = 200, body = DecisionResult),
        (status = 409, body = ErrorBody, description = "`requests.undo_too_late` (something went to the source, or the approval is too old), `requests.owner_mismatch`"),
        (status = 422, body = ErrorBody, description = "`requests.selection_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn undo_approvals(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Json(body): Json<ApproveRequests>,
) -> Result<Json<DecisionResult>, ApiError> {
    let targets = targets(&body.requests)?;
    let outcome = state
        .db
        .undo_approvals(&targets, admin_actor(&session), state.clock.now())
        .await
        .map_err(decision_error)?;
    audit(
        &state,
        &session.subject,
        "requests.approve_undone",
        None,
        Some(json!({ "requestIds": outcome.decided })),
    )
    .await?;
    Ok(Json(outcome.into()))
}

/// Audit details of a bulk reject or revoke: the request ids and the
/// reason, unless one of them is private. An admin's words can name the
/// title, and the audit log keeps ids only for the private category (spec
/// 0007).
async fn decision_details(
    state: &AppState,
    ids: &[i64],
    reason: &str,
) -> Result<serde_json::Value, ApiError> {
    for id in ids {
        if let Some(request) = state.db.get_request(*id).await?
            && is_private(request.category)
        {
            return Ok(json!({ "requestIds": ids, "category": request.category.as_str() }));
        }
    }
    Ok(json!({ "requestIds": ids, "reason": reason }))
}

/// Reject the pending items of the given requests with a reason the
/// members see (D6). Rejected stays rejected (D10).
#[utoipa::path(post, path = "/api/v1/admin/approvals/reject", tag = "requests",
    request_body = RejectRequests,
    responses(
        (status = 200, body = DecisionResult),
        (status = 409, body = ErrorBody, description = "`requests.owner_mismatch`"),
        (status = 422, body = ErrorBody, description = "`requests.selection_invalid`, `requests.reason_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn reject_requests(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Json(body): Json<RejectRequests>,
) -> Result<Json<DecisionResult>, ApiError> {
    let targets = targets(&body.requests)?;
    let reason = rules::reject_reason(&body.reason)
        .ok_or(ApiError::Validation("requests.reason_invalid"))?;
    let outcome = state
        .db
        .reject_requests(&targets, reason, admin_actor(&session), state.clock.now())
        .await
        .map_err(decision_error)?;
    if !outcome.decided.is_empty() {
        audit(
            &state,
            &session.subject,
            "requests.reject",
            None,
            Some(decision_details(&state, &outcome.decided, reason).await?),
        )
        .await?;
    }
    notifications::enqueue_due(&state).await?;
    Ok(Json(outcome.into()))
}

/// Revoke the open items of the given requests with a reason the members
/// see (spec 0004 R3; bulk, all or nothing, each request naming its owner).
/// Waiting, approved, downloading or available: every open item ends as
/// revoked and the member is notified. Nothing is deleted anywhere: when
/// nobody else wants a title, Starr only turns off the monitoring it
/// switched on itself (a running download is left alone). The member's
/// next request for it waits for approval.
#[utoipa::path(post, path = "/api/v1/admin/requests/revoke", tag = "requests",
    request_body = RevokeRequests,
    responses(
        (status = 200, body = RevokeResult),
        (status = 409, body = ErrorBody, description = "`requests.owner_mismatch`"),
        (status = 422, body = ErrorBody, description = "`requests.selection_invalid`, `requests.reason_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn revoke_requests(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Json(body): Json<RevokeRequests>,
) -> Result<Json<RevokeResult>, ApiError> {
    let targets = targets(&body.requests)?;
    let reason = rules::reject_reason(&body.reason)
        .ok_or(ApiError::Validation("requests.reason_invalid"))?;
    let outcome = state
        .db
        .revoke_requests(&targets, reason, admin_actor(&session), state.clock.now())
        .await
        .map_err(decision_error)?;
    if !outcome.revoked.is_empty() {
        audit(
            &state,
            &session.subject,
            "requests.revoke",
            None,
            Some(decision_details(&state, &outcome.revoked, reason).await?),
        )
        .await?;
    }
    if outcome.stop {
        enqueue_submit(&state).await?;
    }
    notifications::enqueue_due(&state).await?;
    Ok(Json(outcome.into()))
}

/// All requests with their owners, newest first; filterable (user story
/// 10). Requests their members hid are listed too.
#[utoipa::path(get, path = "/api/v1/admin/requests", tag = "requests",
    params(RequestQuery),
    responses(
        (status = 200, body = Vec<AdminRequest>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_requests(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Query(query): Query<RequestQuery>,
) -> Result<Json<Vec<AdminRequest>>, ApiError> {
    let requests = state
        .db
        .list_requests(RequestFilter {
            state: query.state.map(Into::into),
            category: query.category.map(Into::into),
            member_id: query.member_id,
            ..RequestFilter::default()
        })
        .await?;
    Ok(Json(admin_requests(&state, &session, requests).await?))
}

/// One request with its owner, items and history.
#[utoipa::path(get, path = "/api/v1/admin/requests/{id}", tag = "requests",
    params(("id" = i64, Path, description = "Request id")),
    responses(
        (status = 200, body = AdminRequestDetail),
        (status = 404, body = ErrorBody, description = "`requests.not_found`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn get_request(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<AdminRequestDetail>, ApiError> {
    let request = state
        .db
        .get_request(id)
        .await?
        .ok_or(ApiError::NotFound("requests.not_found"))?;
    let redacted = !category_visible(
        request.category,
        privacy::adult_open(&state, &session).await?,
    );
    let mut events = state.db.request_events(id).await?;
    if redacted {
        privacy::redact_events(&mut events);
    }
    Ok(Json(AdminRequestDetail {
        request: AdminRequest::new(request, &events, redacted)
            .ok_or(ApiError::NotFound("requests.not_found"))?,
        events: events.into_iter().map(Into::into).collect(),
    }))
}

/// A shared acquisition at a source.
#[allow(clippy::struct_excessive_bools)] // independent facts about one acquisition
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AcquisitionDto {
    pub(crate) id: i64,
    /// In the private category while the admin's session has not unlocked
    /// it (spec 0007 Q9): title, year, poster, album, messages and the
    /// library name are left out.
    pub(crate) redacted: bool,
    title: String,
    year: Option<i32>,
    poster: Option<String>,
    /// Series: the season; absent otherwise.
    season: Option<u32>,
    /// Music: the album (`title` is the artist's).
    album: Option<RequestAlbum>,
    category: CategoryDto,
    /// Absent once the source instance was removed.
    service_id: Option<i64>,
    service_name: Option<String>,
    adapter_type: Option<String>,
    state: AcquisitionStateDto,
    progress: Option<f64>,
    /// Downloading: importing, or arriving in the media server.
    stage: Option<DownloadStageDto>,
    /// Stage `arriving`: the media server(s) it will show up in.
    arriving_in: Option<String>,
    /// The source's message about a stuck download.
    source_message: Option<String>,
    /// Error code of the last failed submission (adapter taxonomy).
    error_code: Option<String>,
    /// The readable reason of the last failure, from the source.
    last_error: Option<String>,
    submit_attempts: u32,
    /// Pending: the next submission attempt.
    next_attempt_at: Option<DateTime<Utc>>,
    submitted_at: Option<DateTime<Utc>>,
    available_at: Option<DateTime<Utc>>,
    synced_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    /// How many request items share it.
    requests: u32,
    /// Pending at a source whose writes are paused: nothing is sent until
    /// an admin resumes them (then it is submitted).
    held: bool,
    /// Nobody wants it any more, but sync still follows it: its download
    /// runs on until the file lands or the download goes away.
    unwanted: bool,
    /// A stop (turn off the monitoring Starr switched on) waits to be sent;
    /// held like any write while writes to the source are paused.
    stop_pending: bool,
    /// That stop is held: writes to the source are paused.
    stop_held: bool,
    /// When Starr turned the monitoring off.
    stopped_at: Option<DateTime<Utc>>,
    /// The steps behind the status chip, from approval on.
    journey: JourneyDto,
}

impl AcquisitionDto {
    /// The admin row of `a`; neutral (Q9 B) unless the private category
    /// is unlocked (`adult`) or `a` is not private.
    pub(crate) fn new(mut a: AcquisitionRecord, adult: bool) -> Self {
        let redacted = !category_visible(a.category, adult);
        if redacted {
            privacy::redact_acquisition(&mut a);
        }
        let journey = acquisition_journey(&a);
        Self {
            redacted,
            journey,
            id: a.id,
            title: a.title,
            year: a.year,
            // An album shows its own cover, not its artist's stand-in.
            poster: file_name(
                a.album
                    .as_ref()
                    .and_then(|al| al.poster_path.clone())
                    .or(a.poster_path),
            ),
            season: a.season_number,
            album: RequestAlbum::of(a.album),
            category: a.category.into(),
            service_id: a.service_instance_id,
            service_name: a.service_name,
            adapter_type: a.adapter_type,
            state: a.state.into(),
            progress: a.progress,
            stage: a.stage.map(Into::into),
            arriving_in: arriving_in(a.stage, a.library_name),
            source_message: a.source_message,
            error_code: a.error_code,
            last_error: a.last_error,
            submit_attempts: a.submit_attempts,
            next_attempt_at: a.next_attempt_at,
            submitted_at: a.submitted_at,
            available_at: a.available_at,
            synced_at: a.synced_at,
            created_at: a.created_at,
            updated_at: a.updated_at,
            requests: a.requests,
            held: a.held,
            unwanted: a.unwanted,
            stop_pending: a.stop_pending,
            stop_held: a.stop_held,
            stopped_at: a.stopped_at,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct AcquisitionQuery {
    state: Option<AcquisitionStateDto>,
}

/// Every acquisition with its state and, when failed, the source's reason
/// (user story 10). Newest first.
#[utoipa::path(get, path = "/api/v1/admin/acquisitions", tag = "requests",
    params(AcquisitionQuery),
    responses(
        (status = 200, body = Vec<AcquisitionDto>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_acquisitions(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Query(query): Query<AcquisitionQuery>,
) -> Result<Json<Vec<AcquisitionDto>>, ApiError> {
    let adult = privacy::adult_open(&state, &session).await?;
    let list = state
        .db
        .list_acquisitions(None, query.state.map(Into::into))
        .await?;
    Ok(Json(
        list.into_iter()
            .map(|a| AcquisitionDto::new(a, adult))
            .collect(),
    ))
}

/// An album the music source has but no music library lists by its
/// `MusicBrainz` release group ("Needs you", spec 0006 Q7).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnlistedAlbumDto {
    pub(crate) acquisition_id: i64,
    artist: String,
    album: String,
    /// Release-group MBID, for the cover (`/api/v1/images/caa/{size}/{ref}`).
    release_group: Option<String>,
    /// The source instance's name (Lidarr).
    source: Option<String>,
    /// The music consume services, " / " between several.
    libraries: Option<String>,
    /// Waiting for the libraries since.
    pub(crate) since: DateTime<Utc>,
}

/// Albums waiting longer than an hour for the music libraries although
/// the source has their files: Starr matches albums only by their
/// `MusicBrainz` release group, so files without `MusicBrainz` tags (or a
/// library that does not cover the source's folder) leave them there.
/// Oldest first.
#[utoipa::path(get, path = "/api/v1/admin/acquisitions/unlisted", tag = "requests",
    responses(
        (status = 200, body = Vec<UnlistedAlbumDto>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_unlisted_albums(
    _admin: AdminSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<UnlistedAlbumDto>>, ApiError> {
    Ok(Json(unlisted_albums(&state).await?))
}

/// Albums waiting for the music libraries (also the Inbox's
/// `album_unlisted` items).
pub(crate) async fn unlisted_albums(state: &AppState) -> Result<Vec<UnlistedAlbumDto>, ApiError> {
    let before = state.clock.now() - starr_db::requests::UNLISTED_AFTER;
    let list = state.db.unlisted_albums(before).await?;
    Ok(list
        .into_iter()
        .map(|a| UnlistedAlbumDto {
            acquisition_id: a.acquisition_id,
            artist: a.artist,
            album: a.album,
            release_group: a.release_group.map(|rg| rg.hyphenated().to_string()),
            source: a.source,
            libraries: a.libraries,
            since: a.since,
        })
        .collect())
}

/// Start a failed acquisition over: submitted again by the worker.
#[utoipa::path(post, path = "/api/v1/admin/acquisitions/{id}/retry", tag = "requests",
    params(("id" = i64, Path, description = "Acquisition id")),
    responses(
        (status = 200, body = AcquisitionDto),
        (status = 404, body = ErrorBody, description = "`acquisitions.not_found`"),
        (status = 409, body = ErrorBody, description = "`acquisitions.not_failed`, `requests.category_not_ready` (its source was removed and the category has none)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn retry_acquisition(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<AcquisitionDto>, ApiError> {
    let find = async |id: i64| {
        state
            .db
            .list_acquisitions(Some(id), None)
            .await?
            .pop()
            .ok_or(ApiError::NotFound("acquisitions.not_found"))
    };
    let before = find(id).await?;
    // Only used when its source was removed: it moves to the current one.
    let source = source_target(&state, before.category).await?;
    let target = match state
        .db
        .retry_acquisition(
            id,
            source.as_ref(),
            admin_actor(&session),
            state.clock.now(),
        )
        .await?
    {
        RetryOutcome::Retried(target) => target,
        RetryOutcome::NotFailed => return Err(ApiError::Conflict("acquisitions.not_failed")),
        RetryOutcome::NoSource => return Err(ApiError::Conflict("requests.category_not_ready")),
    };
    // The audit log names no private title and keeps no source text of it.
    let details = if is_private(before.category) {
        json!({ "acquisitionId": id, "category": before.category.as_str() })
    } else {
        json!({ "acquisitionId": id, "lastError": before.last_error })
    };
    audit(
        &state,
        &session.subject,
        "acquisition.retry",
        privacy::audit_title(before.category, &before.title),
        Some(details),
    )
    .await?;
    enqueue_submit(&state).await?;
    let adult = privacy::adult_open(&state, &session).await?;
    Ok(Json(AcquisitionDto::new(find(target).await?, adult)))
}

#[cfg(test)]
mod tests {
    use starr_db::requests::AcquisitionBrief;

    use super::*;

    #[test]
    fn music_titles_are_canonical_mbids() {
        assert!(title_ref(MediaKind::Movie, "603").is_some());
        assert!(title_ref(MediaKind::Series, "1396").is_some());
        assert_eq!(title_ref(MediaKind::Movie, "0603"), None);
        assert_eq!(
            title_ref(MediaKind::Movie, "1b022e01-4da6-387b-8658-8678046e4cef"),
            None
        );
        for kind in [MediaKind::Artist, MediaKind::Album] {
            assert_eq!(title_ref(kind, "603"), None);
            assert!(title_ref(kind, "1b022e01-4da6-387b-8658-8678046e4cef").is_some());
            assert_eq!(
                title_ref(kind, "1B022E01-4DA6-387B-8658-8678046E4CEF"),
                None
            );
            assert_eq!(title_ref(kind, "1b022e014da6387b86588678046e4cef"), None);
        }
        assert_eq!(category_of(MediaKind::Album), Category::Music);
        assert_eq!(category_of(MediaKind::Artist), Category::Music);
    }

    fn season(n: u32, state: RequestState, stored: Option<DownloadStage>) -> RequestItemRecord {
        RequestItemRecord {
            id: i64::from(n),
            catalog_item_id: i64::from(n),
            season_number: Some(n),
            album: None,
            decision: Decision::Approved,
            state,
            decided_at: None,
            ended_at: None,
            acquisition: Some(AcquisitionBrief {
                id: i64::from(n),
                state: AcquisitionState::Downloading,
                progress: Some(0.5),
                source_message: None,
                last_error: None,
                stage: stored,
                library_name: Some("Kino".to_owned()),
                service_name: Some("Radarr".to_owned()),
                held: false,
                created_at: DateTime::UNIX_EPOCH,
                submitted_at: None,
                available_at: None,
                stage_since: None,
            }),
        }
    }

    #[test]
    fn a_mixed_request_has_no_stage_but_names_where_its_season_arrives() {
        let items = [
            season(1, RequestState::InProgress, Some(DownloadStage::Arriving)),
            season(2, RequestState::InProgress, None),
        ];
        assert_eq!(
            request_stage_of(RequestState::InProgress, &items),
            (None, Some("Kino".to_owned()))
        );
        let importing = [season(
            1,
            RequestState::InProgress,
            Some(DownloadStage::Importing),
        )];
        assert_eq!(
            request_stage_of(RequestState::InProgress, &importing),
            (Some(DownloadStage::Importing), None)
        );
    }
}
