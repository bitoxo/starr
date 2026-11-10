//! The music catalog as tests use it: `MusicBrainz` and `ListenBrainz` fakes
//! serving the adapter's hand-written fixtures (answering Starr's agent
//! only) and an app with `MusicBrainz` as the music catalog. Nothing real is
//! contacted.
#![allow(clippy::type_complexity)]

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{Value, json};
use starr_secrets::CatalogTokenKey;
use starr_server::app;
use starr_worker::catalog::CatalogAccess;
use wiremock::matchers::{header, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::media::{Member, member, put};
use super::services::{Env, env, patch};
use super::{TestResponse, post};

pub fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/musicbrainz/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// `MusicBrainz`: only requests with Starr's agent get answers.
pub async fn fake_musicbrainz() -> MockServer {
    let server = MockServer::start().await;
    let routes: [(&str, Option<(&str, &str)>, &str); 6] = [
        ("/ws/2/artist", None, "ws2/search_artist_radiohead.json"),
        (
            "/ws/2/release-group",
            Some(("query", "ok computer")),
            "ws2/search_release_group_ok_computer.json",
        ),
        (
            "/ws/2/artist/a74b1b7f-71a5-4011-9441-d0b5e4122711",
            None,
            "ws2/artist_radiohead.json",
        ),
        (
            "/ws/2/release-group/b1392450-e666-3926-a536-22c65f834433",
            None,
            "ws2/release_group_ok_computer.json",
        ),
        (
            "/ws/2/release/0b6b4ba0-d36f-47bd-b4ea-6a5b91842d29",
            None,
            "ws2/release_ok_computer_media.json",
        ),
        (
            "/ws/2/release-group",
            Some(("offset", "0")),
            "ws2/browse_radiohead_offset0.json",
        ),
    ];
    for (route, param, file) in routes {
        let mut mock =
            Mock::given(path(route)).and(header("user-agent", starr_adapter_kit::USER_AGENT));
        if let Some((name, value)) = param {
            mock = mock.and(query_param(name, value));
        }
        mock.respond_with(ResponseTemplate::new(200).set_body_string(fixture(file)))
            .mount(&server)
            .await;
    }
    // Genre lanes (spec 0008 slice 2): any genre's search.
    Mock::given(path("/ws/2/release-group"))
        .and(wiremock::matchers::query_param_contains("query", "tag:"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixture("ws2/search_release_group_genre_rock.json")),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path("/ws/2/release-group"))
        .and(query_param("offset", "100"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixture("ws2/browse_radiohead_offset100.json")),
        )
        .mount(&server)
        .await;
    server
}

pub async fn fake_listenbrainz() -> MockServer {
    let server = MockServer::start().await;
    // Genre lanes are ranked by listens (maintainer 2026-11-07).
    Mock::given(path("/1/popularity/release-group"))
        .and(header("user-agent", starr_adapter_kit::USER_AGENT))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixture("listenbrainz/popularity_release_group_rock.json")),
        )
        .mount(&server)
        .await;
    Mock::given(path("/1/stats/sitewide/release-groups"))
        .and(header("user-agent", starr_adapter_kit::USER_AGENT))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixture("listenbrainz/sitewide_release_groups_week.json")),
        )
        .mount(&server)
        .await;
    Mock::given(path("/1/explore/fresh-releases/"))
        .and(header("user-agent", starr_adapter_kit::USER_AGENT))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture("listenbrainz/fresh_releases.json")),
        )
        .mount(&server)
        .await;
    server
}

pub struct Music {
    pub env: Env,
    pub mia: Member,
    pub catalog_role: i64,
    pub mb: MockServer,
    pub lb: MockServer,
}

impl Music {
    pub async fn admin(&self, req: super::Req) -> TestResponse {
        self.env.send(req).await
    }

    pub async fn mia(&self, req: super::Req) -> TestResponse {
        req.cookie(&self.mia.cookie).send(&self.env.app).await
    }
}

/// `MusicBrainz` connected (no key), assigned to music as catalog, music on;
/// Mia is entitled to music only. With `relevance` saved, Discover is ready.
pub async fn music(relevance: Option<Value>) -> Music {
    let mut env = env().await;
    let mb = fake_musicbrainz().await;
    let lb = fake_listenbrainz().await;
    // This process reads lists from the ListenBrainz fake.
    let access = CatalogAccess::new(CatalogTokenKey::generate())
        .with_listenbrainz_base(&url::Url::parse(&lb.uri()).unwrap());
    let access = Arc::new(access);
    env.app.state = env.app.state.clone().with_catalog(Arc::clone(&access));
    // The worker reads the catalog too (`follows.check`), as in one process.
    env.worker_catalog(access);
    env.app.router = app(env.app.state.clone());
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "musicbrainz", "name": "MusicBrainz", "baseUrl": mb.uri() }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let id = res.body["service"]["id"].as_i64().unwrap();
    let res = env
        .send(post(
            "/api/v1/categories/music/roles",
            json!({ "kind": "catalog", "serviceId": id }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let catalog_role = res.body["catalogs"][0]["id"].as_i64().unwrap();
    let res = env
        .send(patch(
            "/api/v1/categories/music",
            json!({ "enabled": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    if let Some(relevance) = relevance {
        let res = env
            .send(put(
                &format!("/api/v1/categories/music/roles/{catalog_role}/relevance"),
                relevance,
            ))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    }
    let mia = member(
        &env,
        "mia",
        json!([{ "category": "music", "enabled": true, "requestMode": "approval" }]),
    )
    .await;
    Music {
        env,
        mia,
        catalog_role,
        mb,
        lb,
    }
}
