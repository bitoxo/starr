//! Album requests end to end (spec 0006 slice 4): members request albums
//! of an artist, admins approve as for movies and series, the worker adds
//! them to Lidarr (Q5 A) and follows their progress, and removal stops
//! exactly the albums Starr turned on (0004, Q13 A). Two members on every
//! rule (lesson 4). `MusicBrainz` and Lidarr are local fakes built from the
//! recorded and hand-written fixtures; nothing real is contacted. Without a
//! music consume service (as here) an album is available when Lidarr has every
//! track of it.
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;
use common::lidarr::{
    ARTIST_ID, FakeLidarr, MOON_SHAPED_POOL, OK_COMPUTER, RADIOHEAD, fake_lidarr,
};
use common::media::{Member, member, put};
use common::music::{Music, music};
use common::services::API_KEY;
use common::{TestResponse, get, post};
use serde_json::{Value, json};
use wiremock::MockServer;

struct Setup {
    m: Music,
    noah: Member,
    lidarr: FakeLidarr,
    lidarr_id: i64,
    _server: MockServer,
}

impl Setup {
    async fn admin(&self, req: common::Req) -> TestResponse {
        self.m.admin(req).await
    }

    async fn as_member(&self, who: &Member, req: common::Req) -> TestResponse {
        req.cookie(&who.cookie).send(&self.m.env.app).await
    }

    /// `who` requests `albums` of Radiohead from the artist page.
    async fn request(&self, who: &Member, albums: &[&str]) -> TestResponse {
        self.as_member(
            who,
            post(
                "/api/v1/requests",
                json!({ "kind": "artist", "titleId": RADIOHEAD, "albums": albums }),
            ),
        )
        .await
    }

    /// Run `kind` now and every job it leaves behind.
    async fn run(&self, kind: &str) {
        let res = self
            .admin(post(&format!("/api/v1/jobs/{kind}/trigger"), json!({})))
            .await;
        assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
        self.m.env.drain().await;
    }

    async fn approve(&self, request: i64, member: i64) -> TestResponse {
        self.admin(post(
            "/api/v1/admin/approvals/approve",
            json!({ "requests": [{ "id": request, "memberId": member }] }),
        ))
        .await
    }

    async fn my_requests(&self, who: &Member) -> Value {
        self.as_member(who, get("/api/v1/requests")).await.body
    }

    fn writes(&self) -> Vec<(String, String, Value)> {
        self.lidarr.state().writes.clone()
    }
}

/// `MusicBrainz` as the music catalog with Discover ready, Lidarr (fake) as
/// the source with quality profile 2, metadata profile 1 ("Standard") and
/// `/music`. Mia requests music with approval, Noah automatically.
async fn setup() -> Setup {
    let m = music(Some(json!({}))).await;
    let (lidarr, server) = fake_lidarr().await;
    let res = m
        .admin(post(
            "/api/v1/services",
            json!({ "adapterType": "lidarr", "name": "Lidarr", "baseUrl": server.uri(), "apiKey": API_KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let lidarr_id = res.body["service"]["id"].as_i64().unwrap();
    m.env.drain().await;
    let res = m
        .admin(put(
            "/api/v1/categories/music/source",
            json!({ "serviceId": lidarr_id }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
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
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["requestsReady"], true, "{}", res.raw);
    let noah = member(
        &m.env,
        "noah",
        json!([{ "category": "music", "enabled": true, "requestMode": "auto" }]),
    )
    .await;
    // Probe and lists only read.
    assert_eq!(lidarr.state().writes.len(), 0);
    Setup {
        m,
        noah,
        lidarr,
        lidarr_id,
        _server: server,
    }
}

fn album_titles(request: &Value) -> Vec<&str> {
    request["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["album"]["title"].as_str().unwrap())
        .collect()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one story, request to available
async fn two_albums_of_a_new_artist_reach_lidarr_after_approval() {
    let s = setup().await;
    let artist = s
        .as_member(&s.m.mia, get(&format!("/api/v1/artists/{RADIOHEAD}")))
        .await;
    assert_eq!(artist.body["requestable"], true, "{}", artist.raw);

    let res = s.request(&s.m.mia, &[OK_COMPUTER, MOON_SHAPED_POOL]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let request = &res.body["request"];
    assert_eq!(request["kind"], "artist");
    assert_eq!(request["titleId"], RADIOHEAD);
    assert_eq!(request["title"], "Radiohead");
    assert_eq!(request["category"], "music");
    assert_eq!(request["state"], "pending_approval");
    assert_eq!(request["follow"], false);
    // The first album's cover stands in (artists have no picture).
    assert_eq!(request["poster"], OK_COMPUTER);
    assert_eq!(album_titles(request), ["OK Computer", "A Moon Shaped Pool"]);
    assert_eq!(request["items"][0]["album"]["id"], OK_COMPUTER);
    assert_eq!(request["items"][0]["album"]["year"], 1997);
    assert_eq!(request["items"][0]["season"], Value::Null);
    let id = request["id"].as_i64().unwrap();

    // Nothing goes out before the approval.
    s.m.env.drain().await;
    assert_eq!(s.writes().len(), 0);
    // The others see "requested", never by whom; the artist too.
    let album = s
        .as_member(&s.noah, get(&format!("/api/v1/albums/{OK_COMPUTER}")))
        .await;
    assert_eq!(album.body["status"], "requested", "{}", album.raw);
    assert_eq!(album.body["requestedByMe"], false);
    assert_eq!(album.body["journeys"], json!([]));
    let artist = s
        .as_member(&s.noah, get(&format!("/api/v1/artists/{RADIOHEAD}")))
        .await;
    let statuses: Vec<(&str, &str)> = artist.body["albums"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["title"].as_str().unwrap(), a["status"].as_str().unwrap()))
        .collect();
    assert!(
        statuses.contains(&("OK Computer", "requested")),
        "{statuses:?}"
    );
    assert!(statuses.contains(&("Daydreaming", "new")), "{statuses:?}");
    let mine = s
        .as_member(&s.m.mia, get(&format!("/api/v1/albums/{OK_COMPUTER}")))
        .await;
    assert_eq!(mine.body["requestedByMe"], true);
    assert_eq!(
        mine.body["journeys"][0]["journey"]["steps"][1]["step"],
        "awaiting_approval"
    );

    // Needs you: the admin sees who asked for which albums.
    let inbox = s.admin(get("/api/v1/admin/approvals")).await;
    assert_eq!(inbox.body[0]["member"]["username"], "mia");
    assert_eq!(
        album_titles(&inbox.body[0]),
        ["OK Computer", "A Moon Shaped Pool"]
    );
    let admin_notes = s.admin(get("/api/v1/admin/notifications")).await;
    assert_eq!(
        admin_notes.body["items"][0]["title"], "Radiohead – OK Computer, A Moon Shaped Pool",
        "{}",
        admin_notes.raw
    );

    let res = s.approve(id, s.m.mia.id).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["decided"], json!([id]));
    s.m.env.drain().await;

    // Lidarr: the artist created with Starr's profiles, monitored, nothing
    // new followed by Lidarr itself, only the two albums monitored.
    {
        let state = s.lidarr.state();
        assert!(state.forbidden.is_empty(), "{:?}", state.forbidden);
        let posts = state.writes_to("POST", "/api/v1/album");
        assert_eq!(posts.len(), 2, "{:?}", state.writes);
        let artist = &posts[0]["artist"];
        assert_eq!(artist["foreignArtistId"], RADIOHEAD);
        assert_eq!(artist["qualityProfileId"], 2);
        assert_eq!(artist["metadataProfileId"], 1);
        assert_eq!(artist["rootFolderPath"], "/music");
        assert_eq!(artist["monitored"], true);
        assert_eq!(artist["monitorNewItems"], "none");
        assert_eq!(artist["addOptions"]["monitor"], "none");
        let to_monitor: Vec<&str> = artist["addOptions"]["albumsToMonitor"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(to_monitor.len(), 2);
        assert!(to_monitor.contains(&OK_COMPUTER) && to_monitor.contains(&MOON_SHAPED_POOL));
        // The second album goes under the artist Lidarr has now.
        assert_eq!(posts[1]["artist"]["id"], ARTIST_ID);
        for post in &posts {
            assert_eq!(post["monitored"], true);
            assert_eq!(post["addOptions"]["searchForNewAlbum"], true);
        }
        assert!(
            state
                .albums
                .iter()
                .all(|a| a["monitored"] == true && a["artistId"] == ARTIST_ID)
        );
    }
    let request = &s.my_requests(&s.m.mia).await[0];
    assert_eq!(request["state"], "submitted", "{request}");
    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(acquisitions.body.as_array().unwrap().len(), 2);
    assert_eq!(acquisitions.body[0]["title"], "Radiohead");
    assert!(acquisitions.body[0]["album"]["title"].is_string());
    // Each album shows its own cover.
    assert_eq!(
        acquisitions.body[0]["poster"],
        acquisitions.body[0]["album"]["id"]
    );
    assert_eq!(acquisitions.body[0]["adapterType"], "lidarr");

    // Lidarr downloads OK Computer and imports half of A Moon Shaped Pool.
    {
        let mut state = s.lidarr.state();
        state.complete(OK_COMPUTER, 12);
        state.album(MOON_SHAPED_POOL)["statistics"]["trackFileCount"] = json!(5);
    }
    s.run("acquisitions.sync").await;
    let request = &s.my_requests(&s.m.mia).await[0];
    let states: Vec<&str> = request["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["available", "partially_available"], "{request}");
    assert_eq!(request["state"], "partially_available");
    let ready = &request["items"][0]["journey"]["steps"];
    assert_eq!(ready.as_array().unwrap().last().unwrap()["step"], "ready");
    // Music has no consume service here: no "Open in", no arriving.
    assert_eq!(request["openIn"], json!([]));
    let notes = s.as_member(&s.m.mia, get("/api/v1/notifications")).await;
    let kinds: Vec<(&str, &str)> = notes.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["kind"].as_str().unwrap(), n["title"].as_str().unwrap()))
        .collect();
    assert!(
        kinds.contains(&(
            "request_partially_available",
            "Radiohead – OK Computer, A Moon Shaped Pool"
        )),
        "{kinds:?}"
    );
    assert!(kinds.contains(&(
        "request_approved",
        "Radiohead – OK Computer, A Moon Shaped Pool"
    )));
    let album = s
        .as_member(&s.noah, get(&format!("/api/v1/albums/{OK_COMPUTER}")))
        .await;
    assert_eq!(album.body["status"], "available");
    let artist = s
        .as_member(&s.noah, get(&format!("/api/v1/artists/{RADIOHEAD}")))
        .await;
    assert_eq!(artist.status, StatusCode::OK);
    // Syncing only read.
    assert_eq!(s.writes().len(), 2);
}

#[tokio::test]
async fn an_album_already_in_lidarr_with_files_needs_no_write() {
    let s = setup().await;
    s.lidarr.state().radiohead_with(OK_COMPUTER, true, 12);
    // From the album page: the album names its artist.
    let res = s
        .as_member(
            &s.noah,
            post(
                "/api/v1/requests",
                json!({ "kind": "album", "titleId": OK_COMPUTER }),
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["request"]["state"], "approved");
    assert_eq!(album_titles(&res.body["request"]), ["OK Computer"]);
    s.m.env.drain().await;
    s.run("acquisitions.sync").await;
    assert_eq!(s.writes(), vec![], "nothing written for a complete album");
    let request = &s.my_requests(&s.noah).await[0];
    assert_eq!(request["state"], "available", "{request}");
    // Starr changed nothing: cancelling would stop nothing either.
    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(acquisitions.body[0]["state"], "available");
}

#[tokio::test]
async fn an_unmonitored_album_gets_one_monitor_write_and_a_search() {
    let s = setup().await;
    s.lidarr.state().radiohead_with(OK_COMPUTER, false, 0);
    let res = s.request(&s.noah, &[OK_COMPUTER]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    s.m.env.drain().await;
    let state = s.lidarr.state();
    assert!(state.forbidden.is_empty(), "{:?}", state.forbidden);
    let paths: Vec<(&str, &str)> = state
        .writes
        .iter()
        .map(|(m, p, _)| (m.as_str(), p.as_str()))
        .collect();
    assert_eq!(
        paths,
        [
            ("PUT", "/api/v1/album/monitor"),
            ("POST", "/api/v1/command")
        ]
    );
    assert_eq!(state.writes[0].2["monitored"], true);
    assert_eq!(state.writes[1].2["name"], "AlbumSearch");
    // The artist Starr found keeps its own settings.
    assert_eq!(state.artist.as_ref().unwrap()["monitorNewItems"], "all");
    assert_eq!(state.artist.as_ref().unwrap()["qualityProfileId"], 1);
}

#[tokio::test]
async fn cancelling_stops_exactly_the_album_and_never_the_artist() {
    let s = setup().await;
    // Both ask for OK Computer; Noah's goes out, Mia attaches.
    let noah = s.request(&s.noah, &[OK_COMPUTER]).await;
    let noah_id = noah.body["request"]["id"].as_i64().unwrap();
    s.m.env.drain().await;
    assert_eq!(s.writes().len(), 1);
    let mia = s.request(&s.m.mia, &[OK_COMPUTER]).await;
    assert_eq!(mia.status, StatusCode::CREATED, "{}", mia.raw);
    // Attaching to a running acquisition counts as approved (D10).
    assert_eq!(mia.body["request"]["state"], "submitted", "{}", mia.raw);
    let mia_id = mia.body["request"]["id"].as_i64().unwrap();

    // One of two cancels: nothing changes in Lidarr.
    let res = s
        .as_member(
            &s.noah,
            post(&format!("/api/v1/requests/{noah_id}/cancel"), json!({})),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    s.m.env.drain().await;
    assert_eq!(s.writes().len(), 1);

    // The last one: exactly one unmonitor of the album, read back.
    let res = s
        .as_member(
            &s.m.mia,
            post(&format!("/api/v1/requests/{mia_id}/cancel"), json!({})),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    s.m.env.drain().await;
    {
        let state = s.lidarr.state();
        assert!(state.forbidden.is_empty(), "{:?}", state.forbidden);
        let puts = state.writes_to("PUT", "/api/v1/album/monitor");
        assert_eq!(puts.len(), 1, "{:?}", state.writes);
        assert_eq!(puts[0]["monitored"], false);
        assert_eq!(puts[0]["albumIds"].as_array().unwrap().len(), 1);
        assert_eq!(state.writes.len(), 2);
        // The artist stays as added (Q13 A).
        let artist = state.artist.as_ref().unwrap();
        assert_eq!(artist["monitored"], true);
        assert_eq!(artist["monitorNewItems"], "none");
    }
    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(acquisitions.body[0]["state"], "cancelled");
    assert!(acquisitions.body[0]["stoppedAt"].is_string());

    // Requesting it again starts over with the member's mode.
    let again = s.request(&s.noah, &[OK_COMPUTER]).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.raw);
    assert_eq!(again.body["request"]["state"], "approved");
}

#[tokio::test]
async fn two_members_cancelling_before_submit_send_nothing() {
    let s = setup().await;
    let paused = s
        .admin(put(
            &format!("/api/v1/services/{}/writes-paused", s.lidarr_id),
            json!({ "paused": true }),
        ))
        .await;
    assert_eq!(paused.status, StatusCode::OK, "{}", paused.raw);
    let noah = s.request(&s.noah, &[OK_COMPUTER]).await;
    let mia = s.request(&s.m.mia, &[OK_COMPUTER]).await;
    for (who, res) in [(&s.noah, noah), (&s.m.mia, mia)] {
        let id = res.body["request"]["id"].as_i64().unwrap();
        let res = s
            .as_member(
                who,
                post(&format!("/api/v1/requests/{id}/cancel"), json!({})),
            )
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    }
    s.admin(put(
        &format!("/api/v1/services/{}/writes-paused", s.lidarr_id),
        json!({ "paused": false }),
    ))
    .await;
    s.m.env.drain().await;
    assert_eq!(s.writes(), vec![]);
    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(acquisitions.body[0]["state"], "cancelled");
}

#[tokio::test]
async fn paused_writes_hold_albums_until_the_admin_resumes() {
    let s = setup().await;
    let held = format!("/api/v1/services/{}/held", s.lidarr_id);
    s.admin(put(
        &format!("/api/v1/services/{}/writes-paused", s.lidarr_id),
        json!({ "paused": true }),
    ))
    .await;
    let res = s.request(&s.noah, &[OK_COMPUTER, MOON_SHAPED_POOL]).await;
    assert_eq!(res.body["request"]["state"], "approved");
    s.m.env.drain().await;
    assert_eq!(s.writes(), vec![]);
    let res = s.admin(get(&held)).await;
    assert_eq!(res.body["total"], 2, "{}", res.raw);
    assert_eq!(res.body["preview"][0]["title"], "Radiohead");
    assert_eq!(res.body["preview"][0]["album"], "OK Computer");
    assert_eq!(res.body["preview"][1]["album"], "A Moon Shaped Pool");
    let journey = &s.my_requests(&s.noah).await[0]["items"][0]["journey"];
    // Members see "approved"; the hold is an admin detail.
    assert!(!journey.to_string().contains("held"), "{journey}");

    s.admin(put(
        &format!("/api/v1/services/{}/writes-paused", s.lidarr_id),
        json!({ "paused": false }),
    ))
    .await;
    s.m.env.drain().await;
    assert_eq!(s.lidarr.state().writes_to("POST", "/api/v1/album").len(), 2);
    assert_eq!(s.admin(get(&held)).await.body["total"], 0);
}

#[tokio::test]
async fn revoked_and_rejected_albums_wait_for_approval_next_time() {
    let s = setup().await;
    let res = s.request(&s.noah, &[OK_COMPUTER]).await;
    let id = res.body["request"]["id"].as_i64().unwrap();
    let res = s
        .admin(post(
            "/api/v1/admin/requests/revoke",
            json!({ "requests": [{ "id": id, "memberId": s.noah.id }], "reason": "not this one" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let request = &s.my_requests(&s.noah).await[0];
    assert_eq!(request["state"], "revoked");
    assert_eq!(request["revokeReason"], "not this one");
    // Noah requests automatically, but after a revocation an admin decides.
    let again = s.request(&s.noah, &[OK_COMPUTER]).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.raw);
    assert_eq!(again.body["request"]["state"], "pending_approval");
    // Another member's next request is not affected.
    let other = s.request(&s.m.mia, &[MOON_SHAPED_POOL]).await;
    let other_id = other.body["request"]["id"].as_i64().unwrap();
    let res = s
        .admin(post(
            "/api/v1/admin/approvals/reject",
            json!({ "requests": [{ "id": other_id, "memberId": s.m.mia.id }], "reason": "no" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(s.my_requests(&s.m.mia).await[0]["state"], "rejected");
}

#[tokio::test]
async fn album_requests_are_validated() {
    let s = setup().await;
    let cases = [
        (
            json!({ "kind": "artist", "titleId": RADIOHEAD }),
            "requests.albums_required",
        ),
        (
            json!({ "kind": "artist", "titleId": RADIOHEAD, "albums": [] }),
            "requests.albums_required",
        ),
        (
            json!({ "kind": "artist", "titleId": RADIOHEAD, "albums": ["nope"] }),
            "requests.unknown_album",
        ),
        (
            json!({ "kind": "album", "titleId": OK_COMPUTER, "albums": [OK_COMPUTER] }),
            "requests.albums_unexpected",
        ),
        (
            json!({ "kind": "artist", "titleId": RADIOHEAD, "albums": [OK_COMPUTER], "seasons": [1] }),
            "requests.seasons_unexpected",
        ),
    ];
    for (body, code) in cases {
        let res = s.as_member(&s.noah, post("/api/v1/requests", body)).await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
        assert_eq!(res.code(), code);
    }
    // At most 300 albums (a discography loads at most that many).
    let many: Vec<&str> = std::iter::repeat_n(OK_COMPUTER, 301).collect();
    let res = s.request(&s.noah, &many).await;
    assert_eq!(res.code(), "requests.albums_required", "{}", res.raw);
    // An album not by the artist (unknown to the catalog) is refused.
    let res = s
        .request(&s.noah, &["0b6b4ba0-d36f-47bd-b4ea-6a5b91842d29"])
        .await;
    assert_eq!(res.code(), "requests.unknown_album", "{}", res.raw);
    // Asking twice: the second is refused, the first stays.
    assert_eq!(
        s.request(&s.noah, &[OK_COMPUTER]).await.status,
        StatusCode::CREATED
    );
    let res = s.request(&s.noah, &[OK_COMPUTER]).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "requests.already_requested");
    // One open and one new: the open one is skipped and named.
    let res = s.request(&s.noah, &[OK_COMPUTER, MOON_SHAPED_POOL]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(album_titles(&res.body["request"]), ["A Moon Shaped Pool"]);
    assert_eq!(res.body["skipped"][0]["album"], OK_COMPUTER);
    assert_eq!(res.body["skipped"][0]["reason"], "already_requested");
    // Not entitled to music: refused.
    let movies_only = member(
        &s.m.env,
        "lea",
        json!([{ "category": "movies", "enabled": true, "requestMode": "auto" }]),
    )
    .await;
    let res = s.request(&movies_only, &[OK_COMPUTER]).await;
    assert_eq!(res.code(), "requests.not_entitled");
}

/// Lidarr's queue entry `n` of the derived `queue_details.json`, moved to
/// album `album_id` of the fake.
fn queue_entry(n: usize, album_id: i64) -> Value {
    let all: Value = serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/../adapters/lidarr/fixtures/3.1.6.5079/queue_details.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap();
    let mut entry = all[n].clone();
    entry["artistId"] = json!(ARTIST_ID);
    entry["albumId"] = json!(album_id);
    entry
}

#[tokio::test]
async fn progress_and_a_blocked_import_come_from_lidarrs_queue() {
    let s = setup().await;
    let res = s.request(&s.noah, &[OK_COMPUTER]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    s.m.env.drain().await;
    let album_id = s.lidarr.state().album(OK_COMPUTER)["id"].as_i64().unwrap();

    // Downloading: a quarter left.
    s.lidarr.state().queue = vec![queue_entry(0, album_id)];
    s.run("acquisitions.sync").await;
    let request = &s.my_requests(&s.noah).await[0];
    assert_eq!(request["state"], "in_progress", "{request}");
    assert_eq!(request["items"][0]["progress"], 0.75);

    // Finished but Lidarr does not import it: admins are told, members
    // read "stuck at import" without Lidarr's words.
    s.lidarr.state().queue = vec![queue_entry(1, album_id)];
    s.run("acquisitions.sync").await;
    let admin = s.admin(get("/api/v1/admin/notifications")).await;
    let blocked = admin.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == "import_blocked")
        .expect("import_blocked");
    assert_eq!(blocked["title"], "Radiohead – OK Computer");
    assert!(
        blocked["reason"]
            .as_str()
            .unwrap()
            .contains("not close enough")
    );
    let request = &s.my_requests(&s.noah).await[0];
    let journey = request["items"][0]["journey"].to_string();
    assert!(journey.contains("stuck_at_import"), "{journey}");
    assert!(!journey.contains("not close enough"), "{journey}");

    // Imported: available, the queue entry gone.
    {
        let mut state = s.lidarr.state();
        state.queue.clear();
        state.complete(OK_COMPUTER, 12);
    }
    s.run("acquisitions.sync").await;
    assert_eq!(s.my_requests(&s.noah).await[0]["state"], "available");
    let member = s.as_member(&s.noah, get("/api/v1/notifications")).await;
    assert!(
        member.body["items"].as_array().unwrap().iter().any(|n| {
            n["kind"] == "request_available" && n["title"] == "Radiohead – OK Computer"
        }),
        "{}",
        member.raw
    );
    assert_eq!(s.lidarr.state().forbidden, Vec::<String>::new());
}
