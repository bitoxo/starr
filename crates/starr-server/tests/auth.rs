//! Auth core (ADR 0005, spec 0002 slice 3): setup token, login, sessions,
//! throttling, CSRF and audit.
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{Method, StatusCode};
use chrono::TimeDelta;
use common::{
    ADMIN_PASSWORD, ADMIN_USER, LAN_ORIGIN, PUBLIC_URL, TestApp, get, login, login_body, post,
    setup_admin, setup_body, test_app, test_app_behind_proxy, test_app_with,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;

/// A reverse proxy in the Docker network: its `X-Forwarded-*` are trusted.
const PROXY_PEER: &str = "172.18.0.2:50000";

async fn me(app: &TestApp, cookie: &str) -> common::TestResponse {
    get("/api/v1/auth/me").cookie(cookie).send(app).await
}

/// Member CRUD arrives in slice 5; insert directly for now.
async fn insert_member(app: &TestApp, username: &str, password: &str, status: &str) {
    let hash = starr_server::auth::crypto::hash_password(&SecretString::from(password)).unwrap();
    sqlx::query(
        "INSERT INTO member (username, display_name, password_hash, status, created_at)
         VALUES (?, ?, ?, ?, '2026-11-01T00:00:00.000Z')",
    )
    .bind(username)
    .bind(username)
    .bind(hash)
    .bind(status)
    .execute(app.state.db.pool())
    .await
    .unwrap();
}

// ---------------------------------------------------------------- setup

#[tokio::test]
async fn setup_state_reports_needs_setup_and_public_url() {
    let app = test_app().await;
    let res = get("/api/v1/setup/state").send(&app).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.body,
        json!({ "needsSetup": true, "publicUrlConfigured": true })
    );

    let app = test_app_with(None).await;
    let res = get("/api/v1/setup/state").send(&app).await;
    assert_eq!(res.body["publicUrlConfigured"], false);
}

#[tokio::test]
async fn setup_with_token_creates_admin_and_logs_in() {
    let app = test_app().await;
    let res = post(
        "/api/v1/setup/admin",
        setup_body(app.setup_token.expose_secret()),
    )
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(
        res.body,
        json!({
            "kind": "admin",
            "username": "root",
            "displayName": "Root Admin",
            "totpEnabled": false,
            "totpEnrollmentRequired": false,
            "linkedMember": null,
            "switchedFrom": null,
            "elevatedUntil": null,
            "privateCategory": null,
        })
    );

    // Plain http request: unprefixed cookie without Secure.
    let set_cookie = res.set_cookie().unwrap();
    for attr in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=2592000"] {
        assert!(set_cookie.contains(attr), "{attr} missing: {set_cookie}");
    }
    assert!(!set_cookie.contains("Secure"), "{set_cookie}");
    let device = res.set_cookie_named("starr_device").unwrap();
    assert!(device.contains("Max-Age=15552000"), "{device}");

    let cookie = res.session_cookie().unwrap();
    assert_eq!(me(&app, &cookie).await.status, StatusCode::OK);
    let state = get("/api/v1/setup/state").send(&app).await;
    assert_eq!(state.body["needsSetup"], false);
}

#[tokio::test]
async fn https_requests_use_host_prefixed_secure_cookies_only() {
    let app = test_app_behind_proxy().await;
    let res = post(
        "/api/v1/setup/admin",
        setup_body(app.setup_token.expose_secret()),
    )
    .header("x-forwarded-proto", "https")
    .peer(PROXY_PEER)
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert!(res.set_cookie().is_none(), "no plain cookie on https");
    let set_cookie = res.set_cookie_named("__Host-starr_session").unwrap();
    for attr in ["Secure", "HttpOnly", "Path=/"] {
        assert!(set_cookie.contains(attr), "{attr} missing: {set_cookie}");
    }
    assert!(!set_cookie.contains("Domain"), "{set_cookie}");
    assert!(res.set_cookie_named("__Host-starr_device").is_some());
    let cookie = res.cookie_pair("__Host-starr_session").unwrap();

    let over_https = get("/api/v1/auth/me")
        .cookie(&cookie)
        .header("x-forwarded-proto", "https")
        .peer(PROXY_PEER)
        .send(&app)
        .await;
    assert_eq!(over_https.status, StatusCode::OK);
    // The same header from a peer that is no proxy (a public address) is
    // ignored: plain http, so the https cookie is not read.
    let direct = get("/api/v1/auth/me")
        .cookie(&cookie)
        .header("x-forwarded-proto", "https")
        .send(&app)
        .await;
    assert_eq!(direct.status, StatusCode::UNAUTHORIZED);
    // A plain-http request does not read the https cookie.
    let over_http = get("/api/v1/auth/me").cookie(&cookie).send(&app).await;
    assert_eq!(over_http.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn additional_origins_pass_the_csrf_check() {
    let app = test_app_behind_proxy().await;
    setup_admin(&app).await;
    let res = post("/api/v1/auth/login", login_body(ADMIN_USER, ADMIN_PASSWORD))
        .origin(Some(LAN_ORIGIN))
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert!(res.session_cookie().is_some());
}

#[tokio::test]
async fn setup_rejects_wrong_token() {
    let app = test_app().await;
    let res = post("/api/v1/setup/admin", setup_body("not-the-token"))
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert_eq!(res.code(), "setup.invalid_token");
    assert!(res.set_cookie().is_none());
    // Still possible with the right token.
    setup_admin(&app).await;
}

#[tokio::test]
async fn setup_token_cannot_be_reused() {
    let app = test_app().await;
    setup_admin(&app).await;
    let mut body = setup_body(app.setup_token.expose_secret());
    body["username"] = json!("second");
    let res = post("/api/v1/setup/admin", body).send(&app).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "setup.already_completed");
}

#[tokio::test]
async fn setup_requires_public_url() {
    let app = test_app_with(None).await;
    let res = post(
        "/api/v1/setup/admin",
        setup_body(app.setup_token.expose_secret()),
    )
    .header("host", "starr.test")
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "setup.public_url_required");
}

#[tokio::test]
async fn setup_validates_input() {
    let app = test_app().await;
    let token = app.setup_token.expose_secret();
    for (field, value, code) in [
        ("password", "short", "auth.password_too_short"),
        ("username", "no spaces", "validation.username_invalid"),
        (
            "displayName",
            &"x".repeat(65),
            "validation.display_name_invalid",
        ),
    ] {
        let mut body = setup_body(token);
        body[field] = json!(value);
        let res = post("/api/v1/setup/admin", body).send(&app).await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        assert_eq!(res.code(), code);
    }
}

#[tokio::test]
async fn no_setup_token_is_issued_once_an_admin_exists() {
    let app = test_app().await;
    setup_admin(&app).await;
    assert!(app.state.issue_setup_token().await.unwrap().is_none());
}

// ---------------------------------------------------------------- login

#[tokio::test]
async fn login_succeeds_and_rotates_the_session() {
    let app = test_app().await;
    let setup_cookie = setup_admin(&app).await;

    let res = post("/api/v1/auth/login", login_body("ROOT ", ADMIN_PASSWORD))
        .cookie(&setup_cookie)
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["username"], "root");
    let new_cookie = res.session_cookie().unwrap();
    assert_ne!(new_cookie, setup_cookie);

    assert_eq!(me(&app, &new_cookie).await.status, StatusCode::OK);
    assert_eq!(
        me(&app, &setup_cookie).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn login_failure_is_identical_for_unknown_user_wrong_password_and_suspended() {
    let app = test_app().await;
    setup_admin(&app).await;
    insert_member(&app, "suspended", ADMIN_PASSWORD, "suspended").await;

    let wrong_password = login(&app, ADMIN_USER, "wrong password!!").await;
    let unknown_user = login(&app, "nobody", ADMIN_PASSWORD).await;
    let suspended = login(&app, "suspended", ADMIN_PASSWORD).await;
    for res in [&wrong_password, &unknown_user, &suspended] {
        assert_eq!(res.status, StatusCode::UNAUTHORIZED);
        assert_eq!(res.raw, r#"{"code":"auth.invalid_credentials"}"#);
        assert!(res.set_cookie().is_none());
    }
    let names = |r: &common::TestResponse| {
        let mut h: Vec<String> = r.headers.keys().map(ToString::to_string).collect();
        h.sort();
        h
    };
    assert_eq!(names(&wrong_password), names(&unknown_user));
    assert_eq!(names(&wrong_password), names(&suspended));
}

#[tokio::test]
async fn members_log_in_as_members() {
    let app = test_app().await;
    insert_member(&app, "mia", "member password 1", "active").await;
    let res = login(&app, "mia", "member password 1").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["kind"], "member");
    let cookie = res.session_cookie().unwrap();
    assert_eq!(me(&app, &cookie).await.body["kind"], "member");
}

#[tokio::test]
async fn members_are_forbidden_on_admin_endpoints() {
    let app = test_app().await;
    insert_member(&app, "mia", "member password 1", "active").await;
    let cookie = login(&app, "mia", "member password 1")
        .await
        .session_cookie()
        .unwrap();
    for (method, path) in [
        (Method::GET, "/api/v1/runs"),
        (Method::GET, "/api/v1/runs/1"),
        (Method::POST, "/api/v1/jobs/system.noop/trigger"),
        (Method::GET, "/api/v1/service-types"),
        (Method::GET, "/api/v1/services"),
        (Method::POST, "/api/v1/services"),
        (Method::GET, "/api/v1/services/1"),
        (Method::PATCH, "/api/v1/services/1"),
        (Method::DELETE, "/api/v1/services/1"),
        (Method::POST, "/api/v1/services/1/check"),
    ] {
        let res = common::Req::new(method.clone(), path, None)
            .cookie(&cookie)
            .send(&app)
            .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{method} {path}");
        assert_eq!(res.code(), "auth.forbidden");
    }
    let admin = setup_admin(&app).await;
    let res = get("/api/v1/runs").cookie(&admin).send(&app).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
}

#[tokio::test]
async fn suspending_an_account_ends_its_sessions() {
    let app = test_app().await;
    insert_member(&app, "mia", "member password 1", "active").await;
    let cookie = login(&app, "mia", "member password 1")
        .await
        .session_cookie()
        .unwrap();
    sqlx::query("UPDATE member SET status = 'suspended' WHERE username = 'mia'")
        .execute(app.state.db.pool())
        .await
        .unwrap();
    assert_eq!(me(&app, &cookie).await.status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------- throttling

#[tokio::test]
async fn five_failures_per_username_throttle_even_the_right_password() {
    let app = test_app().await;
    setup_admin(&app).await;
    for i in 0..5 {
        // Different IPs: only the username limit applies.
        let res = post(
            "/api/v1/auth/login",
            login_body(ADMIN_USER, "wrong password!!"),
        )
        .peer(&format!("198.51.100.{i}:4000"))
        .send(&app)
        .await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "attempt {i}");
    }
    let res = login(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(res.code(), "auth.too_many_attempts");
    assert_eq!(res.headers["retry-after"], "900");

    // Unknown usernames are throttled the same way.
    for _ in 0..5 {
        login(&app, "ghost", "whatever password").await;
    }
    assert_eq!(
        login(&app, "ghost", "whatever password").await.status,
        StatusCode::TOO_MANY_REQUESTS
    );

    app.clock.advance(TimeDelta::minutes(15));
    assert_eq!(
        login(&app, ADMIN_USER, ADMIN_PASSWORD).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn username_variants_share_one_throttle_bucket() {
    let app = test_app().await;
    setup_admin(&app).await;
    for (i, name) in ["ROOT", " root", "Root ", "rOoT", "root"]
        .iter()
        .enumerate()
    {
        let res = post("/api/v1/auth/login", login_body(name, "wrong password!!"))
            .peer(&format!("198.51.100.{i}:4000"))
            .send(&app)
            .await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{name:?}");
    }
    let res = post("/api/v1/auth/login", login_body(ADMIN_USER, ADMIN_PASSWORD))
        .peer("198.51.100.99:4000")
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn a_known_device_still_logs_in_while_the_username_is_under_attack() {
    let app = test_app().await;
    setup_admin(&app).await;
    let first = login(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    let device = first.device_cookie().unwrap();

    // Attacker from many IPs locks the username for untrusted attempts.
    for i in 0..5 {
        post(
            "/api/v1/auth/login",
            login_body(ADMIN_USER, "wrong password!!"),
        )
        .peer(&format!("198.51.100.{i}:4000"))
        .send(&app)
        .await;
    }
    let untrusted = login(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    assert_eq!(untrusted.status, StatusCode::TOO_MANY_REQUESTS);

    let trusted = post("/api/v1/auth/login", login_body(ADMIN_USER, ADMIN_PASSWORD))
        .cookie(&device)
        .send(&app)
        .await;
    assert_eq!(trusted.status, StatusCode::OK, "{}", trusted.raw);
    assert_ne!(
        trusted.device_cookie().unwrap(),
        device,
        "device cookie is renewed"
    );
}

#[tokio::test]
async fn a_device_cookie_has_its_own_limit_and_only_counts_for_its_username() {
    let app = test_app().await;
    setup_admin(&app).await;
    insert_member(&app, "mia", "member password 1", "active").await;
    let device = login(&app, ADMIN_USER, ADMIN_PASSWORD)
        .await
        .device_cookie()
        .unwrap();

    for i in 0..5 {
        let res = post(
            "/api/v1/auth/login",
            login_body(ADMIN_USER, "wrong password!!"),
        )
        .cookie(&device)
        .peer(&format!("198.51.100.{i}:4000"))
        .send(&app)
        .await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    }
    let res = post("/api/v1/auth/login", login_body(ADMIN_USER, ADMIN_PASSWORD))
        .cookie(&device)
        .peer("198.51.100.50:4000")
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    // Those failures did not touch the untrusted username bucket.
    let res = post("/api/v1/auth/login", login_body(ADMIN_USER, ADMIN_PASSWORD))
        .peer("198.51.100.51:4000")
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::OK);

    // The cookie is bound to root: for mia it is ignored (untrusted bucket).
    for i in 0..5 {
        post("/api/v1/auth/login", login_body("mia", "wrong password!!"))
            .cookie(&device)
            .peer(&format!("198.51.101.{i}:4000"))
            .send(&app)
            .await;
    }
    let res = post("/api/v1/auth/login", login_body("mia", "member password 1"))
        .cookie(&device)
        .peer("198.51.101.50:4000")
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn twenty_failures_per_ip_throttle_that_ip() {
    let app = test_app().await;
    setup_admin(&app).await;
    for i in 0..20 {
        let res = login(&app, &format!("user{i}"), "wrong password!!").await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "attempt {i}");
    }
    let res = login(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);

    let other_ip = post("/api/v1/auth/login", login_body(ADMIN_USER, ADMIN_PASSWORD))
        .peer("198.51.100.200:4000")
        .send(&app)
        .await;
    assert_eq!(other_ip.status, StatusCode::OK);
}

#[tokio::test]
async fn behind_proxy_spoofed_forwarded_entries_do_not_evade_the_ip_limit() {
    let app = common::test_app_behind_proxy().await;
    for i in 0..20 {
        let res = post(
            "/api/v1/auth/login",
            login_body(&format!("user{i}"), "wrong password!!"),
        )
        .header("x-forwarded-for", &format!("10.9.9.{i}, 198.51.100.77"))
        .peer(PROXY_PEER)
        .send(&app)
        .await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "attempt {i}");
    }
    let res = post(
        "/api/v1/auth/login",
        login_body("fresh", "wrong password!!"),
    )
    .header("x-forwarded-for", "10.0.0.1, 198.51.100.77")
    .peer(PROXY_PEER)
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    // A different real client behind the same proxy is unaffected.
    let res = post(
        "/api/v1/auth/login",
        login_body("fresh", "wrong password!!"),
    )
    .header("x-forwarded-for", "198.51.100.78")
    .peer(PROXY_PEER)
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------- sessions

#[tokio::test]
async fn idle_sessions_expire_after_fourteen_days() {
    let app = test_app().await;
    let cookie = setup_admin(&app).await;
    app.clock
        .advance(TimeDelta::days(14) - TimeDelta::seconds(1));
    assert_eq!(me(&app, &cookie).await.status, StatusCode::OK);
    // That request counted as activity.
    app.clock
        .advance(TimeDelta::days(14) - TimeDelta::seconds(1));
    assert_eq!(me(&app, &cookie).await.status, StatusCode::OK);
    app.clock.advance(TimeDelta::days(14));
    assert_eq!(me(&app, &cookie).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sessions_expire_thirty_days_after_login_despite_activity() {
    let app = test_app().await;
    let cookie = setup_admin(&app).await;
    for _ in 0..2 {
        app.clock.advance(TimeDelta::days(10));
        assert_eq!(me(&app, &cookie).await.status, StatusCode::OK);
    }
    app.clock.advance(TimeDelta::days(10));
    assert_eq!(me(&app, &cookie).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_invalidates_the_session() {
    let app = test_app().await;
    let cookie = setup_admin(&app).await;
    let res = post("/api/v1/auth/logout", json!({}))
        .cookie(&cookie)
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    let cleared = res.set_cookie().unwrap();
    assert!(cleared.starts_with("starr_session=;"), "{cleared}");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");

    assert_eq!(me(&app, &cookie).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_without_or_with_expired_session_still_clears_the_cookie() {
    let app = test_app().await;
    let res = post("/api/v1/auth/logout", json!({})).send(&app).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    assert!(res.set_cookie().unwrap().contains("Max-Age=0"));

    let cookie = setup_admin(&app).await;
    app.clock.advance(TimeDelta::days(31));
    let res = post("/api/v1/auth/logout", json!({}))
        .cookie(&cookie)
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    assert!(res.set_cookie().unwrap().contains("Max-Age=0"));
}

#[tokio::test]
async fn garbage_cookie_is_unauthenticated() {
    let app = test_app().await;
    let res = me(&app, "starr_session=forged").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.code(), "auth.unauthenticated");
}

// ---------------------------------------------------------------- CSRF

#[tokio::test]
async fn mutating_requests_need_our_origin() {
    let app = test_app().await;
    let cookie = setup_admin(&app).await;
    for origin in [None, Some("https://evil.example"), Some("null")] {
        let res = post("/api/v1/auth/logout", json!({}))
            .cookie(&cookie)
            .origin(origin)
            .send(&app)
            .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{origin:?}");
        assert_eq!(res.code(), "auth.origin_mismatch");

        let res = post("/api/v1/auth/login", login_body(ADMIN_USER, ADMIN_PASSWORD))
            .origin(origin)
            .send(&app)
            .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{origin:?}");
    }
    // The session survived the rejected logouts; GET needs no origin.
    let res = get("/api/v1/auth/me")
        .cookie(&cookie)
        .origin(None)
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::OK);
}

#[tokio::test]
async fn without_public_url_only_same_host_origins_pass() {
    let app = test_app_with(None).await;
    let same = post("/api/v1/auth/login", login_body("x", "y"))
        .origin(Some("http://localhost:5173"))
        .header("host", "localhost:5173")
        .send(&app)
        .await;
    assert_eq!(same.status, StatusCode::UNAUTHORIZED, "{}", same.raw);
    let cross = post("/api/v1/auth/login", login_body("x", "y"))
        .origin(Some(PUBLIC_URL))
        .header("host", "localhost:7979")
        .send(&app)
        .await;
    assert_eq!(cross.status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------- audit

#[tokio::test]
async fn auth_events_are_audited_without_secrets() {
    let app = test_app().await;
    let cookie = setup_admin(&app).await;
    login(&app, ADMIN_USER, "wrong password!!").await;
    post("/api/v1/auth/logout", json!({}))
        .cookie(&cookie)
        .send(&app)
        .await;

    let entries = app.state.db.recent_audit(10).await.unwrap();
    let actions: Vec<&str> = entries.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(
        actions,
        ["auth.logout", "auth.login_failed", "setup.admin_created"]
    );
    assert_eq!(entries[1].target.as_deref(), Some("root"));
    let dump = format!("{entries:?}");
    assert!(!dump.contains("wrong password"), "{dump}");
    assert!(!dump.contains(ADMIN_PASSWORD), "{dump}");
    assert!(dump.contains("203.0.113.10"), "{dump}");
}

#[tokio::test]
async fn malformed_usernames_are_not_audited_or_stored_in_clear() {
    let app = test_app().await;
    setup_admin(&app).await;
    // Someone types their password into the username field.
    login(&app, ADMIN_PASSWORD, "whatever").await;

    let entries = app.state.db.recent_audit(1).await.unwrap();
    assert_eq!(entries[0].action, "auth.login_failed");
    assert_eq!(entries[0].target.as_deref(), Some("<invalid>"));
    let attempts: Vec<(String,)> = sqlx::query_as("SELECT username_hash FROM login_attempt")
        .fetch_all(app.state.db.pool())
        .await
        .unwrap();
    assert!(
        attempts.iter().all(|(u,)| !u.contains("horse")),
        "{attempts:?}"
    );
}

#[tokio::test]
async fn forwarded_for_from_a_public_peer_cannot_dodge_the_ip_limit() {
    let app = test_app_behind_proxy().await;
    setup_admin(&app).await;
    // A client that reaches the port directly picks a new X-Forwarded-For
    // for every attempt: the peer address is throttled all the same.
    for i in 0..20 {
        let res = post(
            "/api/v1/auth/login",
            login_body(&format!("user{i}"), "wrong password!!"),
        )
        .header("x-forwarded-for", &format!("198.51.100.{i}"))
        .send(&app)
        .await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "attempt {i}");
    }
    let res = post(
        "/api/v1/auth/login",
        login_body("other", "wrong password!!"),
    )
    .header("x-forwarded-for", "198.51.100.200")
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS, "{}", res.raw);
    // Through the proxy the forwarded address counts, not the proxy's.
    let res = post(
        "/api/v1/auth/login",
        login_body("other", "wrong password!!"),
    )
    .header("x-forwarded-for", "198.51.100.201")
    .peer(PROXY_PEER)
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.raw);
}
