//! Lesson 6 for account scans, in its own test binary (`tracing` caches
//! callsite interest process-wide, so a thread-local capture is only
//! reliable when no other test runs in parallel): neither the API key nor
//! anything the service echoes of it reaches the logs, on success and on a
//! failing listing.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use common::post;
use common::services::env;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "jellyfin-secret-key-0123456789abcdef";

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
async fn account_scans_never_log_the_key() {
    let capture = Capture(Arc::default());
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let env = env().await;
    let server = MockServer::start().await;
    let info = std::fs::read_to_string(format!(
        "{}/../adapters/jellyfin/fixtures/12.3.0/accounts/system_info.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/System/Info"))
        .and(header(
            "authorization",
            format!("MediaBrowser Token=\"{KEY}\"").as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(info))
        .mount(&server)
        .await;
    // The listing fails and echoes the received credential, as a careless
    // service might.
    Mock::given(method("GET"))
        .and(path("/Users"))
        .respond_with(|req: &wiremock::Request| {
            let auth = req
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            ResponseTemplate::new(500).set_body_string(format!("bad token {auth}"))
        })
        .mount(&server)
        .await;

    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "jellyfin", "name": "Kino", "baseUrl": server.uri(), "apiKey": KEY }),
        ))
        .await;
    let id = res.body["service"]["id"].as_i64().unwrap();
    env.send(post(
        "/api/v1/categories/movies/roles",
        json!({ "kind": "consume", "serviceId": id }),
    ))
    .await;
    env.send(post("/api/v1/jobs/accounts.scan/trigger", json!({})))
        .await;
    env.drain().await;

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("account listing failed") && logs.contains("[redacted]"),
        "expected log lines missing, the check would be vacuous:\n{logs}"
    );
    assert!(!logs.contains(KEY), "API key in logs:\n{logs}");
    // The run report (stored, shown to admins) carries no key either.
    let (report,): (String,) =
        sqlx::query_as("SELECT report FROM run WHERE kind = 'accounts.scan' ORDER BY id DESC")
            .fetch_one(env.app.state.db.pool())
            .await
            .unwrap();
    assert!(report.contains("transient"), "{report}");
    assert!(!report.contains(KEY), "{report}");
}
