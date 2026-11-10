//! Admin hardening (spec 0002 slice 6, ADR 0005): TOTP with recovery codes
//! and two-step login, required enrollment, admin accounts, linked member
//! with switch and re-authentication, operator recovery, audit trail.
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, TimeDelta, Utc};
use common::{
    ADMIN_PASSWORD, ADMIN_USER, PUBLIC_URL, Req, TestApp, TestResponse, elevate_in_place, get,
    login, post, setup_admin, test_app,
};
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use starr_server::app as app_router;
use starr_server::recovery::{RecoverError, RecoverRequest, recover_admin};
use starr_server::{AppState, HttpSettings, app};
use url::Url;

const OTHER_PASSWORD: &str = "another admin password";
const MEMBER_PASSWORD: &str = "member password 1";

fn delete(path: &str, body: Value) -> Req {
    Req::new(Method::DELETE, path, Some(body))
}

fn put(path: &str, body: Value) -> Req {
    Req::new(Method::PUT, path, Some(body))
}

/// TOTP code of a base32 secret at `at` (what an authenticator app shows).
fn totp_code(secret: &str, at: DateTime<Utc>) -> String {
    let secret = totp_rs::Secret::try_from_base32(secret).unwrap();
    let totp = totp_rs::Builder::new().with_secret(secret).build().unwrap();
    totp.generate(u64::try_from(at.timestamp()).unwrap())
        .to_string()
}

fn token_of(link: &Value) -> String {
    link["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned()
}

async fn me(app: &TestApp, cookie: &str) -> TestResponse {
    get("/api/v1/auth/me").cookie(cookie).send(app).await
}

/// Audit actions, oldest first.
async fn audit_actions(app: &TestApp) -> Vec<String> {
    let mut entries = app.state.db.recent_audit(500).await.unwrap();
    entries.reverse();
    entries.into_iter().map(|e| e.action).collect()
}

/// Enroll and confirm TOTP for the admin behind `cookie`; returns the
/// base32 secret and the recovery codes. Confirming rotates the session:
/// `cookie` is replaced by the new one.
async fn enable_totp(app: &TestApp, cookie: &mut String) -> (String, Vec<String>) {
    let res = post("/api/v1/auth/totp/enroll", json!({}))
        .cookie(cookie)
        .send(app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let secret = res.body["secret"].as_str().unwrap().to_owned();
    assert!(
        res.body["otpauthUri"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/Starr:")
    );
    assert_ne!(res.body["qrSvg"].as_str().unwrap(), "");
    let res = post(
        "/api/v1/auth/totp/confirm",
        json!({ "code": totp_code(&secret, app.clock.now()) }),
    )
    .cookie(cookie)
    .send(app)
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    *cookie = res.session_cookie().unwrap();
    let codes: Vec<String> = res.body["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect();
    // The confirming code's step is used up; later codes come from later steps.
    app.clock.advance(TimeDelta::seconds(30));
    (secret, codes)
}

/// Password step of a TOTP login: must not start a session.
async fn password_step(app: &TestApp, username: &str, password: &str) -> String {
    let res = login(app, username, password).await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    assert!(res.session_cookie().is_none(), "no session before the code");
    assert!(
        res.device_cookie().is_none(),
        "no device cookie before the code"
    );
    res.body["pendingToken"].as_str().unwrap().to_owned()
}

async fn code_step(app: &TestApp, pending: &str, code: &str) -> TestResponse {
    post(
        "/api/v1/auth/login/totp",
        json!({ "pendingToken": pending, "code": code }),
    )
    .send(app)
    .await
}

// ---------------------------------------------------------------- TOTP login

#[tokio::test]
async fn totp_login_has_two_steps_and_the_first_grants_nothing() {
    let app = test_app().await;
    let mut cookie = setup_admin(&app).await;
    let (secret, _) = enable_totp(&app, &mut cookie).await;
    assert_eq!(me(&app, &cookie).await.body["totpEnabled"], true);

    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    // The pending token is no session token.
    for name in ["starr_session", "__Host-starr_session"] {
        let res = me(&app, &format!("{name}={pending}")).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    }
    // A wrong password never reaches the second step.
    let res = login(&app, ADMIN_USER, "wrong password!!").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert!(res.body.get("pendingToken").is_none());

    let res = code_step(&app, &pending, "000000").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.code(), "auth.invalid_code");
    assert!(res.session_cookie().is_none());

    let res = code_step(&app, &pending, &totp_code(&secret, app.clock.now())).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["kind"], "admin");
    let session = res.session_cookie().unwrap();
    assert!(res.device_cookie().is_some());
    assert_eq!(me(&app, &session).await.status, StatusCode::OK);

    let actions = audit_actions(&app).await;
    for action in [
        "auth.totp_enrollment_started",
        "auth.totp_enabled",
        "auth.login_second_factor_required",
        "auth.second_factor_failed",
        "auth.login",
    ] {
        assert!(actions.iter().any(|a| a == action), "{action}: {actions:?}");
    }
}

#[tokio::test]
async fn pending_logins_are_single_use_and_expire() {
    let app = test_app().await;
    let mut cookie = setup_admin(&app).await;
    let (secret, _) = enable_totp(&app, &mut cookie).await;

    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    let res = code_step(&app, &pending, &totp_code(&secret, app.clock.now())).await;
    assert_eq!(res.status, StatusCode::OK);
    app.clock.advance(TimeDelta::seconds(30));
    let res = code_step(&app, &pending, &totp_code(&secret, app.clock.now())).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.code(), "auth.login_expired");

    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    app.clock.advance(TimeDelta::minutes(5));
    let res = code_step(&app, &pending, &totp_code(&secret, app.clock.now())).await;
    assert_eq!(res.code(), "auth.login_expired");

    let res = code_step(&app, "made-up-token", "123456").await;
    assert_eq!(res.code(), "auth.login_expired");
}

#[tokio::test]
async fn a_code_is_accepted_once_per_time_step() {
    let app = test_app().await;
    let mut cookie = setup_admin(&app).await;
    let (secret, _) = enable_totp(&app, &mut cookie).await;
    let code = totp_code(&secret, app.clock.now());

    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    assert_eq!(
        code_step(&app, &pending, &code).await.status,
        StatusCode::OK
    );

    // Someone who saw the code replays it within its validity window.
    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    app.clock.advance(TimeDelta::seconds(20));
    let res = code_step(&app, &pending, &code).await;
    assert_eq!(res.code(), "auth.invalid_code");
    let replay = app
        .state
        .db
        .recent_audit(1)
        .await
        .unwrap()
        .remove(0)
        .details
        .unwrap();
    assert!(replay.contains("\"replay\":true"), "{replay}");

    // The next step's code works.
    app.clock.advance(TimeDelta::seconds(30));
    let res = code_step(&app, &pending, &totp_code(&secret, app.clock.now())).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
}

#[tokio::test]
async fn recovery_codes_work_once_each() {
    let app = test_app().await;
    let mut cookie = setup_admin(&app).await;
    let (_, codes) = enable_totp(&app, &mut cookie).await;
    assert_eq!(codes.len(), 10);

    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    let res = code_step(&app, &pending, &codes[0].to_lowercase()).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let session = res.session_cookie().unwrap();
    let status = get("/api/v1/auth/totp").cookie(&session).send(&app).await;
    assert_eq!(status.body["recoveryCodesRemaining"], 9);

    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    let res = code_step(&app, &pending, &codes[0]).await;
    assert_eq!(res.code(), "auth.invalid_code");
    assert!(
        audit_actions(&app)
            .await
            .contains(&"auth.recovery_code_used".to_owned())
    );
}

#[tokio::test]
async fn wrong_codes_are_throttled() {
    let app = test_app().await;
    let mut cookie = setup_admin(&app).await;
    let (secret, _) = enable_totp(&app, &mut cookie).await;
    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    for _ in 0..5 {
        let res = code_step(&app, &pending, "000000").await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    }
    let res = code_step(&app, &pending, &totp_code(&secret, app.clock.now())).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn disabling_totp_needs_password_and_code() {
    let app = test_app().await;
    let mut cookie = setup_admin(&app).await;
    let (secret, codes) = enable_totp(&app, &mut cookie).await;
    let disable = |password: &str, code: &str| {
        post(
            "/api/v1/auth/totp/disable",
            json!({ "password": password, "code": code }),
        )
        .cookie(&cookie)
    };

    let res = disable("wrong password!!", &totp_code(&secret, app.clock.now()))
        .send(&app)
        .await;
    assert_eq!(res.code(), "auth.current_password_invalid");
    let res = disable(ADMIN_PASSWORD, "000000").send(&app).await;
    assert_eq!(res.code(), "auth.invalid_code");
    let res = disable(ADMIN_PASSWORD, &codes[3]).send(&app).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);

    // One-step login again; recovery codes are gone.
    let res = login(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        audit_actions(&app)
            .await
            .contains(&"auth.totp_disabled".to_owned())
    );
    let status = get("/api/v1/auth/totp").cookie(&cookie).send(&app).await;
    assert_eq!(status.body["enabled"], false);
    assert_eq!(status.body["recoveryCodesRemaining"], 0);
}

#[tokio::test]
async fn enrollment_needs_a_valid_code_and_cannot_repeat() {
    let app = test_app().await;
    let mut cookie = setup_admin(&app).await;
    let res = post("/api/v1/auth/totp/confirm", json!({ "code": "123456" }))
        .cookie(&cookie)
        .send(&app)
        .await;
    assert_eq!(res.code(), "totp.no_enrollment");
    post("/api/v1/auth/totp/enroll", json!({}))
        .cookie(&cookie)
        .send(&app)
        .await;
    let res = post("/api/v1/auth/totp/confirm", json!({ "code": "000000" }))
        .cookie(&cookie)
        .send(&app)
        .await;
    assert_eq!(res.code(), "auth.invalid_code");
    assert_eq!(me(&app, &cookie).await.body["totpEnabled"], false);

    enable_totp(&app, &mut cookie).await;
    let res = post("/api/v1/auth/totp/enroll", json!({}))
        .cookie(&cookie)
        .send(&app)
        .await;
    assert_eq!(res.code(), "totp.already_enabled");
}

#[tokio::test]
async fn members_cannot_use_totp_endpoints() {
    let w = linked_world().await;
    for (method, path) in [
        (Method::GET, "/api/v1/auth/totp"),
        (Method::POST, "/api/v1/auth/totp/enroll"),
        (Method::GET, "/api/v1/admins"),
    ] {
        let body = (method != Method::GET).then(|| json!({}));
        let res = Req::new(method.clone(), path, body)
            .cookie(&w.member)
            .send(&w.app)
            .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{method} {path}");
    }
}

// ---------------------------------------------------------------- required TOTP

async fn required_totp_app() -> TestApp {
    let db = starr_db::Database::open_in_memory().await.unwrap();
    let clock = starr_server::clock::Clock::manual(common::t0());
    let settings = HttpSettings {
        public_url: Some(Url::parse(PUBLIC_URL).unwrap()),
        admin_totp_required: true,
        ..HttpSettings::default()
    };
    let state = AppState::new(db, settings).with_clock(clock.clone());
    let setup_token = state.issue_setup_token().await.unwrap().unwrap();
    TestApp {
        router: app(state.clone()),
        state,
        clock,
        setup_token,
    }
}

#[tokio::test]
async fn required_totp_blocks_every_other_admin_api_until_enrolled() {
    let app = required_totp_app().await;
    let mut cookie = setup_admin(&app).await;
    let res = me(&app, &cookie).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["totpEnrollmentRequired"], true);

    for (method, path) in [
        (Method::GET, "/api/v1/members"),
        (Method::GET, "/api/v1/admins"),
        (Method::GET, "/api/v1/runs"),
        (Method::POST, "/api/v1/auth/switch"),
        (Method::POST, "/api/v1/invitations"),
    ] {
        let body = (method != Method::GET).then(|| json!({}));
        let res = Req::new(method.clone(), path, body)
            .cookie(&cookie)
            .send(&app)
            .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{method} {path}");
        assert_eq!(res.code(), "auth.totp_enrollment_required");
    }

    enable_totp(&app, &mut cookie).await;
    let res = get("/api/v1/members").cookie(&cookie).send(&app).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        me(&app, &cookie).await.body["totpEnrollmentRequired"],
        false
    );
}

// ---------------------------------------------------------------- admin accounts

/// Create admin `username` via the API and let them set their password;
/// returns (id, session cookie).
async fn create_admin(app: &TestApp, actor: &str, username: &str) -> (i64, String) {
    elevate_in_place(app, actor).await;
    let res = post("/api/v1/admins", json!({ "username": username }))
        .cookie(actor)
        .send(app)
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let id = res.body["admin"]["id"].as_i64().unwrap();
    // Not usable before the link is used.
    assert_eq!(
        login(app, username, OTHER_PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    let token = token_of(&res.body["setPasswordLink"]);
    let res = post(
        &format!("/api/v1/password-tokens/{token}"),
        json!({ "password": OTHER_PASSWORD }),
    )
    .send(app)
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["kind"], "admin");
    (id, res.session_cookie().unwrap())
}

#[tokio::test]
async fn creating_suspending_reactivating_and_removing_admins_needs_an_elevation() {
    let app = test_app().await;
    let root = setup_admin(&app).await;
    let (ada_id, _) = create_admin(&app, &root, "ada").await;
    let (bob_id, _) = create_admin(&app, &root, "bob").await;
    let res = post(&format!("/api/v1/admins/{ada_id}/suspend"), json!({}))
        .cookie(&root)
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    // The elevation from creating ada has run out.
    app.clock.advance(TimeDelta::minutes(6));
    for res in [
        post(&format!("/api/v1/admins/{bob_id}/suspend"), json!({}))
            .cookie(&root)
            .send(&app)
            .await,
        post("/api/v1/admins", json!({ "username": "eve" }))
            .cookie(&root)
            .send(&app)
            .await,
        post(&format!("/api/v1/admins/{ada_id}/reactivate"), json!({}))
            .cookie(&root)
            .send(&app)
            .await,
        delete(
            &format!("/api/v1/admins/{ada_id}"),
            json!({ "username": "ada" }),
        )
        .cookie(&root)
        .send(&app)
        .await,
    ] {
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
        assert_eq!(res.code(), "auth.elevation_required");
    }
    let list = get("/api/v1/admins").cookie(&root).send(&app).await;
    assert_eq!(list.body.as_array().unwrap().len(), 3);

    // After re-authenticating it works.
    let res = elevate(&app, &root, json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let root = res.session_cookie().unwrap();
    let res = post("/api/v1/admins", json!({ "username": "eve" }))
        .cookie(&root)
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
}

#[tokio::test]
async fn admins_are_created_with_a_set_password_link() {
    let app = test_app().await;
    let root = setup_admin(&app).await;
    let (_, ada) = create_admin(&app, &root, "ada").await;
    assert_eq!(me(&app, &ada).await.body["kind"], "admin");
    assert_eq!(
        login(&app, "ada", OTHER_PASSWORD).await.status,
        StatusCode::OK
    );

    let list = get("/api/v1/admins").cookie(&root).send(&app).await;
    let admins = list.body.as_array().unwrap();
    assert_eq!(admins.len(), 2);
    assert_eq!(admins[0]["current"], true);
    assert_eq!(admins[1]["createdBy"], "root");
    assert_eq!(admins[1]["status"], "active");

    let res = post("/api/v1/admins", json!({ "username": "ada" }))
        .cookie(&root)
        .send(&app)
        .await;
    assert_eq!(res.code(), "validation.username_taken");
    let actions = audit_actions(&app).await;
    assert!(actions.contains(&"admin.created".to_owned()));
    assert!(actions.contains(&"admin.password_set".to_owned()));
}

#[tokio::test]
async fn an_admin_sees_and_ends_only_their_own_sessions() {
    let app = test_app().await;
    let root = setup_admin(&app).await;
    let (_, ada) = create_admin(&app, &root, "ada").await;
    let other = login(&app, ADMIN_USER, ADMIN_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    let list = get("/api/v1/admin/me/sessions")
        .cookie(&root)
        .send(&app)
        .await;
    let sessions = list.body.as_array().unwrap();
    assert_eq!(sessions.len(), 2, "{}", list.raw);
    assert_eq!(sessions.iter().filter(|s| s["current"] == true).count(), 1);
    let other_id = sessions.iter().find(|s| s["current"] == false).unwrap()["id"]
        .as_i64()
        .unwrap();

    // This session ends by signing out, not here.
    let current_id = sessions.iter().find(|s| s["current"] == true).unwrap()["id"]
        .as_i64()
        .unwrap();
    let res = delete(
        &format!("/api/v1/admin/me/sessions/{current_id}"),
        json!({}),
    )
    .cookie(&root)
    .send(&app)
    .await;
    assert_eq!(res.code(), "session.current");
    assert_eq!(me(&app, &root).await.status, StatusCode::OK);

    // Another admin cannot end it.
    let res = delete(&format!("/api/v1/admin/me/sessions/{other_id}"), json!({}))
        .cookie(&ada)
        .send(&app)
        .await;
    assert_eq!(res.code(), "session.not_found");
    assert_eq!(me(&app, &other).await.status, StatusCode::OK);

    let res = delete(&format!("/api/v1/admin/me/sessions/{other_id}"), json!({}))
        .cookie(&root)
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    assert_eq!(me(&app, &other).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&app, &root).await.status, StatusCode::OK);
    assert!(
        audit_actions(&app)
            .await
            .contains(&"admin.session_revoked".to_owned())
    );
}

#[tokio::test]
async fn an_admin_changes_their_password_with_the_current_one() {
    let app = test_app().await;
    let root = setup_admin(&app).await;
    let (_, ada) = create_admin(&app, &root, "ada").await;
    let other = login(&app, ADMIN_USER, ADMIN_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    let wrong = post(
        "/api/v1/admin/me/password",
        json!({ "currentPassword": "nope", "newPassword": "brand new password" }),
    )
    .cookie(&root)
    .send(&app)
    .await;
    assert_eq!(wrong.code(), "auth.current_password_invalid");
    let short = post(
        "/api/v1/admin/me/password",
        json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": "short" }),
    )
    .cookie(&root)
    .send(&app)
    .await;
    assert_eq!(short.code(), "auth.password_too_short");

    let res = post(
        "/api/v1/admin/me/password",
        json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": "brand new password" }),
    )
    .cookie(&root)
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    // This session stays, the other one ends, other admins are untouched.
    assert_eq!(me(&app, &root).await.status, StatusCode::OK);
    assert_eq!(me(&app, &other).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&app, &ada).await.status, StatusCode::OK);
    assert_eq!(
        login(&app, ADMIN_USER, ADMIN_PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, ADMIN_USER, "brand new password").await.status,
        StatusCode::OK
    );
    assert!(
        audit_actions(&app)
            .await
            .contains(&"admin.password_changed".to_owned())
    );
}

#[tokio::test]
async fn members_cannot_use_the_admin_self_service() {
    let app = test_app().await;
    let root = setup_admin(&app).await;
    let res = post(
        "/api/v1/members",
        json!({ "username": "mia", "email": "mia@example.org" }),
    )
    .cookie(&root)
    .send(&app)
    .await;
    let token = token_of(&res.body["setPasswordLink"]);
    let mia = post(
        &format!("/api/v1/password-tokens/{token}"),
        json!({ "password": MEMBER_PASSWORD }),
    )
    .send(&app)
    .await
    .session_cookie()
    .unwrap();
    let list = get("/api/v1/admin/me/sessions")
        .cookie(&mia)
        .send(&app)
        .await;
    assert_eq!(list.status, StatusCode::FORBIDDEN);
    let res = post(
        "/api/v1/admin/me/password",
        json!({ "currentPassword": MEMBER_PASSWORD, "newPassword": "brand new password" }),
    )
    .cookie(&mia)
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn nobody_suspends_or_removes_themselves() {
    let app = test_app().await;
    let root = setup_admin(&app).await;
    create_admin(&app, &root, "ada").await;
    let root_id = 1;
    let res = post(&format!("/api/v1/admins/{root_id}/suspend"), json!({}))
        .cookie(&root)
        .send(&app)
        .await;
    assert_eq!(res.code(), "admin.self");
    let res = delete(
        &format!("/api/v1/admins/{root_id}"),
        json!({ "username": "root" }),
    )
    .cookie(&root)
    .send(&app)
    .await;
    assert_eq!(res.code(), "admin.self");
}

#[tokio::test]
async fn suspension_ends_sessions_and_removal_leaves_a_tombstone() {
    let app = test_app().await;
    let root = setup_admin(&app).await;
    let (ada_id, ada) = create_admin(&app, &root, "ada").await;

    let res = post(&format!("/api/v1/admins/{ada_id}/suspend"), json!({}))
        .cookie(&root)
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["status"], "suspended");
    assert_eq!(me(&app, &ada).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        login(&app, "ada", OTHER_PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );

    let res = post(&format!("/api/v1/admins/{ada_id}/reactivate"), json!({}))
        .cookie(&root)
        .send(&app)
        .await;
    assert_eq!(res.body["status"], "active");
    assert_eq!(
        login(&app, "ada", OTHER_PASSWORD).await.status,
        StatusCode::OK
    );

    let res = delete(
        &format!("/api/v1/admins/{ada_id}"),
        json!({ "username": "wrong" }),
    )
    .cookie(&root)
    .send(&app)
    .await;
    assert_eq!(res.code(), "validation.confirmation_mismatch");
    let res = delete(
        &format!("/api/v1/admins/{ada_id}"),
        json!({ "username": "ada" }),
    )
    .cookie(&root)
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    assert_eq!(
        login(&app, "ada", OTHER_PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    let list = get("/api/v1/admins").cookie(&root).send(&app).await;
    assert_eq!(list.body.as_array().unwrap().len(), 1);
    // The username stays reserved, for admins and members alike.
    let res = post("/api/v1/admins", json!({ "username": "ada" }))
        .cookie(&root)
        .send(&app)
        .await;
    assert_eq!(res.code(), "validation.username_taken");
    let res = post(
        "/api/v1/members",
        json!({ "username": "ada", "email": "ada@example.org" }),
    )
    .cookie(&root)
    .send(&app)
    .await;
    assert_eq!(res.code(), "validation.username_taken");

    let actions = audit_actions(&app).await;
    for action in ["admin.suspended", "admin.reactivated", "admin.removed"] {
        assert!(actions.contains(&action.to_owned()), "{action}");
    }
}

#[tokio::test]
async fn an_admin_set_password_link_does_not_bypass_totp() {
    let app = test_app().await;
    let root = setup_admin(&app).await;
    let (ada_id, mut ada) = create_admin(&app, &root, "ada").await;
    let (secret, _) = enable_totp(&app, &mut ada).await;

    let request = RecoverRequest {
        username: "ada".into(),
        reset_totp: false,
        create: false,
    };
    let recovered = recover_admin(&app.state.db, None, &request, app.clock.now())
        .await
        .unwrap();
    let token = recovered
        .link
        .expose_secret()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    let res = post(
        &format!("/api/v1/password-tokens/{token}"),
        json!({ "password": "brand new password" }),
    )
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    assert!(res.session_cookie().is_none());
    let pending = res.body["pendingToken"].as_str().unwrap().to_owned();
    let res = code_step(&app, &pending, &totp_code(&secret, app.clock.now())).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(
        app.state
            .db
            .get_admin(ada_id)
            .await
            .unwrap()
            .unwrap()
            .username,
        "ada"
    );
}

// ---------------------------------------------------------------- linked member + switch

struct LinkedWorld {
    app: TestApp,
    admin: String,
    member_id: i64,
    /// The member's own session.
    member: String,
}

/// Root admin, member `mia` with a password, linked to root.
async fn linked_world() -> LinkedWorld {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let res = post(
        "/api/v1/members",
        json!({ "username": "mia", "email": "mia@example.org", "displayName": "Mia" }),
    )
    .cookie(&admin)
    .send(&app)
    .await;
    let member_id = res.body["member"]["id"].as_i64().unwrap();
    let token = token_of(&res.body["setPasswordLink"]);
    let res = post(
        &format!("/api/v1/password-tokens/{token}"),
        json!({ "password": MEMBER_PASSWORD }),
    )
    .send(&app)
    .await;
    let member = res.session_cookie().unwrap();
    // Linking needs the member's password: proof it is the admin's own profile.
    for body in [
        json!({ "memberId": member_id }),
        json!({ "memberId": member_id, "memberPassword": "not the password" }),
    ] {
        let res = put("/api/v1/auth/linked-member", body)
            .cookie(&admin)
            .send(&app)
            .await;
        assert_eq!(res.code(), "auth.member_password_invalid");
    }
    let res = put(
        "/api/v1/auth/linked-member",
        json!({ "memberId": member_id, "memberPassword": MEMBER_PASSWORD }),
    )
    .cookie(&admin)
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["linkedMember"]["username"], "mia");
    LinkedWorld {
        app,
        admin,
        member_id,
        member,
    }
}

async fn switch(app: &TestApp, cookie: &str) -> TestResponse {
    post("/api/v1/auth/switch", json!({}))
        .cookie(cookie)
        .send(app)
        .await
}

async fn elevate(app: &TestApp, cookie: &str, body: Value) -> TestResponse {
    post("/api/v1/auth/elevate", body)
        .cookie(cookie)
        .send(app)
        .await
}

#[tokio::test]
async fn a_password_change_also_ends_member_view_sessions() {
    let w = linked_world().await;
    let app = &w.app;
    let other = login(app, ADMIN_USER, ADMIN_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    let res = switch(app, &other).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let member_view = res.session_cookie().unwrap();
    assert_eq!(me(app, &member_view).await.status, StatusCode::OK);

    let res = post(
        "/api/v1/admin/me/password",
        json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": "brand new password" }),
    )
    .cookie(&w.admin)
    .send(app)
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    assert_eq!(me(app, &member_view).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(me(app, &w.admin).await.status, StatusCode::OK);
    // The member's own session is not the admin's.
    assert_eq!(me(app, &w.member).await.status, StatusCode::OK);
}

#[tokio::test]
async fn switching_to_member_view_and_back_needs_reauthentication() {
    let w = linked_world().await;
    let app = &w.app;

    let res = switch(app, &w.admin).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["kind"], "member");
    assert_eq!(res.body["username"], "mia");
    assert_eq!(res.body["switchedFrom"]["username"], "root");
    let member_view = res.session_cookie().unwrap();
    // Rotated: the admin cookie is dead, the new one is a member session.
    assert_eq!(me(app, &w.admin).await.status, StatusCode::UNAUTHORIZED);
    let res = get("/api/v1/members").cookie(&member_view).send(app).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert_eq!(
        get("/api/v1/me")
            .cookie(&member_view)
            .send(app)
            .await
            .status,
        StatusCode::OK
    );

    let res = switch(app, &member_view).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert_eq!(res.code(), "auth.elevation_required");

    let res = elevate(app, &member_view, json!({ "password": "wrong password!!" })).await;
    assert_eq!(res.code(), "auth.current_password_invalid");
    // The member's password is not the admin's.
    let res = elevate(app, &member_view, json!({ "password": MEMBER_PASSWORD })).await;
    assert_eq!(res.code(), "auth.current_password_invalid");

    let res = elevate(app, &member_view, json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let elevated = res.session_cookie().unwrap();
    assert_eq!(me(app, &member_view).await.status, StatusCode::UNAUTHORIZED);

    let res = switch(app, &elevated).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["kind"], "admin");
    let admin_again = res.session_cookie().unwrap();
    assert_eq!(me(app, &elevated).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        get("/api/v1/members")
            .cookie(&admin_again)
            .send(app)
            .await
            .status,
        StatusCode::OK
    );

    let actions = audit_actions(app).await;
    for action in [
        "admin.member_linked",
        "auth.switched_to_member",
        "auth.elevate_failed",
        "auth.elevated",
        "auth.switched_to_admin",
    ] {
        assert!(
            actions.contains(&action.to_owned()),
            "{action}: {actions:?}"
        );
    }
}

#[tokio::test]
async fn elevation_lasts_five_minutes() {
    let w = linked_world().await;
    let member_view = switch(&w.app, &w.admin).await.session_cookie().unwrap();
    let elevated = elevate(&w.app, &member_view, json!({ "password": ADMIN_PASSWORD }))
        .await
        .session_cookie()
        .unwrap();
    w.app.clock.advance(TimeDelta::minutes(5));
    let res = switch(&w.app, &elevated).await;
    assert_eq!(res.code(), "auth.elevation_required");
}

#[tokio::test]
async fn me_says_until_when_an_admin_is_elevated() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    assert!(me(&app, &admin).await.body["elevatedUntil"].is_null());
    let res = elevate(&app, &admin, json!({ "password": ADMIN_PASSWORD })).await;
    let elevated = res.session_cookie().unwrap_or(admin);
    let until = me(&app, &elevated).await.body["elevatedUntil"].clone();
    assert!(until.is_string(), "{until}");
    app.clock.advance(TimeDelta::minutes(5));
    assert!(me(&app, &elevated).await.body["elevatedUntil"].is_null());
}

#[tokio::test]
async fn a_member_view_never_shows_an_elevation() {
    let w = linked_world().await;
    let member_view = switch(&w.app, &w.admin).await.session_cookie().unwrap();
    let res = elevate(&w.app, &member_view, json!({ "password": ADMIN_PASSWORD })).await;
    let elevated = res.session_cookie().unwrap_or(member_view);
    let me = me(&w.app, &elevated).await;
    assert_eq!(me.body["kind"], "member");
    assert!(me.body["elevatedUntil"].is_null(), "{}", me.raw);
    assert!(
        get("/api/v1/auth/me")
            .cookie(&w.member)
            .send(&w.app)
            .await
            .body["elevatedUntil"]
            .is_null()
    );
}

#[tokio::test]
async fn elevation_asks_for_totp_when_it_is_on() {
    let mut w = linked_world().await;
    let (secret, _) = enable_totp(&w.app, &mut w.admin).await;
    let member_view = switch(&w.app, &w.admin).await.session_cookie().unwrap();
    assert_eq!(
        me(&w.app, &member_view).await.body["switchedFrom"]["totpEnabled"],
        true
    );
    let res = elevate(&w.app, &member_view, json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!(res.code(), "auth.code_required");
    let res = elevate(
        &w.app,
        &member_view,
        json!({ "password": ADMIN_PASSWORD, "code": "000000" }),
    )
    .await;
    assert_eq!(res.code(), "auth.invalid_code");
    let res = elevate(
        &w.app,
        &member_view,
        json!({ "password": ADMIN_PASSWORD, "code": totp_code(&secret, w.app.clock.now()) }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = switch(&w.app, &res.session_cookie().unwrap()).await;
    assert_eq!(res.body["kind"], "admin");
}

#[tokio::test]
async fn a_suspended_linked_member_cannot_be_switched_into() {
    let w = linked_world().await;
    let res = post(
        &format!("/api/v1/members/{}/suspend", w.member_id),
        json!({}),
    )
    .cookie(&w.admin)
    .send(&w.app)
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        me(&w.app, &w.admin).await.body["linkedMember"]["active"],
        false
    );
    let res = switch(&w.app, &w.admin).await;
    assert_eq!(res.code(), "auth.linked_member_inactive");
    // The admin session survives the refused switch.
    assert_eq!(me(&w.app, &w.admin).await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_members_own_session_cannot_switch_or_elevate() {
    let w = linked_world().await;
    assert_eq!(
        switch(&w.app, &w.member).await.status,
        StatusCode::FORBIDDEN
    );
    let res = elevate(&w.app, &w.member, json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = put(
        "/api/v1/auth/linked-member",
        json!({ "memberId": w.member_id, "memberPassword": MEMBER_PASSWORD }),
    )
    .cookie(&w.member)
    .send(&w.app)
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_member_is_linked_to_one_admin_and_unlinking_ends_the_member_view() {
    let w = linked_world().await;
    let (_, ada) = create_admin(&w.app, &w.admin, "ada").await;
    let res = put(
        "/api/v1/auth/linked-member",
        json!({ "memberId": w.member_id, "memberPassword": MEMBER_PASSWORD }),
    )
    .cookie(&ada)
    .send(&w.app)
    .await;
    assert_eq!(res.code(), "member.already_linked");

    let member_view = switch(&w.app, &w.admin).await.session_cookie().unwrap();
    // Root logs in elsewhere and unlinks: the member view ends.
    let root = login(&w.app, ADMIN_USER, ADMIN_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    let res = put("/api/v1/auth/linked-member", json!({ "memberId": null }))
        .cookie(&root)
        .send(&w.app)
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["linkedMember"], Value::Null);
    assert_eq!(
        me(&w.app, &member_view).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn suspending_an_admin_ends_their_member_view() {
    let w = linked_world().await;
    let (ada_id, ada) = create_admin(&w.app, &w.admin, "ada").await;
    let res = post(
        "/api/v1/members",
        json!({ "username": "noah", "email": "noah@example.org" }),
    )
    .cookie(&ada)
    .send(&w.app)
    .await;
    let noah = res.body["member"]["id"].as_i64().unwrap();
    let token = token_of(&res.body["setPasswordLink"]);
    post(
        &format!("/api/v1/password-tokens/{token}"),
        json!({ "password": MEMBER_PASSWORD }),
    )
    .send(&w.app)
    .await;
    put(
        "/api/v1/auth/linked-member",
        json!({ "memberId": noah, "memberPassword": MEMBER_PASSWORD }),
    )
    .cookie(&ada)
    .send(&w.app)
    .await;
    let ada_member_view = switch(&w.app, &ada).await.session_cookie().unwrap();
    assert_eq!(me(&w.app, &ada_member_view).await.status, StatusCode::OK);

    post(&format!("/api/v1/admins/{ada_id}/suspend"), json!({}))
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    assert_eq!(
        me(&w.app, &ada_member_view).await.status,
        StatusCode::UNAUTHORIZED
    );
}

// ---------------------------------------------------------------- recovery CLI

fn recover_request(username: &str, reset_totp: bool, create: bool) -> RecoverRequest {
    RecoverRequest {
        username: username.into(),
        reset_totp,
        create,
    }
}

#[tokio::test]
async fn recovery_reactivates_resets_totp_and_issues_a_link() {
    let app = test_app().await;
    let mut root = setup_admin(&app).await;
    let (ada_id, ada) = create_admin(&app, &root, "ada").await;
    enable_totp(&app, &mut root).await;
    // Ada suspends root, then root lost the authenticator too.
    elevate_in_place(&app, &ada).await;
    let res = post("/api/v1/admins/1/suspend", json!({}))
        .cookie(&ada)
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(
        login(&app, ADMIN_USER, ADMIN_PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );

    let public = Url::parse(PUBLIC_URL).unwrap();
    let recovered = recover_admin(
        &app.state.db,
        Some(&public),
        &recover_request("ROOT", true, false),
        app.clock.now(),
    )
    .await
    .unwrap();
    assert!(!recovered.created);
    let link = recovered.link.expose_secret();
    assert!(
        link.starts_with("https://starr.test/set-password/"),
        "{link}"
    );

    // With --reset-totp the old password alone must not get in: only the link.
    let res = login(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.raw);
    let token = link.rsplit('/').next().unwrap();
    let res = post(
        &format!("/api/v1/password-tokens/{token}"),
        json!({ "password": "recovered password" }),
    )
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(
        login(&app, ADMIN_USER, "recovered password").await.status,
        StatusCode::OK
    );
    let entry = app.state.db.recent_audit(500).await.unwrap();
    let recovered_entry = entry
        .iter()
        .find(|e| e.action == "admin.recovered")
        .unwrap();
    assert_eq!(recovered_entry.actor_kind, "system");
    assert_eq!(recovered_entry.target.as_deref(), Some("root"));
    let _ = ada_id;
}

#[tokio::test]
async fn recovery_refuses_tombstones_and_unknown_names_unless_creating() {
    let app = test_app().await;
    let root = setup_admin(&app).await;
    let (ada_id, _) = create_admin(&app, &root, "ada").await;
    delete(
        &format!("/api/v1/admins/{ada_id}"),
        json!({ "username": "ada" }),
    )
    .cookie(&root)
    .send(&app)
    .await;
    let db = &app.state.db;
    let now = app.clock.now();

    for create in [false, true] {
        let err = recover_admin(db, None, &recover_request("ada", false, create), now)
            .await
            .unwrap_err();
        assert!(matches!(err, RecoverError::Tombstone(_)), "{err}");
    }
    let err = recover_admin(db, None, &recover_request("eve", false, false), now)
        .await
        .unwrap_err();
    assert!(matches!(err, RecoverError::NotFound(_)), "{err}");
    let err = recover_admin(db, None, &recover_request("no spaces", false, true), now)
        .await
        .unwrap_err();
    assert!(matches!(err, RecoverError::InvalidUsername(_)), "{err}");

    let created = recover_admin(db, None, &recover_request("eve", false, true), now)
        .await
        .unwrap();
    assert!(created.created);
    let link = created.link.expose_secret();
    assert!(link.starts_with("/set-password/"), "{link}");
    let res = post(
        &format!(
            "/api/v1/password-tokens/{}",
            link.rsplit('/').next().unwrap()
        ),
        json!({ "password": OTHER_PASSWORD }),
    )
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["kind"], "admin");
}

#[tokio::test]
async fn recovery_with_totp_reset_ends_existing_sessions() {
    let app = test_app().await;
    let mut root = setup_admin(&app).await;
    create_admin(&app, &root, "ada").await;
    enable_totp(&app, &mut root).await;
    recover_admin(
        &app.state.db,
        None,
        &recover_request("root", true, false),
        app.clock.now(),
    )
    .await
    .unwrap();
    assert_eq!(me(&app, &root).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn recovery_without_totp_reset_keeps_the_password_and_the_second_factor() {
    let app = test_app().await;
    let mut root = setup_admin(&app).await;
    enable_totp(&app, &mut root).await;
    recover_admin(
        &app.state.db,
        None,
        &recover_request("root", false, false),
        app.clock.now(),
    )
    .await
    .unwrap();
    // Still two steps with the old password.
    password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
}

#[tokio::test]
async fn member_view_actions_are_attributed_to_the_admin() {
    let w = linked_world().await;
    let member_view = switch(&w.app, &w.admin).await.session_cookie().unwrap();
    let res = Req::new(
        Method::PATCH,
        "/api/v1/me",
        Some(json!({ "displayName": "Mia (via root)" })),
    )
    .cookie(&member_view)
    .send(&w.app)
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let entry = w.app.state.db.recent_audit(1).await.unwrap().remove(0);
    assert_eq!(entry.action, "member.profile_updated");
    assert!(
        entry
            .details
            .unwrap_or_default()
            .contains("\"viaAdmin\":\"root\""),
        "member-view action must name the admin"
    );
}

#[tokio::test]
async fn required_totp_cannot_be_turned_off() {
    let app = required_totp_app().await;
    let mut cookie = setup_admin(&app).await;
    let (secret, _) = enable_totp(&app, &mut cookie).await;
    let res = post(
        "/api/v1/auth/totp/disable",
        json!({ "password": ADMIN_PASSWORD, "code": totp_code(&secret, app.clock.now()) }),
    )
    .cookie(&cookie)
    .send(&app)
    .await;
    assert_eq!(res.code(), "totp.required");
}

#[tokio::test]
async fn a_suspended_admin_cannot_finish_a_pending_login() {
    let app = test_app().await;
    let root = setup_admin(&app).await;
    let (ada_id, mut ada) = create_admin(&app, &root, "ada").await;
    let (secret, _) = enable_totp(&app, &mut ada).await;
    let pending = password_step(&app, "ada", OTHER_PASSWORD).await;
    post(&format!("/api/v1/admins/{ada_id}/suspend"), json!({}))
        .cookie(&root)
        .send(&app)
        .await;
    let res = code_step(&app, &pending, &totp_code(&secret, app.clock.now())).await;
    assert_eq!(res.code(), "auth.login_expired");
}

#[tokio::test]
async fn api_answers_are_never_cached() {
    let app = test_app().await;
    let cookie = setup_admin(&app).await;
    let res = post("/api/v1/auth/totp/enroll", json!({}))
        .cookie(&cookie)
        .send(&app)
        .await;
    assert_eq!(
        res.headers.get("cache-control").unwrap().to_str().unwrap(),
        "no-store"
    );
}

#[tokio::test]
async fn a_replaced_web_key_is_refused_while_totp_secrets_exist() {
    use starr_secrets::TotpKey;
    let app = test_app().await;
    let db = &app.state.db;
    let first = TotpKey::generate();
    AppState::check_totp_key(db, &first).await.unwrap();
    // No secrets yet: a new key is simply recorded.
    let second = TotpKey::generate();
    AppState::check_totp_key(db, &second).await.unwrap();
    let settings = HttpSettings {
        public_url: Some(Url::parse(PUBLIC_URL).unwrap()),
        ..HttpSettings::default()
    };
    let state = AppState::new(db.clone(), settings)
        .with_clock(app.clock.clone())
        .with_totp_key(second);
    let state_token = state.issue_setup_token().await.unwrap().unwrap();
    let app = TestApp {
        router: app_router(state.clone()),
        state,
        clock: app.clock.clone(),
        setup_token: state_token,
    };
    let mut cookie = setup_admin(&app).await;
    enable_totp(&app, &mut cookie).await;
    assert!(
        AppState::check_totp_key(&app.state.db, &TotpKey::generate())
            .await
            .is_err()
    );
}
async fn recovery_codes_left(app: &TestApp, cookie: &str) -> i64 {
    get("/api/v1/auth/totp").cookie(cookie).send(app).await.body["recoveryCodesRemaining"]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn an_invalid_or_used_pending_token_never_burns_a_recovery_code() {
    let app = test_app().await;
    let mut cookie = setup_admin(&app).await;
    let (secret, codes) = enable_totp(&app, &mut cookie).await;

    // Unknown pending token with a valid recovery code.
    let res = code_step(&app, "made-up-token", &codes[0]).await;
    assert_eq!(res.code(), "auth.login_expired");
    assert_eq!(recovery_codes_left(&app, &cookie).await, 10);

    // Double submit: the first request (TOTP code) wins, the second
    // (recovery code) finds the pending login gone before any code is used.
    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    let res = code_step(&app, &pending, &totp_code(&secret, app.clock.now())).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = code_step(&app, &pending, &codes[0]).await;
    assert_eq!(res.code(), "auth.login_expired");
    assert_eq!(recovery_codes_left(&app, &cookie).await, 10);

    // Expired pending token: same.
    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    app.clock.advance(TimeDelta::minutes(6));
    let res = code_step(&app, &pending, &codes[0]).await;
    assert_eq!(res.code(), "auth.login_expired");
    let fresh = login(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    let pending = fresh.body["pendingToken"].as_str().unwrap().to_owned();
    let res = code_step(&app, &pending, &codes[0]).await;
    assert_eq!(res.status, StatusCode::OK, "the code was never spent");
}

#[tokio::test]
async fn a_wrong_code_keeps_the_pending_login_for_a_retry() {
    let app = test_app().await;
    let mut cookie = setup_admin(&app).await;
    let (secret, _) = enable_totp(&app, &mut cookie).await;
    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    assert_eq!(
        code_step(&app, &pending, "000000").await.code(),
        "auth.invalid_code"
    );
    let res = code_step(&app, &pending, &totp_code(&secret, app.clock.now())).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
}

#[tokio::test]
async fn enabling_totp_ends_all_other_sessions_and_rotates_the_current_one() {
    let app = test_app().await;
    let first = setup_admin(&app).await;
    let elsewhere = login(&app, ADMIN_USER, ADMIN_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    let mut current = first.clone();
    enable_totp(&app, &mut current).await;
    assert_ne!(current, first);
    assert_eq!(me(&app, &current).await.status, StatusCode::OK);
    assert_eq!(me(&app, &first).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&app, &elsewhere).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn recovery_codes_can_be_regenerated_with_password_and_totp_code() {
    let app = test_app().await;
    let mut cookie = setup_admin(&app).await;
    let (secret, old) = enable_totp(&app, &mut cookie).await;
    let regenerate = |password: &str, code: &str| {
        post(
            "/api/v1/auth/totp/recovery-codes",
            json!({ "password": password, "code": code }),
        )
        .cookie(&cookie)
    };

    let res = regenerate("wrong password!!", &totp_code(&secret, app.clock.now()))
        .send(&app)
        .await;
    assert_eq!(res.code(), "auth.current_password_invalid");
    // A recovery code is not accepted here, and not spent either.
    let res = regenerate(ADMIN_PASSWORD, &old[0]).send(&app).await;
    assert_eq!(res.code(), "auth.invalid_code");
    assert_eq!(recovery_codes_left(&app, &cookie).await, 10);

    let res = regenerate(ADMIN_PASSWORD, &totp_code(&secret, app.clock.now()))
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let new: Vec<String> = res.body["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(new.len(), 10);
    assert!(new.iter().all(|c| !old.contains(c)));
    assert!(
        audit_actions(&app)
            .await
            .contains(&"auth.recovery_codes_regenerated".to_owned())
    );

    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    assert_eq!(
        code_step(&app, &pending, &old[1]).await.code(),
        "auth.invalid_code",
        "old codes stop working"
    );
    let res = code_step(&app, &pending, &new[0]).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);

    // Without TOTP there is nothing to regenerate.
    let app = test_app().await;
    let plain = setup_admin(&app).await;
    let res = post(
        "/api/v1/auth/totp/recovery-codes",
        json!({ "password": ADMIN_PASSWORD, "code": "123456" }),
    )
    .cookie(&plain)
    .send(&app)
    .await;
    assert_eq!(res.code(), "totp.not_enabled");
}

#[tokio::test]
async fn wrong_elevation_passwords_are_throttled() {
    let w = linked_world().await;
    let member_view = switch(&w.app, &w.admin).await.session_cookie().unwrap();
    for _ in 0..5 {
        let res = elevate(
            &w.app,
            &member_view,
            json!({ "password": "wrong password!!" }),
        )
        .await;
        assert_eq!(res.code(), "auth.current_password_invalid");
    }
    let res = elevate(&w.app, &member_view, json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn a_code_used_once_is_refused_on_elevation() {
    let mut w = linked_world().await;
    let (secret, _) = enable_totp(&w.app, &mut w.admin).await;
    let code = totp_code(&secret, w.app.clock.now());

    let member_view = switch(&w.app, &w.admin).await.session_cookie().unwrap();
    let elevated = elevate(
        &w.app,
        &member_view,
        json!({ "password": ADMIN_PASSWORD, "code": code }),
    )
    .await;
    assert_eq!(elevated.status, StatusCode::OK, "{}", elevated.raw);
    let admin = switch(&w.app, &elevated.session_cookie().unwrap())
        .await
        .session_cookie()
        .unwrap();
    let member_view = switch(&w.app, &admin).await.session_cookie().unwrap();
    w.app.clock.advance(TimeDelta::seconds(10));
    let res = elevate(
        &w.app,
        &member_view,
        json!({ "password": ADMIN_PASSWORD, "code": code }),
    )
    .await;
    assert_eq!(res.code(), "auth.invalid_code");
}

#[tokio::test]
async fn a_code_used_once_is_refused_when_disabling() {
    let app = test_app().await;
    let mut cookie = setup_admin(&app).await;
    let (secret, _) = enable_totp(&app, &mut cookie).await;
    let code = totp_code(&secret, app.clock.now());
    let pending = password_step(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    assert_eq!(
        code_step(&app, &pending, &code).await.status,
        StatusCode::OK
    );
    let res = post(
        "/api/v1/auth/totp/disable",
        json!({ "password": ADMIN_PASSWORD, "code": code }),
    )
    .cookie(&cookie)
    .send(&app)
    .await;
    assert_eq!(res.code(), "auth.invalid_code");
    assert_eq!(me(&app, &cookie).await.body["totpEnabled"], true);
}
