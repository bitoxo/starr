//! Categories, service roles and their settings (spec 0003 slice 2), the
//! catalog endpoints that feed the relevance editor, and the secret class
//! of catalog tokens (ADR 0003 amendment 1).
#![allow(clippy::unwrap_used)]

mod common;
// Libraries per category (spec 0005 slice 2), in this binary to keep CI's
// link count down.
#[path = "categories/libraries.rs"]
mod libraries;
// Lidarr as the music source (spec 0006 slice 3).
#[path = "categories/lidarr.rs"]
mod lidarr;
// Whisparr as the adult source (spec 0007 slice 3).
#[path = "categories/whisparr.rs"]
mod whisparr;

use axum::http::{Method, StatusCode};
use common::services::{API_KEY, Env, delete, env, fake_radarr};
use common::{Req, TestResponse, elevate_in_place, get, post};
use serde_json::{Value, json};
use starr_core::adapters::SecretClass;
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TMDB_TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.categories-test.signature";

fn put(path: &str, body: Value) -> Req {
    Req::new(Method::PUT, path, Some(body))
}

fn patch(path: &str, body: Value) -> Req {
    Req::new(Method::PATCH, path, Some(body))
}

fn fixture(dir: &str, name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/{dir}/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// A TMDB that serves the recorded fixtures for the bearer token only.
async fn fake_tmdb() -> MockServer {
    let server = MockServer::start().await;
    for (route, file) in [
        ("/3/authentication", "authentication.json"),
        ("/3/configuration", "configuration.json"),
        ("/3/configuration/languages", "configuration_languages.json"),
        ("/3/configuration/countries", "configuration_countries.json"),
        ("/3/watch/providers/regions", "watch_providers_regions.json"),
        ("/3/genre/movie/list", "genre_movie_list.json"),
        ("/3/genre/tv/list", "genre_tv_list.json"),
        ("/3/watch/providers/movie", "watch_providers_movie_CH.json"),
        ("/3/trending/movie/week", "trending_movie_week_page1.json"),
    ] {
        Mock::given(path(route))
            .and(header(
                "authorization",
                format!("Bearer {TMDB_TOKEN}").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(fixture("tmdb/fixtures/v3", file)),
            )
            .mount(&server)
            .await;
    }
    server
}

/// Radarr with quality profiles and root folders.
async fn fake_radarr_with_lists() -> MockServer {
    let server = fake_radarr().await;
    for (route, file) in [
        ("/api/v3/qualityprofile", "qualityprofile.json"),
        ("/api/v3/rootfolder", "rootfolder.json"),
    ] {
        Mock::given(path(route))
            .and(header("X-Api-Key", API_KEY))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture("radarr/fixtures/6.4.4.10685", file)),
            )
            .mount(&server)
            .await;
    }
    server
}

async fn service(env: &Env, adapter_type: &str, name: &str, url: &str, key: &str) -> i64 {
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": adapter_type, "name": name, "baseUrl": url, "apiKey": key }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    res.body["service"]["id"].as_i64().unwrap()
}

async fn add_role(env: &Env, category: &str, kind: &str, id: i64) -> TestResponse {
    env.send(post(
        &format!("/api/v1/categories/{category}/roles"),
        json!({ "kind": kind, "serviceId": id }),
    ))
    .await
}

fn category<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == name)
        .unwrap()
}

#[tokio::test]
async fn catalog_tokens_are_sealed_with_the_web_key_service_keys_with_the_worker_key() {
    let env = env().await;
    let tmdb = fake_tmdb().await;
    let radarr = fake_radarr().await;
    let tmdb_id = service(&env, "tmdb", "TMDB", &tmdb.uri(), TMDB_TOKEN).await;
    let radarr_id = service(&env, "radarr", "Radarr", &radarr.uri(), API_KEY).await;
    let db = &env.app.state.db;
    let tmdb_conn = db.service_connection(tmdb_id).await.unwrap().unwrap();
    assert_eq!(tmdb_conn.credential.class, SecretClass::CatalogReadToken);
    assert_eq!(
        tmdb_conn.credential.recipient,
        env.app.state.catalog.recipient()
    );
    let radarr_conn = db.service_connection(radarr_id).await.unwrap().unwrap();
    assert_eq!(radarr_conn.credential.class, SecretClass::ServiceCredential);
    assert_ne!(
        radarr_conn.credential.recipient,
        env.app.state.catalog.recipient()
    );

    // The web side opens the TMDB token itself; the worker checks both.
    let reference = env
        .send(get(&format!(
            "/api/v1/services/{tmdb_id}/catalog/reference"
        )))
        .await;
    assert_eq!(reference.status, StatusCode::OK, "{}", reference.raw);
    env.drain().await;
    for id in [tmdb_id, radarr_id] {
        let s = env.send(get(&format!("/api/v1/services/{id}"))).await;
        assert_eq!(s.body["lastError"], Value::Null, "{}", s.raw);
        assert!(s.body["detectedVersion"].is_string(), "{}", s.raw);
    }
    // A Radarr is never opened as a catalog by the web side.
    let res = env
        .send(get(&format!(
            "/api/v1/services/{radarr_id}/catalog/reference"
        )))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.code(), "catalog.not_a_catalog");

    // A new TMDB token is sealed with the web key again.
    elevate_in_place(&env.app, &env.admin).await;
    let res = env
        .send(patch(
            &format!("/api/v1/services/{tmdb_id}"),
            json!({ "apiKey": "eyJhbGciOiJIUzI1NiJ9.rotated" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let rotated = db.service_connection(tmdb_id).await.unwrap().unwrap();
    assert_eq!(rotated.credential.class, SecretClass::CatalogReadToken);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one walk through every role rule, in order
async fn roles_follow_capability_category_and_the_one_source_rule() {
    let env = env().await;
    let radarr = fake_radarr().await;
    let tmdb = service(
        &env,
        "tmdb",
        "TMDB",
        "https://api.themoviedb.org",
        TMDB_TOKEN,
    )
    .await;
    let tmdb2 = service(
        &env,
        "tmdb",
        "TMDB 2",
        "https://api.themoviedb.org",
        TMDB_TOKEN,
    )
    .await;
    let radarr_a = service(&env, "radarr", "Radarr", &radarr.uri(), API_KEY).await;
    let radarr_b = service(&env, "radarr", "Radarr 4K", &radarr.uri(), API_KEY).await;
    let sonarr = service(&env, "sonarr", "Sonarr", "http://sonarr:8989", API_KEY).await;

    let res = add_role(&env, "movies", "catalog", tmdb).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["catalogs"][0]["isDefault"], true);
    assert_eq!(res.body["discoverReady"], false);
    let res = add_role(&env, "movies", "catalog", tmdb2).await;
    assert_eq!(res.body["catalogs"][1]["isDefault"], false);

    // Capability: Radarr has no metadata, TMDB takes no requests.
    let res = add_role(&env, "movies", "catalog", radarr_a).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.code(), "categories.capability_missing");
    let res = add_role(&env, "movies", "source", tmdb).await;
    assert_eq!(res.code(), "categories.capability_missing");
    // Category: Sonarr is no source for movies.
    let res = add_role(&env, "movies", "source", sonarr).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.code(), "categories.adapter_unsuited");
    // TMDB serves no music (MusicBrainz does, spec 0006).
    let res = add_role(&env, "music", "catalog", tmdb).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.code(), "categories.adapter_unsuited");
    let res = add_role(&env, "podcasts", "catalog", tmdb).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.code(), "categories.not_found");
    let res = add_role(&env, "movies", "catalog", 999).await;
    assert_eq!(res.code(), "services.not_found");

    // One source per category; a second is refused, switching replaces it.
    let res = add_role(&env, "movies", "source", radarr_a).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let res = add_role(&env, "movies", "source", radarr_b).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "categories.source_taken");
    let preview = env
        .send(get(&format!(
            "/api/v1/categories/movies/source-switch?serviceId={radarr_b}"
        )))
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.raw);
    assert_eq!(preview.body["current"]["serviceId"], radarr_a);
    assert_eq!(preview.body["nextServiceName"], "Radarr 4K");
    assert_eq!(preview.body["affectedRequests"], json!([]));
    let switched = env
        .send(put(
            "/api/v1/categories/movies/source",
            json!({ "serviceId": radarr_b }),
        ))
        .await;
    assert_eq!(switched.status, StatusCode::OK, "{}", switched.raw);
    assert_eq!(switched.body["source"]["serviceId"], radarr_b);
    let res = env
        .send(put(
            "/api/v1/categories/movies/source",
            json!({ "serviceId": sonarr }),
        ))
        .await;
    assert_eq!(res.code(), "categories.adapter_unsuited");

    // Default catalog: switch it, remove it, the oldest remaining takes over.
    let roles = &switched.body["catalogs"];
    let (first, second) = (
        roles[0]["id"].as_i64().unwrap(),
        roles[1]["id"].as_i64().unwrap(),
    );
    let res = env
        .send(post(
            &format!("/api/v1/categories/movies/roles/{second}/default"),
            json!({}),
        ))
        .await;
    assert_eq!(res.body["catalogs"][1]["isDefault"], true, "{}", res.raw);
    assert_eq!(res.body["catalogs"][0]["isDefault"], false);
    let res = env
        .send(delete(&format!("/api/v1/categories/movies/roles/{second}")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["catalogs"].as_array().unwrap().len(), 1);
    assert_eq!(res.body["catalogs"][0]["id"], first);
    assert_eq!(res.body["catalogs"][0]["isDefault"], true);
    // A role of another category is not found under this one.
    let res = env
        .send(delete(&format!("/api/v1/categories/series/roles/{first}")))
        .await;
    assert_eq!(res.code(), "categories.role_not_found");

    // Every change is audited.
    let actions: Vec<String> =
        sqlx::query_as::<_, (String,)>("SELECT action FROM audit_log ORDER BY id")
            .fetch_all(env.app.state.db.pool())
            .await
            .unwrap()
            .into_iter()
            .map(|(a,)| a)
            .collect();
    for action in [
        "categories.role_add",
        "categories.source_switch",
        "categories.default_catalog",
        "categories.role_remove",
    ] {
        assert!(
            actions.contains(&action.to_owned()),
            "{action}: {actions:?}"
        );
    }
}

#[tokio::test]
async fn jellyfin_and_emby_can_be_watched_in_but_sources_cannot() {
    let env = env().await;
    let jellyfin = service(
        &env,
        "jellyfin",
        "Jellyfin",
        "http://jellyfin:8096",
        API_KEY,
    )
    .await;
    let emby = service(&env, "emby", "Emby", "http://emby:8096", API_KEY).await;
    let radarr = service(&env, "radarr", "Radarr", "http://radarr:7878", API_KEY).await;
    let res = add_role(&env, "movies", "consume", jellyfin).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let res = add_role(&env, "series", "consume", emby).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["consumers"][0]["serviceId"], emby);
    // Consume needs libraries; Radarr has none, and Jellyfin is no catalog.
    let res = add_role(&env, "movies", "consume", radarr).await;
    assert_eq!(res.code(), "categories.capability_missing");
    let res = add_role(&env, "movies", "catalog", jellyfin).await;
    assert_eq!(res.code(), "categories.capability_missing");
}

/// Phase 2 provisions accounts and libraries per category, so music and
/// adult can be switched on and watched in before they have a catalog or
/// a source; adult has no request flow (ADR 0012) but its catalog since
/// spec 0007 slice 2, music has one since spec 0006 slice 4 but is not
/// ready without catalog and source.
#[tokio::test]
async fn music_and_adult_get_consume_roles_before_catalog_and_source() {
    let env = env().await;
    let tmdb = service(&env, "tmdb", "TMDB", "http://127.0.0.1:9", TMDB_TOKEN).await;
    let radarr = service(&env, "radarr", "Radarr", "http://127.0.0.1:9", API_KEY).await;
    let jellyfin = service(&env, "jellyfin", "Jellyfin", "http://jf:8096", API_KEY).await;
    let emby = service(&env, "emby", "Emby", "http://emby:8096", API_KEY).await;

    let all = env.send(get("/api/v1/categories")).await;
    for name in ["music", "adult"] {
        // Off until the admin switches it on.
        assert_eq!(category(&all.body, name)["enabled"], false, "{name}");
        let res = env
            .send(patch(
                &format!("/api/v1/categories/{name}"),
                json!({ "enabled": true, "operatorStatement": true }),
            ))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        assert_eq!(res.body["enabled"], true);
        // Both have their request flow: music (spec 0006 slice 4), adult
        // scenes (spec 0007 slice 4); requests wait for catalog and source.
        assert_eq!(res.body["requestFlow"], true, "{name}");
        assert_eq!(res.body["catalogFlow"], true, "{name}");
        assert_eq!(res.body["sourceFlow"], true, "{name}");
        assert_eq!(res.body["discoverReady"], false);
        assert_eq!(res.body["requestsReady"], false);
    }
    let res = add_role(&env, "music", "consume", jellyfin).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["consumers"][0]["serviceId"], jellyfin);
    let res = add_role(&env, "adult", "consume", emby).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["consumers"][0]["serviceName"], "Emby");

    // Adult takes StashDB as catalog and Whisparr as source only: TMDB and
    // Radarr do not serve it (spec 0007 slices 2 and 3).
    let res = add_role(&env, "adult", "catalog", tmdb).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "categories.adapter_unsuited");
    let res = add_role(&env, "adult", "source", radarr).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "categories.adapter_unsuited");
    // Music takes a source (Lidarr, spec 0006 slice 3), but not Radarr.
    let res = env
        .send(put(
            "/api/v1/categories/music/source",
            json!({ "serviceId": radarr }),
        ))
        .await;
    assert_eq!(res.code(), "categories.adapter_unsuited");
    let res = env
        .send(get(&format!(
            "/api/v1/categories/adult/source-switch?serviceId={radarr}"
        )))
        .await;
    assert_eq!(res.code(), "categories.adapter_unsuited");

    // Switching adult on grants it to nobody, and members never discover
    // adult, even when entitled to it; music shows up, not ready until it
    // has a catalog with a relevance.
    let member = member_cookie(&env).await;
    let me = get("/api/v1/me").cookie(&member).send(&env.app).await;
    // Not entitled: the profile does not even name the private category
    // (spec 0007 privacy gate).
    let adult = me.body["entitlements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["category"] == "adult");
    assert_eq!(adult, None, "{}", me.raw);
    let members = env.send(get("/api/v1/members")).await;
    let id = members.body[0]["id"].as_i64().unwrap();
    let all_on: Vec<Value> = ["movies", "series", "music", "adult"]
        .iter()
        .map(|c| json!({ "category": c, "enabled": true, "requestMode": "auto" }))
        .collect();
    let res = env
        .send(put(
            &format!("/api/v1/members/{id}/entitlements"),
            json!({ "entitlements": all_on }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let discover = get("/api/v1/discover").cookie(&member).send(&env.app).await;
    assert_eq!(discover.status, StatusCode::OK, "{}", discover.raw);
    let names: Vec<&str> = discover
        .body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["category"].as_str())
        .collect();
    assert!(!names.contains(&"adult"), "{names:?}");
    let music = discover
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == "music")
        .unwrap();
    assert_eq!(music["discoverReady"], false, "{}", discover.raw);
    assert_eq!(music["kind"], "album");
    assert_eq!(music["requestable"], false);
    let res = get("/api/v1/discover/adult/trending")
        .cookie(&member)
        .send(&env.app)
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
}

#[tokio::test]
async fn relevance_is_validated_against_tmdb_and_gates_discover() {
    let env = env().await;
    let tmdb = fake_tmdb().await;
    let tmdb_id = service(&env, "tmdb", "TMDB", &tmdb.uri(), TMDB_TOKEN).await;
    let res = add_role(&env, "movies", "catalog", tmdb_id).await;
    let role = res.body["catalogs"][0]["id"].as_i64().unwrap();
    let url = format!("/api/v1/categories/movies/roles/{role}/relevance");

    for (body, code) in [
        // `fr` is ISO 639-1, but not in this catalog's language list.
        (
            json!({ "originalLanguages": ["fr"] }),
            "validation.relevance_language",
        ),
        (
            json!({ "releaseRegion": "XX" }),
            "validation.relevance_release_region",
        ),
        // US is a country, but has no watch-provider data here.
        (
            json!({ "providerRegion": "US" }),
            "validation.relevance_provider_region",
        ),
        (
            json!({ "providerRegion": "CH", "providers": [12345] }),
            "validation.relevance_provider",
        ),
        // Talk is a series genre, not a movie genre.
        (
            json!({ "excludedGenres": [10767] }),
            "validation.relevance_genre",
        ),
    ] {
        let res = env.send(put(&url, body.clone())).await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(res.code(), code, "{body}");
    }

    let res = env
        .send(put(
            &url,
            json!({
                "originalLanguages": ["ko", "EN", "en"],
                "providerRegion": "ch",
                "providers": [8],
                "releaseRegion": "CH",
                "excludedGenres": [28],
            }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let relevance = &res.body["catalogs"][0]["relevance"];
    assert_eq!(relevance["originalLanguages"], json!(["ko", "en"]));
    assert_eq!(relevance["providerRegion"], "CH");
    // Not switched on yet: Discover stays off.
    assert_eq!(res.body["discoverReady"], false);
    let res = env
        .send(patch(
            "/api/v1/categories/movies",
            json!({ "enabled": true }),
        ))
        .await;
    assert_eq!(res.body["enabled"], true);
    assert_eq!(res.body["discoverReady"], true);

    // Series start with Talk, News, Soap and Reality hidden (D4).
    let all = env.send(get("/api/v1/categories")).await;
    assert_eq!(all.body.as_array().unwrap().len(), 4);
    let series = category(&all.body, "series");
    let mut hidden: Vec<u64> = series["defaultRelevance"]["excludedGenres"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    hidden.sort_unstable();
    assert_eq!(hidden, vec![10763, 10764, 10766, 10767]);
    assert_eq!(category(&all.body, "music")["requestFlow"], true);
    assert_eq!(category(&all.body, "adult")["requestFlow"], true);
    assert_eq!(category(&all.body, "movies")["requestFlow"], true);
}

#[tokio::test]
async fn reference_providers_and_trending_preview_come_from_the_catalog() {
    let env = env().await;
    let tmdb = fake_tmdb().await;
    let id = service(&env, "tmdb", "TMDB", &tmdb.uri(), TMDB_TOKEN).await;

    let reference = env
        .send(get(&format!("/api/v1/services/{id}/catalog/reference")))
        .await;
    assert_eq!(reference.status, StatusCode::OK, "{}", reference.raw);
    let languages = reference.body["languages"].as_array().unwrap();
    assert!(
        languages
            .iter()
            .any(|l| l["code"] == "ko" && l["name"] == "Korean"),
        "{languages:?}"
    );
    assert_eq!(
        reference.body["providerRegions"].as_array().unwrap().len(),
        2
    );

    let providers = env
        .send(get(&format!(
            "/api/v1/services/{id}/catalog/providers?kind=movie&region=ch"
        )))
        .await;
    assert_eq!(providers.status, StatusCode::OK, "{}", providers.raw);
    // In the region's display order (Disney Plus first in CH).
    assert_eq!(providers.body[0]["name"], "Disney Plus");
    let logo = providers.body[0]["logo"].as_str().unwrap();
    assert!(!logo.starts_with('/'), "{logo}");
    let res = env
        .send(get(&format!(
            "/api/v1/services/{id}/catalog/providers?kind=movie&region=../x"
        )))
        .await;
    assert_eq!(res.code(), "validation.relevance_provider_region");

    // Trending is filtered by original language only (D4).
    let preview = env
        .send(post(
            &format!("/api/v1/services/{id}/catalog/preview"),
            json!({ "kind": "movie", "relevance": { "originalLanguages": ["ko"] } }),
        ))
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.raw);
    let titles: Vec<&str> = preview
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["Parasite"]);
    assert!(!preview.body[0]["poster"].as_str().unwrap().starts_with('/'));
    let everything = env
        .send(post(
            &format!("/api/v1/services/{id}/catalog/preview"),
            json!({ "kind": "movie", "relevance": {} }),
        ))
        .await;
    assert_eq!(everything.body.as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn an_unreachable_catalog_is_reported_as_unavailable() {
    let env = env().await;
    let id = service(&env, "tmdb", "TMDB", "http://127.0.0.1:9", TMDB_TOKEN).await;
    let res = env
        .send(get(&format!("/api/v1/services/{id}/catalog/reference")))
        .await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.code(), "catalog.unavailable");
}

#[tokio::test]
async fn source_settings_must_come_from_the_sources_live_lists() {
    let env = env().await;
    let radarr = fake_radarr_with_lists().await;
    let radarr_id = service(&env, "radarr", "Radarr", &radarr.uri(), API_KEY).await;
    let res = env
        .send(put(
            "/api/v1/categories/movies/source/settings",
            json!({ "qualityProfileId": 4 }),
        ))
        .await;
    assert_eq!(res.code(), "categories.no_source");
    add_role(&env, "movies", "source", radarr_id).await;
    // Requests stay closed until profile and folder are set.
    let categories = env.send(get("/api/v1/categories")).await;
    assert_eq!(categories.body[0]["category"], "movies");
    assert_eq!(categories.body[0]["requestsReady"], false);

    // Before the lists were read, nothing validates.
    let res = env
        .send(put(
            "/api/v1/categories/movies/source/settings",
            json!({ "qualityProfileId": 4 }),
        ))
        .await;
    assert_eq!(res.code(), "validation.quality_profile");
    let lists = env
        .send(get(&format!("/api/v1/services/{radarr_id}/source-lists")))
        .await;
    assert_eq!(lists.body["fetchedAt"], Value::Null);

    let queued = env
        .send(post(
            &format!("/api/v1/services/{radarr_id}/source-lists/refresh"),
            json!({}),
        ))
        .await;
    assert_eq!(queued.status, StatusCode::ACCEPTED, "{}", queued.raw);
    env.drain().await;
    let lists = env
        .send(get(&format!("/api/v1/services/{radarr_id}/source-lists")))
        .await;
    assert_eq!(lists.status, StatusCode::OK, "{}", lists.raw);
    assert_eq!(lists.body["qualityProfiles"][1]["name"], "HD-1080p");
    assert_eq!(lists.body["rootFolders"][0]["path"], "/movies/");
    assert_eq!(lists.body["rootFolders"][1]["accessible"], false);
    assert!(lists.body["fetchedAt"].is_string());

    let res = env
        .send(put(
            "/api/v1/categories/movies/source/settings",
            json!({ "qualityProfileId": 4, "rootFolderPath": "/elsewhere/" }),
        ))
        .await;
    assert_eq!(res.code(), "validation.root_folder");
    let res = env
        .send(put(
            "/api/v1/categories/movies/source/settings",
            json!({ "qualityProfileId": 4, "rootFolderPath": "/movies/" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let settings = &res.body["source"]["settings"];
    assert_eq!(settings["qualityProfileId"], 4);
    assert_eq!(settings["rootFolderPath"], "/movies/");
    assert_eq!(res.body["requestsReady"], true);
    // Radarr's default unless chosen.
    assert_eq!(settings["minimumAvailability"], "released");
    let res = env
        .send(put(
            "/api/v1/categories/movies/source/settings",
            json!({ "qualityProfileId": 4, "rootFolderPath": "/movies/", "minimumAvailability": "inCinemas" }),
        ))
        .await;
    assert_eq!(
        res.body["source"]["settings"]["minimumAvailability"],
        "inCinemas"
    );

    // Minimum availability is a movie setting.
    let sonarr = service(&env, "sonarr", "Sonarr", "http://sonarr:8989", API_KEY).await;
    add_role(&env, "series", "source", sonarr).await;
    let res = env
        .send(put(
            "/api/v1/categories/series/source/settings",
            json!({ "minimumAvailability": "released" }),
        ))
        .await;
    assert_eq!(res.code(), "validation.minimum_availability");
    // Source lists exist for sources only.
    let tmdb = service(
        &env,
        "tmdb",
        "TMDB",
        "https://api.themoviedb.org",
        TMDB_TOKEN,
    )
    .await;
    let res = env
        .send(get(&format!("/api/v1/services/{tmdb}/source-lists")))
        .await;
    assert_eq!(res.code(), "sources.not_a_source");
}

/// Another Radarr: the same root folders, but only the "Any" profile.
async fn other_radarr_without_hd() -> MockServer {
    let server = fake_radarr().await;
    let profiles: Value = serde_json::from_str(&fixture(
        "radarr/fixtures/6.4.4.10685",
        "qualityprofile.json",
    ))
    .unwrap();
    let only_any = json!([profiles[0]]);
    Mock::given(path("/api/v3/qualityprofile"))
        .and(header("X-Api-Key", API_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(only_any))
        .mount(&server)
        .await;
    Mock::given(path("/api/v3/rootfolder"))
        .and(header("X-Api-Key", API_KEY))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixture("radarr/fixtures/6.4.4.10685", "rootfolder.json")),
        )
        .mount(&server)
        .await;
    server
}

/// Movies with Radarr as the source, lists read, HD-1080p into /movies/,
/// switched on: requests are open. Returns the service and the role id.
async fn movies_taking_requests(env: &Env, radarr: &MockServer) -> (i64, i64) {
    let id = service(env, "radarr", "Radarr", &radarr.uri(), API_KEY).await;
    let role = add_role(env, "movies", "source", id).await.body["source"]["id"]
        .as_i64()
        .unwrap();
    env.send(post(
        &format!("/api/v1/services/{id}/source-lists/refresh"),
        json!({}),
    ))
    .await;
    env.drain().await;
    let res = env
        .send(put(
            "/api/v1/categories/movies/source/settings",
            json!({ "qualityProfileId": 4, "rootFolderPath": "/movies/" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = env
        .send(patch(
            "/api/v1/categories/movies",
            json!({ "enabled": true }),
        ))
        .await;
    assert_eq!(res.body["requestsReady"], true, "{}", res.raw);
    (id, role)
}

async fn movies(env: &Env) -> Value {
    category(&env.send(get("/api/v1/categories")).await.body, "movies").clone()
}

#[tokio::test]
async fn a_new_address_keeps_the_source_settings_unless_that_server_lacks_them() {
    // Spec 0008 ADM-01.
    let env = env().await;
    let radarr = fake_radarr_with_lists().await;
    let (id, _) = movies_taking_requests(&env, &radarr).await;
    let url = format!("/api/v1/services/{id}");

    // Renaming has no consequences: no confirmation.
    let res = env.send(patch(&url, json!({ "name": "Films" }))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);

    // A new address of a source in use names what happens and changes
    // nothing until confirmed.
    let moved = fake_radarr_with_lists().await;
    elevate_in_place(&env.app, &env.admin).await;
    let res = env
        .send(patch(
            &url,
            json!({ "baseUrl": moved.uri(), "apiKey": API_KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "services.confirm_required");
    assert_eq!(
        res.body["effects"],
        json!([{ "kind": "sourceSettingsRechecked", "category": "movies" }])
    );
    let service = env.send(get(&url)).await;
    assert_eq!(service.body["baseUrl"], radarr.uri());

    // Confirmed, the same Radarr at its new address: the settings stay and
    // requests stay open.
    let res = env
        .send(patch(
            &url,
            json!({ "baseUrl": moved.uri(), "apiKey": API_KEY, "confirm": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["service"]["baseUrl"], moved.uri());
    env.drain().await;
    let after = movies(&env).await;
    assert_eq!(
        after["source"]["settings"]["qualityProfileId"], 4,
        "{after}"
    );
    assert_eq!(after["source"]["settings"]["rootFolderPath"], "/movies/");
    assert_eq!(after["requestsReady"], true);

    // Another Radarr without the profile: the lists read from there clear
    // it (the folder stays), and requests close until an admin picks again.
    let other = other_radarr_without_hd().await;
    let res = env
        .send(patch(
            &url,
            json!({ "baseUrl": other.uri(), "apiKey": API_KEY, "confirm": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.drain().await;
    let after = movies(&env).await;
    assert_eq!(
        after["source"]["settings"]["qualityProfileId"],
        Value::Null,
        "{after}"
    );
    assert_eq!(after["source"]["settings"]["rootFolderPath"], "/movies/");
    assert_eq!(after["requestsReady"], false);
}

#[tokio::test]
async fn removing_a_source_is_confirmed_and_its_settings_come_back_with_it() {
    // Spec 0008 ADM-02.
    let env = env().await;
    let radarr = fake_radarr_with_lists().await;
    let (id, role) = movies_taking_requests(&env, &radarr).await;
    let url = format!("/api/v1/categories/movies/roles/{role}");

    let res = env.send(delete(&url)).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "categories.confirm_required");
    assert_eq!(
        res.body["effects"],
        json!([
            { "kind": "requestsClose", "category": "movies" },
            { "kind": "settingsKept", "category": "movies" },
        ])
    );
    assert_eq!(movies(&env).await["source"]["id"], role);

    let res = env.send(delete(&format!("{url}?confirm=true"))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["source"], Value::Null);
    assert_eq!(res.body["requestsReady"], false);

    // Set as the source again: the profile and folder are back.
    let res = add_role(&env, "movies", "source", id).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["source"]["settings"]["qualityProfileId"], 4);
    assert_eq!(res.body["source"]["settings"]["rootFolderPath"], "/movies/");
    assert_eq!(res.body["requestsReady"], true);
}

#[tokio::test]
async fn categories_need_a_session() {
    let env = env().await;
    let res = env.send(get("/api/v1/categories")).await;
    assert_eq!(res.status, StatusCode::OK);
    let anonymous = get("/api/v1/categories").send(&env.app).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}
async fn member_cookie(env: &Env) -> String {
    let res = env
        .send(post(
            "/api/v1/members",
            json!({ "username": "mia", "email": "mia@example.org" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let link = &res.body["setPasswordLink"];
    let token = link["url"].as_str().unwrap().rsplit('/').next().unwrap();
    let res = post(
        &format!("/api/v1/password-tokens/{token}"),
        json!({ "password": "a long enough password" }),
    )
    .send(&env.app)
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    res.session_cookie().unwrap()
}

#[tokio::test]
async fn members_get_403_on_category_catalog_and_source_routes() {
    let env = env().await;
    let tmdb = service(&env, "tmdb", "TMDB", "http://127.0.0.1:9", TMDB_TOKEN).await;
    let radarr = service(&env, "radarr", "Radarr", "http://127.0.0.1:9", API_KEY).await;
    let member = member_cookie(&env).await;
    for req in [
        get("/api/v1/categories"),
        post(
            "/api/v1/categories/movies/roles",
            json!({ "kind": "catalog", "serviceId": tmdb }),
        ),
        patch("/api/v1/categories/movies", json!({ "enabled": true })),
        get(&format!("/api/v1/services/{tmdb}/catalog/reference")),
        post(
            &format!("/api/v1/services/{tmdb}/catalog/preview"),
            json!({ "kind": "movie", "relevance": {} }),
        ),
        post(
            &format!("/api/v1/services/{radarr}/source-lists/refresh"),
            json!({}),
        ),
    ] {
        let res = req.cookie(&member).send(&env.app).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
    }
}
