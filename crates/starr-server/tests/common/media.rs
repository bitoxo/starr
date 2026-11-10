//! Movies and series test environment: TMDB, Radarr and Sonarr fakes from
//! recorded fixtures, both categories set up, and two members (Mia asks
//! with approval, Noah automatically). Shared by the request and Discover
//! tests.
#![allow(clippy::unwrap_used, dead_code)]

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::services::{API_KEY, Env, env, patch};
use super::{Req, TestResponse, post};

pub const TMDB_TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.requests-test.signature";
/// Inception.
pub const MOVIE: u64 = 27205;
/// Game of Thrones (TVDB 121361; seasons 0, 1, 2, 9).
pub const SERIES: u64 = 1399;

pub fn put(path: &str, body: Value) -> Req {
    Req::new(Method::PUT, path, Some(body))
}

pub fn fixture(dir: &str, name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/{dir}/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

pub async fn fake_tmdb() -> MockServer {
    let server = MockServer::start().await;
    for (route, file) in [
        ("/3/authentication", "authentication.json"),
        ("/3/configuration", "configuration.json"),
        ("/3/movie/27205", "movie_27205.json"),
        ("/3/tv/1399", "tv_1399.json"),
        // Reference data (saving a relevance), lists and search (Discover).
        ("/3/configuration/languages", "configuration_languages.json"),
        ("/3/configuration/countries", "configuration_countries.json"),
        ("/3/watch/providers/regions", "watch_providers_regions.json"),
        ("/3/genre/movie/list", "genre_movie_list.json"),
        ("/3/genre/tv/list", "genre_tv_list.json"),
        ("/3/trending/movie/week", "trending_movie_week_page1.json"),
        ("/3/discover/tv", "discover_tv.json"),
        ("/3/search/multi", "search_multi.json"),
        // Category page data (spec 0008 slice 2).
        ("/3/watch/providers/movie", "watch_providers_movie_CH.json"),
        ("/3/watch/providers/tv", "watch_providers_movie_CH.json"),
        ("/3/tv/1399/season/1", "tv_1399_season_1.json"),
        ("/3/tv/on_the_air", "discover_tv.json"),
        (
            "/3/trending/tv/week",
            "recorded/trending_tv_week_page1.json",
        ),
        ("/3/discover/movie", "discover_movie.json"),
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

/// Radarr or Sonarr with status, quality profiles and root folders.
pub async fn fake_source(adapter: &str, version: &str) -> MockServer {
    let server = MockServer::start().await;
    for (route, file) in [
        ("/api/v3/system/status", "system_status.json"),
        ("/api/v3/qualityprofile", "qualityprofile.json"),
        ("/api/v3/rootfolder", "rootfolder.json"),
    ] {
        Mock::given(path(route))
            .and(header("X-Api-Key", API_KEY))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture(&format!("{adapter}/fixtures/{version}"), file)),
            )
            .mount(&server)
            .await;
    }
    server
}

pub struct Setup {
    pub env: Env,
    pub mia: Member,
    pub noah: Member,
    pub radarr: i64,
    pub _servers: Vec<MockServer>,
}

pub struct Member {
    pub id: i64,
    pub cookie: String,
}

impl Setup {
    pub async fn admin(&self, req: Req) -> TestResponse {
        self.env.send(req).await
    }

    pub async fn as_member(&self, member: &Member, req: Req) -> TestResponse {
        req.cookie(&member.cookie).send(&self.env.app).await
    }

    pub async fn request_movie(&self, member: &Member) -> TestResponse {
        self.as_member(
            member,
            post(
                "/api/v1/requests",
                json!({ "kind": "movie", "titleId": MOVIE.to_string() }),
            ),
        )
        .await
    }

    pub async fn request_seasons(&self, member: &Member, seasons: &[u32]) -> TestResponse {
        self.as_member(
            member,
            post(
                "/api/v1/requests",
                json!({ "kind": "series", "titleId": SERIES.to_string(), "seasons": seasons, "follow": true }),
            ),
        )
        .await
    }

    pub async fn submit_jobs(&self) -> i64 {
        let (n,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM job WHERE kind = 'requests.submit'")
                .fetch_one(self.env.app.state.db.pool())
                .await
                .unwrap();
        n
    }
}

pub async fn service(env: &Env, adapter_type: &str, url: &str, key: &str) -> i64 {
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": adapter_type, "name": adapter_type, "baseUrl": url, "apiKey": key }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    res.body["service"]["id"].as_i64().unwrap()
}

pub async fn member(env: &Env, name: &str, entitlements: Value) -> Member {
    let res = env
        .send(post(
            "/api/v1/members",
            json!({
                "username": name,
                "email": format!("{name}@example.org"),
                "entitlements": entitlements,
            }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let id = res.body["member"]["id"].as_i64().unwrap();
    let link = &res.body["setPasswordLink"];
    let token = link["url"].as_str().unwrap().rsplit('/').next().unwrap();
    let res = post(
        &format!("/api/v1/password-tokens/{token}"),
        json!({ "password": "a long enough password" }),
    )
    .send(&env.app)
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    Member {
        id,
        cookie: res.session_cookie().unwrap(),
    }
}

pub fn entitlements(mode: &str) -> Value {
    json!([
        { "category": "movies", "enabled": true, "requestMode": mode },
        { "category": "series", "enabled": true, "requestMode": mode },
    ])
}

/// TMDB, Radarr and Sonarr connected and assigned, settings chosen, both
/// categories on; Mia requests with approval, Noah automatically.
pub async fn setup() -> Setup {
    let env = env().await;
    let tmdb = fake_tmdb().await;
    let radarr = fake_source("radarr", "6.4.4.10685").await;
    let sonarr = fake_source("sonarr", "4.0.20.3014").await;
    let tmdb_id = service(&env, "tmdb", &tmdb.uri(), TMDB_TOKEN).await;
    let radarr_id = service(&env, "radarr", &radarr.uri(), API_KEY).await;
    let sonarr_id = service(&env, "sonarr", &sonarr.uri(), API_KEY).await;
    for (category, source, folder) in [
        ("movies", radarr_id, "/movies/"),
        ("series", sonarr_id, "/tv/"),
    ] {
        for (kind, id) in [("catalog", tmdb_id), ("source", source)] {
            let res = env
                .send(post(
                    &format!("/api/v1/categories/{category}/roles"),
                    json!({ "kind": kind, "serviceId": id }),
                ))
                .await;
            assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
        }
        env.send(post(
            &format!("/api/v1/services/{source}/source-lists/refresh"),
            json!({}),
        ))
        .await;
        env.drain().await;
        let res = env
            .send(put(
                &format!("/api/v1/categories/{category}/source/settings"),
                json!({ "qualityProfileId": 4, "rootFolderPath": folder }),
            ))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        let res = env
            .send(patch(
                &format!("/api/v1/categories/{category}"),
                json!({ "enabled": true }),
            ))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    }
    let mia = member(&env, "mia", entitlements("approval")).await;
    let noah = member(&env, "noah", entitlements("auto")).await;
    Setup {
        env,
        mia,
        noah,
        radarr: radarr_id,
        _servers: vec![tmdb, radarr, sonarr],
    }
}
