//! Lesson 6 for service credentials, in its own test binary: `tracing`
//! caches callsite interest process-wide, so a thread-local capture is only
//! reliable when no other test runs in parallel.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use common::services::{API_KEY, env, patch};
use serde_json::json;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

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

/// Lesson 6: the API key appears in no log line, from the create request to
/// a failing check whose response echoes it.
#[tokio::test]
async fn service_credentials_never_reach_the_logs() {
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
    Mock::given(path("/api/v3/system/status"))
        // Echo the received key back, as a careless service might.
        .respond_with(|req: &wiremock::Request| {
            let key = req
                .headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            ResponseTemplate::new(500).set_body_string(format!("invalid key {key}"))
        })
        .mount(&server)
        .await;
    let res = env.create("Movies", &server.uri(), API_KEY).await;
    let id = res.body["service"]["id"].as_i64().unwrap();
    env.drain().await;
    env.send(patch(
        &format!("/api/v1/services/{id}"),
        json!({ "apiKey": format!("{API_KEY}-2") }),
    ))
    .await;
    env.drain().await;

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("service check failed") && logs.contains("[redacted]"),
        "expected log lines missing, the check would be vacuous:\n{logs}"
    );
    assert!(!logs.contains(API_KEY), "API key in logs:\n{logs}");
}
