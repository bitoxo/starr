//! Starr server: wires configuration, database and HTTP routes together.

pub mod auth;
pub mod clock;
pub mod config;
pub mod recovery;

mod api;
mod web;

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::sync::Arc;

use axum::Router;
use axum::middleware::{from_fn_with_state, map_response};
use axum::routing::get;
use secrecy::SecretString;
use starr_db::{Database, DbError};
use starr_worker::catalog::CatalogAccess;
use starr_worker::secrets::CredentialSealer;
use tokio::net::TcpListener;
use tokio::sync::{Notify, OnceCell, Semaphore};
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use url::Url;

use crate::api::error::ApiError;
pub use crate::api::images::ImageProxy;
use crate::auth::SetupToken;
use crate::auth::totp::TotpSecretBox;
use crate::clock::Clock;
use starr_secrets::{CatalogTokenKey, TotpKey};

/// The parts of the operator configuration that request handling needs.
#[derive(Debug, Clone)]
pub struct HttpSettings {
    /// `STARR_PUBLIC_URL`: origin for CSRF checks; required to finish setup.
    pub public_url: Option<Url>,
    /// `STARR_ADDITIONAL_ORIGINS`: further origins accepted by the CSRF check.
    pub additional_origins: Vec<Url>,
    /// `STARR_BEHIND_PROXY`: take client IP and scheme from `X-Forwarded-*`.
    pub behind_proxy: bool,
    /// `STARR_MEMBER_DELETE_GRACE_DAYS`: restorable period of a deleted member.
    pub member_delete_grace_days: u16,
    /// `STARR_ADMIN_TOTP_REQUIRED`: admins without TOTP must enroll before
    /// any other admin API works.
    pub admin_totp_required: bool,
}

impl Default for HttpSettings {
    fn default() -> Self {
        Self {
            public_url: None,
            additional_origins: Vec::new(),
            behind_proxy: false,
            member_delete_grace_days: 30,
            admin_totp_required: false,
        }
    }
}

/// `app_meta` key of the device cookie signing key.
const DEVICE_KEY_META: &str = "device_cookie_key";

/// `app_meta` key of the recipient of the web key sealing TOTP secrets.
const TOTP_RECIPIENT_META: &str = "totp_recipient";

#[derive(Debug, thiserror::Error)]
pub enum TotpKeyMismatch {
    #[error(
        "web.key does not match the key that sealed the stored TOTP secrets; restore the original web.key from your backup, or turn TOTP off for each admin that has it with `starr admin recover <username> --reset-totp`"
    )]
    Mismatch,
    #[error(transparent)]
    Db(DbError),
}

/// Shared state handed to every request handler. Cheap to clone.
#[derive(Debug, Clone)]
pub struct AppState {
    pub db: Database,
    pub settings: Arc<HttpSettings>,
    pub clock: Clock,
    /// Wakes the in-process worker after enqueueing a job. In split mode no
    /// worker listens; the worker process finds the job with its periodic
    /// queue check (`WorkerConfig::due_check`, one second).
    pub wake: Arc<Notify>,
    setup_token: SetupToken,
    /// Bounds concurrent Argon2 runs (19 MiB and one core each).
    hashing: Arc<Semaphore>,
    /// Device cookie signing key, loaded from (or created in) the database.
    device_key: Arc<OnceCell<[u8; 32]>>,
    /// Seals and opens TOTP secrets with the web process's own key.
    totp_box: TotpSecretBox,
    /// The image proxy; `None` answers `503 images.unavailable`.
    pub(crate) images: Option<Arc<ImageProxy>>,
    /// Opens catalog tokens (TMDB) with the web key, seals new ones (ADR
    /// 0003 amendment 1). Shared with the in-process worker.
    pub catalog: Arc<CatalogAccess>,
    /// The login posters last listed, so serving the login wall's images
    /// does not rebuild the list per image (spec 0008 slice 11).
    pub(crate) login_art: Arc<api::LoginArtMemo>,
}

impl AppState {
    #[must_use]
    pub fn new(db: Database, settings: HttpSettings) -> Self {
        Self {
            db,
            settings: Arc::new(settings),
            clock: Clock::system(),
            wake: Arc::new(Notify::new()),
            setup_token: SetupToken::default(),
            hashing: Arc::new(Semaphore::new(
                std::thread::available_parallelism().map_or(2, NonZeroUsize::get),
            )),
            device_key: Arc::new(OnceCell::new()),
            // In-memory until `with_totp_key` installs the key file's key
            // (tests keep this one).
            totp_box: TotpSecretBox::new(TotpKey::generate()),
            images: None,
            // In-memory until `with_catalog` installs the key file's key.
            catalog: Arc::new(CatalogAccess::new(CatalogTokenKey::generate())),
            login_art: Arc::default(),
        }
    }

    /// Open and seal catalog tokens through `access` (the web key).
    #[must_use]
    pub fn with_catalog(mut self, access: Arc<CatalogAccess>) -> Self {
        self.catalog = access;
        self
    }

    /// Serve images through `proxy`.
    #[must_use]
    pub fn with_images(mut self, proxy: ImageProxy) -> Self {
        self.images = Some(Arc::new(proxy));
        self
    }

    /// Use `key` (from `<data_dir>/web.key`) for TOTP secrets. Its recipient
    /// is recorded in `app_meta`; a different key while sealed TOTP secrets
    /// exist (lost or swapped `web.key`) is refused, instead of failing every
    /// TOTP login later.
    pub async fn check_totp_key(db: &Database, key: &TotpKey) -> Result<(), TotpKeyMismatch> {
        let recorded = db
            .meta_get_or_insert(TOTP_RECIPIENT_META, key.recipient())
            .await
            .map_err(TotpKeyMismatch::Db)?;
        if recorded == key.recipient() {
            return Ok(());
        }
        if db.any_totp_secret().await.map_err(TotpKeyMismatch::Db)? {
            return Err(TotpKeyMismatch::Mismatch);
        }
        db.meta_set(TOTP_RECIPIENT_META, key.recipient())
            .await
            .map_err(TotpKeyMismatch::Db)
    }

    /// Use `key` (from `<data_dir>/web.key`) for TOTP secrets.
    #[must_use]
    pub fn with_totp_key(mut self, key: TotpKey) -> Self {
        self.totp_box = TotpSecretBox::new(key);
        self
    }

    /// Replace the clock (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// While no admin exists, issue a fresh one-time setup token and return it
    /// so the caller can show it to the operator once. Only its hash is kept.
    pub async fn issue_setup_token(&self) -> Result<Option<SecretString>, DbError> {
        if self.db.admin_exists().await? {
            return Ok(None);
        }
        Ok(Some(self.setup_token.issue()))
    }

    /// Run password hashing or verification on the blocking pool, at most one
    /// per CPU at a time so a login flood cannot exhaust memory. The permit
    /// moves into the blocking task: if the client disconnects, the hash keeps
    /// running and keeps its permit until it is done.
    pub(crate) async fn run_hashing<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, ApiError> {
        let permit = Arc::clone(&self.hashing)
            .acquire_owned()
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await?)
    }

    /// The sealer for service credentials, from the recipient the worker
    /// published in `app_meta`; the web side never holds the key that opens
    /// them (ADR 0003). Read on every call, not cached: a worker restarted
    /// with a new key publishes a new recipient (split mode).
    /// `503 services.secrets_unavailable` until a worker initialised the key.
    pub(crate) async fn credential_sealer(&self) -> Result<CredentialSealer, ApiError> {
        let Some(recipient) = self
            .db
            .meta_get(starr_worker::secrets::RECIPIENT_META_KEY)
            .await?
        else {
            tracing::warn!("no service credential recipient yet; is the worker running?");
            return Err(ApiError::Unavailable("services.secrets_unavailable"));
        };
        CredentialSealer::parse(&recipient).map_err(|err| {
            tracing::error!(error = %err, "service credential recipient in app_meta is malformed");
            ApiError::Internal
        })
    }

    async fn device_key(&self) -> Result<[u8; 32], ApiError> {
        let key = self
            .device_key
            .get_or_try_init(|| meta_key(&self.db, DEVICE_KEY_META))
            .await?;
        Ok(*key)
    }

    /// Seals and opens TOTP secrets (web-held key, ADR 0003).
    pub(crate) fn totp_box(&self) -> &TotpSecretBox {
        &self.totp_box
    }
}

/// A 32-byte key from `app_meta`, created on first use (the first process
/// to write it wins).
async fn meta_key(db: &Database, name: &str) -> Result<[u8; 32], ApiError> {
    let stored = db
        .meta_get_or_insert(name, &auth::crypto::random_key_b64())
        .await?;
    auth::crypto::decode_key(&stored).ok_or_else(|| {
        tracing::error!(key = name, "key in app_meta is malformed");
        ApiError::Internal
    })
}

/// Build the full application router.
pub fn app(state: AppState) -> Router {
    let (api, openapi) = api::router().split_for_parts();
    let openapi_json = openapi.to_pretty_json().unwrap_or_default();

    // The guards wrap the whole router, not individual routes: every path
    // under /api/ is closed by default (auth::require_session), documented or
    // not, and mutating requests must come from our own origin
    // (auth::check_origin). The web UI outside /api/ is not affected.
    Router::new()
        .merge(api)
        .route("/api/openapi.json", get(|| async move { openapi_json }))
        .fallback(web::static_handler)
        .layer(from_fn_with_state(state.clone(), auth::require_session))
        .layer(from_fn_with_state(state.clone(), auth::check_origin))
        // Outside the guards: they and the handlers see `X-Forwarded-*`
        // only from a trusted proxy peer.
        .layer(from_fn_with_state(
            state.clone(),
            auth::trust_forwarded_from_proxy,
        ))
        .layer(map_response(security_headers))
        .layer(axum::middleware::from_fn(no_store_api))
        .layer(CompressionLayer::new())
        .layer(
            TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
                tracing::debug_span!(
                    "request",
                    method = %req.method(),
                    path = %redact_tokens(req.uri().path()),
                )
            }),
        )
        .with_state(state)
}

/// Content-Security-Policy for the UI and every API answer. The built UI
/// loads only its own scripts, styles, fonts and images (posters and covers
/// come through Starr's image proxy); the TOTP QR code is a `data:` image.
/// Inline styles stay allowed: Radix and sonner set them at runtime. No
/// framing at all (clickjacking of admin actions).
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; \
connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; \
frame-ancestors 'none'";

/// Browser security headers on every response (UI and API). HSTS is left to
/// the TLS proxy: Starr itself speaks plain http.
async fn security_headers(mut res: axum::response::Response) -> axum::response::Response {
    use axum::http::{HeaderName, HeaderValue, header};
    const HEADERS: [(HeaderName, &str); 6] = [
        (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        // Token pages (/invite/<token>, /set-password/<token>) must not leak
        // their URL to other sites via Referer. `same-origin`, not
        // `no-referrer`: the latter would turn same-origin `Origin` headers
        // into `null` and fail the CSRF check.
        (header::REFERRER_POLICY, "same-origin"),
        (
            HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ),
        (
            HeaderName::from_static("permissions-policy"),
            "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
        ),
    ];
    let headers = res.headers_mut();
    for (name, value) in HEADERS {
        headers.insert(name, HeaderValue::from_static(value));
    }
    res
}

/// API answers can carry secrets (TOTP setup, recovery codes, one-time
/// links, personal data): never keep them in a browser or proxy cache.
/// Proxied images are the exception: they set their own `Cache-Control`
/// (immutable, private); their error answers are not cached either.
async fn no_store_api(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path();
    let api = path.starts_with("/api/");
    let image = path.starts_with("/api/v1/images/");
    let mut res = next.run(req).await;
    let status = res.status();
    let own_caching = image
        && (status.is_success() || status == axum::http::StatusCode::NOT_MODIFIED)
        && res
            .headers()
            .contains_key(axum::http::header::CACHE_CONTROL);
    if api && !own_caching {
        res.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
    }
    res
}

/// Shortest path segment treated as a token (tokens are 43 characters).
const TOKEN_SEGMENT_MIN_CHARS: usize = 32;

/// The request path with token-like segments (invitation and password links)
/// replaced, so tokens never reach the logs (ADR 0006). The query string is
/// left out entirely.
fn redact_tokens(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if segment.len() >= TOKEN_SEGMENT_MIN_CHARS {
                "<redacted>"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Serve the app on `listener` until `shutdown` completes. Connect info is
/// required: it provides the socket address for throttling.
pub async fn serve(
    listener: TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

/// The `OpenAPI` document as JSON (used by `starr openapi` to generate TS types).
#[must_use]
pub fn openapi_json() -> String {
    let (_, openapi) = api::router().split_for_parts();
    openapi.to_pretty_json().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_segments_are_redacted_from_logged_paths() {
        let token = "x".repeat(43);
        assert_eq!(
            redact_tokens(&format!("/api/v1/invitations/{token}/accept")),
            "/api/v1/invitations/<redacted>/accept"
        );
        assert_eq!(
            redact_tokens(&format!("/set-password/{token}")),
            "/set-password/<redacted>"
        );
        assert_eq!(redact_tokens("/api/v1/members/12"), "/api/v1/members/12");
    }

    #[tokio::test]
    async fn hashing_permit_is_held_until_the_hash_finishes_even_if_the_caller_goes_away() {
        let db = Database::open_in_memory().await.unwrap();
        let state = AppState::new(db, HttpSettings::default());
        let total = state.hashing.available_permits();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let caller = {
            let state = state.clone();
            tokio::spawn(async move {
                state
                    .run_hashing(move || {
                        let _ = release_rx.recv();
                        let _ = done_tx.send(());
                    })
                    .await
            })
        };
        // Wait until the work holds its permit, then drop the caller.
        while state.hashing.available_permits() == total {
            tokio::task::yield_now().await;
        }
        caller.abort();
        let _ = caller.await;
        assert_eq!(state.hashing.available_permits(), total - 1);

        release_tx.send(()).unwrap();
        done_rx.await.unwrap();
        while state.hashing.available_permits() != total {
            tokio::task::yield_now().await;
        }
    }
}
