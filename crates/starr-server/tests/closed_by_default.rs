//! Lesson 5: every API path in the `OpenAPI` document returns 401 without a
//! session unless it is on the public list, and the guard proves it ran.
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{Method, StatusCode};
use common::{Req, test_app};
use serde_json::{Value, json};
use starr_server::auth::PUBLIC_ENDPOINTS;

/// Endpoints known when this test was last touched. The sweep must see at
/// least this many, so a broken document cannot make it pass vacuously.
const MIN_KNOWN_ENDPOINTS: usize = 59;

fn documented_endpoints() -> Vec<(Method, String)> {
    let doc: Value = serde_json::from_str(&starr_server::openapi_json()).unwrap();
    let mut endpoints = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            let method = Method::from_bytes(method.to_uppercase().as_bytes()).unwrap();
            endpoints.push((method, path.clone()));
        }
    }
    endpoints
}

fn is_public(method: &Method, path: &str) -> bool {
    PUBLIC_ENDPOINTS
        .iter()
        .any(|e| e.method == method.as_str() && e.path == path)
}

#[tokio::test]
async fn every_non_public_endpoint_requires_a_session() {
    let app = test_app().await;
    let endpoints = documented_endpoints();
    assert!(
        endpoints.len() >= MIN_KNOWN_ENDPOINTS,
        "only {} endpoints in the OpenAPI document: {endpoints:?}",
        endpoints.len()
    );

    let mut protected = 0;
    for (method, path) in &endpoints {
        // Path parameters get a plausible value; the request must be refused
        // before anything looks at it.
        let concrete: String = path
            .split('/')
            .map(|seg| if seg.starts_with('{') { "1" } else { seg })
            .collect::<Vec<_>>()
            .join("/");
        let body = (method != Method::GET).then(|| json!({}));
        let res = Req::new(method.clone(), &concrete, body).send(&app).await;

        if is_public(method, path) {
            assert_ne!(
                res.status,
                StatusCode::UNAUTHORIZED,
                "public {method} {path} demands a session"
            );
        } else {
            protected += 1;
            assert_eq!(
                res.status,
                StatusCode::UNAUTHORIZED,
                "{method} {path} is reachable without a session: {}",
                res.raw
            );
            assert_eq!(res.code(), "auth.unauthenticated", "{method} {path}");
        }
    }
    assert!(protected > 0, "no protected endpoint was checked");
}

#[test]
fn every_public_endpoint_exists_and_has_a_reason() {
    let endpoints = documented_endpoints();
    assert!(!PUBLIC_ENDPOINTS.is_empty());
    for public in PUBLIC_ENDPOINTS {
        assert!(
            endpoints
                .iter()
                .any(|(m, p)| m.as_str() == public.method && p == public.path),
            "public list names {} {} which is not in the OpenAPI document",
            public.method,
            public.path
        );
        assert!(public.reason.len() > 10, "{} needs a reason", public.path);
    }
}

#[tokio::test]
async fn undocumented_api_paths_are_closed_too() {
    let app = test_app().await;
    // Not in the OpenAPI document; the guard wraps the router, not routes.
    for (method, path) in [
        (Method::GET, "/api/openapi.json"),
        (Method::GET, "/api/v1/not-a-route"),
        (Method::POST, "/api/v2/anything"),
        (Method::GET, "/api/v1/health/"),
    ] {
        let res = Req::new(method.clone(), path, None).send(&app).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}
#[test]
fn operation_ids_are_unique() {
    // The generated TypeScript client keys operations by id: a duplicate
    // silently gives one endpoint the other's types.
    let doc: Value = serde_json::from_str(&starr_server::openapi_json()).unwrap();
    let mut seen = std::collections::HashSet::new();
    for item in doc["paths"].as_object().unwrap().values() {
        for op in item.as_object().unwrap().values() {
            let id = op["operationId"].as_str().unwrap();
            assert!(seen.insert(id.to_owned()), "duplicate operationId `{id}`");
        }
    }
}
