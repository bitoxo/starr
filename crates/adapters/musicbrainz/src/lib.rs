//! `MusicBrainz` catalog adapter (ADR 0013, spec 0006 slice 2): [`Probe`]
//! and [`Catalog`] over `MusicBrainz` `/ws/2` (search, details,
//! discography) and `ListenBrainz` (the Trending, Fresh releases and
//! Popular lanes), plus the Cover Art Archive client for Starr's image
//! proxy ([`covers`]).
//!
//! Both services want one request per second at most and a User-Agent with
//! a contact; Starr sends [`starr_adapter_kit::USER_AGENT`] (the project
//! URL, nothing about the operator, spec 0006 Q1) and keeps the budget per
//! host and process ([`client_options`], Q14). `MusicBrainz` answers
//! overload with 503, which pauses every caller ([`AdapterError::Throttled`]).
//! Responses are cached long: `MusicBrainz` asks clients not to poll.
//!
//! Facts used here and their evidence: `docs/concept/capabilities.md`,
//! sections "`MusicBrainz` and `ListenBrainz`" and "Cover Art Archive".

pub mod covers;
mod wire;

use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, NaiveDate};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use starr_adapter_kit::{AuthScheme, CachePolicy, ClientOptions, ResponseCache, ServiceClient};
use starr_core::adapters::{
    AdapterDescriptor, AdapterError, Budgets, Capability, IdentityRule, Probe, ProbeReport,
    RateLimit, SecretClass, Version, VersionRange,
};
use starr_core::catalog::{
    Catalog, CatalogConfiguration, CatalogDetails, CatalogEntry, CatalogRef, Child, ExternalId,
    Genre, ListKind, ListQuery, MediaKind, Page, Relevance, SearchQuery, TracklistRelease,
    WatchProvider,
};
use url::Url;
use uuid::Uuid;

use wire::{mbid, mbid_text};

/// `MusicBrainz` needs no credential.
pub const AUTH: AuthScheme = AuthScheme::None;

/// Base URL operators enter for the `MusicBrainz` service (a mirror works
/// too).
pub const API_BASE: &str = "https://musicbrainz.org";

/// Base URL of the `ListenBrainz` API (fixed; not an operator setting).
pub const LISTENBRAINZ_BASE: &str = "https://api.listenbrainz.org";

/// `/ws/2` is the only `MusicBrainz` API version.
const API_VERSION: Version = Version::new(2, 0, 0, 0);

/// An artist every `MusicBrainz` database has (Radiohead): the probe reads
/// it.
const PROBE_ARTIST: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";

/// Results per search or list page (`MusicBrainz` default and spec 0006).
pub const PAGE_SIZE: u32 = 25;

/// Pages a search or list offers at most.
pub const PAGE_CAP: u32 = 20;

/// Release groups per discography page (the browse maximum).
const BROWSE_LIMIT: u32 = 100;

/// Discography pages read at most for one artist page (300 release
/// groups): each costs a second or more of the budget while a member
/// waits.
pub const DISCOGRAPHY_PAGES: u32 = 3;

/// Longest a request waits for its turn in the budget before it fails as
/// "busy" ([`AdapterError::Throttled`]), so interactive requests do not
/// pile up behind a slow budget.
pub const MAX_WAIT: Duration = Duration::from_secs(10);

/// Days before and after today the Fresh releases lane spans
/// (`ListenBrainz` allows 90).
pub const FRESH_DAYS: u32 = 30;

/// One request per second towards `MusicBrainz` (and towards
/// `ListenBrainz`, whose API guidelines ask the same), per host.
pub const RATE_LIMIT: RateLimit = RateLimit {
    requests: 1,
    per: Duration::from_secs(1),
};

/// Cache lifetimes (spec 0006 "musicbrainz"): search 1 day (raised from the
/// spec's 1 h by the maintainer, 2026-11-12: each search costs two requests
/// of the 1 req/s budget, and results barely change in a day), album
/// details 7 days, discography 1 day, `ListenBrainz` lists by
/// their headers within 1 h to 1 day. `MusicBrainz` sends no useful
/// lifetimes, so min = max.
pub const SEARCH_CACHE: CachePolicy = CachePolicy {
    min_ttl: Duration::from_hours(24),
    max_ttl: Duration::from_hours(24),
};
pub const DETAILS_CACHE: CachePolicy = CachePolicy {
    min_ttl: Duration::from_hours(7 * 24),
    max_ttl: Duration::from_hours(7 * 24),
};
pub const DISCOGRAPHY_CACHE: CachePolicy = CachePolicy {
    min_ttl: Duration::from_hours(24),
    max_ttl: Duration::from_hours(24),
};
pub const LIST_CACHE: CachePolicy = CachePolicy {
    min_ttl: Duration::from_hours(1),
    max_ttl: Duration::from_hours(24),
};
/// Genre lanes: a day, refreshed twice a day by the worker (spec 0008
/// slice 2), so a lane is replaced before it expires.
pub const GENRE_LANE_CACHE: CachePolicy = CachePolicy {
    min_ttl: Duration::from_hours(24),
    max_ttl: Duration::from_hours(24),
};

/// `ListenBrainz`'s popularity lookup by release group (genre lanes,
/// maintainer 2026-11-07).
const POPULARITY_PATH: &str = "1/popularity/release-group";

/// `inc` of the album's release-group lookup (spec 0006 as amended by spec
/// 0008 Q17 B).
pub const ALBUM_INC: &str = "artist-credits+releases+genres";
/// `inc` of the counted release's lookup: track count, tracklist, label.
pub const RELEASE_INC: &str = "media+recordings+artist-credits+labels";

/// How far back a genre lane looks for first releases (about a year).
const GENRE_LANE_DAYS: u64 = 365;

/// The genres of the music category page's tiles and lanes, in display
/// order (spec 0008 F-8: a fixed list of broad `MusicBrainz` genres in
/// code, no setting): the genre as `MusicBrainz` names it, and its display
/// name.
pub const GENRES: [(&str, &str); 12] = [
    ("indie", "Indie"),
    ("electronic", "Electronic"),
    ("jazz", "Jazz"),
    ("hip hop", "Hip-Hop"),
    ("soul", "Soul"),
    ("classical", "Classical"),
    ("ambient", "Ambient"),
    ("rock", "Rock"),
    ("pop", "Pop"),
    ("metal", "Metal"),
    ("folk", "Folk"),
    ("punk", "Punk"),
];

pub const DESCRIPTOR: AdapterDescriptor = AdapterDescriptor {
    type_id: "musicbrainz",
    display_name: "MusicBrainz",
    capabilities: &[Capability::Metadata],
    tested_versions: VersionRange {
        min: API_VERSION,
        max_exclusive: Version::new(3, 0, 0, 0),
    },
    budgets: Budgets {
        rate_limit: RATE_LIMIT,
        login_limit: None,
        expensive_endpoints: &[],
    },
    identity: IdentityRule::SingleUser,
    // No credential at all; the catalog class lets the web process use it
    // for search and Discover (ADR 0003 amendment 1).
    secret_class: SecretClass::CatalogReadToken,
    lists: &[
        ListKind::Trending,
        ListKind::FreshReleases,
        ListKind::Popular,
    ],
};

/// Client options for `MusicBrainz` and `ListenBrainz` in a process that
/// holds `percent` of the host's budget (100 in single-process mode; web
/// 70, worker 30 in split mode, spec 0006 Q14). 503 pauses every caller.
#[must_use]
pub fn client_options(percent: u32) -> ClientOptions {
    ClientOptions {
        rate_limit: starr_adapter_kit::share_of(RATE_LIMIT, percent),
        unavailable_is_throttle: true,
        max_wait: Some(MAX_WAIT),
        private_content: false,
    }
}

/// The `ListenBrainz` client of a process holding `percent` of the budget.
pub fn listenbrainz_client(base: Url, percent: u32) -> Result<ServiceClient, AdapterError> {
    ServiceClient::public_with("listenbrainz", client_options(percent), base)
}

/// The image reference of a release group's cover (`catalog_item`
/// convention of ADR 0013: `<source>:<ref>`).
#[must_use]
pub fn cover_ref(release_group: Uuid) -> String {
    format!("caa:{}", mbid_text(release_group))
}

/// The `MusicBrainz` adapter. Cheap to clone.
#[derive(Debug, Clone)]
pub struct MusicBrainz {
    client: ServiceClient,
    listenbrainz: Option<ServiceClient>,
    cache: Option<Arc<dyn ResponseCache>>,
    /// Fixed "today" for tests; the system date otherwise.
    today: Option<NaiveDate>,
}

impl MusicBrainz {
    /// The adapter over the instance's `MusicBrainz` client. Without
    /// [`Self::with_listenbrainz`] the lists are unavailable (the probe
    /// needs none).
    #[must_use]
    pub fn new(client: ServiceClient) -> Self {
        Self {
            client,
            listenbrainz: None,
            cache: None,
            today: None,
        }
    }

    /// Read the lists from `ListenBrainz` over `client`.
    #[must_use]
    pub fn with_listenbrainz(mut self, client: ServiceClient) -> Self {
        self.listenbrainz = Some(client);
        self
    }

    /// Cache responses in `cache`.
    #[must_use]
    pub fn with_cache(mut self, cache: Arc<dyn ResponseCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Use `today` for date windows instead of the system date.
    #[must_use]
    pub fn with_today(mut self, today: NaiveDate) -> Self {
        self.today = Some(today);
        self
    }

    fn today(&self) -> NaiveDate {
        self.today
            .unwrap_or_else(|| chrono::Utc::now().date_naive())
    }

    async fn get<T: DeserializeOwned>(
        &self,
        client: &ServiceClient,
        path: &str,
        query: &[(&str, String)],
        policy: CachePolicy,
    ) -> Result<T, AdapterError> {
        let cache = self
            .cache
            .as_deref()
            .map(|c| (c as &dyn ResponseCache, policy));
        client.get_json_cached(path, query, cache).await
    }

    /// `GET /ws/2/<path>` as JSON.
    async fn ws<T: DeserializeOwned>(
        &self,
        path: &str,
        mut query: Vec<(&str, String)>,
        policy: CachePolicy,
    ) -> Result<T, AdapterError> {
        query.push(("fmt", "json".to_owned()));
        self.get(&self.client, &format!("ws/2/{path}"), &query, policy)
            .await
    }

    fn listenbrainz(&self) -> Result<&ServiceClient, AdapterError> {
        self.listenbrainz
            .as_ref()
            .ok_or_else(|| AdapterError::Unknown("no ListenBrainz client".to_owned()))
    }

    async fn search_artists(
        &self,
        text: &str,
        offset: u32,
    ) -> Result<(Vec<CatalogEntry>, bool), AdapterError> {
        let wire: wire::ArtistSearchWire = self
            .ws("artist", search_query(text, offset), SEARCH_CACHE)
            .await?;
        let more = wire::has_more(wire.offset, wire.artists.len(), wire.count);
        Ok((
            wire.artists
                .into_iter()
                .filter_map(wire::ArtistWire::into_entry)
                .collect(),
            more,
        ))
    }

    async fn search_albums(
        &self,
        text: &str,
        offset: u32,
    ) -> Result<(Vec<CatalogEntry>, bool), AdapterError> {
        let wire: wire::ReleaseGroupSearchWire = self
            .ws("release-group", search_query(text, offset), SEARCH_CACHE)
            .await?;
        let more = wire::has_more(wire.offset, wire.release_groups.len(), wire.count);
        Ok((
            wire.release_groups
                .into_iter()
                .filter_map(|rg| rg.into_entry(None))
                .collect(),
            more,
        ))
    }

    /// A sitewide release-group statistic (`week` or `month`), one page.
    async fn statistic(&self, range: &str, page: u32) -> Result<Page<CatalogEntry>, AdapterError> {
        let offset = (page - 1) * PAGE_SIZE;
        let wire: Option<wire::StatsWire> = self
            .get(
                self.listenbrainz()?,
                "1/stats/sitewide/release-groups",
                &[
                    ("range", range.to_owned()),
                    ("count", PAGE_SIZE.to_string()),
                    ("offset", offset.to_string()),
                ],
                LIST_CACHE,
            )
            .await?;
        // 204: not computed yet.
        let Some(wire) = wire else {
            return Ok(Page::end());
        };
        let payload = wire.payload;
        let more = wire::has_more(
            payload.offset,
            payload.release_groups.len(),
            payload.total_release_group_count,
        );
        Ok(Page {
            items: payload
                .release_groups
                .into_iter()
                .filter_map(wire::StatsReleaseGroupWire::into_entry)
                .collect(),
            next: next_page(page, more),
        })
    }

    /// Recent and announced releases around today, newest first, filtered
    /// by the relevance's release types; paged here (`ListenBrainz` returns
    /// the whole window at once).
    async fn fresh_releases(
        &self,
        page: u32,
        relevance: &Relevance,
    ) -> Result<Page<CatalogEntry>, AdapterError> {
        let wire: wire::FreshWire = self
            .get(
                self.listenbrainz()?,
                "1/explore/fresh-releases/",
                &[
                    ("release_date", self.today().format("%Y-%m-%d").to_string()),
                    ("days", FRESH_DAYS.to_string()),
                    ("sort", "release_date".to_owned()),
                    ("past", "true".to_owned()),
                    ("future", "true".to_owned()),
                ],
                LIST_CACHE,
            )
            .await?;
        let mut seen = std::collections::HashSet::new();
        let mut items: Vec<CatalogEntry> = wire
            .payload
            .releases
            .into_iter()
            .filter_map(wire::FreshReleaseWire::into_entry)
            .filter(|e| admitted(relevance, e))
            .filter(|e| seen.insert(e.item.id.clone()))
            .collect();
        items.sort_by_key(|e| std::cmp::Reverse(e.release_date));
        let start = usize::try_from((page - 1) * PAGE_SIZE).unwrap_or(usize::MAX);
        let size = PAGE_SIZE as usize;
        let more = items.len() > start.saturating_add(size);
        Ok(Page {
            items: items.into_iter().skip(start).take(size).collect(),
            next: next_page(page, more),
        })
    }

    /// An album: the release group with its genres, and the counted
    /// release with its tracklist and label (spec 0008 slice 2: still two
    /// requests, both cached 7 days).
    async fn album(&self, id: Uuid) -> Result<CatalogDetails, AdapterError> {
        let mut rg: wire::ReleaseGroupWire = self
            .ws(
                &format!("release-group/{}", mbid_text(id)),
                vec![("inc", ALBUM_INC.to_owned())],
                DETAILS_CACHE,
            )
            .await?;
        let counted = wire::counted_release(&rg.releases).and_then(|r| mbid(&r.id));
        let release = match counted {
            Some(release) => Some(
                self.ws::<wire::ReleaseWire>(
                    &format!("release/{}", mbid_text(release)),
                    vec![("inc", RELEASE_INC.to_owned())],
                    DETAILS_CACHE,
                )
                .await?,
            ),
            None => None,
        };
        let release_count = u32::try_from(rg.releases.len()).unwrap_or(u32::MAX);
        let genres = wire::genre_names(std::mem::take(&mut rg.genres));
        let artists: Vec<Uuid> = rg
            .artist_credit
            .iter()
            .filter_map(|c| mbid(&c.artist.id))
            .collect();
        let mut entry = rg
            .into_entry(None)
            .ok_or_else(|| AdapterError::Unknown("release group without a valid id".to_owned()))?;
        if let Some(facts) = entry.album.as_mut() {
            facts.genres = genres;
            if let Some(release) = release {
                facts.track_count = Some(release.media.iter().map(|m| m.track_count).sum());
                facts.tracks = release.tracks(&facts.artist_credit);
                facts.label = release.label();
                facts.tracklist_release = Some(TracklistRelease {
                    date: wire::partial_date(release.date.as_deref()),
                    country: release.country.filter(|c| !c.trim().is_empty()),
                    release_count,
                });
            }
        }
        let mut external_ids = vec![ExternalId::MbReleaseGroup(id)];
        external_ids.extend(artists.into_iter().map(ExternalId::MbArtist));
        Ok(details(entry, external_ids, Vec::new()))
    }

    async fn artist(&self, id: Uuid) -> Result<CatalogDetails, AdapterError> {
        let mut artist: wire::ArtistWire = self
            .ws(
                &format!("artist/{}", mbid_text(id)),
                vec![("inc", "genres".to_owned())],
                DETAILS_CACHE,
            )
            .await?;
        let genres: Vec<Genre> = wire::genre_names(std::mem::take(&mut artist.genres))
            .into_iter()
            .map(|name| Genre {
                kind: MediaKind::Artist,
                id: 0,
                name,
            })
            .collect();
        let mut entry = artist
            .into_entry()
            .ok_or_else(|| AdapterError::Unknown("artist without a valid id".to_owned()))?;
        let mut albums = Vec::new();
        for page in 0..DISCOGRAPHY_PAGES {
            let wire: wire::ReleaseGroupBrowseWire = self
                .ws(
                    "release-group",
                    vec![
                        ("artist", mbid_text(id)),
                        ("limit", BROWSE_LIMIT.to_string()),
                        ("offset", (page * BROWSE_LIMIT).to_string()),
                    ],
                    DISCOGRAPHY_CACHE,
                )
                .await?;
            let more = wire::has_more(wire.offset, wire.release_groups.len(), wire.count);
            albums.extend(
                wire.release_groups
                    .into_iter()
                    .filter_map(|rg| rg.into_entry(Some((&entry.title, id)))),
            );
            if !more || albums.is_empty() {
                break;
            }
        }
        // Newest first; undated last.
        albums.sort_by_key(|a| std::cmp::Reverse(a.release_date));
        // MusicBrainz has no artist pictures: the newest released album of
        // the followed types stands in (spec 0006 Q8).
        let today = self.today();
        let picture = Relevance::music_default();
        entry.poster = albums
            .iter()
            .filter(|a| a.release_date.is_some_and(|d| d <= today))
            .find(|a| admitted(&picture, a))
            .and_then(|a| a.poster.clone());
        let mut found = details(
            entry,
            vec![ExternalId::MbArtist(id)],
            albums
                .into_iter()
                .map(|a| Child::Album(Box::new(a)))
                .collect(),
        );
        found.genres = genres;
        Ok(found)
    }

    /// The `MusicBrainz` search behind a genre lane (spec 0008 Q17 B):
    /// albums tagged with the genre, first released within the last year,
    /// by score. The window starts on the first of a month, so the request
    /// (and its cache key) stays the same between the worker's refresh and
    /// the web's reads for a whole month.
    fn genre_lane_request(&self, genre: &str) -> (String, Vec<(&'static str, String)>) {
        genre_lane_request(genre, lane_window_start(self.today()))
    }

    fn genre_lane_page(wire: wire::ReleaseGroupSearchWire) -> Page<CatalogEntry> {
        Page {
            items: wire
                .release_groups
                .into_iter()
                .filter_map(|rg| rg.into_entry(None))
                .collect(),
            // One page per lane: the background refresh keeps exactly that.
            next: None,
        }
    }

    /// Fetch a genre lane now and replace its cached page, for the worker's
    /// `catalog.refresh-lists` (spec 0008 slice 2): it runs at the
    /// worker's share of the budget, so members never wait for it. The
    /// search's release groups are ranked by their `ListenBrainz` listens
    /// (one popularity lookup per lane, maintainer 2026-11-07), most
    /// listened first; those without listens are dropped. The ranked
    /// answer is cached where the web process reads the lane. Any failure
    /// (a 429 or 503, `ListenBrainz` down, an answer of another shape, a
    /// lane where nothing has listens) leaves the lane cached before until
    /// it expires.
    /// `genre` must be one of [`GENRES`].
    pub async fn refresh_genre_lane(
        &self,
        genre: &str,
    ) -> Result<Page<CatalogEntry>, AdapterError> {
        if !GENRES.iter().any(|(id, _)| *id == genre) {
            return Ok(Page::end());
        }
        // A paused ListenBrainz would fail the ranking: spare the search.
        if let Some(retry_after) = self.listenbrainz()?.throttled_for() {
            return Err(AdapterError::Throttled {
                retry_after: Some(retry_after),
            });
        }
        let (path, query) = self.genre_lane_request(genre);
        let raw: serde_json::Value = self.client.get_json_cached(&path, &query, None).await?;
        // Every release group must read as the web will read it, before
        // ListenBrainz is asked or anything is stored.
        wire::ReleaseGroupSearchWire::deserialize(&raw)
            .map_err(|e| AdapterError::Unknown(format!("genre lane: {e}")))?;
        let mut found = wire::RawSearchWire::deserialize(&raw)
            .map_err(|e| AdapterError::Unknown(format!("genre lane: {e}")))?;
        let searched = !found.release_groups.is_empty();
        let listens = self.listens(&found.release_groups).await?;
        found.rank_by(&listens);
        // A search with results but no release group with listens means
        // the popularity answer changed or is empty for now: keep the lane.
        if searched && found.release_groups.is_empty() {
            return Err(AdapterError::Unknown(
                "no release group of the lane has ListenBrainz listens".to_owned(),
            ));
        }
        let body = serde_json::to_vec(&found).map_err(|e| AdapterError::Unknown(e.to_string()))?;
        let wire: wire::ReleaseGroupSearchWire =
            serde_json::from_slice(&body).map_err(|e| AdapterError::Unknown(e.to_string()))?;
        if let Some(cache) = self.cache.as_deref() {
            cache
                .put(
                    &starr_adapter_kit::cache_key(&path, &query),
                    &body,
                    GENRE_LANE_CACHE.min_ttl,
                )
                .await;
        }
        Ok(Self::genre_lane_page(wire))
    }

    /// Listens per release group from `ListenBrainz`'s popularity lookup
    /// (`POST /1/popularity/release-group`, [V 11-07]); unknown ones are
    /// missing or 0.
    async fn listens(
        &self,
        release_groups: &[serde_json::Value],
    ) -> Result<std::collections::HashMap<String, u64>, AdapterError> {
        let ids: Vec<&str> = release_groups
            .iter()
            .filter_map(|rg| rg.get("id").and_then(serde_json::Value::as_str))
            .collect();
        if ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let found: Vec<wire::PopularityWire> = self
            .listenbrainz()?
            .post_json(
                POPULARITY_PATH,
                &serde_json::json!({ "release_group_mbids": ids }),
            )
            .await?;
        Ok(found
            .into_iter()
            .filter_map(|p| Some((p.release_group_mbid, p.total_listen_count?)))
            .collect())
    }

    /// A genre lane as the last refresh cached it, without any request
    /// (the web process); `None` when none is cached (not refreshed yet, or
    /// expired). `genre` must be one of [`GENRES`].
    pub async fn cached_genre_lane(&self, genre: &str) -> Option<Page<CatalogEntry>> {
        if !GENRES.iter().any(|(id, _)| *id == genre) {
            return None;
        }
        let cache = self.cache.as_deref()?;
        // This month's window; right after the month turned, the lane the
        // last refresh cached under the previous month's window.
        let since = lane_window_start(self.today());
        for since in [since, previous_window_start(since)] {
            let (path, query) = genre_lane_request(genre, since);
            if let Some(cached) = self.client.get_json_cache_only(&path, &query, cache).await {
                return Some(Self::genre_lane_page(cached));
            }
        }
        None
    }
}

/// The first day of a genre lane's window for `today`: the first of the
/// month a year back, so the request (and its cache key) stays the same
/// for a whole month.
fn lane_window_start(today: NaiveDate) -> NaiveDate {
    today
        .checked_sub_days(chrono::Days::new(GENRE_LANE_DAYS))
        .and_then(|d| d.with_day(1))
        .unwrap_or(today)
}

/// The window start of the month before `since`.
fn previous_window_start(since: NaiveDate) -> NaiveDate {
    since
        .checked_sub_days(chrono::Days::new(1))
        .and_then(|d| d.with_day(1))
        .unwrap_or(since)
}

/// The search of a genre lane whose window starts on `since`.
fn genre_lane_request(genre: &str, since: NaiveDate) -> (String, Vec<(&'static str, String)>) {
    let query = format!(
        "tag:\"{}\" AND primarytype:album AND firstreleasedate:[{} TO *]",
        genre.replace(['"', '\\'], ""),
        since.format("%Y-%m-%d")
    );
    (
        "ws/2/release-group".to_owned(),
        vec![
            ("query", query),
            ("limit", PAGE_SIZE.to_string()),
            ("offset", "0".to_owned()),
            ("fmt", "json".to_owned()),
        ],
    )
}

/// Whether `relevance` admits an album entry by its types.
fn admitted(relevance: &Relevance, entry: &CatalogEntry) -> bool {
    entry
        .album
        .as_ref()
        .is_none_or(|a| relevance.admits_album(a.primary_type, &a.secondary_types))
}

fn details(
    entry: CatalogEntry,
    external_ids: Vec<ExternalId>,
    children: Vec<Child>,
) -> CatalogDetails {
    CatalogDetails {
        entry,
        external_ids,
        genres: Vec::new(),
        trailer: None,
        certifications: Vec::new(),
        watch_offers: Vec::new(),
        children,
        facts: starr_core::catalog::TitleFacts::default(),
    }
}

fn next_page(page: u32, more: bool) -> Option<u32> {
    (more && page < PAGE_CAP).then_some(page + 1)
}

/// Lucene query syntax characters `MusicBrainz` search would interpret;
/// member text is searched as typed.
fn escape_query(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "+-&|!(){}[]^\"~*?:\\/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn search_query(text: &str, offset: u32) -> Vec<(&'static str, String)> {
    vec![
        ("query", escape_query(text)),
        ("limit", PAGE_SIZE.to_string()),
        ("offset", offset.to_string()),
    ]
}

fn parse_mbid(item: &CatalogRef) -> Result<Uuid, AdapterError> {
    mbid(&item.id)
        .filter(|id| mbid_text(*id) == item.id)
        .ok_or_else(|| AdapterError::NotFound(format!("not an MBID: {:?}", item.id)))
}

impl Probe for MusicBrainz {
    fn descriptor(&self) -> &'static AdapterDescriptor {
        &DESCRIPTOR
    }

    async fn probe(&self) -> Result<ProbeReport, AdapterError> {
        // Never cached: the probe must prove reachability now.
        let _: wire::ArtistWire = self
            .client
            .get_json_cached(
                &format!("ws/2/artist/{PROBE_ARTIST}"),
                &[("fmt", "json".to_owned())],
                None,
            )
            .await?;
        Ok(ProbeReport {
            version: API_VERSION,
        })
    }
}

impl Catalog for MusicBrainz {
    /// `MusicBrainz` has no languages, regions, providers or genres to
    /// configure: the empty value means "not offered" (ADR 0013).
    fn configuration(
        &self,
    ) -> impl std::future::Future<Output = Result<CatalogConfiguration, AdapterError>> + Send {
        std::future::ready(Ok(CatalogConfiguration::default()))
    }

    fn providers(
        &self,
        _kind: MediaKind,
        _region: &str,
    ) -> impl std::future::Future<Output = Result<Vec<WatchProvider>, AdapterError>> + Send {
        std::future::ready(Ok(Vec::new()))
    }

    /// Artists, then albums (spec 0006 Q11): one `MusicBrainz` search per
    /// requested kind and page.
    async fn search(&self, query: &SearchQuery<'_>) -> Result<Page<CatalogEntry>, AdapterError> {
        let text = query.text.trim();
        if text.is_empty() || !(1..=PAGE_CAP).contains(&query.page) {
            return Ok(Page::end());
        }
        let offset = (query.page - 1) * PAGE_SIZE;
        let mut items = Vec::new();
        let mut more = false;
        if query.kinds.contains(&MediaKind::Artist) {
            let (found, has_more) = self.search_artists(text, offset).await?;
            items.extend(found);
            more |= has_more;
        }
        if query.kinds.contains(&MediaKind::Album) {
            let (found, has_more) = self.search_albums(text, offset).await?;
            items.extend(found);
            more |= has_more;
        }
        Ok(Page {
            items,
            next: next_page(query.page, more),
        })
    }

    /// Album lanes from `ListenBrainz`. Trending and Popular are sitewide
    /// statistics without release types, so the relevance applies to Fresh
    /// releases only.
    async fn list(&self, query: &ListQuery<'_>) -> Result<Page<CatalogEntry>, AdapterError> {
        if query.kind != MediaKind::Album || !(1..=PAGE_CAP).contains(&query.page) {
            return Ok(Page::end());
        }
        // Lists take no filters: genre lanes are refreshed by the worker
        // (`refresh_genre_lane`) and read from the cache only
        // (`cached_genre_lane`), so no list call spends the web budget on
        // them.
        if query.filter.is_some() {
            return Ok(Page::end());
        }
        match query.list {
            ListKind::Trending => self.statistic("week", query.page).await,
            ListKind::Popular => self.statistic("month", query.page).await,
            ListKind::FreshReleases => self.fresh_releases(query.page, query.relevance).await,
            ListKind::Upcoming
            | ListKind::OnServices
            | ListKind::TopRated
            | ListKind::New
            | ListKind::OnTheAir => Ok(Page::end()),
        }
    }

    async fn details(
        &self,
        item: &CatalogRef,
        _language: &str,
    ) -> Result<CatalogDetails, AdapterError> {
        let id = parse_mbid(item)?;
        match item.kind {
            MediaKind::Album => self.album(id).await,
            MediaKind::Artist => self.artist(id).await,
            MediaKind::Movie
            | MediaKind::Series
            | MediaKind::Scene
            | MediaKind::Performer
            | MediaKind::Studio => Err(AdapterError::NotFound(format!(
                "not a MusicBrainz kind: {}",
                item.kind.as_str()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_syntax_is_escaped() {
        assert_eq!(escape_query("AC/DC"), "AC\\/DC");
        assert_eq!(escape_query("a+b (live)"), "a\\+b \\(live\\)");
        assert_eq!(escape_query("Sigur Rós"), "Sigur Rós");
    }

    #[test]
    fn pages_stop_at_the_cap() {
        assert_eq!(next_page(1, true), Some(2));
        assert_eq!(next_page(1, false), None);
        assert_eq!(next_page(PAGE_CAP, true), None);
    }

    #[test]
    fn genre_lane_windows_start_on_the_first_of_a_month() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        // The same window (cache key) for the whole month.
        assert_eq!(lane_window_start(d(2026, 11, 3)), d(2025, 11, 1));
        assert_eq!(lane_window_start(d(2026, 11, 30)), d(2025, 11, 1));
        assert_eq!(lane_window_start(d(2026, 12, 1)), d(2025, 12, 1));
        // After the turn, the web also reads the previous month's lane.
        assert_eq!(previous_window_start(d(2025, 12, 1)), d(2025, 11, 1));
        assert_eq!(previous_window_start(d(2025, 1, 1)), d(2024, 12, 1));
        let (_, q) = genre_lane_request("hip \"hop\"", d(2025, 11, 1));
        assert_eq!(
            q[0].1,
            "tag:\"hip hop\" AND primarytype:album AND firstreleasedate:[2025-11-01 TO *]"
        );
    }

    #[test]
    fn covers_use_the_caa_reference() {
        let id = mbid("B1392450-E666-3926-A536-22C65F834433").unwrap();
        assert_eq!(cover_ref(id), "caa:b1392450-e666-3926-a536-22c65f834433");
    }
}
