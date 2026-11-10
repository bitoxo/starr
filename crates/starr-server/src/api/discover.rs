//! Discover, search and title details for members (spec 0003 slice 5,
//! user stories 1–3 and 5).
//!
//! Only categories the member is entitled to appear, and only those that
//! are switched on with a saved relevance on their default catalog can be
//! browsed (D3). Lists use that relevance; search does not (D3, D12).
//! Every title carries the member's view of its status: available,
//! requested (by anyone, never by whom, D10) or new. Images are file
//! names for Starr's proxy (O4), and "open in" links point at the consume
//! services' public URLs (O3).
//!
//! Catalog calls run here with the web key (ADR 0003 amendment 1) through
//! the cached catalog adapter of each category's default catalog: TMDB for
//! movies and series, `MusicBrainz` for music (spec 0006 slice 2). Music has
//! no request flow yet, so its titles always show as new; its details live
//! in `music.rs`.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use starr_core::adapters::AdapterDescriptor;
use starr_core::catalog::{
    Catalog, CatalogEntry, CatalogRef, Child, GenreRef, ListFilter, ListKind, ListQuery, MediaKind,
    Page, Relevance, SearchQuery, Trailer,
};
use starr_core::categories::{catalog_kinds, is_private};
use starr_core::members::Category;
use starr_core::requests::DownloadStage;
use starr_worker::catalog::AnyCatalog;
use utoipa::{IntoParams, ToSchema};

use super::catalog::{
    MediaKindDto, ProviderDto, ReleaseTypeDto, adapter_error, file_name, open_catalog,
};
use super::common::{CategoryDto, OpenInLink, open_in_links};
use super::error::{ApiError, ErrorBody};
use super::journey::{MemberRequests, UnitJourney};
use super::requests::{
    AvailabilityDto, DownloadStageDto, MemberTitleStatus, Record, arriving_in, category_of,
    member_title_status, record_title, source_target, title_ref, title_ref_or_404,
};
use super::title_facts::{
    Attribution, CARD_GENRES, CastDto, CertificationDto, CrewDto, EpisodeDto, GenreNames,
    NextEpisodeDto, TitleExtras, WatchProviderDto, certification, digital_release, genre_names,
    provider_region, release_region, short_overview,
};
use crate::AppState;
use crate::auth::MemberSession;

/// Language of titles and overviews (D5: English in v1; the adapter falls
/// back to English for empty fields).
const LANGUAGE: &str = "en-US";
/// Longest search text accepted.
const SEARCH_MAX_CHARS: usize = 200;

/// A browse list (D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = DiscoverList)]
pub enum ListKindDto {
    Trending,
    Popular,
    Upcoming,
    OnServices,
    TopRated,
    FreshReleases,
}

impl ListKindDto {
    /// The API name of `list`; `None` for the adult lane `new`, which has
    /// no API before the adult catalog (spec 0007 slice 2).
    const fn of(list: ListKind) -> Option<Self> {
        match list {
            ListKind::Trending => Some(Self::Trending),
            ListKind::Popular => Some(Self::Popular),
            ListKind::Upcoming => Some(Self::Upcoming),
            ListKind::OnServices => Some(Self::OnServices),
            ListKind::TopRated => Some(Self::TopRated),
            ListKind::FreshReleases => Some(Self::FreshReleases),
            ListKind::New | ListKind::OnTheAir => None,
        }
    }
}

/// The kind a category's Discover lanes show: albums for music (artists
/// are found by search and through their albums).
pub(crate) const fn lane_kind(category: Category) -> Option<MediaKind> {
    match category {
        Category::Movies => Some(MediaKind::Movie),
        Category::Series => Some(MediaKind::Series),
        Category::Music => Some(MediaKind::Album),
        Category::Adult => None,
    }
}

/// A category the member may request in.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverCategory {
    category: CategoryDto,
    kind: MediaKindDto,
    /// Switched on with a saved relevance (D3); otherwise nothing can be
    /// browsed or searched here yet.
    discover_ready: bool,
    /// The category's source is set up, so requests can be made.
    requestable: bool,
    /// The lists that can show something with this relevance, in display
    /// order ("On your services" needs providers).
    lists: Vec<ListKindDto>,
    /// The provider switcher of the category page (spec 0008 Q13 B): the
    /// relevance's providers first, then the region's next ones in its
    /// display order, at most 12; empty without a provider region or for
    /// catalogs without providers.
    providers: Vec<ProviderDto>,
    /// Genre tiles and the grid's genre filter: TMDB genres of the kind
    /// without the excluded ones; for music the fixed list of broad
    /// `MusicBrainz` genres (F-8).
    genres: Vec<DiscoverGenre>,
    /// Credits for what is shown here (`JustWatch` for providers,
    /// `MusicBrainz` for genres with their licence).
    attributions: Vec<Attribution>,
}

/// A genre to browse by: `id` is the `genre` parameter of a list.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverGenre {
    /// TMDB: the numeric genre id; music: the `MusicBrainz` genre name.
    id: String,
    name: String,
}

/// One title of the Discover hero (spec 0008 A4): a card with a backdrop
/// and its rank in its category's Trending list.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Highlight {
    /// 1-based rank in the category's Trending list (after relevance).
    rank: u32,
    category: CategoryDto,
    #[serde(flatten)]
    title: TitleCard,
}

/// A title in a list or search result, with the member's view of it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TitleCard {
    kind: MediaKindDto,
    /// The title's id in the category's catalog (TMDB for movies and
    /// series); with `kind` it addresses the title everywhere.
    id: String,
    title: String,
    year: Option<i32>,
    /// Image reference for the proxy: a TMDB file for
    /// `/api/v1/images/tmdb/{size}/{ref}` (movies, series) or a release
    /// group MBID for `/api/v1/images/caa/{size}/{ref}` (albums). Albums
    /// without a cover answer 404 there.
    poster: Option<String>,
    /// Albums: the artist credit (e.g. `A & B`).
    artist: Option<String>,
    /// Albums: album, EP, single, ...
    release_type: Option<ReleaseTypeDto>,
    status: AvailabilityDto,
    /// The member has an open request for (part of) it.
    requested_by_me: bool,
    /// The journeys of the member's open requests for it, per unit (movie,
    /// or season); empty without one.
    journeys: Vec<UnitJourney>,
    /// Backdrop file for the image proxy (movies, series).
    backdrop: Option<String>,
    /// At most 200 characters, cut at a word.
    overview: Option<String>,
    /// Genre names, at most three.
    genres: Vec<String>,
    /// Catalog rating 0–10.
    rating: Option<f32>,
    /// The exact release (movies) or first air date (series), e.g. for
    /// Upcoming.
    release_date: Option<NaiveDate>,
}

/// One page of titles; `next` is the page to ask for next, absent at the
/// end.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TitlePage {
    items: Vec<TitleCard>,
    next: Option<u32>,
}

/// A season with the member's view of it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeasonDetails {
    number: u32,
    name: String,
    episode_count: u32,
    air_date: Option<NaiveDate>,
    /// Season 0 (specials).
    special: bool,
    status: AvailabilityDto,
    requested_by_me: bool,
    /// Requested and downloading: importing, or arriving in the media
    /// server (the title's `arrivingIn` names it).
    stage: Option<DownloadStageDto>,
}

/// Everything the detail view shows (D13).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TitleDetails {
    kind: MediaKindDto,
    /// The title's id in the category's catalog.
    id: String,
    category: CategoryDto,
    title: String,
    original_title: String,
    year: Option<i32>,
    overview: String,
    poster: Option<String>,
    /// Backdrop file for the image proxy.
    backdrop: Option<String>,
    genres: Vec<String>,
    /// TMDB rating 0–10.
    rating: Option<f32>,
    vote_count: u64,
    /// The first `YouTube` trailer, as a link to open there.
    trailer_url: Option<String>,
    status: AvailabilityDto,
    requested_by_me: bool,
    /// Movies: requested and downloading, importing or arriving in the
    /// media server. Series carry it per season.
    stage: Option<DownloadStageDto>,
    /// A movie or season arriving: the media server(s) it will show up in.
    arriving_in: Option<String>,
    /// The journeys of the member's open requests for it, per unit (movie,
    /// or season; a season's chip shows its unit); empty without one.
    journeys: Vec<UnitJourney>,
    /// Series only, specials included.
    seasons: Vec<SeasonDetails>,
    open_in: Vec<OpenInLink>,
    /// Requests can be made in the category right now.
    requestable: bool,
    /// Movies: primary release; series: first air date.
    release_date: Option<NaiveDate>,
    /// Movies: the digital release in the category's release region,
    /// else the earliest one.
    digital_release_date: Option<NaiveDate>,
    /// Movies: running time; series: a typical episode's, if TMDB lists
    /// one.
    runtime_minutes: Option<u32>,
    /// The age rating in the category's release region (else its provider
    /// region; spec 0008 Q12 A).
    certification: Option<CertificationDto>,
    tagline: Option<String>,
    /// Series: TMDB's status (`Returning Series`, `Ended`, `Canceled`, …).
    series_status: Option<String>,
    /// Series: seasons in all.
    season_count: Option<u32>,
    next_episode: Option<NextEpisodeDto>,
    networks: Vec<String>,
    /// Top 12 billed (spec 0008 Q15 A).
    cast: Vec<CastDto>,
    /// Directors or creators, then writers.
    crew: Vec<CrewDto>,
    /// Offers in the category's provider region, in TMDB's order.
    watch_providers: Vec<WatchProviderDto>,
    details: TitleExtras,
    /// Credits for what is shown (`JustWatch` with providers).
    attributions: Vec<Attribution>,
}

/// The episodes of one season.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeasonEpisodes {
    number: u32,
    name: String,
    overview: String,
    air_date: Option<NaiveDate>,
    episodes: Vec<EpisodeDto>,
}

/// How a "See all" grid is sorted (spec 0008 Q6 B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = DiscoverSort)]
pub enum SortDto {
    Popularity,
    /// Vote average, with the minimum votes of "Top rated".
    Rating,
    /// Newest first, released up to today.
    ReleaseDate,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ListParams {
    /// 1-based page from `next` (default 1).
    page: Option<u32>,
    /// A watch provider id (`DiscoverCategory.providers`): the list of
    /// `on_provider`, or a filter of the discover lists ("On \<provider\>"
    /// in the provider region; TMDB catalogs only, not on `trending` or
    /// `on_the_air`: 422 `validation.provider`).
    provider: Option<u64>,
    /// A genre id (`DiscoverCategory.genres`): the list of `genre`, or a
    /// filter. Music: the genre lanes (`list=genre`) only.
    genre: Option<String>,
    /// `popularity`, `rating` or `release_date`: lists `popular`,
    /// `top_rated`, `on_provider` and `genre` of TMDB catalogs only.
    #[param(inline)]
    sort: Option<SortDto>,
    /// Leave out what is available to the member (a page may then hold
    /// fewer titles; `next` stays).
    #[serde(default)]
    hide_owned: bool,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HighlightsQuery {
    /// One category's top 10 (`movies`, `series`); absent: movies and
    /// series mixed.
    category: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchParams {
    /// What to look for (at most 200 characters).
    query: String,
    /// 1-based page from `next` (default 1).
    page: Option<u32>,
}

/// A category's default catalog when Discover is ready there: the
/// instance, its saved relevance and its adapter's descriptor (which lists
/// it serves, ADR 0013).
pub(crate) struct BrowsableCatalog {
    pub(crate) instance_id: i64,
    pub(crate) relevance: Relevance,
    pub(crate) descriptor: &'static AdapterDescriptor,
}

/// A category as the member sees it.
struct MemberCategory {
    category: Category,
    kind: MediaKindDto,
    catalog: Option<BrowsableCatalog>,
}

/// The categories with a catalog `member_id` is entitled to, in display
/// order. Never the private category: global Discover and search do not
/// touch its catalog at all, so its latency or errors cannot be observed
/// outside its own area (spec 0007 Q11 A).
async fn member_categories(
    state: &AppState,
    member_id: i64,
) -> Result<Vec<MemberCategory>, ApiError> {
    let entitled = state.db.entitlements(member_id).await?;
    let enabled = state.db.list_categories().await?;
    let mut out = Vec::new();
    for category in Category::ALL {
        if is_private(category) {
            continue;
        }
        let Some(kind) = lane_kind(category).and_then(MediaKindDto::of) else {
            continue;
        };
        if !entitled.iter().any(|e| e.category == category && e.enabled) {
            continue;
        }
        let switched_on = enabled.iter().any(|c| c.category == category && c.enabled);
        let catalog = match state.db.default_catalog(category).await? {
            Some(role) if switched_on => {
                // An adapter this build does not know (rolled back after a
                // newer one) cannot be browsed.
                let descriptor = starr_worker::adapters::descriptor(&role.adapter_type);
                match (role.relevance()?, descriptor) {
                    (Some(relevance), Some(descriptor)) => Some(BrowsableCatalog {
                        instance_id: role.service_instance_id,
                        relevance,
                        descriptor,
                    }),
                    _ => None,
                }
            }
            _ => None,
        };
        out.push(MemberCategory {
            category,
            kind,
            catalog,
        });
    }
    Ok(out)
}

/// The default catalog of `category`, if the member may browse it.
pub(crate) async fn browsable(
    state: &AppState,
    member_id: i64,
    category: Category,
) -> Result<BrowsableCatalog, ApiError> {
    member_categories(state, member_id)
        .await?
        .into_iter()
        .find(|c| c.category == category)
        .and_then(|c| c.catalog)
        .ok_or(ApiError::NotFound("discover.unavailable"))
}

/// The lists a catalog serves that can show something with this relevance,
/// in the catalog's order ("On your services" needs providers).
fn lists_for(catalog: &BrowsableCatalog) -> Vec<ListKindDto> {
    let relevance = &catalog.relevance;
    catalog
        .descriptor
        .lists
        .iter()
        .copied()
        .filter(|l| {
            *l != ListKind::OnServices
                || (relevance.provider_region.is_some() && !relevance.providers.is_empty())
        })
        .filter_map(ListKindDto::of)
        .collect()
}

/// Pages the catalog serves (TMDB stops at 500).
const MAX_PAGE: u32 = 500;

fn page_of(page: Option<u32>) -> Result<u32, ApiError> {
    match page.unwrap_or(1) {
        n @ 1..=MAX_PAGE => Ok(n),
        _ => Err(ApiError::Validation("validation.page")),
    }
}

fn parse_list(catalog: &BrowsableCatalog, raw: &str) -> Result<ListKind, ApiError> {
    catalog
        .descriptor
        .lists
        .iter()
        .copied()
        .find(|l| l.as_str() == raw)
        .ok_or(ApiError::NotFound("discover.unknown_list"))
}

/// Cards for catalog entries, each with the member's status; genre names
/// from `genres`.
async fn cards(
    state: &AppState,
    member_id: i64,
    entries: Vec<CatalogEntry>,
    genres: &GenreNames,
) -> Result<Vec<TitleCard>, ApiError> {
    let mine = MemberRequests::load(state, member_id, None).await?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        if let Some(card) = card(state, member_id, &mine, entry, genres).await? {
            out.push(card);
        }
    }
    Ok(out)
}

/// The card of one entry; `None` for kinds and ids without an API.
async fn card(
    state: &AppState,
    member_id: i64,
    mine: &MemberRequests,
    entry: CatalogEntry,
    genres: &GenreNames,
) -> Result<Option<TitleCard>, ApiError> {
    let Some(title) = title_ref(entry.item.kind, &entry.item.id) else {
        return Ok(None);
    };
    let Some(kind) = MediaKindDto::of(title.kind) else {
        return Ok(None);
    };
    let (artist, release_type) = match entry.album {
        Some(album) => (
            Some(album.artist_credit),
            album.primary_type.map(ReleaseTypeDto::from),
        ),
        None => (None, None),
    };
    let found = member_title_status(state, member_id, &title).await?;
    // An album's journey is that of its item in the artist's request.
    let journeys = match uuid::Uuid::parse_str(&title.id) {
        Ok(release_group) if title.kind == MediaKind::Album => mine.album(release_group),
        _ => mine.title(kind, &title.id),
    };
    let genre_names = entry
        .genre_ids
        .iter()
        .filter_map(|id| genres.get(&(title.kind, *id)).cloned())
        .take(CARD_GENRES)
        .collect();
    Ok(Some(TitleCard {
        kind,
        journeys,
        id: title.id,
        title: entry.title,
        year: entry.release_date.map(|d| d.year()),
        poster: file_name(entry.poster),
        artist,
        release_type,
        status: found.status.into(),
        requested_by_me: found.requested_by_me,
        backdrop: file_name(entry.backdrop),
        overview: short_overview(&entry.overview),
        genres: genre_names,
        rating: entry.rating,
        release_date: entry.release_date,
    }))
}

/// The category page data of a browsable category: its provider
/// switcher, its genres and their credits. A catalog that cannot answer
/// leaves them empty (Discover itself still works).
async fn page_data(
    state: &AppState,
    category: Category,
    catalog: &BrowsableCatalog,
) -> (Vec<ProviderDto>, Vec<DiscoverGenre>, Vec<Attribution>) {
    let mut attributions = Vec::new();
    if catalog.descriptor.type_id == starr_adapter_musicbrainz::DESCRIPTOR.type_id {
        attributions.push(Attribution::musicbrainz_genres());
        let genres = starr_adapter_musicbrainz::GENRES
            .iter()
            .map(|(id, name)| DiscoverGenre {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
            })
            .collect();
        return (Vec::new(), genres, attributions);
    }
    let Some(kind) = lane_kind(category) else {
        return (Vec::new(), Vec::new(), attributions);
    };
    let Ok(opened) = open_catalog(state, catalog.instance_id).await else {
        return (Vec::new(), Vec::new(), attributions);
    };
    let relevance = &catalog.relevance;
    // Both cached for a day at least; asked side by side.
    let region_providers = async {
        match provider_region(relevance) {
            Some(region) => Some(opened.providers(kind, region).await),
            None => None,
        }
    };
    let (config, region_providers) = tokio::join!(opened.configuration(), region_providers);
    let genres = match config {
        Ok(config) => config
            .genres
            .into_iter()
            .filter(|g| g.kind == kind && !relevance.excluded_genres.contains(&g.id))
            .map(|g| DiscoverGenre {
                id: g.id.to_string(),
                name: g.name,
            })
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err, "catalog genres unavailable");
            Vec::new()
        }
    };
    let providers = match region_providers {
        Some(Ok(all)) => switcher(relevance, all),
        Some(Err(err)) => {
            tracing::warn!(error = %err, "catalog providers unavailable");
            Vec::new()
        }
        None => Vec::new(),
    };
    if !providers.is_empty() {
        attributions.push(Attribution::justwatch());
    }
    (providers, genres, attributions)
}

/// Providers in the switcher (spec 0008 Q13 B).
const SWITCHER_PROVIDERS: usize = 12;

/// The relevance's providers first (in their order), then the region's
/// others in its display order, at most [`SWITCHER_PROVIDERS`].
fn switcher(
    relevance: &Relevance,
    region: Vec<starr_core::catalog::WatchProvider>,
) -> Vec<ProviderDto> {
    let (mut configured, rest): (Vec<_>, Vec<_>) = region
        .into_iter()
        .partition(|p| relevance.providers.contains(&p.id));
    configured.sort_by_key(|p| relevance.providers.iter().position(|id| *id == p.id));
    configured
        .into_iter()
        .chain(rest)
        .take(SWITCHER_PROVIDERS)
        .map(|p| ProviderDto {
            id: p.id,
            name: p.name,
            logo: file_name(p.logo),
        })
        .collect()
}

/// The member's categories and whether each can be browsed (story 1).
#[utoipa::path(get, path = "/api/v1/discover", tag = "discover",
    responses(
        (status = 200, body = Vec<DiscoverCategory>, description = "Empty when the member is entitled to nothing"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn discover_categories(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<DiscoverCategory>>, ApiError> {
    let mut out = Vec::new();
    for c in member_categories(&state, session.subject.id).await? {
        let requestable = c.catalog.is_some() && source_target(&state, c.category).await?.is_some();
        let (providers, genres, attributions) = match &c.catalog {
            Some(catalog) => page_data(&state, c.category, catalog).await,
            None => (Vec::new(), Vec::new(), Vec::new()),
        };
        out.push(DiscoverCategory {
            category: c.category.into(),
            kind: c.kind,
            discover_ready: c.catalog.is_some(),
            requestable,
            lists: c.catalog.as_ref().map(lists_for).unwrap_or_default(),
            providers,
            genres,
            attributions,
        });
    }
    Ok(Json(out))
}

/// One page of a browse list of a category, filtered by its relevance
/// (D3, D4). Endless scrolling follows `next`.
#[utoipa::path(get, path = "/api/v1/discover/{category}/{list}", tag = "discover",
    params(
        ("category" = String, Path, description = "Category (`movies`, `series`, `music`)"),
        ("list" = String, Path, description = "A lane of the category (`DiscoverCategory.lists`): TMDB `trending`, `popular`, `upcoming`, `on_services`, `top_rated`; MusicBrainz `trending`, `fresh_releases`, `popular`. Category page lists (spec 0008): `on_provider` (needs `provider`), `genre` (needs `genre`; music: the genre lanes, one page, refreshed in the background), and for series `on_the_air`"),
        ListParams,
    ),
    responses(
        (status = 200, body = TitlePage),
        (status = 404, body = ErrorBody, description = "`discover.unavailable` (not entitled, or Discover not ready), `discover.unknown_list`"),
        (status = 422, body = ErrorBody, description = "`validation.page` (1 to 500), `validation.provider` (missing for `on_provider`, or on `trending`/`on_the_air` or a non-TMDB catalog), `validation.genre`, `validation.sort`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn discover_list(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path((category, list)): Path<(String, String)>,
    Query(params): Query<ListParams>,
) -> Result<Json<TitlePage>, ApiError> {
    let category = Category::parse(&category).ok_or(ApiError::NotFound("discover.unavailable"))?;
    let page = page_of(params.page)?;
    let member_id = session.subject.id;
    let browsable = browsable(&state, member_id, category).await?;
    let kind = lane_kind(category).ok_or(ApiError::NotFound("discover.unavailable"))?;
    let plan = plan_list(&browsable, kind, &list, &params)?;
    let catalog = open_catalog(&state, browsable.instance_id).await?;
    let found = match &plan.music_genre {
        // Genre lanes are read from the cache the worker fills; the web
        // process never spends its MusicBrainz budget on them.
        Some(genre) => match &catalog {
            AnyCatalog::MusicBrainz(mb) if page == 1 => {
                mb.cached_genre_lane(genre).await.unwrap_or_else(Page::end)
            }
            _ => Page::end(),
        },
        None => catalog
            .list(&ListQuery {
                kind,
                list: plan.list,
                page,
                language: LANGUAGE,
                relevance: &browsable.relevance,
                filter: plan.filter,
            })
            .await
            .map_err(|e| adapter_error(&e))?,
    };
    let mut entries = found.items;
    if let Some(genre) = plan.genre_after {
        entries.retain(|e| e.genre_ids.contains(&genre));
    }
    let names = genre_names(&catalog).await;
    let mut items = cards(&state, member_id, entries, &names).await?;
    if params.hide_owned {
        items.retain(|c| !matches!(c.status, AvailabilityDto::Available));
    }
    Ok(Json(TitlePage {
        items,
        next: found.next,
    }))
}

/// What a list request asks the catalog for.
#[derive(Debug, PartialEq)]
struct ListPlan {
    list: ListKind,
    filter: Option<ListFilter>,
    /// A TMDB genre applied to the page here (with a provider filter,
    /// which takes the catalog's one filter).
    genre_after: Option<u64>,
    /// A music genre lane, read from the cache.
    music_genre: Option<String>,
}

/// Validate a list request against the category's catalog and turn its
/// list name, filters and sort into the catalog's list and filter.
fn plan_list(
    catalog: &BrowsableCatalog,
    kind: MediaKind,
    raw: &str,
    params: &ListParams,
) -> Result<ListPlan, ApiError> {
    let is_tmdb = catalog.descriptor.type_id == starr_adapter_tmdb::DESCRIPTOR.type_id;
    let is_music = catalog.descriptor.type_id == starr_adapter_musicbrainz::DESCRIPTOR.type_id;
    let unknown = || ApiError::NotFound("discover.unknown_list");
    if params.provider.is_some() && !is_tmdb {
        return Err(ApiError::Validation("validation.provider"));
    }
    let genre = match params.genre.as_deref() {
        None => None,
        Some(raw_genre) if is_tmdb => Some(GenreRef::Id(
            raw_genre
                .parse()
                .map_err(|_| ApiError::Validation("validation.genre"))?,
        )),
        Some(raw_genre)
            if is_music
                && starr_adapter_musicbrainz::GENRES
                    .iter()
                    .any(|(id, _)| *id == raw_genre) =>
        {
            Some(GenreRef::Name(raw_genre.to_owned()))
        }
        Some(_) => return Err(ApiError::Validation("validation.genre")),
    };
    let base = match raw {
        "on_provider" if is_tmdb => {
            if params.provider.is_none() {
                return Err(ApiError::Validation("validation.provider"));
            }
            ListKind::Popular
        }
        "genre" if is_tmdb || is_music => {
            if genre.is_none() {
                return Err(ApiError::Validation("validation.genre"));
            }
            ListKind::Popular
        }
        "on_the_air" if is_tmdb && kind == MediaKind::Series => ListKind::OnTheAir,
        "on_provider" | "genre" | "on_the_air" => return Err(unknown()),
        other => {
            let list = parse_list(catalog, other)?;
            if !ListKindDto::of(list).is_some_and(|l| lists_for(catalog).contains(&l)) {
                return Err(unknown());
            }
            list
        }
    };
    // Trending and "On the air" are TMDB's own lists without a provider
    // filter: refused rather than answered with an empty page.
    if params.provider.is_some() && matches!(base, ListKind::Trending | ListKind::OnTheAir) {
        return Err(ApiError::Validation("validation.provider"));
    }
    if is_music {
        // Music genres exist as lanes only.
        return match genre {
            Some(GenreRef::Name(name)) if raw == "genre" => Ok(ListPlan {
                list: base,
                filter: None,
                genre_after: None,
                music_genre: Some(name),
            }),
            Some(_) => Err(ApiError::Validation("validation.genre")),
            None if params.sort.is_some() => Err(ApiError::Validation("validation.sort")),
            None => Ok(ListPlan {
                list: base,
                filter: None,
                genre_after: None,
                music_genre: None,
            }),
        };
    }
    let list = match params.sort {
        None => base,
        Some(sort) if is_tmdb && matches!(base, ListKind::Popular | ListKind::TopRated) => {
            match sort {
                SortDto::Popularity => ListKind::Popular,
                SortDto::Rating => ListKind::TopRated,
                SortDto::ReleaseDate => ListKind::New,
            }
        }
        Some(_) => return Err(ApiError::Validation("validation.sort")),
    };
    let (filter, genre_after) = match (params.provider, genre) {
        (Some(provider), Some(GenreRef::Id(genre))) => {
            (Some(ListFilter::Provider(provider)), Some(genre))
        }
        (Some(provider), _) => (Some(ListFilter::Provider(provider)), None),
        (None, Some(genre)) => (Some(ListFilter::Genre(genre)), None),
        (None, None) => (None, None),
    };
    Ok(ListPlan {
        list,
        filter,
        genre_after,
        music_genre: None,
    })
}

/// The Discover hero (spec 0008 A4, Q2 A): up to ten titles with a
/// backdrop from the Trending lists of the member's movie and series
/// categories, interleaved by rank (or one category's top 10). Music is
/// never in it (no backdrops); composed from the cached Trending lists.
#[utoipa::path(get, path = "/api/v1/discover/highlights", tag = "discover",
    params(HighlightsQuery),
    responses(
        (status = 200, body = Vec<Highlight>, description = "Empty without a browsable movie or series category, and for `music`"),
        (status = 404, body = ErrorBody, description = "`discover.unavailable` (`category` not entitled, unknown, or Discover not ready there)"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn discover_highlights(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Query(query): Query<HighlightsQuery>,
) -> Result<Json<Vec<Highlight>>, ApiError> {
    let member_id = session.subject.id;
    let wanted = match query.category.as_deref() {
        Some(raw) => {
            let category =
                Category::parse(raw).ok_or(ApiError::NotFound("discover.unavailable"))?;
            browsable(&state, member_id, category).await?;
            vec![category]
        }
        None => vec![Category::Movies, Category::Series],
    };
    // (rank, lane, category, entry) of every candidate, and each lane's
    // genre names.
    let mut ranked: Vec<(u32, usize, Category, CatalogEntry)> = Vec::new();
    let mut names: Vec<GenreNames> = Vec::new();
    let mut failure = None;
    for c in member_categories(&state, member_id).await? {
        let Some(catalog) = c.catalog else { continue };
        let Some(kind) = lane_kind(c.category).filter(|k| {
            wanted.contains(&c.category) && matches!(k, MediaKind::Movie | MediaKind::Series)
        }) else {
            continue;
        };
        // Mixed, one failing catalog leaves its titles out; the hero fails
        // only when every catalog failed (as search).
        let (opened, trending) = match trending_of(&state, &catalog, kind).await {
            Ok(found) => found,
            Err(err) if query.category.is_none() => {
                failure.get_or_insert(err);
                continue;
            }
            Err(err) => return Err(err),
        };
        let lane = names.len();
        ranked.extend(
            (1..)
                .zip(trending.items)
                .filter(|(_, e)| e.backdrop.is_some())
                .map(|(rank, e)| (rank, lane, c.category, e)),
        );
        names.push(genre_names(&opened).await);
    }
    if names.is_empty()
        && let Some(err) = failure
    {
        return Err(err);
    }
    interleave(&mut ranked);
    let no_names = GenreNames::new();
    let mine = MemberRequests::load(&state, member_id, None).await?;
    let mut out = Vec::new();
    for (rank, lane, category, entry) in ranked {
        if out.len() == HIGHLIGHTS {
            break;
        }
        if let Some(title) = card(
            &state,
            member_id,
            &mine,
            entry,
            names.get(lane).unwrap_or(&no_names),
        )
        .await?
        {
            out.push(Highlight {
                rank,
                category: category.into(),
                title,
            });
        }
    }
    Ok(Json(out))
}

/// Titles in the hero.
const HIGHLIGHTS: usize = 10;

/// The first Trending page of a category's catalog, with the opened
/// catalog.
async fn trending_of(
    state: &AppState,
    catalog: &BrowsableCatalog,
    kind: MediaKind,
) -> Result<(AnyCatalog, Page<CatalogEntry>), ApiError> {
    let opened = open_catalog(state, catalog.instance_id).await?;
    let trending = opened
        .list(&ListQuery {
            kind,
            list: ListKind::Trending,
            page: 1,
            language: LANGUAGE,
            relevance: &catalog.relevance,
            filter: None,
        })
        .await
        .map_err(|e| adapter_error(&e))?;
    Ok((opened, trending))
}

/// Candidates of several lanes merged by rank: every lane's first, then
/// every lane's second, … (lanes in their order on equal rank).
fn interleave<T>(ranked: &mut [(u32, usize, Category, T)]) {
    ranked.sort_by_key(|(rank, lane, _, _)| (*rank, *lane));
}

/// Search the member's browsable categories: movies and series (TMDB),
/// artists and albums (`MusicBrainz`, spec 0006 Q11); the relevance does
/// not apply and people are left out (D3, D12).
#[utoipa::path(get, path = "/api/v1/search", tag = "discover",
    params(SearchParams),
    responses(
        (status = 200, body = TitlePage, description = "Empty for blank text or without browsable categories"),
        (status = 422, body = ErrorBody, description = "`validation.page` (1 to 500), `validation.search_query`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn search(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<TitlePage>, ApiError> {
    let page = page_of(params.page)?;
    let text = params.query.trim();
    if text.chars().count() > SEARCH_MAX_CHARS {
        return Err(ApiError::Validation("validation.search_query"));
    }
    if text.is_empty() {
        return Ok(Json(TitlePage {
            items: Vec::new(),
            next: None,
        }));
    }
    let member_id = session.subject.id;
    let mut found = Page::end();
    // One search per catalog over the kinds it is the default for.
    let mut by_catalog: BTreeMap<i64, Vec<MediaKind>> = BTreeMap::new();
    for c in member_categories(&state, member_id).await? {
        if let Some(catalog) = c.catalog {
            by_catalog
                .entry(catalog.instance_id)
                .or_default()
                .extend(catalog_kinds(c.category));
        }
    }
    // The catalogs answer side by side (MusicBrainz is slow by budget), and
    // one failing catalog does not hide the others' results: the search
    // fails only when every catalog failed.
    let mut searches = tokio::task::JoinSet::new();
    let mut names = GenreNames::new();
    let mut failure = None;
    for (index, (catalog_id, kinds)) in by_catalog.into_iter().enumerate() {
        let catalog = match open_catalog(&state, catalog_id).await {
            Ok(catalog) => catalog,
            Err(err) => {
                failure.get_or_insert(err);
                continue;
            }
        };
        let text = text.to_owned();
        searches.spawn(async move {
            // Genre names for the cards, read beside the search.
            let query = SearchQuery {
                text: &text,
                kinds: &kinds,
                page,
                language: LANGUAGE,
            };
            let (result, genres) = tokio::join!(catalog.search(&query), genre_names(&catalog));
            (index, result, genres)
        });
    }
    let mut results = Vec::new();
    while let Some(joined) = searches.join_next().await {
        match joined? {
            (index, Ok(page), genres) => {
                names.extend(genres);
                results.push((index, page));
            }
            (_, Err(err), _) => {
                let err = adapter_error(&err);
                failure.get_or_insert(err);
            }
        }
    }
    if results.is_empty()
        && let Some(err) = failure
    {
        return Err(err);
    }
    // In catalog order, as before.
    results.sort_by_key(|(index, _)| *index);
    for (_, result) in results {
        found.items.extend(result.items);
        found.next = found.next.max(result.next);
    }
    Ok(Json(TitlePage {
        items: cards(&state, member_id, found.items, &names).await?,
        next: found.next,
    }))
}

/// `YouTube` link of a trailer; other sites and odd keys give none.
fn trailer_url(trailer: Option<Trailer>) -> Option<String> {
    let trailer = trailer?;
    let key_ok = !trailer.key.is_empty()
        && trailer.key.len() <= 32
        && trailer
            .key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    (trailer.site.eq_ignore_ascii_case("youtube") && key_ok)
        .then(|| format!("https://www.youtube.com/watch?v={}", trailer.key))
}

/// "Open in" links for a title in the consume services of its category.
async fn open_in(
    state: &AppState,
    category: Category,
    kind: MediaKind,
    item: &starr_db::catalog::CatalogItem,
) -> Result<Vec<OpenInLink>, ApiError> {
    let links = state
        .db
        .title_links(
            category,
            kind.as_str(),
            item.tmdb_id,
            item.tvdb_id,
            item.imdb_id.as_deref(),
        )
        .await?;
    Ok(open_in_links(links))
}

/// Details of a movie or series with the member's status per season and
/// where to watch it (D13, D15, O3). Side effect: opening a title records
/// it in the shared catalog cache (when missing, expired or missing a
/// season), so its seasons are known to the status lookup.
#[utoipa::path(get, path = "/api/v1/titles/{kind}/{id}", tag = "discover",
    params(
        ("kind" = MediaKindDto, Path, description = "`movie` or `series` (artists and albums: `/api/v1/artists/{id}`, `/api/v1/albums/{id}`)"),
        ("id" = String, Path, description = "The title's id in the category's catalog (TMDB for movies and series), as in `TitleCard.id`"),
    ),
    responses(
        (status = 200, body = TitleDetails),
        (status = 404, body = ErrorBody, description = "`discover.unavailable` (not entitled, or Discover not ready), `catalog.not_found`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn title_details(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path((api_kind, id)): Path<(MediaKindDto, String)>,
) -> Result<Json<TitleDetails>, ApiError> {
    let kind: MediaKind = api_kind.into();
    // Artists and albums have their own views (`/artists`, `/albums`),
    // scenes only the private area (spec 0007 Q11).
    if matches!(
        kind,
        MediaKind::Artist | MediaKind::Album | MediaKind::Scene
    ) {
        return Err(ApiError::NotFound("catalog.not_found"));
    }
    let title = title_ref_or_404(kind, &id)?;
    let category = category_of(kind);
    let member_id = session.subject.id;
    let catalog = browsable(&state, member_id, category).await?;
    let (details, item) = record_title(&state, category, &title, Record::IfStale).await?;
    let MemberTitleStatus {
        status,
        requested_by_me,
        stage,
        seasons: season_status,
    } = member_title_status(&state, member_id, &title).await?;
    let arriving = stage
        .into_iter()
        .chain(season_status.iter().filter_map(|u| u.stage))
        .find(|s| *s == DownloadStage::Arriving);
    let library = match arriving {
        Some(_) => state.db.library_name(category).await?,
        None => None,
    };
    let journeys = MemberRequests::load(&state, member_id, Some(item.id))
        .await?
        .title(api_kind, &title.id);
    let seasons = details
        .children
        .into_iter()
        .filter_map(|child| match child {
            Child::Season(season) => Some(season),
            Child::Album(_) | Child::Scene(_) => None,
        })
        .map(|s| {
            let unit = season_status.iter().find(|u| u.season == Some(s.number));
            SeasonDetails {
                number: s.number,
                name: s.name,
                episode_count: s.episode_count,
                air_date: s.air_date,
                special: s.is_special,
                status: unit.map_or(AvailabilityDto::New, |u| u.status.into()),
                requested_by_me: unit.is_some_and(|u| u.mine),
                stage: unit.and_then(|u| u.stage).map(Into::into),
            }
        })
        .collect();
    let open_in = open_in(&state, category, kind, &item).await?;
    let requestable = source_target(&state, category).await?.is_some();
    let entry = details.entry;
    let facts = details.facts;
    let release_in = release_region(&catalog.relevance);
    let (watch_providers, attributions) = region_offers(details.watch_offers, &catalog.relevance);
    let series = facts.series.unwrap_or_default();
    Ok(Json(TitleDetails {
        kind: api_kind,
        id: title.id,
        category: category.into(),
        title: entry.title,
        original_title: entry.original_title,
        year: entry.release_date.map(|d| d.year()),
        overview: entry.overview,
        poster: file_name(entry.poster),
        backdrop: file_name(entry.backdrop),
        genres: details.genres.into_iter().map(|g| g.name).collect(),
        rating: entry.rating,
        vote_count: entry.vote_count,
        trailer_url: trailer_url(details.trailer),
        status: status.into(),
        requested_by_me,
        stage: stage.map(Into::into),
        arriving_in: arriving_in(arriving, library),
        journeys,
        seasons,
        open_in,
        requestable,
        release_date: entry.release_date,
        digital_release_date: digital_release(&facts.digital_releases, release_in),
        runtime_minutes: facts.runtime_minutes,
        certification: certification(&details.certifications, release_in),
        tagline: Some(facts.tagline).filter(|t| !t.is_empty()),
        series_status: Some(series.status).filter(|s| !s.is_empty()),
        season_count: series.season_count,
        next_episode: series.next_episode.map(Into::into),
        networks: series.networks,
        cast: facts.cast.into_iter().map(Into::into).collect(),
        crew: facts.crew.into_iter().map(Into::into).collect(),
        watch_providers,
        details: TitleExtras {
            original_language: entry.original_language,
            companies: facts.production_companies,
            external_ids: super::title_facts::ExternalIdsDto::of(&details.external_ids),
        },
        attributions,
    }))
}

/// The offers of the relevance's provider region (Q12 A), credited to
/// `JustWatch` when there are any.
fn region_offers(
    offers: Vec<starr_core::catalog::WatchOffer>,
    relevance: &Relevance,
) -> (Vec<WatchProviderDto>, Vec<Attribution>) {
    let Some(region) = provider_region(relevance) else {
        return (Vec::new(), Vec::new());
    };
    let shown: Vec<WatchProviderDto> = offers
        .into_iter()
        .filter(|o| o.region.eq_ignore_ascii_case(region))
        .map(Into::into)
        .collect();
    let credits = if shown.is_empty() {
        Vec::new()
    } else {
        vec![Attribution::justwatch()]
    };
    (shown, credits)
}

/// The episodes of one season, read when its "Episodes" are opened (spec
/// 0008 slice 2): one catalog request per season, cached a day.
#[utoipa::path(get, path = "/api/v1/titles/series/{id}/seasons/{number}", tag = "discover",
    params(
        ("id" = String, Path, description = "The series' TMDB id, as in `TitleCard.id`"),
        ("number" = u32, Path, description = "Season number (0: specials)"),
    ),
    responses(
        (status = 200, body = SeasonEpisodes),
        (status = 404, body = ErrorBody, description = "`discover.unavailable` (not entitled to series, or Discover not ready), `catalog.not_found`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (admin session)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn season_episodes(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    Path((id, number)): Path<(String, u32)>,
) -> Result<Json<SeasonEpisodes>, ApiError> {
    let series = title_ref_or_404(MediaKind::Series, &id)?;
    let catalog = browsable(&state, session.subject.id, Category::Series).await?;
    let details = open_catalog(&state, catalog.instance_id)
        .await?
        .details(&CatalogRef::season(&series.id, number), LANGUAGE)
        .await
        .map_err(|e| adapter_error(&e))?;
    let overview = details.entry.overview;
    let season = details
        .children
        .into_iter()
        .find_map(|child| match child {
            Child::Season(season) if season.number == number => Some(season),
            _ => None,
        })
        .ok_or(ApiError::NotFound("catalog.not_found"))?;
    Ok(Json(SeasonEpisodes {
        number: season.number,
        name: season.name,
        overview,
        air_date: season.air_date,
        episodes: season.episodes.into_iter().map(Into::into).collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trailer(site: &str, key: &str) -> Option<Trailer> {
        let trailer = Trailer {
            site: site.to_owned(),
            key: key.to_owned(),
            name: "Trailer".to_owned(),
        };
        (!site.is_empty()).then_some(trailer)
    }

    #[test]
    fn only_plain_youtube_keys_become_links() {
        assert_eq!(
            trailer_url(trailer("YouTube", "YoHD9XEInc0")).as_deref(),
            Some("https://www.youtube.com/watch?v=YoHD9XEInc0")
        );
        assert_eq!(trailer_url(trailer("Vimeo", "123")), None);
        assert_eq!(trailer_url(trailer("YouTube", "a&b=c")), None);
        assert_eq!(trailer_url(trailer("YouTube", "")), None);
        assert_eq!(trailer_url(None), None);
    }

    fn catalog(descriptor: &'static AdapterDescriptor, relevance: Relevance) -> BrowsableCatalog {
        BrowsableCatalog {
            instance_id: 1,
            relevance,
            descriptor,
        }
    }

    #[test]
    fn on_services_needs_region_and_providers() {
        let tmdb = &starr_adapter_tmdb::DESCRIPTOR;
        let mut relevance = Relevance::default();
        assert!(!lists_for(&catalog(tmdb, relevance.clone())).contains(&ListKindDto::OnServices));
        relevance.provider_region = Some("CH".to_owned());
        assert!(!lists_for(&catalog(tmdb, relevance.clone())).contains(&ListKindDto::OnServices));
        relevance.providers = vec![8];
        assert_eq!(lists_for(&catalog(tmdb, relevance)).len(), 5);
    }

    #[test]
    fn lists_come_from_the_catalogs_own_descriptor() {
        let relevance = Relevance {
            provider_region: Some("CH".to_owned()),
            providers: vec![8],
            ..Relevance::default()
        };
        let tmdb = catalog(&starr_adapter_tmdb::DESCRIPTOR, relevance);
        assert_eq!(
            lists_for(&tmdb),
            [
                ListKindDto::Trending,
                ListKindDto::Popular,
                ListKindDto::Upcoming,
                ListKindDto::OnServices,
                ListKindDto::TopRated,
            ]
        );
        assert!(matches!(
            parse_list(&tmdb, "fresh_releases"),
            Err(ApiError::NotFound("discover.unknown_list"))
        ));
        assert_eq!(
            parse_list(&tmdb, "top_rated").ok(),
            Some(ListKind::TopRated)
        );
        let music = catalog(
            &starr_adapter_musicbrainz::DESCRIPTOR,
            Relevance::music_default(),
        );
        assert_eq!(
            lists_for(&music),
            [
                ListKindDto::Trending,
                ListKindDto::FreshReleases,
                ListKindDto::Popular
            ]
        );
        assert!(matches!(
            parse_list(&music, "top_rated"),
            Err(ApiError::NotFound("discover.unknown_list"))
        ));
    }

    fn params(provider: Option<u64>, genre: Option<&str>, sort: Option<SortDto>) -> ListParams {
        ListParams {
            page: None,
            provider,
            genre: genre.map(str::to_owned),
            sort,
            hide_owned: false,
        }
    }

    fn code(result: Result<ListPlan, ApiError>) -> &'static str {
        match result {
            Err(ApiError::Validation(code) | ApiError::NotFound(code)) => code,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn category_page_lists_and_filters_become_catalog_lists() {
        let tmdb = catalog(&starr_adapter_tmdb::DESCRIPTOR, Relevance::default());
        let plan = |kind, raw, p: ListParams| plan_list(&tmdb, kind, raw, &p);
        // "On <provider>" and genre lists are Popular with a filter.
        let on = plan(MediaKind::Movie, "on_provider", params(Some(8), None, None)).unwrap();
        assert_eq!(on.list, ListKind::Popular);
        assert_eq!(on.filter, Some(ListFilter::Provider(8)));
        let genre = plan(MediaKind::Movie, "genre", params(None, Some("18"), None)).unwrap();
        assert_eq!(genre.filter, Some(ListFilter::Genre(GenreRef::Id(18))));
        // Both: the provider goes to the catalog, the genre is applied here.
        let both = plan(
            MediaKind::Movie,
            "on_provider",
            params(Some(8), Some("18"), Some(SortDto::Rating)),
        )
        .unwrap();
        assert_eq!(
            (both.list, both.filter, both.genre_after),
            (ListKind::TopRated, Some(ListFilter::Provider(8)), Some(18))
        );
        let newest = plan(
            MediaKind::Series,
            "popular",
            params(None, None, Some(SortDto::ReleaseDate)),
        )
        .unwrap();
        assert_eq!(newest.list, ListKind::New);
        // Series lists for series only.
        assert_eq!(
            plan(MediaKind::Series, "on_the_air", params(None, None, None))
                .unwrap()
                .list,
            ListKind::OnTheAir
        );
        assert_eq!(
            code(plan(
                MediaKind::Movie,
                "on_the_air",
                params(None, None, None)
            )),
            "discover.unknown_list"
        );
        // "Returning soon" was dropped (spec 0008, maintainer 2026-11-07).
        assert_eq!(
            code(plan(
                MediaKind::Series,
                "returning_soon",
                params(None, None, None)
            )),
            "discover.unknown_list"
        );
        // Missing or malformed parameters, and sorts of windowed lists.
        for (raw, p, expected) in [
            (
                "on_provider",
                params(None, None, None),
                "validation.provider",
            ),
            ("genre", params(None, None, None), "validation.genre"),
            (
                "genre",
                params(None, Some("drama"), None),
                "validation.genre",
            ),
            (
                "upcoming",
                params(None, None, Some(SortDto::Rating)),
                "validation.sort",
            ),
            (
                "trending",
                params(None, None, Some(SortDto::Popularity)),
                "validation.sort",
            ),
            (
                "on_services",
                params(None, None, None),
                "discover.unknown_list",
            ),
            // TMDB's own lists take no provider filter.
            (
                "trending",
                params(Some(8), None, None),
                "validation.provider",
            ),
        ] {
            assert_eq!(code(plan(MediaKind::Movie, raw, p)), expected, "{raw}");
        }
        assert_eq!(
            code(plan(
                MediaKind::Series,
                "on_the_air",
                params(Some(8), None, None)
            )),
            "validation.provider"
        );
        // A genre there is applied to the results.
        assert!(
            plan(
                MediaKind::Series,
                "on_the_air",
                params(None, Some("18"), None)
            )
            .is_ok()
        );
    }

    #[test]
    fn music_takes_genre_lanes_only() {
        let music = catalog(
            &starr_adapter_musicbrainz::DESCRIPTOR,
            Relevance::music_default(),
        );
        let plan = |raw, p: ListParams| plan_list(&music, MediaKind::Album, raw, &p);
        let lane = plan("genre", params(None, Some("hip hop"), None)).unwrap();
        assert_eq!(lane.music_genre.as_deref(), Some("hip hop"));
        assert_eq!(lane.filter, None);
        for (raw, p, expected) in [
            (
                "genre",
                params(None, Some("polka"), None),
                "validation.genre",
            ),
            (
                "trending",
                params(None, Some("rock"), None),
                "validation.genre",
            ),
            (
                "trending",
                params(Some(8), None, None),
                "validation.provider",
            ),
            (
                "popular",
                params(None, None, Some(SortDto::Rating)),
                "validation.sort",
            ),
            (
                "on_provider",
                params(None, None, None),
                "discover.unknown_list",
            ),
            (
                "on_the_air",
                params(None, None, None),
                "discover.unknown_list",
            ),
        ] {
            assert_eq!(code(plan(raw, p)), expected, "{raw}");
        }
        assert_eq!(
            plan("fresh_releases", params(None, None, None))
                .unwrap()
                .list,
            ListKind::FreshReleases
        );
    }

    #[test]
    fn the_switcher_lists_configured_providers_first_up_to_twelve() {
        let relevance = Relevance {
            providers: vec![337, 8, 999],
            ..Relevance::default()
        };
        let region: Vec<_> = (1..=20)
            .chain([337])
            .map(|id| starr_core::catalog::WatchProvider {
                id,
                name: format!("P{id}"),
                logo: None,
            })
            .collect();
        let ids: Vec<u64> = switcher(&relevance, region).iter().map(|p| p.id).collect();
        assert_eq!(ids, [337, 8, 1, 2, 3, 4, 5, 6, 7, 9, 10, 11]);
    }

    #[test]
    fn highlights_interleave_by_rank() {
        let mut ranked = vec![
            (1, 0, Category::Movies, "m1"),
            (3, 0, Category::Movies, "m3"),
            (2, 1, Category::Series, "s2"),
            (1, 1, Category::Series, "s1"),
        ];
        interleave(&mut ranked);
        let order: Vec<_> = ranked.iter().map(|r| r.3).collect();
        assert_eq!(order, ["m1", "s1", "s2", "m3"]);
    }
}
