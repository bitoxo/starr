//! TMDB adapter against a `wiremock` fake replaying `fixtures/v3/`
//! (lesson 10). The fixtures are built from TMDB's documented shapes, not
//! recorded (see capabilities.md, TMDB). Every mock requires the bearer
//! token, so a request without it fails the test.
#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use secrecy::SecretString;
use starr_adapter_kit::{Endpoint, ResponseCache, ServiceClient};
use starr_adapter_tmdb::images::{IMAGE_SIZES, ImageCdn};
use starr_adapter_tmdb::{AUTH, CACHE_POLICY, DAILY_CACHE_POLICY, DESCRIPTOR, PAGE_CAP, Tmdb};
use starr_core::adapters::{AdapterError, Probe, Version};
use starr_core::catalog::{
    Catalog, CatalogDetails, CatalogRef, Certification, CrewRole, ExternalId, ListFilter, ListKind,
    ListQuery, MediaKind, OfferKind, Page, RegionalDate, Relevance, SearchQuery,
};
use url::Url;
use wiremock::matchers::{header, header_regex, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.contract-test-token";
const BEARER: &str = "Bearer eyJhbGciOiJIUzI1NiJ9.contract-test-token";

fn fixture(name: &str) -> String {
    let file = format!("{}/fixtures/v3/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file}: {e}"))
}

fn json(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "application/json;charset=utf-8")
        .insert_header("cache-control", "public, max-age=21600")
        .set_body_string(fixture(name))
}

fn status(code: u16, name: &str) -> ResponseTemplate {
    ResponseTemplate::new(code)
        .insert_header("content-type", "application/json;charset=utf-8")
        .set_body_string(fixture(name))
}

fn client(uri: &str, token: &str) -> ServiceClient {
    ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(uri).unwrap(),
            auth: AUTH,
            credential: SecretString::from(token.to_owned()),
        },
    )
    .unwrap()
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
}

fn tmdb(server: &MockServer) -> Tmdb {
    Tmdb::new(client(&server.uri(), TOKEN)).with_today(today())
}

/// `GET /3/<path>` with the bearer token.
fn get(p: &str) -> wiremock::MockBuilder {
    Mock::given(method("GET"))
        .and(path(format!("/3/{p}")))
        .and(header("authorization", BEARER))
}

fn movie(id: &str) -> CatalogRef {
    CatalogRef {
        kind: MediaKind::Movie,
        id: id.into(),
    }
}

fn ids(items: &[starr_core::catalog::CatalogEntry]) -> Vec<&str> {
    items.iter().map(|e| e.item.id.as_str()).collect()
}

// ---------------------------------------------------------------- probe

#[tokio::test]
async fn probe_validates_the_token_and_reports_api_v3() {
    let server = MockServer::start().await;
    get("authentication")
        .respond_with(json("authentication.json"))
        .expect(1)
        .mount(&server)
        .await;
    let report = tmdb(&server).probe().await.unwrap();
    assert_eq!(report.version, Version::new(3, 0, 0, 0));
    assert!(DESCRIPTOR.tested_versions.contains(&report.version));
    assert_eq!(tmdb(&server).descriptor().type_id, "tmdb");
}

#[tokio::test]
async fn invalid_token_is_credentials_invalid_status_code_7() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(status(401, "error_invalid_key.json"))
        .expect(2)
        .mount(&server)
        .await;
    let adapter = Tmdb::new(client(&server.uri(), "a-v3-key-is-not-a-bearer-token"));
    assert_eq!(adapter.probe().await, Err(AdapterError::CredentialsInvalid));
    // Auth precedes resource lookup: unknown ids also answer 401.
    assert_eq!(
        adapter.details(&movie("999999999"), "en-US").await,
        Err(AdapterError::CredentialsInvalid)
    );
}

#[tokio::test]
async fn unsuccessful_authentication_body_is_credentials_invalid() {
    let server = MockServer::start().await;
    get("authentication")
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"success": false, "status_code": 7}"#),
        )
        .mount(&server)
        .await;
    assert_eq!(
        tmdb(&server).probe().await,
        Err(AdapterError::CredentialsInvalid)
    );
}

#[tokio::test]
async fn probe_errors_follow_the_taxonomy() {
    for (response, expect) in [
        (status(500, "error_not_found.json"), "transient"),
        (
            status(429, "error_rate_limit.json").insert_header("retry-after", "3"),
            "throttled",
        ),
        (
            ResponseTemplate::new(200).set_body_string("<html>maintenance</html>"),
            "unknown",
        ),
        (
            ResponseTemplate::new(200).set_body_string(r#"{"unexpected": true}"#),
            "unknown",
        ),
    ] {
        let server = MockServer::start().await;
        get("authentication")
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let err = tmdb(&server).probe().await.unwrap_err();
        assert_eq!(err.code(), expect, "{err:?}");
    }
}

#[tokio::test]
async fn unreachable_is_transient() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .unwrap()
        .port();
    let adapter = Tmdb::new(client(&format!("http://127.0.0.1:{port}"), TOKEN));
    assert!(matches!(
        adapter.probe().await,
        Err(AdapterError::Transient(_))
    ));
}

// ---------------------------------------------------------------- details

#[tokio::test]
async fn movie_details_with_appends() {
    let server = MockServer::start().await;
    get("movie/27205")
        .and(query_param("language", "en-US"))
        .and(query_param(
            "append_to_response",
            "external_ids,videos,release_dates,watch/providers,credits",
        ))
        .and(query_param("include_video_language", "en,null"))
        .respond_with(json("movie_27205.json"))
        .expect(1)
        .mount(&server)
        .await;
    let d = tmdb(&server)
        .details(&movie("27205"), "en-US")
        .await
        .unwrap();
    assert_eq!(d.entry.title, "Inception");
    assert_eq!(d.entry.release_date, NaiveDate::from_ymd_opt(2010, 7, 15));
    assert_eq!(d.entry.genre_ids, vec![28, 878, 12]);
    assert_eq!(d.entry.rating, Some(8.369));
    assert_eq!(
        d.entry.poster.as_deref(),
        Some("/ljsZTbVsrQSqZgWeep2B1QiDKuh.jpg")
    );
    assert_eq!(
        d.external_ids,
        vec![
            ExternalId::Tmdb(27205),
            ExternalId::Imdb("tt1375666".into())
        ]
    );
    let trailer = d.trailer.as_ref().unwrap();
    assert_eq!(
        (trailer.site.as_str(), trailer.key.as_str()),
        ("YouTube", "YoHD9XEInc0")
    );
    // CH: the only rated release (trimmed); BR has no rating.
    assert_eq!(
        d.certifications,
        vec![
            Certification {
                country: "CH".into(),
                rating: "12".into()
            },
            Certification {
                country: "US".into(),
                rating: "PG-13".into()
            },
        ]
    );
    assert!(offered(&d, "CH", 8, OfferKind::Flatrate));
    assert!(offered(&d, "DE", 2, OfferKind::Buy));
    let netflix = d
        .watch_offers
        .iter()
        .find(|o| o.region == "CH" && o.provider_id == 8)
        .unwrap();
    assert_eq!(netflix.provider_name, "Netflix");
    assert_eq!(
        netflix.logo.as_deref(),
        Some("/pbpMk2JmcoNnQwx5JGpXngfoWtp.jpg")
    );
    assert_eq!(d.children, vec![]);
    // Facts of the detail view (spec 0008 slice 2), same request.
    let f = &d.facts;
    assert_eq!(f.runtime_minutes, Some(148));
    assert_eq!(f.tagline, "Your mind is the scene of the crime.");
    assert_eq!(f.cast.len(), 12, "top 12 of 14");
    assert_eq!(
        (f.cast[0].name.as_str(), f.cast[0].character.as_str()),
        ("Leonardo DiCaprio", "Dom Cobb")
    );
    let crew: Vec<_> = f.crew.iter().map(|c| (c.name.as_str(), c.role)).collect();
    assert_eq!(
        crew,
        [
            ("Christopher Nolan", CrewRole::Director),
            ("Christopher Nolan", CrewRole::Writer)
        ]
    );
    assert_eq!(f.production_companies, ["Legendary Pictures", "Syncopy"]);
    assert_eq!(
        f.digital_releases,
        [RegionalDate {
            country: "CH".into(),
            date: NaiveDate::from_ymd_opt(2010, 11, 20).unwrap()
        }]
    );
    assert_eq!(f.series, None);
}

fn offered(d: &CatalogDetails, region: &str, provider: u64, kind: OfferKind) -> bool {
    d.watch_offers
        .iter()
        .any(|o| o.region == region && o.provider_id == provider && o.kind == kind)
}

#[tokio::test]
async fn season_refs_answer_the_season_with_its_episodes() {
    let server = MockServer::start().await;
    get("tv/1399/season/1")
        .and(query_param("language", "en-US"))
        .respond_with(json("tv_1399_season_1.json"))
        .expect(1)
        .mount(&server)
        .await;
    let cache = Arc::new(MemoryCache::default());
    let tmdb = tmdb(&server).with_cache(cache.clone());
    let season = CatalogRef::season("1399", 1);
    let d = tmdb.details(&season, "en-US").await.unwrap();
    assert_eq!(d.entry.item, season);
    assert_eq!(d.entry.title, "Season 1");
    let s = d.seasons().next().unwrap();
    assert_eq!(s.number, 1);
    let episodes: Vec<_> = s
        .episodes
        .iter()
        .map(|e| (e.number, e.name.as_str(), e.runtime_minutes))
        .collect();
    assert_eq!(
        episodes,
        [
            (1, "Winter Is Coming", Some(62)),
            (2, "The Kingsroad", Some(56)),
            (3, "Lord Snow", None)
        ]
    );
    assert_eq!(s.episodes[2].air_date, None);
    // Cached a day (no max-age from the fake): a second look sends
    // nothing (`expect(1)`).
    tmdb.details(&season, "en-US").await.unwrap();
    {
        let entries = cache.entries.lock().unwrap();
        let (key, (_, ttl)) = entries.iter().next().unwrap();
        assert_eq!(key, "3/tv/1399/season/1?language=en-US");
        assert_eq!(*ttl, DAILY_CACHE_POLICY.min_ttl);
        assert_eq!(*ttl, Duration::from_hours(24));
    }
    // A garbage series id in a season ref sends nothing.
    let err = tmdb
        .details(&CatalogRef::season("x", 1), "en-US")
        .await
        .unwrap_err();
    assert!(matches!(err, AdapterError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn on_the_air_is_a_series_list() {
    let server = MockServer::start().await;
    get("tv/on_the_air")
        .and(query_param("page", "1"))
        .and(query_param_is_missing("with_original_language"))
        .respond_with(json("discover_tv.json"))
        .expect(1)
        .mount(&server)
        .await;
    let tmdb = tmdb(&server);
    let relevance = Relevance::default();
    for list in [ListKind::OnTheAir] {
        let page = tmdb
            .list(&ListQuery {
                kind: MediaKind::Series,
                list,
                page: 1,
                language: "en-US",
                relevance: &relevance,
                filter: None,
            })
            .await
            .unwrap();
        assert_ne!(page.items.len(), 0, "{list:?}");
        // Movies have none; nothing is sent.
        let page = tmdb
            .list(&ListQuery {
                kind: MediaKind::Movie,
                list,
                page: 1,
                language: "en-US",
                relevance: &relevance,
                filter: None,
            })
            .await
            .unwrap();
        assert_eq!(page, Page::end());
    }
}

#[tokio::test]
async fn provider_and_genre_filters_narrow_lists() {
    let server = MockServer::start().await;
    get("discover/movie")
        .and(query_param("watch_region", "CH"))
        .and(query_param("with_watch_providers", "8"))
        .and(query_param("with_watch_monetization_types", "flatrate"))
        .and(query_param("with_genres", "18"))
        .respond_with(json("discover_movie.json"))
        .expect(0)
        .mount(&server)
        .await;
    get("discover/movie")
        .and(query_param("watch_region", "CH"))
        .and(query_param("with_watch_providers", "8"))
        .and(query_param("with_watch_monetization_types", "flatrate"))
        .respond_with(json("discover_movie.json"))
        .expect(1)
        .mount(&server)
        .await;
    let tmdb = tmdb(&server);
    let relevance = Relevance {
        provider_region: Some("CH".into()),
        ..Relevance::default()
    };
    let page = tmdb
        .list(&ListQuery {
            kind: MediaKind::Movie,
            list: ListKind::Popular,
            page: 1,
            language: "en-US",
            relevance: &relevance,
            filter: Some(ListFilter::Provider(8)),
        })
        .await
        .unwrap();
    assert_ne!(page.items.len(), 0);
    // Trending cannot be filtered by provider: an empty page, no request.
    let page = tmdb
        .list(&ListQuery {
            kind: MediaKind::Movie,
            list: ListKind::Trending,
            page: 1,
            language: "en-US",
            relevance: &relevance,
            filter: Some(ListFilter::Provider(8)),
        })
        .await
        .unwrap();
    assert_eq!(page, Page::end());
}

#[tokio::test]
async fn series_details_with_seasons_and_specials() {
    let server = MockServer::start().await;
    get("tv/1399")
        .and(query_param(
            "append_to_response",
            "external_ids,videos,content_ratings,watch/providers,aggregate_credits",
        ))
        .respond_with(json("tv_1399.json"))
        .expect(1)
        .mount(&server)
        .await;
    let d = tmdb(&server)
        .details(
            &CatalogRef {
                kind: MediaKind::Series,
                id: "1399".into(),
            },
            "en-US",
        )
        .await
        .unwrap();
    assert_eq!(d.entry.title, "Game of Thrones");
    assert!(d.external_ids.contains(&ExternalId::Tvdb(121_361)));
    let numbers: Vec<u32> = d.seasons().map(|s| s.number).collect();
    assert_eq!(numbers, vec![0, 1, 2, 9]);
    assert!(d.seasons().next().unwrap().is_special);
    assert_eq!(d.seasons().next().unwrap().episode_count, 296);
    assert_eq!(
        d.seasons().nth(1).unwrap().air_date,
        NaiveDate::from_ymd_opt(2011, 4, 17)
    );
    // Announced season without air date: listed, not yet "new" (D8).
    assert_eq!(d.seasons().nth(3).unwrap().air_date, None);
    assert_eq!(
        d.certifications,
        vec![
            Certification {
                country: "US".into(),
                rating: "TV-MA".into()
            },
            Certification {
                country: "DE".into(),
                rating: "16".into()
            },
        ]
    );
    assert!(offered(&d, "CH", 300, OfferKind::Ads));
    let f = &d.facts;
    let series = f.series.as_ref().unwrap();
    assert_eq!(series.status, "Ended");
    assert_eq!(series.season_count, Some(8));
    assert_eq!(series.networks, ["HBO"]);
    assert_eq!(series.next_episode, None);
    assert_eq!(
        (f.cast[0].name.as_str(), f.cast[0].character.as_str()),
        ("Peter Dinklage", "Tyrion Lannister")
    );
    let crew: Vec<_> = f.crew.iter().map(|c| (c.name.as_str(), c.role)).collect();
    assert_eq!(
        crew,
        [
            ("David Benioff", CrewRole::Creator),
            ("David Benioff", CrewRole::Writer),
            ("D. B. Weiss", CrewRole::Writer)
        ]
    );
    // No typical episode length listed.
    assert_eq!(f.runtime_minutes, None);
}

#[tokio::test]
async fn unknown_id_is_not_found_and_garbage_ids_send_nothing() {
    let server = MockServer::start().await;
    get("movie/999999999")
        .respond_with(status(404, "error_not_found.json"))
        .expect(1)
        .mount(&server)
        .await;
    let adapter = tmdb(&server);
    assert!(matches!(
        adapter.details(&movie("999999999"), "en-US").await,
        Err(AdapterError::NotFound(_))
    ));
    for bad in ["", "../configuration", "12a", "-1"] {
        assert!(
            matches!(
                adapter.details(&movie(bad), "en-US").await,
                Err(AdapterError::NotFound(_))
            ),
            "{bad:?}"
        );
    }
}

#[tokio::test]
async fn english_fallback_fills_untranslated_details() {
    let server = MockServer::start().await;
    get("movie/496243")
        .and(query_param("language", "de-DE"))
        .and(query_param("include_video_language", "de,en,null"))
        .respond_with(json("movie_496243_de-DE.json"))
        .expect(1)
        .mount(&server)
        .await;
    get("movie/496243")
        .and(query_param("language", "en-US"))
        .and(query_param_is_missing("append_to_response"))
        .respond_with(json("movie_496243_en-US.json"))
        .expect(1)
        .mount(&server)
        .await;
    // de-CH is sent as de-DE.
    let d = tmdb(&server)
        .details(&movie("496243"), "de-CH")
        .await
        .unwrap();
    assert_eq!(d.entry.title, "Parasite");
    assert!(d.entry.overview.starts_with("All unemployed"));
    assert_eq!(d.entry.original_title, "기생충");
    // Genre names stay in the requested language.
    assert_eq!(d.genres[0].name, "Komödie");
}

#[tokio::test]
async fn english_requests_never_fetch_a_fallback() {
    let server = MockServer::start().await;
    get("movie/496243")
        .and(query_param("language", "en-US"))
        .respond_with(json("movie_496243_de-DE.json"))
        .expect(1)
        .mount(&server)
        .await;
    let d = tmdb(&server)
        .details(&movie("496243"), "en-US")
        .await
        .unwrap();
    assert_eq!(d.entry.overview, "");
}

// ---------------------------------------------------------------- search

#[tokio::test]
async fn search_finds_movies_and_series_but_no_people() {
    let server = MockServer::start().await;
    get("search/multi")
        .and(query_param("query", "inception"))
        .and(query_param("page", "1"))
        .and(query_param("include_adult", "false"))
        .respond_with(json("search_multi.json"))
        .expect(2)
        .mount(&server)
        .await;
    let adapter = tmdb(&server);
    let page = adapter
        .search(&SearchQuery {
            text: "  inception ",
            kinds: &MediaKind::ALL,
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    let refs: Vec<(MediaKind, &str)> = page
        .items
        .iter()
        .map(|e| (e.item.kind, e.item.id.as_str()))
        .collect();
    // Same TMDB id for a movie and a series: two distinct titles.
    assert_eq!(
        refs,
        vec![(MediaKind::Movie, "27205"), (MediaKind::Series, "27205")]
    );
    assert_eq!(page.next, None);
    assert_eq!(page.items[1].release_date, None);
    assert_eq!(page.items[1].rating, None);

    let movies_only = adapter
        .search(&SearchQuery {
            text: "inception",
            kinds: &[MediaKind::Movie],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    assert_eq!(ids(&movies_only.items), vec!["27205"]);
    assert_eq!(movies_only.items[0].item.kind, MediaKind::Movie);
}

#[tokio::test]
async fn empty_search_and_out_of_range_pages_send_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let adapter = tmdb(&server);
    for (text, page) in [("   ", 1), ("x", 0), ("x", PAGE_CAP + 1)] {
        let result = adapter
            .search(&SearchQuery {
                text,
                kinds: &MediaKind::ALL,
                page,
                language: "en-US",
            })
            .await
            .unwrap();
        assert!(result.items.is_empty() && result.next.is_none());
    }
}

// ---------------------------------------------------------------- lists

#[tokio::test]
async fn trending_is_deduplicated_across_pages_and_filtered_by_language() {
    let server = MockServer::start().await;
    get("trending/movie/week")
        .and(query_param("page", "1"))
        .respond_with(json("trending_movie_week_page1.json"))
        .mount(&server)
        .await;
    get("trending/movie/week")
        .and(query_param("page", "2"))
        .respond_with(json("trending_movie_week_page2.json"))
        .mount(&server)
        .await;
    let adapter = tmdb(&server);
    let relevance = Relevance {
        original_languages: vec!["en".into()],
        ..Relevance::default()
    };
    let query = |page| ListQuery {
        kind: MediaKind::Movie,
        list: ListKind::Trending,
        page,
        language: "en-US",
        relevance: &relevance,
        filter: None,
    };
    let first = adapter.list(&query(1)).await.unwrap();
    // Parasite (ko) is filtered out by original language.
    assert_eq!(ids(&first.items), vec!["27205", "1184918"]);
    assert_eq!(first.next, Some(2));
    let second = adapter.list(&query(2)).await.unwrap();
    // Inception was on page 1, Interstellar appears twice on page 2.
    assert_eq!(ids(&second.items), vec!["157336"]);
}

/// The login backdrop (spec 0008 slice 11) reads Trending from the cache
/// only: a visitor who is not signed in never costs a catalog call.
#[tokio::test]
async fn cached_trending_reads_the_cache_only_with_relevance_applied() {
    let server = MockServer::start().await;
    get("trending/movie/week")
        .and(query_param("page", "1"))
        .respond_with(json("trending_movie_week_page1.json"))
        .expect(1)
        .mount(&server)
        .await;
    let cache = Arc::new(MemoryCache::default());
    let adapter = tmdb(&server).with_cache(cache.clone());
    let relevance = Relevance {
        original_languages: vec!["en".into()],
        ..Relevance::default()
    };
    // Nothing cached: no page and no request.
    assert!(
        adapter
            .cached_trending(MediaKind::Movie, "en-US", &relevance)
            .await
            .is_none()
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
    adapter
        .list(&ListQuery {
            kind: MediaKind::Movie,
            list: ListKind::Trending,
            page: 1,
            language: "en-US",
            relevance: &relevance,
            filter: None,
        })
        .await
        .unwrap();
    let cached = adapter
        .cached_trending(MediaKind::Movie, "en-US", &relevance)
        .await
        .unwrap();
    // Parasite (ko) is filtered out as in `list`; posters come along.
    assert_eq!(ids(&cached.items), vec!["27205", "1184918"]);
    assert_eq!(
        cached.items[0].poster.as_deref(),
        Some("/ljsZTbVsrQSqZgWeep2B1QiDKuh.jpg")
    );
    // Kinds TMDB has no Trending for answer nothing (and send nothing).
    assert!(
        adapter
            .cached_trending(MediaKind::Album, "en-US", &relevance)
            .await
            .is_none()
    );
    // Without a cache there is nothing to read.
    assert!(
        tmdb(&server)
            .cached_trending(MediaKind::Movie, "en-US", &relevance)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn trending_without_relevance_keeps_every_language() {
    let server = MockServer::start().await;
    get("trending/movie/week")
        .and(query_param("page", "1"))
        .and(query_param_is_missing("with_original_language"))
        .respond_with(json("trending_movie_week_page1.json"))
        .mount(&server)
        .await;
    let page = tmdb(&server)
        .list(&ListQuery {
            kind: MediaKind::Movie,
            list: ListKind::Trending,
            page: 1,
            language: "en-US",
            relevance: &Relevance::default(),
            filter: None,
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);
}

#[tokio::test]
async fn page_cap_ends_the_list_without_error() {
    let server = MockServer::start().await;
    get("discover/movie")
        .and(query_param("page", "500"))
        .respond_with(json("discover_movie.json"))
        .expect(1)
        .mount(&server)
        .await;
    let adapter = tmdb(&server);
    let relevance = Relevance::default();
    let query = |page| ListQuery {
        kind: MediaKind::Movie,
        list: ListKind::Popular,
        page,
        language: "en-US",
        relevance: &relevance,
        filter: None,
    };
    // total_pages says 1001; TMDB serves up to 500.
    let last = adapter.list(&query(500)).await.unwrap();
    assert_eq!(last.items.len(), 2);
    assert_eq!(last.next, None);
    // Page 501 would be a 400 status_code 22: not requested at all.
    let beyond = adapter.list(&query(501)).await.unwrap();
    assert!(beyond.items.is_empty() && beyond.next.is_none());
}

#[tokio::test]
async fn relevance_becomes_discover_parameters() {
    let server = MockServer::start().await;
    get("discover/tv")
        .and(query_param("with_original_language", "en|de"))
        .and(query_param("without_genres", "10767,10763,10766,10764"))
        .and(query_param("watch_region", "CH"))
        .and(query_param("with_watch_providers", "8|337"))
        .and(query_param(
            "with_watch_monetization_types",
            "flatrate|free|ads",
        ))
        .and(query_param("air_date.gte", "2026-08-03"))
        .and(query_param("sort_by", "popularity.desc"))
        .respond_with(json("discover_tv.json"))
        .expect(1)
        .mount(&server)
        .await;
    let relevance = Relevance {
        original_languages: vec!["en".into(), "de".into()],
        provider_region: Some("CH".into()),
        providers: vec![8, 337],
        release_region: Some("CH".into()),
        excluded_genres: starr_adapter_tmdb::DEFAULT_SERIES_EXCLUDED_GENRES.to_vec(),
        ..Relevance::default()
    };
    let page = tmdb(&server)
        .list(&ListQuery {
            kind: MediaKind::Series,
            list: ListKind::OnServices,
            page: 1,
            language: "en-US",
            relevance: &relevance,
            filter: None,
        })
        .await
        .unwrap();
    // The talk show is dropped by Starr even if TMDB let it through.
    assert_eq!(ids(&page.items), vec!["1399"]);
    assert_eq!(page.next, Some(2));
}

#[tokio::test]
async fn upcoming_and_top_rated_parameters() {
    let server = MockServer::start().await;
    get("discover/movie")
        .and(query_param("region", "CH"))
        .and(query_param("with_release_type", "2|3"))
        .and(query_param("release_date.gte", "2026-11-01"))
        .and(query_param("primary_release_date.gte", "2025-11-01"))
        .respond_with(json("discover_movie.json"))
        .expect(1)
        .mount(&server)
        .await;
    get("discover/movie")
        .and(query_param("sort_by", "vote_average.desc"))
        .and(query_param("vote_count.gte", "5000"))
        .respond_with(json("discover_movie.json"))
        .expect(1)
        .mount(&server)
        .await;
    let adapter = tmdb(&server);
    let relevance = Relevance {
        release_region: Some("CH".into()),
        ..Relevance::default()
    };
    for list in [ListKind::Upcoming, ListKind::TopRated] {
        adapter
            .list(&ListQuery {
                kind: MediaKind::Movie,
                list,
                page: 1,
                language: "en-US",
                relevance: &relevance,
                filter: None,
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn music_kinds_and_lists_send_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let adapter = tmdb(&server);
    for (kind, list) in [
        (MediaKind::Album, ListKind::Trending),
        (MediaKind::Artist, ListKind::Popular),
        (MediaKind::Movie, ListKind::FreshReleases),
    ] {
        let page = adapter
            .list(&ListQuery {
                kind,
                list,
                page: 1,
                language: "en-US",
                relevance: &Relevance::default(),
                filter: None,
            })
            .await
            .unwrap();
        assert!(
            page.items.is_empty() && page.next.is_none(),
            "{kind:?} {list:?}"
        );
    }
    let album = CatalogRef {
        kind: MediaKind::Album,
        id: "603".into(),
    };
    assert!(matches!(
        adapter.details(&album, "en-US").await,
        Err(AdapterError::NotFound(_))
    ));
    assert_eq!(
        adapter.providers(MediaKind::Artist, "CH").await.unwrap(),
        vec![]
    );
    assert!(!DESCRIPTOR.lists.contains(&ListKind::FreshReleases));
}

#[tokio::test]
async fn on_services_without_providers_is_empty_without_a_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let page = tmdb(&server)
        .list(&ListQuery {
            kind: MediaKind::Movie,
            list: ListKind::OnServices,
            page: 1,
            language: "en-US",
            relevance: &Relevance {
                provider_region: Some("CH".into()),
                ..Relevance::default()
            },
            filter: None,
        })
        .await
        .unwrap();
    assert!(page.items.is_empty() && page.next.is_none());
}

#[tokio::test]
async fn list_fallback_fetches_the_english_page_for_untranslated_items() {
    let server = MockServer::start().await;
    get("discover/movie")
        .and(query_param("language", "de-DE"))
        .respond_with(json("discover_movie.json"))
        .expect(1)
        .mount(&server)
        .await;
    let english = fixture("discover_movie.json").replace(
        r#""overview": "", "popularity": 1.2"#,
        r#""overview": "A film.", "popularity": 1.2"#,
    );
    get("discover/movie")
        .and(query_param("language", "en-US"))
        .respond_with(ResponseTemplate::new(200).set_body_string(english))
        .expect(1)
        .mount(&server)
        .await;
    let page = tmdb(&server)
        .list(&ListQuery {
            kind: MediaKind::Movie,
            list: ListKind::Popular,
            page: 500,
            language: "de-DE",
            relevance: &Relevance::default(),
            filter: None,
        })
        .await
        .unwrap();
    let film = page.items.iter().find(|e| e.item.id == "1000001").unwrap();
    assert_eq!(film.overview, "A film.");
    assert_eq!(film.title, "Ein Film");
}

#[tokio::test]
async fn throttling_and_not_found_on_lists() {
    let server = MockServer::start().await;
    get("discover/movie")
        .respond_with(status(429, "error_rate_limit.json").insert_header("retry-after", "2"))
        .expect(1)
        .mount(&server)
        .await;
    let adapter = tmdb(&server);
    let query = ListQuery {
        kind: MediaKind::Movie,
        list: ListKind::Popular,
        page: 1,
        language: "en-US",
        relevance: &Relevance::default(),
        filter: None,
    };
    assert_eq!(
        adapter.list(&query).await,
        Err(AdapterError::Throttled {
            retry_after: Some(Duration::from_secs(2))
        })
    );
    // The instance pauses: the next call does not reach TMDB.
    assert!(matches!(
        adapter.list(&query).await,
        Err(AdapterError::Throttled { .. })
    ));
}

// ---------------------------------------------------------------- reference

#[tokio::test]
async fn configuration_and_providers() {
    let server = MockServer::start().await;
    for (p, f) in [
        ("configuration", "configuration.json"),
        ("configuration/languages", "configuration_languages.json"),
        ("configuration/countries", "configuration_countries.json"),
        ("watch/providers/regions", "watch_providers_regions.json"),
        ("genre/movie/list", "genre_movie_list.json"),
        ("genre/tv/list", "genre_tv_list.json"),
    ] {
        get(p).respond_with(json(f)).expect(1).mount(&server).await;
    }
    get("watch/providers/movie")
        .and(query_param("watch_region", "CH"))
        .respond_with(json("watch_providers_movie_CH.json"))
        .expect(1)
        .mount(&server)
        .await;
    let adapter = tmdb(&server);
    let config = adapter.configuration().await.unwrap();
    // The proxy's size allowlist covers every size TMDB lists.
    for size in [
        &config.image_sizes.poster,
        &config.image_sizes.backdrop,
        &config.image_sizes.logo,
        &config.image_sizes.profile,
        &config.image_sizes.still,
    ]
    .into_iter()
    .flatten()
    {
        assert!(IMAGE_SIZES.contains(&size.as_str()), "{size}");
    }
    assert!(config.languages.iter().any(|l| l.code == "xx"));
    assert_eq!(config.countries.len(), 3);
    assert_eq!(config.provider_regions[0].code, "CH");
    assert!(
        config
            .genres
            .iter()
            .any(|g| g.kind == MediaKind::Series && g.id == 10767)
    );
    assert!(
        !config
            .genres
            .iter()
            .any(|g| g.kind == MediaKind::Movie && g.id == 10767)
    );

    let providers = adapter.providers(MediaKind::Movie, "CH").await.unwrap();
    let order: Vec<u64> = providers.iter().map(|p| p.id).collect();
    assert_eq!(order, vec![337, 1823, 8, 9]);
    assert_eq!(providers[1].logo, None);
}

// ---------------------------------------------------------------- cache

#[derive(Debug, Default)]
struct MemoryCache {
    entries: Mutex<HashMap<String, (Vec<u8>, Duration)>>,
}

impl ResponseCache for MemoryCache {
    fn get<'a>(&'a self, key: &'a str) -> starr_adapter_kit::BoxFuture<'a, Option<Vec<u8>>> {
        let hit = self.entries.lock().unwrap().get(key).map(|e| e.0.clone());
        Box::pin(async move { hit })
    }

    fn put<'a>(
        &'a self,
        key: &'a str,
        body: &'a [u8],
        ttl: Duration,
    ) -> starr_adapter_kit::BoxFuture<'a, ()> {
        self.entries
            .lock()
            .unwrap()
            .insert(key.to_owned(), (body.to_vec(), ttl));
        Box::pin(async {})
    }
}

#[tokio::test]
async fn responses_are_cached_for_their_max_age_but_never_the_probe() {
    let server = MockServer::start().await;
    get("movie/27205")
        .respond_with(json("movie_27205.json").insert_header("age", "600"))
        .expect(1)
        .mount(&server)
        .await;
    get("authentication")
        .respond_with(json("authentication.json"))
        .expect(2)
        .mount(&server)
        .await;
    let cache = Arc::new(MemoryCache::default());
    let adapter = tmdb(&server).with_cache(cache.clone());
    let first = adapter.details(&movie("27205"), "en-US").await.unwrap();
    let second = adapter.details(&movie("27205"), "en-US").await.unwrap();
    assert_eq!(first, second);
    adapter.probe().await.unwrap();
    adapter.probe().await.unwrap();

    let entries = cache.entries.lock().unwrap();
    assert_eq!(entries.len(), 1);
    let (key, (_, ttl)) = entries.iter().next().unwrap();
    assert!(key.starts_with("3/movie/27205?language=en-US&"), "{key}");
    assert!(!key.contains(TOKEN));
    // max-age 21600 minus age 600.
    assert_eq!(*ttl, Duration::from_mins(350));
    assert!(*ttl <= CACHE_POLICY.max_ttl);
}

// ---------------------------------------------------------------- images

#[tokio::test]
async fn image_cdn_fetches_validated_paths_without_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/t/p/w500/kqjL17yufvn9OVLyXYpvtyrFfak.jpg"))
        .and(header_regex("accept", "^image/webp"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/webp")
                .set_body_bytes(b"RIFF....WEBP".to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let cdn = ImageCdn::with_base(Url::parse(&format!("{}/t/p/", server.uri())).unwrap()).unwrap();
    let image = cdn
        .fetch("w500", "kqjL17yufvn9OVLyXYpvtyrFfak.jpg", true)
        .await
        .unwrap();
    assert_eq!(image.content_type.as_deref(), Some("image/webp"));
    assert_eq!(image.body, b"RIFF....WEBP");
    let requests = server.received_requests().await.unwrap();
    assert!(requests[0].headers.get("authorization").is_none());

    for (size, file) in [
        ("w500", "..%2F..%2F3%2Fconfiguration"),
        ("w500", "../kqjL17yufvn9OVLyXYpvtyrFfak.jpg"),
        ("..", "kqjL17yufvn9OVLyXYpvtyrFfak.jpg"),
        ("w501", "kqjL17yufvn9OVLyXYpvtyrFfak.jpg"),
    ] {
        assert!(matches!(
            cdn.fetch(size, file, false).await,
            Err(AdapterError::Rejected(_))
        ));
    }
    server.verify().await;
}

#[tokio::test]
async fn missing_image_is_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let cdn = ImageCdn::with_base(Url::parse(&format!("{}/t/p/", server.uri())).unwrap()).unwrap();
    assert!(matches!(
        cdn.fetch("w92", "kqjL17yufvn9OVLyXYpvtyrFfak.png", false)
            .await,
        Err(AdapterError::NotFound(_))
    ));
}
