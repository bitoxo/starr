//! Members, entitlements, invitations, password links and member
//! self-service (spec 0002 slice 5, ADR 0006). Personal data is always tested
//! with two members (lesson 4).
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{Method, StatusCode};
use chrono::TimeDelta;
use common::{
    ADMIN_PASSWORD, ADMIN_USER, Req, TestApp, TestResponse, get, login, post, setup_admin, test_app,
};
use serde_json::{Value, json};

const PASSWORD: &str = "member password 1";

fn put(path: &str, body: Value) -> Req {
    Req::new(Method::PUT, path, Some(body))
}

fn patch(path: &str, body: Value) -> Req {
    Req::new(Method::PATCH, path, Some(body))
}

fn delete(path: &str, body: Option<Value>) -> Req {
    Req::new(Method::DELETE, path, body)
}

/// The token at the end of a link (`…/set-password/<token>`).
fn token_of(link: &Value) -> String {
    link["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned()
}

async fn create_member(app: &TestApp, admin: &str, username: &str) -> (i64, String) {
    let res = post(
        "/api/v1/members",
        json!({ "username": username, "email": format!("{username}@example.org") }),
    )
    .cookie(admin)
    .send(app)
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    (
        res.body["member"]["id"].as_i64().unwrap(),
        token_of(&res.body["setPasswordLink"]),
    )
}

async fn set_password(app: &TestApp, token: &str, password: &str) -> TestResponse {
    post(
        &format!("/api/v1/password-tokens/{token}"),
        json!({ "password": password }),
    )
    .send(app)
    .await
}

/// Admin session plus two members who set their password (and are logged in).
struct World {
    app: TestApp,
    admin: String,
    mia: (i64, String),
    noah: (i64, String),
}

async fn world() -> World {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let mut members = Vec::new();
    for name in ["mia", "noah"] {
        let (id, token) = create_member(&app, &admin, name).await;
        let res = set_password(&app, &token, PASSWORD).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        members.push((id, res.session_cookie().unwrap()));
    }
    let noah = members.pop().unwrap();
    let mia = members.pop().unwrap();
    World {
        app,
        admin,
        mia,
        noah,
    }
}

async fn status_of(app: &TestApp, cookie: &str) -> StatusCode {
    get("/api/v1/me").cookie(cookie).send(app).await.status
}

// ---------------------------------------------------------------- admin CRUD

#[tokio::test]
async fn admin_creates_member_and_member_sets_password_via_link() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let res = post(
        "/api/v1/members",
        json!({
            "username": "Mia",
            "email": "Mia@Example.org",
            "displayName": "Mia M.",
            "entitlements": [{ "category": "movies", "enabled": true, "requestMode": "auto" }],
        }),
    )
    .cookie(&admin)
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let member = &res.body["member"];
    assert_eq!(member["username"], "mia");
    assert_eq!(member["email"], "mia@example.org");
    assert_eq!(member["status"], "active");
    assert_eq!(member["passwordSet"], false);
    assert_eq!(
        member["entitlements"],
        json!([
            { "category": "movies", "enabled": true, "requestMode": "auto" },
            { "category": "series", "enabled": false, "requestMode": "approval" },
            { "category": "music", "enabled": false, "requestMode": "approval" },
            { "category": "adult", "enabled": false, "requestMode": "approval" },
        ])
    );
    let link = &res.body["setPasswordLink"];
    assert!(
        link["url"]
            .as_str()
            .unwrap()
            .starts_with("https://starr.test/set-password/"),
        "{link}"
    );
    let token = token_of(link);

    // No login before the password is set.
    assert_eq!(
        login(&app, "mia", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    let res = set_password(&app, &token, "short").await;
    assert_eq!(res.code(), "auth.password_too_short");
    let res = set_password(&app, &token, PASSWORD).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["kind"], "member");
    let cookie = res.session_cookie().unwrap();
    assert_eq!(status_of(&app, &cookie).await, StatusCode::OK);
    // Single use.
    assert_eq!(
        set_password(&app, &token, PASSWORD).await.code(),
        "password_token.used"
    );
    assert_eq!(login(&app, "mia", PASSWORD).await.status, StatusCode::OK);

    let audit = app.state.db.recent_audit(20).await.unwrap();
    assert!(
        audit
            .iter()
            .any(|e| e.action == "member.created" && e.actor_name.as_deref() == Some("root"))
    );
}

#[tokio::test]
async fn create_member_validates_input_and_uniqueness() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    create_member(&app, &admin, "mia").await;
    for (body, code) in [
        (
            json!({ "username": "_x", "email": "x@example.org" }),
            "validation.username_invalid",
        ),
        (
            json!({ "username": "xavier", "email": "nope" }),
            "validation.email_invalid",
        ),
        (
            json!({ "username": "MIA", "email": "other@example.org" }),
            "validation.username_taken",
        ),
        (
            json!({ "username": "root", "email": "other@example.org" }),
            "validation.username_taken",
        ),
        (
            json!({ "username": "other", "email": "MIA@example.org" }),
            "validation.email_taken",
        ),
    ] {
        let res = post("/api/v1/members", body.clone())
            .cookie(&admin)
            .send(&app)
            .await;
        assert_eq!(res.code(), code, "{body}: {}", res.raw);
    }
}

#[tokio::test]
async fn member_endpoints_are_admin_only() {
    let w = world().await;
    let mia = &w.mia.1;
    for req in [
        get("/api/v1/members"),
        get(&format!("/api/v1/members/{}", w.noah.0)),
        post(&format!("/api/v1/members/{}/suspend", w.noah.0), json!({})),
        get("/api/v1/invitations"),
        post("/api/v1/invitations", json!({})),
    ] {
        let res = req.cookie(mia).send(&w.app).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
    }
    // And the member area is member-only.
    let res = get("/api/v1/me").cookie(&w.admin).send(&w.app).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn list_filters_and_shows_status() {
    let w = world().await;
    post(&format!("/api/v1/members/{}/suspend", w.mia.0), json!({}))
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    let all = get("/api/v1/members").cookie(&w.admin).send(&w.app).await;
    assert_eq!(all.body.as_array().unwrap().len(), 2);
    let suspended = get("/api/v1/members?status=suspended")
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    let names: Vec<&str> = suspended
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["username"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["mia"]);
    let search = get("/api/v1/members?q=NOAH")
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    assert_eq!(search.body[0]["username"], "noah");
    assert_eq!(search.body.as_array().unwrap().len(), 1);
}

// ---------------------------------------------------------------- suspension

#[tokio::test]
async fn suspension_ends_the_members_sessions_immediately() {
    let w = world().await;
    // A second session for Mia.
    let second = login(&w.app, "mia", PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    let detail = get(&format!("/api/v1/members/{}", w.mia.0))
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    assert_eq!(detail.body["sessions"].as_array().unwrap().len(), 2);

    let res = post(&format!("/api/v1/members/{}/suspend", w.mia.0), json!({}))
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["status"], "suspended");
    assert_eq!(res.body["sessions"], json!([]));
    assert_eq!(status_of(&w.app, &w.mia.1).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status_of(&w.app, &second).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        login(&w.app, "mia", PASSWORD).await.code(),
        "auth.invalid_credentials"
    );
    // The other member is untouched.
    assert_eq!(status_of(&w.app, &w.noah.1).await, StatusCode::OK);

    let again = post(&format!("/api/v1/members/{}/suspend", w.mia.0), json!({}))
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    assert_eq!(again.code(), "member.not_active");

    let res = post(
        &format!("/api/v1/members/{}/reactivate", w.mia.0),
        json!({}),
    )
    .cookie(&w.admin)
    .send(&w.app)
    .await;
    assert_eq!(res.body["status"], "active");
    assert_eq!(login(&w.app, "mia", PASSWORD).await.status, StatusCode::OK);
}

// ---------------------------------------------------------------- deletion

#[tokio::test]
async fn member_who_never_used_starr_is_deleted_outright() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let (id, token) = create_member(&app, &admin, "mia").await;

    let wrong = delete(
        &format!("/api/v1/members/{id}"),
        Some(json!({ "username": "noah" })),
    )
    .cookie(&admin)
    .send(&app)
    .await;
    assert_eq!(wrong.code(), "validation.confirmation_mismatch");

    let res = delete(
        &format!("/api/v1/members/{id}"),
        Some(json!({ "username": "mia" })),
    )
    .cookie(&admin)
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(
        res.body,
        json!({ "outcome": "deleted", "graceUntil": null })
    );
    let gone = get(&format!("/api/v1/members/{id}"))
        .cookie(&admin)
        .send(&app)
        .await;
    assert_eq!(gone.code(), "member.not_found");
    // The link died with the member, and the name is free again.
    assert_eq!(
        set_password(&app, &token, PASSWORD).await.code(),
        "password_token.not_found"
    );
    create_member(&app, &admin, "mia").await;
}

#[tokio::test]
async fn deletion_with_grace_period_restore_and_tombstone() {
    let w = world().await;
    let path = format!("/api/v1/members/{}", w.mia.0);
    let res = delete(&path, Some(json!({ "username": "mia" })))
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["outcome"], "scheduled");
    assert_eq!(res.body["graceUntil"], "2026-12-01T12:00:00Z");
    assert_eq!(status_of(&w.app, &w.mia.1).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status_of(&w.app, &w.noah.1).await, StatusCode::OK);

    let detail = get(&path).cookie(&w.admin).send(&w.app).await;
    assert_eq!(detail.body["status"], "retired");
    assert_eq!(detail.body["deletion"]["requestedBy"], "root");
    assert_eq!(
        login(&w.app, "mia", PASSWORD).await.code(),
        "auth.invalid_credentials"
    );

    // Restore within the grace period: back as suspended.
    w.app.clock.advance(TimeDelta::days(10));
    let res = post(&format!("{path}/restore"), json!({}))
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["status"], "suspended");
    assert_eq!(res.body["deletion"], Value::Null);

    // Delete again and let the grace period run out.
    delete(&path, Some(json!({ "username": "mia" })))
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    w.app.clock.advance(TimeDelta::days(30));
    // The admin's session idled out meanwhile.
    let admin = login(&w.app, ADMIN_USER, ADMIN_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    let late = post(&format!("{path}/restore"), json!({}))
        .cookie(&admin)
        .send(&w.app)
        .await;
    assert_eq!(late.code(), "member.not_restorable");
    let tombstoned = w
        .app
        .state
        .db
        .tombstone_due_members(w.app.clock.now())
        .await
        .unwrap();
    assert_eq!(tombstoned, ["mia"]);

    // Tombstone: unlisted, name reserved, email free, the other member intact.
    assert_eq!(
        get(&path).cookie(&admin).send(&w.app).await.code(),
        "member.not_found"
    );
    let list = get("/api/v1/members").cookie(&admin).send(&w.app).await;
    assert_eq!(list.body.as_array().unwrap().len(), 1);
    assert_eq!(list.body[0]["username"], "noah");
    let res = post(
        "/api/v1/members",
        json!({ "username": "mia", "email": "new@example.org" }),
    )
    .cookie(&admin)
    .send(&w.app)
    .await;
    assert_eq!(res.code(), "validation.username_taken");
    let res = post(
        "/api/v1/members",
        json!({ "username": "mia2", "email": "mia@example.org" }),
    )
    .cookie(&admin)
    .send(&w.app)
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(login(&w.app, "noah", PASSWORD).await.status, StatusCode::OK);
}

// ---------------------------------------------------------------- entitlements

#[tokio::test]
async fn entitlements_are_per_member_and_admin_only() {
    let w = world().await;
    let body = json!({ "entitlements": [
        { "category": "adult", "enabled": true, "requestMode": "approval" },
        { "category": "series", "enabled": true, "requestMode": "auto" },
    ]});
    // Members cannot grant themselves anything, adult included.
    let res = put(
        &format!("/api/v1/members/{}/entitlements", w.mia.0),
        body.clone(),
    )
    .cookie(&w.mia.1)
    .send(&w.app)
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = patch(
        "/api/v1/me",
        json!({ "displayName": "Mia", "entitlements": body["entitlements"] }),
    )
    .cookie(&w.mia.1)
    .send(&w.app)
    .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    let me = get("/api/v1/me").cookie(&w.mia.1).send(&w.app).await;
    assert!(
        me.body["entitlements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["enabled"] == false)
    );

    let res = put(&format!("/api/v1/members/{}/entitlements", w.mia.0), body)
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(
        res.body[3],
        json!({ "category": "adult", "enabled": true, "requestMode": "approval" })
    );

    let mia = get("/api/v1/me").cookie(&w.mia.1).send(&w.app).await;
    assert_eq!(mia.body["entitlements"][1]["enabled"], true);
    let noah = get("/api/v1/me").cookie(&w.noah.1).send(&w.app).await;
    assert!(
        noah.body["entitlements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["enabled"] == false)
    );

    let audit = w.app.state.db.recent_audit(5).await.unwrap();
    assert_eq!(audit[0].action, "member.entitlements_updated");
    assert_eq!(audit[0].target.as_deref(), Some("mia"));
}

#[tokio::test]
async fn member_detail_says_since_when_nsfw_is_on() {
    let w = world().await;
    let path = format!("/api/v1/members/{}", w.mia.0);
    let detail = get(&path).cookie(&w.admin).send(&w.app).await;
    assert!(detail.body["adultSince"].is_null(), "{}", detail.raw);

    let on = json!({ "entitlements": [
        { "category": "adult", "enabled": true, "requestMode": "approval" },
    ]});
    let res = put(&format!("{path}/entitlements"), on)
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let detail = get(&path).cookie(&w.admin).send(&w.app).await;
    let since = detail.body["adultSince"].as_str().unwrap().to_owned();

    // Changing another category keeps the time; the other member has none.
    let more = json!({ "entitlements": [
        { "category": "adult", "enabled": true, "requestMode": "auto" },
        { "category": "movies", "enabled": true, "requestMode": "auto" },
    ]});
    put(&format!("{path}/entitlements"), more)
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    let detail = get(&path).cookie(&w.admin).send(&w.app).await;
    assert_eq!(detail.body["adultSince"], since);
    let noah = get(&format!("/api/v1/members/{}", w.noah.0))
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    assert!(noah.body["adultSince"].is_null());
}

// ---------------------------------------------------------------- password links

#[tokio::test]
async fn password_reset_link_replaces_older_links_and_ends_sessions() {
    let w = world().await;
    let path = format!("/api/v1/members/{}/password-reset", w.mia.0);
    let first = post(&path, json!({})).cookie(&w.admin).send(&w.app).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.raw);
    let second = post(&path, json!({})).cookie(&w.admin).send(&w.app).await;
    assert_eq!(
        set_password(&w.app, &token_of(&first.body), "first new password")
            .await
            .code(),
        "password_token.not_found"
    );
    // The old password works until the link is used.
    assert_eq!(login(&w.app, "mia", PASSWORD).await.status, StatusCode::OK);

    let res = set_password(&w.app, &token_of(&second.body), "second new password").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(status_of(&w.app, &w.mia.1).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status_of(&w.app, &w.noah.1).await, StatusCode::OK);
    assert_eq!(
        login(&w.app, "mia", "second new password").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn password_links_expire_after_seven_days() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let (_, token) = create_member(&app, &admin, "mia").await;
    app.clock.advance(TimeDelta::days(7));
    assert_eq!(
        set_password(&app, &token, PASSWORD).await.code(),
        "password_token.expired"
    );
}

/// The set-password page checks its link on load (spec 0008 AUTH-03): the
/// same answers as using it, without touching it, and nothing about whose
/// link it is.
#[tokio::test]
async fn password_links_can_be_checked_before_use() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let (_, token) = create_member(&app, &admin, "mia").await;
    let check = |token: String| {
        let app = &app;
        async move {
            get(&format!("/api/v1/password-tokens/{token}"))
                .send(app)
                .await
        }
    };
    let res = check(token.clone()).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert!(res.body["expiresAt"].is_string(), "{}", res.raw);
    assert_eq!(res.body.as_object().unwrap().len(), 1, "{}", res.raw);
    assert!(!res.raw.contains("mia"));
    assert_eq!(res.headers["referrer-policy"], "same-origin");
    // Looking does not use it up.
    assert_eq!(check(token.clone()).await.status, StatusCode::OK);
    assert_eq!(
        check("not-a-real-token".to_owned()).await.code(),
        "password_token.not_found"
    );
    let res = get(&format!("/api/v1/password-tokens/{token}"))
        .header("sec-fetch-site", "cross-site")
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);

    let res = set_password(&app, &token, PASSWORD).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(check(token).await.code(), "password_token.used");

    let (_, late) = create_member(&app, &admin, "noah").await;
    app.clock.advance(TimeDelta::days(7));
    assert_eq!(check(late).await.code(), "password_token.expired");
}

#[tokio::test]
async fn suspension_voids_password_links_and_blocks_new_ones() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let (id, token) = create_member(&app, &admin, "mia").await;
    post(&format!("/api/v1/members/{id}/suspend"), json!({}))
        .cookie(&admin)
        .send(&app)
        .await;
    assert_eq!(
        set_password(&app, &token, PASSWORD).await.code(),
        "password_token.not_found"
    );
    let res = post(&format!("/api/v1/members/{id}/password-reset"), json!({}))
        .cookie(&admin)
        .send(&app)
        .await;
    assert_eq!(res.code(), "member.not_active");
}

// ---------------------------------------------------------------- invitations

async fn invite(app: &TestApp, admin: &str, body: Value) -> (i64, String) {
    let res = post("/api/v1/invitations", body)
        .cookie(admin)
        .send(app)
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    (
        res.body["invitation"]["id"].as_i64().unwrap(),
        token_of(&res.body["link"]),
    )
}

async fn accept(app: &TestApp, token: &str, body: Value) -> TestResponse {
    post(&format!("/api/v1/invitations/{token}/accept"), body)
        .send(app)
        .await
}

#[tokio::test]
async fn invitation_creates_member_with_presets_and_logs_in() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let (id, token) = invite(
        &app,
        &admin,
        json!({
            "username": "mia",
            "expiresInDays": 3,
            "entitlements": [
                { "category": "music", "enabled": true, "requestMode": "auto" },
                { "category": "adult", "enabled": true, "requestMode": "approval" },
            ],
        }),
    )
    .await;

    let preview = get(&format!("/api/v1/invitations/{token}"))
        .send(&app)
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.raw);
    assert_eq!(
        preview.body,
        json!({ "presetUsername": "mia", "presetEmail": null, "expiresAt": "2026-11-04T12:00:00Z" })
    );

    // Email is required when not preset.
    let res = accept(&app, &token, json!({ "password": PASSWORD })).await;
    assert_eq!(res.code(), "validation.email_invalid");
    let res = accept(
        &app,
        &token,
        json!({ "username": "ignored", "email": "mia@example.org", "password": PASSWORD }),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(res.body["username"], "mia");
    let cookie = res.session_cookie().unwrap();
    let me = get("/api/v1/me").cookie(&cookie).send(&app).await;
    assert_eq!(me.body["entitlements"][2]["enabled"], true);
    // The member's own profile names the private category only once
    // unlocked (spec 0007); the admin sees the preset.
    assert!(!me.raw.contains("adult"), "{}", me.raw);
    let members = get("/api/v1/members").cookie(&admin).send(&app).await;
    let mia = members
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["username"] == "mia")
        .unwrap()
        .clone();
    assert_eq!(mia["entitlements"][3]["category"], "adult", "{mia}");
    assert_eq!(mia["entitlements"][3]["enabled"], true, "{mia}");

    // Single use.
    let again = accept(
        &app,
        &token,
        json!({ "email": "x@example.org", "password": PASSWORD }),
    )
    .await;
    assert_eq!(again.code(), "invitation.used");
    let list = get("/api/v1/invitations").cookie(&admin).send(&app).await;
    assert_eq!(list.body[0]["id"], id);
    assert_eq!(list.body[0]["state"], "accepted");
    assert_eq!(list.body[0]["usedBy"], "mia");
}

#[tokio::test]
async fn expired_revoked_and_unknown_invitations_are_refused() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let (revoked, revoked_token) = invite(&app, &admin, json!({})).await;
    let (_, expiring_token) = invite(&app, &admin, json!({ "expiresInDays": 1 })).await;

    let res = post(&format!("/api/v1/invitations/{revoked}/revoke"), json!({}))
        .cookie(&admin)
        .send(&app)
        .await;
    assert_eq!(res.body["state"], "revoked");
    let body = json!({ "username": "mia", "email": "mia@example.org", "password": PASSWORD });
    assert_eq!(
        accept(&app, &revoked_token, body.clone()).await.code(),
        "invitation.revoked"
    );
    let again = post(&format!("/api/v1/invitations/{revoked}/revoke"), json!({}))
        .cookie(&admin)
        .send(&app)
        .await;
    assert_eq!(again.code(), "invitation.not_pending");

    app.clock.advance(TimeDelta::days(1));
    assert_eq!(
        accept(&app, &expiring_token, body.clone()).await.code(),
        "invitation.expired"
    );
    let unknown = accept(&app, &"x".repeat(43), body).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.code(), "invitation.not_found");
    let members = get("/api/v1/members").cookie(&admin).send(&app).await;
    assert_eq!(members.body, json!([]));
}

#[tokio::test]
async fn invitation_presets_are_validated() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    for (body, code) in [
        (json!({ "expiresInDays": 0 }), "validation.expiry_invalid"),
        (json!({ "expiresInDays": 31 }), "validation.expiry_invalid"),
        (json!({ "username": "root" }), "validation.username_taken"),
        (json!({ "username": "-bad" }), "validation.username_invalid"),
        (json!({ "email": "bad" }), "validation.email_invalid"),
    ] {
        let res = post("/api/v1/invitations", body.clone())
            .cookie(&admin)
            .send(&app)
            .await;
        assert_eq!(res.code(), code, "{body}");
    }
}

#[tokio::test]
async fn token_endpoints_require_our_origin_and_refuse_cross_site_previews() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let (_, token) = invite(&app, &admin, json!({})).await;
    let body = json!({ "username": "mia", "email": "mia@example.org", "password": PASSWORD });
    let res = post(&format!("/api/v1/invitations/{token}/accept"), body)
        .origin(Some("https://evil.example"))
        .send(&app)
        .await;
    assert_eq!(res.code(), "auth.origin_mismatch");
    let res = post(
        &format!("/api/v1/password-tokens/{token}"),
        json!({ "password": PASSWORD }),
    )
    .origin(None)
    .send(&app)
    .await;
    assert_eq!(res.code(), "auth.origin_mismatch");
    let res = get(&format!("/api/v1/invitations/{token}"))
        .header("sec-fetch-site", "cross-site")
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = get(&format!("/api/v1/invitations/{token}"))
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.headers["referrer-policy"], "same-origin");
}

#[tokio::test]
async fn failed_accept_leaves_the_invitation_usable() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let (_, token) = invite(&app, &admin, json!({})).await;
    let conflicting =
        json!({ "username": "root", "email": "mia@example.org", "password": PASSWORD });
    assert_eq!(
        accept(&app, &token, conflicting).await.code(),
        "validation.username_taken"
    );
    let ok = json!({ "username": "mia", "email": "mia@example.org", "password": PASSWORD });
    assert_eq!(accept(&app, &token, ok).await.status, StatusCode::CREATED);
}

#[tokio::test]
async fn token_failures_do_not_block_logins() {
    let app = test_app().await;
    setup_admin(&app).await;
    for i in 0..25 {
        get(&format!("/api/v1/invitations/{i:0>43}"))
            .send(&app)
            .await;
    }
    assert_eq!(
        login(&app, ADMIN_USER, ADMIN_PASSWORD).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn token_endpoints_are_rate_limited_per_ip() {
    let app = test_app().await;
    for i in 0..20 {
        let res = get(&format!("/api/v1/invitations/{i:0>43}"))
            .send(&app)
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND);
    }
    let res = get(&format!("/api/v1/invitations/{}", "y".repeat(43)))
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    let res = set_password(&app, &"z".repeat(43), PASSWORD).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    // Another client is not affected.
    let res = get(&format!("/api/v1/invitations/{}", "y".repeat(43)))
        .peer("198.51.100.7:4000")
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------- self-service

#[tokio::test]
async fn member_changes_display_name() {
    let w = world().await;
    let res = patch("/api/v1/me", json!({ "displayName": "  Mia M.  " }))
        .cookie(&w.mia.1)
        .send(&w.app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["displayName"], "Mia M.");
    let me = get("/api/v1/auth/me").cookie(&w.mia.1).send(&w.app).await;
    assert_eq!(me.body["displayName"], "Mia M.");
    let noah = get("/api/v1/me").cookie(&w.noah.1).send(&w.app).await;
    assert_eq!(noah.body["displayName"], "noah");
    let too_long = patch("/api/v1/me", json!({ "displayName": "x".repeat(65) }))
        .cookie(&w.mia.1)
        .send(&w.app)
        .await;
    assert_eq!(too_long.code(), "validation.display_name_invalid");
}

#[tokio::test]
async fn password_change_needs_the_current_password_and_ends_other_sessions() {
    let w = world().await;
    let other = login(&w.app, "mia", PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    let wrong = post(
        "/api/v1/me/password",
        json!({ "currentPassword": "nope", "newPassword": "brand new password" }),
    )
    .cookie(&w.mia.1)
    .send(&w.app)
    .await;
    assert_eq!(wrong.code(), "auth.current_password_invalid");
    let short = post(
        "/api/v1/me/password",
        json!({ "currentPassword": PASSWORD, "newPassword": "short" }),
    )
    .cookie(&w.mia.1)
    .send(&w.app)
    .await;
    assert_eq!(short.code(), "auth.password_too_short");

    let res = post(
        "/api/v1/me/password",
        json!({ "currentPassword": PASSWORD, "newPassword": "brand new password" }),
    )
    .cookie(&w.mia.1)
    .send(&w.app)
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    assert_eq!(status_of(&w.app, &w.mia.1).await, StatusCode::OK);
    assert_eq!(status_of(&w.app, &other).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status_of(&w.app, &w.noah.1).await, StatusCode::OK);
    assert_eq!(
        login(&w.app, "mia", "brand new password").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn sessions_are_listed_and_revoked_only_for_their_owner() {
    let w = world().await;
    let other = login(&w.app, "mia", PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    let list = get("/api/v1/me/sessions")
        .cookie(&w.mia.1)
        .send(&w.app)
        .await;
    let sessions = list.body.as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions.iter().filter(|s| s["current"] == true).count(), 1);
    let other_id = sessions.iter().find(|s| s["current"] == false).unwrap()["id"]
        .as_i64()
        .unwrap();

    // Noah cannot end Mia's session.
    let res = delete(&format!("/api/v1/me/sessions/{other_id}"), None)
        .cookie(&w.noah.1)
        .send(&w.app)
        .await;
    assert_eq!(res.code(), "session.not_found");
    assert_eq!(status_of(&w.app, &other).await, StatusCode::OK);

    let res = delete(&format!("/api/v1/me/sessions/{other_id}"), None)
        .cookie(&w.mia.1)
        .send(&w.app)
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    assert_eq!(status_of(&w.app, &other).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status_of(&w.app, &w.mia.1).await, StatusCode::OK);

    // Log out everywhere: this session too, cookie cleared, Noah untouched.
    let res = delete("/api/v1/me/sessions", None)
        .cookie(&w.mia.1)
        .send(&w.app)
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    assert!(res.set_cookie().unwrap().starts_with("starr_session=;"));
    assert_eq!(status_of(&w.app, &w.mia.1).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status_of(&w.app, &w.noah.1).await, StatusCode::OK);
}
