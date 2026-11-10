//! HTTP plumbing every adapter uses (ADR 0009, lesson 11).
//!
//! Adapters never build HTTP clients themselves (clippy `disallowed-methods`
//! outside this crate). Starr hands each adapter a [`ServiceClient`]: one
//! client per service instance with
//! - a rate limiter from the adapter's [`Budgets`](starr_core::adapters::Budgets),
//! - a shared "throttled until" gate: one 429 slows every caller of that
//!   instance, not just the one that received it,
//! - connect and request timeouts, no redirects (an API key must never
//!   follow a redirect to another host),
//! - the credential sent as a sensitive header and redacted from every log
//!   line and error detail produced here,
//! - HTTP failures mapped to the [`AdapterError`] taxonomy,
//! - optional response caching that honours `Cache-Control` within bounds
//!   the adapter sets ([`ResponseCache`], [`CachePolicy`]),
//! - per-key serialization of writes ([`ServiceClient::serialize`]).

mod cache;
#[cfg(feature = "contract")]
pub mod contract;
pub mod mediabrowser;
mod redact;
pub mod servarr;

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, RETRY_AFTER,
};
use reqwest::{Method, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;
use starr_core::adapters::{AdapterDescriptor, AdapterError, RateLimit};
use url::Url;

pub use cache::{BoxFuture, CachePolicy, ResponseCache, cache_ttl};
pub use redact::Redactor;

/// A random password nobody knows (64 hex characters): what Starr writes
/// to leave an account unusable (Starr's younger twin, an unfinished
/// create, D17) or to disable it on a service without a disable flag (D9).
/// Never stored, never logged.
#[must_use]
pub fn random_password() -> SecretString {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes[..]);
    let hex = bytes.iter().fold(String::with_capacity(64), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    });
    SecretString::from(hex)
}

/// Total time for one request, including reading the body.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Pause after a 429 without a usable `Retry-After`.
const DEFAULT_THROTTLE: Duration = Duration::from_secs(30);
/// Longest pause a service's `Retry-After` can impose.
const MAX_THROTTLE: Duration = Duration::from_hours(1);
/// Redirects [`ServiceClient::get_bytes_redirected`] follows at most.
pub const MAX_REDIRECTS: usize = 3;
/// Pause after a 503 from a service that answers overload with 503
/// ([`ClientOptions::unavailable_is_throttle`]) without a `Retry-After`.
const UNAVAILABLE_THROTTLE: Duration = Duration::from_secs(10);

/// The User-Agent of every request Starr sends: product token with Starr's
/// version and the project URL as contact (spec 0006 Q1, ADR 0013). Nothing
/// about the operator is in it.
pub const USER_AGENT: &str = concat!(
    "Starr/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/bitoxo/starr )"
);
/// How much of an error body is kept for logs and reports.
const BODY_SNIPPET: usize = 300;
/// What logs show in place of the path or a detail of a private-content
/// request ([`ClientOptions::private_content`]).
const PRIVATE_LOG: &str = "[private]";
/// Largest JSON body accepted.
const MAX_JSON_BODY: usize = 16 * 1024 * 1024;

/// How the service expects the credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScheme {
    /// API key in a request header, e.g. `X-Api-Key`.
    ApiKeyHeader(&'static str),
    /// `Authorization: Bearer <credential>`.
    Bearer,
    /// `Authorization: MediaBrowser Token="<credential>"`: the only header
    /// form Jellyfin 12 accepts by default (`capabilities.md`, Jellyfin).
    MediaBrowserToken,
    /// No credential at all (`MusicBrainz`): nothing is sent.
    None,
    /// A username and password ([`LoginCredential`]) that the adapter
    /// exchanges for a session token ([`ServiceClient::set_session_token`]);
    /// the token travels as `Bearer <token>` in this header, and a renewed
    /// token in the same response header replaces it (Navidrome's sliding
    /// JWT in `X-ND-Authorization`, `capabilities.md`). The password is
    /// never sent anywhere but in the adapter's login request.
    Session(&'static str),
}

/// The credential of an [`AuthScheme::Session`] service: an account's
/// username and password, stored sealed as one JSON value.
#[derive(Debug, Clone)]
pub struct LoginCredential {
    pub username: String,
    pub password: SecretString,
}

#[derive(Serialize, serde::Deserialize)]
struct StoredLogin<'a> {
    username: std::borrow::Cow<'a, str>,
    password: std::borrow::Cow<'a, str>,
}

impl LoginCredential {
    /// The value Starr seals and stores for the login.
    #[must_use]
    pub fn encode(username: &str, password: &SecretString) -> SecretString {
        let stored = StoredLogin {
            username: username.into(),
            password: password.expose_secret().into(),
        };
        // Two strings always serialize.
        SecretString::from(serde_json::to_string(&stored).unwrap_or_default())
    }

    /// Read a stored login; anything else is refused without quoting it.
    pub fn decode(stored: &SecretString) -> Result<Self, AdapterError> {
        let login: StoredLogin<'_> = serde_json::from_str(stored.expose_secret())
            .map_err(|_| AdapterError::Rejected("the stored login is unreadable".to_owned()))?;
        Ok(Self {
            username: login.username.into_owned(),
            password: SecretString::from(login.password.into_owned()),
        })
    }
}

/// The session of an [`AuthScheme::Session`] client.
struct Session {
    header: HeaderName,
    login: LoginCredential,
    token: Mutex<Option<HeaderValue>>,
    /// The descriptor's login budget, if it has one.
    /// Shared with the client this one replaced (same instance), so a new
    /// URL or credential does not reset the budget.
    logins: Option<Arc<DefaultDirectRateLimiter>>,
}

impl Session {
    fn token(&self) -> Option<HeaderValue> {
        self.token
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set(&self, token: &str) -> Result<(), AdapterError> {
        let token = token.trim();
        let token = token.strip_prefix("Bearer ").unwrap_or(token).trim();
        if token.is_empty() {
            return Err(AdapterError::Unknown(
                "the service answered with an empty session token".to_owned(),
            ));
        }
        let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
            AdapterError::Unknown("the session token contains invalid characters".to_owned())
        })?;
        value.set_sensitive(true);
        *self.token.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
        Ok(())
    }

    /// A renewed token in a successful response replaces the current one.
    fn renew(&self, headers: &HeaderMap) {
        if let Some(renewed) = headers.get(&self.header).and_then(|v| v.to_str().ok()) {
            // A malformed renewal keeps the token Starr has.
            let _ = self.set(renewed);
        }
    }
}

/// How a client deviates from its descriptor's defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientOptions {
    /// The limiter's budget (e.g. the descriptor's budget times the
    /// process's share in split mode, spec 0006 Q14).
    pub rate_limit: RateLimit,
    /// The service answers overload with 503 instead of 429 (`MusicBrainz`):
    /// a 503 is [`AdapterError::Throttled`] and pauses every caller.
    pub unavailable_is_throttle: bool,
    /// Longest a request waits for the limiter before failing as
    /// [`AdapterError::Throttled`], so requests do not pile up behind a
    /// slow budget (an interactive page should rather say "busy"). `None`:
    /// wait as long as it takes.
    pub max_wait: Option<Duration>,
    /// The service holds private content (the adult catalog and source,
    /// spec 0007): logs show neither request paths (they can carry a
    /// title's key, e.g. a `StashDB` UUID) nor the service's own words, at
    /// any level, and error details carry no body.
    pub private_content: bool,
}

/// The part of `limit` a process gets when it holds `percent` of a budget
/// that is shared by several processes on one host (split mode, spec 0006
/// Q14): the same burst over a proportionally longer period, so the
/// processes together never exceed `limit`.
#[must_use]
pub fn share_of(limit: RateLimit, percent: u32) -> RateLimit {
    let percent = percent.clamp(1, 100);
    RateLimit {
        requests: limit.requests,
        per: limit.per * 100 / percent,
    }
}

impl ClientOptions {
    /// The descriptor's budget, 503 as a transient failure.
    #[must_use]
    pub const fn of(descriptor: &AdapterDescriptor) -> Self {
        Self {
            rate_limit: descriptor.budgets.rate_limit,
            unavailable_is_throttle: false,
            max_wait: None,
            private_content: false,
        }
    }
}

/// Where and how to reach one service instance.
#[derive(Debug)]
pub struct Endpoint {
    /// Base URL including any URL base path, e.g. `http://radarr:7878/radarr`.
    pub base_url: Url,
    pub auth: AuthScheme,
    pub credential: SecretString,
}

/// HTTP client for one service instance. Cheap to clone; clones share the
/// rate limiter and the throttle gate.
#[derive(Clone)]
pub struct ServiceClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    base_url: Url,
    /// `None` for clients of public hosts (e.g. an image CDN).
    auth_header: Option<(HeaderName, HeaderValue)>,
    limiter: DefaultDirectRateLimiter,
    throttled_until: Mutex<Option<Instant>>,
    redactor: Redactor,
    service: &'static str,
    serial: KeyedLocks,
    /// The base URL is itself a credential (webhooks): never shown.
    secret_base: bool,
    /// 503 means "slow down" ([`ClientOptions::unavailable_is_throttle`]).
    unavailable_is_throttle: bool,
    /// [`ClientOptions::max_wait`].
    max_wait: Option<Duration>,
    /// [`ClientOptions::private_content`].
    private_content: bool,
    /// [`AuthScheme::Session`] only.
    session: Option<Session>,
}

/// One async mutex per key, created on demand and dropped when unused.
#[derive(Debug, Default)]
struct KeyedLocks {
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl KeyedLocks {
    async fn lock(&self, key: &str) -> SerialGuard {
        let lock = {
            let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
            // Only the map holds these: nobody owns or waits for them.
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            Arc::clone(locks.entry(key.to_owned()).or_default())
        };
        SerialGuard {
            _guard: lock.lock_owned().await,
        }
    }
}

/// Held while a serialized operation runs; dropping it lets the next one in.
#[derive(Debug)]
pub struct SerialGuard {
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl std::fmt::Debug for ServiceClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let base_url = if self.inner.secret_base {
            "[redacted]"
        } else {
            self.inner.base_url.as_str()
        };
        f.debug_struct("ServiceClient")
            .field("service", &self.inner.service)
            .field("base_url", &base_url)
            .finish_non_exhaustive()
    }
}

fn quota(limit: RateLimit) -> Quota {
    let burst = NonZeroU32::new(limit.requests).unwrap_or(NonZeroU32::MIN);
    let period = limit.per.checked_div(burst.get()).unwrap_or(limit.per);
    Quota::with_period(period.max(Duration::from_millis(1)))
        .unwrap_or_else(|| Quota::per_second(NonZeroU32::MIN))
        .allow_burst(burst)
}

impl ServiceClient {
    /// Build the client for one instance. Only Starr calls this (the worker);
    /// adapters receive the result.
    pub fn new(descriptor: &AdapterDescriptor, endpoint: Endpoint) -> Result<Self, AdapterError> {
        Self::with_options(descriptor, endpoint, ClientOptions::of(descriptor))
    }

    /// [`Self::new`] with a budget or error mapping other than the
    /// descriptor's defaults.
    pub fn with_options(
        descriptor: &AdapterDescriptor,
        endpoint: Endpoint,
        options: ClientOptions,
    ) -> Result<Self, AdapterError> {
        let (header_name, raw_value) = match endpoint.auth {
            AuthScheme::None => {
                return Self::build(
                    descriptor.type_id,
                    options,
                    endpoint.base_url,
                    None,
                    Redactor::new(Vec::new()),
                    false,
                );
            }
            AuthScheme::Session(name) => {
                let header = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                    AdapterError::Unknown(format!("invalid session header name {name:?}"))
                })?;
                let login = LoginCredential::decode(&endpoint.credential)?;
                let secrets = vec![
                    SecretString::from(login.password.expose_secret().to_owned()),
                    endpoint.credential,
                ];
                let mut client = Self::build(
                    descriptor.type_id,
                    options,
                    endpoint.base_url,
                    None,
                    Redactor::new(secrets),
                    false,
                )?;
                let inner = Arc::get_mut(&mut client.inner).ok_or_else(|| {
                    AdapterError::Unknown("the new client is shared already".to_owned())
                })?;
                inner.session = Some(Session {
                    header,
                    login,
                    token: Mutex::new(None),
                    logins: descriptor
                        .budgets
                        .login_limit
                        .map(|limit| Arc::new(RateLimiter::direct(quota(limit)))),
                });
                return Ok(client);
            }
            AuthScheme::ApiKeyHeader(name) => (
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                    AdapterError::Unknown(format!("invalid auth header name {name:?}"))
                })?,
                SecretString::from(endpoint.credential.expose_secret().to_owned()),
            ),
            AuthScheme::Bearer => (
                AUTHORIZATION,
                SecretString::from(format!("Bearer {}", endpoint.credential.expose_secret())),
            ),
            AuthScheme::MediaBrowserToken => (
                AUTHORIZATION,
                SecretString::from(format!(
                    "MediaBrowser Token=\"{}\"",
                    endpoint.credential.expose_secret()
                )),
            ),
        };
        let mut header_value = HeaderValue::from_str(raw_value.expose_secret())
            .map_err(|_| AdapterError::Rejected("credential contains invalid characters".into()))?;
        header_value.set_sensitive(true);
        Self::build(
            descriptor.type_id,
            options,
            endpoint.base_url,
            Some((header_name, header_value)),
            Redactor::new(vec![endpoint.credential]),
            false,
        )
    }

    /// A client for a public host that needs no credential (e.g. an image
    /// CDN), with its own rate limit and throttle gate.
    pub fn public(
        service: &'static str,
        rate_limit: RateLimit,
        base_url: Url,
    ) -> Result<Self, AdapterError> {
        Self::public_with(
            service,
            ClientOptions {
                rate_limit,
                unavailable_is_throttle: false,
                max_wait: None,
                private_content: false,
            },
            base_url,
        )
    }

    /// [`Self::public`] with [`ClientOptions`].
    pub fn public_with(
        service: &'static str,
        options: ClientOptions,
        base_url: Url,
    ) -> Result<Self, AdapterError> {
        Self::build(
            service,
            options,
            base_url,
            None,
            Redactor::new(Vec::new()),
            false,
        )
    }

    /// A client for a URL that is itself the credential: a webhook (a
    /// Discord webhook URL carries its token). The URL is redacted from
    /// every log line, error detail and `Debug` output; call
    /// [`ServiceClient::post_to_url`] to send to it.
    pub fn secret_url(
        service: &'static str,
        rate_limit: RateLimit,
        url: &SecretString,
    ) -> Result<Self, AdapterError> {
        let parsed = Url::parse(url.expose_secret())
            .map_err(|_| AdapterError::Rejected("the webhook URL is not a valid URL".into()))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(AdapterError::Rejected(
                "the webhook URL must start with http:// or https://".into(),
            ));
        }
        // The whole URL in both spellings, and the path and query alone
        // (where tokens live), in case a transport error quotes only part.
        let mut secrets = vec![
            SecretString::from(url.expose_secret().to_owned()),
            SecretString::from(parsed.as_str().to_owned()),
        ];
        if parsed.path().len() > 1 {
            secrets.push(SecretString::from(parsed.path().to_owned()));
        }
        // A service may echo the token alone: the last path segment (where
        // Discord and most others put it), long segments, query values.
        let segments: Vec<&str> = parsed
            .path_segments()
            .map(|s| s.filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        let last = segments.len().saturating_sub(1);
        for (index, segment) in segments.iter().enumerate() {
            if (index == last && segment.len() >= 8) || segment.len() >= 16 {
                secrets.push(SecretString::from((*segment).to_owned()));
            }
        }
        if let Some(query) = parsed.query() {
            secrets.push(SecretString::from(query.to_owned()));
        }
        for (_, value) in parsed.query_pairs() {
            if value.len() >= 8 {
                secrets.push(SecretString::from(value.into_owned()));
            }
        }
        Self::build(
            service,
            ClientOptions {
                rate_limit,
                unavailable_is_throttle: false,
                max_wait: None,
                private_content: false,
            },
            parsed,
            None,
            Redactor::new(secrets),
            true,
        )
    }

    /// `POST` `body` as JSON to the client's URL itself (webhooks). The
    /// reply body is ignored.
    pub async fn post_to_url<B: Serialize + ?Sized>(&self, body: &B) -> Result<(), AdapterError> {
        self.request(
            Method::POST,
            "",
            &[],
            Some(encode(body)?),
            None,
            MAX_JSON_BODY,
        )
        .await?;
        Ok(())
    }

    fn build(
        service: &'static str,
        options: ClientOptions,
        mut base_url: Url,
        auth_header: Option<(HeaderName, HeaderValue)>,
        redactor: Redactor,
        secret_base: bool,
    ) -> Result<Self, AdapterError> {
        // The one place in Starr that constructs an HTTP client (ADR 0009).
        #[allow(clippy::disallowed_methods)]
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .build()
            .map_err(|err| AdapterError::Unknown(format!("cannot build HTTP client: {err}")))?;

        // A webhook URL is used exactly as given (`post_to_url`).
        if !secret_base && !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                base_url,
                auth_header,
                limiter: RateLimiter::direct(quota(options.rate_limit)),
                throttled_until: Mutex::new(None),
                redactor,
                service,
                serial: KeyedLocks::default(),
                secret_base,
                unavailable_is_throttle: options.unavailable_is_throttle,
                max_wait: options.max_wait,
                private_content: options.private_content,
                session: None,
            }),
        })
    }

    /// `GET` like [`Self::get_json`], but `also_not_found` (besides 404 and
    /// 410) means [`AdapterError::NotFound`]: for services that answer an
    /// unknown ID with another status (Jellyfin 10.x: 400). Every other
    /// status keeps its taxonomy.
    pub async fn get_json_not_found_on<T: DeserializeOwned>(
        &self,
        path: &str,
        also_not_found: StatusCode,
    ) -> Result<T, AdapterError> {
        let response = self
            .request_with(
                Method::GET,
                path,
                &[],
                None,
                None,
                MAX_JSON_BODY,
                Extra {
                    also_not_found: Some(also_not_found),
                    sensitive: false,
                    ..Extra::default()
                },
            )
            .await?;
        self.decode(&Method::GET, path, &response.body)
    }

    /// `GET` a path relative to the base URL and decode the JSON body.
    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, AdapterError> {
        let response = self
            .request(Method::GET, path, &[], None, None, MAX_JSON_BODY)
            .await?;
        self.decode(&Method::GET, path, &response.body)
    }

    /// `GET` a path with query parameters and decode the JSON body. With a
    /// cache, a fresh cached body is used without a request, and a fetched
    /// body is stored for its `Cache-Control` lifetime clamped by the
    /// policy. The cache key is the path plus the encoded query; it never
    /// contains the credential (which travels in a header).
    pub async fn get_json_cached<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cache: Option<(&dyn ResponseCache, CachePolicy)>,
    ) -> Result<T, AdapterError> {
        let key = request_key(path, query);
        if let Some((cache, _)) = cache
            && let Some(body) = cache.get(&key).await
        {
            match serde_json::from_slice(&body) {
                Ok(value) => return Ok(value),
                // A body of another shape (e.g. cached by an older Starr)
                // is fetched again.
                Err(err) => {
                    tracing::debug!(service = self.inner.service, path = self.log_path(path), error = %self.log_text(&err.to_string()), "cached body does not decode; fetching again");
                }
            }
        }
        self.fetch_and_cache(path, query, &key, cache).await
    }

    /// [`Self::get_json_cached`] without reading the cache first: fetches
    /// now and replaces the cached body (background refreshes that must
    /// not wait for an entry to expire).
    pub async fn get_json_refreshed<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cache: (&dyn ResponseCache, CachePolicy),
    ) -> Result<T, AdapterError> {
        let key = request_key(path, query);
        self.fetch_and_cache(path, query, &key, Some(cache)).await
    }

    /// The body [`Self::get_json_cached`] cached for `path` and `query`,
    /// without any request: `None` when nothing fresh is cached (or it does
    /// not decode). For lists only the background fills.
    pub async fn get_json_cache_only<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cache: &dyn ResponseCache,
    ) -> Option<T> {
        let body = cache.get(&request_key(path, query)).await?;
        serde_json::from_slice(&body).ok()
    }

    async fn fetch_and_cache<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        key: &str,
        cache: Option<(&dyn ResponseCache, CachePolicy)>,
    ) -> Result<T, AdapterError> {
        let response = self
            .request(Method::GET, path, query, None, None, MAX_JSON_BODY)
            .await?;
        // An empty reply (204 No Content: ListenBrainz statistics not
        // computed yet) reads as JSON `null`, so `Option<T>` targets get
        // `None`; it is not cached.
        if response.body.is_empty() {
            return self.decode(&Method::GET, path, b"null");
        }
        let value = self.decode(&Method::GET, path, &response.body)?;
        if let Some((cache, policy)) = cache {
            cache
                .put(key, &response.body, cache_ttl(&response.headers, policy))
                .await;
        }
        Ok(value)
    }

    /// `POST` `body` as JSON and decode the JSON reply (GraphQL queries,
    /// which read but travel as `POST`). With a cache, a fresh body cached
    /// under `key` is used without a request, and a fetched body is stored
    /// there for its `Cache-Control` lifetime clamped by the policy. The
    /// adapter chooses the key (e.g. a hash of the query and its variables,
    /// so member search text is never stored as such). Only replies `keep`
    /// accepts are stored (GraphQL answers errors with HTTP 200: those must
    /// not stick).
    pub async fn post_json_cached<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        cache: Option<(&dyn ResponseCache, CachePolicy, &str)>,
        keep: impl Fn(&T) -> bool,
    ) -> Result<T, AdapterError> {
        if let Some((cache, _, key)) = cache
            && let Some(cached) = cache.get(key).await
        {
            match serde_json::from_slice(&cached) {
                Ok(value) => return Ok(value),
                Err(err) => {
                    tracing::debug!(service = self.inner.service, path = self.log_path(path), error = %self.log_text(&err.to_string()), "cached body does not decode; fetching again");
                }
            }
        }
        let response = self
            .request(
                Method::POST,
                path,
                &[],
                Some(encode(body)?),
                None,
                MAX_JSON_BODY,
            )
            .await?;
        let value = self.decode(&Method::POST, path, &response.body)?;
        if let Some((cache, policy, key)) = cache
            && keep(&value)
        {
            cache
                .put(key, &response.body, cache_ttl(&response.headers, policy))
                .await;
        }
        Ok(value)
    }

    /// `GET` a binary resource with query parameters **without the
    /// instance's credential** (images of a keyed catalog), following at
    /// most [`MAX_REDIRECTS`] redirects, each only to a URL `allow`
    /// accepts. Every hop shares this client's limiter, so the host's
    /// budget covers images too; no hop carries the credential.
    pub async fn get_bytes_anonymous(
        &self,
        path: &str,
        query: &[(&str, String)],
        accept: Option<&str>,
        max_body: usize,
        allow: impl Fn(&Url) -> bool,
    ) -> Result<BinaryResponse, AdapterError> {
        let first = Extra {
            redirect_ok: true,
            anonymous: true,
            ..Extra::default()
        };
        let response = self
            .request_with(Method::GET, path, query, None, accept, max_body, first)
            .await?;
        self.follow(path, response, accept, max_body, true, allow)
            .await
    }

    /// Follow the redirects of `response` (see [`Self::get_bytes_redirected`]).
    async fn follow(
        &self,
        path: &str,
        mut response: RawResponse,
        accept: Option<&str>,
        max_body: usize,
        anonymous: bool,
        allow: impl Fn(&Url) -> bool,
    ) -> Result<BinaryResponse, AdapterError> {
        let mut hops = 0;
        while let Some(location) = response.location.take() {
            hops += 1;
            if hops > MAX_REDIRECTS {
                return Err(AdapterError::Rejected(format!(
                    "GET {}: more than {MAX_REDIRECTS} redirects",
                    self.log_path(path)
                )));
            }
            let target = Url::parse(&location).map_err(|_| {
                AdapterError::Rejected(format!(
                    "GET {}: redirect to an invalid URL",
                    self.log_path(path)
                ))
            })?;
            if !allow(&target) {
                let host = if self.inner.private_content {
                    PRIVATE_LOG.to_owned()
                } else {
                    redirect_host(&response.headers)
                        .unwrap_or_else(|| "an unknown location".to_owned())
                };
                return Err(AdapterError::Rejected(format!(
                    "GET {}: redirects to {host}",
                    self.log_path(path),
                )));
            }
            let next = Extra {
                absolute: true,
                redirect_ok: true,
                anonymous,
                ..Extra::default()
            };
            response = self
                .request_with(
                    Method::GET,
                    target.as_str(),
                    &[],
                    None,
                    accept,
                    max_body,
                    next,
                )
                .await?;
        }
        let content_type = response
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Ok(BinaryResponse {
            content_type,
            body: response.body,
        })
    }

    /// `GET` a binary resource (e.g. an image) of at most `max_body` bytes,
    /// sending `accept` as the `Accept` header.
    pub async fn get_bytes(
        &self,
        path: &str,
        accept: Option<&str>,
        max_body: usize,
    ) -> Result<BinaryResponse, AdapterError> {
        let response = self
            .request(Method::GET, path, &[], None, accept, max_body)
            .await?;
        let content_type = response
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Ok(BinaryResponse {
            content_type,
            body: response.body,
        })
    }

    /// `GET` a binary resource like [`Self::get_bytes`], following at most
    /// [`MAX_REDIRECTS`] redirects, each only to a URL `allow` accepts (the
    /// Cover Art Archive answers 307 to `archive.org`, which answers 302 to
    /// one of its storage hosts). Only for clients without a credential:
    /// nothing but the URL travels to a redirect target. Every hop shares
    /// this client's limiter.
    pub async fn get_bytes_redirected(
        &self,
        path: &str,
        accept: Option<&str>,
        max_body: usize,
        allow: impl Fn(&Url) -> bool,
    ) -> Result<BinaryResponse, AdapterError> {
        if self.inner.auth_header.is_some() {
            return Err(AdapterError::Rejected(
                "redirects are followed only without a credential".to_owned(),
            ));
        }
        let first = Extra {
            redirect_ok: true,
            ..Extra::default()
        };
        let response = self
            .request_with(Method::GET, path, &[], None, accept, max_body, first)
            .await?;
        self.follow(path, response, accept, max_body, false, allow)
            .await
    }

    /// `POST` `body` as JSON to a path relative to the base URL and decode
    /// the JSON reply.
    pub async fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, AdapterError> {
        let reply = self
            .request(
                Method::POST,
                path,
                &[],
                Some(encode(body)?),
                None,
                MAX_JSON_BODY,
            )
            .await?;
        self.decode(&Method::POST, path, &reply.body)
    }

    /// [`Self::post_json`] for a body that may carry a secret (a password,
    /// when `sensitive`): an error reply is neither logged nor quoted.
    pub async fn post_json_sensitive<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        sensitive: bool,
    ) -> Result<T, AdapterError> {
        let reply = self
            .request_with(
                Method::POST,
                path,
                &[],
                Some(encode(body)?),
                None,
                MAX_JSON_BODY,
                Extra {
                    also_not_found: None,
                    sensitive,
                    ..Extra::default()
                },
            )
            .await?;
        if sensitive {
            // A login reply carries tokens the redactor does not know:
            // never quoted, not even when it does not parse.
            return serde_json::from_slice(&reply.body).map_err(|_| {
                AdapterError::Unknown(format!(
                    "response of POST {path} is not the expected JSON (body withheld)"
                ))
            });
        }
        self.decode(&Method::POST, path, &reply.body)
    }

    /// `PUT` `body` as JSON to a path relative to the base URL. The reply
    /// body is ignored: callers verify writes by reading back (lesson 9).
    pub async fn put_json<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), AdapterError> {
        self.request(
            Method::PUT,
            path,
            &[],
            Some(encode(body)?),
            None,
            MAX_JSON_BODY,
        )
        .await?;
        Ok(())
    }

    /// [`Self::put_json`] for a body that may carry a secret (a password,
    /// when `sensitive`): an error reply is neither logged nor quoted in
    /// the error, since it might echo the body.
    pub async fn put_json_sensitive<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
        sensitive: bool,
    ) -> Result<(), AdapterError> {
        self.request_with(
            Method::PUT,
            path,
            &[],
            Some(encode(body)?),
            None,
            MAX_JSON_BODY,
            Extra {
                sensitive,
                ..Extra::default()
            },
        )
        .await?;
        Ok(())
    }

    /// `POST` `body` as JSON to a path relative to the base URL, with query
    /// parameters. The reply body is ignored: callers verify writes by
    /// reading back (lesson 9). With `sensitive` the body carries a secret
    /// (a password): an error reply is neither logged nor quoted in the
    /// error, since it might echo the body.
    pub async fn post_json_no_reply<B: Serialize + ?Sized>(
        &self,
        path: &str,
        query: &[(&str, String)],
        body: &B,
        sensitive: bool,
    ) -> Result<(), AdapterError> {
        self.request_with(
            Method::POST,
            path,
            query,
            Some(encode(body)?),
            None,
            MAX_JSON_BODY,
            Extra {
                also_not_found: None,
                sensitive,
                ..Extra::default()
            },
        )
        .await?;
        Ok(())
    }

    /// `DELETE` a path relative to the base URL. The reply body is ignored:
    /// callers verify by reading back (lesson 9).
    pub async fn delete(&self, path: &str) -> Result<(), AdapterError> {
        self.request(Method::DELETE, path, &[], None, None, MAX_JSON_BODY)
            .await?;
        Ok(())
    }

    /// Serialize operations on `key` within this process: one holder of
    /// the guard at a time. Scoped to this service instance (clones share
    /// it). Starr runs jobs in exactly one worker process (also in split
    /// mode, ADR 0003), so this stops two jobs from racing on the same
    /// remote object. Not covered: two Starr service rows pointing at the
    /// same remote service, and a write still running on a client that was
    /// just replaced after its URL or credential changed.
    pub async fn serialize(&self, key: &str) -> SerialGuard {
        self.inner.serial.lock(key).await
    }

    fn decode<T: DeserializeOwned>(
        &self,
        method: &Method,
        path: &str,
        body: &[u8],
    ) -> Result<T, AdapterError> {
        serde_json::from_slice(body).map_err(|err| {
            // serde errors quote offending values, so redact them too;
            // private content keeps neither them nor the body.
            let detail = if self.inner.private_content {
                format!("response of {method} {PRIVATE_LOG} is not the expected JSON")
            } else {
                format!(
                    "response of {method} {path} is not the expected JSON ({}): {}",
                    self.inner.redactor.redact(&err.to_string()),
                    self.snippet(body)
                )
            };
            tracing::warn!(service = self.inner.service, path = self.log_path(path), detail = %self.log_text(&detail), "unexpected response body");
            AdapterError::Unknown(detail)
        })
    }

    /// The base URL requests are relative to (for redirect checks of
    /// adapters whose images live next to their API); `None` for a client
    /// whose URL is itself the credential ([`Self::secret_url`]).
    #[must_use]
    pub fn base_url(&self) -> Option<&Url> {
        (!self.inner.secret_base).then_some(&self.inner.base_url)
    }

    /// The credential redactor, for adapters that log foreign data.
    #[must_use]
    pub fn redactor(&self) -> &Redactor {
        &self.inner.redactor
    }

    /// The request path as logs may show it.
    fn log_path<'a>(&self, path: &'a str) -> &'a str {
        if self.inner.private_content {
            PRIVATE_LOG
        } else {
            path
        }
    }

    /// A detail (an error, the service's words) as logs may show it.
    fn log_text<'a>(&self, text: &'a str) -> &'a str {
        if self.inner.private_content {
            PRIVATE_LOG
        } else {
            text
        }
    }

    /// [`AuthScheme::Session`]: the login the adapter exchanges for a
    /// session token; `None` for every other scheme.
    #[must_use]
    pub fn login(&self) -> Option<&LoginCredential> {
        self.inner.session.as_ref().map(|s| &s.login)
    }

    /// [`AuthScheme::Session`]: whether a session token is held. Clones
    /// share it, so it outlives one adapter (one job run).
    #[must_use]
    pub fn has_session(&self) -> bool {
        self.inner
            .session
            .as_ref()
            .is_some_and(|s| s.token().is_some())
    }

    /// [`AuthScheme::Session`]: use `token` (with or without `Bearer `)
    /// from now on.
    pub fn set_session_token(&self, token: &str) -> Result<(), AdapterError> {
        match &self.inner.session {
            Some(session) => session.set(token),
            None => Err(AdapterError::Unknown(
                "this client has no session to set".to_owned(),
            )),
        }
    }

    /// [`AuthScheme::Session`]: take over the login budget of `old` (the
    /// client this freshly built one replaces). No effect otherwise.
    fn keep_logins_of(&mut self, old: &Self) {
        let Some(budget) = old.inner.session.as_ref().and_then(|s| s.logins.clone()) else {
            return;
        };
        if let Some(session) = Arc::get_mut(&mut self.inner).and_then(|i| i.session.as_mut())
            && session.logins.is_some()
        {
            session.logins = Some(budget);
        }
    }

    /// [`AuthScheme::Session`]: forget the token (the service refused it).
    pub fn clear_session(&self) {
        if let Some(session) = &self.inner.session {
            *session.token.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
    }

    /// [`AuthScheme::Session`]: take a turn of the descriptor's login
    /// budget before logging in, or fail as [`AdapterError::Throttled`]
    /// right away (never queue logins: a service that counts them per IP
    /// would lock out every user behind it).
    pub fn login_turn(&self) -> Result<(), AdapterError> {
        let Some(limiter) = self.inner.session.as_ref().and_then(|s| s.logins.as_ref()) else {
            return Ok(());
        };
        limiter
            .check()
            .map_err(|not_until| AdapterError::Throttled {
                retry_after: Some(not_until.wait_time_from(governor::clock::Clock::now(
                    &governor::clock::DefaultClock::default(),
                ))),
            })
    }

    /// `GET` a path with query parameters and decode the JSON body, with
    /// the total the service reports in `X-Total-Count` (react-admin style
    /// paging, Navidrome); `None` without a readable one.
    pub async fn get_json_counted<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<(T, Option<u64>), AdapterError> {
        let response = self
            .request(Method::GET, path, query, None, None, MAX_JSON_BODY)
            .await?;
        let total = response
            .headers
            .get("x-total-count")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok());
        Ok((self.decode(&Method::GET, path, &response.body)?, total))
    }

    fn snippet(&self, body: &[u8]) -> String {
        // Redact before cutting, so a secret spanning the cut cannot leak
        // its prefix.
        let text = self.inner.redactor.redact(&String::from_utf8_lossy(body));
        text.chars().take(BODY_SNIPPET).collect()
    }

    /// How long this client still pauses after a service asked it to slow
    /// down (`None`: requests go out), so a caller can skip work that
    /// would need it.
    #[must_use]
    pub fn throttled_for(&self) -> Option<Duration> {
        self.throttle_remaining()
    }

    fn throttle_remaining(&self) -> Option<Duration> {
        let gate = self
            .inner
            .throttled_until
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        gate.and_then(|until| until.checked_duration_since(Instant::now()))
            .filter(|d| !d.is_zero())
    }

    fn throttle_for(&self, pause: Duration) {
        let mut gate = self
            .inner
            .throttled_until
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(until) = Instant::now().checked_add(pause.min(MAX_THROTTLE)) else {
            return;
        };
        if gate.is_none_or(|current| current < until) {
            *gate = Some(until);
        }
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Vec<u8>>,
        accept: Option<&str>,
        max_body: usize,
    ) -> Result<RawResponse, AdapterError> {
        self.request_with(
            method,
            path,
            query,
            body,
            accept,
            max_body,
            Extra::default(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn request_with(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Vec<u8>>,
        accept: Option<&str>,
        max_body: usize,
        extra: Extra,
    ) -> Result<RawResponse, AdapterError> {
        let service = self.inner.service;
        // What the logs may show of this request (private content: neither
        // the path, which can carry a title's key, nor foreign text).
        let log_path = self.log_path(path);
        // A 429 seen by anyone pauses everyone (lesson 11); fail fast and let
        // the job layer decide when to come back.
        if let Some(remaining) = self.throttle_remaining() {
            return Err(AdapterError::Throttled {
                retry_after: Some(remaining),
            });
        }
        self.wait_turn().await?;

        let mut url = if extra.absolute {
            Url::parse(path)
        } else {
            self.inner.base_url.join(path.trim_start_matches('/'))
        }
        .map_err(|err| AdapterError::Unknown(format!("invalid path {path:?}: {err}")))?;
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        let mut builder = self.builder(method.clone(), url, extra.anonymous);
        if let Some(accept) = accept {
            builder = builder.header(ACCEPT, accept);
        }
        let started = Instant::now();
        if let Some(body) = body {
            builder = builder.header(CONTENT_TYPE, "application/json").body(body);
        }
        let mut response = builder.send().await.map_err(|err| {
            let detail = self.inner.redactor.redact(&describe(&err));
            tracing::warn!(service, %method, path = log_path, detail = %self.log_text(&detail), "request failed");
            AdapterError::Transient(detail)
        })?;
        let status = response.status();
        let headers = response.headers().clone();
        let answered = response.url().clone();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|err| {
            let detail = self.inner.redactor.redact(&describe(&err));
            tracing::warn!(service, %method, path = log_path, %status, detail = %self.log_text(&detail), "reading response failed");
            AdapterError::Transient(detail)
        })? {
            if body.len() + chunk.len() > max_body {
                tracing::warn!(service, %method, path = log_path, max_body, "response body too large");
                return Err(AdapterError::Rejected(format!(
                    "{method} {log_path}: response larger than {max_body} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        tracing::debug!(service, %method, path = log_path, status = status.as_u16(), elapsed_ms = started.elapsed().as_millis(), "service request");

        if status.is_success() {
            self.on_success(&headers);
            return Ok(RawResponse {
                headers,
                body,
                location: None,
            });
        }
        if extra.redirect_ok
            && status.is_redirection()
            && let Some(location) = location(&headers, &answered)
        {
            return Ok(RawResponse {
                headers,
                body,
                location: Some(location),
            });
        }
        // A reply to a request that carried a secret may echo it: neither
        // logged nor quoted.
        let snippet = if extra.sensitive || self.inner.private_content {
            String::new()
        } else {
            self.snippet(&body)
        };
        tracing::warn!(service, %method, path = log_path, status = status.as_u16(), headers = ?redacted_headers(&headers, &self.inner.redactor), body = %snippet, "service returned an error");
        let context = format!("{method} {log_path}: HTTP {status}");
        if extra.also_not_found == Some(status) {
            return Err(AdapterError::NotFound(context));
        }
        let mut err = self.classify(status, &headers, &context);
        // A permanent rejection carries the service's reason (validation
        // messages), so admins see why without digging in logs.
        // Validation messages of the Servarr family are extracted from the
        // JSON body; other bodies are kept as a (redacted) snippet.
        if let AdapterError::Rejected(detail) = &mut err
            && status.is_client_error()
            && !extra.sensitive
            && !self.inner.private_content
        {
            let reason = match servarr::error_messages(&body) {
                Some(messages) => self.snippet(messages.as_bytes()),
                None => snippet.trim().to_owned(),
            };
            if !reason.is_empty() {
                detail.push_str(": ");
                detail.push_str(&reason);
            }
        }
        if let AdapterError::Throttled { retry_after } = &err {
            self.throttle_for(retry_after.unwrap_or(DEFAULT_THROTTLE));
        }
        Err(err)
    }
}

impl ServiceClient {
    /// A request with the credential, unless it is `anonymous`.
    fn builder(&self, method: Method, url: Url, anonymous: bool) -> reqwest::RequestBuilder {
        let request = self.inner.http.request(method, url);
        if anonymous {
            request
        } else {
            self.authorize(request)
        }
    }

    /// Add the credential header, or the session token once there is one.
    fn authorize(&self, mut builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some((name, value)) = &self.inner.auth_header {
            builder = builder.header(name, value.clone());
        }
        if let Some(session) = &self.inner.session
            && let Some(token) = session.token()
        {
            builder = builder.header(&session.header, token);
        }
        builder
    }

    /// After a successful reply: renew the session, and when the service
    /// says its window is used up (`ListenBrainz`), make everyone wait for
    /// it to reset instead of earning a 429.
    fn on_success(&self, headers: &HeaderMap) {
        if let Some(session) = &self.inner.session {
            session.renew(headers);
        }
        if let Some(pause) = window_used_up(headers) {
            // No path: it could name private content.
            tracing::debug!(
                service = self.inner.service,
                pause_s = pause.as_secs(),
                "rate-limit window used up; pausing"
            );
            self.throttle_for(pause);
        }
    }

    /// Wait for the limiter, at most [`ClientOptions::max_wait`].
    async fn wait_turn(&self) -> Result<(), AdapterError> {
        match self.inner.max_wait {
            None => {
                self.inner.limiter.until_ready().await;
                Ok(())
            }
            Some(wait) => tokio::time::timeout(wait, self.inner.limiter.until_ready())
                .await
                .map_err(|_| AdapterError::Throttled {
                    retry_after: Some(wait),
                }),
        }
    }

    /// [`classify`], with 503 as "slow down" for services that say so.
    fn classify(&self, status: StatusCode, headers: &HeaderMap, context: &str) -> AdapterError {
        if status == StatusCode::SERVICE_UNAVAILABLE && self.inner.unavailable_is_throttle {
            AdapterError::Throttled {
                retry_after: Some(retry_after(headers).unwrap_or(UNAVAILABLE_THROTTLE)),
            }
        } else {
            classify(status, headers, context)
        }
    }
}

/// The `Location` header of a redirect.
/// A relative `Location` is resolved against `answered`, the URL that sent it.
fn location(headers: &HeaderMap, answered: &Url) -> Option<String> {
    let raw = headers
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())?;
    Some(
        answered
            .join(raw)
            .map_or_else(|_| raw.to_owned(), String::from),
    )
}

/// Per-request options beyond the common ones (independent switches,
/// hence the bools).
#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)]
struct Extra {
    /// Besides 404 and 410, this status means [`AdapterError::NotFound`].
    also_not_found: Option<StatusCode>,
    /// The request body carries a secret (a password).
    sensitive: bool,
    /// A redirect with a `Location` is returned instead of refused.
    redirect_ok: bool,
    /// `path` is an absolute URL (a redirect target), not relative to the
    /// base URL.
    absolute: bool,
    /// Sent without the instance's credential or session token.
    anonymous: bool,
}

/// A successful response before decoding.
struct RawResponse {
    headers: HeaderMap,
    body: Vec<u8>,
    /// Set only for an accepted redirect ([`Extra::redirect_ok`]).
    location: Option<String>,
}

/// A binary resource as fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryResponse {
    /// The service's `Content-Type`, if it sent a readable one.
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// Cache key of a request: path plus encoded query, in the given order.
fn request_key(path: &str, query: &[(&str, String)]) -> String {
    let path = path.trim_start_matches('/');
    if query.is_empty() {
        return path.to_owned();
    }
    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())))
        .finish();
    format!("{path}?{encoded}")
}

fn encode<B: Serialize + ?Sized>(body: &B) -> Result<Vec<u8>, AdapterError> {
    serde_json::to_vec(body)
        .map_err(|err| AdapterError::Unknown(format!("cannot encode request body: {err}")))
}

/// `reqwest` errors say little in `Display`; include the cause chain.
fn describe(err: &reqwest::Error) -> String {
    let mut out = if err.is_timeout() {
        "timed out".to_owned()
    } else if err.is_connect() {
        "connection failed".to_owned()
    } else {
        "request error".to_owned()
    };
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
}

/// Response headers safe to log: everything but cookies.
fn redacted_headers(headers: &HeaderMap, redactor: &Redactor) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| {
            let name = name.as_str();
            name != "set-cookie" && !name.contains("authorization")
        })
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                redactor.redact(value.to_str().unwrap_or("<binary>")),
            )
        })
        .collect()
}

/// Map a non-success HTTP status to the error taxonomy.
fn classify(status: StatusCode, headers: &HeaderMap, context: &str) -> AdapterError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => AdapterError::CredentialsInvalid,
        StatusCode::TOO_MANY_REQUESTS => AdapterError::Throttled {
            retry_after: retry_after(headers),
        },
        StatusCode::NOT_FOUND | StatusCode::GONE => AdapterError::NotFound(context.to_owned()),
        StatusCode::REQUEST_TIMEOUT => AdapterError::Transient(context.to_owned()),
        s if s.is_server_error() => AdapterError::Transient(context.to_owned()),
        s if s.is_client_error() => AdapterError::Rejected(context.to_owned()),
        // Redirects are not followed on purpose (the credential must not
        // travel to another URL); typical causes are http->https or an auth
        // proxy in front of the service, so name the target host.
        s if s.is_redirection() => AdapterError::Rejected(format!(
            "{context}: redirects to {}",
            redirect_host(headers).unwrap_or_else(|| "an unknown location".to_owned())
        )),
        _ => AdapterError::Unknown(context.to_owned()),
    }
}

/// Scheme and host of a `Location` header (no path or query, which could
/// carry tokens).
fn redirect_host(headers: &HeaderMap) -> Option<String> {
    let location = headers.get(reqwest::header::LOCATION)?.to_str().ok()?;
    let url = Url::parse(location).ok()?;
    Some(format!("{}://{}", url.scheme(), url.host_str()?))
}

/// `Retry-After` in seconds, else `X-RateLimit-Reset-In` (`ListenBrainz`).
/// The HTTP-date form is not used by the services Starr talks to so far
/// and yields `None` (the default pause applies).
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    seconds(headers, RETRY_AFTER.as_str()).or_else(|| seconds(headers, RATE_LIMIT_RESET_IN))
}

/// Seconds until the service's rate-limit window resets.
const RATE_LIMIT_RESET_IN: &str = "x-ratelimit-reset-in";

/// Requests left in the service's rate-limit window.
const RATE_LIMIT_REMAINING: &str = "x-ratelimit-remaining";

/// Longest pause after a successful reply that used up the window: such
/// windows last seconds (`ListenBrainz`: 10 s), so a larger value (a proxy
/// with per-route windows in front of a service) never stops a whole
/// instance for long.
const WINDOW_PAUSE_MAX: Duration = Duration::from_mins(1);

/// How long to pause after a successful reply that reports no requests
/// left in the window (`X-RateLimit-Remaining: 0` with
/// `X-RateLimit-Reset-In`), at most [`WINDOW_PAUSE_MAX`]; `None` while
/// requests are left or either header is missing or not a number.
fn window_used_up(headers: &HeaderMap) -> Option<Duration> {
    let remaining = headers.get(RATE_LIMIT_REMAINING)?.to_str().ok()?.trim();
    if remaining.parse::<u64>().ok()? > 0 {
        return None;
    }
    seconds(headers, RATE_LIMIT_RESET_IN).map(|pause| pause.min(WINDOW_PAUSE_MAX))
}

/// A header holding whole seconds, capped at [`MAX_THROTTLE`].
fn seconds(headers: &HeaderMap, name: &str) -> Option<Duration> {
    headers
        .get(name)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_THROTTLE))
}

/// The cache key [`ServiceClient::get_json_cached`] uses for `path` and
/// `query`, for adapters that store a body they derived from the reply
/// (a ranked lane) where the web process will look for it.
#[must_use]
pub fn cache_key(path: &str, query: &[(&str, String)]) -> String {
    request_key(path, query)
}

/// One [`ServiceClient`] per service instance, rebuilt when the instance's
/// connection settings change (`fingerprint`). Shared by all jobs of a worker
/// so they share each instance's rate limiter and throttle gate.
#[derive(Debug, Default)]
pub struct ClientPool {
    clients: Mutex<HashMap<i64, (Vec<u8>, ServiceClient)>>,
}

impl ClientPool {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The client for `instance_id`, built with `build` if there is none or
    /// its `fingerprint` changed (new URL or credential).
    pub fn get_or_build(
        &self,
        instance_id: i64,
        fingerprint: &[u8],
        build: impl FnOnce() -> Result<ServiceClient, AdapterError>,
    ) -> Result<ServiceClient, AdapterError> {
        let mut clients = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((known, client)) = clients.get(&instance_id)
            && known.as_slice() == fingerprint
        {
            return Ok(client.clone());
        }
        let mut client = build()?;
        // A login budget is the instance's, not the client's: a corrected
        // password must not buy a second login within the window.
        if let Some((_, old)) = clients.get(&instance_id) {
            client.keep_logins_of(old);
        }
        clients.insert(instance_id, (fingerprint.to_vec(), client.clone()));
        Ok(client)
    }

    /// Forget the client of a deleted instance.
    pub fn remove(&self, instance_id: i64) {
        self.clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&instance_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn statuses_map_to_the_taxonomy() {
        let none = HeaderMap::new();
        let code = |s: u16| {
            classify(StatusCode::from_u16(s).unwrap(), &none, "ctx")
                .code()
                .to_owned()
        };
        assert_eq!(code(401), "credentials_invalid");
        assert_eq!(code(403), "credentials_invalid");
        assert_eq!(code(404), "not_found");
        assert_eq!(code(400), "rejected");
        assert_eq!(code(409), "rejected");
        assert_eq!(code(408), "transient");
        assert_eq!(code(500), "transient");
        assert_eq!(code(503), "transient");
        assert_eq!(code(302), "rejected");
        assert_eq!(code(101), "unknown");
        assert_eq!(code(429), "throttled");
    }

    /// Poll a future once without a runtime.
    fn ready<F: std::future::Future>(future: F) -> Option<F::Output> {
        let mut future = std::pin::pin!(future);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(out) => Some(out),
            std::task::Poll::Pending => None,
        }
    }

    #[test]
    fn keyed_locks_serialize_per_key_and_forget_unused_keys() {
        let locks = KeyedLocks::default();
        let held = ready(locks.lock("tvdb:1")).unwrap();
        assert!(ready(locks.lock("tvdb:1")).is_none(), "same key must wait");
        assert!(ready(locks.lock("tvdb:2")).is_some(), "other keys run");
        drop(held);
        assert!(ready(locks.lock("tvdb:1")).is_some(), "released");
        // Entries nobody holds are pruned on the next lock.
        let _again = ready(locks.lock("tvdb:3")).unwrap();
        let keys: Vec<_> = locks.locks.lock().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["tvdb:3".to_owned()]);
    }

    #[test]
    fn redirects_name_only_the_target_host() {
        let err = classify(
            StatusCode::FOUND,
            &headers(&[("location", "https://auth.example.com/login?rd=secret")]),
            "GET x",
        );
        assert_eq!(
            err,
            AdapterError::Rejected("GET x: redirects to https://auth.example.com".to_owned())
        );
    }

    #[test]
    fn retry_after_seconds_are_read_dates_are_ignored() {
        let err = classify(
            StatusCode::TOO_MANY_REQUESTS,
            &headers(&[("retry-after", "12")]),
            "ctx",
        );
        assert_eq!(
            err,
            AdapterError::Throttled {
                retry_after: Some(Duration::from_secs(12))
            }
        );
        let err = classify(
            StatusCode::TOO_MANY_REQUESTS,
            &headers(&[("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")]),
            "ctx",
        );
        assert_eq!(err, AdapterError::Throttled { retry_after: None });
    }

    #[test]
    fn shares_split_a_budget_without_exceeding_it() {
        let mb = RateLimit {
            requests: 1,
            per: Duration::from_secs(1),
        };
        let web = share_of(mb, 70);
        let worker = share_of(mb, 30);
        assert_eq!(web.requests, 1);
        assert_eq!(
            web.per,
            Duration::from_millis(1428) + Duration::from_nanos(571_428)
        );
        assert_eq!(
            worker.per,
            Duration::from_millis(3333) + Duration::from_nanos(333_333)
        );
        // Together at most one request per second.
        let rate = |l: RateLimit| f64::from(l.requests) / l.per.as_secs_f64();
        assert!(rate(web) + rate(worker) <= 1.0 + 1e-9);
        assert_eq!(share_of(mb, 100), mb);
        assert_eq!(share_of(mb, 0).per, Duration::from_secs(100));
    }

    #[test]
    fn quota_spreads_requests_over_the_period() {
        let q = quota(RateLimit {
            requests: 10,
            per: Duration::from_secs(1),
        });
        assert_eq!(q.burst_size().get(), 10);
        assert_eq!(q.replenish_interval(), Duration::from_millis(100));
        // Degenerate budgets still yield a usable limiter.
        let q = quota(RateLimit {
            requests: 0,
            per: Duration::ZERO,
        });
        assert_eq!(q.burst_size().get(), 1);
    }
}
