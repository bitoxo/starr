//! Artists and albums for members (spec 0006 slices 2 and 4, user stories
//! 3–5): the artist and album views with each album's status, and album
//! requests.
//!
//! Details come from the music category's default catalog (`MusicBrainz`)
//! in the web process (ADR 0003 amendment 1). Covers are release-group
//! MBIDs for `/api/v1/images/caa/{size}/{ref}`; an artist shows its newest
//! album's cover (spec 0006 Q8).
//!
//! A music request is the artist's, with one item per album (release
//! group, spec 0006 Q4 A): albums are stored under their artist when they
//! are requested, and only then (an artist's discography is a catalog
//! read, not data Starr keeps). Availability (spec 0006 Q7 B, slice 5):
//! where the music category has consume services (Navidrome, the music
//! libraries of Jellyfin/Emby), an album is there when one of them lists
//! its release group, matched by MBID only; without any, when Lidarr has a
//! file for every track of the release it monitors
//! (`TitlePresence::NoService`).

use std::collections::{BTreeMap, BTreeSet};

use axum::Json;
use axum::extract::{Path, State};
use chrono::{Datelike, NaiveDate};
use serde::Serialize;
use serde_json::json;
use starr_core::adapters::AdapterError;
use starr_core::catalog::{
    Catalog, CatalogEntry, CatalogRef, Child, ExternalId, MediaKind, Relevance,
};
use starr_core::members::{Category, RequestMode};
use starr_core::requests::{self as rules, Availability};
use starr_db::catalog::{CatalogItem, Provenance};
use starr_db::requests::{NewRequest, RequestFollow, TitlePresence, Unit};
use utoipa::ToSchema;

use super::catalog::{ReleaseTypeDto, SecondaryTypeDto, adapter_error, file_name, open_catalog};
use super::common::{CategoryDto, OpenInLink, audit_session, open_in_links};
use super::discover::browsable;
use super::error::{ApiError, ErrorBody};
use super::follows::{artist_cutoff, follow_artist};
use super::journey::{MemberRequests, UnitJourney};
use super::notifications;
use super::requests::{
    AvailabilityDto, CreateRequest, CreatedRequest, MemberTitleStatus, SkipReason, availability,
    enqueue_submit, member_title_status, my_request, natural_key, ready_source, source_target,
    split_known, title_ref, title_ref_or_404, unit_stage,
};
use super::title_facts::{Attribution, TrackDto, TracklistReleaseDto};
use crate::AppState;
use crate::auth::{MemberSession, Session};

/// Language for catalog calls (`MusicBrainz` ignores it).
const LANGUAGE: &str = "en-US";
/// How long a recorded artist or album is kept before it counts as stale.
const METADATA_TTL: chrono::TimeDelta = chrono::TimeDelta::days(30);
/// Most albums one request takes (a discography loads at most 300, ADR
/// 0013 amendment 1).
const MAX_ALBUMS: usize = 300;

/// An album in an artist's discography.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DiscographyAlbum {
    /// Release-group MBID.
    id: String,
    title: String,
    year: Option<i32>,
    /// First release; partial dates count as their first day.
    release_date: Option<NaiveDate>,
    release_type: Option<ReleaseTypeDto>,
    secondary_types: Vec<SecondaryTypeDto>,
    /// Release-group MBID for `/api/v1/images/caa/{size}/{ref}`.
    cover: Option<String>,
    /// Shown without "Show all": the category's release types (album and
    /// EP without secondary types by default, spec 0006 Q3).
    in_default_view: bool,
    status: AvailabilityDto,
    /// The member has an open request for it.
    requested_by_me: bool,
}

/// An artist with their discography (story 3).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtistDetails {
    /// Artist MBID.
    id: String,
    category: CategoryDto,
    name: String,
    /// `MusicBrainz`'s note telling same-named artists apart.
    disambiguation: String,
    /// The newest released album's cover (no artist pictures in
    /// `MusicBrainz`).
    cover: Option<String>,
    /// Newest first, undated last.
    albums: Vec<DiscographyAlbum>,
    /// Requests can be made in the category right now (the music source is
    /// set up completely).
    requestable: bool,
    /// The journeys of the member's open album requests of this artist,
    /// one per album (`album` set).
    journeys: Vec<UnitJourney>,
    /// `MusicBrainz` genre names, most voted first.
    genres: Vec<String>,
    /// Credits for what is shown (`MusicBrainz` genres, CC BY-NC-SA).
    attributions: Vec<Attribution>,
}

/// An album (story 4).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlbumDetails {
    /// Release-group MBID.
    id: String,
    category: CategoryDto,
    title: String,
    /// The artist credit as printed (e.g. `A & B`).
    artist: String,
    /// The first credited artist's MBID, for the artist view.
    artist_id: Option<String>,
    year: Option<i32>,
    release_date: Option<NaiveDate>,
    release_type: Option<ReleaseTypeDto>,
    secondary_types: Vec<SecondaryTypeDto>,
    /// Tracks of the release the album is counted by.
    track_count: Option<u32>,
    disambiguation: String,
    cover: Option<String>,
    status: AvailabilityDto,
    /// The member has an open request for it.
    requested_by_me: bool,
    /// Requests can be made in the category right now (the music source is
    /// set up completely and the album names its artist).
    requestable: bool,
    /// The journey of the member's open request for it; empty without one.
    journeys: Vec<UnitJourney>,
    /// Available (or partly): where to listen to it (Navidrome, the music
    /// libraries of Jellyfin/Emby with a public URL).
    open_in: Vec<OpenInLink>,
    /// The tracklist of the release the track count is of (spec 0008).
    tracks: Vec<TrackDto>,
    /// Which release that is.
    tracklist_release: Option<TracklistReleaseDto>,
    /// The release's first label.
    label: Option<String>,
    /// `MusicBrainz` genre names, most voted first.
    genres: Vec<String>,
    /// Credits for what is shown (`MusicBrainz` genres, CC BY-NC-SA).
    attributions: Vec<Attribution>,
}

fn discography_album(
    entry: CatalogEntry,
    relevance: &Relevance,
    known: &BTreeMap<String, (Availability, bool)>,
) -> DiscographyAlbum {
    let album = entry.album;
    let (release_type, secondary_types) = album
        .map(|a| (a.primary_type, a.secondary_types))
        .unwrap_or_default();
    let (status, mine) = known
        .get(&entry.item.id)
        .copied()
        .unwrap_or((Availability::New, false));
    DiscographyAlbum {
        in_default_view: relevance.admits_album(release_type, &secondary_types),
        id: entry.item.id,
        title: entry.title,
        year: entry.release_date.map(|d| d.year()),
        release_date: entry.release_date,
        release_type: release_type.map(Into::into),
        secondary_types: secondary_types.into_iter().map(Into::into).collect(),
        cover: file_name(entry.poster),
        status: status.into(),
        requested_by_me: mine,
    }
}

/// An artist and their discography, grouped by the client; albums outside
/// the category's release types are marked for "Show all". Each album has
/// the member's status (available, requested by anyone, new).
#[utoipa::path(get, path = "/api/v1/artists/{id}", tag = "discover",
    params(("id" = String, Path, description = "Artist MBID (lower case), as in `TitleCard.id`")),
    responses(
        (status = 200, body = ArtistDetails),
        (status = 404, body = ErrorBody, description = "`discover.unavailable` (not entitled to music, or Discover not ready), `catalog.not_found`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable` (also while MusicBrainz asks Starr to slow down)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn artist_details(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ArtistDetails>, ApiError> {
    let member_id = session.subject.id;
    let title = title_ref_or_404(MediaKind::Artist, &id)?;
    let catalog = browsable(&state, member_id, Category::Music).await?;
    let details = open_catalog(&state, catalog.instance_id)
        .await?
        .details(&title, LANGUAGE)
        .await
        .map_err(|e| adapter_error(&e))?;
    let mut known: BTreeMap<String, (Availability, bool)> =
        member_title_status(&state, member_id, &title)
            .await?
            .seasons
            .into_iter()
            .filter_map(|u| Some((u.album?.hyphenated().to_string(), (u.status, u.mine))))
            .collect();
    // Albums nobody requested count as available where a music library
    // has them (by release group, spec 0006 Q7 B).
    let discography: Vec<uuid::Uuid> = details
        .children
        .iter()
        .filter_map(|child| match child {
            Child::Album(album) => uuid::Uuid::parse_str(&album.item.id).ok(),
            Child::Season(_) | Child::Scene(_) => None,
        })
        .collect();
    if let TitlePresence::Albums(present) = state
        .db
        .album_presence(Category::Music, &discography)
        .await?
    {
        for (rg, tracks) in present {
            if tracks > 0 {
                known
                    .entry(rg.hyphenated().to_string())
                    .or_insert((Availability::Available, false));
            }
        }
    }
    let albums = details
        .children
        .into_iter()
        .filter_map(|child| match child {
            Child::Album(album) => Some(discography_album(*album, &catalog.relevance, &known)),
            Child::Season(_) | Child::Scene(_) => None,
        })
        .collect();
    let journeys = MemberRequests::load(&state, member_id, None)
        .await?
        .title(super::catalog::MediaKindDto::Artist, &title.id);
    let entry = details.entry;
    let genres: Vec<String> = details.genres.into_iter().map(|g| g.name).collect();
    let attributions = if genres.is_empty() {
        Vec::new()
    } else {
        vec![Attribution::musicbrainz_genres()]
    };
    Ok(Json(ArtistDetails {
        genres,
        attributions,
        id: title.id,
        category: Category::Music.into(),
        name: entry.title,
        disambiguation: entry.overview,
        cover: file_name(entry.poster),
        albums,
        requestable: source_target(&state, Category::Music).await?.is_some(),
        journeys,
    }))
}

/// An album with artist, year, type, track count and the member's status.
#[utoipa::path(get, path = "/api/v1/albums/{id}", tag = "discover",
    params(("id" = String, Path, description = "Release-group MBID (lower case), as in `TitleCard.id`")),
    responses(
        (status = 200, body = AlbumDetails),
        (status = 404, body = ErrorBody, description = "`discover.unavailable` (not entitled to music, or Discover not ready), `catalog.not_found`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable` (also while MusicBrainz asks Starr to slow down)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn album_details(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<AlbumDetails>, ApiError> {
    let member_id = session.subject.id;
    let title = title_ref_or_404(MediaKind::Album, &id)?;
    let catalog = browsable(&state, member_id, Category::Music).await?;
    let details = open_catalog(&state, catalog.instance_id)
        .await?
        .details(&title, LANGUAGE)
        .await
        .map_err(|e| adapter_error(&e))?;
    let tracks = details.entry.album.as_ref().and_then(|a| a.track_count);
    let found = album_status(&state, member_id, &title, tracks).await?;
    let release_group = uuid::Uuid::parse_str(&title.id).ok();
    let journeys = match release_group {
        Some(release_group) => MemberRequests::load(&state, member_id, None)
            .await?
            .album(release_group),
        None => Vec::new(),
    };
    let listenable = matches!(
        found.status,
        Availability::Available | Availability::PartiallyAvailable
    );
    let open_in = match release_group {
        Some(rg) if listenable => state
            .db
            .album_links(&[rg])
            .await?
            .remove(&rg)
            .map(open_in_links)
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let entry = details.entry;
    let album = entry.album.unwrap_or_default();
    let attributions = if album.genres.is_empty() {
        Vec::new()
    } else {
        vec![Attribution::musicbrainz_genres()]
    };
    Ok(Json(AlbumDetails {
        tracks: album.tracks.into_iter().map(Into::into).collect(),
        tracklist_release: album.tracklist_release.map(Into::into),
        label: album.label,
        genres: album.genres,
        attributions,
        requestable: album.artist.is_some()
            && source_target(&state, Category::Music).await?.is_some(),
        id: title.id,
        category: Category::Music.into(),
        title: entry.title,
        artist: album.artist_credit,
        artist_id: album.artist.map(|a| a.hyphenated().to_string()),
        year: entry.release_date.map(|d| d.year()),
        release_date: entry.release_date,
        release_type: album.primary_type.map(Into::into),
        secondary_types: album.secondary_types.into_iter().map(Into::into).collect(),
        track_count: album.track_count,
        disambiguation: entry.overview,
        cover: file_name(entry.poster),
        status: found.status.into(),
        requested_by_me: found.requested_by_me,
        open_in,
        journeys,
    }))
}

/// What a member sees of album `title`: a unit of its artist, from its
/// acquisition, anyone's open requests (never by whom, D10) and the music
/// consume services (by release group, spec 0006 Q7 B). An album nobody
/// requested is new unless a consume service has it (Starr does not read
/// Lidarr from the web process; ADR 0003). `tracks` is the catalog's
/// track count where the caller has it: fewer tracks in the library are
/// "partially available", as when a request for it is checked.
pub(crate) async fn album_status(
    state: &AppState,
    member_id: i64,
    title: &CatalogRef,
    tracks: Option<u32>,
) -> Result<MemberTitleStatus, ApiError> {
    let release_group = uuid::Uuid::parse_str(&title.id).ok();
    let presence = state
        .db
        .album_presence(Category::Music, release_group.as_slice())
        .await?;
    // Not requested: what the library has, by the same rule as a request.
    let unrequested = MemberTitleStatus {
        status: match rules::unit_availability(
            None,
            false,
            presence.unit(None, release_group),
            tracks,
        ) {
            Availability::Requested => Availability::New,
            other => other,
        },
        requested_by_me: false,
        stage: None,
        seasons: Vec::new(),
    };
    let Some(item) = state
        .db
        .resolve_catalog_item(MediaKind::Album, natural_key(title).as_slice())
        .await?
    else {
        return Ok(unrequested);
    };
    let Some(artist) = item.parent_id else {
        return Ok(unrequested);
    };
    let facts = state.db.unit_facts(artist, member_id).await?;
    let Some(fact) = facts.iter().find(|f| f.catalog_item_id == item.id) else {
        return Ok(unrequested);
    };
    Ok(MemberTitleStatus {
        status: availability(fact, &presence),
        requested_by_me: fact.mine,
        stage: unit_stage(fact, &presence),
        seasons: Vec::new(),
    })
}

fn unknown_album() -> ApiError {
    ApiError::Validation("requests.unknown_album")
}

/// The albums a request names, as catalog refs (deduplicated, in order).
fn wanted_albums(body: &CreateRequest) -> Result<Vec<CatalogRef>, ApiError> {
    let kind: MediaKind = body.kind.into();
    match kind {
        MediaKind::Album => {
            if body.albums.is_some() {
                return Err(ApiError::Validation("requests.albums_unexpected"));
            }
            Ok(vec![title_ref_or_404(kind, &body.title_id)?])
        }
        MediaKind::Artist => {
            let ids = body
                .albums
                .as_deref()
                .filter(|a| !a.is_empty())
                .ok_or(ApiError::Validation("requests.albums_required"))?;
            if ids.len() > MAX_ALBUMS {
                return Err(ApiError::Validation("requests.albums_required"));
            }
            let mut seen = BTreeSet::new();
            ids.iter()
                .filter(|id| seen.insert(id.as_str()))
                .map(|id| title_ref(MediaKind::Album, id).ok_or_else(unknown_album))
                .collect()
        }
        MediaKind::Movie
        | MediaKind::Series
        | MediaKind::Scene
        | MediaKind::Performer
        | MediaKind::Studio => Err(ApiError::Validation("requests.albums_unexpected")),
    }
}

/// Read the artist of request `title` (an artist, or an album naming its
/// first credited artist) and the `wanted` albums from the catalog, and
/// record them: the artist, and the albums under it. An album must be the
/// artist's: in its discography as loaded, or crediting it.
async fn record_albums(
    state: &AppState,
    title: &CatalogRef,
    wanted: &[CatalogRef],
) -> Result<(CatalogItem, Vec<CatalogItem>), ApiError> {
    let role = state
        .db
        .default_catalog(Category::Music)
        .await?
        .ok_or(ApiError::Conflict("requests.category_not_ready"))?;
    let catalog = open_catalog(state, role.service_instance_id).await?;
    // The album view's album, read in full (it has the track count).
    let mut read: Vec<CatalogEntry> = Vec::new();
    let artist_ref = match title.kind {
        MediaKind::Album => {
            let album = catalog
                .details(title, LANGUAGE)
                .await
                .map_err(|e| adapter_error(&e))?;
            let artist = album
                .entry
                .album
                .as_ref()
                .and_then(|a| a.artist)
                .ok_or_else(unknown_album)?;
            read.push(album.entry);
            CatalogRef {
                kind: MediaKind::Artist,
                id: artist.hyphenated().to_string(),
            }
        }
        _ => title.clone(),
    };
    let artist_mbid = uuid::Uuid::parse_str(&artist_ref.id).map_err(|_| unknown_album())?;
    let artist = catalog
        .details(&artist_ref, LANGUAGE)
        .await
        .map_err(|e| adapter_error(&e))?;
    let mut entries = Vec::with_capacity(wanted.len());
    for want in wanted {
        let known = read
            .iter()
            .find(|e| e.item.id == want.id)
            .cloned()
            .or_else(|| {
                artist.children.iter().find_map(|child| match child {
                    Child::Album(album) if album.item.id == want.id => Some((**album).clone()),
                    _ => None,
                })
            });
        if let Some(entry) = known {
            entries.push(entry);
            continue;
        }
        // Beyond the discography as loaded (at most 300): the album itself
        // must credit the artist.
        let album = match catalog.details(want, LANGUAGE).await {
            Ok(album) => album,
            Err(AdapterError::NotFound(_)) => return Err(unknown_album()),
            Err(err) => return Err(adapter_error(&err)),
        };
        if !album
            .external_ids
            .contains(&ExternalId::MbArtist(artist_mbid))
        {
            return Err(unknown_album());
        }
        entries.push(album.entry);
    }
    let now = state.clock.now();
    let provenance = Provenance {
        adapter_type: &role.adapter_type,
        instance_id: role.service_instance_id,
        fetched_at: now,
        expires_at: now + METADATA_TTL,
    };
    let item = state
        .db
        .upsert_catalog_item(&artist, &provenance, now)
        .await?;
    let refs: Vec<&CatalogEntry> = entries.iter().collect();
    let albums = state
        .db
        .upsert_albums(item.id, &refs, &provenance, now)
        .await?;
    Ok((item, albums))
}

/// Request albums (spec 0006 slice 4): one request of the artist with one
/// item per album, admitted per album by the member's mode, an earlier
/// refusal and a shared acquisition like seasons (C2, D10). Albums that
/// are available or already requested by the member are skipped.
pub(crate) async fn create_album_request(
    state: &AppState,
    session: &Session,
    body: CreateRequest,
    mode: RequestMode,
) -> Result<CreatedRequest, ApiError> {
    if body.seasons.is_some() {
        return Err(ApiError::Validation("requests.seasons_unexpected"));
    }
    let kind: MediaKind = body.kind.into();
    let title = title_ref_or_404(kind, &body.title_id)?;
    let wanted = wanted_albums(&body)?;
    let source = ready_source(state, Category::Music, kind).await?;
    let (artist, albums) = record_albums(state, &title, &wanted).await?;
    let member_id = session.subject.id;
    let units: Vec<Unit> = albums
        .iter()
        .map(|a| Unit {
            catalog_item_id: a.id,
            season_number: None,
        })
        .collect();
    // An album credited to two artists is stored once, under the artist it
    // was first requested through: its facts are that artist's.
    let parents: BTreeSet<i64> = albums
        .iter()
        .map(|a| a.parent_id.unwrap_or(artist.id))
        .collect();
    let mut facts = Vec::new();
    for parent in parents {
        facts.extend(state.db.unit_facts(parent, member_id).await?);
    }
    let release_groups: Vec<uuid::Uuid> = facts.iter().filter_map(|f| f.release_group).collect();
    let presence = state
        .db
        .album_presence(Category::Music, &release_groups)
        .await?;
    let (units, skipped) = split_known(&facts, &presence, units);
    if units.is_empty() {
        if body.follow {
            // Nothing left to request, but "follow the artist" holds.
            follow_artist(state, session, &artist).await?;
        }
        let all_available = skipped.iter().all(|s| s.reason == SkipReason::Available);
        return Err(ApiError::Conflict(if all_available {
            "requests.already_available"
        } else {
            "requests.already_requested"
        }));
    }
    let actor = starr_db::requests::Actor {
        trigger: starr_db::requests::EventTrigger::Member,
        name: &session.subject.username,
    };
    let created = state
        .db
        .create_request(
            &NewRequest {
                member_id,
                catalog_item_id: artist.id,
                // Albums first released after today come by the follow.
                follow: if body.follow {
                    RequestFollow::Start {
                        cutoff: artist_cutoff(state),
                    }
                } else {
                    RequestFollow::No
                },
                units: &units,
                mode,
                source: &source,
            },
            actor,
            state.clock.now(),
        )
        .await
        .map_err(|err| match err {
            starr_db::DbError::RequestOpen => ApiError::Conflict("requests.already_requested"),
            starr_db::DbError::AlreadyAvailable => ApiError::Conflict("requests.already_available"),
            other => other.into(),
        })?;
    let requested: Vec<&str> = albums
        .iter()
        .filter(|a| units.iter().any(|u| u.catalog_item_id == a.id))
        .map(|a| a.title.as_str())
        .collect();
    audit_session(
        state,
        session,
        "request.create",
        Some(&artist.title),
        Some(json!({
            "requestId": created.request_id,
            "albums": requested,
            "state": created.state.as_str(),
        })),
    )
    .await?;
    if created.submit {
        enqueue_submit(state).await?;
    }
    notifications::enqueue_due(state).await?;
    let request = my_request(state, member_id, created.request_id).await?;
    Ok(CreatedRequest { request, skipped })
}
