//! Lidarr as the music source (spec 0006 slice 3): the admin connects
//! Lidarr, makes it the music source and picks quality profile, metadata
//! profile and root folder from the lists the worker read; a profile that
//! drops albums or EPs is flagged. Music requests open once the settings
//! are complete (slice 4), and setting all of this up sends Lidarr nothing
//! but reads.
use super::common::services::{API_KEY, Env, env};
use super::common::{Req, get, post};
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn put(path: &str, body: Value) -> Req {
    Req::new(Method::PUT, path, Some(body))
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/lidarr/fixtures/3.1.6.5079/recorded/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// A Lidarr that answers its status and lists (recorded from 3.1.6.5079)
/// for the right key only.
async fn fake_lidarr() -> MockServer {
    let server = MockServer::start().await;
    for (route, file) in [
        ("/api/v1/system/status", "system_status.json"),
        ("/api/v1/qualityprofile", "qualityprofile.json"),
        ("/api/v1/metadataprofile", "metadataprofile.json"),
        ("/api/v1/rootfolder", "rootfolder.json"),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .and(header("X-Api-Key", API_KEY))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture(file)))
            .mount(&server)
            .await;
    }
    server
}

async fn lidarr(env: &Env, url: &str) -> i64 {
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "lidarr", "name": "Lidarr", "baseUrl": url, "apiKey": API_KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    res.body["service"]["id"].as_i64().unwrap()
}

async fn settings(env: &Env, body: Value) -> super::common::TestResponse {
    env.send(put("/api/v1/categories/music/source/settings", body))
        .await
}

#[tokio::test]
async fn lidarr_becomes_the_music_source_with_a_metadata_profile() {
    let env = env().await;
    let server = fake_lidarr().await;
    let id = lidarr(&env, &server.uri()).await;
    env.drain().await;
    let service = env.send(get(&format!("/api/v1/services/{id}"))).await;
    assert_eq!(
        service.body["detectedVersion"], "3.1.6.5079",
        "{}",
        service.raw
    );
    assert_eq!(service.body["capabilities"], json!(["requests"]));
    // Like every source, Lidarr starts with writes going out; the admin
    // can pause them as for Radarr and Sonarr.
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
    let music = categories
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == "music")
        .unwrap();
    assert_eq!(music["sourceFlow"], true);
    assert_eq!(music["requestFlow"], true);
    assert_eq!(music["requestsReady"], false);

    let res = env
        .send(put(
            "/api/v1/categories/music/source",
            json!({ "serviceId": id }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["source"]["adapterType"], "lidarr");
    // Lidarr is no source for series.
    let res = env
        .send(put(
            "/api/v1/categories/series/source",
            json!({ "serviceId": id }),
        ))
        .await;
    assert_eq!(res.code(), "categories.adapter_unsuited");

    // Before the lists were read nothing validates.
    let res = settings(&env, json!({ "metadataProfileId": 1 })).await;
    assert_eq!(res.code(), "validation.metadata_profile");
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
    assert_eq!(lists.body["qualityProfiles"][1]["name"], "Lossless");
    assert_eq!(lists.body["rootFolders"][0]["path"], "/music");
    let profiles = &lists.body["metadataProfiles"];
    assert_eq!(profiles[0]["name"], "Standard");
    // Lidarr's shipped "Standard" drops EPs: flagged like "None" (Q6).
    assert_eq!(profiles[0]["admitsAlbumsAndEps"], false);
    assert_eq!(profiles[0]["primaryTypes"], json!(["Album"]));
    assert_eq!(profiles[1]["name"], "None");
    assert_eq!(profiles[1]["admitsAlbumsAndEps"], false);

    let res = settings(&env, json!({ "metadataProfileId": 9 })).await;
    assert_eq!(res.code(), "validation.metadata_profile");
    let res = settings(&env, json!({ "minimumAvailability": "released" })).await;
    assert_eq!(res.code(), "validation.minimum_availability");
    let res = settings(
        &env,
        json!({ "qualityProfileId": 2, "metadataProfileId": 1, "rootFolderPath": "/music" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let saved = &res.body["source"]["settings"];
    assert_eq!(saved["qualityProfileId"], 2);
    assert_eq!(saved["metadataProfileId"], 1);
    assert_eq!(saved["rootFolderPath"], "/music");
    assert_eq!(saved["minimumAvailability"], Value::Null);
    // Complete: members can request albums (spec 0006 slice 4).
    assert_eq!(res.body["requestsReady"], true);

    // Setting all of this up only read from Lidarr.
    let writes: Vec<String> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method != wiremock::http::Method::GET)
        .map(|r| format!("{} {}", r.method, r.url.path()))
        .collect();
    assert_eq!(writes, Vec::<String>::new());
}

#[tokio::test]
async fn only_lidarr_takes_a_metadata_profile() {
    let env = env().await;
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "radarr", "name": "Radarr", "baseUrl": "http://127.0.0.1:9", "apiKey": API_KEY }),
        ))
        .await;
    let radarr = res.body["service"]["id"].as_i64().unwrap();
    env.send(put(
        "/api/v1/categories/movies/source",
        json!({ "serviceId": radarr }),
    ))
    .await;
    let res = env
        .send(put(
            "/api/v1/categories/movies/source/settings",
            json!({ "metadataProfileId": 1 }),
        ))
        .await;
    assert_eq!(res.code(), "validation.metadata_profile_not_offered");
    // Movies and series report no metadata profile.
    let res = env
        .send(put("/api/v1/categories/movies/source/settings", json!({})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(
        res.body["source"]["settings"]["metadataProfileId"],
        Value::Null
    );
}
