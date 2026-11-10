//! `StashDB` catalog adapter (ADR 0014, spec 0007 slice 2): [`Probe`] and
//! [`Catalog`] over stash-box GraphQL for the private category: search
//! scenes, performers and studios, scene details, performer and studio
//! pages with their scenes, and the New and Trending lanes; plus the image
//! fetch for Starr's adult image proxy ([`StashDb::image`]).
//!
//! The key is a catalog read token (ADR 0003 amendment 1) accepted only
//! for an account whose roles go no further than `READ`/`VOTE`
//! ([`read_only`], spec 0007 Q14): the probe refuses anything else. Each
//! operator brings their own account and key; Starr ships none.
//!
//! Budget (spec 0007 Q1): at most 2 requests per second per host, split
//! web 70 % / worker 30 % in split mode, 429 and 503 pause every caller.
//! The client is a private-content client: logs show neither paths nor
//! the service's words, error details carry no body, and cache keys are
//! hashes under [`CACHE_PREFIX`] (so member search text and titles are
//! never stored as keys, and the adult rows can be purged as a group).
//!
//! Facts and their evidence: `docs/concept/capabilities.md`, "`StashDB`".

mod wire;

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use starr_adapter_kit::{
    AuthScheme, BinaryResponse, CachePolicy, ClientOptions, ResponseCache, ServiceClient,
};
use starr_core::adapters::{
    AdapterDescriptor, AdapterError, Budgets, Capability, IdentityRule, Probe, ProbeReport,
    RateLimit, SecretClass, Version, VersionRange,
};
use starr_core::catalog::{
    Catalog, CatalogConfiguration, CatalogDetails, CatalogEntry, CatalogRef, Child, ExternalId,
    ListKind, ListQuery, MediaKind, Page, SearchQuery, WatchProvider,
};
use url::Url;
use uuid::Uuid;

pub use wire::image_ref;

/// The key travels in the `ApiKey` header.
pub const AUTH: AuthScheme = AuthScheme::ApiKeyHeader("ApiKey");

/// Base URL operators enter for `StashDB` (another stash-box works too).
pub const API_BASE: &str = "https://stashdb.org";

/// The GraphQL endpoint relative to the base URL.
const GRAPHQL: &str = "graphql";

/// Prefix of every `catalog_cache` key this adapter writes (spec 0007
/// "Catalog cache"): retention purges the adult rows by it.
pub const CACHE_PREFIX: &str = "adult:";

/// Results per search or lane page.
pub const PAGE_SIZE: u32 = 25;

/// Pages a search or lane offers at most.
pub const PAGE_CAP: u32 = 20;

/// How many of each kind a search found in all, by stash-box's own count
/// (spec 0008 PRIV-14).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchCounts {
    pub scenes: u64,
    pub performers: u64,
    pub studios: u64,
}

/// One page of one kind's search: entries, stash-box's total, more pages.
type Counted = (Vec<CatalogEntry>, u64, bool);

/// Scenes per page of a performer or studio page, and pages read at most
/// (ADR 0013's discography rule: 3 pages of 100).
pub const SCENE_PAGE_SIZE: u32 = 100;
pub const SCENE_PAGES: u32 = 3;

/// Longest a request waits for its turn in the budget before it fails as
/// busy.
pub const MAX_WAIT: Duration = Duration::from_secs(10);

/// The sizes stash-box resizes images to (`internal/api/routes_image.go`
/// `allowedSizes`).
pub const IMAGE_SIZES: &[&str] = &["300", "600", "1280"];

/// Largest image accepted.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// The roles a key's account may have (spec 0007 Q14): reading and voting
/// cannot change the catalog.
pub const READ_ONLY_ROLES: [&str; 2] = ["READ", "VOTE"];

/// At most two requests per second per host (spec 0007 Q1).
pub const RATE_LIMIT: RateLimit = RateLimit {
    requests: 2,
    per: Duration::from_secs(1),
};

/// Cache lifetimes (spec 0007 Q1): search 1 h, scene details 7 days,
/// performer and studio scene lists 1 day; the lanes 1 h.
pub const SEARCH_CACHE: CachePolicy = fixed(Duration::from_hours(1));
pub const DETAILS_CACHE: CachePolicy = fixed(Duration::from_hours(7 * 24));
pub const SCENE_LIST_CACHE: CachePolicy = fixed(Duration::from_hours(24));
pub const LANE_CACHE: CachePolicy = fixed(Duration::from_hours(1));

const fn fixed(ttl: Duration) -> CachePolicy {
    CachePolicy {
        min_ttl: ttl,
        max_ttl: ttl,
    }
}

pub const DESCRIPTOR: AdapterDescriptor = AdapterDescriptor {
    type_id: "stashdb",
    display_name: "StashDB",
    capabilities: &[Capability::Metadata],
    // stash-box 0.10.x was current when the adapter was written; the
    // schema parts Starr reads have been stable across 0.x.
    tested_versions: VersionRange {
        min: Version::new(0, 10, 0, 0),
        max_exclusive: Version::new(1, 0, 0, 0),
    },
    budgets: Budgets {
        rate_limit: RATE_LIMIT,
        login_limit: None,
        expensive_endpoints: &[],
    },
    identity: IdentityRule::SingleUser,
    // Catalog read token (spec 0007 Q14 A), read-only accounts only.
    secret_class: SecretClass::CatalogReadToken,
    lists: &[ListKind::New, ListKind::Trending],
};

/// Client options in a process holding `percent` of the host's budget
/// (100 alone; web 70, worker 30 when split). Private content: no paths,
/// bodies or the service's words in logs or errors.
#[must_use]
pub fn client_options(percent: u32) -> ClientOptions {
    ClientOptions {
        rate_limit: starr_adapter_kit::share_of(RATE_LIMIT, percent),
        unavailable_is_throttle: true,
        max_wait: Some(MAX_WAIT),
        private_content: true,
    }
}

/// Whether an account with `roles` can only read (and vote): at least
/// `READ`, nothing beyond `READ`/`VOTE`. Unknown roles count as more.
#[must_use]
pub fn read_only(roles: &[String]) -> bool {
    roles.iter().any(|r| r == "READ") && roles.iter().all(|r| READ_ONLY_ROLES.contains(&r.as_str()))
}

/// What the probe tells admins when a key could write (spec 0007 Q14).
pub const NOT_READ_ONLY: &str =
    "the StashDB account can do more than read; use a read-only StashDB account";

/// A stash-box version (`v0.10.3`, `0.10.3-12-gabcdef`), without its
/// prefix and build suffix.
#[must_use]
pub fn parse_version(raw: &str) -> Option<Version> {
    let raw = raw.trim();
    let raw = raw.strip_prefix('v').unwrap_or(raw);
    let core: String = raw
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    core.trim_end_matches('.').parse().ok()
}

/// Whether `size` is one of [`IMAGE_SIZES`].
#[must_use]
pub fn valid_image_size(size: &str) -> bool {
    IMAGE_SIZES.contains(&size)
}

/// Whether `reference` is an image id in canonical spelling; nothing else
/// can pass, so a validated reference cannot address another path.
#[must_use]
pub fn valid_image_ref(reference: &str) -> bool {
    wire::uuid(reference).is_some()
}

/// The `StashDB` adapter. Cheap to clone.
#[derive(Debug, Clone)]
pub struct StashDb {
    client: ServiceClient,
    cache: Option<Arc<dyn ResponseCache>>,
}

/// What the key's account may do, as the probe saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCheck {
    pub version: Version,
    pub roles: Vec<String>,
}

impl KeyCheck {
    /// The key may be used: its account only reads.
    #[must_use]
    pub fn read_only(&self) -> bool {
        read_only(&self.roles)
    }
}

impl StashDb {
    #[must_use]
    pub fn new(client: ServiceClient) -> Self {
        Self {
            client,
            cache: None,
        }
    }

    /// Cache responses in `cache` (keys under [`CACHE_PREFIX`]).
    #[must_use]
    pub fn with_cache(mut self, cache: Arc<dyn ResponseCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// One GraphQL query; `policy` caches it under a hash of query and
    /// variables (never cached without).
    async fn query<T: DeserializeOwned>(
        &self,
        document: &str,
        variables: Value,
        policy: Option<CachePolicy>,
    ) -> Result<T, AdapterError> {
        let body = json!({ "query": document, "variables": variables });
        let key = cache_key(&body);
        let cache = match (self.cache.as_deref(), policy) {
            (Some(cache), Some(policy)) => Some((cache, policy, key.as_str())),
            _ => None,
        };
        let envelope: wire::Envelope<T> = self
            .client
            .post_json_cached(GRAPHQL, &body, cache, |e: &wire::Envelope<T>| {
                e.errors.is_empty() && e.data.is_some()
            })
            .await?;
        if let Some(error) = envelope.errors.first() {
            // The service's words are never quoted: they can name content.
            let message = error.message.to_ascii_lowercase();
            if ["unauthorized", "not authorized", "forbidden", "api key"]
                .iter()
                .any(|m| message.contains(m))
            {
                return Err(AdapterError::CredentialsInvalid);
            }
            return Err(AdapterError::Unknown(
                "the catalog answered with a GraphQL error".to_owned(),
            ));
        }
        envelope
            .data
            .ok_or_else(|| AdapterError::Unknown("the catalog answered without data".to_owned()))
    }

    /// Version and roles of the key's account (proves the key).
    pub async fn check_key(&self) -> Result<KeyCheck, AdapterError> {
        let data: wire::ProbeData = self.query(wire::PROBE, json!({}), None).await?;
        let version = parse_version(&data.version.version).ok_or_else(|| {
            AdapterError::Unknown("the catalog reported an unreadable version".to_owned())
        })?;
        let me = data.me.ok_or(AdapterError::CredentialsInvalid)?;
        Ok(KeyCheck {
            version,
            roles: me.roles.unwrap_or_default(),
        })
    }

    async fn scenes(
        &self,
        input: Value,
        policy: CachePolicy,
    ) -> Result<wire::ScenesPage, AdapterError> {
        let data: wire::QueryScenesData = self
            .query(
                &wire::query_scenes(),
                json!({ "input": input }),
                Some(policy),
            )
            .await?;
        Ok(data.query_scenes)
    }

    /// A lane: scenes by date or trend, one page.
    async fn lane(&self, sort: &str, page: u32) -> Result<Page<CatalogEntry>, AdapterError> {
        let found = self
            .scenes(
                json!({ "sort": sort, "direction": "DESC", "page": page, "per_page": PAGE_SIZE }),
                LANE_CACHE,
            )
            .await?;
        Ok(page_of(page, found.count, scene_entries(found.scenes)))
    }

    async fn search_scenes(&self, text: &str, page: u32) -> Result<Counted, AdapterError> {
        let data: wire::SearchScenesData = self
            .query(
                &wire::search_scenes(),
                json!({ "term": text, "page": page, "perPage": PAGE_SIZE }),
                Some(SEARCH_CACHE),
            )
            .await?;
        let found = data.search_scenes;
        let more = wire::has_more(offset(page), found.scenes.len(), found.count);
        Ok((scene_entries(found.scenes), found.count, more))
    }

    async fn search_performers(&self, text: &str, page: u32) -> Result<Counted, AdapterError> {
        let data: wire::SearchPerformersData = self
            .query(
                &wire::search_performers(),
                json!({ "term": text, "page": page, "perPage": PAGE_SIZE }),
                Some(SEARCH_CACHE),
            )
            .await?;
        let found = data.search_performers;
        let more = wire::has_more(offset(page), found.performers.len(), found.count);
        Ok((
            entity_entries(found.performers, MediaKind::Performer),
            found.count,
            more,
        ))
    }

    async fn search_studios(&self, text: &str, page: u32) -> Result<Counted, AdapterError> {
        let data: wire::QueryStudiosData = self
            .query(
                &wire::query_studios(),
                json!({ "input": { "name": text, "page": page, "per_page": PAGE_SIZE } }),
                Some(SEARCH_CACHE),
            )
            .await?;
        let found = data.query_studios;
        let more = wire::has_more(offset(page), found.studios.len(), found.count);
        Ok((
            entity_entries(found.studios, MediaKind::Studio),
            found.count,
            more,
        ))
    }

    async fn scene(&self, id: Uuid) -> Result<CatalogDetails, AdapterError> {
        let data: wire::FindSceneData = self
            .query(
                &wire::find_scene(),
                json!({ "id": wire::uuid_text(id) }),
                Some(DETAILS_CACHE),
            )
            .await?;
        let entry = data
            .find_scene
            .and_then(wire::SceneWire::into_entry)
            .ok_or_else(not_found)?;
        let mut external_ids = vec![ExternalId::StashScene(id)];
        if let Some(facts) = &entry.scene {
            external_ids.extend(
                facts
                    .studio
                    .iter()
                    .filter_map(|s| wire::uuid(&s.item.id))
                    .map(ExternalId::StashStudio),
            );
            external_ids.extend(
                facts
                    .performers
                    .iter()
                    .filter_map(|p| wire::uuid(&p.item.id))
                    .map(ExternalId::StashPerformer),
            );
        }
        Ok(details(entry, external_ids, Vec::new()))
    }

    /// One page of a performer's (`filter` = `performers`) or studio's
    /// (`studios`) scenes, newest first, cached a day. The studio page and
    /// "More from the studio" send the same first page, so they share one
    /// cached answer.
    async fn entity_scenes(
        &self,
        filter: &str,
        id: &str,
        page: u32,
    ) -> Result<wire::ScenesPage, AdapterError> {
        let mut input = json!({
            "sort": "DATE",
            "direction": "DESC",
            "page": page,
            "per_page": SCENE_PAGE_SIZE,
        });
        input[filter] = json!({ "value": [id], "modifier": "INCLUDES" });
        self.scenes(input, SCENE_LIST_CACHE).await
    }

    /// The newest scenes of a studio: the first page of its studio page
    /// (spec 0008 "More from \<studio\>"; no request when that page is
    /// cached).
    pub async fn studio_scenes(
        &self,
        studio: &CatalogRef,
    ) -> Result<Vec<CatalogEntry>, AdapterError> {
        if studio.kind != MediaKind::Studio {
            return Err(not_found());
        }
        let id = wire::uuid(&studio.id).ok_or_else(not_found)?;
        let found = self
            .entity_scenes("studios", &wire::uuid_text(id), 1)
            .await?;
        Ok(scene_entries(found.scenes))
    }

    /// The search of the private area with the total of each kind (spec
    /// 0008 PRIV-14: counts on the group headers): scenes, then performers,
    /// then studios, one query per requested kind and page. The totals are
    /// stash-box's own `count` of each query, so they cost nothing extra.
    pub async fn search_counted(
        &self,
        query: &SearchQuery<'_>,
    ) -> Result<(Page<CatalogEntry>, SearchCounts), AdapterError> {
        let text = query.text.trim();
        let mut counts = SearchCounts::default();
        if text.is_empty() || !(1..=PAGE_CAP).contains(&query.page) {
            return Ok((Page::end(), counts));
        }
        let mut items = Vec::new();
        let mut more = false;
        for kind in [MediaKind::Scene, MediaKind::Performer, MediaKind::Studio] {
            if !query.kinds.contains(&kind) {
                continue;
            }
            let (found, count, has_more) = match kind {
                MediaKind::Scene => self.search_scenes(text, query.page).await?,
                MediaKind::Performer => self.search_performers(text, query.page).await?,
                _ => self.search_studios(text, query.page).await?,
            };
            match kind {
                MediaKind::Scene => counts.scenes = count,
                MediaKind::Performer => counts.performers = count,
                _ => counts.studios = count,
            }
            items.extend(found);
            more |= has_more;
        }
        Ok((
            Page {
                items,
                next: next_page(query.page, more),
            },
            counts,
        ))
    }

    /// Scenes matching `text` of one studio or performer (the private
    /// search's entity filter, spec 0008 Q7 C): `queryScenes` with the
    /// text and the entity, one page, cached like a search. stash-box
    /// combines text and a studio as AND (verified 2026-11-07,
    /// capabilities.md); for performers that is assumed, so the answer is
    /// still narrowed to the entity here, which costs nothing when the
    /// server already did it.
    pub async fn search_scenes_of(
        &self,
        text: &str,
        entity: &CatalogRef,
        page: u32,
    ) -> Result<(Page<CatalogEntry>, u64), AdapterError> {
        let text = text.trim();
        if text.is_empty() || !(1..=PAGE_CAP).contains(&page) {
            return Ok((Page::end(), 0));
        }
        let field = match entity.kind {
            MediaKind::Studio => "studios",
            MediaKind::Performer => "performers",
            _ => return Err(not_found()),
        };
        let id = wire::uuid_text(wire::uuid(&entity.id).ok_or_else(not_found)?);
        let mut input = json!({ "text": text, "page": page, "per_page": PAGE_SIZE });
        input[field] = json!({ "value": [id], "modifier": "INCLUDES" });
        let found = self.scenes(input, SEARCH_CACHE).await?;
        let count = found.count;
        let len = found.scenes.len();
        let items = scene_entries(found.scenes)
            .into_iter()
            .filter(|e| names_entity(e, entity))
            .collect();
        Ok((
            Page {
                items,
                next: next_page(page, wire::has_more(offset(page), len, count)),
            },
            count,
        ))
    }

    /// A performer or studio with its scenes, newest first (at most
    /// [`SCENE_PAGES`] pages of [`SCENE_PAGE_SIZE`]).
    async fn entity(&self, kind: MediaKind, id: Uuid) -> Result<CatalogDetails, AdapterError> {
        let text = wire::uuid_text(id);
        let (entry, filter, external) = if kind == MediaKind::Performer {
            let data: wire::FindPerformerData = self
                .query(
                    &wire::find_performer(),
                    json!({ "id": text }),
                    Some(DETAILS_CACHE),
                )
                .await?;
            (
                data.find_performer,
                "performers",
                ExternalId::StashPerformer(id),
            )
        } else {
            let data: wire::FindStudioData = self
                .query(
                    &wire::find_studio(),
                    json!({ "id": text }),
                    Some(DETAILS_CACHE),
                )
                .await?;
            (data.find_studio, "studios", ExternalId::StashStudio(id))
        };
        let entry = entry
            .and_then(|e| e.into_entry(kind))
            .ok_or_else(not_found)?;
        let mut scenes = Vec::new();
        for page in 1..=SCENE_PAGES {
            let found = self.entity_scenes(filter, &text, page).await?;
            let len = found.scenes.len();
            scenes.extend(scene_entries(found.scenes));
            let start = u64::from((page - 1) * SCENE_PAGE_SIZE);
            if !wire::has_more(start, len, found.count) {
                break;
            }
        }
        Ok(details(
            entry,
            vec![external],
            scenes
                .into_iter()
                .map(|s| Child::Scene(Box::new(s)))
                .collect(),
        ))
    }

    /// An image by id in one of [`IMAGE_SIZES`], from the instance's own
    /// `/images/{id}` route (stash-box `routes_image.go`), **without the
    /// key**: redirects are followed only over https to the instance's
    /// host or its subdomains (its CDN), or to the base URL's own origin.
    /// Invalid sizes or references are rejected without a request.
    pub async fn image(&self, size: &str, reference: &str) -> Result<BinaryResponse, AdapterError> {
        if !valid_image_size(size) || !valid_image_ref(reference) {
            return Err(AdapterError::Rejected("invalid image path".to_owned()));
        }
        let base = self
            .client
            .base_url()
            .ok_or_else(|| AdapterError::Unknown("the catalog has no base URL".to_owned()))?
            .clone();
        self.client
            .get_bytes_anonymous(
                &format!("images/{reference}"),
                &[("size", size.to_owned())],
                Some("image/*"),
                MAX_IMAGE_BYTES,
                |url| image_host(&base, url),
            )
            .await
    }
}

/// Whether an image redirect may go to `url`: the base URL's own origin,
/// or https on the base's host or one of its subdomains (`cdn.stashdb.org`
/// for `stashdb.org`), never with credentials in the URL.
#[must_use]
pub fn image_host(base: &Url, url: &Url) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if url.origin() == base.origin() {
        return true;
    }
    let (Some(host), Some(base_host)) = (url.host_str(), base.host_str()) else {
        return false;
    };
    let base_host = base_host.strip_prefix("www.").unwrap_or(base_host);
    url.scheme() == "https" && (host == base_host || host.ends_with(&format!(".{base_host}")))
}

/// The cache key of a query: [`CACHE_PREFIX`] and the SHA-256 of the
/// request body.
fn cache_key(body: &Value) -> String {
    let digest = Sha256::digest(body.to_string().as_bytes());
    digest
        .iter()
        .fold(String::from(CACHE_PREFIX), |mut key, b| {
            let _ = write!(key, "{b:02x}");
            key
        })
}

/// Neutral: neither the id nor the kind of what was not found.
fn not_found() -> AdapterError {
    AdapterError::NotFound("no such title in the catalog".to_owned())
}

fn offset(page: u32) -> u64 {
    u64::from(page.saturating_sub(1)) * u64::from(PAGE_SIZE)
}

fn page_of(page: u32, count: u64, items: Vec<CatalogEntry>) -> Page<CatalogEntry> {
    let more = wire::has_more(offset(page), items.len(), count);
    Page {
        items,
        next: next_page(page, more),
    }
}

fn next_page(page: u32, more: bool) -> Option<u32> {
    (more && page < PAGE_CAP).then_some(page + 1)
}

/// Whether a scene names `entity` as its studio or among its performers.
fn names_entity(scene: &CatalogEntry, entity: &CatalogRef) -> bool {
    scene.scene.as_ref().is_some_and(|facts| {
        facts.studio.iter().any(|s| s.item == *entity)
            || facts.performers.iter().any(|p| p.item == *entity)
    })
}

fn scene_entries(scenes: Vec<wire::SceneWire>) -> Vec<CatalogEntry> {
    scenes
        .into_iter()
        .filter_map(wire::SceneWire::into_entry)
        .collect()
}

fn entity_entries(entities: Vec<wire::EntityWire>, kind: MediaKind) -> Vec<CatalogEntry> {
    entities
        .into_iter()
        .filter_map(|e| e.into_entry(kind))
        .collect()
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

impl Probe for StashDb {
    fn descriptor(&self) -> &'static AdapterDescriptor {
        &DESCRIPTOR
    }

    /// Proves the key, reads the version and refuses a key whose account
    /// can do more than read (spec 0007 Q14, checked by every probe).
    async fn probe(&self) -> Result<ProbeReport, AdapterError> {
        let check = self.check_key().await?;
        if !check.read_only() {
            return Err(AdapterError::Rejected(NOT_READ_ONLY.to_owned()));
        }
        DESCRIPTOR.check_version(check.version)?;
        Ok(ProbeReport {
            version: check.version,
        })
    }
}

impl Catalog for StashDb {
    /// No languages, regions, providers or genres to configure.
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

    /// Scenes, then performers, then studios: one query per requested kind
    /// and page.
    async fn search(&self, query: &SearchQuery<'_>) -> Result<Page<CatalogEntry>, AdapterError> {
        Ok(self.search_counted(query).await?.0)
    }

    /// The scene lanes: New (by release date) and Trending (stash-box's
    /// own `TRENDING` sort). The relevance has nothing to filter here.
    async fn list(&self, query: &ListQuery<'_>) -> Result<Page<CatalogEntry>, AdapterError> {
        // The lanes take no filters.
        if query.kind != MediaKind::Scene
            || !(1..=PAGE_CAP).contains(&query.page)
            || query.filter.is_some()
        {
            return Ok(Page::end());
        }
        match query.list {
            ListKind::New => self.lane("DATE", query.page).await,
            ListKind::Trending => self.lane("TRENDING", query.page).await,
            ListKind::Popular
            | ListKind::Upcoming
            | ListKind::OnServices
            | ListKind::TopRated
            | ListKind::FreshReleases
            | ListKind::OnTheAir => Ok(Page::end()),
        }
    }

    async fn details(
        &self,
        item: &CatalogRef,
        _language: &str,
    ) -> Result<CatalogDetails, AdapterError> {
        let id = wire::uuid(&item.id).ok_or_else(not_found)?;
        match item.kind {
            MediaKind::Scene => self.scene(id).await,
            MediaKind::Performer | MediaKind::Studio => self.entity(item.kind, id).await,
            MediaKind::Movie | MediaKind::Series | MediaKind::Artist | MediaKind::Album => {
                Err(not_found())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec 0007 Q13: cached adult answers are gone 30 days after their
    /// last use at the latest. Rows are never refreshed by a read, and
    /// housekeeping deletes them hourly once expired, so every lifetime
    /// must stay well within that.
    #[test]
    fn cached_answers_never_outlive_the_retention() {
        let retention = Duration::from_hours(30 * 24);
        for policy in [SEARCH_CACHE, DETAILS_CACHE, SCENE_LIST_CACHE, LANE_CACHE] {
            assert!(policy.max_ttl + Duration::from_hours(1) < retention);
        }
    }

    #[test]
    fn only_read_and_vote_pass() {
        let roles = |r: &[&str]| r.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(read_only(&roles(&["READ"])));
        assert!(read_only(&roles(&["READ", "VOTE"])));
        for bad in [
            &["READ", "EDIT"][..],
            &["READ", "MODIFY"],
            &["READ", "ADMIN"],
            &["READ", "BOT"],
            &["READ", "SOMETHING_NEW"],
            &["VOTE"],
            &[],
        ] {
            assert!(!read_only(&roles(bad)), "{bad:?}");
        }
    }

    #[test]
    fn versions_lose_prefix_and_build_suffix() {
        assert_eq!(parse_version("v0.10.3"), Some(Version::new(0, 10, 3, 0)));
        assert_eq!(
            parse_version("0.10.3-12-gabcdef"),
            Some(Version::new(0, 10, 3, 0))
        );
        assert_eq!(parse_version("dev"), None);
    }

    #[test]
    fn image_redirects_stay_with_the_instance() {
        let base = Url::parse("https://stashdb.org/").unwrap();
        let ok = |u: &str| image_host(&base, &Url::parse(u).unwrap());
        assert!(ok("https://cdn.stashdb.org/images/ab/cd/x"));
        assert!(ok("https://stashdb.org/images/x"));
        assert!(!ok("http://cdn.stashdb.org/images/x"));
        assert!(!ok("https://stashdb.org.evil.example/x"));
        assert!(!ok("https://evilstashdb.org/x"));
        assert!(!ok("https://u:p@cdn.stashdb.org/x"));
        let local = Url::parse("http://127.0.0.1:9000/").unwrap();
        assert!(image_host(
            &local,
            &Url::parse("http://127.0.0.1:9000/cdn/x").unwrap()
        ));
        assert!(!image_host(
            &local,
            &Url::parse("http://127.0.0.1:9001/x").unwrap()
        ));
    }

    #[test]
    fn cache_keys_are_prefixed_hashes() {
        let key = cache_key(&json!({ "query": "q", "variables": { "term": "Scene A" } }));
        assert!(key.starts_with(CACHE_PREFIX));
        assert_eq!(key.len(), CACHE_PREFIX.len() + 64);
        assert!(!key.contains("Scene"));
    }
}
