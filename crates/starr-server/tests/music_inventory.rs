//! Music availability from the music libraries (spec 0006 slice 5, Q7 B):
//! Navidrome as music's consume service, albums matched by their
//! `MusicBrainz` release group only, "arriving" while Lidarr has the files
//! and Navidrome does not list them yet, an admin hint when that lasts,
//! "Open in" for albums and removal (0004 R4) per album. Two members on
//! every rule (lesson 4). `MusicBrainz`, Lidarr and Navidrome are local
//! fakes; nothing real is contacted.
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;
use common::lidarr::{FakeLidarr, MOON_SHAPED_POOL, OK_COMPUTER, RADIOHEAD, fake_lidarr};
use common::media::{Member, member, put};
use common::music::{Music, music};
use common::navidrome::{FakeNavidrome, ND_PASSWORD, ND_USER, fake_navidrome};
use common::services::API_KEY;
use common::{TestResponse, get, post};
use serde_json::{Value, json};
use wiremock::MockServer;

/// In the `MusicBrainz` fixture discography; nobody requests it.
const ASTORIA: &str = "1f0b6c3a-2e4d-4a5b-8c9d-0e1f2a3b4c5d";
const ND_OK_COMPUTER: &str = "0Fq7nT2sYp8Vw3Ld5Hk9Bz";
const PUBLIC: &str = "https://music.example.com";

struct Setup {
    m: Music,
    noah: Member,
    lidarr: FakeLidarr,
    navidrome: FakeNavidrome,
    _servers: [MockServer; 2],
}

impl Setup {
    async fn admin(&self, req: common::Req) -> TestResponse {
        self.m.admin(req).await
    }

    async fn as_member(&self, who: &Member, req: common::Req) -> TestResponse {
        req.cookie(&who.cookie).send(&self.m.env.app).await
    }

    async fn run(&self, kind: &str) {
        let res = self
            .admin(post(&format!("/api/v1/jobs/{kind}/trigger"), json!({})))
            .await;
        assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
        self.m.env.drain().await;
    }

    async fn sql(&self, statement: &'static str) {
        sqlx::query(statement)
            .execute(self.m.env.app.state.db.pool())
            .await
            .unwrap();
    }

    async fn my_request(&self, who: &Member) -> Value {
        self.as_member(who, get("/api/v1/requests")).await.body[0].clone()
    }

    async fn album(&self, who: &Member, id: &str) -> Value {
        self.as_member(who, get(&format!("/api/v1/albums/{id}")))
            .await
            .body
    }
}

/// Music with `MusicBrainz`, Lidarr (fake) as the source and Navidrome
/// (fake, public URL) as the consume service. Noah requests automatically,
/// Mia with approval.
async fn setup() -> Setup {
    let m = music(Some(json!({}))).await;
    let (lidarr, lidarr_server) = fake_lidarr().await;
    let res = m
        .admin(post(
            "/api/v1/services",
            json!({ "adapterType": "lidarr", "name": "Lidarr", "baseUrl": lidarr_server.uri(), "apiKey": API_KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let lidarr_id = res.body["service"]["id"].as_i64().unwrap();
    m.env.drain().await;
    m.admin(put(
        "/api/v1/categories/music/source",
        json!({ "serviceId": lidarr_id }),
    ))
    .await;
    m.admin(post(
        &format!("/api/v1/services/{lidarr_id}/source-lists/refresh"),
        json!({}),
    ))
    .await;
    m.env.drain().await;
    let res = m
        .admin(put(
            "/api/v1/categories/music/source/settings",
            json!({ "qualityProfileId": 2, "metadataProfileId": 1, "rootFolderPath": "/music" }),
        ))
        .await;
    assert_eq!(res.body["requestsReady"], true, "{}", res.raw);

    let (navidrome, navidrome_server) = fake_navidrome().await;
    // Navidrome takes the admin's login, never an API key.
    let res = m
        .admin(post(
            "/api/v1/services",
            json!({ "adapterType": "navidrome", "name": "Navidrome", "baseUrl": navidrome_server.uri(), "apiKey": API_KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    let res = m
        .admin(post(
            "/api/v1/services",
            json!({ "adapterType": "navidrome", "name": "Navidrome", "baseUrl": navidrome_server.uri(),
                    "publicUrl": PUBLIC, "username": ND_USER, "password": ND_PASSWORD }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let navidrome_id = res.body["service"]["id"].as_i64().unwrap();
    m.env.drain().await;
    let service = m
        .admin(get(&format!("/api/v1/services/{navidrome_id}")))
        .await;
    assert_eq!(service.body["lastError"], Value::Null, "{}", service.raw);
    assert_eq!(service.body["detectedVersion"], "0.64.2.0");
    assert!(!service.raw.contains(ND_PASSWORD), "never shown again");
    let res = m
        .admin(post(
            "/api/v1/categories/music/roles",
            json!({ "kind": "consume", "serviceId": navidrome_id }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let noah = member(
        &m.env,
        "noah",
        json!([{ "category": "music", "enabled": true, "requestMode": "auto" }]),
    )
    .await;
    Setup {
        m,
        noah,
        lidarr,
        navidrome,
        _servers: [lidarr_server, navidrome_server],
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one story: requested, arriving, listed, gone
async fn albums_arrive_until_navidrome_lists_their_release_group() {
    let s = setup().await;
    let res = s
        .as_member(
            &s.noah,
            post(
                "/api/v1/requests",
                json!({ "kind": "artist", "titleId": RADIOHEAD, "albums": [OK_COMPUTER, MOON_SHAPED_POOL] }),
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    s.m.env.drain().await;

    // Lidarr has OK Computer, Navidrome has not scanned it (or cannot
    // match it: untagged files never count, not even by name).
    s.lidarr.state().complete(OK_COMPUTER, 12);
    s.navidrome
        .state()
        .scan("untagged", "OK Computer", None, 12);
    s.run("inventory.sync").await;
    s.run("acquisitions.sync").await;
    let request = s.my_request(&s.noah).await;
    assert_eq!(request["items"][0]["state"], "in_progress", "{request}");
    assert_eq!(request["items"][0]["stage"], "arriving");
    assert_eq!(request["arrivingIn"], "Navidrome");
    assert_eq!(request["openIn"], json!([]));
    // Not yet an admin matter: the libraries usually scan within minutes.
    let unlisted = s.admin(get("/api/v1/admin/acquisitions/unlisted")).await;
    assert_eq!(unlisted.status, StatusCode::OK, "{}", unlisted.raw);
    assert_eq!(unlisted.body, json!([]));
    // An hour later it is.
    s.sql("UPDATE acquisition SET download_stage_since = '2026-01-01T00:00:00.000Z' WHERE download_stage = 'arriving'")
        .await;
    let unlisted = s.admin(get("/api/v1/admin/acquisitions/unlisted")).await;
    assert_eq!(
        unlisted.body.as_array().unwrap().len(),
        1,
        "{}",
        unlisted.raw
    );
    assert_eq!(unlisted.body[0]["artist"], "Radiohead");
    assert_eq!(unlisted.body[0]["album"], "OK Computer");
    assert_eq!(unlisted.body[0]["releaseGroup"], OK_COMPUTER);
    assert_eq!(unlisted.body[0]["source"], "Lidarr");
    assert_eq!(unlisted.body[0]["libraries"], "Navidrome");
    let forbidden = s
        .as_member(&s.noah, get("/api/v1/admin/acquisitions/unlisted"))
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);

    // Navidrome scans the tagged files: available, with a link; A Moon
    // Shaped Pool is there with fewer tracks than Lidarr expects.
    {
        let mut nd = s.navidrome.state();
        nd.scan(ND_OK_COMPUTER, "OK Computer", Some(OK_COMPUTER), 12);
        nd.scan("pool", "A Moon Shaped Pool", Some(MOON_SHAPED_POOL), 5);
        nd.scan("astoria", "Live at the Astoria", Some(ASTORIA), 9);
    }
    s.lidarr.state().complete(MOON_SHAPED_POOL, 11);
    s.run("inventory.sync").await;
    s.run("acquisitions.sync").await;
    let request = s.my_request(&s.noah).await;
    let states: Vec<&str> = request["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["available", "partially_available"], "{request}");
    let link = format!("{PUBLIC}/app/#/album/{ND_OK_COMPUTER}/show");
    assert_eq!(request["items"][0]["openIn"][0]["url"], link.as_str());
    assert_eq!(request["items"][0]["openIn"][0]["service"], "navidrome");
    assert_eq!(
        request["items"][1]["openIn"][0]["url"],
        format!("{PUBLIC}/app/#/album/pool/show")
    );
    assert_eq!(request["openIn"], json!([]), "two albums: links per album");
    let ready = request["items"][0]["journey"]["steps"].as_array().unwrap();
    assert_eq!(ready.last().unwrap()["step"], "ready");
    assert_eq!(
        s.admin(get("/api/v1/admin/acquisitions/unlisted"))
            .await
            .body,
        json!([])
    );

    // Mia sees the same albums available, opens them, and cannot request
    // them again; an album only Navidrome has counts too.
    let album = s.album(&s.m.mia, OK_COMPUTER).await;
    assert_eq!(album["status"], "available", "{album}");
    assert_eq!(album["openIn"][0]["url"], link.as_str());
    assert_eq!(album["requestedByMe"], false);
    let artist = s
        .as_member(&s.m.mia, get(&format!("/api/v1/artists/{RADIOHEAD}")))
        .await;
    let shown = |id: &str| {
        artist.body["albums"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .map(|a| a["status"].as_str().unwrap().to_owned())
    };
    assert_eq!(shown(OK_COMPUTER).as_deref(), Some("available"));
    assert_eq!(shown(ASTORIA).as_deref(), Some("available"));
    let res =
        s.m.mia(post(
            "/api/v1/requests",
            json!({ "kind": "artist", "titleId": RADIOHEAD, "albums": [ASTORIA] }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "requests.already_available");

    // OK Computer leaves Navidrome (two complete runs, then six hours):
    // Lidarr still has the files, so it stays; once those are gone too,
    // it is removed and new again for everyone.
    s.navidrome.state().lose(ND_OK_COMPUTER);
    s.run("inventory.sync").await;
    s.sql("UPDATE inventory SET last_seen = '2026-01-01T00:00:00.000Z' WHERE kind = 'album' AND remote_id = '0Fq7nT2sYp8Vw3Ld5Hk9Bz'").await;
    s.run("inventory.sync").await;
    s.sql("UPDATE acquisition SET synced_at = '2026-01-01T00:00:00.000Z'")
        .await;
    s.run("acquisitions.sync").await;
    let request = s.my_request(&s.noah).await;
    assert_eq!(
        request["items"][0]["state"], "available",
        "Lidarr has it: kept"
    );
    s.lidarr.state().album(OK_COMPUTER)["statistics"]["trackFileCount"] = json!(0);
    s.sql("UPDATE acquisition SET synced_at = '2026-01-01T00:00:00.000Z'")
        .await;
    s.run("acquisitions.sync").await;
    let request = s.my_request(&s.noah).await;
    assert_eq!(request["items"][0]["state"], "removed", "{request}");
    let album = s.album(&s.m.mia, OK_COMPUTER).await;
    assert_eq!(album["status"], "new", "{album}");
    assert_eq!(album["openIn"], json!([]));

    // Navidrome was only read, with one login for the whole story.
    let nd = s.navidrome.state();
    assert_eq!(nd.logins, 1);
    assert!(
        nd.requests
            .iter()
            .all(|r| r.starts_with("GET ") || r == "POST /auth/login"),
        "{:?}",
        nd.requests
    );
    assert_eq!(s.lidarr.state().forbidden, Vec::<String>::new());
}

#[tokio::test]
async fn navidromes_libraries_can_be_mapped_to_music() {
    let s = setup().await;
    s.run("inventory.sync").await;
    let unmapped = s.admin(get("/api/v1/libraries/unmapped")).await;
    let names: Vec<(&str, &str)> = unmapped
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["name"].as_str().unwrap(), l["content"].as_str().unwrap()))
        .collect();
    assert_eq!(names, [("Music Library", "music")], "{}", unmapped.raw);
}
