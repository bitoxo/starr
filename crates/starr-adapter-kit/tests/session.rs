//! Session auth (`AuthScheme::Session`, Navidrome): the stored login is
//! readable only by the adapter, the token travels as a bearer in the
//! scheme's header, a renewed token in a response replaces it, logins keep
//! to the login budget, and totals come from `X-Total-Count`.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use starr_adapter_kit::{AuthScheme, Endpoint, LoginCredential, ServiceClient};
use starr_core::adapters::{
    AdapterDescriptor, AdapterError, Budgets, IdentityRule, RateLimit, SecretClass, Version,
    VersionRange,
};
use url::Url;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PASSWORD: &str = "pässw0rd \"quoted\" secret-4711";

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
        login_limit: Some(RateLimit {
            requests: 1,
            per: Duration::from_secs(20),
        }),
        expensive_endpoints: &[],
    },
    identity: IdentityRule::SingleUser,
    secret_class: SecretClass::ServiceCredential,
    lists: &[],
};

fn session(uri: &str) -> ServiceClient {
    ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(uri).unwrap(),
            auth: AuthScheme::Session("X-ND-Authorization"),
            credential: LoginCredential::encode("starr", &SecretString::from(PASSWORD.to_owned())),
        },
    )
    .unwrap()
}

#[test]
fn logins_round_trip_and_garbage_is_refused_without_quoting_it() {
    let stored = LoginCredential::encode("admin user", &SecretString::from(PASSWORD.to_owned()));
    let login = LoginCredential::decode(&stored).unwrap();
    assert_eq!(login.username, "admin user");
    assert_eq!(login.password.expose_secret(), PASSWORD);
    let err = LoginCredential::decode(&SecretString::from("plain-api-key".to_owned())).unwrap_err();
    assert!(!err.to_string().contains("plain-api-key"), "{err}");
    assert!(!format!("{login:?}").contains("secret-4711"));
    // A client of the scheme needs a readable login.
    let refused = ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse("http://localhost:9").unwrap(),
            auth: AuthScheme::Session("X-ND-Authorization"),
            credential: SecretString::from("plain-api-key".to_owned()),
        },
    );
    assert!(matches!(refused, Err(AdapterError::Rejected(_))));
}

#[tokio::test]
async fn the_token_is_sent_renewed_from_responses_and_cleared_on_demand() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/first"))
        .and(header("x-nd-authorization", "Bearer token-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-nd-authorization", "token-2")
                .insert_header("x-total-count", "42")
                .set_body_string("[]"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/second"))
        .and(header("x-nd-authorization", "Bearer token-2"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/anonymous"))
        .and(header_exists("x-nd-authorization"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/anonymous"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;

    let client = session(&server.uri());
    assert_eq!(client.login().unwrap().username, "starr");
    assert!(!client.has_session());
    client.set_session_token("Bearer token-1").unwrap();
    assert!(client.has_session());
    let (_, total): (Vec<u8>, _) = client.get_json_counted("api/first", &[]).await.unwrap();
    assert_eq!(total, Some(42));
    // A clone shares the renewed token.
    let (_, total): (Vec<u8>, _) = client
        .clone()
        .get_json_counted("api/second", &[])
        .await
        .unwrap();
    assert_eq!(total, None);
    client.clear_session();
    assert!(!client.has_session());
    let _: Vec<u8> = client.get_json("api/anonymous").await.unwrap();
    assert!(client.set_session_token("  ").is_err());
}

#[tokio::test]
async fn logins_keep_to_the_login_budget_and_other_schemes_have_no_session() {
    let client = session("http://localhost:9");
    client.login_turn().unwrap();
    match client.login_turn() {
        Err(AdapterError::Throttled {
            retry_after: Some(wait),
        }) => assert!(wait > Duration::from_secs(15), "{wait:?}"),
        other => panic!("second login in 20 s: {other:?}"),
    }

    let plain = ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse("http://localhost:9").unwrap(),
            auth: AuthScheme::ApiKeyHeader("X-Api-Key"),
            credential: SecretString::from("key-0123456789".to_owned()),
        },
    )
    .unwrap();
    assert!(plain.login().is_none());
    assert!(plain.set_session_token("x").is_err());
}

#[test]
fn a_rebuilt_client_keeps_the_instances_login_budget() {
    let pool = starr_adapter_kit::ClientPool::new();
    let first = pool
        .get_or_build(7, b"old", || Ok(session("http://localhost:9")))
        .unwrap();
    first.login_turn().unwrap();
    // The admin corrects the password: a new client, the same budget.
    let second = pool
        .get_or_build(7, b"new", || Ok(session("http://localhost:9")))
        .unwrap();
    assert!(matches!(
        second.login_turn(),
        Err(AdapterError::Throttled { .. })
    ));
    // Another instance has its own.
    let other = pool
        .get_or_build(8, b"x", || Ok(session("http://localhost:9")))
        .unwrap();
    other.login_turn().unwrap();
}

#[tokio::test]
async fn a_login_reply_that_does_not_parse_is_never_quoted() {
    #[derive(Debug, serde::Deserialize)]
    struct Reply {
        #[allow(dead_code)]
        token: String,
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"token": null, "subsonicToken": "sub-secret-0123456789", "jwt": "eyJ.secret.sig"}"#,
        ))
        .mount(&server)
        .await;
    let err = session(&server.uri())
        .post_json_sensitive::<_, Reply>("auth/login", &serde_json::json!({}), true)
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(matches!(err, AdapterError::Unknown(_)), "{err:?}");
    assert!(
        !text.contains("sub-secret") && !text.contains("eyJ"),
        "{text}"
    );
}

#[tokio::test]
async fn an_echoed_password_is_redacted() {
    let server = MockServer::start().await;
    Mock::given(path("/auth/echo"))
        .respond_with(ResponseTemplate::new(400).set_body_string(format!("bad: {PASSWORD}")))
        .mount(&server)
        .await;
    let err = session(&server.uri())
        .get_json::<serde_json::Value>("auth/echo")
        .await
        .unwrap_err();
    assert!(!err.to_string().contains("secret-4711"), "{err}");
}
