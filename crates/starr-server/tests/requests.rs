//! Requests, approvals and acquisitions (spec 0003 slice 4). Every member
//! endpoint is exercised by two members against the same title (lesson 4).
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;
use common::media::{MOVIE, SERIES, entitlements, member, put, service, setup};
use common::services::{delete, patch};
use common::{get, post};
use serde_json::{Value, json};

fn refs(requests: &[(i64, i64)]) -> Value {
    json!({
        "requests": requests
            .iter()
            .map(|(id, member)| json!({ "id": id, "memberId": member }))
            .collect::<Vec<_>>()
    })
}

#[tokio::test]
async fn two_members_request_the_same_movie_and_share_one_acquisition() {
    let s = setup().await;
    let mia = s.request_movie(&s.mia).await;
    assert_eq!(mia.status, StatusCode::CREATED, "{}", mia.raw);
    assert_eq!(mia.body["request"]["state"], "pending_approval");
    assert_eq!(mia.body["request"]["title"], "Inception");
    assert_eq!(mia.body["request"]["canWithdraw"], true);
    assert_eq!(mia.body["request"]["items"][0]["season"], Value::Null);
    let mia_id = mia.body["request"]["id"].as_i64().unwrap();
    assert_eq!(
        s.submit_jobs().await,
        0,
        "nothing to submit before approval"
    );

    // Noah requests automatically: approved, the acquisition starts.
    let noah = s.request_movie(&s.noah).await;
    assert_eq!(noah.status, StatusCode::CREATED, "{}", noah.raw);
    assert_eq!(noah.body["request"]["state"], "approved");
    assert_eq!(noah.body["request"]["canWithdraw"], false);
    assert_eq!(s.submit_jobs().await, 1);

    // Switching the movie source would leave Noah's request finishing at
    // this Radarr.
    let preview = s
        .admin(get(&format!(
            "/api/v1/categories/movies/source-switch?serviceId={}",
            s.radarr
        )))
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.raw);
    assert_eq!(
        preview.body["affectedRequests"],
        json!([noah.body["request"]["id"]])
    );

    // Each sees only their own request, never the other member.
    let mine = s.as_member(&s.mia, get("/api/v1/requests")).await;
    assert_eq!(mine.body.as_array().unwrap().len(), 1);
    assert_eq!(mine.body[0]["id"], mia_id);
    assert!(!mine.raw.contains("noah"), "{}", mine.raw);
    let theirs = s.as_member(&s.noah, get("/api/v1/requests")).await;
    assert_eq!(theirs.body.as_array().unwrap().len(), 1);
    assert!(!theirs.raw.contains("mia"), "{}", theirs.raw);

    // Needs you: only Mia's request waits, with who asked and when.
    let inbox = s.admin(get("/api/v1/admin/approvals")).await;
    assert_eq!(inbox.status, StatusCode::OK, "{}", inbox.raw);
    assert_eq!(inbox.body.as_array().unwrap().len(), 1);
    assert_eq!(inbox.body[0]["member"]["username"], "mia");
    assert!(inbox.body[0]["createdAt"].is_string());

    // Approving with the wrong owner changes nothing.
    let wrong = s
        .admin(post(
            "/api/v1/admin/approvals/approve",
            refs(&[(mia_id, s.noah.id)]),
        ))
        .await;
    assert_eq!(wrong.status, StatusCode::CONFLICT);
    assert_eq!(wrong.code(), "requests.owner_mismatch");
    let approved = s
        .admin(post(
            "/api/v1/admin/approvals/approve",
            refs(&[(mia_id, s.mia.id)]),
        ))
        .await;
    assert_eq!(approved.status, StatusCode::OK, "{}", approved.raw);
    assert_eq!(approved.body["decided"], json!([mia_id]));
    // Mia's request joined Noah's acquisition: one acquisition, two items.
    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(acquisitions.body.as_array().unwrap().len(), 1);
    assert_eq!(acquisitions.body[0]["requests"], 2);
    assert_eq!(acquisitions.body[0]["state"], "pending");
    assert_eq!(acquisitions.body[0]["serviceId"], s.radarr);
    let inbox = s.admin(get("/api/v1/admin/approvals")).await;
    assert_eq!(inbox.body, json!([]));

    // Status: requested, and only "by me" for the member's own request.
    for member in [&s.mia, &s.noah] {
        let status = s
            .as_member(
                member,
                get(&format!(
                    "/api/v1/requests/status?kind=movie&ids={MOVIE},550"
                )),
            )
            .await;
        assert_eq!(status.status, StatusCode::OK, "{}", status.raw);
        assert_eq!(status.body[0]["status"], "requested");
        assert_eq!(status.body[0]["requestedByMe"], true);
        assert_eq!(status.body[1]["status"], "new");
        assert_eq!(status.body[1]["requestedByMe"], false);
    }
}

#[tokio::test]
async fn others_requests_show_as_requested_without_names() {
    let s = setup().await;
    let noah = s.request_seasons(&s.noah, &[1]).await;
    assert_eq!(noah.status, StatusCode::CREATED, "{}", noah.raw);
    assert_eq!(noah.body["request"]["follow"], true);
    assert_eq!(noah.body["request"]["items"][0]["season"], 1);

    let status = s
        .as_member(
            &s.mia,
            get(&format!("/api/v1/requests/status?kind=series&ids={SERIES}")),
        )
        .await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.raw);
    assert!(!status.raw.contains("noah"), "{}", status.raw);
    let title = &status.body[0];
    assert_eq!(title["status"], "requested");
    assert_eq!(title["requestedByMe"], false);
    let seasons = title["seasons"].as_array().unwrap();
    let season = |n: u64| seasons.iter().find(|s| s["number"] == n).unwrap();
    assert_eq!(season(1)["status"], "requested");
    assert_eq!(season(1)["requestedByMe"], false);
    assert_eq!(season(2)["status"], "new");

    // Mia asks for seasons 1 and 2 in approval mode: season 1 attaches
    // to Noah's running acquisition (approved), season 2 waits.
    let mia = s.request_seasons(&s.mia, &[1, 2]).await;
    assert_eq!(mia.status, StatusCode::CREATED, "{}", mia.raw);
    assert_eq!(mia.body["request"]["items"][0]["state"], "approved");
    assert_eq!(mia.body["request"]["items"][1]["state"], "pending_approval");
    assert_eq!(mia.body["request"]["state"], "pending_approval");
    assert_eq!(mia.body["request"]["canWithdraw"], true);
}

#[tokio::test]
async fn withdraw_reject_and_their_limits() {
    let s = setup().await;
    let mia = s.request_movie(&s.mia).await;
    let id = mia.body["request"]["id"].as_i64().unwrap();
    let url = format!("/api/v1/requests/{id}/withdraw");

    // Noah cannot withdraw (or even see) Mia's request.
    let res = s.as_member(&s.noah, post(&url, json!({}))).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.code(), "requests.not_found");
    let res = s.as_member(&s.mia, post(&url, json!({}))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["state"], "cancelled");
    let res = s.as_member(&s.mia, post(&url, json!({}))).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "requests.not_pending");

    // Asked again, then rejected with a reason Mia reads.
    let again = s.request_movie(&s.mia).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.raw);
    let again_id = again.body["request"]["id"].as_i64().unwrap();
    let dup = s.request_movie(&s.mia).await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    assert_eq!(dup.code(), "requests.already_requested");
    let empty = s
        .admin(post(
            "/api/v1/admin/approvals/reject",
            json!({ "requests": [{ "id": again_id, "memberId": s.mia.id }], "reason": " " }),
        ))
        .await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(empty.code(), "requests.reason_invalid");
    let none = s
        .admin(post(
            "/api/v1/admin/approvals/reject",
            json!({ "requests": [], "reason": "x" }),
        ))
        .await;
    assert_eq!(none.code(), "requests.selection_invalid");
    let rejected = s
        .admin(post(
            "/api/v1/admin/approvals/reject",
            json!({
                "requests": [{ "id": again_id, "memberId": s.mia.id }],
                "reason": "We keep this one on disc only",
            }),
        ))
        .await;
    assert_eq!(rejected.status, StatusCode::OK, "{}", rejected.raw);
    let mine = s.as_member(&s.mia, get("/api/v1/requests")).await;
    let rejected = mine
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == again_id)
        .unwrap();
    assert_eq!(rejected["state"], "rejected");
    assert_eq!(rejected["rejectReason"], "We keep this one on disc only");
    // The member's view never names the admin either.
    assert!(!mine.raw.contains("root"), "{}", mine.raw);

    // History for admins: created, cancelled; created, rejected by root.
    let detail = s
        .admin(get(&format!("/api/v1/admin/requests/{again_id}")))
        .await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.raw);
    let events = detail.body["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["trigger"], "member");
    assert_eq!(events[0]["actor"], "mia");
    assert_eq!(events[1]["toState"], "rejected");
    assert_eq!(events[1]["trigger"], "admin");
    assert_eq!(events[1]["actor"], "root");
}

#[tokio::test]
async fn invalid_requests_are_refused() {
    let s = setup().await;
    let send = |body: Value| s.as_member(&s.mia, post("/api/v1/requests", body));
    let res = send(json!({ "kind": "movie", "titleId": MOVIE.to_string(), "seasons": [1] })).await;
    assert_eq!(res.code(), "requests.seasons_unexpected");
    let res = send(json!({ "kind": "movie", "titleId": MOVIE.to_string(), "follow": true })).await;
    assert_eq!(res.code(), "requests.follow_movie");
    let res = send(json!({ "kind": "series", "titleId": SERIES.to_string() })).await;
    assert_eq!(res.code(), "requests.seasons_required");
    let res = send(json!({ "kind": "series", "titleId": SERIES.to_string(), "seasons": [] })).await;
    assert_eq!(res.code(), "requests.seasons_required");
    let res =
        send(json!({ "kind": "series", "titleId": SERIES.to_string(), "seasons": [7] })).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.code(), "requests.unknown_season");
    let res = send(json!({ "kind": "movie", "titleId": "1" })).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
    // Ids are opaque catalog ids; one that names no TMDB title is not found.
    for id in ["abc", "0", "", "0603", " 603", "+603"] {
        let res = send(json!({ "kind": "movie", "titleId": id })).await;
        assert_eq!(res.code(), "catalog.not_found", "{id}");
    }
    // The old field is gone (ADR 0012).
    let res = send(json!({ "kind": "movie", "tmdbId": MOVIE })).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);

    // Not entitled to movies: no request, no status.
    let lea = member(
        &s.env,
        "lea",
        json!([{ "category": "series", "enabled": true, "requestMode": "auto" }]),
    )
    .await;
    let res = s.request_movie(&lea).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.code(), "requests.not_entitled");
    let res = s
        .as_member(
            &lea,
            get(&format!("/api/v1/requests/status?kind=movie&ids={MOVIE}")),
        )
        .await;
    assert_eq!(res.code(), "requests.not_entitled");
    let res = s
        .as_member(&s.mia, get("/api/v1/requests/status?kind=movie&ids=x"))
        .await;
    assert_eq!(res.code(), "validation.title_ids");

    // A category switched off takes no requests.
    s.admin(patch(
        "/api/v1/categories/movies",
        json!({ "enabled": false }),
    ))
    .await;
    let res = s.request_movie(&s.noah).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "requests.category_not_ready");

    // Admins use the admin side, members the member side.
    let res = s.admin(get("/api/v1/requests")).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    for path in [
        "/api/v1/admin/approvals",
        "/api/v1/admin/requests",
        "/api/v1/admin/acquisitions",
    ] {
        let res = s.as_member(&s.mia, get(path)).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{path}");
    }
    let res = s
        .as_member(
            &s.mia,
            post("/api/v1/admin/approvals/approve", refs(&[(1, s.mia.id)])),
        )
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn titles_already_in_the_library_cannot_be_requested() {
    let s = setup().await;
    let db = &s.env.app.state.db;
    let jellyfin = service(
        &s.env,
        "jellyfin",
        "http://jellyfin:8096",
        "jellyfin-key-0123456789",
    )
    .await;
    let res = s
        .admin(post(
            "/api/v1/categories/movies/roles",
            json!({ "kind": "consume", "serviceId": jellyfin }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    sqlx::query(
        "INSERT INTO inventory (service_instance_id, natural_key, kind, tmdb_id, remote_id, title,
             present_since, last_seen)
         VALUES (?, 'movie:tmdb:27205', 'movie', 27205, 'abc', 'Inception',
             '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z')",
    )
    .bind(jellyfin)
    .execute(db.pool())
    .await
    .unwrap();

    let status = s
        .as_member(
            &s.mia,
            get(&format!("/api/v1/requests/status?kind=movie&ids={MOVIE}")),
        )
        .await;
    assert_eq!(status.body[0]["status"], "available");
    let res = s.request_movie(&s.mia).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "requests.already_available");
}

#[tokio::test]
async fn admin_overview_filters_and_acquisition_retry() {
    let s = setup().await;
    s.request_movie(&s.mia).await;
    s.request_seasons(&s.noah, &[1, 2]).await;
    let all = s.admin(get("/api/v1/admin/requests")).await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.raw);
    assert_eq!(all.body.as_array().unwrap().len(), 2);
    // Every row names its owner (lesson 4).
    assert!(
        all.body
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["member"]["username"].is_string())
    );
    let series = s.admin(get("/api/v1/admin/requests?category=series")).await;
    assert_eq!(series.body.as_array().unwrap().len(), 1);
    assert_eq!(series.body[0]["member"]["username"], "noah");
    assert_eq!(series.body[0]["items"].as_array().unwrap().len(), 2);
    let pending = s
        .admin(get("/api/v1/admin/requests?state=pending_approval"))
        .await;
    assert_eq!(pending.body.as_array().unwrap().len(), 1);
    assert_eq!(pending.body[0]["member"]["username"], "mia");
    let by_member = s
        .admin(get(&format!(
            "/api/v1/admin/requests?memberId={}",
            s.noah.id
        )))
        .await;
    assert_eq!(by_member.body.as_array().unwrap().len(), 1);
    let res = s.admin(get("/api/v1/admin/requests/999")).await;
    assert_eq!(res.code(), "requests.not_found");

    let acquisitions = s
        .admin(get("/api/v1/admin/acquisitions?state=pending"))
        .await;
    let list = acquisitions.body.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|a| a["title"] == "Game of Thrones"));
    let id = list[0]["id"].as_i64().unwrap();
    let res = s
        .admin(post(
            &format!("/api/v1/admin/acquisitions/{id}/retry"),
            json!({}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "acquisitions.not_failed");
    sqlx::query(
        "UPDATE acquisition SET state = 'failed', last_error = 'Path not allowed' WHERE id = ?",
    )
    .bind(id)
    .execute(s.env.app.state.db.pool())
    .await
    .unwrap();
    let res = s
        .admin(post(
            &format!("/api/v1/admin/acquisitions/{id}/retry"),
            json!({}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["state"], "pending");
    let res = s
        .admin(post("/api/v1/admin/acquisitions/999/retry", json!({})))
        .await;
    assert_eq!(res.code(), "acquisitions.not_found");
}

#[tokio::test]
async fn removing_the_source_fails_shared_requests_visibly() {
    let s = setup().await;
    let mia = s.request_movie(&s.noah).await;
    assert_eq!(mia.status, StatusCode::CREATED, "{}", mia.raw);
    let res = s
        .admin(delete(&format!("/api/v1/services/{}", s.radarr)))
        .await;
    assert!(res.status.is_success(), "{}", res.raw);

    let mine = s.as_member(&s.noah, get("/api/v1/requests")).await;
    assert_eq!(mine.body[0]["state"], "failed");
    let acquisitions = s
        .admin(get("/api/v1/admin/acquisitions?state=failed"))
        .await;
    let failed = &acquisitions.body[0];
    assert_eq!(failed["serviceId"], Value::Null);
    assert_eq!(failed["errorCode"], "source_removed");
    // The category lost its source too: nowhere to retry yet.
    let id = failed["id"].as_i64().unwrap();
    let res = s
        .admin(post(
            &format!("/api/v1/admin/acquisitions/{id}/retry"),
            json!({}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "requests.category_not_ready");
}

#[tokio::test]
async fn available_and_already_requested_seasons_are_skipped_not_refused() {
    let s = setup().await;
    let first = s.request_seasons(&s.noah, &[1]).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.raw);
    assert_eq!(first.body["skipped"], json!([]));

    // Season 2 is in Jellyfin with all its episodes.
    let jellyfin = service(
        &s.env,
        "jellyfin",
        "http://jellyfin:8096",
        "jellyfin-key-0123456789",
    )
    .await;
    let res = s
        .admin(post(
            "/api/v1/categories/series/roles",
            json!({ "kind": "consume", "serviceId": jellyfin }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    sqlx::query(
        "INSERT INTO inventory (service_instance_id, natural_key, kind, tmdb_id, tvdb_id,
             remote_id, title, seasons, present_since, last_seen)
         VALUES (?, 'series:tvdb:121361', 'series', 1399, 121361, 'x', 'Game of Thrones',
             '{\"2\": 10}', '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z')",
    )
    .bind(jellyfin)
    .execute(s.env.app.state.db.pool())
    .await
    .unwrap();

    // Asking for 0, 1 and 2: 1 is Noah's already, 2 is there; 0 is new.
    let res = s.request_seasons(&s.noah, &[0, 1, 2]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let items = res.body["request"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["season"], 0);
    assert_eq!(
        res.body["skipped"],
        json!([
            { "season": 1, "reason": "already_requested" },
            { "season": 2, "reason": "available" },
        ])
    );
    // Nothing left to ask for: refused, with the reason.
    let res = s.request_seasons(&s.mia, &[2]).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "requests.already_available");
    let res = s.request_seasons(&s.noah, &[0, 1]).await;
    assert_eq!(res.code(), "requests.already_requested");
    // Mixed reasons with nothing left: the member's own request wins.
    let res = s.request_seasons(&s.noah, &[1, 2]).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "requests.already_requested");
}

/// D10 (maintainer 2026-11-02): asking again after a rejection waits for an
/// admin, even when the title runs for someone else; that request stays
/// untouched.
#[tokio::test]
async fn asking_again_after_a_rejection_goes_back_to_needs_you() {
    let s = setup().await;
    let mia = s.request_movie(&s.mia).await;
    let mia_id = mia.body["request"]["id"].as_i64().unwrap();
    let res = s
        .admin(post(
            "/api/v1/admin/approvals/reject",
            json!({ "requests": [{ "id": mia_id, "memberId": s.mia.id }], "reason": "Not now" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let noah = s.request_movie(&s.noah).await;
    assert_eq!(noah.body["request"]["state"], "approved");

    // Mia, now even in auto mode, asks again: pending, not attached.
    let res = s
        .admin(put(
            &format!("/api/v1/members/{}/entitlements", s.mia.id),
            json!({ "entitlements": entitlements("auto") }),
        ))
        .await;
    assert!(res.status.is_success(), "{}", res.raw);
    let again = s.request_movie(&s.mia).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.raw);
    assert_eq!(again.body["request"]["state"], "pending_approval");
    let inbox = s.admin(get("/api/v1/admin/approvals")).await;
    assert_eq!(inbox.body.as_array().unwrap().len(), 1);
    assert_eq!(inbox.body[0]["member"]["username"], "mia");
    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(acquisitions.body[0]["requests"], 1);
    let theirs = s.as_member(&s.noah, get("/api/v1/requests")).await;
    assert_eq!(theirs.body[0]["state"], "approved");
    assert!(!theirs.raw.contains("mia"), "{}", theirs.raw);
}

/// Spec 0003 (maintainer 2026-11-02): an admin pauses writes to a source;
/// approvals go on, their acquisitions are held and nothing reaches the
/// source until the writes are resumed.
#[tokio::test]
#[allow(clippy::used_underscore_binding)] // the fakes are kept alive there
#[allow(clippy::too_many_lines)] // one pause-hold-resume story
async fn paused_writes_hold_approved_requests_until_resumed() {
    let s = setup().await;
    let radarr = &s._servers[1];
    let writes_path = format!("/api/v1/services/{}/writes-paused", s.radarr);

    // Admins only.
    let res = s
        .as_member(&s.noah, put(&writes_path, json!({ "paused": true })))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);

    let res = s.admin(put(&writes_path, json!({ "paused": true }))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert!(res.body["writesPausedAt"].is_string());
    assert_eq!(res.body["writesPausedBy"], common::ADMIN_USER);
    let service = s
        .admin(get(&format!("/api/v1/services/{}", s.radarr)))
        .await;
    assert!(service.body["writesPausedAt"].is_string());
    // Pausing again changes nothing and is not logged twice.
    let again = s.admin(put(&writes_path, json!({ "paused": true }))).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.raw);
    assert_eq!(again.body["writesPausedAt"], res.body["writesPausedAt"]);

    // Approval still works; the acquisition is held.
    let noah = s.request_movie(&s.noah).await;
    assert_eq!(noah.status, StatusCode::CREATED, "{}", noah.raw);
    s.env.drain().await;
    assert_eq!(s.submit_jobs().await, 0);
    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(acquisitions.body[0]["state"], "pending");
    assert_eq!(acquisitions.body[0]["held"], true);
    let writes = radarr
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| matches!(r.method.as_str(), "POST" | "PUT"))
        .count();
    assert_eq!(writes, 0, "nothing written while paused");
    // The member sees it approved, as any acquisition not yet submitted.
    let mine = s.as_member(&s.noah, get("/api/v1/requests")).await;
    assert_eq!(mine.body[0]["state"], "approved");

    // Admins see what resuming would send out (the confirmation dialog).
    let held_path = format!("/api/v1/services/{}/held", s.radarr);
    let held = s.admin(get(&held_path)).await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.raw);
    assert_eq!(held.body["total"], 1);
    assert_eq!(held.body["preview"].as_array().unwrap().len(), 1);
    assert!(held.body["preview"][0]["title"].is_string(), "{}", held.raw);
    assert_eq!(held.body["preview"][0]["season"], Value::Null);
    let res = s.as_member(&s.noah, get(&held_path)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
    let res = s.admin(get("/api/v1/services/999/held")).await;
    assert_eq!(res.code(), "services.not_found");

    // A held acquisition is not failed: the retry does not bypass the pause.
    let id = acquisitions.body[0]["id"].as_i64().unwrap();
    let res = s
        .admin(post(
            &format!("/api/v1/admin/acquisitions/{id}/retry"),
            json!({}),
        ))
        .await;
    assert_eq!(res.code(), "acquisitions.not_failed");

    // Only sources have writes to pause.
    let tmdb: (i64,) =
        sqlx::query_as("SELECT id FROM service_instance WHERE adapter_type = 'tmdb'")
            .fetch_one(s.env.app.state.db.pool())
            .await
            .unwrap();
    let res = s
        .admin(put(
            &format!("/api/v1/services/{}/writes-paused", tmdb.0),
            json!({ "paused": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "sources.not_a_source");
    let res = s
        .admin(put(
            "/api/v1/services/999/writes-paused",
            json!({ "paused": true }),
        ))
        .await;
    assert_eq!(res.code(), "services.not_found");

    // Resuming sends the held acquisition out.
    let res = s.admin(put(&writes_path, json!({ "paused": false }))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["writesPausedAt"], Value::Null);
    assert_eq!(res.body["writesPausedBy"], Value::Null);
    assert_eq!(s.submit_jobs().await, 1, "resume enqueues requests.submit");
    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(acquisitions.body[0]["held"], false);
    let held = s.admin(get(&held_path)).await;
    assert_eq!(held.body["total"], 0, "nothing is held while writes go out");
    assert_eq!(held.body["preview"], json!([]));

    let audit = s.env.app.state.db.recent_audit(20).await.unwrap();
    let actions: Vec<&str> = audit
        .iter()
        .filter(|e| e.action.starts_with("services.writes_"))
        .map(|e| e.action.as_str())
        .collect();
    assert_eq!(
        actions,
        vec!["services.writes_resumed", "services.writes_paused"]
    );
    assert!(
        audit
            .iter()
            .filter(|e| e.action.starts_with("services.writes_"))
            .all(|e| e.actor_name.as_deref() == Some(common::ADMIN_USER)
                && e.target.as_deref() == Some("radarr"))
    );
    let resumed: Value = serde_json::from_str(audit[0].details.as_deref().unwrap()).unwrap();
    assert!(resumed["pausedSince"].is_string(), "{resumed}");
    assert_eq!(resumed["pausedBy"], common::ADMIN_USER);
}

/// The steps of a journey as `(step, status)` pairs.
fn steps(journey: &Value) -> Vec<(String, String)> {
    journey["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["step"].as_str().unwrap().to_owned(),
                s["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

/// Status journey (maintainer, 2026-11-02): every request item carries the
/// steps behind its chip; members never see another member or an admin's
/// name, admins see who decided.
#[tokio::test]
async fn journeys_show_the_way_without_other_members_names() {
    let s = setup().await;
    // Mia (approval) waits; Noah (auto) is approved on request.
    let mia = s.request_movie(&s.mia).await;
    let mia_id = mia.body["request"]["id"].as_i64().unwrap();
    let journey = &mia.body["request"]["items"][0]["journey"];
    assert_eq!(
        steps(journey),
        pairs(&[
            ("requested", "done"),
            ("awaiting_approval", "current"),
            ("getting_it", "upcoming"),
            ("arriving", "upcoming"),
            ("ready", "upcoming"),
        ])
    );
    assert_eq!(journey["steps"][1]["note"], "can_withdraw");
    assert!(journey["steps"][0]["at"].is_string());

    let noah = s.request_movie(&s.noah).await;
    let journey = &noah.body["request"]["items"][0]["journey"];
    assert_eq!(journey["steps"][1]["step"], "approved_automatically");
    assert_eq!(journey["steps"][1]["status"], "current");

    s.admin(post(
        "/api/v1/admin/approvals/approve",
        refs(&[(mia_id, s.mia.id)]),
    ))
    .await;

    // Members: approved, without the admin's name, the source or Noah.
    let mine = s.as_member(&s.mia, get("/api/v1/requests")).await;
    let journey = &mine.body[0]["items"][0]["journey"];
    assert_eq!(journey["steps"][1]["step"], "approved");
    assert_eq!(journey["steps"][1]["actor"], Value::Null);
    assert_eq!(journey["source"], Value::Null);
    for name in ["noah", common::ADMIN_USER, "radarr"] {
        assert!(!mine.raw.contains(name), "{name}: {}", mine.raw);
    }
    let theirs = s.as_member(&s.noah, get("/api/v1/requests")).await;
    for name in ["mia", common::ADMIN_USER, "radarr"] {
        assert!(!theirs.raw.contains(name), "{name}: {}", theirs.raw);
    }

    // Admins: who approved, and the source.
    let detail = s
        .admin(get(&format!("/api/v1/admin/requests/{mia_id}")))
        .await;
    let journey = &detail.body["request"]["items"][0]["journey"];
    assert_eq!(journey["steps"][1]["step"], "approved");
    assert_eq!(journey["steps"][1]["actor"], common::ADMIN_USER);
    assert_eq!(journey["source"], "radarr");
    let list = s.admin(get("/api/v1/admin/requests")).await;
    assert!(list.body[0]["items"][0]["journey"]["steps"].is_array());
    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(
        acquisitions.body[0]["journey"]["steps"][0]["step"],
        "approved"
    );
}

#[tokio::test]
async fn declined_journeys_end_with_the_reason_and_held_is_for_admins() {
    let s = setup().await;
    let mia = s.request_movie(&s.mia).await;
    let id = mia.body["request"]["id"].as_i64().unwrap();
    let res = s
        .admin(post(
            "/api/v1/admin/approvals/reject",
            json!({ "requests": [{ "id": id, "memberId": s.mia.id }], "reason": "Not in 4K" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let mine = s.as_member(&s.mia, get("/api/v1/requests")).await;
    let journey = &mine.body[0]["items"][0]["journey"];
    assert_eq!(
        steps(journey),
        pairs(&[("requested", "done"), ("declined", "failed")])
    );
    assert_eq!(journey["steps"][1]["detail"], "Not in 4K");
    assert_eq!(journey["steps"][1]["actor"], Value::Null);
    let detail = s.admin(get(&format!("/api/v1/admin/requests/{id}"))).await;
    assert_eq!(
        detail.body["request"]["items"][0]["journey"]["steps"][1]["actor"],
        common::ADMIN_USER
    );

    // Paused writes: admins see "held", the member an approved request.
    let res = s
        .admin(put(
            &format!("/api/v1/services/{}/writes-paused", s.radarr),
            json!({ "paused": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let noah = s.request_movie(&s.noah).await;
    let noah_id = noah.body["request"]["id"].as_i64().unwrap();
    let journey = &noah.body["request"]["items"][0]["journey"];
    assert!(
        !steps(journey).iter().any(|(step, _)| step == "held"),
        "{journey}"
    );
    assert_eq!(journey["steps"][1]["status"], "current");
    let detail = s
        .admin(get(&format!("/api/v1/admin/requests/{noah_id}")))
        .await;
    let journey = &detail.body["request"]["items"][0]["journey"];
    assert_eq!(journey["steps"][2]["step"], "held");
    assert_eq!(journey["steps"][2]["status"], "warning");
}
