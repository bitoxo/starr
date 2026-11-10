//! Shared helpers for API tests: an app on an in-memory database with a
//! manual clock, and request builders that look like a browser on our origin.
#![allow(clippy::unwrap_used, dead_code)]

pub mod lidarr;
pub mod media;
pub mod music;
pub mod navidrome;
pub mod privacy;
pub mod services;
pub mod stashdb;
pub mod whisparr;

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use starr_db::Database;
use starr_server::clock::Clock;
use starr_server::{AppState, HttpSettings, app};
use tower::ServiceExt;
use url::Url;

pub const PUBLIC_URL: &str = "https://starr.test";
pub const ADMIN_USER: &str = "root";
pub const ADMIN_PASSWORD: &str = "correct horse battery staple";
pub const PEER: &str = "203.0.113.10:50000";
/// Second way to reach the test instance (plain http on the LAN).
pub const LAN_ORIGIN: &str = "http://192.168.1.10:7979";

pub struct TestApp {
    pub router: Router,
    pub state: AppState,
    pub clock: Clock,
    /// The setup token issued at "startup" (no admin exists yet).
    pub setup_token: SecretString,
}

pub fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-01T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

pub async fn test_app_with(public_url: Option<&str>) -> TestApp {
    build(HttpSettings {
        public_url: public_url.map(|u| Url::parse(u).unwrap()),
        ..HttpSettings::default()
    })
    .await
}

pub async fn test_app_behind_proxy() -> TestApp {
    build(HttpSettings {
        public_url: Some(Url::parse(PUBLIC_URL).unwrap()),
        additional_origins: vec![Url::parse(LAN_ORIGIN).unwrap()],
        behind_proxy: true,
        ..HttpSettings::default()
    })
    .await
}

async fn build(settings: HttpSettings) -> TestApp {
    let db = Database::open_in_memory().await.unwrap();
    let clock = Clock::manual(t0());
    let state = AppState::new(db, settings).with_clock(clock.clone());
    let setup_token = state.issue_setup_token().await.unwrap().unwrap();
    TestApp {
        router: app(state.clone()),
        state,
        clock,
        setup_token,
    }
}

pub async fn test_app() -> TestApp {
    test_app_with(Some(PUBLIC_URL)).await
}

#[derive(Debug)]
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
    pub raw: String,
}

impl TestResponse {
    pub fn code(&self) -> &str {
        self.body["code"].as_str().unwrap_or_default()
    }

    /// `name=value` of the session cookie set by this response, if any.
    pub fn session_cookie(&self) -> Option<String> {
        self.cookie_pair("starr_session")
    }

    /// `name=value` of the device cookie set by this response, if any.
    pub fn device_cookie(&self) -> Option<String> {
        self.cookie_pair("starr_device")
    }

    /// `name=value` of a cookie set (not cleared) by this response.
    pub fn cookie_pair(&self, name: &str) -> Option<String> {
        self.set_cookie_named(name)
            .and_then(|c| c.split(';').next().map(str::to_owned))
            .filter(|pair| !pair.ends_with('='))
    }

    pub fn set_cookie(&self) -> Option<String> {
        self.set_cookie_named("starr_session")
    }

    /// The full `Set-Cookie` header for cookie `name`.
    pub fn set_cookie_named(&self, name: &str) -> Option<String> {
        let prefix = format!("{name}=");
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with(&prefix))
            .map(str::to_owned)
    }
}

/// Request builder defaults: our origin, a socket peer, optional cookie.
pub struct Req {
    method: Method,
    path: String,
    body: Option<Value>,
    cookie: Option<String>,
    origin: Option<String>,
    headers: Vec<(&'static str, String)>,
    peer: SocketAddr,
}

pub fn get(path: &str) -> Req {
    Req::new(Method::GET, path, None)
}

pub fn post(path: &str, body: Value) -> Req {
    Req::new(Method::POST, path, Some(body))
}

impl Req {
    pub fn new(method: Method, path: &str, body: Option<Value>) -> Self {
        Self {
            method,
            path: path.to_owned(),
            body,
            cookie: None,
            origin: Some(PUBLIC_URL.to_owned()),
            headers: Vec::new(),
            peer: PEER.parse().unwrap(),
        }
    }

    pub fn cookie(mut self, cookie: &str) -> Self {
        self.cookie = Some(cookie.to_owned());
        self
    }

    pub fn origin(mut self, origin: Option<&str>) -> Self {
        self.origin = origin.map(str::to_owned);
        self
    }

    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_owned()));
        self
    }

    pub fn peer(mut self, peer: &str) -> Self {
        self.peer = peer.parse().unwrap();
        self
    }

    pub async fn send(self, app: &TestApp) -> TestResponse {
        send_to(&app.router, self).await
    }
}

pub async fn send_to(router: &Router, req: Req) -> TestResponse {
    let mut builder = Request::builder().method(req.method).uri(&req.path);
    if let Some(origin) = &req.origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    if let Some(cookie) = &req.cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    for (name, value) in &req.headers {
        builder = builder.header(*name, value);
    }
    let mut request = match req.body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    request.extensions_mut().insert(ConnectInfo(req.peer));

    let res = router.clone().oneshot(request).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();
    let body = serde_json::from_str(&raw).unwrap_or(Value::Null);
    TestResponse {
        status,
        headers,
        body,
        raw,
    }
}

pub fn setup_body(token: &str) -> Value {
    json!({
        "token": token,
        "username": ADMIN_USER,
        "displayName": "Root Admin",
        "password": ADMIN_PASSWORD,
    })
}

/// Complete setup and return the admin's session cookie.
pub async fn setup_admin(app: &TestApp) -> String {
    let res = post(
        "/api/v1/setup/admin",
        setup_body(app.setup_token.expose_secret()),
    )
    .send(app)
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    res.session_cookie().unwrap()
}

/// Mark the session behind `cookie` as elevated for five minutes from the
/// test clock's now, as `POST /api/v1/auth/elevate` would, but without
/// rotating the cookie. For tests where the elevation is incidental; the
/// elevation flow itself is tested through the endpoint.
pub async fn elevate_in_place(app: &TestApp, cookie: &str) {
    let token = cookie
        .split_once('=')
        .map_or(cookie, |(_, token)| token)
        .split(';')
        .next()
        .unwrap();
    let until = (app.clock.now() + chrono::TimeDelta::minutes(5))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    let hash = starr_server::auth::crypto::hash_token(token);
    let done = sqlx::query("UPDATE session SET elevated_until = ? WHERE token_hash = ?")
        .bind(until)
        .bind(hash.as_slice())
        .execute(app.state.db.pool())
        .await
        .unwrap();
    assert_eq!(done.rows_affected(), 1, "no session for {cookie}");
}

pub fn login_body(username: &str, password: &str) -> Value {
    json!({ "username": username, "password": password })
}

pub async fn login(app: &TestApp, username: &str, password: &str) -> TestResponse {
    post("/api/v1/auth/login", login_body(username, password))
        .send(app)
        .await
}
