//! Whisparr as the adult source (spec 0007 slice 3): the admin connects
//! Whisparr v3, makes it the adult source and picks quality profile and
//! root folder from the lists the worker read. Adult requests stay closed
//! until slice 4, writes can be paused like every other source, and
//! setting all of this up sends Whisparr nothing but reads. Fixtures are
//! derived from Whisparr's source (placeholders only); no Whisparr is
//! contacted.
use super::common::services::{API_KEY, Env, env};
use super::common::{Req, get, post};
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn put(path: &str, body: Value) -> Req {
    Req::new(Method::PUT, path, Some(body))
}

fn fixture(version: &str, name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/whisparr/fixtures/{version}/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// A Whisparr that answers its status (of `version`) and lists for the
/// right key only.
async fn fake_whisparr(version: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/system/status"))
        .and(header("X-Api-Key", API_KEY))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture(version, "system_status.json")),
        )
        .mount(&server)
        .await;
    for (route, file) in [
        ("/api/v3/qualityprofile", "qualityprofile.json"),
        ("/api/v3/rootfolder", "rootfolder.json"),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .and(header("X-Api-Key", API_KEY))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture("3.1.0.2116", file)))
            .mount(&server)
            .await;
    }
    server
}

async fn whisparr(env: &Env, url: &str) -> i64 {
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "whisparr", "name": "Whisparr", "baseUrl": url, "apiKey": API_KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    res.body["service"]["id"].as_i64().unwrap()
}

async fn writes_to(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method != wiremock::http::Method::GET)
        .map(|r| format!("{} {}", r.method, r.url.path()))
        .collect()
}

fn adult(categories: &Value) -> &Value {
    categories
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == "adult")
        .unwrap()
}

#[tokio::test]
async fn whisparr_becomes_the_adult_source_and_opens_scene_requests() {
    let env = env().await;
    let server = fake_whisparr("3.1.0.2116").await;
    let id = whisparr(&env, &server.uri()).await;
    env.drain().await;
    let service = env.send(get(&format!("/api/v1/services/{id}"))).await;
    assert_eq!(
        service.body["detectedVersion"], "3.1.0.2116",
        "{}",
        service.raw
    );
    assert_eq!(service.body["lastError"], Value::Null, "{}", service.raw);
    assert_eq!(service.body["capabilities"], json!(["requests"]));
    // Like every source, Whisparr starts with writes going out; the admin
    // can pause them as for Radarr (spec 0003 O5).
    assert_eq!(service.body["writesPausedAt"], Value::Null);
    let paused = env
        .send(put(
            &format!("/api/v1/services/{id}/writes-paused"),
            json!({ "paused": true }),
        ))
        .await;
    assert_eq!(paused.status, StatusCode::OK, "{}", paused.raw);
    assert!(paused.body["writesPausedAt"].is_string(), "{}", paused.raw);

    let categories = env.send(get("/api/v1/categories")).await;
    let before = adult(&categories.body);
    assert_eq!(before["sourceFlow"], true);
    assert_eq!(before["requestFlow"], true);
    assert_eq!(before["requestsReady"], false);

    let res = env
        .send(put(
            "/api/v1/categories/adult/source",
            json!({ "serviceId": id }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["source"]["adapterType"], "whisparr");
    // Whisparr is no source for movies, series or music.
    for other in ["movies", "series", "music"] {
        let res = env
            .send(put(
                &format!("/api/v1/categories/{other}/source"),
                json!({ "serviceId": id }),
            ))
            .await;
        assert_eq!(res.code(), "categories.adapter_unsuited", "{other}");
    }

    let queued = env
        .send(post(
            &format!("/api/v1/services/{id}/source-lists/refresh"),
            json!({}),
        ))
        .await;
    assert_eq!(queued.status, StatusCode::ACCEPTED, "{}", queued.raw);
    env.drain().await;
    let lists = env
        .send(get(&format!("/api/v1/services/{id}/source-lists")))
        .await;
    assert_eq!(lists.status, StatusCode::OK, "{}", lists.raw);
    assert_eq!(lists.body["qualityProfiles"][1]["name"], "HD-1080p");
    assert_eq!(lists.body["rootFolders"][0]["path"], "/data/private");
    assert_eq!(lists.body["metadataProfiles"], json!([]));

    let settings = |body: Value| env.send(put("/api/v1/categories/adult/source/settings", body));
    // Radarr's minimum availability and Lidarr's metadata profile are not
    // Whisparr's.
    let res = settings(json!({ "minimumAvailability": "released" })).await;
    assert_eq!(res.code(), "validation.minimum_availability");
    let res = settings(json!({ "metadataProfileId": 1 })).await;
    assert_eq!(res.code(), "validation.metadata_profile_not_offered");
    let res = settings(json!({ "qualityProfileId": 9 })).await;
    assert_eq!(res.code(), "validation.quality_profile");
    let res = settings(json!({ "qualityProfileId": 3, "rootFolderPath": "/data/private" })).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let saved = &res.body["source"]["settings"];
    assert_eq!(saved["qualityProfileId"], 3);
    assert_eq!(saved["rootFolderPath"], "/data/private");
    assert_eq!(saved["minimumAvailability"], Value::Null);
    assert_eq!(saved["metadataProfileId"], Value::Null);
    // Complete: scenes can be requested (spec 0007 slice 4) once the
    // category is on with its operator statement and members unlock it.
    assert_eq!(res.body["requestFlow"], true);
    assert_eq!(res.body["requestsReady"], true);

    // Setting all of this up only read from Whisparr.
    assert_eq!(writes_to(&server).await, Vec::<String>::new());
}

/// Whisparr v2 (Sonarr-based) or a future 4.x is outside the tested range:
/// the probe says so and the instance stays read-only (spec 0007 Q2).
#[tokio::test]
async fn whisparr_v2_is_reported_as_unsupported() {
    let env = env().await;
    let server = fake_whisparr("2.2.0.231").await;
    let id = whisparr(&env, &server.uri()).await;
    env.drain().await;
    let service = env.send(get(&format!("/api/v1/services/{id}"))).await;
    assert_eq!(
        service.body["detectedVersion"], "2.2.0.231",
        "{}",
        service.raw
    );
    assert_eq!(
        service.body["lastError"], "version_unsupported",
        "{}",
        service.raw
    );
    assert_eq!(writes_to(&server).await, Vec::<String>::new());
}
