//! Lesson 6 for member passwords (spec 0005 M10 4, ADR 0010), in its own
//! test binary (`tracing` caches callsite interest process-wide, so a
//! thread-local capture is only reliable when no other test runs in
//! parallel): accepting an invitation, the worker creating the account
//! from the handoff, a password change and wrong current passwords never
//! put a member's password into the logs, even at TRACE.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use chrono::Utc;
use common::services::env;
use common::{Req, get, post};
use serde_json::json;
use starr_adapter_kit::AuthScheme;
use starr_adapter_kit::contract::media_server::{FakeFlavor, MediaServerFake, Recorded};

const KEY: &str = "jellyfin-passwords-key-0123456789";
const MOVIES: &str = "f137a2dd21bbc1b99aa5c0f6bf02a805";
const FIRST: &str = "first-secret-Kinoabend";
const SECOND: &str = "second-secret-Filmnacht";
const WRONG: &str = "wrong-secret-Popcorn";

#[derive(Clone)]
struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one story, every step logged
async fn member_passwords_never_reach_the_logs() {
    let capture = Capture(Arc::default());
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut env = env().await;
    env.app.clock.advance(Utc::now() - env.app.clock.now());
    let fake = MediaServerFake::start(
        FakeFlavor::Jellyfin,
        AuthScheme::MediaBrowserToken,
        KEY,
        Recorded::from_dir(&format!(
            "{}/../adapters/jellyfin/fixtures/12.3.0/accounts",
            env!("CARGO_MANIFEST_DIR")
        )),
        "friend",
    )
    .await;
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "jellyfin", "name": "Kino", "baseUrl": fake.server.uri(), "apiKey": KEY }),
        ))
        .await;
    let id = res.body["service"]["id"].as_i64().unwrap();
    env.send(Req::new(
        Method::PATCH,
        "/api/v1/categories/movies",
        Some(json!({ "enabled": true })),
    ))
    .await;
    let res = env
        .send(post(
            "/api/v1/categories/movies/roles",
            json!({ "kind": "consume", "serviceId": id }),
        ))
        .await;
    let role = res.body["consumers"][0]["id"].clone();
    env.send(post("/api/v1/jobs/accounts.scan/trigger", json!({})))
        .await;
    env.drain().await;
    env.send(Req::new(
        Method::PUT,
        &format!("/api/v1/categories/movies/roles/{role}/libraries"),
        Some(json!({ "libraryIds": [MOVIES] })),
    ))
    .await;
    env.drain().await;
    env.enable_totp().await;
    env.elevate().await;
    let plan = env
        .send(get(&format!("/api/v1/services/{id}/account-plan")))
        .await;
    let res = env
        .send(post(
            &format!("/api/v1/services/{id}/account-plan/confirm"),
            json!({ "fingerprint": plan.body["fingerprint"] }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.drain().await;

    // Accept an invitation: the account is created from the handoff.
    let res = env
        .send(post(
            "/api/v1/invitations",
            json!({ "username": "mia", "entitlements": [{ "category": "movies", "enabled": true, "requestMode": "approval" }] }),
        ))
        .await;
    let token = res.body["link"]["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    let res = post(
        &format!("/api/v1/invitations/{token}/accept"),
        json!({ "email": "mia@example.org", "password": FIRST }),
    )
    .send(&env.app)
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let mia = res.session_cookie().unwrap();
    env.drain().await;
    assert!(fake.user("mia").is_some(), "created");

    // A wrong current password, then a change written to the account.
    let res = post(
        "/api/v1/me/password",
        json!({ "currentPassword": WRONG, "newPassword": SECOND }),
    )
    .cookie(&mia)
    .send(&env.app)
    .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    let res = post(
        "/api/v1/me/password",
        json!({ "currentPassword": FIRST, "newPassword": SECOND }),
    )
    .cookie(&mia)
    .send(&env.app)
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    env.drain().await;
    let res = post("/api/v1/me/services/activate", json!({ "password": WRONG }))
        .cookie(&mia)
        .send(&env.app)
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    let sent = fake.writes().await;
    assert!(
        sent.iter().any(|w| w.body.to_string().contains(SECOND)),
        "the new password was written: {sent:?}"
    );

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("accounts.password"),
        "expected log lines missing, the check would be vacuous:\n{logs}"
    );
    for secret in [FIRST, SECOND, WRONG, "Kinoabend", "Filmnacht"] {
        assert!(!logs.contains(secret), "{secret} in logs:\n{logs}");
    }
}
