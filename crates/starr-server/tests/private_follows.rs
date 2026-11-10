//! Performer and studio follows end to end (spec 0007 slice 5, Q4): an
//! entitled member follows a performer or a studio in the private area,
//! `follows.check` requests the scenes released after the follow date in
//! the member's name and mode, oldest first and at most the weekly cap per
//! follow; the rest show on the follow and notify `follow_capped` once a
//! week. Two members on every rule (Ada asks for approval, Dora is
//! automatic, Eve asks for approval and follows the studio), Ben without
//! the entitlement. `StashDB` and Whisparr are local fakes with
//! placeholders only; nothing real is contacted.
#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::media::{Member, member, put};
use common::privacy::{Privacy, assert_clean, privacy};
use common::stashdb::{ListedScene, PERFORMER_P, STUDIO_S, mount_entity_scenes};
use common::whisparr::{FakeWhisparr, connect_whisparr, fake_whisparr, mount_scene};
use common::{Req, TestResponse, get, post};
use serde_json::{Value, json};
use wiremock::MockServer;

/// Released after the follow date (2026-11-01).
const NEW: &str = "5ce0e000-0000-4000-8000-000000000101";
const OLD: &str = "5ce0e000-0000-4000-8000-000000000102";
const SAME_DAY: &str = "5ce0e000-0000-4000-8000-000000000103";
const UNDATED: &str = "5ce0e000-0000-4000-8000-000000000104";

struct Follows {
    p: Privacy,
    /// Entitled to adult with request mode `auto`.
    dora: Member,
    /// Entitled to adult with request mode `approval`.
    eve: Member,
    whisparr: FakeWhisparr,
    _server: MockServer,
}

impl Follows {
    async fn as_member(&self, who: &Member, req: Req) -> TestResponse {
        self.p.as_member(who, req).await
    }

    async fn follow(&self, who: &Member, kind: &str, id: &str) -> TestResponse {
        self.as_member(
            who,
            post("/api/v1/private/follows", json!({ "kind": kind, "id": id })),
        )
        .await
    }

    /// `follows.check` as the scheduler runs it, then whatever it queued.
    async fn check(&self) {
        // Every follow is due again (a day later, as the next run).
        sqlx::query(
            "UPDATE follow SET checked_at = strftime('%Y-%m-%dT%H:%M:%fZ', checked_at, '-1 day')
             WHERE checked_at IS NOT NULL",
        )
        .execute(self.p.pool())
        .await
        .unwrap();
        let res = self
            .p
            .admin(post("/api/v1/jobs/follows.check/trigger", json!({})))
            .await;
        assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
        self.p.setup.env.drain().await;
    }

    /// The member's scene requests, newest first (without the fixture's
    /// seeded private movie request).
    async fn requests(&self, who: &Member) -> Vec<Value> {
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

    async fn requested(&self, who: &Member) -> Vec<String> {
        let mut ids: Vec<String> = self
            .requests(who)
            .await
            .iter()
            .map(|r| r["titleId"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        ids
    }

    async fn my_follows(&self, who: &Member) -> Vec<Value> {
        let res = self.as_member(who, get("/api/v1/private/follows")).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        res.body.as_array().unwrap().clone()
    }

    async fn notifications(&self, who: &Member, kind: &str) -> Vec<Value> {
        let res = self.as_member(who, get("/api/v1/notifications")).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        res.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["kind"] == kind)
            .cloned()
            .collect()
    }

    fn assert_only_scene_writes(&self) {
        let state = self.whisparr.state();
        assert_eq!(state.forbidden, Vec::<String>::new());
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

/// The privacy setup (Ada already follows Performer P) with Whisparr as
/// the adult source, the worker reading `StashDB` like the web side, Dora
/// (`auto`) and Eve (`approval`) entitled, and Ada, Dora and Eve unlocked.
async fn follows(whisparr_knows: &[(&str, &str)]) -> Follows {
    let mut p = privacy().await;
    let access = Arc::clone(&p.setup.env.app.state.catalog);
    p.setup.env.worker_catalog(access);
    let (whisparr, server) = fake_whisparr(whisparr_knows).await;
    connect_whisparr(&p.setup.env, &server).await;
    let dora = member(
        &p.setup.env,
        "dora",
        json!([{ "category": "adult", "enabled": true, "requestMode": "auto" }]),
    )
    .await;
    let eve = member(
        &p.setup.env,
        "eve",
        json!([{ "category": "adult", "enabled": true, "requestMode": "approval" }]),
    )
    .await;
    for cookie in [&p.ada.cookie, &dora.cookie, &eve.cookie] {
        assert_eq!(p.set_unlocked(cookie, true).await.status, StatusCode::OK);
    }
    Follows {
        p,
        dora,
        eve,
        whisparr,
        _server: server,
    }
}

#[tokio::test]
async fn a_followed_performers_new_scene_is_requested_once_in_the_members_mode() {
    let f = follows(&[(NEW, "Scene New")]).await;
    // Spec 0007 acceptance: one new scene, and scenes from before or on
    // the follow day, or without a date.
    mount_entity_scenes(
        &f.p.stashdb,
        &[
            (OLD, "Scene Old", Some("2026-10-30")),
            (SAME_DAY, "Scene Same Day", Some("2026-11-01")),
            (NEW, "Scene New", Some("2026-11-05")),
            (UNDATED, "Scene Undated", None),
        ],
    )
    .await;

    // Ada follows already (the fixture): following again returns it.
    let res = f.follow(&f.p.ada, "performer", PERFORMER_P).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["id"], f.p.ada_follow);
    // Ada asks for approval: her request waits for an admin.
    f.check().await;
    assert_eq!(f.requested(&f.p.ada).await, [NEW]);
    let ada = &f.requests(&f.p.ada).await[0];
    assert_eq!(ada["state"], "pending_approval", "{ada}");
    assert_eq!(ada["follow"], true);
    assert!(f.whisparr.state().writes.is_empty(), "nothing approved yet");

    // Dora starts following today, from the page.
    let res = f.follow(&f.dora, "performer", PERFORMER_P).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let dora_follow = res.body["id"].as_i64().unwrap();
    assert_eq!(res.body["kind"], "performer");
    assert_eq!(res.body["entityId"], PERFORMER_P);
    assert_eq!(res.body["name"], "Performer P");
    assert_eq!(res.body["knownThrough"], "2026-11-01");
    assert_eq!(res.body["cappedScenes"], Value::Null);
    // The weekly cap in numbers (spec 0008 PRIV-09): nothing used yet.
    assert_eq!(res.body["weeklyCap"], 10);
    assert_eq!(res.body["remainingThisWeek"], 10);
    let again = f.follow(&f.dora, "performer", PERFORMER_P).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.body["id"], dora_follow);
    // The page shows each member their own follow.
    let page = f
        .as_member(
            &f.dora,
            get(&format!("/api/v1/private/performers/{PERFORMER_P}")),
        )
        .await;
    assert_eq!(page.body["follow"]["id"], dora_follow, "{}", page.raw);
    let page = f
        .as_member(
            &f.eve,
            get(&format!("/api/v1/private/performers/{PERFORMER_P}")),
        )
        .await;
    assert_eq!(page.body["follow"], Value::Null, "{}", page.raw);
    assert!(
        f.whisparr.state().writes.is_empty(),
        "following writes nothing"
    );

    f.check().await;
    // Exactly the new scene for Dora too, approved by her mode.
    assert_eq!(f.requested(&f.p.ada).await, [NEW]);
    assert_eq!(f.requested(&f.dora).await, [NEW]);
    let dora = &f.requests(&f.dora).await[0];
    assert_eq!(dora["state"], "submitted", "{dora}");
    // Dora's goes to Whisparr as the scene alone.
    assert_eq!(
        f.whisparr.state().writes_to("POST", "/api/v3/movie").len(),
        1
    );
    f.assert_only_scene_writes();
    // Both are told, in Starr with the title (the gate is open).
    let told = f.notifications(&f.p.ada, "follow_requested").await;
    assert_eq!(told.len(), 1);
    assert_eq!(told[0]["title"], "Scene New");
    assert_eq!(f.notifications(&f.dora, "follow_requested").await.len(), 1);
    // Nothing over the cap: the fixture's old hint is gone.
    assert_eq!(f.my_follows(&f.p.ada).await[0]["cappedScenes"], Value::Null);

    // The next day's run asks for nothing again.
    f.check().await;
    assert_eq!(f.requests(&f.p.ada).await.len(), 1);
    assert_eq!(f.requests(&f.dora).await.len(), 1);
    assert_eq!(
        f.whisparr.state().writes_to("POST", "/api/v3/movie").len(),
        1
    );

    // Unfollowed: no more requests, the follow leaves the list.
    let res = f
        .as_member(
            &f.dora,
            Req::new(
                Method::DELETE,
                &format!("/api/v1/follows/{dora_follow}"),
                None,
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    assert_eq!(f.my_follows(&f.dora).await, Vec::<Value>::new());
    // Audited by id and category only.
    let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT action, target, details FROM audit_log WHERE action LIKE 'follow.%'",
    )
    .fetch_all(f.p.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for (action, target, details) in &rows {
        assert_eq!(target, &None, "{action}");
        let details = details.as_deref().unwrap_or_default();
        assert!(details.contains("\"category\":\"adult\""), "{details}");
        assert!(!details.contains(PERFORMER_P), "{details}");
    }
}

fn day(n: usize) -> String {
    format!("2026-11-{:02}", n + 1)
}

fn cap_scene(n: usize) -> String {
    format!("5ce0e000-0000-4000-8000-0000000002{n:02}")
}

#[tokio::test]
async fn the_weekly_cap_requests_the_oldest_and_leaves_the_rest_to_the_member() {
    let f = follows(&[]).await;
    // Spec 0007 acceptance "Cap": 14 new scenes in a week, cap 10.
    let ids: Vec<String> = (1..=14).map(cap_scene).collect();
    let titles: Vec<String> = (1..=14).map(|n| format!("Scene {n:02}")).collect();
    let dates: Vec<String> = (1..=14).map(day).collect();
    let listed: Vec<ListedScene<'_>> = (0..14)
        .map(|i| (ids[i].as_str(), titles[i].as_str(), Some(dates[i].as_str())))
        .collect();
    mount_entity_scenes(&f.p.stashdb, &listed).await;
    let oldest = |n: usize| -> Vec<String> {
        let mut v: Vec<String> = ids[..n].to_vec();
        v.sort();
        v
    };

    // Ada follows Performer P (the fixture) and asks for the newest scene
    // by hand; Eve follows Studio S.
    mount_scene(&f.p.stashdb, &ids[13], &titles[13]).await;
    let res = f
        .as_member(
            &f.p.ada,
            post("/api/v1/private/requests", json!({ "scenes": [ids[13]] })),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let res = f.follow(&f.eve, "studio", STUDIO_S).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["kind"], "studio");
    let categories = f.p.admin(get("/api/v1/categories")).await;
    let adult = categories
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == "adult")
        .unwrap()
        .clone();
    assert_eq!(adult["catalogs"][0]["followCapPerWeek"], 10, "{adult}");
    let role = adult["catalogs"][0]["id"].as_i64().unwrap();

    f.check().await;
    // The ten oldest each; Ada's own request did not use up her cap.
    let mut ada_expected = oldest(10);
    ada_expected.push(ids[13].clone());
    ada_expected.sort();
    assert_eq!(f.requested(&f.p.ada).await, ada_expected);
    assert_eq!(f.requested(&f.eve).await, oldest(10));
    // The rest show on the follow: 3 for Ada (13 wanted), 4 for Eve.
    assert_eq!(f.my_follows(&f.p.ada).await[0]["cappedScenes"], 3);
    assert_eq!(f.my_follows(&f.eve).await[0]["cappedScenes"], 4);
    // The week is used up; Ada's own request does not count.
    assert_eq!(f.my_follows(&f.p.ada).await[0]["remainingThisWeek"], 0);
    assert_eq!(f.my_follows(&f.eve).await[0]["weeklyCap"], 10);
    let page = f
        .as_member(&f.eve, get(&format!("/api/v1/private/studios/{STUDIO_S}")))
        .await;
    assert_eq!(page.body["follow"]["cappedScenes"], 4, "{}", page.raw);
    // One notification each, naming the followed performer or studio in
    // Starr; nothing for Dora, who follows nothing.
    let capped = f.notifications(&f.p.ada, "follow_capped").await;
    assert_eq!(capped.len(), 1, "{capped:?}");
    assert_eq!(capped[0]["title"], "Performer P");
    assert_eq!(f.notifications(&f.eve, "follow_capped").await.len(), 1);
    assert_eq!(
        f.notifications(&f.dora, "follow_capped").await,
        Vec::<Value>::new()
    );
    // Locked, it is neither listed nor counted (spec 0008 Q24 A).
    f.p.set_unlocked(&f.p.ada.cookie, false).await;
    let locked = f.as_member(&f.p.ada, get("/api/v1/notifications")).await;
    assert_clean("ada (locked): notifications", &locked);
    assert!(!locked.raw.contains("follow_capped"), "{}", locked.raw);
    f.p.set_unlocked(&f.p.ada.cookie, true).await;
    assert!(f.whisparr.state().writes.is_empty(), "approval mode only");

    // The next run in the same week: the cap is used up, no second
    // notification.
    f.check().await;
    assert_eq!(f.requests(&f.p.ada).await.len(), 11);
    assert_eq!(f.requests(&f.eve).await.len(), 10);
    assert_eq!(f.notifications(&f.p.ada, "follow_capped").await.len(), 1);
    assert_eq!(f.my_follows(&f.p.ada).await[0]["cappedScenes"], 3);

    // The admin raises the cap: it applies from the next run on.
    let res =
        f.p.admin(put(
            &format!("/api/v1/categories/adult/roles/{role}/follow-cap"),
            json!({ "perWeek": 12 }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["catalogs"][0]["followCapPerWeek"], 12);
    assert_eq!(f.requests(&f.p.ada).await.len(), 11);
    f.check().await;
    assert_eq!(f.requests(&f.p.ada).await.len(), 13);
    assert_eq!(f.requested(&f.eve).await, oldest(12));
    assert_eq!(f.my_follows(&f.p.ada).await[0]["cappedScenes"], 1);
    assert_eq!(f.my_follows(&f.eve).await[0]["cappedScenes"], 2);
    assert_eq!(f.my_follows(&f.eve).await[0]["weeklyCap"], 12);
    assert_eq!(f.my_follows(&f.eve).await[0]["remainingThisWeek"], 0);

    // A week later the window has room again: the rest come, the hint
    // goes.
    sqlx::query(
        "UPDATE request SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', created_at, '-8 days')
         WHERE follow_id IS NOT NULL",
    )
    .execute(f.p.pool())
    .await
    .unwrap();
    f.check().await;
    assert_eq!(f.requests(&f.p.ada).await.len(), 14);
    assert_eq!(f.requested(&f.eve).await, oldest(14));
    assert_eq!(f.my_follows(&f.p.ada).await[0]["cappedScenes"], Value::Null);
    assert_eq!(f.my_follows(&f.eve).await[0]["cappedScenes"], Value::Null);
    assert!(f.whisparr.state().writes.is_empty(), "approval mode only");
}

fn delete(path: &str) -> Req {
    Req::new(Method::DELETE, path, None)
}

/// Undo of an unfollow (spec 0008 F8, PRIV-17): the follow comes back with
/// its original cutoff and start, only through the gate, only the
/// member's own private follow, and audited by id and category only.
#[tokio::test]
async fn an_unfollow_can_be_undone_with_the_original_cutoff() {
    let f = follows(&[]).await;
    let id = f.p.ada_follow;
    let before = f.my_follows(&f.p.ada).await[0].clone();
    let restore = format!("/api/v1/private/follows/{id}/restore");
    let res = f
        .as_member(&f.p.ada, delete(&format!("/api/v1/follows/{id}")))
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    assert_eq!(f.my_follows(&f.p.ada).await, Vec::<Value>::new());

    // Not for another member, nor without the entitlement or while locked.
    for who in [&f.eve, &f.p.ben] {
        let res = f.as_member(who, post(&restore, json!({}))).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
        assert_clean("restore by another member", &res);
    }
    f.p.set_unlocked(&f.p.ada.cookie, false).await;
    let res = f.as_member(&f.p.ada, post(&restore, json!({}))).await;
    assert_eq!(res.code(), "private.not_found", "{}", res.raw);
    assert_clean("ada (locked): restore", &res);
    f.p.set_unlocked(&f.p.ada.cookie, true).await;

    let res = f.as_member(&f.p.ada, post(&restore, json!({}))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    for field in ["id", "knownThrough", "createdAt", "entityId"] {
        assert_eq!(res.body[field], before[field], "{field}: {}", res.raw);
    }
    assert_eq!(f.my_follows(&f.p.ada).await.len(), 1);
    // Twice is harmless.
    let again = f.as_member(&f.p.ada, post(&restore, json!({}))).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.raw);

    // Followed anew meanwhile: the old follow stays ended.
    f.as_member(&f.p.ada, delete(&format!("/api/v1/follows/{id}")))
        .await;
    let renewed = f.follow(&f.p.ada, "performer", PERFORMER_P).await;
    assert_eq!(renewed.status, StatusCode::CREATED, "{}", renewed.raw);
    let res = f.as_member(&f.p.ada, post(&restore, json!({}))).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "follows.followed_again");

    // A follow of another category is not undone here.
    let res = f
        .as_member(
            &f.p.ada,
            post("/api/v1/private/follows/999999/restore", json!({})),
        )
        .await;
    assert_eq!(res.code(), "follows.not_found", "{}", res.raw);

    let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT action, target, details FROM audit_log WHERE action = 'follow.restore'",
    )
    .fetch_all(f.p.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (_, target, details) = &rows[0];
    assert_eq!(target, &None);
    let details = details.as_deref().unwrap_or_default();
    assert!(details.contains("\"category\":\"adult\""), "{details}");
    assert!(!details.contains(PERFORMER_P), "{details}");
}

#[tokio::test]
async fn private_follows_exist_only_behind_the_gate() {
    let f = follows(&[]).await;
    // Ben has no entitlement: the area does not exist for him.
    for req in [
        get("/api/v1/private/follows"),
        post(
            "/api/v1/private/follows",
            json!({ "kind": "performer", "id": PERFORMER_P }),
        ),
    ] {
        let res = f.as_member(&f.p.ben, req).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
        assert_eq!(res.code(), "private.not_found");
        assert_clean("ben", &res);
    }
    // Locked, neither.
    f.p.set_unlocked(&f.eve.cookie, false).await;
    let res = f.follow(&f.eve, "studio", STUDIO_S).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
    let res = f.as_member(&f.eve, get("/api/v1/private/follows")).await;
    assert_eq!(res.code(), "private.not_found");
    f.p.set_unlocked(&f.eve.cookie, true).await;
    // Ids that name nothing.
    let res = f.follow(&f.eve, "performer", "not-a-uuid").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
    assert_eq!(res.code(), "catalog.not_found");
    // Other members' follows stay theirs: Eve cannot end Ada's.
    let res = f
        .as_member(
            &f.eve,
            Req::new(
                Method::DELETE,
                &format!("/api/v1/follows/{}", f.p.ada_follow),
                None,
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
    // The general list never shows private follows, even unlocked.
    let general = f.as_member(&f.p.ada, get("/api/v1/follows")).await;
    assert_eq!(general.body, json!([]), "{}", general.raw);
    // Switched off, the area is gone for everyone.
    assert_eq!(f.p.switch_adult(false).await.status, StatusCode::OK);
    let res = f.as_member(&f.p.ada, get("/api/v1/private/follows")).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
}

#[tokio::test]
async fn only_the_private_catalog_takes_a_weekly_cap() {
    let f = follows(&[]).await;
    let categories = f.p.admin(get("/api/v1/categories")).await;
    let all = categories.body.as_array().unwrap();
    let catalog_of = |category: &str| {
        all.iter().find(|c| c["category"] == category).unwrap()["catalogs"][0].clone()
    };
    let adult = catalog_of("adult");
    let movies = catalog_of("movies");
    assert_eq!(movies["followCapPerWeek"], Value::Null, "{movies}");
    let cap = |category: &str, role: &Value, per_week: i64| {
        put(
            &format!(
                "/api/v1/categories/{category}/roles/{}/follow-cap",
                role["id"]
            ),
            json!({ "perWeek": per_week }),
        )
    };
    let res = f.p.admin(cap("movies", &movies, 5)).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "categories.follow_cap_not_offered");
    for out_of_range in [0, 101] {
        let res = f.p.admin(cap("adult", &adult, out_of_range)).await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
        assert_eq!(res.code(), "validation.follow_cap");
    }
    // A role of another category under this one's path is not found.
    let res = f.p.admin(cap("adult", &movies, 5)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
    // Members cannot set it.
    let res = f.as_member(&f.p.ada, cap("adult", &adult, 5)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
    let res = f.p.admin(cap("adult", &adult, 1)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let audit: Vec<(String,)> =
        sqlx::query_as("SELECT details FROM audit_log WHERE action = 'categories.follow_cap'")
            .fetch_all(f.p.pool())
            .await
            .unwrap();
    assert_eq!(audit.len(), 1);
    assert!(audit[0].0.contains("\"perWeek\":1"), "{audit:?}");
}
/// The fixture's follow as `follows.check` left it: its hint, and the
/// code of its item in the last run's report.
async fn last_check(f: &Follows) -> (Option<i64>, Option<String>) {
    let (capped,): (Option<i64>,) = sqlx::query_as("SELECT capped_scenes FROM follow WHERE id = ?")
        .bind(f.p.ada_follow)
        .fetch_one(f.p.pool())
        .await
        .unwrap();
    let (report,): (String,) = sqlx::query_as(
        "SELECT report FROM run WHERE kind = 'follows.check' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(f.p.pool())
    .await
    .unwrap();
    let report: Value = serde_json::from_str(&report).unwrap();
    let code = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["target"] == format!("[adult] follow #{}", f.p.ada_follow))
        .and_then(|i| i["code"].as_str())
        .map(str::to_owned);
    (capped, code)
}

/// Requests any follow created.
async fn by_follow(f: &Follows) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM request WHERE follow_id IS NOT NULL")
        .fetch_one(f.p.pool())
        .await
        .unwrap();
    n
}

#[tokio::test]
async fn a_follow_that_cannot_be_checked_requests_nothing_and_loses_its_hint() {
    let f = follows(&[]).await;
    mount_entity_scenes(&f.p.stashdb, &[(NEW, "Scene New", Some("2026-11-05"))]).await;

    // The catalog key was refused by its last check (spec 0007 Q14): the
    // worker does not use it either.
    sqlx::query("UPDATE service_instance SET last_error = 'refused' WHERE id = ?")
        .bind(f.p.stashdb_id)
        .execute(f.p.pool())
        .await
        .unwrap();
    let asked = common::stashdb::graphql_requests(&f.p.stashdb).await;
    f.check().await;
    assert_eq!(
        last_check(&f).await,
        (None, Some("category_not_ready".to_owned()))
    );
    assert_eq!(common::stashdb::graphql_requests(&f.p.stashdb).await, asked);
    assert_eq!(by_follow(&f).await, 0);
    sqlx::query("UPDATE service_instance SET last_error = NULL WHERE id = ?")
        .bind(f.p.stashdb_id)
        .execute(f.p.pool())
        .await
        .unwrap();

    // Switched on without a valid operator statement (an older release
    // toggled it): no requests.
    sqlx::query("UPDATE follow SET capped_scenes = 4 WHERE id = ?")
        .bind(f.p.ada_follow)
        .execute(f.p.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE category SET statement_at = NULL, statement_by = NULL WHERE name = 'adult'",
    )
    .execute(f.p.pool())
    .await
    .unwrap();
    f.check().await;
    assert_eq!(
        last_check(&f).await,
        (None, Some("category_not_ready".to_owned()))
    );
    assert_eq!(by_follow(&f).await, 0);
    sqlx::query(
        "UPDATE category SET statement_at = updated_at, statement_by = 'root' WHERE name = 'adult'",
    )
    .execute(f.p.pool())
    .await
    .unwrap();

    // A performer without its StashDB ID fails alone.
    sqlx::query(
        "DELETE FROM catalog_key WHERE scheme = 'stashdb_performer'
           AND catalog_item_id = (SELECT catalog_item_id FROM follow WHERE id = ?)",
    )
    .bind(f.p.ada_follow)
    .execute(f.p.pool())
    .await
    .unwrap();
    f.check().await;
    assert_eq!(last_check(&f).await, (None, Some("no_stash_id".to_owned())));
    assert_eq!(by_follow(&f).await, 0);
}

#[tokio::test]
async fn following_needs_a_category_that_takes_requests() {
    // The privacy setup has no adult source: new scenes could not become
    // requests.
    let p = privacy().await;
    p.set_unlocked(&p.ada.cookie, true).await;
    let res = p
        .as_member(
            &p.ada,
            post(
                "/api/v1/private/follows",
                json!({ "kind": "studio", "id": STUDIO_S }),
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "requests.category_not_ready");
    // The page offers no follow then; the existing one is still shown.
    let page = p
        .as_member(&p.ada, get(&format!("/api/v1/private/studios/{STUDIO_S}")))
        .await;
    assert_eq!(page.body["requestable"], false, "{}", page.raw);
    assert_eq!(page.body["follow"], Value::Null);
}
