//! Scene requests end to end (spec 0007 slice 4): members of the private
//! category request scenes in the private area, admins approve as for
//! movies (Q10 A) after unlocking (Q9 B), the worker adds exactly the scene
//! to Whisparr (Q16 A) and follows its file, which is what "available"
//! means for a scene (Q5 A); removal stops the scene only (0004). Two
//! entitled members on every shared rule (Ada asks for approval, Dora is
//! automatic), Ben without the entitlement, Root locked and Ops unlocked.
//! `StashDB` and Whisparr are local fakes with placeholders only; nothing
//! real is contacted.
#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use axum::http::StatusCode;
use common::media::{Member, member};
use common::privacy::{Privacy, assert_clean, privacy};
use common::stashdb::{PERFORMER_P, SCENE_A};
use common::whisparr::{FakeWhisparr, SCENE_B, connect_whisparr, fake_whisparr, mount_scene};
use common::{Req, TestResponse, get, post};
use serde_json::{Value, json};
use wiremock::MockServer;

struct Scenes {
    p: Privacy,
    /// Entitled to adult with request mode `auto`.
    dora: Member,
    whisparr: FakeWhisparr,
    _server: MockServer,
}

impl Scenes {
    async fn as_member(&self, who: &Member, req: Req) -> TestResponse {
        self.p.as_member(who, req).await
    }

    async fn request(&self, who: &Member, scenes: &[&str]) -> TestResponse {
        self.as_member(
            who,
            post("/api/v1/private/requests", json!({ "scenes": scenes })),
        )
        .await
    }

    async fn mine(&self, who: &Member) -> Vec<Value> {
        let res = self
            .as_member(who, get("/api/v1/private/requests?hidden=true"))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        res.body
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"] == "scene")
            .cloned()
            .collect()
    }

    async fn run(&self, kind: &str) {
        let res = self
            .p
            .admin(post(&format!("/api/v1/jobs/{kind}/trigger"), json!({})))
            .await;
        assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
        self.p.setup.env.drain().await;
    }

    async fn approve_as(&self, cookie: &str, request: i64, member: i64) -> TestResponse {
        post(
            "/api/v1/admin/approvals/approve",
            json!({ "requests": [{ "id": request, "memberId": member }] }),
        )
        .cookie(cookie)
        .send(&self.p.setup.env.app)
        .await
    }

    async fn cancel(&self, who: &Member, id: i64) -> TestResponse {
        self.as_member(
            who,
            post(&format!("/api/v1/requests/{id}/cancel"), json!({})),
        )
        .await
    }

    fn writes(&self) -> Vec<(String, String, Value)> {
        self.whisparr.state().writes.clone()
    }

    fn assert_nothing_forbidden(&self) {
        let state = self.whisparr.state();
        assert_eq!(state.forbidden, Vec::<String>::new());
        // Only the scene itself is ever written: never a performer or a
        // studio, never another monitor mode.
        for (method, path, body) in &state.writes {
            assert!(
                path == "/api/v3/movie"
                    || path.starts_with("/api/v3/movie/")
                    || path == "/api/v3/command",
                "{method} {path}"
            );
            if path == "/api/v3/movie" {
                assert_eq!(body["addOptions"]["monitor"], "movieOnly");
            }
        }
    }
}

/// The privacy setup with Whisparr as the adult source, Scene B known to
/// the `StashDB` fake, Dora entitled with `auto`, Ada and Dora unlocked.
async fn scenes() -> Scenes {
    let p = privacy().await;
    let (whisparr, server) = fake_whisparr(&[(SCENE_A, "Scene A"), (SCENE_B, "Scene B")]).await;
    connect_whisparr(&p.setup.env, &server).await;
    mount_scene(&p.stashdb, SCENE_B, "Scene B").await;
    let dora = member(
        &p.setup.env,
        "dora",
        json!([{ "category": "adult", "enabled": true, "requestMode": "auto" }]),
    )
    .await;
    for cookie in [&p.ada.cookie, &dora.cookie] {
        assert_eq!(p.set_unlocked(cookie, true).await.status, StatusCode::OK);
    }
    assert!(whisparr.state().writes.is_empty(), "setting up only reads");
    Scenes {
        p,
        dora,
        whisparr,
        _server: server,
    }
}

#[tokio::test]
async fn a_scene_goes_from_request_to_whisparr_to_available() {
    let s = scenes().await;
    let area = s.as_member(&s.p.ada, get("/api/v1/private")).await;
    assert_eq!(area.body["requestable"], true, "{}", area.raw);
    let page = s
        .as_member(&s.p.ada, get(&format!("/api/v1/private/scenes/{SCENE_A}")))
        .await;
    assert_eq!(page.body["status"], "new", "{}", page.raw);
    assert_eq!(page.body["requestable"], true);
    assert_eq!(page.body["journey"], Value::Null);

    let res = s.request(&s.p.ada, &[SCENE_A]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let request = &res.body["requests"][0];
    assert_eq!(request["kind"], "scene");
    assert_eq!(request["titleId"], SCENE_A);
    assert_eq!(request["title"], "Scene A");
    assert_eq!(request["category"], "adult");
    assert_eq!(request["state"], "pending_approval");
    assert_eq!(request["poster"], "1a6e0000-0000-4000-8000-000000000002");
    let id = request["id"].as_i64().unwrap();
    // Waiting for an admin: nothing goes to Whisparr.
    s.p.setup.env.drain().await;
    assert_eq!(s.writes().len(), 0, "{:?}", s.writes());
    let page = s
        .as_member(&s.p.ada, get(&format!("/api/v1/private/scenes/{SCENE_A}")))
        .await;
    assert_eq!(page.body["status"], "requested");
    assert_eq!(page.body["requestedByMe"], true);
    assert!(page.body["journey"]["steps"].is_array(), "{}", page.raw);
    // The general request list never shows it (the private area's tab does).
    let general = s.as_member(&s.p.ada, get("/api/v1/requests")).await;
    assert_clean("ada: general requests", &general);
    assert_eq!(s.mine(&s.p.ada).await.len(), 1);

    // Root has not unlocked: a neutral row, approving works all the same.
    let root = s.p.setup.env.admin.clone();
    let inbox = get("/api/v1/admin/approvals")
        .cookie(&root)
        .send(&s.p.setup.env.app)
        .await;
    let row = inbox
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap()
        .clone();
    assert_eq!(row["redacted"], true);
    assert_eq!(row["kind"], "scene");
    assert_eq!(row["titleId"], Value::Null);
    assert_eq!(row["title"], "");
    assert_eq!(row["member"]["id"], s.p.ada.id);
    assert!(!inbox.raw.contains(SCENE_A), "{}", inbox.raw);
    assert_clean("root (locked): approvals", &inbox);
    // Ops unlocked sees what is approved.
    assert_eq!(
        s.p.set_unlocked(&s.p.ops, true).await.status,
        StatusCode::OK
    );
    let inbox = s.p.ops(get("/api/v1/admin/approvals")).await;
    assert!(inbox.raw.contains("Scene A"), "{}", inbox.raw);

    let res = s.approve_as(&root, id, s.p.ada.id).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    s.p.setup.env.drain().await;
    // Exactly the scene, monitored, with the category's profile and folder;
    // Whisparr's search is asked for with the add.
    let adds = s.whisparr.state().writes_to("POST", "/api/v3/movie");
    assert_eq!(adds.len(), 1, "{:?}", s.writes());
    assert_eq!(adds[0]["foreignId"], SCENE_A);
    assert_eq!(adds[0]["itemType"], "scene");
    assert_eq!(adds[0]["monitored"], true);
    assert_eq!(adds[0]["qualityProfileId"], 3);
    assert_eq!(adds[0]["rootFolderPath"], "/data/private");
    assert_eq!(adds[0]["addOptions"]["searchForMovie"], true);
    assert_eq!(s.writes().len(), 1, "{:?}", s.writes());
    s.assert_nothing_forbidden();
    assert_eq!(s.mine(&s.p.ada).await[0]["state"], "submitted");

    // Whisparr downloads it: progress, then the file. Available means
    // Whisparr has the file (the adult libraries are not asked).
    let scene_id = s.whisparr.state().scene(SCENE_A)["id"].as_i64().unwrap();
    s.whisparr.state().queue.push((
        scene_id,
        json!({ "movieId": scene_id, "size": 100.0, "sizeleft": 40.0, "status": "downloading",
                "trackedDownloadStatus": "ok", "trackedDownloadState": "downloading",
                "statusMessages": [] }),
    ));
    s.run("acquisitions.sync").await;
    let mine = s.mine(&s.p.ada).await;
    assert_eq!(mine[0]["state"], "in_progress", "{mine:?}");
    s.whisparr.state().queue.clear();
    s.whisparr.state().scene(SCENE_A)["hasFile"] = json!(true);
    s.run("acquisitions.sync").await;
    let mine = s.mine(&s.p.ada).await;
    assert_eq!(mine[0]["state"], "available", "{mine:?}");
    // No public URL on any adult library yet: nothing to open.
    assert_eq!(mine[0]["openIn"], json!([]));
    let page = s
        .as_member(&s.p.ada, get(&format!("/api/v1/private/scenes/{SCENE_A}")))
        .await;
    assert_eq!(page.body["status"], "available");
    // Following the download wrote nothing.
    assert_eq!(s.writes().len(), 1);

    // With the Emby adult library mapped (public URL set), "open in" leads
    // to that library, not to the scene (Q5 A).
    let pool = s.p.pool().clone();
    sqlx::query(
        "INSERT INTO service_instance (adapter_type, name, base_url, public_url, credential_sealed,
             credential_recipient, created_at, updated_at)
         VALUES ('emby', 'Media M', 'http://127.0.0.1:9', 'https://media.example.invalid', x'00',
             'age1x', '2026-11-01T11:00:00.000Z', '2026-11-01T11:00:00.000Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO service_role (category, kind, service_instance_id, settings, created_at, updated_at)
         SELECT 'adult', 'consume', id, '{\"libraries\":[{\"id\":\"lib9\",\"grantId\":\"g9\"}]}',
             created_at, created_at FROM service_instance WHERE name = 'Media M'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mine = s.mine(&s.p.ada).await;
    assert_eq!(
        mine[0]["openIn"],
        json!([{ "service": "emby", "name": "Media M",
                 "url": "https://media.example.invalid/web/index.html#!/item?id=lib9" }]),
        "{mine:?}"
    );
    let page = s
        .as_member(&s.p.ada, get(&format!("/api/v1/private/scenes/{SCENE_A}")))
        .await;
    assert_eq!(page.body["openIn"][0]["service"], "emby", "{}", page.raw);
    // The adult library is still no evidence: the scene stays available
    // from Whisparr's file.
    s.run("acquisitions.sync").await;
    assert_eq!(s.mine(&s.p.ada).await[0]["state"], "available");

    // Requesting it again: available, nothing new.
    let res = s.request(&s.dora, &[SCENE_A]).await;
    assert_eq!(res.code(), "requests.already_available", "{}", res.raw);
}

/// Two members on one scene (D10, 0004): Ada attaches to Dora's running
/// acquisition; one cancelling changes nothing, both cancelling sends
/// exactly one unmonitor of the scene.
#[tokio::test]
async fn shared_scenes_attach_and_the_last_cancel_unmonitors_the_scene_once() {
    let s = scenes().await;
    let res = s.request(&s.dora, &[SCENE_B]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["requests"][0]["state"], "approved");
    let dora_request = res.body["requests"][0]["id"].as_i64().unwrap();
    s.p.setup.env.drain().await;
    assert_eq!(
        s.whisparr.state().writes_to("POST", "/api/v3/movie").len(),
        1
    );
    // Ada asks for approval, but the scene is being acquired already.
    let res = s.request(&s.p.ada, &[SCENE_B]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["requests"][0]["state"], "submitted");
    let ada_request = res.body["requests"][0]["id"].as_i64().unwrap();
    s.p.setup.env.drain().await;
    assert_eq!(s.writes().len(), 1, "attaching sends nothing");

    let res = s.cancel(&s.p.ada, ada_request).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["kind"], "scene");
    s.p.setup.env.drain().await;
    assert_eq!(s.writes().len(), 1, "Dora still wants it");
    let res = s.cancel(&s.dora, dora_request).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    s.p.setup.env.drain().await;
    let puts = s.whisparr.state().writes_to("PUT", "/api/v3/movie/");
    assert_eq!(puts.len(), 1, "{:?}", s.writes());
    assert_eq!(puts[0]["monitored"], false);
    assert_eq!(puts[0]["foreignId"], SCENE_B);
    assert_eq!(
        s.whisparr.state().scene(SCENE_B)["monitored"],
        false,
        "only the scene, which Starr added"
    );
    s.assert_nothing_forbidden();
}

/// Scenes already in Whisparr (C5): with a file → available with zero
/// writes; unmonitored without a file → exactly one monitor write (plus
/// Whisparr's search), read back.
#[tokio::test]
async fn scenes_already_in_whisparr_are_joined_with_the_fewest_writes() {
    let s = scenes().await;
    s.whisparr
        .state()
        .has_scene(SCENE_A, "Scene A", false, true);
    s.whisparr
        .state()
        .has_scene(SCENE_B, "Scene B", false, false);
    // Monitored by someone else, no file: only Whisparr's search.
    let c = "5ce0e000-0000-4000-8000-00000000000c";
    mount_scene(&s.p.stashdb, c, "Scene C").await;
    s.whisparr.state().has_scene(c, "Scene C", true, false);
    let res = s.request(&s.dora, &[SCENE_A, SCENE_B, c]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["requests"].as_array().unwrap().len(), 3);
    s.p.setup.env.drain().await;
    let writes = s.writes();
    let puts: Vec<&Value> = writes
        .iter()
        .filter(|(m, _, _)| m == "PUT")
        .map(|(_, _, b)| b)
        .collect();
    assert_eq!(puts.len(), 1, "{writes:?}");
    assert_eq!(puts[0]["foreignId"], SCENE_B);
    assert_eq!(puts[0]["monitored"], true);
    let commands = s.whisparr.state().writes_to("POST", "/api/v3/command");
    assert_eq!(commands.len(), 2, "{writes:?}");
    assert!(commands.iter().all(|c| c["name"] == "MoviesSearch"));
    assert_eq!(
        s.whisparr.state().writes_to("POST", "/api/v3/movie").len(),
        0
    );
    let mine = s.mine(&s.dora).await;
    let state_of = |title: &str| {
        mine.iter()
            .find(|r| r["title"] == title)
            .map(|r| r["state"].clone())
            .unwrap()
    };
    assert_eq!(state_of("Scene A"), "available");
    assert_eq!(state_of("Scene B"), "submitted");
    s.assert_nothing_forbidden();

    // Dora cancels the joined scene with its file: nothing in Whisparr.
    let a = mine.iter().find(|r| r["title"] == "Scene A").unwrap()["id"]
        .as_i64()
        .unwrap();
    let res = s
        .as_member(
            &s.dora,
            post(&format!("/api/v1/requests/{a}/cancel"), json!({})),
        )
        .await;
    assert_eq!(res.code(), "requests.not_cancellable", "{}", res.raw);
    // Revoked by an admin: Starr turned on nothing for it, so nothing stops.
    let res =
        s.p.admin(post(
            "/api/v1/admin/requests/revoke",
            json!({ "requests": [{ "id": a, "memberId": s.dora.id }], "reason": "Reason R" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    s.p.setup.env.drain().await;
    assert_eq!(s.writes().len(), 3, "{:?}", s.writes());
}

/// Q10 A: a revoked or rejected scene waits for an admin the next time,
/// even for an automatic member; performers and studios are never
/// requestable; a performer page requests several scenes at once, one
/// request each, skipping what the member asked for already.
#[tokio::test]
async fn refusals_wait_for_an_admin_and_a_performer_page_requests_several_scenes() {
    let s = scenes().await;
    let res = s.request(&s.dora, &[SCENE_A]).await;
    let id = res.body["requests"][0]["id"].as_i64().unwrap();
    s.p.setup.env.drain().await;
    let res =
        s.p.admin(post(
            "/api/v1/admin/requests/revoke",
            json!({ "requests": [{ "id": id, "memberId": s.dora.id }], "reason": "Reason R" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    s.p.setup.env.drain().await;
    // Starr added it, so the revocation unmonitors it.
    let puts = s.whisparr.state().writes_to("PUT", "/api/v3/movie/");
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0]["monitored"], false);

    // The performer page: both scenes are requestable, the performer not.
    let page = s
        .as_member(
            &s.dora,
            get(&format!("/api/v1/private/performers/{PERFORMER_P}")),
        )
        .await;
    assert_eq!(page.body["requestable"], true, "{}", page.raw);
    // (StashDB knows no scene by a performer's id.)
    let missing = common::stashdb::fixture("0.10.3/recorded/find_scene_unknown.json");
    wiremock::Mock::given(wiremock::matchers::body_string_contains(PERFORMER_P))
        .and(wiremock::matchers::body_string_contains("query FindScene"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(missing))
        .with_priority(1)
        .mount(&s.p.stashdb)
        .await;
    let res = s.request(&s.dora, &[PERFORMER_P]).await;
    assert_eq!(res.code(), "requests.unknown_scene", "{}", res.raw);
    let res = s.request(&s.dora, &[SCENE_A, SCENE_B, SCENE_B]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let made = res.body["requests"].as_array().unwrap();
    assert_eq!(made.len(), 2, "one request per scene: {}", res.raw);
    let state_of = |title: &str| {
        made.iter()
            .find(|r| r["title"] == title)
            .map(|r| r["state"].clone())
            .unwrap()
    };
    assert_eq!(
        state_of("Scene A"),
        "pending_approval",
        "after a revocation"
    );
    assert_eq!(state_of("Scene B"), "approved");
    // Again: both open already, nothing left.
    let res = s.request(&s.dora, &[SCENE_A, SCENE_B]).await;
    assert_eq!(res.code(), "requests.already_requested", "{}", res.raw);
    let res = s
        .as_member(
            &s.dora,
            get(&format!("/api/v1/private/performers/{PERFORMER_P}")),
        )
        .await;
    let scenes = res.body["scenes"].as_array().unwrap();
    let a = scenes.iter().find(|c| c["id"] == SCENE_A).unwrap();
    assert_eq!(a["status"], "requested");
    assert_eq!(a["requestedByMe"], true);
    // Bounds of one call.
    let res = s.request(&s.dora, &[]).await;
    assert_eq!(res.code(), "requests.scenes_required");
    let many: Vec<String> = (0..21)
        .map(|n| format!("5ce0e000-0000-4000-8000-{n:012x}"))
        .collect();
    let res = s
        .as_member(
            &s.dora,
            post("/api/v1/private/requests", json!({ "scenes": many })),
        )
        .await;
    assert_eq!(res.code(), "requests.scenes_required");
    let res = s
        .request(&s.dora, &["5CE0E000-0000-4000-8000-00000000000A"])
        .await;
    assert_eq!(res.code(), "requests.unknown_scene");
}

/// Spec 0004 R4 for scenes (Q5 A): Whisparr's file is the only evidence;
/// no file on two checks six hours apart removes the scene, which can be
/// requested again.
#[tokio::test]
async fn a_scene_whose_file_is_gone_twice_is_removed_and_requestable_again() {
    let s = scenes().await;
    s.whisparr.state().has_scene(SCENE_A, "Scene A", true, true);
    let res = s.request(&s.dora, &[SCENE_A]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    s.p.setup.env.drain().await;
    assert_eq!(s.mine(&s.dora).await[0]["state"], "available");
    let backdate = async || {
        sqlx::query("UPDATE acquisition SET synced_at = '2026-01-01T00:00:00.000Z' WHERE category = 'adult' AND state = 'available'")
            .execute(s.p.pool())
            .await
            .unwrap();
    };
    // Checked again after six hours: the file is still there.
    backdate().await;
    s.run("acquisitions.sync").await;
    assert_eq!(s.mine(&s.dora).await[0]["state"], "available");
    // Whisparr answering 404 for its library (a wrong address) is no
    // evidence: reported, nothing recorded, however often.
    s.whisparr.state().lost = true;
    for _ in 0..3 {
        backdate().await;
        s.run("acquisitions.sync").await;
    }
    assert_eq!(s.mine(&s.dora).await[0]["state"], "available");
    let (missing,): (Option<String>,) = sqlx::query_as(
        "SELECT file_missing_since FROM acquisition WHERE category = 'adult' AND state = 'available'",
    )
    .fetch_one(s.p.pool())
    .await
    .unwrap();
    assert_eq!(missing, None);
    s.whisparr.state().lost = false;
    // Gone once: still available.
    s.whisparr.state().scene(SCENE_A)["hasFile"] = json!(false);
    backdate().await;
    s.run("acquisitions.sync").await;
    assert_eq!(s.mine(&s.dora).await[0]["state"], "available");
    // Gone again on the next check: removed.
    backdate().await;
    s.run("acquisitions.sync").await;
    assert_eq!(s.mine(&s.dora).await[0]["state"], "removed");
    let page = s
        .as_member(&s.dora, get(&format!("/api/v1/private/scenes/{SCENE_A}")))
        .await;
    assert_eq!(page.body["status"], "new", "{}", page.raw);
    assert!(s.writes().is_empty(), "removal writes nothing");
}

/// While the private category is off (spec 0007 "What switching off
/// does"), approved scenes are held like paused writes; switched on again
/// with the statement, they go out.
#[tokio::test]
async fn writes_wait_while_the_private_category_is_off() {
    let s = scenes().await;
    let res = s.request(&s.p.ada, &[SCENE_A]).await;
    let id = res.body["requests"][0]["id"].as_i64().unwrap();
    assert_eq!(s.p.switch_adult(false).await.status, StatusCode::OK);
    let res = s
        .approve_as(&s.p.setup.env.admin.clone(), id, s.p.ada.id)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    s.p.setup.env.drain().await;
    s.run("requests.submit").await;
    assert!(s.writes().is_empty(), "{:?}", s.writes());
    assert_eq!(s.p.switch_adult(true).await.status, StatusCode::OK);
    s.run("requests.submit").await;
    assert_eq!(
        s.whisparr.state().writes_to("POST", "/api/v3/movie").len(),
        1
    );
    // A stop waits as well: revoked while off, sent once it is on again.
    assert_eq!(s.p.switch_adult(false).await.status, StatusCode::OK);
    let res =
        s.p.admin(post(
            "/api/v1/admin/requests/revoke",
            json!({ "requests": [{ "id": id, "memberId": s.p.ada.id }], "reason": "Reason R" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    s.run("requests.submit").await;
    assert_eq!(
        s.whisparr.state().writes_to("PUT", "/api/v3/movie/").len(),
        0
    );
    assert_eq!(s.p.switch_adult(true).await.status, StatusCode::OK);
    s.run("requests.submit").await;
    let puts = s.whisparr.state().writes_to("PUT", "/api/v3/movie/");
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0]["monitored"], false);
}

/// The general routes never take a scene, for anyone; the private ones
/// only through the gate (404 as if they did not exist).
#[tokio::test]
async fn scene_requests_exist_only_behind_the_gate() {
    let s = scenes().await;
    let asked = common::stashdb::graphql_requests(&s.p.stashdb).await;
    for who in [&s.p.ben, &s.p.ada, &s.dora] {
        let res = s
            .as_member(
                who,
                post(
                    "/api/v1/requests",
                    json!({ "kind": "scene", "titleId": SCENE_A }),
                ),
            )
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
        assert_eq!(res.code(), "catalog.not_found");
        let res = s
            .as_member(
                who,
                get(&format!("/api/v1/requests/status?kind=scene&ids={SCENE_A}")),
            )
            .await;
        assert_eq!(res.code(), "catalog.not_found", "{}", res.raw);
        let res = s
            .as_member(who, get(&format!("/api/v1/titles/scene/{SCENE_A}")))
            .await;
        assert_eq!(res.code(), "catalog.not_found", "{}", res.raw);
    }
    assert_eq!(common::stashdb::graphql_requests(&s.p.stashdb).await, asked);
    // Ben (not entitled), Ada locked, and everyone while adult is off.
    let res = s.request(&s.p.ben, &[SCENE_A]).await;
    assert_eq!(res.code(), "private.not_found", "{}", res.raw);
    s.p.set_unlocked(&s.p.ada.cookie, false).await;
    let res = s.request(&s.p.ada, &[SCENE_A]).await;
    assert_eq!(res.code(), "private.not_found", "{}", res.raw);
    let res = s.as_member(&s.p.ada, get("/api/v1/private/requests")).await;
    assert_eq!(res.code(), "private.not_found", "{}", res.raw);
    assert_eq!(s.p.switch_adult(false).await.status, StatusCode::OK);
    let res = s.request(&s.dora, &[SCENE_A]).await;
    assert_eq!(res.code(), "private.not_found", "{}", res.raw);
    assert_eq!(common::stashdb::graphql_requests(&s.p.stashdb).await, asked);
    assert_eq!(s.writes().len(), 0, "{:?}", s.writes());
    // Admins have no member route here.
    let res =
        s.p.ops(post(
            "/api/v1/private/requests",
            json!({ "scenes": [SCENE_A] }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
}
