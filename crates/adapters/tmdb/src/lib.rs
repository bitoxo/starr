//! TMDB catalog adapter (ADR 0009, spec 0003 slice 1): [`Probe`] and
//! [`Catalog`] over TMDB API v3, plus the image CDN client for Starr's
//! image proxy ([`images`]).
//!
//! Facts used here and their evidence: `docs/concept/capabilities.md`,
//! section "TMDB".

pub mod images;
mod params;
mod wire;

use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use serde::de::DeserializeOwned;
use starr_adapter_kit::{AuthScheme, CachePolicy, ResponseCache, ServiceClient};
use starr_core::adapters::{
    AdapterDescriptor, AdapterError, Budgets, Capability, IdentityRule, Probe, ProbeReport,
    RateLimit, SecretClass, Version, VersionRange,
};
use starr_core::catalog::{
    Catalog, CatalogConfiguration, CatalogDetails, CatalogEntry, CatalogRef, Child, ImageSizes,
    ListKind, ListQuery, MediaKind, Page, Relevance, SearchQuery, TitleFacts, WatchProvider,
};

pub use params::{
    DEFAULT_SERIES_EXCLUDED_GENRES, PAGE_CAP, PAGE_LISTS, TOP_RATED_MIN_VOTES_MOVIE,
    TOP_RATED_MIN_VOTES_SERIES,
};
use params::{FALLBACK_LANGUAGE, ListRequest, is_english, tmdb_kind, tmdb_language};
use wire::{DetailsWire, ItemWire, PageWire};

/// TMDB takes the API Read Access Token as a bearer token. The v3 "API
/// key" works only as a query parameter, which Starr never uses for
/// credentials (lesson 6).
pub const AUTH: AuthScheme = AuthScheme::Bearer;

/// Whether `value` has the shape of TMDB's 32-hex v3 "API key" (also the
/// shape of Radarr and Sonarr API keys). Starr never stores one as a TMDB
/// credential: the API refuses it, and re-sealing refuses to move one to
/// the web key (ADR 0003 amendment 1).
#[must_use]
pub fn is_hex_api_key(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Base URL operators enter for the TMDB service.
pub const API_BASE: &str = "https://api.themoviedb.org";

/// The API reports no version; Starr speaks v3 (path `/3/`).
const API_VERSION: Version = Version::new(3, 0, 0, 0);

/// How many earlier trending pages are checked for repeats.
const TRENDING_LOOKBACK: u32 = 3;

/// Cheapest authenticated call: validates the token, nothing else.
const PROBE_PATH: &str = "3/authentication";

/// Cache bounds: keep at least five minutes (TMDB sends `max-age=0` at
/// times) and never longer than six months (TMDB terms).
pub const CACHE_POLICY: CachePolicy = CachePolicy {
    min_ttl: Duration::from_mins(5),
    max_ttl: Duration::from_hours(183 * 24),
};

/// Season episodes and the providers of a region change rarely: kept a
/// day (spec 0008 slice 2).
pub const DAILY_CACHE_POLICY: CachePolicy = CachePolicy {
    min_ttl: Duration::from_hours(24),
    max_ttl: Duration::from_hours(24),
};

pub const DESCRIPTOR: AdapterDescriptor = AdapterDescriptor {
    type_id: "tmdb",
    display_name: "TMDB",
    capabilities: &[Capability::Metadata],
    tested_versions: VersionRange {
        min: API_VERSION,
        max_exclusive: Version::new(4, 0, 0, 0),
    },
    budgets: Budgets {
        // TMDB: ~40 requests per second per IP, 429 beyond.
        rate_limit: RateLimit {
            requests: 40,
            per: Duration::from_secs(1),
        },
        login_limit: None,
        expensive_endpoints: &[],
    },
    identity: IdentityRule::SingleUser,
    secret_class: SecretClass::CatalogReadToken,
    // Movies and series lists of spec 0003 D4; no music lists.
    lists: &[
        ListKind::Trending,
        ListKind::Popular,
        ListKind::Upcoming,
        ListKind::OnServices,
        ListKind::TopRated,
    ],
};

/// The TMDB adapter. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Tmdb {
    client: ServiceClient,
    cache: Option<Arc<dyn ResponseCache>>,
    /// Fixed "today" for tests; the system date otherwise.
    today: Option<NaiveDate>,
}

impl Tmdb {
    #[must_use]
    pub fn new(client: ServiceClient) -> Self {
        Self {
            client,
            cache: None,
            today: None,
        }
    }

    /// Cache responses in `cache` for their `Cache-Control` lifetime within
    /// [`CACHE_POLICY`].
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
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, AdapterError> {
        self.get_with(path, query, CACHE_POLICY).await
    }

    async fn get_with<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        policy: CachePolicy,
    ) -> Result<T, AdapterError> {
        let cache = self
            .cache
            .as_deref()
            .map(|c| (c as &dyn ResponseCache, policy));
        self.client.get_json_cached(path, query, cache).await
    }

    /// One season with its episodes (`GET /3/tv/{id}/season/{n}`), cached
    /// a day; the details of a season ref.
    async fn season(
        &self,
        item: &CatalogRef,
        series: u64,
        number: u32,
        language: &str,
    ) -> Result<CatalogDetails, AdapterError> {
        let path = format!("3/tv/{series}/season/{number}");
        let wire: wire::SeasonDetailsWire = self
            .get_with(
                &path,
                &[("language", tmdb_language(language))],
                DAILY_CACHE_POLICY,
            )
            .await?;
        let (season, overview) = wire.into_season();
        let entry = CatalogEntry {
            item: item.clone(),
            title: season.name.clone(),
            original_title: season.name.clone(),
            original_language: String::new(),
            overview,
            release_date: season.air_date,
            poster: season.poster.clone(),
            backdrop: None,
            genre_ids: Vec::new(),
            rating: None,
            vote_count: 0,
            album: None,
            scene: None,
        };
        Ok(CatalogDetails {
            entry,
            external_ids: vec![starr_core::catalog::ExternalId::Tmdb(series)],
            genres: Vec::new(),
            trailer: None,
            certifications: Vec::new(),
            watch_offers: Vec::new(),
            children: vec![Child::Season(season)],
            facts: TitleFacts::default(),
        })
    }

    /// One list page as TMDB returns it: entries of `kind` (mixed lists
    /// filtered to `kind`) and the cursor.
    async fn page(
        &self,
        path: &str,
        query: &[(&str, String)],
        page: u32,
        kind: Option<MediaKind>,
        kinds: &[MediaKind],
    ) -> Result<Page<CatalogEntry>, AdapterError> {
        let wire: PageWire = self.get(path, query).await?;
        let items = wire
            .results
            .into_iter()
            .filter_map(|item| {
                let k = kind.or_else(|| item.mixed_kind())?;
                kinds.contains(&k).then(|| item.into_entry(k))
            })
            .collect();
        Ok(Page {
            items,
            next: params::next_page(page, wire.total_pages),
        })
    }

    /// The first Trending page of `kind` as the cache holds it, without any
    /// request: `None` when nothing fresh is cached (or there is no cache).
    /// Relevance is applied as in [`Catalog::list`]; untranslated entries
    /// stay as they are. For pages anyone may see (the login backdrop,
    /// spec 0008 slice 11), which must never cost a catalog call.
    pub async fn cached_trending(
        &self,
        kind: MediaKind,
        language: &str,
        relevance: &Relevance,
    ) -> Option<Page<CatalogEntry>> {
        let cache = self.cache.as_deref()?;
        let (path, query) = params::list_request(
            &ListRequest {
                kind,
                list: ListKind::Trending,
                page: 1,
                language,
                relevance,
                filter: None,
            },
            self.today(),
        )?;
        let wire: PageWire = self
            .client
            .get_json_cache_only(&path, &query, cache)
            .await?;
        let mut ids = std::collections::HashSet::new();
        let items = wire
            .results
            .into_iter()
            .map(|item| item.into_entry(kind))
            .filter(|e| ids.insert(e.item.id.clone()) && params::is_relevant(e, relevance))
            .collect();
        Some(Page {
            items,
            next: params::next_page(1, wire.total_pages),
        })
    }

    /// Ids on the [`TRENDING_LOOKBACK`] trending pages before `page`: TMDB
    /// repeats titles across pages of one trending query, so a page drops
    /// what the pages just before it showed. Bounded, so a deep page costs
    /// a few (usually cached) requests at most; repeats from further back
    /// slip through, which is harmless.
    async fn trending_seen(
        &self,
        kind: MediaKind,
        page: u32,
        language: &str,
    ) -> Result<std::collections::HashSet<String>, AdapterError> {
        let mut seen = std::collections::HashSet::new();
        let Some(tmdb) = tmdb_kind(kind) else {
            return Ok(seen);
        };
        let path = format!("3/trending/{tmdb}/week");
        for earlier in page.saturating_sub(TRENDING_LOOKBACK).max(1)..page {
            let query = [
                ("language", tmdb_language(language)),
                ("page", earlier.to_string()),
            ];
            let wire: PageWire = self.get(&path, &query).await?;
            seen.extend(wire.results.into_iter().map(|i| i.id.to_string()));
        }
        Ok(seen)
    }

    /// Fill untranslated entries from the same request in English (D5).
    /// `kind` as for [`Tmdb::page`].
    async fn with_fallback(
        &self,
        mut page: Page<CatalogEntry>,
        path: &str,
        query: &[(&str, String)],
        kind: Option<MediaKind>,
        language: &str,
    ) -> Result<Page<CatalogEntry>, AdapterError> {
        if is_english(language) || !page.items.iter().any(params::needs_fallback) {
            return Ok(page);
        }
        let english_query: Vec<(&str, String)> = query
            .iter()
            .map(|(k, v)| {
                if *k == "language" {
                    (*k, FALLBACK_LANGUAGE.to_owned())
                } else {
                    (*k, v.clone())
                }
            })
            .collect();
        let english: PageWire = self.get(path, &english_query).await?;
        let english = english
            .results
            .into_iter()
            .filter_map(|item| {
                let kind = kind.or_else(|| item.mixed_kind())?;
                Some(item.into_entry(kind))
            })
            .collect();
        params::apply_fallbacks(&mut page.items, english, language);
        Ok(page)
    }

    fn parse_id(item: &CatalogRef) -> Result<u64, AdapterError> {
        item.id
            .parse()
            .map_err(|_| AdapterError::NotFound(format!("not a TMDB id: {:?}", item.id)))
    }
}

impl Probe for Tmdb {
    fn descriptor(&self) -> &'static AdapterDescriptor {
        &DESCRIPTOR
    }

    async fn probe(&self) -> Result<ProbeReport, AdapterError> {
        // Never cached: the probe must prove the token now.
        let auth: wire::AuthenticationWire = self.client.get_json(PROBE_PATH).await?;
        if !auth.success {
            return Err(AdapterError::CredentialsInvalid);
        }
        Ok(ProbeReport {
            version: API_VERSION,
        })
    }
}

impl Catalog for Tmdb {
    async fn configuration(&self) -> Result<CatalogConfiguration, AdapterError> {
        let config: wire::ConfigurationWire = self.get("3/configuration", &[]).await?;
        let languages: Vec<wire::LanguageWire> = self.get("3/configuration/languages", &[]).await?;
        let countries: Vec<wire::RegionWire> = self.get("3/configuration/countries", &[]).await?;
        let regions: wire::Results<Vec<wire::RegionWire>> =
            self.get("3/watch/providers/regions", &[]).await?;
        let mut genres = Vec::new();
        for (kind, tmdb) in params::KINDS
            .into_iter()
            .filter_map(|k| Some((k, tmdb_kind(k)?)))
        {
            let list: wire::GenreListWire = self
                .get(
                    &format!("3/genre/{tmdb}/list"),
                    &[("language", FALLBACK_LANGUAGE.to_owned())],
                )
                .await?;
            genres.extend(list.into_genres(kind));
        }
        let images = config.images;
        Ok(CatalogConfiguration {
            image_sizes: ImageSizes {
                poster: images.poster_sizes,
                backdrop: images.backdrop_sizes,
                logo: images.logo_sizes,
                profile: images.profile_sizes,
                still: images.still_sizes,
            },
            languages: languages.into_iter().map(Into::into).collect(),
            countries: countries.into_iter().map(Into::into).collect(),
            provider_regions: regions.results.into_iter().map(Into::into).collect(),
            genres,
        })
    }

    async fn providers(
        &self,
        kind: MediaKind,
        region: &str,
    ) -> Result<Vec<WatchProvider>, AdapterError> {
        let Some(tmdb) = tmdb_kind(kind) else {
            return Ok(Vec::new());
        };
        let list: wire::Results<Vec<wire::ProviderWire>> = self
            .get_with(
                &format!("3/watch/providers/{tmdb}"),
                &[("watch_region", region.to_owned())],
                DAILY_CACHE_POLICY,
            )
            .await?;
        Ok(wire::providers_in_order(list.results, region))
    }

    async fn search(&self, query: &SearchQuery<'_>) -> Result<Page<CatalogEntry>, AdapterError> {
        let text = query.text.trim();
        if text.is_empty() || query.kinds.is_empty() || !params::page_in_range(query.page) {
            return Ok(Page::end());
        }
        // One cursor over movies and series; people are dropped (D12).
        let path = "3/search/multi";
        let params = [
            ("query", text.to_owned()),
            ("page", query.page.to_string()),
            ("language", tmdb_language(query.language)),
            ("include_adult", "false".to_owned()),
        ];
        let page = self
            .page(path, &params, query.page, None, query.kinds)
            .await?;
        self.with_fallback(page, path, &params, None, query.language)
            .await
    }

    async fn list(&self, query: &ListQuery<'_>) -> Result<Page<CatalogEntry>, AdapterError> {
        if !params::page_in_range(query.page) {
            return Ok(Page::end());
        }
        let filter = query.filter.as_ref();
        let Some((path, params)) = params::list_request(
            &ListRequest {
                kind: query.kind,
                list: query.list,
                page: query.page,
                language: query.language,
                relevance: query.relevance,
                filter,
            },
            self.today(),
        ) else {
            return Ok(Page::end());
        };
        let mut page = self
            .page(&path, &params, query.page, Some(query.kind), &[query.kind])
            .await?;
        if query.list == ListKind::Trending && query.page > 1 {
            let seen = self
                .trending_seen(query.kind, query.page, query.language)
                .await?;
            page.items.retain(|e| !seen.contains(&e.item.id));
        }
        // Within one page, too.
        let mut ids = std::collections::HashSet::new();
        page.items.retain(|e| ids.insert(e.item.id.clone()));
        page.items.retain(|e| {
            params::is_relevant(e, query.relevance) && params::passes_filter(e, filter)
        });
        self.with_fallback(page, &path, &params, Some(query.kind), query.language)
            .await
    }

    async fn details(
        &self,
        item: &CatalogRef,
        language: &str,
    ) -> Result<CatalogDetails, AdapterError> {
        if let Some((series, number)) = item.season_parts() {
            let series = Self::parse_id(&CatalogRef {
                kind: MediaKind::Series,
                id: series.to_owned(),
            })?;
            return self.season(item, series, number, language).await;
        }
        let id = Self::parse_id(item)?;
        let kind = tmdb_kind(item.kind).ok_or_else(|| {
            AdapterError::NotFound(format!("not a TMDB kind: {}", item.kind.as_str()))
        })?;
        let path = format!("3/{kind}/{id}");
        let series = item.kind == MediaKind::Series;
        // Cast and crew come with the same request (spec 0008 Q15 A).
        let (ratings, credits) = if series {
            ("content_ratings", "aggregate_credits")
        } else {
            ("release_dates", "credits")
        };
        let tmdb_lang = tmdb_language(language);
        // Videos follow `language` strictly; add English (and
        // language-less) trailers as fallback.
        let video_lang = match tmdb_lang.split('-').next().unwrap_or("en") {
            "en" => "en,null".to_owned(),
            other => format!("{other},en,null"),
        };
        let query = [
            ("language", tmdb_lang),
            (
                "append_to_response",
                format!("external_ids,videos,{ratings},watch/providers,{credits}"),
            ),
            ("include_video_language", video_lang),
        ];
        let mut wire: DetailsWire = self.get(&path, &query).await?;
        let mut facts = wire.take_facts(series);
        let mut entry = wire.item.into_entry(item.kind);
        let genres: Vec<_> = wire
            .genres
            .into_iter()
            .map(|g| starr_core::catalog::Genre {
                kind: item.kind,
                id: g.id,
                name: g.name,
            })
            .collect();
        entry.genre_ids = genres.iter().map(|g| g.id).collect();
        if !is_english(language) && params::needs_fallback(&entry) {
            let english: ItemWire = self
                .get(&path, &[("language", FALLBACK_LANGUAGE.to_owned())])
                .await?;
            params::apply_fallback(&mut entry, &english.into_entry(item.kind), language);
        }
        let certifications = if series {
            wire::series_certifications(wire.content_ratings.map(|r| r.results).unwrap_or_default())
        } else {
            let releases = wire.release_dates.map(|r| r.results).unwrap_or_default();
            facts.digital_releases = wire::digital_releases(&releases);
            wire::movie_certifications(&releases)
        };
        Ok(CatalogDetails {
            entry,
            external_ids: wire::external_ids(id, wire.external_ids),
            genres,
            trailer: wire.videos.and_then(|v| wire::trailer(v.results)),
            certifications,
            watch_offers: wire::watch_offers(
                wire.watch_providers.map(|w| w.results).unwrap_or_default(),
            ),
            children: if series {
                wire::seasons(wire.seasons)
                    .into_iter()
                    .map(Child::Season)
                    .collect()
            } else {
                Vec::new()
            },
            facts,
        })
    }
}
