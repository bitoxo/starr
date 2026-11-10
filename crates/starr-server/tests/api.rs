#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;
use common::{get, setup_admin, test_app};

#[tokio::test]
async fn health_is_ok() {
    let app = test_app().await;
    let res = get("/api/v1/health").send(&app).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["status"], "ok", "{}", res.raw);
}

#[tokio::test]
async fn status_reports_version() {
    let app = test_app().await;
    let res = get("/api/v1/system/status").send(&app).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.raw.contains(starr_core::VERSION), "{}", res.raw);
}

#[tokio::test]
async fn openapi_document_needs_a_session() {
    let app = test_app().await;
    let res = get("/api/openapi.json").send(&app).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);

    let cookie = setup_admin(&app).await;
    let res = get("/api/openapi.json").cookie(&cookie).send(&app).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.raw.contains("/api/v1/health"), "{}", res.raw);
}

#[tokio::test]
async fn unknown_api_route_is_closed_then_404_not_spa() {
    let app = test_app().await;
    // Closed by default even for routes that do not exist.
    let res = get("/api/v1/does-not-exist").send(&app).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    let cookie = setup_admin(&app).await;
    let res = get("/api/v1/does-not-exist")
        .cookie(&cookie)
        .send(&app)
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn client_routes_fall_back_to_ui() {
    let app = test_app().await;
    let res = get("/some/client/route").send(&app).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.raw.contains("<title>"), "{}", res.raw);
}

#[tokio::test]
async fn every_response_carries_the_security_headers() {
    let app = test_app().await;
    // The UI (SPA fallback), a public and a closed API route, an unknown file.
    for path in [
        "/",
        "/admin/services",
        "/api/v1/health",
        "/api/v1/services",
        "/nope.js",
    ] {
        let res = get(path).send(&app).await;
        let csp = res.headers["content-security-policy"].to_str().unwrap();
        for directive in [
            "default-src 'self'",
            "script-src 'self'",
            "img-src 'self' data:",
            "object-src 'none'",
            "base-uri 'none'",
            "form-action 'self'",
            "frame-ancestors 'none'",
        ] {
            assert!(
                csp.contains(directive),
                "{path}: {directive} missing in {csp}"
            );
        }
        assert!(!csp.contains("unsafe-eval"), "{path}: {csp}");
        assert_eq!(res.headers["x-frame-options"], "DENY", "{path}");
        assert_eq!(res.headers["x-content-type-options"], "nosniff", "{path}");
        assert_eq!(res.headers["referrer-policy"], "same-origin", "{path}");
        assert_eq!(
            res.headers["cross-origin-opener-policy"], "same-origin",
            "{path}"
        );
        assert!(res.headers.contains_key("permissions-policy"), "{path}");
    }
}
