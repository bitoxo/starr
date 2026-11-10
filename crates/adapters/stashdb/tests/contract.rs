//! `StashDB` adapter against a `wiremock` fake replaying `fixtures/`
//! (lesson 10). `fixtures/0.10.3/recorded/` holds sanitised read-only
//! answers of stashdb.org (recorded 2026-11-06, README there); the files
//! directly in `fixtures/` are **derived by hand from the stash-box GraphQL
//! schema** (`graphql/schema/*.graphql`, read 2026-11-04) for what the
//! recording did not show (a `VOTE` or `EDIT` role, partial and missing
//! dates, portrait covers, invalid ids, other GraphQL errors, a CDN
//! redirect) and say so in their `extensions.fixture`. Starr's tests never
//! contact `StashDB` or any stash-box; every fixture holds neutral
//! placeholders only ("Scene A", "Performer P", "Studio S").
//!
//! Every mock requires Starr's User-Agent, `POST /graphql` and the key in
//! the `ApiKey` header, so a request without them fails the test.
#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use secrecy::SecretString;
use starr_adapter_kit::{BoxFuture, ClientOptions, Endpoint, ResponseCache, ServiceClient};
use starr_adapter_stashdb::{
    AUTH, CACHE_PREFIX, DESCRIPTOR, NOT_READ_ONLY, RATE_LIMIT, SearchCounts, StashDb,
    client_options,
};
use starr_core::adapters::{AdapterError, Probe, RateLimit, SecretClass, Version};
use starr_core::catalog::{
    Catalog, CatalogRef, Child, ExternalId, ListKind, ListQuery, MediaKind, Relevance, SearchQuery,
};
use url::Url;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const USER_AGENT: &str = concat!(
    "Starr/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/bitoxo/starr )"
);
const KEY: &str = "placeholder-catalog-key";
const SCENE_A: &str = "5ce0e000-0000-4000-8000-00000000000a";
const SCENE_B: &str = "5ce0e000-0000-4000-8000-00000000000b";
const PERFORMER_P: &str = "9e7f0000-0000-4000-8000-0000000000aa";
const PERFORMER_Q: &str = "9e7f0000-0000-4000-8000-0000000000bb";
const STUDIO_S: &str = "57d10000-0000-4000-8000-0000000000aa";
const IMAGE_2: &str = "1a6e0000-0000-4000-8000-000000000002";

const FAST: RateLimit = RateLimit {
    requests: 100,
    per: Duration::from_secs(1),
};

fn fixture(name: &str) -> String {
    let file = format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&file).unwrap()
}

fn client(uri: &str) -> ServiceClient {
    ServiceClient::with_options(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(uri).unwrap(),
            auth: AUTH,
            credential: SecretString::from(KEY.to_owned()),
        },
        ClientOptions {
            rate_limit: FAST,
            ..client_options(100)
        },
    )
    .unwrap()
}

/// Answer the GraphQL operation whose body contains `marker` with `file`.
async fn answer(server: &MockServer, marker: &str, file: &str) {
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(header("user-agent", USER_AGENT))
        .and(header("apikey", KEY))
        .and(body_string_contains(marker))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(fixture(file)),
        )
        .mount(server)
        .await;
}

#[derive(Debug, Default)]
struct MemoryCache(Mutex<HashMap<String, Vec<u8>>>);

impl ResponseCache for MemoryCache {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Vec<u8>>> {
        let hit = self.0.lock().unwrap().get(key).cloned();
        Box::pin(async move { hit })
    }

    fn put<'a>(&'a self, key: &'a str, body: &'a [u8], _ttl: Duration) -> BoxFuture<'a, ()> {
        self.0.lock().unwrap().insert(key.to_owned(), body.to_vec());
        Box::pin(async {})
    }
}

fn scene(id: &str) -> CatalogRef {
    CatalogRef {
        kind: MediaKind::Scene,
        id: id.into(),
    }
}

#[test]
fn descriptor_declares_the_budget_lanes_and_the_catalog_token_class() {
    assert_eq!(DESCRIPTOR.type_id, "stashdb");
    assert_eq!(
        RATE_LIMIT,
        RateLimit {
            requests: 2,
            per: Duration::from_secs(1)
        }
    );
    assert_eq!(DESCRIPTOR.budgets.rate_limit, RATE_LIMIT);
    assert_eq!(DESCRIPTOR.lists, [ListKind::New, ListKind::Trending]);
    assert_eq!(DESCRIPTOR.secret_class, SecretClass::CatalogReadToken);
    // Web 1.4 / worker 0.6 requests per second in split mode.
    let web = client_options(70);
    assert_eq!(web.rate_limit.requests, 2);
    assert_eq!(web.rate_limit.per, Duration::from_secs(1) * 100 / 70);
    assert_eq!(
        client_options(30).rate_limit.per,
        Duration::from_secs(1) * 100 / 30
    );
    let full = client_options(100);
    assert!(
        full.private_content,
        "paths and bodies never reach the logs"
    );
    assert!(full.unavailable_is_throttle);
}

#[tokio::test]
async fn the_probe_accepts_a_read_only_key_and_reports_the_version() {
    let server = MockServer::start().await;
    answer(&server, "query Probe", "probe_read.json").await;
    let report = StashDb::new(client(&server.uri())).probe().await.unwrap();
    assert_eq!(report.version, Version::new(0, 10, 3, 0));
}

#[tokio::test]
async fn the_probe_refuses_a_key_that_could_write() {
    let server = MockServer::start().await;
    answer(&server, "query Probe", "probe_edit.json").await;
    let db = StashDb::new(client(&server.uri()));
    let err = db.probe().await.unwrap_err();
    assert_eq!(err, AdapterError::Rejected(NOT_READ_ONLY.to_owned()));
    let check = db.check_key().await.unwrap();
    assert!(!check.read_only());
    assert_eq!(check.roles, ["READ", "VOTE", "EDIT"]);
}

#[tokio::test]
async fn refused_keys_are_credentials_invalid_and_other_errors_quote_nothing() {
    let server = MockServer::start().await;
    answer(&server, "query FindScene", "error_not_authorized.json").await;
    answer(&server, "query SearchScenes", "error_other.json").await;
    let db = StashDb::new(client(&server.uri()));
    assert_eq!(
        db.details(&scene(SCENE_A), "en-US").await.unwrap_err(),
        AdapterError::CredentialsInvalid
    );
    let err = db
        .search(&SearchQuery {
            text: "Scene",
            kinds: &[MediaKind::Scene],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap_err();
    assert!(matches!(err, AdapterError::Unknown(_)), "{err:?}");
    assert!(!err.to_string().contains("Scene A"), "{err}");

    let unauthorized = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&unauthorized)
        .await;
    assert_eq!(
        StashDb::new(client(&unauthorized.uri()))
            .probe()
            .await
            .unwrap_err(),
        AdapterError::CredentialsInvalid
    );
}

#[tokio::test]
async fn throttling_pauses_and_says_nothing_of_the_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "7"))
        .mount(&server)
        .await;
    let err = StashDb::new(client(&server.uri()))
        .details(&scene(SCENE_A), "en-US")
        .await
        .unwrap_err();
    assert!(matches!(err, AdapterError::Throttled { .. }), "{err:?}");
}

#[tokio::test]
async fn search_finds_scenes_performers_and_studios() {
    let server = MockServer::start().await;
    answer(&server, "query SearchScenes", "search_scenes.json").await;
    answer(&server, "query SearchPerformers", "search_performers.json").await;
    answer(&server, "query QueryStudios", "query_studios.json").await;
    let db = StashDb::new(client(&server.uri()));
    let page = db
        .search(&SearchQuery {
            text: " Scene ",
            kinds: &[MediaKind::Scene, MediaKind::Performer, MediaKind::Studio],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    let found: Vec<_> = page
        .items
        .iter()
        .map(|e| (e.item.kind, e.item.id.as_str(), e.title.as_str()))
        .collect();
    // The scene with an invalid id is left out.
    assert_eq!(
        found,
        [
            (MediaKind::Scene, SCENE_A, "Scene A"),
            (MediaKind::Performer, PERFORMER_P, "Performer P"),
            (MediaKind::Studio, STUDIO_S, "Studio S"),
        ]
    );
    // 26 scenes in all: there is a second page.
    assert_eq!(page.next, Some(2));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let body = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(body.contains(r#""term":"Scene""#), "{body}");
    // Only identifying and display fields are asked for.
    for field in ["details", "tags", "fingerprints", "urls", "url"] {
        assert!(
            !body.contains(&format!(" {field} ")),
            "{field} asked for: {body}"
        );
    }

    // Blank text and pages beyond the cap ask nothing.
    for (text, page) in [("  ", 1), ("Scene", 0), ("Scene", 21)] {
        let empty = db
            .search(&SearchQuery {
                text,
                kinds: &[MediaKind::Scene],
                page,
                language: "en-US",
            })
            .await
            .unwrap();
        assert_eq!(empty.items, []);
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn lanes_sort_by_date_and_trend() {
    let server = MockServer::start().await;
    answer(&server, r#""sort":"DATE""#, "lane_new.json").await;
    answer(&server, r#""sort":"TRENDING""#, "lane_trending.json").await;
    let db = StashDb::new(client(&server.uri()));
    let relevance = Relevance::default();
    let lane = async |list, kind| {
        db.list(&ListQuery {
            kind,
            list,
            page: 1,
            language: "en-US",
            relevance: &relevance,
            filter: None,
        })
        .await
        .unwrap()
    };
    let new = lane(ListKind::New, MediaKind::Scene).await;
    let ids: Vec<_> = new.items.iter().map(|e| e.item.id.as_str()).collect();
    assert_eq!(ids[..2], [SCENE_A, SCENE_B]);
    assert_eq!(new.next, None);
    // Partial dates count as their first day; no date stays unknown.
    assert_eq!(
        new.items[1].release_date,
        NaiveDate::from_ymd_opt(2026, 9, 1)
    );
    assert_eq!(new.items[2].release_date, None);
    assert_eq!(new.items[2].poster, None);
    let trending = lane(ListKind::Trending, MediaKind::Scene).await;
    assert_eq!(trending.items[0].item.id, SCENE_B);
    assert_eq!(trending.next, Some(2));
    // Other kinds and lists ask nothing.
    let before = server.received_requests().await.unwrap().len();
    assert_eq!(lane(ListKind::Popular, MediaKind::Scene).await.items, []);
    assert_eq!(lane(ListKind::New, MediaKind::Performer).await.items, []);
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn a_scene_has_its_studio_performers_running_time_and_a_landscape_cover() {
    let server = MockServer::start().await;
    answer(&server, "query FindScene", "find_scene.json").await;
    let details = StashDb::new(client(&server.uri()))
        .details(&scene(SCENE_A), "en-US")
        .await
        .unwrap();
    let entry = &details.entry;
    assert_eq!(entry.title, "Scene A");
    assert_eq!(entry.release_date, NaiveDate::from_ymd_opt(2026, 10, 30));
    // The first image is portrait: the landscape one is the cover.
    assert_eq!(
        entry.poster.as_deref(),
        Some(format!("stashdb:{IMAGE_2}").as_str())
    );
    let facts = entry.scene.as_ref().unwrap();
    assert_eq!(facts.duration_seconds, Some(1800));
    assert_eq!(facts.studio.as_ref().unwrap().name, "Studio S");
    let performers: Vec<_> = facts
        .performers
        .iter()
        .map(|p| p.item.id.as_str())
        .collect();
    assert_eq!(performers, [PERFORMER_P, PERFORMER_Q]);
    assert!(
        details
            .external_ids
            .contains(&ExternalId::StashScene(SCENE_A.parse().unwrap()))
    );
    assert!(
        details
            .external_ids
            .contains(&ExternalId::StashStudio(STUDIO_S.parse().unwrap()))
    );
    // The description (spec 0008 Q14), asked for by the scene page only,
    // as trimmed plain text without control characters.
    assert_eq!(
        facts.details.as_deref(),
        Some("Details D: a placeholder description of Scene A.\n\nSecond paragraph.")
    );
    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(body.contains("...SceneParts details"), "{body}");
}

#[tokio::test]
async fn lists_and_search_never_ask_for_details() {
    let server = MockServer::start().await;
    answer(&server, "query SearchScenes", "search_scenes.json").await;
    answer(&server, r#""sort":"DATE""#, "lane_new.json").await;
    let db = StashDb::new(client(&server.uri()));
    db.search(&SearchQuery {
        text: "Scene",
        kinds: &[MediaKind::Scene],
        page: 1,
        language: "en-US",
    })
    .await
    .unwrap();
    let lane = db
        .list(&ListQuery {
            kind: MediaKind::Scene,
            list: ListKind::New,
            page: 1,
            language: "en-US",
            relevance: &Relevance::default(),
            filter: None,
        })
        .await
        .unwrap();
    assert!(
        lane.items
            .iter()
            .all(|e| e.scene.as_ref().is_some_and(|f| f.details.is_none()))
    );
    for request in server.received_requests().await.unwrap() {
        let body = String::from_utf8(request.body.clone()).unwrap();
        assert!(!body.contains("details"), "{body}");
    }
}

#[tokio::test]
async fn more_from_the_studio_is_the_studio_pages_first_page_from_one_cache_entry() {
    let server = MockServer::start().await;
    answer(&server, "query FindStudio", "find_studio.json").await;
    answer(&server, r#""modifier":"INCLUDES""#, "entity_scenes.json").await;
    let cache = Arc::new(MemoryCache::default());
    let db = StashDb::new(client(&server.uri())).with_cache(cache.clone());
    let studio = CatalogRef {
        kind: MediaKind::Studio,
        id: STUDIO_S.into(),
    };
    db.details(&studio, "en-US").await.unwrap();
    let before = server.received_requests().await.unwrap().len();
    let scenes = db.studio_scenes(&studio).await.unwrap();
    assert_eq!(scenes[0].item.id, SCENE_A);
    // The studio page's first page was cached: nothing new was sent.
    assert_eq!(server.received_requests().await.unwrap().len(), before);
    // Only studios, by canonical id.
    for bad in [
        CatalogRef {
            kind: MediaKind::Performer,
            id: PERFORMER_P.into(),
        },
        CatalogRef {
            kind: MediaKind::Studio,
            id: "../x".into(),
        },
    ] {
        assert!(matches!(
            db.studio_scenes(&bad).await,
            Err(AdapterError::NotFound(_))
        ));
    }
}

#[tokio::test]
async fn counted_search_reports_the_total_of_each_kind() {
    let server = MockServer::start().await;
    answer(&server, "query SearchScenes", "search_scenes.json").await;
    answer(&server, "query SearchPerformers", "search_performers.json").await;
    answer(&server, "query QueryStudios", "query_studios.json").await;
    let db = StashDb::new(client(&server.uri()));
    let (page, counts) = db
        .search_counted(&SearchQuery {
            text: "Scene",
            kinds: &[MediaKind::Scene, MediaKind::Performer, MediaKind::Studio],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);
    assert_eq!(
        counts,
        SearchCounts {
            scenes: 26,
            performers: 1,
            studios: 1
        }
    );
    // Kinds not asked for count nothing and send nothing.
    let (_, only) = db
        .search_counted(&SearchQuery {
            text: "Scene",
            kinds: &[MediaKind::Studio],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    assert_eq!(
        only,
        SearchCounts {
            scenes: 0,
            performers: 0,
            studios: 1
        }
    );
    // Blank text counts nothing.
    let (_, none) = db
        .search_counted(&SearchQuery {
            text: " ",
            kinds: &[MediaKind::Scene],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    assert_eq!(none, SearchCounts::default());
}

#[tokio::test]
async fn search_scenes_of_an_entity_sends_text_and_filter_and_keeps_its_scenes_only() {
    let server = MockServer::start().await;
    // A server that ignores the filter: a scene of another studio comes
    // back too.
    let mut answer_body: serde_json::Value =
        serde_json::from_str(&fixture("entity_scenes.json")).unwrap();
    let scenes = answer_body["data"]["queryScenes"]["scenes"]
        .as_array_mut()
        .unwrap();
    scenes[1]["studio"] =
        serde_json::json!({ "id": "57d10000-0000-4000-8000-0000000000bb", "name": "Studio T" });
    scenes[1]["performers"] = serde_json::json!([]);
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains(r#""text":"Scene""#))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer_body))
        .mount(&server)
        .await;
    let db = StashDb::new(client(&server.uri()));
    let studio = CatalogRef {
        kind: MediaKind::Studio,
        id: STUDIO_S.into(),
    };
    let (page, count) = db.search_scenes_of("Scene", &studio, 1).await.unwrap();
    assert_eq!(count, 3);
    assert_ne!(page.items.len(), 0);
    assert!(page.items.iter().all(|e| {
        e.scene
            .as_ref()
            .and_then(|f| f.studio.as_ref())
            .is_some_and(|s| s.item.id == STUDIO_S)
    }));
    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(
        body.contains(&format!(
            r#""studios":{{"modifier":"INCLUDES","value":["{STUDIO_S}"]}}"#
        )),
        "{body}"
    );
    // Blank text or another kind sends nothing.
    assert_eq!(
        db.search_scenes_of("  ", &studio, 1)
            .await
            .unwrap()
            .0
            .items
            .len(),
        0
    );
    assert!(
        db.search_scenes_of("Scene", &scene(SCENE_A), 1)
            .await
            .is_err()
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn unknown_or_invalid_ids_are_not_found_without_naming_them() {
    let server = MockServer::start().await;
    answer(
        &server,
        "query FindScene",
        "0.10.3/recorded/find_scene_unknown.json",
    )
    .await;
    let db = StashDb::new(client(&server.uri()));
    let err = db.details(&scene(SCENE_B), "en-US").await.unwrap_err();
    assert!(matches!(err, AdapterError::NotFound(_)), "{err:?}");
    assert!(!err.to_string().contains(SCENE_B), "{err}");
    for bad in ["x", "5CE0E000-0000-4000-8000-00000000000A", "../graphql"] {
        assert!(matches!(
            db.details(&scene(bad), "en-US").await,
            Err(AdapterError::NotFound(_))
        ));
    }
    let movie = CatalogRef {
        kind: MediaKind::Movie,
        id: SCENE_A.into(),
    };
    assert!(matches!(
        db.details(&movie, "en-US").await,
        Err(AdapterError::NotFound(_))
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn performer_and_studio_pages_list_their_scenes_newest_first() {
    let server = MockServer::start().await;
    answer(&server, "query FindPerformer", "find_performer.json").await;
    answer(&server, "query FindStudio", "find_studio.json").await;
    answer(&server, r#""modifier":"INCLUDES""#, "entity_scenes.json").await;
    let db = StashDb::new(client(&server.uri()));
    for (kind, id, name, filter) in [
        (
            MediaKind::Performer,
            PERFORMER_P,
            "Performer P",
            "performers",
        ),
        (MediaKind::Studio, STUDIO_S, "Studio S", "studios"),
    ] {
        let details = db
            .details(
                &CatalogRef {
                    kind,
                    id: id.into(),
                },
                "en-US",
            )
            .await
            .unwrap();
        assert_eq!(details.entry.title, name);
        assert!(details.entry.poster.is_some());
        let scenes: Vec<_> = details
            .children
            .iter()
            .map(|c| match c {
                Child::Scene(s) => s.item.id.as_str(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(scenes[..2], [SCENE_A, SCENE_B]);
        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8(requests.last().unwrap().body.clone()).unwrap();
        assert!(
            body.contains(&format!(
                r#""{filter}":{{"modifier":"INCLUDES","value":["{id}"]}}"#
            )),
            "{body}"
        );
        assert!(body.contains(r#""sort":"DATE""#) && body.contains(r#""direction":"DESC""#));
    }
    // Three scenes in all: one page each, no more.
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn answers_are_cached_under_prefixed_hashes() {
    let server = MockServer::start().await;
    answer(&server, "query FindScene", "find_scene.json").await;
    let cache = Arc::new(MemoryCache::default());
    let db = StashDb::new(client(&server.uri())).with_cache(cache.clone());
    for _ in 0..2 {
        db.details(&scene(SCENE_A), "en-US").await.unwrap();
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let keys: Vec<String> = cache.0.lock().unwrap().keys().cloned().collect();
    assert_eq!(keys.len(), 1);
    assert!(keys[0].starts_with(CACHE_PREFIX), "{keys:?}");
    assert!(!keys[0].contains(SCENE_A), "{keys:?}");
    // The probe is never cached.
    answer(&server, "query Probe", "probe_read.json").await;
    db.probe().await.unwrap();
    db.probe().await.unwrap();
    assert_eq!(cache.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn error_answers_are_never_cached() {
    let server = MockServer::start().await;
    answer(&server, "query FindScene", "error_not_authorized.json").await;
    let cache = Arc::new(MemoryCache::default());
    let db = StashDb::new(client(&server.uri())).with_cache(cache.clone());
    for _ in 0..2 {
        assert!(db.details(&scene(SCENE_A), "en-US").await.is_err());
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    assert_eq!(cache.0.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn images_are_fetched_by_id_without_the_key() {
    let server = MockServer::start().await;
    let cdn = format!("{}/cdn/{IMAGE_2}.jpg", server.uri());
    Mock::given(method("GET"))
        .and(path(format!("/images/{IMAGE_2}")))
        .and(query_param("size", "600"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", cdn.as_str()))
        .mount(&server)
        .await;
    Mock::given(path(format!("/cdn/{IMAGE_2}.jpg")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(vec![1, 2, 3]),
        )
        .mount(&server)
        .await;
    let db = StashDb::new(client(&server.uri()));
    let image = db.image("600", IMAGE_2).await.unwrap();
    assert_eq!(image.body, [1, 2, 3]);
    for (size, reference) in [("500", IMAGE_2), ("600", "../graphql"), ("full", IMAGE_2)] {
        assert!(matches!(
            db.image(size, reference).await,
            Err(AdapterError::Rejected(_))
        ));
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(request.headers.get("apikey").is_none(), "{}", request.url);
        assert_eq!(request.headers.get("user-agent").unwrap(), USER_AGENT);
    }
}

// --- recorded from StashDB 0.10.3 ---------------------------------------------
//
// `fixtures/0.10.3/recorded/`: read-only queries against stashdb.org with
// a fresh READ account, 2026-11-06, sanitised (README there). Status and
// the headers that matter are in `responses.json`.

const RECORDED: &str = "0.10.3/recorded";
/// Scene D: in the recorded Whisparr with its file.
const SCENE_D: &str = "5ce00002-0000-4000-8000-000000000002";
const SCENE_D_IMAGE: &str = "1a6e0002-0000-4000-8000-000000000002";
const PERFORMER_Q_REC: &str = "9e7f0002-0000-4000-8000-000000000002";
const STUDIO_S_REC: &str = "57d10001-0000-4000-8000-000000000001";

fn recorded(name: &str) -> String {
    fixture(&format!("{RECORDED}/{name}.json"))
}

/// Status and headers of a recorded answer.
fn recorded_response(name: &str) -> (u16, serde_json::Value) {
    let all: serde_json::Value = serde_json::from_str(&recorded("responses")).unwrap();
    let response = &all[name];
    (
        u16::try_from(response["status"].as_u64().unwrap()).unwrap(),
        response["headers"].clone(),
    )
}

async fn answer_recorded(server: &MockServer, marker: &str, name: &str) {
    let (status, _) = recorded_response(name);
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(header("apikey", KEY))
        .and(body_string_contains(marker))
        .respond_with(
            ResponseTemplate::new(status)
                .insert_header("content-type", "application/json")
                .set_body_string(recorded(name)),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn recorded_probe_reads_version_and_the_read_role() {
    let server = MockServer::start().await;
    answer_recorded(&server, "query Probe", "probe").await;
    let db = StashDb::new(client(&server.uri()));
    let check = db.check_key().await.unwrap();
    assert_eq!(check.version, Version::new(0, 10, 3, 0));
    assert_eq!(check.roles, ["READ"]);
    assert!(check.read_only());
    assert_eq!(db.probe().await.unwrap().version, check.version);
}

/// Recorded: without a key `StashDB` answers HTTP 200 with a GraphQL "not
/// authorized" error per field; a wrong key gets HTTP 401 with an empty
/// body. Both are a refused credential.
#[tokio::test]
async fn recorded_refusals_are_credentials_invalid() {
    let server = MockServer::start().await;
    answer_recorded(&server, "query Probe", "probe_without_key").await;
    assert_eq!(recorded_response("probe_without_key").0, 200);
    assert_eq!(
        StashDb::new(client(&server.uri()))
            .probe()
            .await
            .unwrap_err(),
        AdapterError::CredentialsInvalid
    );

    let (status, headers) = recorded_response("probe_bad_key");
    assert_eq!(
        (status, headers["Content-Length"].as_str()),
        (401, Some("0"))
    );
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(status))
        .mount(&server)
        .await;
    assert_eq!(
        StashDb::new(client(&server.uri()))
            .probe()
            .await
            .unwrap_err(),
        AdapterError::CredentialsInvalid
    );
}

#[tokio::test]
async fn a_recorded_scene_has_its_facts_and_an_unknown_one_is_not_found() {
    let server = MockServer::start().await;
    answer_recorded(&server, SCENE_D, "find_scene_d").await;
    answer_recorded(&server, "query FindScene", "find_scene_unknown").await;
    let db = StashDb::new(client(&server.uri()));
    let details = db.details(&scene(SCENE_D), "en-US").await.unwrap();
    let entry = &details.entry;
    assert_eq!(entry.title, "Scene D");
    // `release_date` exists on 0.10.3 (no GraphQL error).
    assert_eq!(entry.release_date, NaiveDate::from_ymd_opt(2025, 3, 15));
    assert_eq!(
        entry.poster.as_deref(),
        Some(format!("stashdb:{SCENE_D_IMAGE}").as_str())
    );
    let facts = entry.scene.as_ref().unwrap();
    assert_eq!(facts.duration_seconds, Some(1620));
    assert_eq!(facts.studio.as_ref().unwrap().item.id, STUDIO_S_REC);
    assert_eq!(facts.performers.len(), 2);
    assert!(details.external_ids.contains(&ExternalId::StashPerformer(
        PERFORMER_Q_REC.parse().unwrap()
    )));
    let unknown = "5ce0ffff-0000-4000-8000-00000000ffff";
    assert!(matches!(
        db.details(&scene(unknown), "en-US").await,
        Err(AdapterError::NotFound(_))
    ));
}

/// Every recorded `queryScenes` sort reads as a lane page: three scenes
/// kept of 25, `count` as recorded, so there is a next page.
#[tokio::test]
async fn recorded_scene_lists_read_as_lanes() {
    for name in [
        "query_scenes_date",
        "query_scenes_trending",
        "query_scenes_created_at",
        "query_scenes_performer_date",
        "query_scenes_studio_date",
    ] {
        let server = MockServer::start().await;
        answer_recorded(&server, "query QueryScenes", name).await;
        let relevance = Relevance::default();
        let page = StashDb::new(client(&server.uri()))
            .list(&ListQuery {
                kind: MediaKind::Scene,
                list: ListKind::New,
                page: 1,
                language: "en-US",
                relevance: &relevance,
                filter: None,
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 3, "{name}");
        assert_eq!(page.next, Some(2), "{name}");
        for entry in &page.items {
            assert!(entry.release_date.is_some(), "{name}");
            assert!(entry.poster.is_some(), "{name}");
            assert!(entry.scene.as_ref().unwrap().studio.is_some(), "{name}");
        }
    }
}

#[tokio::test]
async fn recorded_performer_and_studio_pages_read_their_scenes() {
    let server = MockServer::start().await;
    answer_recorded(&server, "query FindPerformer", "find_performer").await;
    answer_recorded(&server, "query FindStudio", "find_studio").await;
    // Page 1 is the recorded page; only it was recorded, so later pages
    // answer empty with the same count.
    for (filter, name, count) in [
        ("performers", "query_scenes_performer_date", 250),
        ("studios", "query_scenes_studio_date", 300),
    ] {
        Mock::given(method("POST"))
            .and(body_string_contains(format!(r#""{filter}":"#)))
            .and(body_string_contains(r#""page":1,"#))
            .respond_with(ResponseTemplate::new(200).set_body_string(recorded(name)))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains(format!(r#""{filter}":"#)))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"data":{{"queryScenes":{{"count":{count},"scenes":[]}}}}}}"#
            )))
            .with_priority(2)
            .mount(&server)
            .await;
    }
    let db = StashDb::new(client(&server.uri()));
    for (kind, id, name) in [
        (MediaKind::Performer, PERFORMER_Q_REC, "Performer Q"),
        // The studio's logo is recorded with width and height -1.
        (MediaKind::Studio, STUDIO_S_REC, "Studio S"),
    ] {
        let details = db
            .details(
                &CatalogRef {
                    kind,
                    id: id.into(),
                },
                "en-US",
            )
            .await
            .unwrap();
        assert_eq!(details.entry.title, name);
        assert!(details.entry.poster.is_some(), "{name}");
        // The page says there are more: the next one is asked for, and the
        // empty answer ends the list.
        assert_eq!(details.children.len(), 3, "{name}");
    }
    // Two lookups and 2 + 2 scene pages.
    assert_eq!(server.received_requests().await.unwrap().len(), 6);
}

#[tokio::test]
async fn recorded_searches_read_scenes_performers_and_studios() {
    let server = MockServer::start().await;
    answer_recorded(&server, "query SearchScenes", "search_scenes").await;
    answer_recorded(&server, "query SearchPerformers", "search_performers").await;
    answer_recorded(&server, "query QueryStudios", "query_studios").await;
    let page = StashDb::new(client(&server.uri()))
        .search(&SearchQuery {
            text: "Studio",
            kinds: &[MediaKind::Scene, MediaKind::Performer, MediaKind::Studio],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    let kinds: Vec<_> = page.items.iter().map(|e| e.item.kind).collect();
    assert_eq!(
        kinds,
        [
            [MediaKind::Scene; 3].as_slice(),
            &[MediaKind::Performer; 3],
            &[MediaKind::Studio; 3]
        ]
        .concat()
    );
    // A studio without any image has no poster.
    assert_eq!(page.items[7].poster, None);
    assert_eq!(page.next, Some(2));
}

/// Recorded: `/images/<id>?size=300|600` answers 200 `image/jpeg` without
/// the key, directly (no CDN redirect), cacheable for a long time.
#[tokio::test]
async fn recorded_images_are_served_directly_without_the_key() {
    for size in ["300", "600"] {
        let (status, headers) = recorded_response(&format!("image_without_key_{size}"));
        assert_eq!(status, 200);
        assert_eq!(headers["Content-Type"], "image/jpeg");
        assert_eq!(headers["Cache-Control"], "max-age=604800000");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/images/{SCENE_D_IMAGE}")))
            .and(query_param("size", size))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("content-type", "image/jpeg")
                    .insert_header("cache-control", "max-age=604800000")
                    .set_body_bytes(vec![0xff, 0xd8, 0xff]),
            )
            .mount(&server)
            .await;
        let image = StashDb::new(client(&server.uri()))
            .image(size, SCENE_D_IMAGE)
            .await
            .unwrap();
        assert_eq!(image.content_type.as_deref(), Some("image/jpeg"));
        assert_eq!(image.body, [0xff, 0xd8, 0xff]);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].headers.get("apikey").is_none());
    }
}
