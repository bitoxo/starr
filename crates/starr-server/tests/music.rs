//! Music catalog (spec 0006 slice 2): `MusicBrainz` as a service without a
//! credential, the music catalog role with its release-type relevance,
//! Discover lanes from `ListenBrainz`, search over artists and albums, and the
//! artist and album views (album requests: `music_requests.rs`).
//! `MusicBrainz` and `ListenBrainz` are `wiremock` fakes serving the adapter's
//! hand-written fixtures; nothing real is contacted.
#![allow(clippy::unwrap_used, clippy::type_complexity)]

mod common;

use axum::http::StatusCode;
use common::media::{TMDB_TOKEN, fake_tmdb, member, put, service};
use common::music::music;
use common::services::{API_KEY, env, patch};
use common::{TestResponse, get, post};
use serde_json::{Value, json};
use wiremock::matchers::path;
use wiremock::{Mock, ResponseTemplate};

const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";

fn titles(res: &TestResponse) -> Vec<&str> {
    res.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn musicbrainz_takes_no_key_and_offers_the_music_lists() {
    let env = env().await;
    let types = env.send(get("/api/v1/service-types")).await;
    let mb = types
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["typeId"] == "musicbrainz")
        .unwrap();
    assert_eq!(mb["needsCredential"], false);
    assert_eq!(mb["defaultBaseUrl"], "https://musicbrainz.org");
    assert_eq!(mb["categories"], json!(["music"]));
    let tmdb = types
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["typeId"] == "tmdb")
        .unwrap();
    assert_eq!(tmdb["needsCredential"], true);
    // A key given to MusicBrainz is refused: nothing secret goes there.
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "musicbrainz", "name": "MB", "baseUrl": "https://musicbrainz.org", "apiKey": API_KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "validation.api_key");
    // Radarr still needs one.
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "radarr", "name": "R", "baseUrl": "http://radarr:7878" }),
        ))
        .await;
    assert_eq!(res.code(), "validation.api_key");
}

#[tokio::test]
async fn the_music_catalog_takes_release_types_and_no_source_yet() {
    let m = music(None).await;
    let categories = m.admin(get("/api/v1/categories")).await;
    let music = categories
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == "music")
        .unwrap();
    assert_eq!(music["catalogFlow"], true);
    assert_eq!(music["requestFlow"], true);
    // Without Lidarr nobody can request yet.
    assert_eq!(music["requestsReady"], false);
    assert_eq!(music["catalogs"][0]["adapterType"], "musicbrainz");
    assert_eq!(music["discoverReady"], false);
    // Albums and EPs without secondary types, as the starting point.
    assert_eq!(
        music["defaultRelevance"]["releaseTypes"],
        json!(["album", "ep"])
    );
    assert_eq!(
        music["defaultRelevance"]["excludedSecondaryTypes"]
            .as_array()
            .unwrap()
            .len(),
        12
    );
    let url = format!(
        "/api/v1/categories/music/roles/{}/relevance",
        m.catalog_role
    );
    for body in [
        json!({ "originalLanguages": ["en"] }),
        json!({ "providerRegion": "CH" }),
        json!({ "excludedGenres": [1] }),
    ] {
        let res = m.admin(put(&url, body.clone())).await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(res.code(), "validation.relevance_not_offered", "{body}");
    }
    let res = m
        .admin(put(
            &url,
            json!({ "releaseTypes": ["ep", "album", "ep"], "excludedSecondaryTypes": ["live"] }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["discoverReady"], true);
    assert_eq!(
        res.body["catalogs"][0]["relevance"]["releaseTypes"],
        json!(["album", "ep"])
    );
    // Music's source is Lidarr (slice 3), never Radarr.
    let radarr = m
        .admin(post(
            "/api/v1/services",
            json!({ "adapterType": "radarr", "name": "Radarr", "baseUrl": "http://127.0.0.1:9", "apiKey": API_KEY }),
        ))
        .await;
    let radarr = radarr.body["service"]["id"].as_i64().unwrap();
    let res = m
        .admin(post(
            "/api/v1/categories/music/roles",
            json!({ "kind": "source", "serviceId": radarr }),
        ))
        .await;
    assert_eq!(res.code(), "categories.adapter_unsuited");
    // TMDB-only reference data is simply empty for MusicBrainz.
    let service = music["catalogs"][0]["serviceId"].as_i64().unwrap();
    let reference = m
        .admin(get(&format!(
            "/api/v1/services/{service}/catalog/reference"
        )))
        .await;
    assert_eq!(reference.status, StatusCode::OK, "{}", reference.raw);
    assert_eq!(reference.body["languages"], json!([]));
    // The preview shows the Trending lane, from ListenBrainz.
    let preview = m
        .admin(post(
            &format!("/api/v1/services/{service}/catalog/preview"),
            json!({ "kind": "album", "relevance": { "releaseTypes": ["album"] } }),
        ))
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.raw);
    assert_eq!(preview.body[0]["title"], "Monsters");
    assert_eq!(preview.body[0]["kind"], "album");
    assert_eq!(
        preview.body[0]["poster"],
        "326b4a29-dff5-4fab-87dc-efc1494001c6"
    );
}

#[tokio::test]
async fn members_discover_music_lanes_from_the_catalogs_own_lists() {
    let m = music(Some(
        json!({ "releaseTypes": ["album", "ep"], "excludedSecondaryTypes": ["live"] }),
    ))
    .await;
    let discover = m.mia(get("/api/v1/discover")).await;
    assert_eq!(discover.status, StatusCode::OK, "{}", discover.raw);
    let music = &discover.body[0];
    assert_eq!(music["category"], "music");
    assert_eq!(music["kind"], "album");
    assert_eq!(music["discoverReady"], true);
    // No source, no request flow: requests are closed.
    assert_eq!(music["requestable"], false);
    assert_eq!(
        music["lists"],
        json!(["trending", "fresh_releases", "popular"])
    );

    let trending = m.mia(get("/api/v1/discover/music/trending")).await;
    assert_eq!(trending.status, StatusCode::OK, "{}", trending.raw);
    assert_eq!(titles(&trending), ["Monsters", "After Hours"]);
    let card = &trending.body["items"][1];
    assert_eq!(card["kind"], "album");
    assert_eq!(card["id"], "78570bea-2a26-467c-a3db-c52723ceb394");
    assert_eq!(card["artist"], "The Weeknd");
    assert_eq!(card["poster"], "78570bea-2a26-467c-a3db-c52723ceb394");
    assert_eq!(card["status"], "new");
    assert_eq!(card["requestedByMe"], false);

    let fresh = m.mia(get("/api/v1/discover/music/fresh_releases")).await;
    assert_eq!(fresh.status, StatusCode::OK, "{}", fresh.raw);
    // The relevance's release types: the live album and the single stay out.
    assert_eq!(
        titles(&fresh),
        ["The Marvelous Real", "Profound Mysteries IV"]
    );
    assert_eq!(fresh.body["items"][0]["releaseType"], "ep");

    // TMDB lists are not music lists.
    let res = m.mia(get("/api/v1/discover/music/top_rated")).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.code(), "discover.unknown_list");

    // The browser never gets a foreign URL, and every upstream request
    // carried Starr's agent and nothing else identifying.
    assert!(!trending.raw.contains("http"), "{}", trending.raw);
    for server in [&m.mb, &m.lb] {
        for request in server.received_requests().await.unwrap() {
            assert_eq!(
                request.headers.get("user-agent").unwrap(),
                starr_adapter_kit::USER_AGENT
            );
            assert!(request.headers.get("authorization").is_none());
        }
    }
}

#[tokio::test]
async fn search_finds_artists_and_albums() {
    let m = music(Some(json!({}))).await;
    let res = m.mia(get("/api/v1/search?query=ok%20computer")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let kinds: Vec<&str> = res.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["artist", "artist", "album", "album"]);
    assert_eq!(res.body["items"][0]["title"], "Radiohead");
    assert_eq!(res.body["items"][0]["poster"], Value::Null);
    assert_eq!(res.body["items"][2]["id"], OK_COMPUTER);
    assert_eq!(res.body["items"][2]["artist"], "Radiohead");
    assert_eq!(res.body["items"][2]["releaseType"], "album");
    assert_eq!(res.body["next"], 2);
}

#[tokio::test]
async fn artist_and_album_views_show_the_discography_and_album_facts() {
    let m = music(Some(
        json!({ "releaseTypes": ["album", "ep"], "excludedSecondaryTypes": ["live"] }),
    ))
    .await;
    let artist = m.mia(get(&format!("/api/v1/artists/{RADIOHEAD}"))).await;
    assert_eq!(artist.status, StatusCode::OK, "{}", artist.raw);
    assert_eq!(artist.body["name"], "Radiohead");
    assert_eq!(artist.body["category"], "music");
    assert_eq!(artist.body["requestable"], false);
    // The newest released album or EP stands in for an artist picture.
    assert_eq!(artist.body["cover"], "c4d5e6f7-0a1b-4c2d-8e3f-4a5b6c7d8e9f");
    let albums = artist.body["albums"].as_array().unwrap();
    let view: Vec<(&str, bool)> = albums
        .iter()
        .map(|a| {
            (
                a["title"].as_str().unwrap(),
                a["inDefaultView"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        view,
        [
            ("Next Album", true),
            ("Live at the Astoria", false),
            ("A Moon Shaped Pool", true),
            ("Daydreaming", false),
            ("OK Computer", true),
            ("Undated EP", true),
        ]
    );
    assert_eq!(albums[1]["secondaryTypes"], json!(["live"]));
    assert_eq!(albums[3]["releaseType"], "single");

    let album = m.mia(get(&format!("/api/v1/albums/{OK_COMPUTER}"))).await;
    assert_eq!(album.status, StatusCode::OK, "{}", album.raw);
    assert_eq!(album.body["title"], "OK Computer");
    assert_eq!(album.body["artist"], "Radiohead & Friends");
    assert_eq!(album.body["artistId"], RADIOHEAD);
    assert_eq!(album.body["year"], 1997);
    assert_eq!(album.body["trackCount"], 14);
    assert_eq!(album.body["releaseType"], "album");
    assert_eq!(album.body["cover"], OK_COMPUTER);
    assert_eq!(album.body["status"], "new");
    assert_eq!(album.body["requestable"], false);
    // Tracklist, label and genres of the counted release (spec 0008),
    // with the MusicBrainz genre licence credited.
    let tracks = album.body["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 14);
    assert_eq!(
        tracks[0],
        json!({ "disc": 1, "discTitle": "", "position": 1, "title": "Airbag",
                "lengthSeconds": 284, "artist": null })
    );
    assert_eq!(tracks[13]["artist"], "Radiohead");
    assert_eq!(album.body["label"], "Parlophone");
    assert_eq!(
        album.body["tracklistRelease"],
        json!({ "date": "1997-05-21", "country": "GB", "releaseCount": 3 })
    );
    assert_eq!(album.body["genres"][0], "alternative rock");
    assert_eq!(album.body["attributions"][0]["source"], "MusicBrainz");
    assert_eq!(album.body["attributions"][0]["licence"], "CC BY-NC-SA 3.0");
    assert_eq!(
        artist.body["genres"],
        json!(["alternative rock", "art rock", "electronic"])
    );
    assert_eq!(artist.body["attributions"][0]["data"], "genres");

    // Non-canonical ids name nothing; the movie view takes no music.
    let res = m
        .mia(get(&format!(
            "/api/v1/albums/{}",
            OK_COMPUTER.to_uppercase()
        )))
        .await;
    assert_eq!(res.code(), "catalog.not_found");
    let res = m
        .mia(get(&format!("/api/v1/titles/album/{OK_COMPUTER}")))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    // No music requests without a source (Lidarr).
    let res = m
        .mia(post(
            "/api/v1/requests",
            json!({ "kind": "album", "titleId": OK_COMPUTER }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "requests.category_not_ready");
}

#[tokio::test]
async fn music_views_need_the_music_entitlement_and_a_ready_catalog() {
    let m = music(None).await;
    // Not ready: no relevance saved.
    let res = m.mia(get(&format!("/api/v1/albums/{OK_COMPUTER}"))).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
    assert_eq!(res.code(), "discover.unavailable");
    let res = m.mia(get("/api/v1/discover/music/trending")).await;
    assert_eq!(res.code(), "discover.unavailable");
    // Ready, but Noah is entitled to movies only.
    let url = format!(
        "/api/v1/categories/music/roles/{}/relevance",
        m.catalog_role
    );
    assert_eq!(m.admin(put(&url, json!({}))).await.status, StatusCode::OK);
    let noah = member(
        &m.env,
        "noah",
        json!([{ "category": "movies", "enabled": true, "requestMode": "auto" }]),
    )
    .await;
    let res = get(&format!("/api/v1/artists/{RADIOHEAD}"))
        .cookie(&noah.cookie)
        .send(&m.env.app)
        .await;
    assert_eq!(res.code(), "discover.unavailable");
    // Admins use their own views.
    let res = m.admin(get(&format!("/api/v1/albums/{OK_COMPUTER}"))).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
}

/// A busy `MusicBrainz` must not hide movie results: catalogs fail on their
/// own, the search only when every catalog failed.
#[tokio::test]
async fn a_failing_music_catalog_does_not_hide_movie_results() {
    let m = music(Some(json!({}))).await;
    let tmdb = fake_tmdb().await;
    let tmdb_id = service(&m.env, "tmdb", &tmdb.uri(), TMDB_TOKEN).await;
    let res = m
        .admin(post(
            "/api/v1/categories/movies/roles",
            json!({ "kind": "catalog", "serviceId": tmdb_id }),
        ))
        .await;
    let role = res.body["catalogs"][0]["id"].as_i64().unwrap();
    m.admin(put(
        &format!("/api/v1/categories/movies/roles/{role}/relevance"),
        json!({}),
    ))
    .await;
    m.admin(patch(
        "/api/v1/categories/movies",
        json!({ "enabled": true }),
    ))
    .await;
    let lena = member(
        &m.env,
        "lena",
        json!([
            { "category": "movies", "enabled": true, "requestMode": "auto" },
            { "category": "music", "enabled": true, "requestMode": "auto" },
        ]),
    )
    .await;
    Mock::given(path("/ws/2/artist"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .mount(&m.mb)
        .await;
    let res = get("/api/v1/search?query=inception")
        .cookie(&lena.cookie)
        .send(&m.env.app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let kinds: Vec<&str> = res.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"movie"), "{kinds:?}");
    assert!(!kinds.contains(&"artist"), "{kinds:?}");
    // Music alone: the failure shows, without a raw upstream error.
    let res = m.mia(get("/api/v1/search?query=x")).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE, "{}", res.raw);
    assert_eq!(res.code(), "catalog.unavailable");
}

#[tokio::test]
async fn only_one_musicbrainz_can_be_connected() {
    let m = music(None).await;
    let res = m
        .admin(post(
            "/api/v1/services",
            json!({ "adapterType": "musicbrainz", "name": "MB 2", "baseUrl": "https://musicbrainz.org" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "services.musicbrainz_exists");
}
/// Genre lanes (spec 0008 Q17 B, F-8): the music category lists the fixed
/// genres with the licence credit; a lane is empty until
/// `catalog.refresh-lists` filled it, and the web never asks `MusicBrainz`
/// for it.
#[tokio::test]
async fn music_genre_lanes_come_from_the_background_refresh() {
    let m = music(Some(json!({}))).await;
    let discover = m.mia(get("/api/v1/discover")).await;
    let music = &discover.body[0];
    let genres: Vec<&str> = music["genres"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert_eq!(genres.len(), 12);
    assert_eq!(genres[..4], ["indie", "electronic", "jazz", "hip hop"]);
    assert_eq!(music["genres"][3]["name"], "Hip-Hop");
    assert_eq!(music["providers"], json!([]));
    assert_eq!(
        music["attributions"],
        json!([{ "data": "genres", "source": "MusicBrainz", "licence": "CC BY-NC-SA 3.0",
                 "url": "https://creativecommons.org/licenses/by-nc-sa/3.0/" }])
    );

    let lane = "/api/v1/discover/music/genre?genre=rock";
    let res = m.mia(get(lane)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["items"], json!([]));
    assert_eq!(tagged(&m.mb).await, 0, "the web asked MusicBrainz");

    let res = m
        .admin(post(
            "/api/v1/jobs/catalog.refresh-lists/trigger",
            json!({}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    m.env.drain().await;
    assert_eq!(tagged(&m.mb).await, 12);
    let res = m.mia(get(lane)).await;
    // Ranked by ListenBrainz listens; the release without any is dropped.
    assert_eq!(
        titles(&res),
        ["OK Computer OKNOTOK 1997 2017", "OK Computer"]
    );
    assert_eq!(res.body["next"], Value::Null);
    assert_eq!(tagged(&m.mb).await, 12, "read from the cache");
    // One page per lane; unknown genres and filters on lanes are refused.
    let res = m.mia(get(&format!("{lane}&page=2"))).await;
    assert_eq!(res.body["items"], json!([]));
    for path in [
        "/api/v1/discover/music/genre?genre=polka",
        "/api/v1/discover/music/trending?genre=rock",
    ] {
        let res = m.mia(get(path)).await;
        assert_eq!(res.code(), "validation.genre", "{path}: {}", res.raw);
    }
}

/// `MusicBrainz` requests of genre lanes the fake received.
async fn tagged(mb: &wiremock::MockServer) -> usize {
    mb.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.query().is_some_and(|q| q.contains("tag")))
        .count()
}
