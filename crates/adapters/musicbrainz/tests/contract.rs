//! `MusicBrainz` adapter against `wiremock` fakes replaying `fixtures/`
//! (lesson 10). The fixtures are hand-written from the shapes in the
//! official documentation, **not recorded**: Starr's tests never call
//! `MusicBrainz`, `ListenBrainz` or the Cover Art Archive.
//!
//! - `fixtures/ws2/`: `/ws/2` JSON as in the `MusicBrainz` API docs
//!   ("Search", "Examples"): hyphenated keys, `score` as a string,
//!   `artist-credit` with `name`/`joinphrase`/`artist`. The browse keys
//!   `release-group-count`/`release-group-offset` are inferred from the
//!   documented release browse; the 404 body is undocumented, so the
//!   adapter decides on the status alone.
//! - `fixtures/listenbrainz/`: sitewide release-group statistics (204 when
//!   not computed yet) and fresh releases. The docs show the release
//!   objects without a wrapper; `payload.releases` is an assumption to
//!   verify with the first recording (spec 0006 open verifications).
//! - The Cover Art Archive is faked inline (307 to the archive, 404 when no
//!   front image was chosen).
//!
//! Every mock requires Starr's exact User-Agent, so a request without it
//! fails the test.
#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use secrecy::SecretString;
use starr_adapter_kit::{ClientOptions, Endpoint, ResponseCache, ServiceClient};
use starr_adapter_musicbrainz::covers::CoverArchive;
use starr_adapter_musicbrainz::{
    AUTH, DESCRIPTOR, GENRE_LANE_CACHE, GENRES, MusicBrainz, RATE_LIMIT, client_options,
    listenbrainz_client,
};
use starr_core::adapters::{AdapterError, Probe, RateLimit, Version};
use starr_core::catalog::{
    Catalog, CatalogRef, Child, ExternalId, GenreRef, ListFilter, ListKind, ListQuery, MediaKind,
    Page, ReleaseType, Relevance, SearchQuery, SecondaryType,
};
use url::Url;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const USER_AGENT: &str = concat!(
    "Starr/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/bitoxo/starr )"
);
const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";
const OK_COMPUTER_RELEASE: &str = "0b6b4ba0-d36f-47bd-b4ea-6a5b91842d29";

/// Fast enough that functional tests do not wait; the real budget is
/// tested on its own below.
const FAST: RateLimit = RateLimit {
    requests: 100,
    per: Duration::from_secs(1),
};

fn fixture(name: &str) -> String {
    let file = format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file}: {e}"))
}

fn json(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "application/json; charset=utf-8")
        .set_body_string(fixture(name))
}

fn mb_client(uri: &str, options: ClientOptions) -> ServiceClient {
    ServiceClient::with_options(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(uri).unwrap(),
            auth: AUTH,
            credential: SecretString::from(String::new()),
        },
        options,
    )
    .unwrap()
}

fn fast() -> ClientOptions {
    ClientOptions {
        rate_limit: FAST,
        unavailable_is_throttle: true,
        max_wait: None,
        private_content: false,
    }
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 3).unwrap()
}

fn adapter(mb: &MockServer, lb: &MockServer) -> MusicBrainz {
    MusicBrainz::new(mb_client(&mb.uri(), fast()))
        .with_listenbrainz(
            ServiceClient::public_with("listenbrainz", fast(), Url::parse(&lb.uri()).unwrap())
                .unwrap(),
        )
        .with_today(today())
}

/// `GET <path>` with Starr's User-Agent.
fn get(p: &str) -> wiremock::MockBuilder {
    Mock::given(method("GET"))
        .and(path(p.to_owned()))
        .and(header("user-agent", USER_AGENT))
}

/// `GET /ws/2/<p>?fmt=json` with Starr's User-Agent.
fn ws(p: &str) -> wiremock::MockBuilder {
    get(&format!("/ws/2/{p}")).and(query_param("fmt", "json"))
}

fn album(id: &str) -> CatalogRef {
    CatalogRef {
        kind: MediaKind::Album,
        id: id.into(),
    }
}

fn artist(id: &str) -> CatalogRef {
    CatalogRef {
        kind: MediaKind::Artist,
        id: id.into(),
    }
}

fn titles(items: &[starr_core::catalog::CatalogEntry]) -> Vec<&str> {
    items.iter().map(|e| e.title.as_str()).collect()
}

/// Every request a fake received carried exactly Starr's agent and nothing
/// that could identify the operator (no credential, no cookie, no
/// referrer, no forwarded address).
async fn assert_only_starr_identifies(server: &MockServer) {
    for request in server.received_requests().await.unwrap() {
        let agent = request.headers.get("user-agent").unwrap();
        assert_eq!(agent.to_str().unwrap(), USER_AGENT);
        for name in [
            "authorization",
            "cookie",
            "referer",
            "from",
            "x-forwarded-for",
        ] {
            assert!(
                request.headers.get(name).is_none(),
                "{name} sent to {}",
                request.url
            );
        }
    }
}

#[derive(Debug, Default)]
struct MemoryCache(Mutex<HashMap<String, (Vec<u8>, Duration)>>);

impl ResponseCache for MemoryCache {
    fn get<'a>(&'a self, key: &'a str) -> starr_adapter_kit::BoxFuture<'a, Option<Vec<u8>>> {
        let hit = self.0.lock().unwrap().get(key).map(|(b, _)| b.clone());
        Box::pin(async move { hit })
    }

    fn put<'a>(
        &'a self,
        key: &'a str,
        body: &'a [u8],
        ttl: Duration,
    ) -> starr_adapter_kit::BoxFuture<'a, ()> {
        self.0
            .lock()
            .unwrap()
            .insert(key.to_owned(), (body.to_vec(), ttl));
        Box::pin(async {})
    }
}

#[test]
fn descriptor_declares_one_request_per_second_and_the_music_lists() {
    assert_eq!(DESCRIPTOR.type_id, "musicbrainz");
    assert_eq!(
        DESCRIPTOR.budgets.rate_limit,
        RateLimit {
            requests: 1,
            per: Duration::from_secs(1)
        }
    );
    assert_eq!(
        DESCRIPTOR.lists,
        [
            ListKind::Trending,
            ListKind::FreshReleases,
            ListKind::Popular
        ]
    );
    // 503 pauses everyone; the split shares never add up to more than the
    // budget (spec 0006 Q14).
    assert!(client_options(100).unavailable_is_throttle);
    assert_eq!(client_options(100).rate_limit, RATE_LIMIT);
    let rate = |l: RateLimit| f64::from(l.requests) / l.per.as_secs_f64();
    assert!(
        rate(client_options(70).rate_limit) + rate(client_options(30).rate_limit) <= 1.0 + 1e-9
    );
}

#[tokio::test]
async fn probe_reads_a_known_artist_with_starrs_agent() {
    let mb = MockServer::start().await;
    ws(&format!("artist/{RADIOHEAD}"))
        .respond_with(json("ws2/artist_radiohead.json"))
        .expect(1)
        .mount(&mb)
        .await;
    let report = MusicBrainz::new(mb_client(&mb.uri(), fast()))
        .probe()
        .await
        .unwrap();
    assert_eq!(report.version, Version::new(2, 0, 0, 0));
    assert_only_starr_identifies(&mb).await;
}

#[tokio::test]
async fn search_finds_artists_then_albums_with_escaped_text() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    ws("artist")
        .and(query_param("query", "ok\\/computer"))
        .and(query_param("limit", "25"))
        .and(query_param("offset", "0"))
        .respond_with(json("ws2/search_artist_radiohead.json"))
        .expect(1)
        .mount(&mb)
        .await;
    ws("release-group")
        .and(query_param("query", "ok\\/computer"))
        .and(query_param("offset", "0"))
        .respond_with(json("ws2/search_release_group_ok_computer.json"))
        .expect(1)
        .mount(&mb)
        .await;
    let page = adapter(&mb, &lb)
        .search(&SearchQuery {
            text: " ok/computer ",
            kinds: &[MediaKind::Artist, MediaKind::Album],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    // The row without a valid MBID is dropped at the boundary.
    assert_eq!(
        titles(&page.items),
        [
            "Radiohead",
            "Radiohead Tribute Band",
            "OK Computer",
            "OK Computer OKNOTOK 1997 2017"
        ]
    );
    // 27 artists: there is a second page.
    assert_eq!(page.next, Some(2));
    let tribute = &page.items[1];
    assert_eq!(tribute.item.kind, MediaKind::Artist);
    assert_eq!(tribute.overview, "US tribute act");
    assert_eq!(tribute.poster, None);
    let ok = &page.items[2];
    assert_eq!(ok.item, album(OK_COMPUTER));
    assert_eq!(ok.release_date, NaiveDate::from_ymd_opt(1997, 5, 21));
    assert_eq!(ok.poster.as_deref(), Some(&*format!("caa:{OK_COMPUTER}")));
    let facts = ok.album.as_ref().unwrap();
    assert_eq!(facts.artist_credit, "Radiohead");
    assert_eq!(facts.artist.unwrap().to_string(), RADIOHEAD);
    assert_eq!(facts.primary_type, Some(ReleaseType::Album));
    let oknotok = page.items[3].album.as_ref().unwrap();
    assert_eq!(oknotok.secondary_types, [SecondaryType::Compilation]);
    assert_eq!(
        page.items[3].release_date,
        NaiveDate::from_ymd_opt(2017, 1, 1)
    );
    assert_only_starr_identifies(&mb).await;
}

#[tokio::test]
async fn search_asks_only_for_the_requested_kinds_and_blank_text_sends_nothing() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    ws("release-group")
        .respond_with(json("ws2/search_release_group_ok_computer.json"))
        .expect(1)
        .mount(&mb)
        .await;
    let mb_adapter = adapter(&mb, &lb);
    let page = mb_adapter
        .search(&SearchQuery {
            text: "ok computer",
            kinds: &[MediaKind::Album],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.next, None);
    let blank = mb_adapter
        .search(&SearchQuery {
            text: "  ",
            kinds: &[MediaKind::Artist, MediaKind::Album],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    assert_eq!(blank.items.len(), 0);
    // Movies are no MusicBrainz kind.
    let movies = mb_adapter
        .search(&SearchQuery {
            text: "ok computer",
            kinds: &[MediaKind::Movie],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    assert_eq!(movies.items.len(), 0);
}

#[tokio::test]
async fn album_details_count_the_tracks_of_the_earliest_official_release() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    // Two requests as before, with more `inc` (spec 0008 slice 2).
    ws(&format!("release-group/{OK_COMPUTER}"))
        .and(query_param("inc", "artist-credits+releases+genres"))
        .respond_with(json("ws2/release_group_ok_computer.json"))
        .expect(1)
        .mount(&mb)
        .await;
    ws(&format!("release/{OK_COMPUTER_RELEASE}"))
        .and(query_param("inc", "media+recordings+artist-credits+labels"))
        .respond_with(json("ws2/release_ok_computer_media.json"))
        .expect(1)
        .mount(&mb)
        .await;
    let details = adapter(&mb, &lb)
        .details(&album(OK_COMPUTER), "en-US")
        .await
        .unwrap();
    assert_eq!(details.entry.title, "OK Computer");
    let facts = details.entry.album.as_ref().unwrap();
    assert_eq!(facts.artist_credit, "Radiohead & Friends");
    assert_eq!(facts.track_count, Some(14));
    // The tracklist of the counted release: disc and track order, track
    // credits only where they differ, lengths rounded up to seconds.
    assert_eq!(facts.tracks.len(), 14);
    let first = &facts.tracks[0];
    assert_eq!(
        (first.disc, first.position, first.title.as_str()),
        (1, 1, "Airbag")
    );
    assert_eq!(first.length_seconds, Some(284));
    assert_eq!(first.artist_credit, None);
    let bonus: Vec<_> = facts.tracks[12..]
        .iter()
        .map(|t| {
            (
                t.disc,
                t.disc_title.as_str(),
                t.position,
                t.length_seconds,
                t.artist_credit.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        bonus,
        [
            (2, "Bonus", 1, Some(201), None),
            (2, "Bonus", 2, None, Some("Radiohead"))
        ]
    );
    assert_eq!(facts.label.as_deref(), Some("Parlophone"));
    let release = facts.tracklist_release.as_ref().unwrap();
    assert_eq!(release.date, chrono::NaiveDate::from_ymd_opt(1997, 5, 21));
    assert_eq!(release.country.as_deref(), Some("GB"));
    assert_eq!(release.release_count, 3);
    // Genres, most voted first (ties by name), at most five.
    assert_eq!(
        facts.genres,
        [
            "alternative rock",
            "art rock",
            "rock",
            "electronic",
            "britpop"
        ]
    );
    assert_eq!(facts.primary_type, Some(ReleaseType::Album));
    assert_eq!(details.children.len(), 0);
    let rg = uuid::Uuid::parse_str(OK_COMPUTER).unwrap();
    let rh = uuid::Uuid::parse_str(RADIOHEAD).unwrap();
    assert_eq!(details.external_ids[0], ExternalId::MbReleaseGroup(rg));
    assert_eq!(details.external_ids[1], ExternalId::MbArtist(rh));
    assert_eq!(details.external_ids.len(), 3);
}

#[tokio::test]
async fn artist_details_page_through_the_discography_and_pick_the_newest_album_cover() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    ws(&format!("artist/{RADIOHEAD}"))
        .respond_with(json("ws2/artist_radiohead.json"))
        .expect(1)
        .mount(&mb)
        .await;
    ws("release-group")
        .and(query_param("artist", RADIOHEAD))
        .and(query_param("limit", "100"))
        .and(query_param("offset", "0"))
        .respond_with(json("ws2/browse_radiohead_offset0.json"))
        .expect(1)
        .mount(&mb)
        .await;
    ws("release-group")
        .and(query_param("artist", RADIOHEAD))
        .and(query_param("offset", "100"))
        .respond_with(json("ws2/browse_radiohead_offset100.json"))
        .expect(1)
        .mount(&mb)
        .await;
    let details = adapter(&mb, &lb)
        .details(&artist(RADIOHEAD), "en-US")
        .await
        .unwrap();
    assert_eq!(details.entry.title, "Radiohead");
    // Genres from the same lookup (`inc=genres`), keyed by name.
    let genres: Vec<_> = details
        .genres
        .iter()
        .map(|g| (g.kind, g.id, g.name.as_str()))
        .collect();
    assert_eq!(
        genres,
        [
            (MediaKind::Artist, 0, "alternative rock"),
            (MediaKind::Artist, 0, "art rock"),
            (MediaKind::Artist, 0, "electronic")
        ]
    );
    let albums: Vec<_> = details
        .children
        .iter()
        .map(|c| match c {
            Child::Album(a) => a.as_ref(),
            Child::Season(_) | Child::Scene(_) => panic!("not an album under an artist"),
        })
        .collect();
    // Newest first, undated last.
    assert_eq!(
        titles(&albums.iter().map(|a| (*a).clone()).collect::<Vec<_>>()),
        [
            "Next Album",
            "Live at the Astoria",
            "A Moon Shaped Pool",
            "Daydreaming",
            "OK Computer",
            "Undated EP"
        ]
    );
    // Browse results carry no credit: the artist is known.
    let facts = albums[2].album.as_ref().unwrap();
    assert_eq!(facts.artist_credit, "Radiohead");
    assert_eq!(
        albums[1].album.as_ref().unwrap().secondary_types,
        [SecondaryType::Live]
    );
    // The picture: the newest released album or EP without secondary types
    // (not the announced one, not the live album, not the single).
    assert_eq!(
        details.entry.poster.as_deref(),
        Some("caa:c4d5e6f7-0a1b-4c2d-8e3f-4a5b6c7d8e9f")
    );
}

#[tokio::test]
async fn unknown_and_malformed_ids_are_not_found() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    let unknown = "11111111-2222-4333-8444-555555555555";
    ws(&format!("release-group/{unknown}"))
        .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"error":"Not Found"}"#))
        .expect(1)
        .mount(&mb)
        .await;
    let a = adapter(&mb, &lb);
    assert!(matches!(
        a.details(&album(unknown), "en-US").await,
        Err(AdapterError::NotFound(_))
    ));
    // Malformed or non-canonical ids never reach MusicBrainz.
    for bad in ["../artist", "B1392450-E666-3926-A536-22C65F834433", "42"] {
        assert!(matches!(
            a.details(&album(bad), "en-US").await,
            Err(AdapterError::NotFound(_))
        ));
    }
    let movie = CatalogRef {
        kind: MediaKind::Movie,
        id: OK_COMPUTER.into(),
    };
    assert!(matches!(
        a.details(&movie, "en-US").await,
        Err(AdapterError::NotFound(_))
    ));
    assert_eq!(mb.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_503_throttles_every_caller_and_backs_off() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    ws("artist")
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&mb)
        .await;
    let a = adapter(&mb, &lb);
    let query = SearchQuery {
        text: "radiohead",
        kinds: &[MediaKind::Artist],
        page: 1,
        language: "en-US",
    };
    let err = a.search(&query).await.unwrap_err();
    let AdapterError::Throttled { retry_after } = err else {
        panic!("{err:?}");
    };
    assert!(retry_after.is_some_and(|d| d >= Duration::from_secs(1)));
    // Backing off: details now fail at once, without a request.
    assert!(matches!(
        a.details(&artist(RADIOHEAD), "en-US").await,
        Err(AdapterError::Throttled { .. })
    ));
    assert_eq!(mb.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn responses_are_cached_with_the_specs_lifetimes() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    ws("artist")
        .respond_with(json("ws2/search_artist_radiohead.json"))
        .expect(1)
        .mount(&mb)
        .await;
    ws(&format!("release-group/{OK_COMPUTER}"))
        .respond_with(json("ws2/release_group_ok_computer.json"))
        .expect(1)
        .mount(&mb)
        .await;
    ws(&format!("release/{OK_COMPUTER_RELEASE}"))
        .respond_with(json("ws2/release_ok_computer_media.json"))
        .expect(1)
        .mount(&mb)
        .await;
    let cache = Arc::new(MemoryCache::default());
    let a = adapter(&mb, &lb).with_cache(cache.clone());
    let query = SearchQuery {
        text: "radiohead",
        kinds: &[MediaKind::Artist],
        page: 1,
        language: "en-US",
    };
    for _ in 0..2 {
        a.search(&query).await.unwrap();
        a.details(&album(OK_COMPUTER), "en-US").await.unwrap();
    }
    let ttls: HashMap<String, Duration> = cache
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|(k, (_, ttl))| (k.split('?').next().unwrap().to_owned(), *ttl))
        .collect();
    assert_eq!(ttls["ws/2/artist"], Duration::from_hours(24));
    assert_eq!(
        ttls[&format!("ws/2/release-group/{OK_COMPUTER}")],
        Duration::from_hours(7 * 24)
    );
}

#[tokio::test]
async fn trending_and_popular_are_listenbrainz_statistics() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    get("/1/stats/sitewide/release-groups")
        .and(query_param("range", "week"))
        .and(query_param("count", "25"))
        .and(query_param("offset", "0"))
        .respond_with(json("listenbrainz/sitewide_release_groups_week.json"))
        .expect(1)
        .mount(&lb)
        .await;
    get("/1/stats/sitewide/release-groups")
        .and(query_param("range", "month"))
        .and(query_param("offset", "25"))
        .respond_with(json("listenbrainz/sitewide_release_groups_month.json"))
        .expect(1)
        .mount(&lb)
        .await;
    let a = adapter(&mb, &lb);
    let relevance = Relevance::music_default();
    let list = |list, page| ListQuery {
        kind: MediaKind::Album,
        list,
        page,
        language: "en-US",
        relevance: &relevance,
        filter: None,
    };
    let week = a.list(&list(ListKind::Trending, 1)).await.unwrap();
    // The entry without a release-group MBID is dropped, and so is the
    // second row of a release group (statistics count by artist credit
    // and title, so one release group can show up twice): the first,
    // most listened one stays.
    assert_eq!(titles(&week.items), ["Monsters", "After Hours"]);
    assert_eq!(week.next, Some(2));
    let monsters = week.items[0].album.as_ref().unwrap();
    assert_eq!(
        monsters.artist_credit,
        "All Time Low ft. Demi Lovato & blackbear"
    );
    assert_eq!(
        monsters.artist.unwrap().to_string(),
        "62162215-b023-4f0e-84bd-1e9412d5b32c"
    );
    let month = a.list(&list(ListKind::Popular, 2)).await.unwrap();
    assert_eq!(titles(&month.items), ["OK Computer"]);
    assert_eq!(month.next, None);
    // Artists have no lanes; nothing is asked.
    let artists = a
        .list(&ListQuery {
            kind: MediaKind::Artist,
            ..list(ListKind::Trending, 1)
        })
        .await
        .unwrap();
    assert_eq!(artists.items.len(), 0);
    assert_only_starr_identifies(&lb).await;
    assert!(mb.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn statistics_not_computed_yet_are_an_empty_lane() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    get("/1/stats/sitewide/release-groups")
        .respond_with(ResponseTemplate::new(204))
        .mount(&lb)
        .await;
    let page = adapter(&mb, &lb)
        .list(&ListQuery {
            kind: MediaKind::Album,
            list: ListKind::Trending,
            page: 1,
            language: "en-US",
            relevance: &Relevance::default(),
            filter: None,
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 0);
    assert_eq!(page.next, None);
}

#[tokio::test]
async fn fresh_releases_follow_the_release_types_newest_first() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    get("/1/explore/fresh-releases/")
        .and(query_param("release_date", "2026-11-03"))
        .and(query_param("days", "30"))
        .and(query_param("past", "true"))
        .and(query_param("future", "true"))
        .respond_with(json("listenbrainz/fresh_releases.json"))
        .expect(2)
        .mount(&lb)
        .await;
    let a = adapter(&mb, &lb);
    let query = |relevance| ListQuery {
        kind: MediaKind::Album,
        list: ListKind::FreshReleases,
        page: 1,
        language: "en-US",
        relevance,
        filter: None,
    };
    let default = Relevance::music_default();
    let page = a.list(&query(&default)).await.unwrap();
    // Albums and EPs without secondary types, each release group once,
    // the announced one first.
    assert_eq!(
        titles(&page.items),
        ["The Marvelous Real", "Profound Mysteries IV"]
    );
    assert_eq!(
        page.items[0].album.as_ref().unwrap().primary_type,
        Some(ReleaseType::Ep)
    );
    assert_eq!(page.next, None);
    let everything = Relevance::default();
    let all = a.list(&query(&everything)).await.unwrap();
    assert_eq!(
        titles(&all.items),
        [
            "The Marvelous Real",
            "A Single",
            "Live Somewhere",
            "Profound Mysteries IV"
        ]
    );
}

/// Three uncached requests at the real budget take at least two seconds,
/// also when sent at once (spec 0006 Q14: never above 1 req/s per host).
#[tokio::test]
async fn the_real_budget_allows_one_request_per_second() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    ws("artist")
        .respond_with(json("ws2/search_artist_radiohead.json"))
        .mount(&mb)
        .await;
    let a = MusicBrainz::new(mb_client(&mb.uri(), client_options(100)))
        .with_listenbrainz(listenbrainz_client(Url::parse(&lb.uri()).unwrap(), 100).unwrap());
    let started = Instant::now();
    let search = |text: &'static str| {
        let a = a.clone();
        async move {
            a.search(&SearchQuery {
                text,
                kinds: &[MediaKind::Artist],
                page: 1,
                language: "en-US",
            })
            .await
            .unwrap();
        }
    };
    tokio::join!(search("a"), search("b"), search("c"));
    assert!(
        started.elapsed() >= Duration::from_millis(1950),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(mb.received_requests().await.unwrap().len(), 3);
}

/// The web process's share in split mode (70 %) spaces requests further.
#[tokio::test]
async fn the_web_share_spaces_requests_by_more_than_a_second() {
    let mb = MockServer::start().await;
    ws("artist")
        .respond_with(json("ws2/search_artist_radiohead.json"))
        .mount(&mb)
        .await;
    let a = MusicBrainz::new(mb_client(&mb.uri(), client_options(70)));
    let started = Instant::now();
    for text in ["a", "b"] {
        a.search(&SearchQuery {
            text,
            kinds: &[MediaKind::Artist],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    }
    assert!(
        started.elapsed() >= Duration::from_millis(1400),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn covers_follow_the_archive_redirects_and_a_missing_front_is_not_found() {
    let caa = MockServer::start().await;
    // As live: the archive's 307 to archive.org, then its 302 to a storage
    // host (here all on the fake's own origin, which a base override allows).
    let download = format!("{}/download/mbid-x/front-250.jpg", caa.uri());
    let storage = format!("{}/0/items/mbid-x/front-250.jpg", caa.uri());
    get(&format!("/release-group/{OK_COMPUTER}/front-250"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", download.as_str()))
        .expect(1)
        .mount(&caa)
        .await;
    get("/download/mbid-x/front-250.jpg")
        .respond_with(ResponseTemplate::new(302).insert_header("location", storage.as_str()))
        .expect(1)
        .mount(&caa)
        .await;
    get("/0/items/mbid-x/front-250.jpg")
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(vec![0xff, 0xd8, 0xff]),
        )
        .expect(1)
        .mount(&caa)
        .await;
    let missing = "11111111-2222-4333-8444-555555555555";
    get(&format!("/release-group/{missing}/front-500"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&caa)
        .await;
    get(&format!("/release-group/{OK_COMPUTER}/front-1200"))
        .respond_with(
            ResponseTemplate::new(307).insert_header("location", "https://evil.example/x.jpg"),
        )
        .expect(1)
        .mount(&caa)
        .await;
    let archive = CoverArchive::with_base(Url::parse(&caa.uri()).unwrap()).unwrap();
    let cover = archive.front("250", OK_COMPUTER).await.unwrap();
    assert_eq!(cover.body, [0xff, 0xd8, 0xff]);
    assert_eq!(cover.content_type.as_deref(), Some("image/jpeg"));
    assert!(matches!(
        archive.front("500", missing).await,
        Err(AdapterError::NotFound(_))
    ));
    assert!(matches!(
        archive.front("1200", OK_COMPUTER).await,
        Err(AdapterError::Rejected(_))
    ));
    // Invalid input never reaches the archive.
    for (size, reference) in [
        ("w500", OK_COMPUTER),
        ("250", "../../etc"),
        ("250", "B1392450-E666-3926-A536-22C65F834433"),
    ] {
        assert!(matches!(
            archive.front(size, reference).await,
            Err(AdapterError::Rejected(_))
        ));
    }
    assert_eq!(caa.received_requests().await.unwrap().len(), 5);
    assert_only_starr_identifies(&caa).await;
}

#[tokio::test]
async fn genre_lanes_are_refreshed_in_the_background_and_read_from_the_cache_only() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    // One search per lane: albums of the genre first released since the
    // first of the month a year back (today 2026-11-03), by score.
    ws("release-group")
        .and(query_param(
            "query",
            "tag:\"rock\" AND primarytype:album AND firstreleasedate:[2025-11-01 TO *]",
        ))
        .and(query_param("limit", "25"))
        .and(query_param("offset", "0"))
        .respond_with(json("ws2/search_release_group_genre_rock.json"))
        .expect(2)
        .mount(&mb)
        .await;
    // Then one ListenBrainz popularity lookup for the lane's release groups
    // (maintainer 2026-11-07; shape as seen live, [V 11-07]).
    Mock::given(method("POST"))
        .and(path("/1/popularity/release-group"))
        .and(header("user-agent", USER_AGENT))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "release_group_mbids": [
                "b1392450-e666-3926-a536-22c65f834433",
                "5a6f2bc7-3a5c-4d1e-9f0a-2b7c1e8d9f10",
                "c0ffee00-0000-4000-8000-000000000003",
            ]
        })))
        .respond_with(json("listenbrainz/popularity_release_group_rock.json"))
        .expect(2)
        .mount(&lb)
        .await;
    let cache = Arc::new(MemoryCache::default());
    let web = adapter(&mb, &lb).with_cache(cache.clone());
    // Nothing cached yet: the web side sends nothing and has no lane.
    assert!(web.cached_genre_lane("rock").await.is_none());
    assert_eq!(mb.received_requests().await.unwrap().len(), 0);
    assert_eq!(lb.received_requests().await.unwrap().len(), 0);

    let worker = adapter(&mb, &lb).with_cache(cache.clone());
    let refreshed = worker.refresh_genre_lane("rock").await.unwrap();
    // Most listened first; the release without listens is dropped.
    let titles: Vec<&str> = refreshed.items.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, ["OK Computer OKNOTOK 1997 2017", "OK Computer"]);
    assert_eq!(refreshed.next, None);
    {
        let entries = cache.0.lock().unwrap();
        assert_eq!(entries.len(), 1);
        let (key, (_, ttl)) = entries.iter().next().unwrap();
        assert!(key.starts_with("ws/2/release-group?query=tag"), "{key}");
        assert_eq!(*ttl, GENRE_LANE_CACHE.min_ttl);
        assert_eq!(*ttl, Duration::from_hours(24));
    }
    let lane = web.cached_genre_lane("rock").await.unwrap();
    assert_eq!(lane, refreshed);
    // A refresh fetches again even while the lane is cached (`expect(2)`).
    worker.refresh_genre_lane("rock").await.unwrap();
    // `list` takes no genre filter: lanes never go through it (so no list
    // call can spend the web budget on them).
    let listed = web
        .list(&ListQuery {
            kind: MediaKind::Album,
            list: ListKind::Popular,
            page: 1,
            language: "en-US",
            relevance: &Relevance::music_default(),
            filter: Some(ListFilter::Genre(GenreRef::Name("rock".into()))),
        })
        .await
        .unwrap();
    assert_eq!(listed, Page::end());
    // Only the fixed genres (F-8) and one page; nothing is sent for others.
    assert!(GENRES.iter().any(|(id, _)| *id == "hip hop"));
    assert_eq!(GENRES.len(), 12);
    assert!(web.cached_genre_lane("not a genre").await.is_none());
    assert_eq!(
        worker.refresh_genre_lane("not a genre").await.unwrap(),
        Page::end()
    );
    for (filter, page) in [
        (ListFilter::Genre(GenreRef::Name("rock".into())), 2),
        (ListFilter::Genre(GenreRef::Id(18)), 1),
        (ListFilter::Provider(8), 1),
    ] {
        let found = web
            .list(&ListQuery {
                kind: MediaKind::Album,
                list: ListKind::Popular,
                page,
                language: "en-US",
                relevance: &Relevance::music_default(),
                filter: Some(filter),
            })
            .await
            .unwrap();
        assert_eq!(found, Page::end());
    }
}

/// A 429 from `ListenBrainz`'s popularity lookup fails the lane as
/// throttled (waiting for `X-RateLimit-Reset-In`) and keeps the lane
/// cached before.
#[tokio::test]
async fn a_throttled_popularity_lookup_keeps_the_cached_lane() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    ws("release-group")
        .respond_with(json("ws2/search_release_group_genre_rock.json"))
        .mount(&mb)
        .await;
    Mock::given(method("POST"))
        .and(path("/1/popularity/release-group"))
        .respond_with(json("listenbrainz/popularity_release_group_rock.json"))
        .up_to_n_times(1)
        .mount(&lb)
        .await;
    Mock::given(method("POST"))
        .and(path("/1/popularity/release-group"))
        .respond_with(ResponseTemplate::new(429).insert_header("x-ratelimit-reset-in", "5"))
        .mount(&lb)
        .await;
    let cache = Arc::new(MemoryCache::default());
    let worker = adapter(&mb, &lb).with_cache(cache.clone());
    let first = worker.refresh_genre_lane("rock").await.unwrap();
    assert_eq!(first.items.len(), 2);
    assert_eq!(
        worker.refresh_genre_lane("rock").await,
        Err(AdapterError::Throttled {
            retry_after: Some(Duration::from_secs(5))
        })
    );
    let web = adapter(&mb, &lb).with_cache(cache);
    assert_eq!(web.cached_genre_lane("rock").await.unwrap(), first);
}
/// Failures after the search never replace a good lane: a release group
/// the lane cannot read, a lane where no release group has listens (a
/// changed or empty popularity answer), a popularity body of another shape
/// and a `ListenBrainz` 5xx each fail the refresh, and the lane cached
/// before stays.
#[tokio::test]
async fn a_failed_ranking_keeps_the_cached_lane() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    ws("release-group")
        .respond_with(json("ws2/search_release_group_genre_rock.json"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&mb)
        .await;
    // Later searches: an element without a title, then the good answer.
    ws("release-group")
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                r#"{"count":1,"offset":0,"release-groups":[{"id":"b1392450-e666-3926-a536-22c65f834433"}]}"#,
            ),
        )
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&mb)
        .await;
    ws("release-group")
        .respond_with(json("ws2/search_release_group_genre_rock.json"))
        .with_priority(3)
        .mount(&mb)
        .await;
    let popularity = || Mock::given(method("POST")).and(path("/1/popularity/release-group"));
    popularity()
        .respond_with(json("listenbrainz/popularity_release_group_rock.json"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&lb)
        .await;
    popularity()
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{"release_group_mbid":"b1392450-e666-3926-a536-22c65f834433","total_listen_count":null},{"release_group_mbid":"5a6f2bc7-3a5c-4d1e-9f0a-2b7c1e8d9f10","total_listen_count":0}]"#,
        ))
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&lb)
        .await;
    popularity()
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                r#"[{"mbid":"b1392450-e666-3926-a536-22c65f834433","listens":7}]"#,
            ),
        )
        .up_to_n_times(1)
        .with_priority(3)
        .mount(&lb)
        .await;
    popularity()
        .respond_with(ResponseTemplate::new(500))
        .with_priority(4)
        .mount(&lb)
        .await;
    let cache = Arc::new(MemoryCache::default());
    let worker = adapter(&mb, &lb).with_cache(cache.clone());
    let good = worker.refresh_genre_lane("rock").await.unwrap();
    assert_eq!(good.items.len(), 2);
    let web = adapter(&mb, &lb).with_cache(cache);

    // The search answers an unreadable release group.
    assert!(matches!(
        worker.refresh_genre_lane("rock").await,
        Err(AdapterError::Unknown(_))
    ));
    assert_eq!(web.cached_genre_lane("rock").await.unwrap(), good);
    // No release group has listens.
    assert!(matches!(
        worker.refresh_genre_lane("rock").await,
        Err(AdapterError::Unknown(_))
    ));
    assert_eq!(web.cached_genre_lane("rock").await.unwrap(), good);
    // The popularity answer has another shape.
    assert!(matches!(
        worker.refresh_genre_lane("rock").await,
        Err(AdapterError::Unknown(_))
    ));
    assert_eq!(web.cached_genre_lane("rock").await.unwrap(), good);
    // ListenBrainz is down.
    assert!(matches!(
        worker.refresh_genre_lane("rock").await,
        Err(AdapterError::Transient(_))
    ));
    assert_eq!(web.cached_genre_lane("rock").await.unwrap(), good);
}

/// While `ListenBrainz` pauses (its window is used up), a lane is not
/// searched at all: the `MusicBrainz` request would be wasted.
#[tokio::test]
async fn a_paused_listenbrainz_skips_the_search() {
    let (mb, lb) = (MockServer::start().await, MockServer::start().await);
    ws("release-group")
        .respond_with(json("ws2/search_release_group_genre_rock.json"))
        .mount(&mb)
        .await;
    Mock::given(method("POST"))
        .and(path("/1/popularity/release-group"))
        .respond_with(
            json("listenbrainz/popularity_release_group_rock.json")
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset-in", "9"),
        )
        .mount(&lb)
        .await;
    let worker = adapter(&mb, &lb).with_cache(Arc::new(MemoryCache::default()));
    worker.refresh_genre_lane("rock").await.unwrap();
    let searches = mb.received_requests().await.unwrap().len();
    assert!(matches!(
        worker.refresh_genre_lane("jazz").await,
        Err(AdapterError::Throttled { .. })
    ));
    assert_eq!(mb.received_requests().await.unwrap().len(), searches);
}
