//! Lesson 6: no password, setup token or session token appears in logs.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use common::{ADMIN_PASSWORD, ADMIN_USER, get, login, post, setup_body, test_app};
use secrecy::ExposeSecret;
use serde_json::json;
use tracing::Level;

/// Appends formatted log lines to a shared buffer.
#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

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
async fn auth_flows_never_log_secrets() {
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let app = test_app().await;
    let token = app.setup_token.expose_secret().to_owned();
    let wrong_password = "hunter2-but-longer";

    post("/api/v1/setup/admin", setup_body("wrong-setup-token-value"))
        .send(&app)
        .await;
    let setup = post("/api/v1/setup/admin", setup_body(&token))
        .send(&app)
        .await;
    let setup_cookie = setup.session_cookie().unwrap();
    login(&app, ADMIN_USER, wrong_password).await;
    // Password typed into the username field.
    login(&app, ADMIN_PASSWORD, wrong_password).await;
    let session = login(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    let cookie = session.session_cookie().unwrap();
    get("/api/v1/auth/me").cookie(&cookie).send(&app).await;
    post("/api/v1/auth/logout", json!({}))
        .cookie(&cookie)
        .send(&app)
        .await;

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("/api/v1/auth/login"),
        "nothing captured, the check would be vacuous:\n{logs}"
    );
    let token_value = |c: &str| c.trim_start_matches("starr_session=").to_owned();
    for secret in [
        token,
        "wrong-setup-token-value".to_owned(),
        ADMIN_PASSWORD.to_owned(),
        wrong_password.to_owned(),
        token_value(&setup_cookie),
        token_value(&cookie),
    ] {
        assert!(
            !logs.contains(&secret),
            "secret `{secret}` in logs:\n{logs}"
        );
    }
}
#[tokio::test]
async fn totp_flows_never_log_secrets_codes_or_pending_tokens() {
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let app = test_app().await;
    let setup = post(
        "/api/v1/setup/admin",
        setup_body(app.setup_token.expose_secret()),
    )
    .send(&app)
    .await;
    let cookie = setup.session_cookie().unwrap();
    let enroll = post("/api/v1/auth/totp/enroll", json!({}))
        .cookie(&cookie)
        .send(&app)
        .await;
    let secret = enroll.body["secret"].as_str().unwrap().to_owned();
    let totp = totp_rs::Builder::new()
        .with_secret(totp_rs::Secret::try_from_base32(&secret).unwrap())
        .build()
        .unwrap();
    let code_at = |app: &common::TestApp| {
        totp.generate(u64::try_from(app.clock.now().timestamp()).unwrap())
            .to_string()
    };
    let confirm_code = code_at(&app);
    let confirm = post("/api/v1/auth/totp/confirm", json!({ "code": confirm_code }))
        .cookie(&cookie)
        .send(&app)
        .await;
    let recovery: Vec<String> = confirm.body["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect();
    app.clock.advance(chrono::TimeDelta::seconds(30));
    let challenge = login(&app, ADMIN_USER, ADMIN_PASSWORD).await;
    let pending = challenge.body["pendingToken"].as_str().unwrap().to_owned();
    let login_code = code_at(&app);
    post(
        "/api/v1/auth/login/totp",
        json!({ "pendingToken": pending, "code": login_code }),
    )
    .send(&app)
    .await;

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("/api/v1/auth/login/totp"),
        "nothing captured, the check would be vacuous:\n{logs}"
    );
    let mut secrets = vec![secret, pending, confirm_code, login_code];
    secrets.extend(recovery);
    for secret in secrets {
        assert!(
            !logs.contains(&secret),
            "secret `{secret}` in logs:\n{logs}"
        );
    }
}
