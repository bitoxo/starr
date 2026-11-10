//! Service-instance test environment: app, admin session and a worker with
//! the credential key, plus a recorded Radarr fake.
#![allow(clippy::unwrap_used, dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::http::Method;
use serde_json::{Value, json};
use starr_worker::secrets::CredentialKey;
use starr_worker::{SystemJobs, Worker, WorkerConfig};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{Req, TestApp, TestResponse, post, setup_admin, test_app};

pub const API_KEY: &str = "radarr-api-key-0123456789abcdef";

/// A temp dir for the key file, removed on drop.
pub struct KeyDir(pub PathBuf);

impl KeyDir {
    pub fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "starr-services-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for KeyDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// App + admin cookie + a worker holding the credential key, as in
/// single-process mode.
pub struct Env {
    pub app: TestApp,
    pub admin: String,
    pub worker: Worker<SystemJobs>,
    /// Recovery codes once [`Env::enable_totp`] ran (each elevates once).
    recovery: Vec<String>,
    keys: KeyDir,
}

pub async fn env() -> Env {
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let keys = KeyDir::new();
    let key = CredentialKey::init(&app.state.db, &keys.0.join("secret.key"))
        .await
        .unwrap();
    let worker = Worker::new(
        app.state.db.clone(),
        SystemJobs::new(key).with_catalog(Arc::clone(&app.state.catalog)),
        Arc::clone(&app.state.wake),
        WorkerConfig::default(),
    );
    Env {
        app,
        admin,
        worker,
        recovery: Vec::new(),
        keys,
    }
}

impl Env {
    pub async fn send(&self, req: Req) -> TestResponse {
        req.cookie(&self.admin).send(&self.app).await
    }

    /// The worker opens catalogs with `access` from now on (the web side's,
    /// when a test replaced it), keeping its credential key.
    pub fn worker_catalog(&mut self, access: Arc<starr_worker::catalog::CatalogAccess>) {
        let key = CredentialKey::load(&self.keys.0.join("secret.key")).unwrap();
        self.worker = Worker::new(
            self.app.state.db.clone(),
            SystemJobs::new(key).with_catalog(access),
            Arc::clone(&self.app.state.wake),
            WorkerConfig::default(),
        );
    }

    /// The worker keeps `images` (and its adult tree) from now on, as
    /// housekeeping does in production, opening catalogs with the web
    /// side's access; it keeps its credential key.
    pub fn worker_images(&mut self, images: starr_worker::image_cache::ImageCache) {
        let key = CredentialKey::load(&self.keys.0.join("secret.key")).unwrap();
        self.worker = Worker::new(
            self.app.state.db.clone(),
            SystemJobs::new(key)
                .with_catalog(Arc::clone(&self.app.state.catalog))
                .with_image_cache(images, u64::MAX),
            Arc::clone(&self.app.state.wake),
            WorkerConfig::default(),
        );
    }

    /// Execute queued jobs until the queue is empty.
    pub async fn drain(&self) {
        while self.worker.tick(&CancellationToken::new()).await.unwrap() {}
    }

    /// Re-authenticate the admin (`POST /auth/elevate`, M10 2): the new
    /// session cookie replaces the old one for five minutes of elevation.
    pub async fn elevate(&mut self) {
        let body = match self.recovery.pop() {
            Some(code) => json!({ "password": super::ADMIN_PASSWORD, "code": code }),
            None => json!({ "password": super::ADMIN_PASSWORD }),
        };
        let res = self.send(post("/api/v1/auth/elevate", body)).await;
        assert_eq!(res.status, axum::http::StatusCode::OK, "{}", res.raw);
        self.admin = res.session_cookie().unwrap();
    }

    /// Enroll and confirm TOTP for the admin (spec 0005 M10 1: going live
    /// needs it). Later elevations use the recovery codes.
    pub async fn enable_totp(&mut self) {
        let res = self.send(post("/api/v1/auth/totp/enroll", json!({}))).await;
        assert_eq!(res.status, axum::http::StatusCode::OK, "{}", res.raw);
        let secret =
            totp_rs::Secret::try_from_base32(res.body["secret"].as_str().unwrap()).unwrap();
        let totp = totp_rs::Builder::new().with_secret(secret).build().unwrap();
        let at = u64::try_from(self.app.clock.now().timestamp()).unwrap();
        let res = self
            .send(post(
                "/api/v1/auth/totp/confirm",
                json!({ "code": totp.generate(at).to_string() }),
            ))
            .await;
        assert_eq!(res.status, axum::http::StatusCode::OK, "{}", res.raw);
        self.admin = res.session_cookie().unwrap();
        self.recovery = res.body["codes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_owned())
            .collect();
    }

    /// Turn the admin's TOTP off again (with a recovery code).
    pub async fn disable_totp(&mut self) -> TestResponse {
        let code = self.recovery.pop().unwrap();
        let res = self
            .send(post(
                "/api/v1/auth/totp/disable",
                json!({ "password": super::ADMIN_PASSWORD, "code": code }),
            ))
            .await;
        self.recovery.clear();
        res
    }

    pub async fn create(&self, name: &str, base_url: &str, key: &str) -> TestResponse {
        self.send(post(
            "/api/v1/services",
            json!({ "adapterType": "radarr", "name": name, "baseUrl": base_url, "apiKey": key }),
        ))
        .await
    }
}

pub async fn fake_radarr() -> MockServer {
    let body = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adapters/radarr/fixtures/6.4.4.10685/system_status.json"
    ))
    .unwrap();
    let server = MockServer::start().await;
    Mock::given(path("/api/v3/system/status"))
        .and(header("X-Api-Key", API_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;
    Mock::given(path("/api/v3/system/status"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    server
}

pub fn patch(path: &str, body: Value) -> Req {
    Req::new(Method::PATCH, path, Some(body))
}

pub fn delete(path: &str) -> Req {
    Req::new(Method::DELETE, path, None)
}
