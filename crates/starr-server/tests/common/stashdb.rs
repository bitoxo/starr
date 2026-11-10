//! The adult catalog as tests use it: a `StashDB` fake serving the
//! adapter's fixtures, which are derived by hand from the stash-box schema
//! (never recorded) and hold neutral placeholders only ("Scene A",
//! "Performer P", "Studio S"). Images are a few placeholder bytes. Nothing
//! real is contacted.
#![allow(dead_code)]

use axum::http::StatusCode;
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::services::Env;
use super::{TestResponse, post};

pub const KEY: &str = "placeholder-catalog-key";
pub const SCENE_A: &str = "5ce0e000-0000-4000-8000-00000000000a";
pub const PERFORMER_P: &str = "9e7f0000-0000-4000-8000-0000000000aa";
pub const STUDIO_S: &str = "57d10000-0000-4000-8000-0000000000aa";
/// The landscape image of Scene A.
pub const IMAGE: &str = "1a6e0000-0000-4000-8000-000000000002";
/// What the fake serves as an image (placeholder bytes).
pub const IMAGE_BYTES: &[u8] = b"PNG placeholder";

pub fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/stashdb/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// A `StashDB` fake whose key's account has the roles of `probe`
/// (`probe_read.json` or `probe_edit.json`). Every GraphQL answer needs
/// Starr's agent and the key; images are served only without the key.
pub async fn fake_stashdb(probe: &str) -> MockServer {
    let server = MockServer::start().await;
    // A performer's or studio's scenes before the lanes (both sort by date);
    // `mount_entity_scenes` replaces them (priority 1).
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains(r#""modifier":"INCLUDES""#))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("entity_scenes.json")))
        .with_priority(2)
        .mount(&server)
        .await;
    for (marker, file) in [
        ("query Probe", probe),
        ("query SearchScenes", "search_scenes.json"),
        ("query SearchPerformers", "search_performers.json"),
        ("query QueryStudios", "query_studios.json"),
        ("query FindScene", "find_scene.json"),
        ("query FindPerformer", "find_performer.json"),
        ("query FindStudio", "find_studio.json"),
        (r#""sort":"DATE""#, "lane_new.json"),
        (r#""sort":"TRENDING""#, "lane_trending.json"),
    ] {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(header("user-agent", starr_adapter_kit::USER_AGENT))
            .and(header("apikey", KEY))
            .and(body_string_contains(marker))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture(file)))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path_regex(r"^/images/[0-9a-f-]{36}$"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(IMAGE_BYTES),
        )
        .mount(&server)
        .await;
    server
}

/// A scene of a performer's or studio's list: UUID, placeholder title and
/// release date (`YYYY-MM-DD`, partial, or none).
pub type ListedScene<'a> = (&'a str, &'a str, Option<&'a str>);

/// Every performer's and studio's scene list answers `scenes` from now on
/// (newest first, as `StashDB` sorts them), all by Studio S with Performer
/// P. Placeholders only.
pub async fn mount_entity_scenes(server: &MockServer, scenes: &[ListedScene<'_>]) {
    let mut sorted = scenes.to_vec();
    sorted.sort_by(|a, b| b.2.cmp(&a.2));
    let list: Vec<serde_json::Value> = sorted
        .iter()
        .map(|(id, title, date)| {
            json!({
                "id": id,
                "title": title,
                "release_date": date,
                "duration": 1800,
                "studio": { "id": STUDIO_S, "name": "Studio S" },
                "performers": [
                    { "performer": { "id": PERFORMER_P, "name": "Performer P" }, "as": null }
                ],
                "images": [],
            })
        })
        .collect();
    let body = json!({ "data": { "queryScenes": { "count": list.len(), "scenes": list } } });
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains(r#""modifier":"INCLUDES""#))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .with_priority(1)
        .mount(server)
        .await;
}

/// GraphQL requests the fake received.
pub async fn graphql_requests(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/graphql")
        .count()
}

/// Image requests the fake received.
pub async fn image_requests(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().starts_with("/images/"))
        .count()
}

/// Connect the fake as a `StashDB` service.
pub async fn add_stashdb(env: &Env, server: &MockServer) -> TestResponse {
    env.send(post(
        "/api/v1/services",
        json!({
            "adapterType": "stashdb",
            "name": "Catalog C",
            "baseUrl": server.uri(),
            "apiKey": KEY,
        }),
    ))
    .await
}

/// Connect the fake and make it the adult catalog; returns the service id.
pub async fn connect_stashdb(env: &Env, server: &MockServer) -> i64 {
    let res = add_stashdb(env, server).await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let id = res.body["service"]["id"].as_i64().unwrap();
    let res = env
        .send(post(
            "/api/v1/categories/adult/roles",
            json!({ "kind": "catalog", "serviceId": id }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    // The check job runs the probe (the fake answers it).
    env.drain().await;
    id
}
