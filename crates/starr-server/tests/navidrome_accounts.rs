//! Navidrome accounts through the API (spec 0006 slice 7, Q9 A; spec 0005
//! D9, M9), in its own test binary so the log capture is reliable
//! (`tracing` caches callsite interest process-wide): an invited member
//! gets a Navidrome account with the music library, suspension locks it
//! with a random password (Navidrome has no off switch), reactivation asks
//! the member to activate, and their password unlocks it. Neither the
//! member's nor the admin's password reaches the logs, the database or an
//! API response. Only a local fake is contacted (derived fixtures).
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use chrono::Utc;
use common::services::env;
use common::{Req, get, post};
use serde_json::{Value, json};
use starr_adapter_kit::contract::navidrome::{Derived, NavidromeFake};

const ND_ADMIN_PASSWORD: &str = "nd-admin-Kammerton";
const FIRST: &str = "first-secret-Plattenteller";

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

fn my_service(body: &Value) -> Value {
    body["services"][0].clone()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one story, every step logged
async fn suspension_locks_navidrome_and_the_member_password_unlocks_it() {
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
    let fake = NavidromeFake::start(
        Derived::from_dir(&format!(
            "{}/../adapters/navidrome/fixtures/0.64.2/derived",
            env!("CARGO_MANIFEST_DIR")
        )),
        &[("starr", ND_ADMIN_PASSWORD)],
    )
    .await;
    env.send(Req::new(
        Method::PATCH,
        "/api/v1/categories/music",
        Some(json!({ "enabled": true })),
    ))
    .await;
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "navidrome", "name": "Navidrome", "baseUrl": fake.server.uri(),
                    "publicUrl": "https://music.example.org", "username": "starr",
                    "password": ND_ADMIN_PASSWORD }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let id = res.body["service"]["id"].as_i64().unwrap();
    env.drain().await;
    let res = env
        .send(post(
            "/api/v1/categories/music/roles",
            json!({ "kind": "consume", "serviceId": id }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let role = res.body["consumers"][0]["id"].clone();
    env.send(post("/api/v1/jobs/accounts.scan/trigger", json!({})))
        .await;
    env.drain().await;
    let res = env
        .send(Req::new(
            Method::PUT,
            &format!("/api/v1/categories/music/roles/{role}/libraries"),
            Some(json!({ "libraryIds": ["1"] })),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.drain().await;

    // The matrix says how Navidrome disables; the foreign inventory shows
    // the admins as privileged and nothing was written.
    let matrix = env.send(get("/api/v1/accounts/matrix")).await;
    assert_eq!(
        matrix.body["services"][0]["disablesByPassword"], true,
        "{}",
        matrix.raw
    );
    let accounts = env
        .send(get(&format!("/api/v1/services/{id}/accounts")))
        .await;
    let privileged: Vec<_> = accounts.body["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["privileged"] == true)
        .map(|a| a["username"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(privileged, ["morgan", "starr"]);
    assert_eq!(fake.writes().await, vec![]);

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
    assert_eq!(
        fake.writes().await,
        vec![],
        "the existing friends are undecided"
    );

    // Accepting an invitation creates the account from the handoff.
    let res = env
        .send(post(
            "/api/v1/invitations",
            json!({ "username": "mia", "entitlements": [{ "category": "music", "enabled": true, "requestMode": "approval" }] }),
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
    env.drain().await;
    assert!(
        fake.login_works("mia", FIRST),
        "created with the Starr password"
    );
    let mia_remote = fake.user("mia").unwrap()["id"].as_str().unwrap().to_owned();
    assert_eq!(fake.grants(&mia_remote), vec![1], "only the music library");
    let members = env.send(get("/api/v1/members")).await;
    let mia = members
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["username"] == "mia")
        .unwrap()["id"]
        .as_i64()
        .unwrap();

    // Suspension: a random password, recorded as Starr's lock.
    let res = env
        .send(post(&format!("/api/v1/members/{mia}/suspend"), json!({})))
        .await;
    assert!(res.status.is_success(), "{}", res.raw);
    env.drain().await;
    assert!(!fake.login_works("mia", FIRST), "locked out");
    let matrix = env.send(get("/api/v1/accounts/matrix")).await;
    let cell = matrix.body["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["username"] == "mia")
        .unwrap()["cells"][0]
        .clone();
    assert_eq!(cell["lockedByStarr"], true, "{cell}");
    let accounts = env
        .send(get(&format!("/api/v1/services/{id}/accounts")))
        .await;
    let row = accounts.body["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["remoteId"] == mia_remote.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["lockedByStarr"], true);

    // Reactivation writes nothing: My services asks to activate.
    let before = fake.writes().await.len();
    let res = env
        .send(post(
            &format!("/api/v1/members/{mia}/reactivate"),
            json!({}),
        ))
        .await;
    assert!(res.status.is_success(), "{}", res.raw);
    env.drain().await;
    assert_eq!(fake.writes().await.len(), before, "nothing to enable");
    let res = post(
        "/api/v1/auth/login",
        json!({ "username": "mia", "password": FIRST }),
    )
    .send(&env.app)
    .await;
    assert!(res.status.is_success(), "{}", res.raw);
    let cookie = res.session_cookie().unwrap();
    let mine = get("/api/v1/me/services")
        .cookie(&cookie)
        .send(&env.app)
        .await;
    let service = my_service(&mine.body);
    assert_eq!(service["state"], "activate", "{}", mine.raw);
    assert_eq!(service["lockedByStarr"], true);
    assert_eq!(service["passwordChangeSignsOut"], true);

    // Activate: the member's password unlocks the account.
    let res = post("/api/v1/me/services/activate", json!({ "password": FIRST }))
        .cookie(&cookie)
        .send(&env.app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.drain().await;
    assert!(fake.login_works("mia", FIRST), "unlocked");
    let mine = get("/api/v1/me/services")
        .cookie(&cookie)
        .send(&env.app)
        .await;
    let service = my_service(&mine.body);
    assert_eq!(service["state"], "ready", "{}", mine.raw);
    assert_eq!(service["lockedByStarr"], false);
    // Starr's own admin account and the operator's were never written.
    for w in fake.writes().await {
        assert!(!w.target.contains("6aB3kQ9zXw1Lm4Np7Rs2Tu"), "{w:?}");
        assert!(!w.target.contains("0Fq7Hc2Vd8Jk5Lp3Mn9Rb1"), "{w:?}");
    }

    // No password anywhere but in the requests that carried them.
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("accounts.password"),
        "expected log lines missing, the check would be vacuous:\n{logs}"
    );
    let scrambled: Vec<String> = fake
        .writes()
        .await
        .iter()
        .filter_map(|w| w.body["password"].as_str().map(str::to_owned))
        .filter(|p| p != FIRST)
        .collect();
    assert_eq!(scrambled.len(), 2, "the create and the suspension");
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT coalesce(details, '') FROM audit_log
         UNION ALL SELECT plan FROM account_plan
         UNION ALL SELECT coalesce(last_error_detail, '') FROM service_account
         UNION ALL SELECT coalesce(report, '') FROM run",
    )
    .fetch_all(env.app.state.db.pool())
    .await
    .unwrap();
    let stored: String = rows
        .into_iter()
        .map(|(t,)| t)
        .collect::<Vec<_>>()
        .join("\n");
    let shown = [matrix.raw, accounts.raw, mine.raw].join("\n");
    for secret in [FIRST, ND_ADMIN_PASSWORD]
        .iter()
        .copied()
        .chain(scrambled.iter().map(String::as_str))
    {
        assert!(!logs.contains(secret), "{secret} in logs");
        assert!(!stored.contains(secret), "{secret} stored");
        assert!(!shown.contains(secret), "{secret} shown");
    }
}
