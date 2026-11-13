//! Client behaviour added for catalog adapters: bearer auth (redacted when
//! echoed), query parameters, body size cap, public clients.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use secrecy::SecretString;
use starr_adapter_kit::{AuthScheme, ClientOptions, Endpoint, ServiceClient};
use starr_core::adapters::{
    AdapterDescriptor, AdapterError, Budgets, IdentityRule, RateLimit, SecretClass, Version,
    VersionRange,
};
use url::Url;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.bearer-secret-4711";

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

fn bearer(uri: &str) -> ServiceClient {
    ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(uri).unwrap(),
            auth: AuthScheme::Bearer,
            credential: SecretString::from(TOKEN.to_owned()),
        },
    )
    .unwrap()
}

#[tokio::test]
async fn bearer_token_and_query_are_sent_and_an_echoed_token_is_redacted() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/3/search"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(query_param("query", "a b&c"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok": true}"#))
        .mount(&server)
        .await;
    Mock::given(path("/3/echo"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(format!("not json: Bearer {TOKEN}")),
        )
        .mount(&server)
        .await;
    let client = bearer(&server.uri());
    let value: serde_json::Value = client
        .get_json_cached("3/search", &[("query", "a b&c".to_owned())], None)
        .await
        .unwrap();
    assert_eq!(value["ok"], true);

    let err = client
        .get_json::<serde_json::Value>("3/echo")
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(!text.contains(TOKEN), "{text}");
    assert!(text.contains("[redacted]"), "{text}");
}

#[tokio::test]
async fn oversized_bodies_are_rejected() {
    let server = MockServer::start().await;
    Mock::given(path("/big.jpg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 2048]))
        .mount(&server)
        .await;
    let client = ServiceClient::public(
        "cdn",
        DESCRIPTOR.budgets.rate_limit,
        Url::parse(&server.uri()).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        client.get_bytes("big.jpg", None, 1024).await,
        Err(AdapterError::Rejected(_))
    ));
    let ok = client.get_bytes("big.jpg", None, 2048).await.unwrap();
    assert_eq!(ok.body.len(), 2048);
    // A public client sends no credential.
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|r| r.headers.get("authorization").is_none())
    );
}

const USER_AGENT: &str = concat!(
    "Starr/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/bitoxo/starr )"
);

fn no_credential(uri: &str, unavailable_is_throttle: bool) -> ServiceClient {
    ServiceClient::with_options(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(uri).unwrap(),
            auth: AuthScheme::None,
            credential: SecretString::from(String::new()),
        },
        ClientOptions {
            rate_limit: DESCRIPTOR.budgets.rate_limit,
            unavailable_is_throttle,
            max_wait: None,
            private_content: false,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn every_request_carries_the_project_user_agent_and_no_credential_scheme_sends_nothing() {
    let server = MockServer::start().await;
    Mock::given(path("/ws/2/artist"))
        .and(header("user-agent", USER_AGENT))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    assert_eq!(starr_adapter_kit::USER_AGENT, USER_AGENT);
    let client = no_credential(&server.uri(), false);
    let _: serde_json::Value = client.get_json("ws/2/artist").await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].headers.get("authorization").is_none());
    // A client with a credential sends the same agent.
    let _: serde_json::Value = bearer(&server.uri()).get_json("ws/2/artist").await.unwrap();
}

#[tokio::test]
async fn a_503_is_a_throttle_only_where_the_service_says_so_and_pauses_every_caller() {
    let server = MockServer::start().await;
    Mock::given(path("/busy"))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "7"))
        .mount(&server)
        .await;
    let plain = no_credential(&server.uri(), false);
    assert!(matches!(
        plain.get_json::<serde_json::Value>("busy").await,
        Err(AdapterError::Transient(_))
    ));
    let strict = no_credential(&server.uri(), true);
    assert_eq!(
        strict.get_json::<serde_json::Value>("busy").await,
        Err(AdapterError::Throttled {
            retry_after: Some(Duration::from_secs(7))
        })
    );
    // The gate holds: the next call fails without a request.
    let before = server.received_requests().await.unwrap().len();
    assert!(matches!(
        strict.get_json::<serde_json::Value>("other").await,
        Err(AdapterError::Throttled { .. })
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn an_empty_reply_reads_as_none() {
    let server = MockServer::start().await;
    Mock::given(path("/stats"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = no_credential(&server.uri(), false);
    let value: Option<serde_json::Value> =
        client.get_json_cached("stats", &[], None).await.unwrap();
    assert_eq!(value, None);
}

#[tokio::test]
async fn redirects_are_followed_only_where_allowed() {
    let server = MockServer::start().await;
    let target = format!("{}/files/cover.jpg", server.uri());
    let again = format!("{}/rg/front-250", server.uri());
    Mock::given(path("/rg/front-250"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", target.as_str()))
        .mount(&server)
        .await;
    let looping = format!("{}/rg/loop", server.uri());
    Mock::given(path("/rg/loop"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", looping.as_str()))
        .mount(&server)
        .await;
    Mock::given(path("/rg/twice"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", again.as_str()))
        .mount(&server)
        .await;
    Mock::given(path("/files/cover.jpg"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(vec![1, 2, 3]),
        )
        .mount(&server)
        .await;
    let client = no_credential(&server.uri(), false);
    let image = client
        .get_bytes_redirected("rg/front-250", None, 1024, |url| {
            url.path().starts_with("/files/")
        })
        .await
        .unwrap();
    assert_eq!(image.body, [1, 2, 3]);
    assert_eq!(image.content_type.as_deref(), Some("image/jpeg"));
    assert!(matches!(
        client
            .get_bytes_redirected("rg/front-250", None, 1024, |_| false)
            .await,
        Err(AdapterError::Rejected(_))
    ));
    // A chain (the archive's 307, then archive.org's 302) is followed when
    // every hop is allowed, refused when one in the middle is not.
    let chained = client
        .get_bytes_redirected("rg/twice", None, 1024, |_| true)
        .await
        .unwrap();
    assert_eq!(chained.body, [1, 2, 3]);
    assert!(matches!(
        client
            .get_bytes_redirected("rg/twice", None, 1024, |url| {
                url.path().starts_with("/files/")
            })
            .await,
        Err(AdapterError::Rejected(_))
    ));
    // A loop ends after a few hops.
    let before = server.received_requests().await.unwrap().len();
    assert!(matches!(
        client
            .get_bytes_redirected("rg/loop", None, 1024, |_| true)
            .await,
        Err(AdapterError::Rejected(_))
    ));
    let hops = server.received_requests().await.unwrap().len() - before;
    assert_eq!(hops, starr_adapter_kit::MAX_REDIRECTS + 1);
    // A client with a credential never follows one.
    assert!(matches!(
        bearer(&server.uri())
            .get_bytes_redirected("rg/front-250", None, 1024, |_| true)
            .await,
        Err(AdapterError::Rejected(_))
    ));
}

#[tokio::test]
async fn retry_after_is_capped_and_a_slow_budget_fails_fast() {
    let server = MockServer::start().await;
    Mock::given(path("/busy"))
        .respond_with(
            ResponseTemplate::new(503).insert_header("retry-after", "18446744073709551615"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let strict = no_credential(&server.uri(), true);
    // No overflow, at most an hour.
    assert_eq!(
        strict.get_json::<serde_json::Value>("busy").await,
        Err(AdapterError::Throttled {
            retry_after: Some(Duration::from_hours(1))
        })
    );
    // One request per minute, waiting at most 50 ms: the second one fails
    // as throttled instead of queueing.
    let slow = ServiceClient::with_options(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(&server.uri()).unwrap(),
            auth: AuthScheme::None,
            credential: SecretString::from(String::new()),
        },
        ClientOptions {
            rate_limit: RateLimit {
                requests: 1,
                per: Duration::from_mins(1),
            },
            unavailable_is_throttle: false,
            max_wait: Some(Duration::from_millis(50)),
            private_content: false,
        },
    )
    .unwrap();
    let _: serde_json::Value = slow.get_json("ok").await.unwrap();
    assert!(matches!(
        slow.get_json::<serde_json::Value>("ok").await,
        Err(AdapterError::Throttled { .. })
    ));
}

/// `ListenBrainz`'s rate-limit headers (seen live 2026-11-07:
/// `X-RateLimit-Limit`, `-Remaining`, `-Reset-In` in seconds): a reply
/// that used up the window pauses every caller until it resets, and a 429
/// without `Retry-After` waits for `X-RateLimit-Reset-In`.
#[tokio::test]
async fn rate_limit_headers_pause_until_the_window_resets() {
    let server = MockServer::start().await;
    Mock::given(path("/last"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-limit", "30")
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset-in", "9")
                .set_body_string("[]"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/more"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-remaining", "12")
                .insert_header("x-ratelimit-reset-in", "9")
                .set_body_string("[]"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/over"))
        .respond_with(ResponseTemplate::new(429).insert_header("x-ratelimit-reset-in", "4"))
        .mount(&server)
        .await;

    // Requests left: no pause.
    let client = no_credential(&server.uri(), false);
    let _: serde_json::Value = client.get_json("more").await.unwrap();
    let _: serde_json::Value = client.get_json("more").await.unwrap();

    // The window is used up: the reply still counts, then everyone waits.
    let _: serde_json::Value = client.get_json("last").await.unwrap();
    let before = server.received_requests().await.unwrap().len();
    match client.get_json::<serde_json::Value>("more").await {
        Err(AdapterError::Throttled {
            retry_after: Some(wait),
        }) => assert!(wait <= Duration::from_secs(9) && wait > Duration::from_secs(7)),
        other => panic!("{other:?}"),
    }
    assert_eq!(server.received_requests().await.unwrap().len(), before);

    let fresh = no_credential(&server.uri(), false);
    assert_eq!(
        fresh.get_json::<serde_json::Value>("over").await,
        Err(AdapterError::Throttled {
            retry_after: Some(Duration::from_secs(4))
        })
    );
}
/// Only `Remaining: 0` together with `Reset-In` pauses after a success;
/// other or malformed rate-limit headers (`MusicBrainz`'s epoch
/// `X-RateLimit-Reset`, a non-numeric value) never do, and the pause after
/// a success is capped at a minute (`ListenBrainz`'s window is seconds).
#[tokio::test]
async fn only_a_used_up_window_with_reset_in_pauses_and_briefly() {
    let server = MockServer::start().await;
    for (route, headers) in [
        ("/no-reset-in", vec![("x-ratelimit-remaining", "0")]),
        (
            "/epoch",
            vec![
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1762480000"),
            ],
        ),
        (
            "/garbled",
            vec![
                ("x-ratelimit-remaining", "none"),
                ("x-ratelimit-reset-in", "30"),
            ],
        ),
        (
            "/huge",
            vec![
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset-in", "86400"),
            ],
        ),
    ] {
        let mut reply = ResponseTemplate::new(200).set_body_string("[]");
        for (name, value) in headers {
            reply = reply.insert_header(name, value);
        }
        Mock::given(path(route))
            .respond_with(reply)
            .mount(&server)
            .await;
    }
    let client = no_credential(&server.uri(), false);
    for route in ["no-reset-in", "epoch", "garbled", "no-reset-in"] {
        let _: serde_json::Value = client.get_json(route).await.unwrap();
    }
    let _: serde_json::Value = client.get_json("huge").await.unwrap();
    match client.get_json::<serde_json::Value>("epoch").await {
        Err(AdapterError::Throttled {
            retry_after: Some(wait),
        }) => assert!(wait <= Duration::from_secs(60), "{wait:?}"),
        other => panic!("{other:?}"),
    }
}

/// A 503 without `Retry-After` from a service that answers overload with
/// 503 pauses every caller briefly, for two seconds (`MusicBrainz` sends
/// such 503s at its 1 req/s limit, and the pause holds every member).
#[tokio::test]
async fn a_503_without_retry_after_pauses_two_seconds() {
    let server = MockServer::start().await;
    Mock::given(path("/busy"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let strict = no_credential(&server.uri(), true);
    assert_eq!(
        strict.get_json::<serde_json::Value>("busy").await,
        Err(AdapterError::Throttled {
            retry_after: Some(Duration::from_secs(2))
        })
    );
    let paused = strict.throttled_for().unwrap();
    assert!(paused <= Duration::from_secs(2), "{paused:?}");
    // Meanwhile the next call fails without a request.
    assert!(matches!(
        strict.get_json::<serde_json::Value>("busy").await,
        Err(AdapterError::Throttled { .. })
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

/// The Cover Art Archive's 307 goes to `archive.org`, whose 302 goes to a
/// storage host: with `get_bytes_redirected` only requests to the client's
/// own origin take from its budget, hops to other origins do not (each
/// host has its own limits). A hop back to the client's own origin still
/// does. `get_bytes_anonymous` (a catalog's images on its CDN) keeps every
/// hop on the budget.
#[tokio::test]
async fn only_requests_to_the_clients_own_origin_take_from_its_budget() {
    let archive = MockServer::start().await;
    let storage = MockServer::start().await;
    let download = format!("{}/download/cover.jpg", storage.uri());
    let image = format!("{}/items/cover.jpg", storage.uri());
    let home = format!("{}/files/cover.jpg", archive.uri());
    Mock::given(path("/rg/front-250"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", download.as_str()))
        .mount(&archive)
        .await;
    Mock::given(path("/rg/home"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", home.as_str()))
        .mount(&archive)
        .await;
    Mock::given(path("/download/cover.jpg"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", image.as_str()))
        .mount(&storage)
        .await;
    Mock::given(path("/items/cover.jpg"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(vec![1, 2, 3]),
        )
        .mount(&storage)
        .await;
    // Two requests per minute, waiting at most 50 ms.
    let budgeted = || {
        ServiceClient::with_options(
            &DESCRIPTOR,
            Endpoint {
                base_url: Url::parse(&archive.uri()).unwrap(),
                auth: AuthScheme::None,
                credential: SecretString::from(String::new()),
            },
            ClientOptions {
                rate_limit: RateLimit {
                    requests: 2,
                    per: Duration::from_mins(1),
                },
                unavailable_is_throttle: true,
                max_wait: Some(Duration::from_millis(50)),
                private_content: false,
            },
        )
        .unwrap()
    };
    let client = budgeted();
    // Three requests, one of them to the archive: one unit of the budget.
    let cover = client
        .get_bytes_redirected("rg/front-250", None, 1024, |_| true)
        .await
        .unwrap();
    assert_eq!(cover.body, [1, 2, 3]);
    assert_eq!(storage.received_requests().await.unwrap().len(), 2);
    // The second unit goes to this request; its hop back to the archive
    // finds the budget used up.
    assert!(matches!(
        client
            .get_bytes_redirected("rg/home", None, 1024, |_| true)
            .await,
        Err(AdapterError::Throttled { .. })
    ));
    assert_eq!(archive.received_requests().await.unwrap().len(), 2);
    // Anonymous fetches meter every hop: the third request (the image)
    // finds the budget of two used up.
    assert!(matches!(
        budgeted()
            .get_bytes_anonymous("rg/front-250", &[], None, 1024, |_| true)
            .await,
        Err(AdapterError::Throttled { .. })
    ));
    assert_eq!(storage.received_requests().await.unwrap().len(), 3);
}
