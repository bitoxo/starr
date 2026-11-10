//! Concurrency on a real (file-backed, multi-connection) database: limits
//! that hold one request at a time must also hold under parallel requests.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::http::{Method, StatusCode};
use common::{
    ADMIN_PASSWORD, ADMIN_USER, PUBLIC_URL, Req, TestApp, elevate_in_place, login_body, post,
    send_to,
};
use secrecy::ExposeSecret;
use serde_json::json;
use starr_db::Database;
use starr_server::clock::Clock;
use starr_server::{AppState, HttpSettings, app};
use url::Url;

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("starr-race-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// App on a file database with the production pool (8 connections, WAL).
async fn file_app(dir: &TempDir) -> TestApp {
    let db = Database::open(&dir.0.join("starr.db"), &dir.0.join("backups"))
        .await
        .unwrap();
    let clock = Clock::manual(common::t0());
    let settings = HttpSettings {
        public_url: Some(Url::parse(PUBLIC_URL).unwrap()),
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

async fn parallel(app: &TestApp, requests: Vec<Req>) -> Vec<StatusCode> {
    let handles: Vec<_> = requests
        .into_iter()
        .map(|req| {
            let router = app.router.clone();
            tokio::spawn(async move { send_to(&router, req).await.status })
        })
        .collect();
    let mut statuses = Vec::new();
    for handle in handles {
        statuses.push(handle.await.unwrap());
    }
    statuses
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn parallel_failed_logins_never_exceed_the_username_limit() {
    let dir = TempDir::new();
    let app = file_app(&dir).await;
    common::setup_admin(&app).await;

    let requests = (0..30)
        .map(|i| {
            post(
                "/api/v1/auth/login",
                login_body(ADMIN_USER, "wrong password!!"),
            )
            .peer(&format!("198.51.100.{i}:4000"))
        })
        .collect();
    let statuses = parallel(&app, requests).await;
    let let_through = statuses
        .iter()
        .filter(|s| **s != StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!(let_through, 5, "{statuses:?}");
    assert!(
        statuses
            .iter()
            .all(|s| matches!(*s, StatusCode::UNAUTHORIZED | StatusCode::TOO_MANY_REQUESTS))
    );
    // And the right password is throttled too now.
    let res = post("/api/v1/auth/login", login_body(ADMIN_USER, ADMIN_PASSWORD))
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn parallel_setup_creates_exactly_one_admin() {
    let dir = TempDir::new();
    let app = file_app(&dir).await;
    let token = app.setup_token.expose_secret().to_owned();

    let requests = (0..10)
        .map(|i| {
            post(
                "/api/v1/setup/admin",
                json!({
                    "token": token,
                    "username": format!("admin{i}"),
                    "password": ADMIN_PASSWORD,
                }),
            )
            .peer(&format!("198.51.100.{i}:4000"))
        })
        .collect();
    let statuses = parallel(&app, requests).await;
    let created = statuses
        .iter()
        .filter(|s| **s == StatusCode::CREATED)
        .count();
    assert_eq!(created, 1, "{statuses:?}");
    assert!(statuses.iter().all(|s| matches!(
        *s,
        StatusCode::CREATED | StatusCode::CONFLICT | StatusCode::FORBIDDEN
    )));
    let (admins,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM admin_account")
        .fetch_one(app.state.db.pool())
        .await
        .unwrap();
    assert_eq!(admins, 1);
}
/// Create admins `names` (via the API, password set via their link).
async fn add_admins(app: &TestApp, root: &str, names: &[&str]) {
    elevate_in_place(app, root).await;
    for name in names {
        let res = post("/api/v1/admins", json!({ "username": name }))
            .cookie(root)
            .send(app)
            .await;
        assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
        let url = res.body["setPasswordLink"]["url"].as_str().unwrap();
        let token = url.rsplit('/').next().unwrap();
        let res = post(
            &format!("/api/v1/password-tokens/{token}"),
            json!({ "password": ADMIN_PASSWORD }),
        )
        .send(app)
        .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    }
}

async fn admin_ids(app: &TestApp) -> Vec<(String, i64)> {
    sqlx::query_as("SELECT username, id FROM admin_account ORDER BY id")
        .fetch_all(app.state.db.pool())
        .await
        .unwrap()
}

async fn active_admins(app: &TestApp) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM admin_account WHERE suspended_at IS NULL AND removed_at IS NULL",
    )
    .fetch_one(app.state.db.pool())
    .await
    .unwrap();
    n
}

async fn login_all(app: &TestApp, admins: &[(String, i64)]) -> Vec<(i64, String)> {
    let mut sessions = Vec::new();
    for (name, id) in admins {
        let res = post("/api/v1/auth/login", login_body(name, ADMIN_PASSWORD))
            .send(app)
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        let cookie = res.session_cookie().unwrap();
        // Suspending and removing admins need a fresh elevation.
        elevate_in_place(app, &cookie).await;
        sessions.push((*id, cookie));
    }
    sessions
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn admins_suspending_each_other_in_parallel_always_leave_one_active() {
    let dir = TempDir::new();
    let app = file_app(&dir).await;
    let root = common::setup_admin(&app).await;
    add_admins(&app, &root, &["ada", "bob"]).await;
    let admins = admin_ids(&app).await;

    for round in 0..15 {
        // Everyone logs in, then everyone suspends everyone else at once.
        let sessions = login_all(&app, &admins).await;
        let mut requests = Vec::new();
        for (actor, cookie) in &sessions {
            for (_, target) in &admins {
                if target != actor {
                    requests.push(
                        post(&format!("/api/v1/admins/{target}/suspend"), json!({})).cookie(cookie),
                    );
                }
            }
        }
        let statuses = parallel(&app, requests).await;
        assert!(
            statuses.iter().all(|s| matches!(
                *s,
                StatusCode::OK
                    | StatusCode::CONFLICT
                    | StatusCode::UNAUTHORIZED
                    | StatusCode::FORBIDDEN
            )),
            "round {round}: {statuses:?}"
        );
        assert!(
            active_admins(&app).await >= 1,
            "round {round}: no active admin left ({statuses:?})"
        );
        sqlx::query("UPDATE admin_account SET suspended_at = NULL")
            .execute(app.state.db.pool())
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn admins_removing_each_other_in_parallel_leave_one_active() {
    let dir = TempDir::new();
    let app = file_app(&dir).await;
    let root = common::setup_admin(&app).await;
    add_admins(&app, &root, &["ada", "bob", "cyd"]).await;
    let admins = admin_ids(&app).await;
    let sessions = login_all(&app, &admins).await;
    let mut requests = Vec::new();
    for (actor, cookie) in &sessions {
        for (target_name, target) in &admins {
            if target != actor {
                requests.push(
                    Req::new(
                        Method::DELETE,
                        &format!("/api/v1/admins/{target}"),
                        Some(json!({ "username": target_name })),
                    )
                    .cookie(cookie),
                );
            }
        }
    }
    let statuses = parallel(&app, requests).await;
    assert!(active_admins(&app).await >= 1, "{statuses:?}");
    let removed = statuses
        .iter()
        .filter(|s| **s == StatusCode::NO_CONTENT)
        .count();
    assert!((1..=3).contains(&removed), "{statuses:?}");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn racing_second_steps_spend_a_recovery_code_only_for_the_winner() {
    let dir = TempDir::new();
    let app = file_app(&dir).await;
    let mut cookie = common::setup_admin(&app).await;
    let enroll = post("/api/v1/auth/totp/enroll", json!({}))
        .cookie(&cookie)
        .send(&app)
        .await;
    let secret = totp_rs::Secret::try_from_base32(enroll.body["secret"].as_str().unwrap()).unwrap();
    let totp = totp_rs::Builder::new().with_secret(secret).build().unwrap();
    let code_now = |app: &TestApp| {
        totp.generate(u64::try_from(app.clock.now().timestamp()).unwrap())
            .to_string()
    };
    let res = post(
        "/api/v1/auth/totp/confirm",
        json!({ "code": code_now(&app) }),
    )
    .cookie(&cookie)
    .send(&app)
    .await;
    cookie = res.session_cookie().unwrap();
    let mut recovery: Vec<String> = res.body["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect();

    let mut spent = 0;
    for round in 0..5 {
        app.clock.advance(chrono::TimeDelta::seconds(30));
        let res = post("/api/v1/auth/login", login_body(ADMIN_USER, ADMIN_PASSWORD))
            .send(&app)
            .await;
        let pending = res.body["pendingToken"].as_str().unwrap().to_owned();
        let code = recovery.pop().unwrap();
        let requests = vec![
            post(
                "/api/v1/auth/login/totp",
                json!({ "pendingToken": pending, "code": code_now(&app) }),
            ),
            post(
                "/api/v1/auth/login/totp",
                json!({ "pendingToken": pending, "code": code }),
            ),
        ];
        let statuses = parallel(&app, requests).await;
        assert_eq!(
            statuses.iter().filter(|s| **s == StatusCode::OK).count(),
            1,
            "round {round}: {statuses:?}"
        );
        if statuses[1] == StatusCode::OK {
            spent += 1;
        }
        let left = common::get("/api/v1/auth/totp")
            .cookie(&cookie)
            .send(&app)
            .await
            .body["recoveryCodesRemaining"]
            .as_i64()
            .unwrap();
        assert_eq!(
            left,
            10 - spent,
            "round {round}: a losing request spent its code"
        );
    }
}
