//! Notifications (spec 0003 O1, O2): personal streams exercised by two
//! members (lesson 4), the admin stream, email preferences, and channels
//! whose secrets the web side seals but never returns.
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;
use common::media::{put, setup};
use common::services::{delete, patch};
use common::{elevate_in_place, get, post};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HOOK_TOKEN: &str = "very-secret-webhook-token";

async fn deliver_jobs(s: &common::media::Setup) -> i64 {
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM job WHERE kind = 'notifications.deliver'")
            .fetch_one(s.env.app.state.db.pool())
            .await
            .unwrap();
    n
}

fn kinds(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn streams_are_personal_and_the_admin_stream_is_separate() {
    let s = setup().await;
    let mia = s.request_movie(&s.mia).await;
    assert_eq!(mia.status, StatusCode::CREATED, "{}", mia.raw);
    let mia_request = mia.body["request"]["id"].as_i64().unwrap();
    // Noah requests the series automatically: nobody needs to hear.
    let noah = s.request_seasons(&s.noah, &[1]).await;
    assert_eq!(noah.status, StatusCode::CREATED, "{}", noah.raw);

    let admin = s.admin(get("/api/v1/admin/notifications")).await;
    assert_eq!(admin.status, StatusCode::OK, "{}", admin.raw);
    assert_eq!(kinds(&admin.body), vec!["approval_pending"]);
    assert_eq!(admin.body["items"][0]["member"], "mia");
    assert_eq!(admin.body["items"][0]["title"], "Inception");
    assert_eq!(admin.body["unread"], 1);

    let approved = s
        .admin(post(
            "/api/v1/admin/approvals/approve",
            json!({ "requests": [{ "id": mia_request, "memberId": s.mia.id }] }),
        ))
        .await;
    assert_eq!(approved.status, StatusCode::OK, "{}", approved.raw);

    let mine = s.as_member(&s.mia, get("/api/v1/notifications")).await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.raw);
    assert_eq!(kinds(&mine.body), vec!["request_approved"]);
    assert_eq!(mine.body["unread"], 1);
    assert_eq!(mine.body["items"][0]["requestId"], mia_request);
    assert_eq!(mine.body["items"][0]["read"], false);
    assert_eq!(mine.body["items"][0]["member"], Value::Null);
    let id = mine.body["items"][0]["id"].as_i64().unwrap();

    // Noah sees nothing of Mia's, and cannot mark hers read.
    // No channel, nothing to send: no delivery run is queued.
    assert_eq!(deliver_jobs(&s).await, 0);

    let theirs = s.as_member(&s.noah, get("/api/v1/notifications")).await;
    assert_eq!(theirs.body["items"], json!([]));
    assert!(!theirs.raw.contains("mia"));
    let marked = s
        .as_member(
            &s.noah,
            post("/api/v1/notifications/read", json!({ "ids": [id] })),
        )
        .await;
    assert_eq!(marked.status, StatusCode::OK, "{}", marked.raw);
    assert_eq!(marked.body["unread"], 0);
    let mine = s.as_member(&s.mia, get("/api/v1/notifications")).await;
    assert_eq!(mine.body["unread"], 1);

    let marked = s
        .as_member(&s.mia, post("/api/v1/notifications/read", json!({})))
        .await;
    assert_eq!(marked.body["unread"], 0);
    // Members do not touch the admin stream, nor admins a member's.
    let admin = s.admin(get("/api/v1/admin/notifications")).await;
    assert_eq!(admin.body["unread"], 1);
    let forbidden = s
        .as_member(&s.mia, get("/api/v1/admin/notifications"))
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
    let forbidden = s.admin(get("/api/v1/notifications")).await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);

    let read = s
        .admin(post(
            "/api/v1/admin/notifications/read",
            json!({ "ids": [] }),
        ))
        .await;
    assert_eq!(read.body["unread"], 1, "an empty list marks nothing");
    let read = s
        .admin(post("/api/v1/admin/notifications/read", json!({})))
        .await;
    assert_eq!(read.body["unread"], 0);
}

/// The admin bell shows how a request turned out, not just "done" (spec
/// 0008 slice 12): approval notifications carry the request's state now.
#[tokio::test]
async fn admin_approval_notifications_carry_the_requests_state_now() {
    let s = setup().await;
    let movie = s.request_movie(&s.mia).await;
    assert_eq!(movie.status, StatusCode::CREATED, "{}", movie.raw);
    let movie_id = movie.body["request"]["id"].as_i64().unwrap();
    let series = s.request_seasons(&s.mia, &[1]).await;
    let series_id = series.body["request"]["id"].as_i64().unwrap();

    let state_of = |page: &Value, id: i64| {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["requestId"] == id)
            .map(|n| n["requestState"].clone())
            .unwrap()
    };
    let admin = s.admin(get("/api/v1/admin/notifications")).await;
    assert_eq!(state_of(&admin.body, movie_id), "pending_approval");

    let res = s
        .admin(post(
            "/api/v1/admin/approvals/reject",
            json!({ "requests": [{ "id": series_id, "memberId": s.mia.id }], "reason": "Not now" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = s
        .admin(post(
            "/api/v1/admin/approvals/approve",
            json!({ "requests": [{ "id": movie_id, "memberId": s.mia.id }] }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);

    let admin = s.admin(get("/api/v1/admin/notifications")).await;
    assert_eq!(state_of(&admin.body, series_id), "rejected");
    let approved = state_of(&admin.body, movie_id);
    assert!(
        approved == "approved" || approved == "submitted",
        "{approved}"
    );
    // Members' notifications name the outcome in their kind already.
    let mine = s.as_member(&s.mia, get("/api/v1/notifications")).await;
    assert!(
        mine.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["requestState"].is_null()),
        "{}",
        mine.raw
    );
}

#[tokio::test]
async fn email_is_opt_in_for_member_kinds_only() {
    let s = setup().await;
    let prefs = s
        .as_member(&s.mia, get("/api/v1/me/notification-preferences"))
        .await;
    assert_eq!(prefs.status, StatusCode::OK, "{}", prefs.raw);
    assert_eq!(prefs.body["email"], json!([]));
    assert_eq!(prefs.body["emailAvailable"], false);
    assert_eq!(prefs.body["emailAddress"], "mia@example.org");

    let refused = s
        .as_member(
            &s.mia,
            put(
                "/api/v1/me/notification-preferences",
                json!({ "email": ["approval_pending"] }),
            ),
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.code(), "validation.notification_kind");

    let saved = s
        .as_member(
            &s.mia,
            put(
                "/api/v1/me/notification-preferences",
                json!({ "email": ["request_available", "request_rejected"] }),
            ),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.raw);
    assert_eq!(
        saved.body["email"],
        json!(["request_available", "request_rejected"])
    );
    // Noah's choices are his own.
    let noah = s
        .as_member(&s.noah, get("/api/v1/me/notification-preferences"))
        .await;
    assert_eq!(noah.body["email"], json!([]));
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one walk through a channel's life, in order
async fn channels_seal_their_secrets_and_test_through_the_worker() {
    let s = setup().await;
    elevate_in_place(&s.env.app, &s.env.admin).await;
    let hook = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/api/webhooks/9/{HOOK_TOKEN}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&hook)
        .await;
    let url = format!("{}/api/webhooks/9/{HOOK_TOKEN}", hook.uri());

    let bad = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({ "kind": "discord", "name": "ops", "secret": "https://example.org/nope" }),
        ))
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bad.code(), "validation.discord_url");
    let member_kind = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({ "kind": "discord", "name": "ops", "secret": url, "adminEvents": ["request_approved"] }),
        ))
        .await;
    assert_eq!(member_kind.code(), "validation.notification_kind");

    let created = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({
                "kind": "discord",
                "name": "ops",
                "secret": url,
                "adminEvents": ["approval_pending", "acquisition_failed"],
            }),
        ))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.raw);
    assert!(!created.raw.contains(HOOK_TOKEN), "{}", created.raw);
    assert_eq!(created.body["host"], "127.0.0.1");
    assert_eq!(created.body["hasSecret"], true);
    let id = created.body["id"].as_i64().unwrap();

    let email = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({
                "kind": "email",
                "name": "mail",
                "email": {
                    "host": "smtp.example.org", "port": 587, "security": "starttls",
                    "username": "starr", "fromAddress": "starr@example.org",
                },
                "secret": "smtp-password-0815",
            }),
        ))
        .await;
    assert_eq!(email.status, StatusCode::CREATED, "{}", email.raw);
    assert!(!email.raw.contains("smtp-password-0815"));
    assert_eq!(email.body["email"]["host"], "smtp.example.org");
    let again = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({
                "kind": "email",
                "name": "mail 2",
                "email": { "host": "x", "port": 25, "security": "none", "fromAddress": "a@b.org" },
            }),
        ))
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.code(), "notifications.channel_taken");
    let invalid = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({
                "kind": "email",
                "name": "mail 3",
                "email": { "host": "x", "port": 25, "security": "none", "fromAddress": "nope" },
            }),
        ))
        .await;
    assert_eq!(invalid.code(), "validation.from_address");
    // With an email server, members can opt in.
    let prefs = s
        .as_member(&s.mia, get("/api/v1/me/notification-preferences"))
        .await;
    assert_eq!(prefs.body["emailAvailable"], true);

    // Members never see channels.
    let forbidden = s
        .as_member(&s.mia, get("/api/v1/admin/notification-channels"))
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);

    // The test runs in the worker, which alone opens the URL.
    let test = s
        .admin(post(
            &format!("/api/v1/admin/notification-channels/{id}/test"),
            json!({}),
        ))
        .await;
    assert_eq!(test.status, StatusCode::ACCEPTED, "{}", test.raw);
    s.env.drain().await;
    let list = s.admin(get("/api/v1/admin/notification-channels")).await;
    let ops = list
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap();
    assert_eq!(ops["lastOk"], true, "{}", list.raw);
    assert!(!list.raw.contains(HOOK_TOKEN));

    // A pending approval reaches Discord right away: the request queues a
    // delivery run, without waiting for the minute schedule.
    s.request_movie(&s.mia).await;
    assert_eq!(deliver_jobs(&s).await, 1);
    s.env.drain().await;
    let received = hook.received_requests().await.unwrap();
    assert_eq!(received.len(), 2, "test + approval");
    let body: Value = serde_json::from_slice(&received[1].body).unwrap();
    assert!(
        body["embeds"][0]["description"]
            .as_str()
            .unwrap()
            .contains("mia asked for Inception")
    );

    let disabled = s
        .admin(patch(
            &format!("/api/v1/admin/notification-channels/{id}"),
            json!({ "enabled": false, "adminEvents": ["import_blocked", "acquisition_failed"] }),
        ))
        .await;
    assert_eq!(disabled.status, StatusCode::OK, "{}", disabled.raw);
    assert_eq!(disabled.body["enabled"], false);
    assert_eq!(
        disabled.body["adminEvents"],
        json!(["acquisition_failed", "import_blocked"])
    );
    let wrong = s
        .admin(patch(
            &format!("/api/v1/admin/notification-channels/{id}"),
            json!({ "email": { "host": "x", "port": 25, "security": "none", "fromAddress": "a@b.org" } }),
        ))
        .await;
    assert_eq!(wrong.code(), "validation.smtp_settings_required");

    let gone = s
        .admin(delete(&format!("/api/v1/admin/notification-channels/{id}")))
        .await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT);
    let missing = s
        .admin(post(
            &format!("/api/v1/admin/notification-channels/{id}/test"),
            json!({}),
        ))
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.code(), "notifications.channel_not_found");
}
#[tokio::test]
async fn patching_secrets_keeps_them_secret_and_login_complete() {
    let s = setup().await;
    elevate_in_place(&s.env.app, &s.env.admin).await;
    let half = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({
                "kind": "email",
                "name": "mail",
                "email": { "host": "smtp.example.org", "port": 587, "security": "starttls",
                           "username": "starr", "fromAddress": "starr@example.org" },
            }),
        ))
        .await;
    assert_eq!(half.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(half.code(), "validation.smtp_login_incomplete");
    let email = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({
                "kind": "email",
                "name": "mail",
                "email": { "host": "smtp.example.org", "port": 587, "security": "starttls",
                           "username": "starr", "fromAddress": "starr@example.org" },
                "secret": "smtp-password-0815",
            }),
        ))
        .await;
    assert_eq!(email.status, StatusCode::CREATED, "{}", email.raw);
    let email_id = email.body["id"].as_i64().unwrap();
    let path = format!("/api/v1/admin/notification-channels/{email_id}");

    // Removing the password while a username is set would send without login.
    let cleared = s.admin(patch(&path, json!({ "secret": "" }))).await;
    assert_eq!(cleared.code(), "validation.smtp_login_incomplete");
    // Dropping both is a relay without login.
    let relay = s
        .admin(patch(
            &path,
            json!({
                "secret": "",
                "email": { "host": "relay.lan", "port": 25, "security": "none",
                           "fromAddress": "starr@example.org" },
            }),
        ))
        .await;
    assert_eq!(relay.status, StatusCode::OK, "{}", relay.raw);
    assert_eq!(relay.body["hasSecret"], false);
    assert_eq!(relay.body["email"]["username"], Value::Null);

    let hook = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({ "kind": "webhook", "name": "hook", "secret": "https://one.example.org/h/first-token-123" }),
        ))
        .await;
    let hook_path = format!(
        "/api/v1/admin/notification-channels/{}",
        hook.body["id"].as_i64().unwrap()
    );
    let blank = s.admin(patch(&hook_path, json!({ "secret": "  " }))).await;
    assert_eq!(blank.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(blank.code(), "validation.webhook_url");
    let moved = s
        .admin(patch(
            &hook_path,
            json!({ "secret": "https://two.example.org/h/second-token-456" }),
        ))
        .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.raw);
    assert_eq!(moved.body["host"], "two.example.org");
    assert!(!moved.raw.contains("second-token-456"));
}

#[tokio::test]
async fn changing_where_a_channel_sends_needs_an_elevation() {
    let s = setup().await;
    elevate_in_place(&s.env.app, &s.env.admin).await;
    let hook = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({ "kind": "webhook", "name": "hook", "secret": "https://one.example.org/h/first-token-123" }),
        ))
        .await;
    assert_eq!(hook.status, StatusCode::CREATED, "{}", hook.raw);
    let hook_path = format!(
        "/api/v1/admin/notification-channels/{}",
        hook.body["id"].as_i64().unwrap()
    );
    // The elevation for adding it has run out.
    s.env.app.clock.advance(chrono::TimeDelta::minutes(6));
    let moved = s
        .admin(patch(
            &hook_path,
            json!({ "secret": "https://evil.example.org/h/collect" }),
        ))
        .await;
    assert_eq!(moved.status, StatusCode::FORBIDDEN, "{}", moved.raw);
    assert_eq!(moved.code(), "auth.elevation_required");
    // Name, switch and events are no target change.
    let renamed = s
        .admin(patch(
            &hook_path,
            json!({ "name": "alerts", "enabled": false, "adminEvents": ["import_blocked"] }),
        ))
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.raw);
    assert_eq!(renamed.body["host"], "one.example.org");

    elevate_in_place(&s.env.app, &s.env.admin).await;
    let email = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({
                "kind": "email",
                "name": "mail",
                "email": { "host": "smtp.example.org", "port": 587, "security": "starttls",
                           "fromAddress": "starr@example.org" },
            }),
        ))
        .await;
    assert_eq!(email.status, StatusCode::CREATED, "{}", email.raw);
    s.env.app.clock.advance(chrono::TimeDelta::minutes(6));
    let email_path = format!(
        "/api/v1/admin/notification-channels/{}",
        email.body["id"].as_i64().unwrap()
    );
    let same = json!({ "email": { "host": "smtp.example.org", "port": 587, "security": "starttls",
                                  "fromAddress": "starr@example.org" } });
    let res = s.admin(patch(&email_path, same)).await;
    assert_eq!(res.status, StatusCode::OK, "same settings: {}", res.raw);
    let other = json!({ "email": { "host": "smtp.evil.example", "port": 587, "security": "starttls",
                                   "fromAddress": "starr@example.org" } });
    let res = s.admin(patch(&email_path, other)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
    assert_eq!(res.code(), "auth.elevation_required");
}

#[tokio::test]
async fn the_smtp_password_never_follows_the_settings_to_another_server() {
    let s = setup().await;
    elevate_in_place(&s.env.app, &s.env.admin).await;
    let email = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({
                "kind": "email",
                "name": "mail",
                "email": { "host": "smtp.example.org", "port": 587, "security": "starttls",
                           "username": "starr", "fromAddress": "starr@example.org" },
                "secret": "smtp-password-0815",
            }),
        ))
        .await;
    assert_eq!(email.status, StatusCode::CREATED, "{}", email.raw);
    let path = format!(
        "/api/v1/admin/notification-channels/{}",
        email.body["id"].as_i64().unwrap()
    );
    let settings = |host: &str, port: u16| {
        json!({ "host": host, "port": port, "security": "starttls",
                "username": "starr", "fromAddress": "starr@example.org" })
    };
    for (host, port) in [("smtp.evil.example", 587), ("smtp.example.org", 2525)] {
        let res = s
            .admin(patch(&path, json!({ "email": settings(host, port) })))
            .await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
        assert_eq!(res.code(), "validation.credential_required_for_new_origin");
    }
    // Another sender on the same server keeps the password; a new server
    // with its password is fine.
    let mut same = settings("smtp.example.org", 587);
    same["fromName"] = json!("Starr at home");
    let res = s.admin(patch(&path, json!({ "email": same }))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = s
        .admin(patch(
            &path,
            json!({ "email": settings("smtp.other.example", 587), "secret": "other-pass" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
}

#[tokio::test]
async fn adding_a_channel_needs_an_elevation() {
    let s = setup().await;
    s.env.app.clock.advance(chrono::TimeDelta::minutes(10));
    let res = s
        .admin(post(
            "/api/v1/admin/notification-channels",
            json!({ "kind": "webhook", "name": "hook", "secret": "https://evil.example.org/h/collect" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
    assert_eq!(res.code(), "auth.elevation_required");
}
