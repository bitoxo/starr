//! The private area's catalog (spec 0007 slice 2): the `StashDB` key is
//! checked on save (read-only accounts only, Q14), `StashDB` becomes the
//! adult catalog, and an entitled member who unlocked browses lanes,
//! search and the scene, performer and studio pages. Requests are not
//! open yet. Everything runs against the `StashDB` fake (placeholders
//! only); nothing real is contacted.
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;
use chrono::TimeDelta;
use common::media::{Member, member, put};
use common::services::{Env, env, patch};
use common::stashdb::{
    IMAGE, KEY, PERFORMER_P, SCENE_A, STUDIO_S, add_stashdb, connect_stashdb, fake_stashdb,
    graphql_requests,
};
use common::{Req, TestResponse, elevate_in_place, get, post};
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

struct World {
    env: Env,
    ada: Member,
    stashdb: MockServer,
}

impl World {
    async fn ada(&self, req: Req) -> TestResponse {
        req.cookie(&self.ada.cookie).send(&self.env.app).await
    }
}

/// Adult on (with the statement), Ada entitled and unlocked; `StashDB`
/// connected as the adult catalog when `connected`.
async fn world(connected: bool) -> World {
    let env = env().await;
    let res = env
        .send(patch(
            "/api/v1/categories/adult",
            json!({ "enabled": true, "operatorStatement": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let stashdb = fake_stashdb("probe_read.json").await;
    if connected {
        connect_stashdb(&env, &stashdb).await;
    }
    let ada = member(
        &env,
        "ada",
        json!([{ "category": "adult", "enabled": true, "requestMode": "approval" }]),
    )
    .await;
    let res = put("/api/v1/auth/private", json!({ "unlocked": true }))
        .cookie(&ada.cookie)
        .send(&env.app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    World { env, ada, stashdb }
}

fn adult(categories: &Value) -> Value {
    categories
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == "adult")
        .unwrap()
        .clone()
}

#[tokio::test]
async fn a_stashdb_key_is_stored_only_when_its_account_can_only_read() {
    let env = env().await;
    // An account that could edit StashDB: refused, nothing stored.
    let editor = fake_stashdb("probe_edit.json").await;
    let res = add_stashdb(&env, &editor).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "validation.stashdb_not_read_only");
    // A key StashDB refuses.
    let refusing = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&refusing)
        .await;
    let res = add_stashdb(&env, &refusing).await;
    assert_eq!(res.code(), "validation.api_key_refused", "{}", res.raw);
    // Unreachable: not stored either.
    let res = env
        .send(common::post(
            "/api/v1/services",
            json!({ "adapterType": "stashdb", "name": "Catalog C", "baseUrl": "http://127.0.0.1:9", "apiKey": KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE, "{}", res.raw);
    assert_eq!(res.code(), "services.check_unavailable");
    let list = env.send(get("/api/v1/services")).await;
    assert_eq!(list.body, json!([]), "{}", list.raw);

    // A read-only account: stored, checked again by the job.
    let reader = fake_stashdb("probe_read.json").await;
    let res = add_stashdb(&env, &reader).await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let id = res.body["service"]["id"].as_i64().unwrap();
    env.drain().await;
    let service = env.send(get(&format!("/api/v1/services/{id}"))).await;
    assert_eq!(
        service.body["detectedVersion"], "0.10.3.0",
        "{}",
        service.raw
    );
    // One StashDB per Starr (its budget is per host).
    let res = add_stashdb(&env, &reader).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "services.stashdb_exists");
    // A new key is checked as well.
    elevate_in_place(&env.app, &env.admin).await;
    let res = env
        .send(patch(
            &format!("/api/v1/services/{id}"),
            json!({ "baseUrl": editor.uri(), "apiKey": KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "validation.stashdb_not_read_only");
}

#[tokio::test]
async fn stashdb_becomes_the_adult_catalog_without_a_relevance() {
    let w = world(true).await;
    let categories = w.env.send(get("/api/v1/categories")).await;
    let adult = adult(&categories.body);
    assert_eq!(adult["catalogFlow"], true, "{adult}");
    assert_eq!(adult["discoverReady"], true, "{adult}");
    assert_eq!(adult["requestFlow"], true);
    // No source yet: nothing can be requested.
    assert_eq!(adult["requestsReady"], false);
    assert_eq!(adult["catalogs"][0]["adapterType"], "stashdb");
    let role = adult["catalogs"][0]["id"].as_i64().unwrap();
    // Nothing to restrict: the empty relevance only.
    let path = format!("/api/v1/categories/adult/roles/{role}/relevance");
    let res = w.env.send(put(&path, json!({}))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = w
        .env
        .send(put(&path, json!({ "originalLanguages": ["en"] })))
        .await;
    assert_eq!(res.code(), "validation.relevance_not_offered");

    // The admin catalog tools never ask StashDB (nothing there has an
    // admin-visible preview).
    let service = adult["catalogs"][0]["serviceId"].as_i64().unwrap();
    let before = graphql_requests(&w.stashdb).await;
    for req in [
        get(&format!("/api/v1/services/{service}/catalog/reference")),
        get(&format!(
            "/api/v1/services/{service}/catalog/providers?kind=movie&region=DE"
        )),
        common::post(
            &format!("/api/v1/services/{service}/catalog/preview"),
            json!({ "kind": "movie", "relevance": {} }),
        ),
    ] {
        let res = w.env.send(req).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        assert!(!res.raw.contains("Scene"), "{}", res.raw);
    }
    assert_eq!(graphql_requests(&w.stashdb).await, before);
}

#[tokio::test]
async fn an_entitled_member_who_unlocked_browses_lanes_search_and_pages() {
    let w = world(true).await;
    let area = w.ada(get("/api/v1/private")).await;
    assert_eq!(
        area.body,
        json!({ "ready": true, "requestable": false, "lists": ["new", "trending"] }),
        "{}",
        area.raw
    );

    let lane = w.ada(get("/api/v1/private/discover/new")).await;
    assert_eq!(lane.status, StatusCode::OK, "{}", lane.raw);
    let first = &lane.body["items"][0];
    assert_eq!(first["kind"], "scene");
    assert_eq!(first["id"], SCENE_A);
    assert_eq!(first["title"], "Scene A");
    assert_eq!(first["date"], "2026-10-30");
    // The landscape image, as an id for the adult image source.
    assert_eq!(first["image"], IMAGE);
    assert_eq!(first["studio"]["name"], "Studio S");
    assert_eq!(first["durationSeconds"], 1800);
    // Its performers, for the caption (spec 0008 Q7 A, PRIV-25).
    assert_eq!(first["performers"][0]["id"], PERFORMER_P);
    assert_eq!(first["performers"][0]["name"], "Performer P");
    assert_eq!(first["performers"][0]["kind"], "performer");
    assert_eq!(lane.body["next"], Value::Null);
    let trending = w.ada(get("/api/v1/private/discover/trending?page=1")).await;
    assert_eq!(trending.body["next"], 2, "{}", trending.raw);

    let search = |query: &str| post("/api/v1/private/search", json!({ "query": query }));
    let found = w.ada(search("Scene")).await;
    assert_eq!(found.status, StatusCode::OK, "{}", found.raw);
    assert_eq!(found.body["scenes"][0]["id"], SCENE_A);
    assert_eq!(found.body["performers"][0]["title"], "Performer P");
    assert_eq!(found.body["studios"][0]["title"], "Studio S");
    assert_eq!(found.body["next"], 2);
    // Totals for the group headers (spec 0008 PRIV-14).
    assert_eq!(
        found.body["counts"],
        json!({ "scenes": 26, "performers": 1, "studios": 1 }),
        "{}",
        found.raw
    );
    // A later page asks for scenes only: the lanes come from the first.
    let before = graphql_requests(&w.stashdb).await;
    let second = w
        .ada(post(
            "/api/v1/private/search",
            json!({ "query": "Scene", "page": 2 }),
        ))
        .await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.raw);
    assert_eq!(second.body["performers"], json!([]), "{}", second.raw);
    assert_eq!(second.body["studios"], json!([]), "{}", second.raw);
    assert_eq!(graphql_requests(&w.stashdb).await, before + 1);
    let blank = w.ada(search(" ")).await;
    assert_eq!(blank.body["scenes"], json!([]));
    assert_eq!(
        blank.body["counts"],
        json!({ "scenes": 0, "performers": 0, "studios": 0 })
    );

    let scene = w
        .ada(get(&format!("/api/v1/private/scenes/{SCENE_A}")))
        .await;
    assert_eq!(scene.status, StatusCode::OK, "{}", scene.raw);
    assert_eq!(scene.body["title"], "Scene A");
    assert_eq!(scene.body["requestable"], false);
    assert_eq!(scene.body["studio"]["id"], STUDIO_S);
    assert_eq!(scene.body["performers"][0]["id"], PERFORMER_P);
    assert_eq!(scene.body["performers"][0]["kind"], "performer");

    for (path, name) in [
        (
            format!("/api/v1/private/performers/{PERFORMER_P}"),
            "Performer P",
        ),
        (format!("/api/v1/private/studios/{STUDIO_S}"), "Studio S"),
    ] {
        let page = w.ada(get(&path)).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.raw);
        assert_eq!(page.body["name"], name);
        assert_eq!(page.body["scenes"][0]["id"], SCENE_A);
        assert_eq!(page.body["requestable"], false);
    }

    // Unknown lists, invalid pages and ids.
    let res = w.ada(get("/api/v1/private/discover/popular")).await;
    assert_eq!(res.code(), "discover.unknown_list");
    let res = w.ada(get("/api/v1/private/discover/new?page=21")).await;
    assert_eq!(res.code(), "validation.page");
    let res = w.ada(get("/api/v1/private/scenes/not-a-uuid")).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.code(), "catalog.not_found");

    // Cached: the same lane again asks nothing.
    let before = graphql_requests(&w.stashdb).await;
    w.ada(get("/api/v1/private/discover/new")).await;
    assert_eq!(graphql_requests(&w.stashdb).await, before);
}

/// Spec 0008 slice 2: the scene page carries the description (plain,
/// trimmed) and "More from the studio" (its other scenes, from the studio
/// page's cached first page); the search narrows scenes to a studio or a
/// performer.
#[tokio::test]
async fn the_scene_page_has_its_description_and_more_from_the_studio() {
    let w = world(true).await;
    let scene = w
        .ada(get(&format!("/api/v1/private/scenes/{SCENE_A}")))
        .await;
    assert_eq!(scene.status, StatusCode::OK, "{}", scene.raw);
    assert_eq!(
        scene.body["details"],
        "Details D: a placeholder description of Scene A.\n\nSecond paragraph."
    );
    let more = scene.body["moreFromStudio"].as_array().unwrap();
    assert_ne!(more.len(), 0, "{}", scene.raw);
    assert!(
        more.iter()
            .all(|s| s["id"] != SCENE_A && s["kind"] == "scene")
    );
    assert!(more.iter().all(|s| s["status"].is_string()));
    // The studio page afterwards reuses the cached first page.
    let before = graphql_requests(&w.stashdb).await;
    let studio = w
        .ada(get(&format!("/api/v1/private/studios/{STUDIO_S}")))
        .await;
    assert_eq!(studio.status, StatusCode::OK, "{}", studio.raw);
    // Only the studio itself was asked for; its scenes came from the cache.
    assert_eq!(graphql_requests(&w.stashdb).await, before + 1);

    let filtered = w
        .ada(post(
            "/api/v1/private/search",
            json!({ "query": "Scene", "studio": STUDIO_S }),
        ))
        .await;
    assert_eq!(filtered.status, StatusCode::OK, "{}", filtered.raw);
    let scenes = filtered.body["scenes"].as_array().unwrap();
    assert_ne!(scenes.len(), 0, "{}", filtered.raw);
    assert!(scenes.iter().all(|s| s["studio"]["id"] == STUDIO_S));
    // The lanes stay as without the filter; the scene total is the
    // filtered one.
    assert_eq!(filtered.body["studios"][0]["title"], "Studio S");
    assert_eq!(filtered.body["counts"]["studios"], 1, "{}", filtered.raw);
    assert_ne!(filtered.body["counts"]["scenes"], 26, "{}", filtered.raw);
    for body in [
        json!({ "query": "Scene", "studio": STUDIO_S, "performer": PERFORMER_P }),
        json!({ "query": "Scene", "studio": "not-a-uuid" }),
        json!({ "query": "Scene", "performer": STUDIO_S.to_uppercase() }),
    ] {
        let res = w.ada(post("/api/v1/private/search", body)).await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
        assert_eq!(res.code(), "validation.search_filter");
    }
}

/// The search text never appears in a URL: the API takes it only as a
/// body, and no documented route has it in its path or query.
#[test]
fn the_private_search_takes_its_text_only_in_a_body() {
    let doc: Value = serde_json::from_str(&starr_server::openapi_json()).unwrap();
    let search = &doc["paths"]["/api/v1/private/search"];
    assert!(search.get("get").is_none(), "{search}");
    let operation = &search["post"];
    assert!(operation["requestBody"].is_object(), "{operation}");
    assert!(
        operation
            .get("parameters")
            .is_none_or(|p| p.as_array().is_some_and(Vec::is_empty)),
        "{operation}"
    );
}

#[tokio::test]
async fn a_search_text_in_the_url_is_ignored() {
    let w = world(true).await;
    let before = graphql_requests(&w.stashdb).await;
    let res = w.ada(get("/api/v1/private/search?query=Scene")).await;
    assert_ne!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(graphql_requests(&w.stashdb).await, before);
}

#[tokio::test]
async fn without_a_catalog_the_area_exists_but_is_not_ready() {
    let w = world(false).await;
    let area = w.ada(get("/api/v1/private")).await;
    assert_eq!(area.body["ready"], false, "{}", area.raw);
    assert_eq!(area.body["lists"], json!([]));
    for req in [
        get("/api/v1/private/discover/new"),
        post("/api/v1/private/search", json!({ "query": "Scene" })),
    ] {
        let res = w.ada(req).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
        assert_eq!(res.code(), "discover.unavailable");
    }
}

#[tokio::test]
async fn only_a_key_whose_last_check_passed_is_used() {
    let w = world(true).await;
    // Refused (the account gained a write role), then a check that failed
    // for another reason: the key stays unused until a check passes.
    for error in ["rejected", "transient"] {
        sqlx::query("UPDATE service_instance SET last_error = ?")
            .bind(error)
            .execute(w.env.app.state.db.pool())
            .await
            .unwrap();
        let before = graphql_requests(&w.stashdb).await;
        let area = w.ada(get("/api/v1/private")).await;
        assert_eq!(area.body["ready"], false, "{error}: {}", area.raw);
        let res = w.ada(get("/api/v1/private/discover/new")).await;
        assert_eq!(res.code(), "discover.unavailable");
        assert_eq!(graphql_requests(&w.stashdb).await, before);
    }
}

#[tokio::test]
async fn browsing_keeps_the_unlock_open_and_locking_closes_the_area() {
    let w = world(true).await;
    let clock = &w.env.app.clock;
    clock.advance(TimeDelta::minutes(25));
    let lane = w.ada(get("/api/v1/private/discover/new")).await;
    assert_eq!(lane.status, StatusCode::OK);
    clock.advance(TimeDelta::minutes(25));
    let area = w.ada(get("/api/v1/private")).await;
    assert_eq!(area.status, StatusCode::OK, "browsing kept it open");
    clock.advance(TimeDelta::minutes(31));
    let area = w.ada(get("/api/v1/private")).await;
    assert_eq!(area.status, StatusCode::NOT_FOUND);
    assert_eq!(area.code(), "private.not_found");
    // Admins have no member area: 403 as for every member route.
    let res = w.env.send(get("/api/v1/private")).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
}
