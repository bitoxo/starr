//! The credential never appears in logs or error details, even when the
//! service echoes it back (lesson 6).
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use secrecy::SecretString;
use starr_adapter_kit::{AuthScheme, Endpoint, ServiceClient};
use starr_core::adapters::{
    AdapterDescriptor, AdapterError, Budgets, IdentityRule, RateLimit, SecretClass, Version,
    VersionRange,
};
use tracing::Level;
use url::Url;
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "super-secret-api-key-4711";

const DESCRIPTOR: AdapterDescriptor = AdapterDescriptor {
    type_id: "test",
    display_name: "Test",
    capabilities: &[],
    tested_versions: VersionRange {
        min: Version::new(1, 0, 0, 0),
        max_exclusive: Version::new(2, 0, 0, 0),
    },
    budgets: Budgets {
        rate_limit: RateLimit {
            requests: 100,
            per: Duration::from_secs(1),
        },
        login_limit: None,
        expensive_endpoints: &[],
    },
    identity: IdentityRule::SingleUser,
    secret_class: SecretClass::ServiceCredential,
    lists: &[],
};

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
async fn echoed_credentials_are_redacted_from_logs_and_errors() {
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let server = MockServer::start().await;
    Mock::given(path("/fails"))
        .and(header("x-api-key", KEY))
        .respond_with(
            ResponseTemplate::new(500)
                .insert_header("x-echo-key", KEY)
                // The key straddles the 300-char snippet cut.
                .set_body_string(format!("{}{KEY}", "x".repeat(290))),
        )
        .mount(&server)
        .await;
    Mock::given(path("/garbage"))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!("<p>{KEY}</p>")))
        .mount(&server)
        .await;

    Mock::given(path("/rejects"))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(400).set_body_string(format!(
            "[{{\"errorMessage\": \"Root folder does not exist for key {KEY}\", \"attemptedValue\": \"{KEY}\"}}]"
        )))
        .mount(&server)
        .await;

    let client = ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(&server.uri()).unwrap(),
            auth: AuthScheme::ApiKeyHeader("X-Api-Key"),
            credential: SecretString::from(KEY.to_owned()),
        },
    )
    .unwrap();

    let errors = [
        client.get_json::<serde_json::Value>("fails").await,
        client.get_json::<serde_json::Value>("garbage").await,
    ];
    assert!(matches!(errors[0], Err(AdapterError::Transient(_))));
    assert!(matches!(errors[1], Err(AdapterError::Unknown(_))));
    // A rejection carries the service's reason (its validation message,
    // not the raw body), with the key redacted.
    let rejected = client
        .post_json::<_, serde_json::Value>("rejects", &serde_json::json!({}))
        .await;
    match &rejected {
        Err(AdapterError::Rejected(detail)) => {
            assert!(detail.contains("Root folder does not exist"), "{detail}");
            assert!(detail.contains("[redacted]"), "{detail}");
            assert!(!detail.contains(KEY), "{detail}");
            assert!(!detail.contains("attemptedValue"), "{detail}");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    for err in &errors {
        let shown = format!("{err:?}");
        assert!(!shown.contains(KEY), "{shown}");
    }
    assert!(!format!("{client:?}").contains(KEY));

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("[redacted]"),
        "echoed key was not logged at all, the check would be vacuous:\n{logs}"
    );
    assert!(!logs.contains(KEY), "credential in logs:\n{logs}");
    // Not even a prefix survives the snippet cut.
    assert!(
        !logs.contains(&KEY[..10]),
        "credential prefix in logs:\n{logs}"
    );
}

/// A webhook URL is the credential (Discord's carries its token): it is
/// posted to as given and never shows up in logs, errors or `Debug`.
#[tokio::test]
async fn webhook_urls_are_redacted_from_logs_errors_and_debug() {
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let server = MockServer::start().await;
    let token = "hook-token-0815";
    Mock::given(path(format!("/api/webhooks/42/{token}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(path(format!("/api/webhooks/43/{token}")))
        .respond_with(ResponseTemplate::new(400).set_body_string(format!("bad token {token}")))
        .mount(&server)
        .await;
    let rate = RateLimit {
        requests: 10,
        per: Duration::from_secs(1),
    };
    let ok = SecretString::from(format!("{}/api/webhooks/42/{token}", server.uri()));
    let client = ServiceClient::secret_url("discord", rate, &ok).unwrap();
    client
        .post_to_url(&serde_json::json!({"content": "hi"}))
        .await
        .unwrap();
    assert!(!format!("{client:?}").contains(token));

    let bad = SecretString::from(format!("{}/api/webhooks/43/{token}", server.uri()));
    let client = ServiceClient::secret_url("discord", rate, &bad).unwrap();
    let err = client
        .post_to_url(&serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, AdapterError::Rejected(_)), "{err:?}");
    assert!(!err.to_string().contains(token), "{err}");

    // Nothing listens here: the transport error must not quote the URL.
    let closed = SecretString::from(format!("http://127.0.0.1:9/api/webhooks/44/{token}"));
    let client = ServiceClient::secret_url("discord", rate, &closed).unwrap();
    let err = client
        .post_to_url(&serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(!err.to_string().contains(token), "{err}");

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!logs.contains(token), "{logs}");
    assert!(
        ServiceClient::secret_url("discord", rate, &SecretString::from("ftp://x/y".to_owned()))
            .is_err()
    );
}
/// A password sent to a service (spec 0005 slice 4, M10 4) never shows up
/// in logs or error details, even when the service echoes the request body
/// in its error reply.
#[tokio::test]
async fn echoed_passwords_never_reach_logs_or_errors() {
    const PASSWORD: &str = "member-pw-Ünïcødé-4711";
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let server = MockServer::start().await;
    for (route, status) in [("/Users/Password", 400), ("/Users/New", 500)] {
        Mock::given(path(route))
            .and(header("x-api-key", KEY))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_string(format!("{{\"error\":\"bad NewPw {PASSWORD}\"}}")),
            )
            .mount(&server)
            .await;
    }
    let client = ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(&server.uri()).unwrap(),
            auth: AuthScheme::ApiKeyHeader("X-Api-Key"),
            credential: SecretString::from(KEY.to_owned()),
        },
    )
    .unwrap();
    let body = serde_json::json!({ "NewPw": PASSWORD });
    let rejected = client
        .post_json_no_reply(
            "Users/Password",
            &[("userId", "a1".to_owned())],
            &body,
            true,
        )
        .await;
    assert!(
        matches!(rejected, Err(AdapterError::Rejected(_))),
        "{rejected:?}"
    );
    let failed = client
        .post_json_sensitive::<_, serde_json::Value>("Users/New", &body, true)
        .await;
    assert!(
        matches!(failed, Err(AdapterError::Transient(_))),
        "{failed:?}"
    );
    for shown in [format!("{rejected:?}"), format!("{failed:?}")] {
        assert!(!shown.contains(PASSWORD), "{shown}");
    }
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("service returned an error"),
        "the failures were not logged at all, the check would be vacuous:\n{logs}"
    );
    assert!(!logs.contains(PASSWORD), "password in logs:\n{logs}");
    assert!(
        !logs.contains("member-pw"),
        "password prefix in logs:\n{logs}"
    );
}

/// Spec 0007: a client for private content (the adult catalog and source)
/// logs neither the request path (it can carry a `StashDB` UUID) nor the
/// service's words, at any level.
#[tokio::test]
async fn private_content_clients_log_no_path_and_no_foreign_text() {
    const UUID: &str = "7f3c1a52-0d4e-4b8a-9c61-2a5e8f9b0c11";
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let server = MockServer::start().await;
    Mock::given(path(format!("/scene/{UUID}")))
        .respond_with(ResponseTemplate::new(500).set_body_string("Scene A by Performer P"))
        .mount(&server)
        .await;
    Mock::given(path("/garbage"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<p>Studio S</p>"))
        .mount(&server)
        .await;
    let client = ServiceClient::with_options(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(&server.uri()).unwrap(),
            auth: AuthScheme::ApiKeyHeader("X-Api-Key"),
            credential: SecretString::from(KEY.to_owned()),
        },
        starr_adapter_kit::ClientOptions {
            rate_limit: DESCRIPTOR.budgets.rate_limit,
            unavailable_is_throttle: false,
            max_wait: None,
            private_content: true,
        },
    )
    .unwrap();
    let failed = client
        .get_json::<serde_json::Value>(&format!("scene/{UUID}?foreignId={UUID}"))
        .await;
    assert!(failed.is_err());
    let garbage = client.get_json::<serde_json::Value>("garbage").await;
    assert!(garbage.is_err());
    // The errors themselves (they reach run reports, logs of callers and
    // `last_error`) carry neither the path nor the body.
    for err in [failed.unwrap_err(), garbage.unwrap_err()] {
        let text = err.to_string();
        for marker in [UUID, "Scene A", "Performer P", "Studio S"] {
            assert!(!text.contains(marker), "the error names {marker:?}: {text}");
        }
    }

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("[private]"), "{logs}");
    for marker in [UUID, "Scene A", "Performer P", "Studio S"] {
        assert!(!logs.contains(marker), "the logs name {marker:?}: {logs}");
    }
}
