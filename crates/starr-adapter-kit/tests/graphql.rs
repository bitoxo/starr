//! Client behaviour added for the GraphQL catalog (`StashDB`, spec 0007
//! slice 2): posted queries cached under a key the adapter chooses, and
//! image fetches that never carry the instance's credential.
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use secrecy::SecretString;
use starr_adapter_kit::{
    AuthScheme, BoxFuture, CachePolicy, Endpoint, ResponseCache, ServiceClient,
};
use starr_core::adapters::{
    AdapterDescriptor, AdapterError, Budgets, IdentityRule, RateLimit, SecretClass, Version,
    VersionRange,
};
use url::Url;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "catalog-key-4711";

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
    secret_class: SecretClass::CatalogReadToken,
    lists: &[],
};

fn keyed(uri: &str) -> ServiceClient {
    ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(uri).unwrap(),
            auth: AuthScheme::ApiKeyHeader("ApiKey"),
            credential: SecretString::from(KEY.to_owned()),
        },
    )
    .unwrap()
}

#[derive(Debug, Default)]
struct MemoryCache(Mutex<HashMap<String, Vec<u8>>>);

impl ResponseCache for MemoryCache {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Vec<u8>>> {
        let hit = self.0.lock().unwrap().get(key).cloned();
        Box::pin(async move { hit })
    }

    fn put<'a>(&'a self, key: &'a str, body: &'a [u8], _ttl: Duration) -> BoxFuture<'a, ()> {
        self.0.lock().unwrap().insert(key.to_owned(), body.to_vec());
        Box::pin(async {})
    }
}

const HOUR: CachePolicy = CachePolicy {
    min_ttl: Duration::from_hours(1),
    max_ttl: Duration::from_hours(1),
};

#[tokio::test]
async fn posted_queries_are_cached_under_the_callers_key() {
    let server = MockServer::start().await;
    let body = serde_json::json!({ "query": "{ a }" });
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(header("apikey", KEY))
        .and(body_json(&body))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"data": 1}"#))
        .expect(1)
        .mount(&server)
        .await;
    let client = keyed(&server.uri());
    let cache = MemoryCache::default();
    for _ in 0..2 {
        let value: serde_json::Value = client
            .post_json_cached(
                "graphql",
                &body,
                Some((&cache, HOUR, "k:1")),
                |_: &serde_json::Value| true,
            )
            .await
            .unwrap();
        assert_eq!(value["data"], 1);
    }
    assert!(cache.0.lock().unwrap().contains_key("k:1"));
}

#[tokio::test]
async fn replies_the_caller_does_not_keep_are_not_cached() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"data": null, "errors": [{}]}"#),
        )
        .expect(2)
        .mount(&server)
        .await;
    let client = keyed(&server.uri());
    let cache = MemoryCache::default();
    let body = serde_json::json!({ "query": "{ a }" });
    for _ in 0..2 {
        let _: serde_json::Value = client
            .post_json_cached(
                "graphql",
                &body,
                Some((&cache, HOUR, "k:1")),
                |v: &serde_json::Value| v["errors"].is_null(),
            )
            .await
            .unwrap();
    }
    assert!(cache.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn posted_queries_without_a_cache_always_go_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"data": 2}"#))
        .expect(2)
        .mount(&server)
        .await;
    let client = keyed(&server.uri());
    let body = serde_json::json!({ "query": "{ a }" });
    for _ in 0..2 {
        let value: serde_json::Value = client
            .post_json_cached("graphql", &body, None, |_: &serde_json::Value| true)
            .await
            .unwrap();
        assert_eq!(value["data"], 2);
    }
}

#[tokio::test]
async fn anonymous_fetches_send_no_credential_on_any_hop() {
    let server = MockServer::start().await;
    // A relative location, resolved against the answering URL.
    let target = "/cdn/image.jpg";
    Mock::given(path("/images/abc"))
        .and(query_param("size", "300"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", target))
        .mount(&server)
        .await;
    Mock::given(path("/cdn/image.jpg"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(vec![7]),
        )
        .mount(&server)
        .await;
    let client = keyed(&server.uri());
    let size = [("size", "300".to_owned())];
    let image = client
        .get_bytes_anonymous("images/abc", &size, Some("image/*"), 1024, |url| {
            url.path().starts_with("/cdn/")
        })
        .await
        .unwrap();
    assert_eq!(image.body, [7]);
    assert_eq!(image.content_type.as_deref(), Some("image/jpeg"));
    assert!(matches!(
        client
            .get_bytes_anonymous("images/abc", &size, None, 1024, |_| false)
            .await,
        Err(AdapterError::Rejected(_))
    ));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    for request in requests {
        assert!(
            request.headers.get("apikey").is_none(),
            "credential sent to {}",
            request.url
        );
    }
}
