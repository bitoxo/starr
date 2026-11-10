//! Authentication and request guards (ADR 0005).
//!
//! Everything under `/api/` is **closed by default**: [`require_session`]
//! wraps the whole application router (not individual routes), so even a
//! route missing from the `OpenAPI` document is protected. A request passes
//! only if its method and path are in [`PUBLIC_ENDPOINTS`] or it carries a
//! valid session. Handlers then pick the account kind they need with the
//! [`AnySession`], [`AdminSession`] or [`MemberSession`] extractors.
//! [`check_origin`] rejects cross-site mutating requests (CSRF).

pub mod crypto;
pub mod device;
pub mod request;
pub mod totp;

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::{DateTime, TimeDelta, Utc};
use secrecy::{ExposeSecret, SecretString};
use starr_core::auth::{
    DEVICE_COOKIE_LIFETIME, SESSION_ABSOLUTE_LIFETIME, SESSION_IDLE_TIMEOUT, SessionValidity,
    Subject, SubjectKind, session_validity,
};
use starr_db::Database;
use starr_db::auth::{NewSession, RotatedSession};

use crate::AppState;
use crate::api::error::ApiError;
use crypto::{TokenHash, hash_token, hashes_equal};

/// An API route that works without a session.
#[derive(Debug, Clone, Copy)]
pub struct PublicEndpoint {
    pub method: &'static str,
    /// Route path exactly as registered (the `OpenAPI` path).
    pub path: &'static str,
    /// Why this endpoint must be reachable without logging in.
    pub reason: &'static str,
}

/// The only API routes reachable without a session. Everything else under
/// `/api/` requires one. Keep this list short and every reason concrete.
pub const PUBLIC_ENDPOINTS: &[PublicEndpoint] = &[
    PublicEndpoint {
        method: "GET",
        path: "/api/v1/health",
        reason: "Docker healthcheck and uptime probes run without credentials",
    },
    PublicEndpoint {
        method: "GET",
        path: "/api/v1/system/status",
        reason: "UI shows reachability and version before anyone is logged in",
    },
    PublicEndpoint {
        method: "GET",
        path: "/api/v1/setup/state",
        reason: "UI decides between setup page and login page",
    },
    PublicEndpoint {
        method: "POST",
        path: "/api/v1/setup/admin",
        reason: "Creates the first admin; guarded by the one-time setup token instead",
    },
    PublicEndpoint {
        method: "POST",
        path: "/api/v1/auth/login",
        reason: "Starts a session; throttled per username, device and IP",
    },
    PublicEndpoint {
        method: "POST",
        path: "/api/v1/auth/login/totp",
        reason: "Second login step for admins with TOTP; needs the short-lived single-use token of the password step, code attempts throttled",
    },
    PublicEndpoint {
        method: "POST",
        path: "/api/v1/auth/logout",
        reason: "Must always clear the cookie, even when the session already expired",
    },
    PublicEndpoint {
        method: "GET",
        path: "/api/v1/invitations/{token}",
        reason: "Invited people have no account yet; the accept page shows presets and expiry. Guarded by the 256-bit token, rate-limited",
    },
    PublicEndpoint {
        method: "POST",
        path: "/api/v1/invitations/{token}/accept",
        reason: "Creates the invited member's account; guarded by the single-use token, rate-limited",
    },
    PublicEndpoint {
        method: "GET",
        path: "/api/v1/password-tokens/{token}",
        reason: "The set-password page checks its link before asking for a password; guarded by the 256-bit token, rate-limited, says nothing about the account",
    },
    PublicEndpoint {
        method: "POST",
        path: "/api/v1/password-tokens/{token}",
        reason: "Sets a password from an admin-issued link before the member can log in; single-use token, rate-limited",
    },
    PublicEndpoint {
        method: "GET",
        path: "/api/v1/login-art",
        reason: "Poster art behind the login page: file names from the cached movie and series Trending lists only (never the private category), no catalog call",
    },
    PublicEndpoint {
        method: "GET",
        path: "/api/v1/login-art/{file}",
        reason: "The login page's posters; serves exactly the files the login-art list names right now, so the image proxy stays closed to visitors",
    },
];

/// Whether a request may skip the session check. Paths are compared segment
/// by segment as received: a `{param}` segment of the template matches one
/// non-empty segment, everything else must be equal. Any variant that does not
/// match (trailing slash, double slash, extra segment) stays closed.
fn is_public(method: &Method, path: &str) -> bool {
    // HEAD is served by GET handlers.
    let method = if method == Method::HEAD {
        "GET"
    } else {
        method.as_str()
    };
    PUBLIC_ENDPOINTS
        .iter()
        .any(|e| e.method == method && path_matches(e.path, path))
}

fn path_matches(template: &str, path: &str) -> bool {
    let mut actual = path.split('/');
    for expected in template.split('/') {
        let Some(segment) = actual.next() else {
            return false;
        };
        let is_param = expected.starts_with('{') && expected.ends_with('}');
        if is_param && segment.is_empty() || !is_param && segment != expected {
            return false;
        }
    }
    actual.next().is_none()
}

fn is_api(path: &str) -> bool {
    path.starts_with("/api/")
}

/// What an admin may still do while `STARR_ADMIN_TOTP_REQUIRED` is set and
/// their TOTP is off: see who they are and enroll. (Logout is public.)
const TOTP_ENROLLMENT_ENDPOINTS: &[(&str, &str)] = &[
    ("GET", "/api/v1/auth/me"),
    ("GET", "/api/v1/auth/totp"),
    ("POST", "/api/v1/auth/totp/enroll"),
    ("POST", "/api/v1/auth/totp/confirm"),
];

fn allowed_during_enrollment(method: &Method, path: &str) -> bool {
    let method = if method == Method::HEAD {
        "GET"
    } else {
        method.as_str()
    };
    TOTP_ENROLLMENT_ENDPOINTS
        .iter()
        .any(|(m, p)| *m == method && *p == path)
}

/// An authenticated request: who it is, and which session it uses.
#[derive(Debug, Clone)]
pub struct Session {
    pub subject: Subject,
    /// Hash of the session token (to tell the current session apart).
    pub token_hash: TokenHash,
    /// Member view of an admin: the admin who may switch back.
    pub origin_admin_id: Option<i64>,
    /// Re-authenticated until then (switching back to admin is allowed).
    pub elevated_until: Option<DateTime<Utc>>,
    /// Admin subjects: TOTP is on.
    pub totp_enabled: bool,
    /// The private (adult) category is unlocked until then (spec 0007 Q7);
    /// read through `api::privacy`, never on its own.
    pub private_unlocked_until: Option<DateTime<Utc>>,
}

/// Names and flags of Starr's cookies for one request. On https (as reported
/// by the trusted proxy) cookies use the `__Host-` prefix and `Secure`; on
/// plain http (LAN access, development) the unprefixed names without `Secure`.
/// Each request reads only the cookies matching its own scheme.
#[derive(Debug, Clone, Copy)]
pub struct CookieScheme {
    https: bool,
}

impl CookieScheme {
    #[must_use]
    pub fn for_request(headers: &HeaderMap, behind_proxy: bool) -> Self {
        Self {
            https: request::is_https(headers, behind_proxy),
        }
    }

    #[must_use]
    pub fn session_name(self) -> &'static str {
        if self.https {
            "__Host-starr_session"
        } else {
            "starr_session"
        }
    }

    #[must_use]
    pub fn device_name(self) -> &'static str {
        if self.https {
            "__Host-starr_device"
        } else {
            "starr_device"
        }
    }

    fn base(self, name: &'static str, value: String) -> Cookie<'static> {
        Cookie::build((name, value))
            .http_only(true)
            .secure(self.https)
            .same_site(SameSite::Lax)
            .path("/")
            .build()
    }

    fn build(self, name: &'static str, value: String, lifetime: TimeDelta) -> Cookie<'static> {
        let mut cookie = self.base(name, value);
        cookie.set_max_age(cookie::time::Duration::seconds(lifetime.num_seconds()));
        cookie
    }

    /// A cookie that makes the browser delete `name` (same attributes, so a
    /// `__Host-` cookie is accepted).
    fn removal(self, name: &'static str) -> Cookie<'static> {
        let mut cookie = self.base(name, String::new());
        cookie.make_removal();
        cookie
    }
}

/// Middleware on the whole application with `STARR_BEHIND_PROXY=true`:
/// `X-Forwarded-*` from a peer that cannot be the proxy are removed before
/// anything reads them (see [`request::drop_untrusted_forwarded`]).
pub async fn trust_forwarded_from_proxy(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    if state.settings.behind_proxy {
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(peer)| peer.ip());
        request::drop_untrusted_forwarded(req.headers_mut(), peer);
    }
    next.run(req).await
}

/// Middleware on the whole application: under `/api/`, public endpoints pass,
/// everything else needs a valid session, which is then available to the
/// session extractors. Paths outside `/api/` (the web UI) are untouched.
pub async fn require_session(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if !is_api(path) || is_public(req.method(), path) {
        return next.run(req).await;
    }
    let scheme = CookieScheme::for_request(req.headers(), state.settings.behind_proxy);
    match load_session(&state, req.headers(), scheme).await {
        Ok(Some(session)) => {
            // STARR_ADMIN_TOTP_REQUIRED: until TOTP is on, an admin session
            // only reaches the enrollment endpoints.
            if session.subject.kind == SubjectKind::Admin
                && state.settings.admin_totp_required
                && !session.totp_enabled
                && !allowed_during_enrollment(req.method(), req.uri().path())
            {
                return ApiError::TotpEnrollmentRequired.into_response();
            }
            req.extensions_mut().insert(session);
            next.run(req).await
        }
        Ok(None) => ApiError::Unauthenticated.into_response(),
        Err(err) => err.into_response(),
    }
}

/// Resolve the session cookie to a live session, applying idle and absolute
/// lifetime. Expired sessions are deleted.
pub(crate) async fn load_session(
    state: &AppState,
    headers: &HeaderMap,
    scheme: CookieScheme,
) -> Result<Option<Session>, ApiError> {
    let jar = CookieJar::from_headers(headers);
    let Some(cookie) = jar.get(scheme.session_name()) else {
        return Ok(None);
    };
    let token_hash = hash_token(cookie.value());
    let Some(record) = state.db.find_session(&token_hash).await? else {
        return Ok(None);
    };
    let now = state.clock.now();
    match session_validity(record.expires_at, record.last_seen_at, now) {
        SessionValidity::Expired => {
            state.db.delete_session(&token_hash).await?;
            Ok(None)
        }
        SessionValidity::Valid { touch } => {
            if touch {
                state.db.touch_session(&token_hash, now).await?;
            }
            Ok(Some(Session {
                subject: record.subject,
                token_hash,
                origin_admin_id: record.origin_admin_id,
                elevated_until: record.elevated_until,
                totp_enabled: record.totp_enabled,
                private_unlocked_until: record.private_unlocked_until,
            }))
        }
    }
}

/// CSRF defence for everything under `/api/`: mutating methods need an
/// allowed `Origin` (see [`request::origin_allowed`]).
pub async fn check_origin(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if safe
        || !is_api(req.uri().path())
        || request::origin_allowed(
            req.headers(),
            state.settings.public_url.as_ref(),
            &state.settings.additional_origins,
        )
    {
        next.run(req).await
    } else {
        ApiError::OriginMismatch.into_response()
    }
}

/// Any logged-in account.
#[derive(Debug)]
pub struct AnySession(pub Session);

/// A logged-in admin; members get `403`.
#[derive(Debug)]
pub struct AdminSession(pub Session);

/// A logged-in member; admins get `403`.
#[derive(Debug)]
pub struct MemberSession(pub Session);

/// The session placed by [`require_session`]. Its absence means the handler
/// runs without the auth layer (or on a public route), which is a wiring bug:
/// fail loudly instead of quietly answering 401, so the `OpenAPI` sweep test
/// notices a missing layer. Use one session extractor per handler: the first
/// one takes the session.
fn take_session(parts: &mut Parts) -> Result<Session, ApiError> {
    parts.extensions.remove::<Session>().ok_or_else(|| {
        tracing::error!(path = %parts.uri.path(), "session extractor used without the auth layer");
        ApiError::Internal
    })
}

impl<S: Send + Sync> FromRequestParts<S> for AnySession {
    type Rejection = ApiError;
    fn from_request_parts(
        parts: &mut Parts,
        _: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(take_session(parts).map(Self))
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AdminSession {
    type Rejection = ApiError;
    fn from_request_parts(
        parts: &mut Parts,
        _: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            take_session(parts).and_then(|session| match session.subject.kind {
                SubjectKind::Admin => Ok(Self(session)),
                SubjectKind::Member => Err(ApiError::Forbidden),
            }),
        )
    }
}

impl<S: Send + Sync> FromRequestParts<S> for MemberSession {
    type Rejection = ApiError;
    fn from_request_parts(
        parts: &mut Parts,
        _: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            take_session(parts).and_then(|session| match session.subject.kind {
                SubjectKind::Member => Ok(Self(session)),
                SubjectKind::Admin => Err(ApiError::Forbidden),
            }),
        )
    }
}

/// Who is calling: client IP (see [`request::client_ip`]) and the cookie
/// scheme of the request.
#[derive(Debug)]
pub struct Client {
    pub ip: IpAddr,
    pub cookies: CookieScheme,
}

impl FromRequestParts<AppState> for Client {
    type Rejection = ApiError;
    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let behind_proxy = state.settings.behind_proxy;
        // Serving without connect info would put every client into one
        // throttling bucket: refuse instead.
        let client =
            if let Some(ConnectInfo(peer)) = parts.extensions.get::<ConnectInfo<SocketAddr>>() {
                Ok(Self {
                    ip: request::client_ip(&parts.headers, *peer, behind_proxy),
                    cookies: CookieScheme::for_request(&parts.headers, behind_proxy),
                })
            } else {
                tracing::error!("no socket address on request; serve with connect info");
                Err(ApiError::Internal)
            };
        std::future::ready(client)
    }
}

/// Maximum stored `User-Agent` length.
const USER_AGENT_MAX_CHARS: usize = 256;

/// Create a session for `subject`, renew the device cookie and return the
/// cookies. Any session presented with the request is deleted (rotation).
pub async fn start_session(
    state: &AppState,
    jar: CookieJar,
    headers: &HeaderMap,
    client: &Client,
    subject: &Subject,
) -> Result<CookieJar, ApiError> {
    let scheme = client.cookies;
    if let Some(old) = jar.get(scheme.session_name()) {
        state.db.delete_session(&hash_token(old.value())).await?;
    }
    let now = state.clock.now();
    housekeeping(&state.db, now).await?;

    let token = crypto::new_token();
    let user_agent: Option<String> = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|ua| ua.chars().take(USER_AGENT_MAX_CHARS).collect());
    state
        .db
        .create_session(
            &NewSession {
                token_hash: &hash_token(token.expose_secret()),
                subject_kind: subject.kind,
                subject_id: subject.id,
                expires_at: now + SESSION_ABSOLUTE_LIFETIME,
                ip: &client.ip.to_string(),
                user_agent: user_agent.as_deref(),
            },
            now,
        )
        .await?;
    let device = device::issue(&state.device_key().await?, &subject.username);
    Ok(jar
        .add(scheme.build(
            scheme.session_name(),
            token.expose_secret().to_owned(),
            SESSION_ABSOLUTE_LIFETIME,
        ))
        .add(scheme.build(scheme.device_name(), device, DEVICE_COOKIE_LIFETIME)))
}

/// The subject a rotated session continues with.
#[derive(Debug, Clone, Copy)]
pub struct NextSession {
    pub kind: SubjectKind,
    pub id: i64,
    /// Member view: the admin who may switch back.
    pub origin_admin_id: Option<i64>,
    pub elevated_until: Option<DateTime<Utc>>,
}

/// Replace the current session by a new token (switch, elevation): the old
/// token stops working at once. Keeps the absolute expiry of the login.
pub async fn rotate_session(
    state: &AppState,
    jar: CookieJar,
    scheme: CookieScheme,
    current: &Session,
    next: NextSession,
) -> Result<CookieJar, ApiError> {
    let token = crypto::new_token();
    let rotated = state
        .db
        .rotate_session(
            &current.token_hash,
            &RotatedSession {
                token_hash: &hash_token(token.expose_secret()),
                subject_kind: next.kind,
                subject_id: next.id,
                origin_admin_id: next.origin_admin_id,
                elevated_until: next.elevated_until,
            },
            state.clock.now(),
        )
        .await?;
    if !rotated {
        return Err(ApiError::Unauthenticated);
    }
    Ok(jar.add(scheme.build(
        scheme.session_name(),
        token.expose_secret().to_owned(),
        SESSION_ABSOLUTE_LIFETIME,
    )))
}

/// The device id of a valid device cookie for `username`, if presented.
pub async fn presented_device(
    state: &AppState,
    jar: &CookieJar,
    scheme: CookieScheme,
    username: &str,
) -> Result<Option<String>, ApiError> {
    let Some(cookie) = jar.get(scheme.device_name()) else {
        return Ok(None);
    };
    Ok(device::verify(
        &state.device_key().await?,
        cookie.value(),
        username,
    ))
}

/// Drop dead sessions and old login attempts. Runs on login, which is rare
/// enough to keep this cheap and frequent enough to keep the tables small.
async fn housekeeping(db: &Database, now: DateTime<Utc>) -> Result<(), ApiError> {
    db.delete_expired_sessions(now, now - SESSION_IDLE_TIMEOUT)
        .await?;
    db.delete_expired_pending_logins(now).await?;
    db.prune_login_attempts(now - TimeDelta::days(1)).await?;
    Ok(())
}

/// Delete the presented session (if any) and clear the session cookie.
/// Returns the subject of a live session, for auditing.
pub async fn end_session(
    state: &AppState,
    jar: CookieJar,
    headers: &HeaderMap,
    scheme: CookieScheme,
) -> Result<(CookieJar, Option<Subject>), ApiError> {
    let session = load_session(state, headers, scheme).await?;
    if let Some(cookie) = jar.get(scheme.session_name()) {
        state.db.delete_session(&hash_token(cookie.value())).await?;
    }
    // `add`, not `remove`: the jar only emits removals for cookies it saw,
    // and logout must clear the cookie unconditionally.
    let jar = jar.add(scheme.removal(scheme.session_name()));
    Ok((jar, session.map(|s| s.subject)))
}

/// The one-time setup token, held only as a hash in memory while no admin
/// exists. A restart before setup issues a new one.
#[derive(Clone, Default)]
pub struct SetupToken(Arc<Mutex<Option<TokenHash>>>);

impl std::fmt::Debug for SetupToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetupToken(..)")
    }
}

impl SetupToken {
    /// Generate a new token, keep its hash and return it (to be logged once).
    pub fn issue(&self) -> SecretString {
        let token = crypto::new_token();
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(hash_token(token.expose_secret()));
        token
    }

    /// Whether `candidate` is the current token. False once consumed.
    #[must_use]
    pub fn matches(&self, candidate: &SecretString) -> bool {
        let candidate = hash_token(candidate.expose_secret());
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|hash| hashes_equal(hash, &candidate))
    }

    /// Invalidate the token.
    pub fn consume(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_list_matches_methods_and_paths_exactly() {
        assert!(is_public(&Method::GET, "/api/v1/health"));
        assert!(is_public(&Method::HEAD, "/api/v1/health"));
        assert!(!is_public(&Method::POST, "/api/v1/health"));
        assert!(!is_public(&Method::GET, "/api/v1/auth/me"));
        assert!(!is_public(&Method::GET, "/api/v1/health/"));
        assert!(!is_public(&Method::GET, "/api/v1//health"));
    }

    #[test]
    fn public_templates_match_one_segment_per_parameter() {
        assert!(is_public(&Method::GET, "/api/v1/invitations/abc"));
        assert!(is_public(&Method::POST, "/api/v1/invitations/abc/accept"));
        assert!(is_public(&Method::POST, "/api/v1/password-tokens/abc"));
        assert!(!is_public(&Method::GET, "/api/v1/invitations"));
        assert!(!is_public(&Method::POST, "/api/v1/invitations"));
        assert!(!is_public(&Method::GET, "/api/v1/invitations/"));
        assert!(!is_public(&Method::POST, "/api/v1/invitations/1/revoke"));
        assert!(!is_public(&Method::GET, "/api/v1/invitations/a/b"));
        assert!(!is_public(&Method::POST, "/api/v1/invitations//accept"));
        assert!(!is_public(&Method::POST, "/api/v1/password-tokens/abc/x"));
        assert!(is_public(&Method::GET, "/api/v1/password-tokens/abc"));
        assert!(is_public(&Method::GET, "/api/v1/login-art/abc.jpg"));
        assert!(!is_public(&Method::GET, "/api/v1/login-art/a/b.jpg"));
        assert!(!is_public(&Method::POST, "/api/v1/login-art"));
    }

    #[test]
    fn enrollment_allows_only_identity_and_totp_setup() {
        assert!(allowed_during_enrollment(&Method::GET, "/api/v1/auth/me"));
        assert!(allowed_during_enrollment(
            &Method::POST,
            "/api/v1/auth/totp/enroll"
        ));
        assert!(!allowed_during_enrollment(
            &Method::POST,
            "/api/v1/auth/totp/disable"
        ));
        assert!(!allowed_during_enrollment(&Method::GET, "/api/v1/members"));
        assert!(!allowed_during_enrollment(&Method::POST, "/api/v1/auth/me"));
    }

    #[test]
    fn setup_token_is_single_use() {
        let store = SetupToken::default();
        assert!(!store.matches(&SecretString::from("anything")));
        let token = store.issue();
        assert!(store.matches(&token));
        assert!(!store.matches(&SecretString::from("wrong")));
        store.consume();
        assert!(!store.matches(&token));
    }

    #[test]
    fn cookie_names_and_flags_follow_the_scheme() {
        let https = CookieScheme { https: true };
        let http = CookieScheme { https: false };
        let c = https.build(https.session_name(), "v".into(), TimeDelta::days(1));
        assert_eq!(c.name(), "__Host-starr_session");
        assert_eq!(c.secure(), Some(true));
        assert_eq!(c.path(), Some("/"));
        assert!(c.domain().is_none());
        let c = http.build(http.session_name(), "v".into(), TimeDelta::days(1));
        assert_eq!(c.name(), "starr_session");
        assert_eq!(c.secure(), Some(false));
        assert_eq!(https.device_name(), "__Host-starr_device");
        assert_eq!(http.device_name(), "starr_device");
    }
}
