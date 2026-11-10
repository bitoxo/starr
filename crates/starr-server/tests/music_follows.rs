//! Artist follows end to end (spec 0006 slice 6): members follow an
//! artist (from the artist page, or while requesting albums), and
//! `follows.check` requests every album of the category's release types
//! first released after the follow date in the member's name and mode
//! (Q3 B, Q10 A). Two members on every rule (lesson 4). `MusicBrainz` and
//! Lidarr are local fakes built from the recorded and hand-written
//! fixtures; nothing real is contacted.
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;
use common::lidarr::{FakeLidarr, MOON_SHAPED_POOL, OK_COMPUTER, RADIOHEAD, fake_lidarr};
use common::media::{Member, member, put};
use common::music::{Music, music};
use common::services::{API_KEY, delete};
use common::{TestResponse, get, post};
use serde_json::{Value, json};
use wiremock::matchers::{path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LIVE: &str = "1f0b6c3a-2e4d-4a5b-8c9d-0e1f2a3b4c5d";
const SINGLE: &str = "d1e2f3a4-b5c6-4d7e-8f9a-0b1c2d3e4f5a";
const COMPILATION: &str = "f0e1d2c3-b4a5-4f6e-8d7c-6b5a4f3e2d1c";

/// Album and EP without secondary types (spec 0006 Q3 B, the default).
fn default_types() -> Value {
    json!({
        "releaseTypes": ["album", "ep"],
        "excludedSecondaryTypes": [
            "compilation", "soundtrack", "spokenword", "interview", "audiobook",
            "audio_drama", "live", "remix", "dj_mix", "mixtape", "demo", "field_recording",
        ],
    })
}

struct Setup {
    m: Music,
    noah: Member,
    lidarr: FakeLidarr,
    _server: MockServer,
}

impl Setup {
    async fn as_member(&self, who: &Member, req: common::Req) -> TestResponse {
        req.cookie(&who.cookie).send(&self.m.env.app).await
    }

    async fn follow(&self, who: &Member) -> TestResponse {
        self.as_member(
            who,
            post(
                "/api/v1/follows",
                json!({ "kind": "artist", "titleId": RADIOHEAD }),
            ),
        )
        .await
    }

    async fn follows(&self, who: &Member) -> Value {
        let res = self.as_member(who, get("/api/v1/follows")).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        res.body
    }

    async fn requests(&self, who: &Member) -> Vec<Value> {
        let res = self.as_member(who, get("/api/v1/requests")).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        res.body.as_array().unwrap().clone()
    }

    /// Run `follows.check` (and what it leaves behind) and return its
    /// report.
    async fn check(&self) -> Value {
        let res = self
            .m
            .admin(post("/api/v1/jobs/follows.check/trigger", json!({})))
            .await;
        assert!(res.status.is_success(), "{}", res.raw);
        self.m.env.drain().await;
        let (state, report): (String, Option<String>) = sqlx::query_as(
            "SELECT state, report FROM run WHERE kind = 'follows.check' ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(self.m.env.app.state.db.pool())
        .await
        .unwrap();
        assert_eq!(state, "succeeded");
        serde_json::from_str(&report.unwrap()).unwrap()
    }

    /// A day later: every follow is due again and the catalog is read
    /// again.
    async fn a_day_later(&self) {
        let db = self.m.env.app.state.db.pool();
        sqlx::query("DELETE FROM catalog_cache")
            .execute(db)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE follow SET checked_at = strftime('%Y-%m-%dT%H:%M:%fZ', checked_at, '-1 day')
             WHERE checked_at IS NOT NULL",
        )
        .execute(db)
        .await
        .unwrap();
    }

    /// From now on `MusicBrainz` lists Radiohead's discography as OK
    /// Computer (1997) and, announced for 2099, a new album, a live album,
    /// a single and a compilation, plus an EP without a date.
    async fn announced(&self) {
        let rg = |id: &str, title: &str, date: Option<&str>, primary: &str, secondary: &[&str]| {
            json!({
                "id": id, "title": title, "first-release-date": date,
                "primary-type": primary, "secondary-types": secondary, "disambiguation": "",
            })
        };
        let groups = [
            rg(OK_COMPUTER, "OK Computer", Some("1997-05-21"), "Album", &[]),
            rg(
                MOON_SHAPED_POOL,
                "Next Album",
                Some("2099-03-01"),
                "Album",
                &[],
            ),
            rg(
                LIVE,
                "Live at the Astoria",
                Some("2099-02"),
                "Album",
                &["Live"],
            ),
            rg(SINGLE, "Daydreaming", Some("2099-01-15"), "Single", &[]),
            rg(
                COMPILATION,
                "Best Of",
                Some("2099"),
                "Album",
                &["Compilation"],
            ),
            rg(
                "e1f2a3b4-c5d6-4e7f-8a9b-0c1d2e3f4a5b",
                "Undated EP",
                None,
                "EP",
                &[],
            ),
        ];
        Mock::given(path("/ws/2/release-group"))
            .and(query_param("artist", RADIOHEAD))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "release-group-count": groups.len(),
                "release-group-offset": 0,
                "release-groups": groups,
            })))
            .with_priority(1)
            .mount(&self.m.mb)
            .await;
        self.a_day_later().await;
    }

    /// How many requests the `MusicBrainz` fake has answered so far.
    async fn musicbrainz_requests(&self) -> usize {
        self.m.mb.received_requests().await.unwrap().len()
    }
}

/// `MusicBrainz` as the music catalog with `relevance`, Lidarr (fake) as the
/// source. Mia requests music with approval, Noah automatically.
async fn setup_with(relevance: Value) -> Setup {
    let m = music(Some(relevance)).await;
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
        _server: server,
    }
}

async fn setup() -> Setup {
    setup_with(default_types()).await
}

fn album_ids(request: &Value) -> Vec<&str> {
    request["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["album"]["id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn two_members_follow_an_artist_each_on_their_own() {
    let s = setup().await;
    let today = s.m.env.app.clock.now().date_naive().to_string();
    // Mia follows from the artist page without requesting.
    let mia = s.follow(&s.m.mia).await;
    assert_eq!(mia.status, StatusCode::CREATED, "{}", mia.raw);
    assert_eq!(mia.body["kind"], "artist");
    assert_eq!(mia.body["titleId"], RADIOHEAD);
    assert_eq!(mia.body["title"], "Radiohead");
    assert_eq!(mia.body["category"], "music");
    assert_eq!(mia.body["knownThrough"], today.as_str());
    assert!(mia.body["seasonsThrough"].is_null(), "{}", mia.raw);
    let again = s.follow(&s.m.mia).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.raw);
    assert_eq!(again.body["id"], mia.body["id"]);

    // Noah follows while requesting OK Computer.
    let res = s
        .as_member(
            &s.noah,
            post(
                "/api/v1/requests",
                json!({ "kind": "artist", "titleId": RADIOHEAD, "albums": [OK_COMPUTER], "follow": true }),
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["request"]["follow"], true);
    let noah = s.follows(&s.noah).await;
    assert_eq!(noah.as_array().unwrap().len(), 1, "{noah}");
    assert_ne!(noah[0]["id"], mia.body["id"]);
    assert_eq!(noah[0]["knownThrough"], today.as_str());

    // Each sees only their own; nobody ends another member's follow.
    let mine = s.follows(&s.m.mia).await;
    assert_eq!(mine.as_array().unwrap().len(), 1);
    assert!(!mine.to_string().contains("noah"), "{mine}");
    let mia_follow = mine[0]["id"].as_i64().unwrap();
    let res = s
        .as_member(&s.noah, delete(&format!("/api/v1/follows/{mia_follow}")))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
    let res = s
        .as_member(&s.m.mia, delete(&format!("/api/v1/follows/{mia_follow}")))
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    assert_eq!(s.follows(&s.m.mia).await, json!([]));
    assert_eq!(s.follows(&s.noah).await.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn asking_with_follow_follows_the_artist_even_when_nothing_is_left_to_request() {
    let s = setup().await;
    let body = json!({ "kind": "album", "titleId": OK_COMPUTER });
    let res = s.as_member(&s.m.mia, post("/api/v1/requests", body)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    // From the album page again, now with "follow the artist".
    let body = json!({ "kind": "album", "titleId": OK_COMPUTER, "follow": true });
    let res = s.as_member(&s.m.mia, post("/api/v1/requests", body)).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "requests.already_requested");
    let follows = s.follows(&s.m.mia).await;
    assert_eq!(follows.as_array().unwrap().len(), 1, "{follows}");
    assert_eq!(follows[0]["titleId"], RADIOHEAD);
    assert_eq!(s.follows(&s.noah).await, json!([]));
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one story: follow, new album, once
async fn a_new_album_becomes_a_request_in_each_members_name_once() {
    let s = setup().await;
    assert_eq!(s.follow(&s.m.mia).await.status, StatusCode::CREATED);
    assert_eq!(s.follow(&s.noah).await.status, StatusCode::CREATED);
    s.announced().await;

    // A new album, a live album, a single and a compilation after the
    // follow date: exactly the album, for each member (Q3 B); nothing
    // from before the follow date, nothing without a date.
    let report = s.check().await;
    assert_eq!(report["expectation"]["status"], "met", "{report}");
    assert_eq!(
        report["expectation"]["detail"],
        "checked 2 follows, 0 new seasons, 2 new albums, 0 new scenes, 2 requests created, 0 scenes over the weekly cap",
        "{report}"
    );
    assert_eq!(report["counters"]["albums_requested"], 2);

    // In each member's name with their request mode: Noah's is approved
    // and goes to Lidarr; Mia's joins the running acquisition (D10).
    let noah = s.requests(&s.noah).await;
    assert_eq!(noah.len(), 1, "{noah:?}");
    assert_eq!(album_ids(&noah[0]), [MOON_SHAPED_POOL]);
    assert_eq!(noah[0]["titleId"], RADIOHEAD);
    assert_eq!(noah[0]["follow"], true);
    assert_ne!(noah[0]["state"], "pending_approval", "{noah:?}");
    assert_eq!(
        noah[0]["items"][0]["journey"]["steps"][0]["note"],
        "by_follow"
    );
    let mia = s.requests(&s.m.mia).await;
    assert_eq!(mia.len(), 1, "{mia:?}");
    assert_eq!(album_ids(&mia[0]), [MOON_SHAPED_POOL]);
    let added: Vec<String> = s
        .lidarr
        .state()
        .writes
        .iter()
        .filter(|(method, path, _)| method == "POST" && path == "/api/v1/album")
        .map(|(_, _, body)| body["foreignAlbumId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(added, [MOON_SHAPED_POOL], "one add, for both members");

    let (actor, detail): (String, Option<String>) = sqlx::query_as(
        "SELECT trigger || ':' || actor, detail FROM request_event WHERE request_id = ?
         ORDER BY id LIMIT 1",
    )
    .bind(noah[0]["id"].as_i64().unwrap())
    .fetch_one(s.m.env.app.state.db.pool())
    .await
    .unwrap();
    assert_eq!(actor, "system:follows.check");
    assert!(
        detail
            .as_deref()
            .unwrap()
            .starts_with("new album of a followed artist"),
        "{detail:?}"
    );

    // Both are told (follow_requested), with "Artist – Album".
    for who in [&s.m.mia, &s.noah] {
        let res = s.as_member(who, get("/api/v1/notifications")).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        let found = res.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["kind"] == "follow_requested")
            .unwrap_or_else(|| panic!("{}", res.raw))
            .clone();
        assert_eq!(found["title"], "Radiohead – Next Album", "{found}");
    }

    // Both are noted as checked when the run started, however far into
    // the run each came, so each is due again exactly a day later.
    let (stamps,): (i64,) = sqlx::query_as(
        "SELECT count(DISTINCT checked_at) FROM follow WHERE checked_at IS NOT NULL",
    )
    .fetch_one(s.m.env.app.state.db.pool())
    .await
    .unwrap();
    assert_eq!(stamps, 1);

    // Checked today: not again until a day has passed (spread, Q14).
    let report = s.check().await;
    assert_eq!(report["total"], 0, "{report}");

    // The next day nothing is asked again; the follow date stays.
    s.a_day_later().await;
    let report = s.check().await;
    assert_eq!(
        report["expectation"]["detail"],
        "checked 2 follows, 0 new seasons, 0 new albums, 0 new scenes, 0 requests created, 0 scenes over the weekly cap",
        "{report}"
    );
    assert_eq!(s.requests(&s.noah).await.len(), 1);
    assert_eq!(s.requests(&s.m.mia).await.len(), 1);
    let today = s.m.env.app.clock.now().date_naive().to_string();
    assert_eq!(s.follows(&s.m.mia).await[0]["knownThrough"], today.as_str());
}

#[tokio::test]
async fn a_withdrawn_album_is_not_asked_for_again() {
    let s = setup().await;
    assert_eq!(s.follow(&s.m.mia).await.status, StatusCode::CREATED);
    s.announced().await;
    s.check().await;
    let mia = s.requests(&s.m.mia).await;
    assert_eq!(mia[0]["state"], "pending_approval", "{mia:?}");
    let id = mia[0]["id"].as_i64().unwrap();
    let res = s
        .as_member(
            &s.m.mia,
            post(&format!("/api/v1/requests/{id}/withdraw"), json!({})),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);

    s.a_day_later().await;
    let report = s.check().await;
    assert_eq!(report["counters"]["requests_created"], 0, "{report}");
    // Noah follows now: the album is new for him (his follow, his date).
    assert_eq!(s.follow(&s.noah).await.status, StatusCode::CREATED);
    let report = s.check().await;
    assert_eq!(report["counters"]["requests_created"], 1, "{report}");
    assert_eq!(album_ids(&s.requests(&s.noah).await[0]), [MOON_SHAPED_POOL]);
    assert_eq!(s.lidarr.state().forbidden, Vec::<String>::new());
}

#[tokio::test]
async fn the_categorys_release_types_decide_what_counts() {
    // The admin also wants singles (one per-category setting, Q3 B).
    let mut relevance = default_types();
    relevance["releaseTypes"] = json!(["album", "ep", "single"]);
    let s = setup_with(relevance).await;
    assert_eq!(s.follow(&s.m.mia).await.status, StatusCode::CREATED);
    s.announced().await;
    let report = s.check().await;
    assert_eq!(report["counters"]["albums_requested"], 2, "{report}");
    let mia = s.requests(&s.m.mia).await;
    assert_eq!(mia.len(), 1, "one request with both");
    let mut ids = album_ids(&mia[0]);
    ids.sort_unstable();
    let mut expected = vec![MOON_SHAPED_POOL, SINGLE];
    expected.sort_unstable();
    assert_eq!(ids, expected);
}

#[tokio::test]
async fn members_without_music_are_skipped_and_keep_their_follow_date() {
    let s = setup().await;
    assert_eq!(s.follow(&s.m.mia).await.status, StatusCode::CREATED);
    assert_eq!(s.follow(&s.noah).await.status, StatusCode::CREATED);
    let res =
        s.m.admin(put(
            &format!("/api/v1/members/{}/entitlements", s.noah.id),
            json!({ "entitlements": [
                { "category": "music", "enabled": false, "requestMode": "auto" },
            ] }),
        ))
        .await;
    assert!(res.status.is_success(), "{}", res.raw);
    s.announced().await;
    let report = s.check().await;
    assert_eq!(report["skipped"], 1, "{report}");
    assert_eq!(report["counters"]["requests_created"], 1, "{report}");
    assert_eq!(s.requests(&s.noah).await.len(), 0);

    // Entitled again: the album that came meanwhile is asked for.
    let res =
        s.m.admin(put(
            &format!("/api/v1/members/{}/entitlements", s.noah.id),
            json!({ "entitlements": [
                { "category": "music", "enabled": true, "requestMode": "approval" },
            ] }),
        ))
        .await;
    assert!(res.status.is_success(), "{}", res.raw);
    s.a_day_later().await;
    s.check().await;
    let noah = s.requests(&s.noah).await;
    assert_eq!(noah.len(), 1, "{noah:?}");
    assert_eq!(album_ids(&noah[0]), [MOON_SHAPED_POOL]);
}

#[tokio::test]
async fn only_series_and_artists_of_an_entitled_member_can_be_followed() {
    let s = setup().await;
    let res = s
        .as_member(
            &s.m.mia,
            post(
                "/api/v1/follows",
                json!({ "kind": "album", "titleId": OK_COMPUTER }),
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "follows.not_followable");

    let lea = member(
        &s.m.env,
        "lea",
        json!([{ "category": "movies", "enabled": true, "requestMode": "auto" }]),
    )
    .await;
    let res = s.follow(&lea).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "requests.not_entitled");

    let res = s
        .as_member(
            &s.m.mia,
            post(
                "/api/v1/follows",
                json!({ "kind": "artist", "titleId": "not-an-mbid" }),
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
}

/// The cost of artist follows towards `MusicBrainz` per daily check (spec
/// 0006 Q14): one discography page per 100 release groups (cached for a
/// day, shared by every follower and the artist page) and the artist
/// itself at most weekly (cached for seven days).
#[tokio::test]
async fn a_daily_check_reads_the_discography_once_for_every_follower() {
    let s = setup().await;
    let at_start = s.musicbrainz_requests().await;
    assert_eq!(s.follow(&s.m.mia).await.status, StatusCode::CREATED);
    assert_eq!(s.follow(&s.noah).await.status, StatusCode::CREATED);
    // Following read the artist and its discography (103 release groups:
    // two pages), once for both.
    let followed = s.musicbrainz_requests().await;
    assert_eq!(followed - at_start, 3, "artist and two discography pages");
    // The first check right after reads from the cache.
    let report = s.check().await;
    assert_eq!(report["total"], 2, "{report}");
    assert_eq!(s.musicbrainz_requests().await, followed);

    // A day later the discography has expired, the artist has not: two
    // pages, once for both followers.
    sqlx::query("DELETE FROM catalog_cache WHERE request LIKE '%release-group%'")
        .execute(s.m.env.app.state.db.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE follow SET checked_at = strftime('%Y-%m-%dT%H:%M:%fZ', checked_at, '-1 day')",
    )
    .execute(s.m.env.app.state.db.pool())
    .await
    .unwrap();
    let report = s.check().await;
    assert_eq!(report["total"], 2, "{report}");
    assert_eq!(s.musicbrainz_requests().await - followed, 2);
}
