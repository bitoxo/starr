//! TMDB adapter against responses recorded from the live API on 2026-11-01
//! (`fixtures/v3/recorded/`, `record-fixtures.sh`; lesson 10). The
//! synthetic fixtures in `contract.rs` cover scenarios live data does not
//! show reliably (duplicates within a page, empty overviews, odd values).
#![allow(clippy::unwrap_used)]

use std::collections::HashSet;

use chrono::NaiveDate;
use secrecy::SecretString;
use starr_adapter_kit::{Endpoint, ServiceClient};
use starr_adapter_tmdb::images::{IMAGE_SIZES, valid_file};
use starr_adapter_tmdb::{AUTH, DESCRIPTOR, Tmdb};
use starr_core::adapters::{AdapterError, Probe, Version};
use starr_core::catalog::{
    Catalog, CatalogEntry, CatalogRef, ExternalId, ListKind, ListQuery, MediaKind, Relevance,
    SearchQuery,
};
use url::Url;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.recorded-test";

fn recorded(name: &str) -> String {
    let file = format!(
        "{}/fixtures/v3/recorded/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file}: {e}"))
}

fn reply(status: u16, name: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .insert_header("content-type", "application/json;charset=utf-8")
        .set_body_string(recorded(name))
}

fn get(p: &str) -> wiremock::MockBuilder {
    Mock::given(method("GET"))
        .and(path(format!("/3/{p}")))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
}

fn tmdb(server: &MockServer, token: &str) -> Tmdb {
    let client = ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(&server.uri()).unwrap(),
            auth: AUTH,
            credential: SecretString::from(token.to_owned()),
        },
    )
    .unwrap();
    Tmdb::new(client).with_today(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap())
}

fn assert_proxyable(entries: &[CatalogEntry]) {
    for e in entries {
        for p in [&e.poster, &e.backdrop].into_iter().flatten() {
            assert!(valid_file(p.trim_start_matches('/')), "{p}");
        }
    }
}

#[tokio::test]
async fn probe_with_recorded_answers() {
    let server = MockServer::start().await;
    get("authentication")
        .respond_with(reply(200, "authentication"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(reply(401, "error_invalid_key"))
        .mount(&server)
        .await;
    let report = tmdb(&server, TOKEN).probe().await.unwrap();
    assert_eq!(report.version, Version::new(3, 0, 0, 0));
    assert_eq!(
        tmdb(&server, "wrong").probe().await,
        Err(AdapterError::CredentialsInvalid)
    );
}

#[tokio::test]
async fn not_found_and_page_501_map_to_the_taxonomy() {
    let server = MockServer::start().await;
    get("movie/999999999")
        .respond_with(reply(404, "error_not_found"))
        .mount(&server)
        .await;
    let adapter = tmdb(&server, TOKEN);
    let item = CatalogRef {
        kind: MediaKind::Movie,
        id: "999999999".into(),
    };
    assert!(matches!(
        adapter.details(&item, "en-US").await,
        Err(AdapterError::NotFound(_))
    ));
    // The recorded 400 for page 501 proves the cap; the adapter never asks.
    assert!(recorded("error_page_501").contains("\"status_code\":22"));
}

#[tokio::test]
async fn reference_data_decodes_and_sizes_fit_the_proxy_allowlist() {
    let server = MockServer::start().await;
    for (p, f) in [
        ("configuration", "configuration"),
        ("configuration/languages", "configuration_languages"),
        ("configuration/countries", "configuration_countries"),
        ("watch/providers/regions", "watch_providers_regions"),
        ("genre/movie/list", "genre_movie_list"),
        ("genre/tv/list", "genre_tv_list"),
    ] {
        get(p).respond_with(reply(200, f)).mount(&server).await;
    }
    get("watch/providers/movie")
        .and(query_param("watch_region", "CH"))
        .respond_with(reply(200, "watch_providers_movie_CH"))
        .mount(&server)
        .await;
    let adapter = tmdb(&server, TOKEN);
    let config = adapter.configuration().await.unwrap();
    let sizes = &config.image_sizes;
    for size in [
        &sizes.poster,
        &sizes.backdrop,
        &sizes.logo,
        &sizes.profile,
        &sizes.still,
    ]
    .into_iter()
    .flatten()
    {
        assert!(IMAGE_SIZES.contains(&size.as_str()), "{size}");
    }
    // Counts as documented in capabilities.md.
    assert_eq!(config.languages.len(), 187);
    assert_eq!(config.countries.len(), 251);
    assert_eq!(config.provider_regions.len(), 139);
    for id in [10767, 10763, 10766, 10764] {
        assert!(
            config
                .genres
                .iter()
                .any(|g| g.kind == MediaKind::Series && g.id == id)
        );
    }
    let providers = adapter.providers(MediaKind::Movie, "CH").await.unwrap();
    assert_eq!(providers.len(), 84);
    // Netflix leads Switzerland's display order (display_priorities["CH"]).
    assert_eq!(providers[0].id, 8);
}

#[tokio::test]
async fn trending_repeats_across_pages_are_dropped() {
    let server = MockServer::start().await;
    for page in [1, 2] {
        get("trending/movie/week")
            .and(query_param("page", page.to_string()))
            .respond_with(reply(200, &format!("trending_movie_week_page{page}")))
            .mount(&server)
            .await;
    }
    let adapter = tmdb(&server, TOKEN);
    let relevance = Relevance::default();
    let query = |page| ListQuery {
        kind: MediaKind::Movie,
        list: ListKind::Trending,
        page,
        language: "en-US",
        relevance: &relevance,
        filter: None,
    };
    let first = adapter.list(&query(1)).await.unwrap();
    let second = adapter.list(&query(2)).await.unwrap();
    assert_eq!(first.items.len(), 20);
    // Id 1300968 was on both recorded pages; page 2 keeps 19.
    assert_eq!(second.items.len(), 19);
    let ids: HashSet<_> = first.items.iter().map(|e| &e.item.id).collect();
    assert!(second.items.iter().all(|e| !ids.contains(&e.item.id)));
    assert_proxyable(&first.items);
}

#[tokio::test]
async fn discover_lists_decode_and_respect_the_cap() {
    let server = MockServer::start().await;
    get("discover/movie")
        .and(query_param("page", "500"))
        .respond_with(reply(200, "discover_movie_popular_page500"))
        .mount(&server)
        .await;
    get("discover/movie")
        .and(query_param("vote_count.gte", "5000"))
        .respond_with(reply(200, "discover_movie_top_rated"))
        .mount(&server)
        .await;
    get("discover/tv")
        .and(query_param("watch_region", "CH"))
        .respond_with(reply(200, "discover_tv_on_services_CH"))
        .mount(&server)
        .await;
    let adapter = tmdb(&server, TOKEN);
    let none = Relevance::default();
    let popular = adapter
        .list(&ListQuery {
            kind: MediaKind::Movie,
            list: ListKind::Popular,
            page: 500,
            language: "en-US",
            relevance: &none,
            filter: None,
        })
        .await
        .unwrap();
    assert_eq!(popular.items.len(), 20);
    assert_eq!(popular.next, None, "total_pages 1001, cap 500");

    let top = adapter
        .list(&ListQuery {
            kind: MediaKind::Movie,
            list: ListKind::TopRated,
            page: 1,
            language: "en-US",
            relevance: &none,
            filter: None,
        })
        .await
        .unwrap();
    assert!(top.items.iter().all(|e| e.vote_count >= 5000));

    let relevance = Relevance {
        original_languages: vec!["en".into(), "de".into()],
        provider_region: Some("CH".into()),
        providers: vec![8, 337],
        release_region: None,
        excluded_genres: starr_adapter_tmdb::DEFAULT_SERIES_EXCLUDED_GENRES.to_vec(),
        ..Relevance::default()
    };
    let on_services = adapter
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
    assert_ne!(on_services.items.len(), 0);
    assert_proxyable(&on_services.items);
}

#[tokio::test]
async fn search_and_details_map_recorded_shapes() {
    let server = MockServer::start().await;
    get("search/multi")
        .respond_with(reply(200, "search_multi_inception"))
        .mount(&server)
        .await;
    get("movie/27205")
        .respond_with(reply(200, "movie_27205"))
        .mount(&server)
        .await;
    get("tv/1399")
        .respond_with(reply(200, "tv_1399"))
        .mount(&server)
        .await;
    get("tv/93405")
        .and(query_param("language", "de-DE"))
        .respond_with(reply(200, "tv_93405_de-DE"))
        .expect(1)
        .mount(&server)
        .await;
    let adapter = tmdb(&server, TOKEN);

    let found = adapter
        .search(&SearchQuery {
            text: "inception",
            kinds: &MediaKind::ALL,
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    assert_eq!(found.items[0].item.id, "27205");
    assert_eq!(found.next, None);
    assert_proxyable(&found.items);

    let movie = adapter
        .details(
            &CatalogRef {
                kind: MediaKind::Movie,
                id: "27205".into(),
            },
            "en-US",
        )
        .await
        .unwrap();
    assert_eq!(movie.entry.title, "Inception");
    assert!(
        movie
            .external_ids
            .contains(&ExternalId::Imdb("tt1375666".into()))
    );
    assert!(movie.trailer.is_some());
    let rating = |c: &str| {
        movie
            .certifications
            .iter()
            .find(|x| x.country == c)
            .map(|x| x.rating.clone())
    };
    assert_eq!(rating("CH").as_deref(), Some("14"));
    assert_eq!(rating("US").as_deref(), Some("PG-13"));
    assert_ne!(movie.watch_offers.len(), 0);

    let series = adapter
        .details(
            &CatalogRef {
                kind: MediaKind::Series,
                id: "1399".into(),
            },
            "en-US",
        )
        .await
        .unwrap();
    assert!(series.external_ids.contains(&ExternalId::Tvdb(121_361)));
    let numbers: Vec<u32> = series.seasons().map(|s| s.number).collect();
    assert_eq!(numbers, (0..=8).collect::<Vec<_>>());
    assert!(series.seasons().next().unwrap().is_special);
    assert_eq!(series.seasons().nth(8).unwrap().episode_count, 6);

    // de-CH is sent as de-DE, which TMDB translates (recorded de-CH: Hangul
    // title, empty overview); no English fallback request is needed.
    let squid = adapter
        .details(
            &CatalogRef {
                kind: MediaKind::Series,
                id: "93405".into(),
            },
            "de-CH",
        )
        .await
        .unwrap();
    assert_eq!(squid.entry.title, "Squid Game");
    assert_ne!(squid.entry.overview, "");
    assert!(recorded("tv_93405_de-CH").contains("\"overview\":\"\""));
}
