//! Follows and `follows.check` (spec 0003 D8, slice 7). Every member
//! endpoint is exercised by two members following the same series
//! (lesson 4).
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::StatusCode;
use common::media::{MOVIE, SERIES, Setup, fixture, member, put, setup};
use common::services::delete;
use common::{get, post};
use serde_json::{Value, json};
use wiremock::matchers::path;
use wiremock::{Mock, ResponseTemplate};

async fn follow(s: &Setup, member: &common::media::Member) -> common::TestResponse {
    s.as_member(
        member,
        post(
            "/api/v1/follows",
            json!({ "kind": "series", "titleId": SERIES.to_string() }),
        ),
    )
    .await
}

/// From now on TMDB lists Game of Thrones season 9 with an air date.
async fn season_nine_dated(s: &Setup) {
    season_nine_airs(s, "2026-12-01").await;
}

/// From now on TMDB lists Game of Thrones season 9 airing on `date`.
#[allow(clippy::used_underscore_binding)] // the fakes are kept alive there
async fn season_nine_airs(s: &Setup, date: &str) {
    let mut tv: Value = serde_json::from_str(&fixture("tmdb/fixtures/v3", "tv_1399.json")).unwrap();
    for season in tv["seasons"].as_array_mut().unwrap() {
        if season["season_number"] == 9 {
            season["air_date"] = json!(date);
            season["episode_count"] = json!(8);
        }
    }
    Mock::given(path(format!("/3/tv/{SERIES}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(tv))
        .with_priority(1)
        .mount(&s._servers[0])
        .await;
    forget_catalog_responses(s).await;
}

/// A day later: every follow is due again and the next run reads the
/// catalog again.
async fn forget_catalog_responses(s: &Setup) {
    sqlx::query("DELETE FROM catalog_cache")
        .execute(s.env.app.state.db.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE follow SET checked_at = strftime('%Y-%m-%dT%H:%M:%fZ', checked_at, '-1 day')
         WHERE checked_at IS NOT NULL",
    )
    .execute(s.env.app.state.db.pool())
    .await
    .unwrap();
}

/// Run `follows.check` and return the report of its run.
async fn check(s: &Setup) -> Value {
    let res = s
        .admin(post("/api/v1/jobs/follows.check/trigger", json!({})))
        .await;
    assert!(res.status.is_success(), "{}", res.raw);
    s.env.drain().await;
    let (state, report): (String, Option<String>) = sqlx::query_as(
        "SELECT state, report FROM run WHERE kind = 'follows.check' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(s.env.app.state.db.pool())
    .await
    .unwrap();
    assert_eq!(state, "succeeded");
    serde_json::from_str(&report.unwrap()).unwrap()
}

async fn requests_of(s: &Setup, member: &common::media::Member) -> Vec<Value> {
    let res = s.as_member(member, get("/api/v1/requests")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    res.body.as_array().unwrap().clone()
}

#[tokio::test]
async fn two_members_follow_the_same_series_each_on_their_own() {
    let s = setup().await;
    // Mia follows by requesting season 1 with "also get new seasons".
    let res = s.request_seasons(&s.mia, &[1]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    // Noah follows from the detail sheet without requesting.
    let noah = follow(&s, &s.noah).await;
    assert_eq!(noah.status, StatusCode::CREATED, "{}", noah.raw);
    assert_eq!(noah.body["title"], "Game of Thrones");
    assert_eq!(noah.body["kind"], "series");
    assert_eq!(noah.body["titleId"], SERIES.to_string());
    assert_eq!(noah.body["category"], "series");
    // Seasons 1 and 2 have an air date, season 9 not yet.
    assert_eq!(noah.body["seasonsThrough"], 2);
    let again = follow(&s, &s.noah).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.raw);
    assert_eq!(again.body["id"], noah.body["id"]);

    let mine = s.as_member(&s.mia, get("/api/v1/follows")).await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.raw);
    assert_eq!(mine.body.as_array().unwrap().len(), 1);
    assert_eq!(mine.body[0]["seasonsThrough"], 2);
    assert_ne!(mine.body[0]["id"], noah.body["id"]);
    assert!(!mine.raw.contains("noah"), "{}", mine.raw);
    let mia_follow = mine.body[0]["id"].as_i64().unwrap();

    // Nobody ends another member's follow.
    let res = s
        .as_member(&s.noah, delete(&format!("/api/v1/follows/{mia_follow}")))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
    assert_eq!(res.code(), "follows.not_found");
    let mine = s.as_member(&s.mia, get("/api/v1/follows")).await;
    assert_eq!(mine.body.as_array().unwrap().len(), 1);

    let res = s
        .as_member(&s.mia, delete(&format!("/api/v1/follows/{mia_follow}")))
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    let mine = s.as_member(&s.mia, get("/api/v1/follows")).await;
    assert_eq!(mine.body, json!([]));
    let theirs = s.as_member(&s.noah, get("/api/v1/follows")).await;
    assert_eq!(
        theirs.body.as_array().unwrap().len(),
        1,
        "Noah still follows"
    );
    // Mia's request stays.
    assert_eq!(requests_of(&s, &s.mia).await.len(), 1);
}

#[tokio::test]
async fn only_series_and_artists_of_an_entitled_member_can_be_followed() {
    let s = setup().await;
    let res = s
        .as_member(
            &s.mia,
            post(
                "/api/v1/follows",
                json!({ "kind": "movie", "titleId": MOVIE.to_string() }),
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "follows.not_followable");

    let lea = member(
        &s.env,
        "lea",
        json!([{ "category": "movies", "enabled": true, "requestMode": "auto" }]),
    )
    .await;
    let res = follow(&s, &lea).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "requests.not_entitled");

    let res = s.admin(get("/api/v1/follows")).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
}

#[tokio::test]
async fn new_seasons_become_requests_in_the_members_name_once() {
    let s = setup().await;
    // Mia (approval) follows by requesting season 1; she asked for the
    // announced season 9 before and was rejected. Noah (auto) follows
    // later, so the check reaches him first.
    assert_eq!(
        s.request_seasons(&s.mia, &[1]).await.status,
        StatusCode::CREATED
    );
    let res = s
        .as_member(
            &s.mia,
            post(
                "/api/v1/requests",
                json!({ "kind": "series", "titleId": SERIES.to_string(), "seasons": [9] }),
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let res = s
        .admin(post(
            "/api/v1/admin/approvals/reject",
            json!({
                "requests": [{ "id": res.body["request"]["id"], "memberId": s.mia.id }],
                "reason": "Not announced yet",
            }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(follow(&s, &s.noah).await.status, StatusCode::CREATED);

    // Nothing new yet: season 9 has no air date.
    let report = check(&s).await;
    assert_eq!(report["expectation"]["status"], "met", "{report}");
    assert_eq!(
        report["expectation"]["detail"],
        "checked 2 follows, 0 new seasons, 0 new albums, 0 new scenes, 0 requests created, 0 scenes over the weekly cap"
    );
    assert_eq!(report["total"], 2);
    assert_eq!(report["succeeded"], 2);
    assert_eq!(requests_of(&s, &s.noah).await.len(), 0);

    season_nine_dated(&s).await;
    let report = check(&s).await;
    assert_eq!(report["expectation"]["status"], "met", "{report}");
    assert_eq!(
        report["expectation"]["detail"],
        "checked 2 follows, 2 new seasons, 0 new albums, 0 new scenes, 2 requests created, 0 scenes over the weekly cap",
    );
    assert_eq!(report["counters"]["requests_created"], 2);

    // In each member's name, with their request mode: Noah's is approved
    // and acquired; Mia's waits for an admin although it could attach,
    // because she was rejected for season 9 before (D10).
    let noah = requests_of(&s, &s.noah).await;
    assert_eq!(noah.len(), 1);
    assert_eq!(noah[0]["items"][0]["season"], 9);
    assert_eq!(noah[0]["follow"], true);
    assert_ne!(noah[0]["state"], "pending_approval", "{noah:?}");
    let mia = requests_of(&s, &s.mia).await;
    assert_eq!(mia.len(), 3);
    let item = &mia[0]["items"][0];
    assert_eq!(mia[0]["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        (
            &item["season"],
            &item["state"],
            &item["progress"],
            &item["stage"]
        ),
        (
            &json!(9),
            &json!("pending_approval"),
            &Value::Null,
            &Value::Null
        )
    );
    // Its journey starts with "requested by your follow".
    assert_eq!(item["journey"]["steps"][0]["note"], "by_follow");
    let (actor, detail): (String, Option<String>) = sqlx::query_as(
        "SELECT trigger || ':' || actor, detail FROM request_event WHERE request_id = ?
         ORDER BY id LIMIT 1",
    )
    .bind(noah[0]["id"].as_i64().unwrap())
    .fetch_one(s.env.app.state.db.pool())
    .await
    .unwrap();
    assert_eq!(actor, "system:follows.check");
    assert_eq!(detail.as_deref(), Some("new season of a followed series"));
    let follows = s.as_member(&s.mia, get("/api/v1/follows")).await;
    assert_eq!(follows.body[0]["seasonsThrough"], 9);
    assert!(follows.body[0]["checkedAt"].is_string());

    // The next day nothing is asked again.
    forget_catalog_responses(&s).await;
    let report = check(&s).await;
    assert_eq!(
        report["expectation"]["detail"],
        "checked 2 follows, 0 new seasons, 0 new albums, 0 new scenes, 0 requests created, 0 scenes over the weekly cap",
    );
    assert_eq!(requests_of(&s, &s.mia).await.len(), 3);
    assert_eq!(requests_of(&s, &s.noah).await.len(), 1);
}

#[tokio::test]
async fn members_without_entitlement_are_skipped_and_keep_their_cutoff() {
    let s = setup().await;
    assert_eq!(follow(&s, &s.mia).await.status, StatusCode::CREATED);
    assert_eq!(follow(&s, &s.noah).await.status, StatusCode::CREATED);
    let res = s
        .admin(put(
            &format!("/api/v1/members/{}/entitlements", s.noah.id),
            json!({ "entitlements": [
                { "category": "movies", "enabled": true, "requestMode": "auto" },
                { "category": "series", "enabled": false, "requestMode": "auto" },
            ] }),
        ))
        .await;
    assert!(res.status.is_success(), "{}", res.raw);
    let res = s
        .admin(post(
            &format!("/api/v1/members/{}/suspend", s.mia.id),
            json!({}),
        ))
        .await;
    assert!(res.status.is_success(), "{}", res.raw);

    season_nine_dated(&s).await;
    let report = check(&s).await;
    assert_eq!(report["expectation"]["status"], "met", "{report}");
    assert_eq!(report["skipped"], 2, "{report}");
    let codes: Vec<&str> = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"not_entitled"), "{codes:?}");
    assert!(codes.contains(&"member_inactive"), "{codes:?}");
    assert_eq!(requests_of(&s, &s.noah).await.len(), 0);

    // Entitled again: the season that came meanwhile is asked for.
    let res = s
        .admin(put(
            &format!("/api/v1/members/{}/entitlements", s.noah.id),
            json!({ "entitlements": [
                { "category": "movies", "enabled": true, "requestMode": "auto" },
                { "category": "series", "enabled": true, "requestMode": "approval" },
            ] }),
        ))
        .await;
    assert!(res.status.is_success(), "{}", res.raw);
    forget_catalog_responses(&s).await;
    check(&s).await;
    let noah = requests_of(&s, &s.noah).await;
    assert_eq!(noah.len(), 1);
    assert_eq!(noah[0]["state"], "pending_approval");
}
#[tokio::test]
#[allow(clippy::used_underscore_binding)] // the fakes are kept alive there
async fn a_catalog_failure_fails_the_run_and_keeps_the_cutoff() {
    let s = setup().await;
    assert_eq!(follow(&s, &s.mia).await.status, StatusCode::CREATED);
    // TMDB answers the series with 503 for one run.
    let down = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&down);
    let healthy = fixture("tmdb/fixtures/v3", "tv_1399.json");
    Mock::given(path(format!("/3/tv/{SERIES}")))
        .respond_with(move |_: &wiremock::Request| {
            if flag.load(Ordering::SeqCst) {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200).set_body_string(healthy.clone())
            }
        })
        .with_priority(2)
        .mount(&s._servers[0])
        .await;
    forget_catalog_responses(&s).await;
    let report = check(&s).await;
    assert_eq!(report["expectation"]["status"], "unmet", "{report}");
    assert_eq!(report["failed"], 1, "{report}");
    assert_eq!(report["total"], 1);
    let follows = s.as_member(&s.mia, get("/api/v1/follows")).await;
    assert_eq!(follows.body[0]["seasonsThrough"], 2);
    assert!(
        follows.body[0]["checkedAt"].is_string(),
        "looked at, to be checked again a day later"
    );
    down.store(false, Ordering::SeqCst);

    // Back the next day: the season that came meanwhile is asked for.
    season_nine_dated(&s).await;
    let report = check(&s).await;
    assert_eq!(report["expectation"]["status"], "met", "{report}");
    assert_eq!(report["counters"]["requests_created"], 1);
}

#[tokio::test]
async fn a_new_season_the_member_asked_for_already_is_not_asked_again() {
    let s = setup().await;
    assert_eq!(follow(&s, &s.mia).await.status, StatusCode::CREATED);
    let res = s
        .as_member(
            &s.mia,
            post(
                "/api/v1/requests",
                json!({ "kind": "series", "titleId": SERIES.to_string(), "seasons": [9] }),
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);

    season_nine_dated(&s).await;
    let report = check(&s).await;
    assert_eq!(
        report["expectation"]["detail"],
        "checked 1 follows, 1 new seasons, 0 new albums, 0 new scenes, 0 requests created, 0 scenes over the weekly cap",
        "{report}"
    );
    assert_eq!(requests_of(&s, &s.mia).await.len(), 1);
    // Handled: the cutoff moved past it.
    let follows = s.as_member(&s.mia, get("/api/v1/follows")).await;
    assert_eq!(follows.body[0]["seasonsThrough"], 9);
}
#[tokio::test]
async fn an_announced_season_airing_later_is_asked_for() {
    let s = setup().await;
    // Season 9 is announced with a date far ahead when Mia starts following
    // (maintainer, 2026-11-02: only aired seasons are known at the start).
    season_nine_airs(&s, "2099-01-01").await;
    let res = follow(&s, &s.mia).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["seasonsThrough"], 2);

    let report = check(&s).await;
    assert_eq!(
        report["expectation"]["detail"],
        "checked 1 follows, 1 new seasons, 0 new albums, 0 new scenes, 1 requests created, 0 scenes over the weekly cap",
        "{report}"
    );
    let mia = requests_of(&s, &s.mia).await;
    assert_eq!(mia[0]["items"][0]["season"], 9);
}

#[tokio::test]
async fn asking_with_follow_follows_even_when_nothing_is_left_to_request() {
    let s = setup().await;
    let res = s
        .as_member(
            &s.mia,
            post(
                "/api/v1/requests",
                json!({ "kind": "series", "titleId": SERIES.to_string(), "seasons": [1] }),
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    // Season 1 again, now with "also get new seasons": nothing to request.
    let res = s.request_seasons(&s.mia, &[1]).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "requests.already_requested");
    let follows = s.as_member(&s.mia, get("/api/v1/follows")).await;
    assert_eq!(follows.body.as_array().unwrap().len(), 1, "{}", follows.raw);
    assert_eq!(follows.body[0]["seasonsThrough"], 2);
    // Noah never asked with follow: nothing followed for him.
    let theirs = s.as_member(&s.noah, get("/api/v1/follows")).await;
    assert_eq!(theirs.body, json!([]));
}
