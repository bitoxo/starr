//! Image proxy (spec 0003 O4): session required (members included), strict
//! path validation before any CDN request, disk cache, WebP preference,
//! browser caching headers.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::http::StatusCode;
use common::{Req, TestApp, get, post, setup_admin, test_app};
use serde_json::{Value, json};
use starr_adapter_musicbrainz::covers::CoverArchive;
use starr_adapter_tmdb::images::ImageCdn;
use starr_server::{ImageProxy, app};
use starr_worker::image_cache::ImageCache;
use url::Url;
use wiremock::matchers::{header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FILE: &str = "kqjL17yufvn9OVLyXYpvtyrFfak.jpg";

struct CacheDir(PathBuf);

impl CacheDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "starr-images-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for CacheDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct World {
    app: TestApp,
    admin: String,
    cdn: MockServer,
    caa: MockServer,
    dir: CacheDir,
}

async fn world() -> World {
    let cdn = MockServer::start().await;
    let caa = MockServer::start().await;
    let dir = CacheDir::new();
    let mut t = test_app().await;
    let proxy = ImageProxy::new(
        ImageCdn::with_base(Url::parse(&format!("{}/t/p/", cdn.uri())).unwrap()).unwrap(),
        CoverArchive::with_base(Url::parse(&caa.uri()).unwrap()).unwrap(),
        ImageCache::new(dir.0.clone()),
    );
    t.state = t.state.clone().with_images(proxy);
    t.router = app(t.state.clone());
    let admin = setup_admin(&t).await;
    World {
        app: t,
        admin,
        cdn,
        caa,
        dir,
    }
}

const ALBUM: &str = "b1392450-e666-3926-a536-22c65f834433";

/// A cover: one redirect within the archive (in production only to
/// `*.archive.org` over https), fetched once with Starr's agent, cached
/// under the MBID without dashes; the browser only ever talks to Starr.
#[tokio::test]
async fn covers_are_fetched_once_through_the_archive_redirect_and_cached() {
    let w = world().await;
    let target = format!("{}/download/mbid-{ALBUM}/front-250.jpg", w.caa.uri());
    Mock::given(method("GET"))
        .and(path(format!("/release-group/{ALBUM}/front-250")))
        .and(wiremock::matchers::header(
            "user-agent",
            starr_adapter_kit::USER_AGENT,
        ))
        .respond_with(ResponseTemplate::new(307).insert_header("location", target.as_str()))
        .expect(1)
        .mount(&w.caa)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/download/mbid-{ALBUM}/front-250.jpg")))
        .respond_with(image("image/jpeg", "JPEGDATA"))
        .expect(1)
        .mount(&w.caa)
        .await;
    let member = member_cookie(&w.app, &w.admin).await;
    for _ in 0..2 {
        let res = get(&format!("/api/v1/images/caa/250/{ALBUM}"))
            .cookie(&member)
            .header("accept", "image/avif,image/webp,*/*")
            .send(&w.app)
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        assert_eq!(res.raw, "JPEGDATA");
        assert_eq!(res.headers["content-type"], "image/jpeg");
        assert_eq!(
            res.headers["etag"],
            "\"caa-250-b1392450e6663926a53622c65f834433-jpg\""
        );
    }
    assert!(
        w.dir
            .0
            .join("caa/250/b1392450e6663926a53622c65f834433.jpg")
            .exists()
    );
    w.caa.verify().await;
}

/// No front cover: 404, remembered, so the archive is asked once a day at
/// most.
#[tokio::test]
async fn a_missing_cover_is_remembered() {
    let w = world().await;
    Mock::given(method("GET"))
        .and(path(format!("/release-group/{ALBUM}/front-500")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&w.caa)
        .await;
    let member = member_cookie(&w.app, &w.admin).await;
    for _ in 0..3 {
        let res = get(&format!("/api/v1/images/caa/500/{ALBUM}"))
            .cookie(&member)
            .send(&w.app)
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
        assert_eq!(res.body["code"], "images.not_found");
    }
    w.caa.verify().await;
}

#[tokio::test]
async fn cover_paths_are_validated_before_any_request() {
    let w = world().await;
    let member = member_cookie(&w.app, &w.admin).await;
    for bad in [
        format!("/api/v1/images/caa/w500/{ALBUM}"),
        format!("/api/v1/images/caa/250/{}", ALBUM.to_uppercase()),
        "/api/v1/images/caa/250/not-an-mbid".to_owned(),
        "/api/v1/images/caa/250/..%2F..%2Fetc".to_owned(),
    ] {
        let res = get(&bad).cookie(&member).send(&w.app).await;
        assert!(
            matches!(
                res.status,
                StatusCode::UNPROCESSABLE_ENTITY | StatusCode::NOT_FOUND
            ),
            "{bad}: {}",
            res.status
        );
    }
    // Without a session nothing is served.
    let res = get(&format!("/api/v1/images/caa/250/{ALBUM}"))
        .send(&w.app)
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert!(w.caa.received_requests().await.unwrap().is_empty());
}

fn image(content_type: &str, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", content_type)
        .insert_header("cache-control", "max-age=31536000")
        .set_body_bytes(body.as_bytes().to_vec())
}

async fn member_cookie(app: &TestApp, admin: &str) -> String {
    let res = post(
        "/api/v1/members",
        json!({ "username": "mia", "email": "mia@example.org" }),
    )
    .cookie(admin)
    .send(app)
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let link = &res.body["setPasswordLink"];
    let token = link["url"].as_str().unwrap().rsplit('/').next().unwrap();
    let res = post(
        &format!("/api/v1/password-tokens/{token}"),
        json!({ "password": "a long enough password" }),
    )
    .send(app)
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    res.session_cookie().unwrap()
}

fn url(size: &str, file: &str) -> String {
    format!("/api/v1/images/tmdb/{size}/{file}")
}

#[tokio::test]
async fn webp_is_fetched_once_cached_and_served_with_long_caching() {
    let w = world().await;
    Mock::given(method("GET"))
        .and(path(format!("/t/p/w500/{FILE}")))
        .and(header_regex("accept", "^image/webp"))
        .respond_with(image("image/webp", "WEBPDATA"))
        .expect(1)
        .mount(&w.cdn)
        .await;
    let member = member_cookie(&w.app, &w.admin).await;

    for _ in 0..2 {
        let res = get(&url("w500", FILE))
            .cookie(&member)
            .header("accept", "image/avif,image/webp,*/*")
            .send(&w.app)
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        assert_eq!(res.raw, "WEBPDATA");
        assert_eq!(res.headers["content-type"], "image/webp");
        assert_eq!(
            res.headers["cache-control"],
            "private, max-age=31536000, immutable"
        );
        assert_eq!(res.headers["vary"], "Accept");
        assert_eq!(
            res.headers["etag"],
            "\"tmdb-w500-kqjL17yufvn9OVLyXYpvtyrFfak-webp\""
        );
        assert_eq!(res.headers["x-content-type-options"], "nosniff");
    }
    assert!(
        w.dir
            .0
            .join("tmdb/w500/kqjL17yufvn9OVLyXYpvtyrFfak.webp")
            .exists()
    );
    w.cdn.verify().await;

    // Revalidation by ETag answers 304 without a body.
    let res = get(&url("w500", FILE))
        .cookie(&member)
        .header("accept", "image/webp")
        .header(
            "if-none-match",
            "\"tmdb-w500-kqjL17yufvn9OVLyXYpvtyrFfak-webp\"",
        )
        .send(&w.app)
        .await;
    assert_eq!(res.status, StatusCode::NOT_MODIFIED);
    assert_eq!(res.raw, "");
}

#[tokio::test]
async fn browsers_without_webp_get_the_original_format() {
    let w = world().await;
    Mock::given(method("GET"))
        .and(path(format!("/t/p/original/{FILE}")))
        .respond_with(image("image/jpeg", "JPEGDATA"))
        .expect(1)
        .mount(&w.cdn)
        .await;
    let res = get(&url("original", FILE))
        .cookie(&w.admin)
        .header("accept", "image/png,image/*")
        .send(&w.app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.headers["content-type"], "image/jpeg");
    assert_eq!(res.raw, "JPEGDATA");
    assert!(
        w.dir
            .0
            .join("tmdb/original/kqjL17yufvn9OVLyXYpvtyrFfak.jpg")
            .exists()
    );
}

#[tokio::test]
async fn webp_requested_but_jpeg_returned_is_cached_and_found_again() {
    let w = world().await;
    Mock::given(method("GET"))
        .and(path(format!("/t/p/w342/{FILE}")))
        .respond_with(image("image/jpeg", "JPEGDATA"))
        .expect(1)
        .mount(&w.cdn)
        .await;
    for _ in 0..2 {
        let res = get(&url("w342", FILE))
            .cookie(&w.admin)
            .header("accept", "image/webp,*/*")
            .send(&w.app)
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        assert_eq!(res.headers["content-type"], "image/jpeg");
    }
    w.cdn.verify().await;
}

#[tokio::test]
async fn an_unexpected_format_is_served_but_not_cached() {
    let w = world().await;
    let png = "kqjL17yufvn9OVLyXYpvtyrFfak.png";
    Mock::given(method("GET"))
        .and(path(format!("/t/p/w92/{png}")))
        .respond_with(image("image/jpeg", "JPEGDATA"))
        .expect(2)
        .mount(&w.cdn)
        .await;
    for _ in 0..2 {
        let res = get(&url("w92", png))
            .cookie(&w.admin)
            .header("accept", "image/png")
            .send(&w.app)
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    }
    assert!(
        !w.dir
            .0
            .join("tmdb/w92/kqjL17yufvn9OVLyXYpvtyrFfak.jpg")
            .exists()
    );
    w.cdn.verify().await;
}

#[tokio::test]
async fn a_session_is_required() {
    let w = world().await;
    Mock::given(method("GET"))
        .respond_with(image("image/jpeg", "X"))
        .expect(0)
        .mount(&w.cdn)
        .await;
    let res = get(&url("w500", FILE)).send(&w.app).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unknown_sources_are_not_found_without_upstream_or_disk() {
    let w = world().await;
    Mock::given(method("GET"))
        .respond_with(image("image/jpeg", "X"))
        .expect(0)
        .mount(&w.cdn)
        .await;
    for attempt in [
        "/api/v1/images/fanart/250/1b022e01-4da6-387b-8658-8678046e4cef".to_owned(),
        format!("/api/v1/images/CAA/250/{ALBUM}"),
        format!("/api/v1/images/TMDB/w500/{FILE}"),
        format!("/api/v1/images/%2e%2e/w500/{FILE}"),
    ] {
        let res = get(&attempt).cookie(&w.admin).send(&w.app).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{attempt}: {}", res.raw);
        assert_eq!(res.code(), "images.not_found", "{attempt}");
    }
    let res = get("/api/v1/images/fanart/250/x").send(&w.app).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "session first");
    assert!(!w.dir.0.exists(), "nothing written for unknown sources");
    w.cdn.verify().await;
}

#[tokio::test]
async fn traversal_and_invalid_names_never_reach_the_cdn_or_disk() {
    let w = world().await;
    Mock::given(method("GET"))
        .respond_with(image("image/jpeg", "X"))
        .expect(0)
        .mount(&w.cdn)
        .await;
    let attempts = [
        url("w500", "..%2F..%2F3%2Fconfiguration"),
        url("w500", "%2e%2e"),
        url("w500", "kqjL17yufvn9OVLyXYpvtyrFfak.jpg%2F..%2F..%2Fx"),
        url("w500", "..kqjL17yufvn9OVLyXYpvtyrFfak.jpg"),
        url("w500", "kqjL17yufvn9OVLyXYpvtyrFfak.svg"),
        url("w500", "kqjL17yufvn9OVLyXYpvtyrFfak.jpg%00.png"),
        url("w500", "short.jpg"),
        url("%2e%2e", FILE),
        url("w501", FILE),
        url("W500", FILE),
    ];
    for attempt in &attempts {
        let res = get(attempt).cookie(&w.admin).send(&w.app).await;
        assert!(
            matches!(
                res.status,
                StatusCode::UNPROCESSABLE_ENTITY | StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST
            ),
            "{attempt}: {} {}",
            res.status,
            res.raw
        );
        assert_ne!(res.status, StatusCode::OK, "{attempt}");
    }
    // Plain `..` segments are normalised by the router or rejected.
    let res = get("/api/v1/images/tmdb/w500/../../../etc/passwd")
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    assert_ne!(res.status, StatusCode::OK);
    let res = get(&url("w500", "..%2F..%2F3%2Fconfiguration"))
        .cookie(&w.admin)
        .send(&w.app)
        .await;
    assert_eq!(res.code(), "validation.image_path");
    assert!(!w.dir.0.exists(), "nothing written for invalid paths");
    w.cdn.verify().await;
}

#[tokio::test]
async fn cdn_errors_map_to_not_found_and_unavailable() {
    let w = world().await;
    Mock::given(method("GET"))
        .and(path("/t/p/w92/AAAAAAAAAAAAAAAAAAAAAAAAAAA.jpg"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&w.cdn)
        .await;
    Mock::given(method("GET"))
        .and(path("/t/p/w92/BBBBBBBBBBBBBBBBBBBBBBBBBBB.jpg"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&w.cdn)
        .await;
    Mock::given(method("GET"))
        .and(path("/t/p/w92/CCCCCCCCCCCCCCCCCCCCCCCCCCC.jpg"))
        .respond_with(image("text/html", "<script>alert(1)</script>"))
        .mount(&w.cdn)
        .await;
    let fetch = |file: &'static str| {
        let req: Req = get(&url("w92", file)).cookie(&w.admin);
        req.send(&w.app)
    };
    let res = fetch("AAAAAAAAAAAAAAAAAAAAAAAAAAA.jpg").await;
    assert_eq!(
        (res.status, res.code()),
        (StatusCode::NOT_FOUND, "images.not_found")
    );
    let res = fetch("BBBBBBBBBBBBBBBBBBBBBBBBBBB.jpg").await;
    assert_eq!(
        (res.status, res.code()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "images.upstream_unavailable"
        )
    );
    // A non-image answer is never passed through.
    let res = fetch("CCCCCCCCCCCCCCCCCCCCCCCCCCC.jpg").await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!res.raw.contains("script"));
    // Error answers stay uncacheable.
    assert_eq!(res.headers["cache-control"], "no-store");
}

#[tokio::test]
async fn without_a_proxy_images_are_unavailable() {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let res = get(&url("w500", FILE)).cookie(&admin).send(&app).await;
    assert_eq!(
        (res.status, res.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "images.unavailable")
    );
    let _: Value = res.body;
}
