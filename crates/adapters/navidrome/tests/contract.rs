//! Navidrome contract tests against a local fake (never a real Navidrome).
//!
//! The fixtures in `fixtures/0.64.2/derived/` are **hand-written from
//! Navidrome's source** (`model/album.go`, `model/library.go`,
//! `server/auth.go`, the Subsonic response envelope), not recorded: shapes
//! and field names follow the source's JSON tags, values are invented. They
//! stay for what the maintainer's recording (`recorded/`, replayed in
//! `recorded.rs`) cannot show: albums with `MusicBrainz` tags, a second
//! library, albums with only missing files. The ping is the recorded one. The
//! fake is stricter than the service: it answers the native API only with
//! the current token, returns at most [`PAGE_CAP`] albums per page whatever
//! `_end` asks for, filters by `library_id` and counts logins.
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use secrecy::SecretString;
use serde_json::Value;
use starr_adapter_kit::{Endpoint, LoginCredential, ServiceClient};
use starr_adapter_navidrome::{AUTH, DESCRIPTOR, Navidrome};
use starr_core::adapters::{AdapterError, Probe, Version};
use starr_core::library::{ItemKind, Library, LibraryContent, UnitCounts};
use url::Url;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/0.64.2/derived");
const RECORDED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/0.64.2/recorded");
const PASSWORD: &str = "admin-pass-0123";
/// Most albums the fake returns per page.
const PAGE_CAP: usize = 2;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}/{name}")).unwrap()
}

#[allow(clippy::struct_excessive_bools)] // independent fake modes
#[derive(Debug, Default)]
struct State {
    /// The token the fake accepts; `None` until a login.
    token: Option<String>,
    logins: usize,
    is_admin: bool,
    /// Albums of every library.
    albums: Vec<Value>,
    /// Drop `X-Total-Count`.
    no_total: bool,
    /// Report one more album after the first page.
    growing_total: bool,
    /// Ignore the `library_id` filter (an older Navidrome).
    ignore_filter: bool,
    /// The ping's server type and version.
    kind: String,
    version: String,
    /// Every request as `METHOD path`.
    seen: Vec<String>,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<State>>);

impl Respond for Fake {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.0.lock().unwrap();
        let path = request.url.path().to_owned();
        state.seen.push(format!("{} {path}", request.method));
        let query: std::collections::HashMap<String, String> =
            request.url.query_pairs().into_owned().collect();
        match (request.method.as_str(), path.as_str()) {
            ("GET", "/rest/ping") => {
                // As recorded from 0.64.2 (`recorded/`).
                let mut ping: Value = serde_json::from_str(
                    &std::fs::read_to_string(format!("{RECORDED}/rest_ping_no_creds.json"))
                        .unwrap(),
                )
                .unwrap();
                ping["subsonic-response"]["type"] = Value::String(state.kind.clone());
                ping["subsonic-response"]["serverVersion"] = Value::String(state.version.clone());
                // A Subsonic error is an HTTP 200.
                return ResponseTemplate::new(200).set_body_json(ping);
            }
            ("POST", "/auth/login") => {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                state.logins += 1;
                if body["username"] != "starr" || body["password"] != PASSWORD {
                    return ResponseTemplate::new(401)
                        .set_body_string(r#"{"error":"Invalid username or password"}"#);
                }
                let token = format!("token-{}", state.logins);
                state.token = Some(token.clone());
                let mut reply: Value = serde_json::from_str(&fixture("login.json")).unwrap();
                reply["token"] = Value::String(token);
                reply["isAdmin"] = Value::Bool(state.is_admin);
                return ResponseTemplate::new(200).set_body_json(reply);
            }
            _ => {}
        }
        let sent = request
            .headers
            .get("x-nd-authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let valid = state.token.as_ref().map(|t| format!("Bearer {t}"));
        if sent.is_none() || sent != valid {
            return ResponseTemplate::new(401).set_body_string("Unauthorized");
        }
        // Sliding renewal: a new token with every answer.
        let renewed = format!("{}-r", state.token.clone().unwrap());
        state.token = Some(renewed.clone());
        let reply = |body: Value, total: Option<usize>| {
            let mut r = ResponseTemplate::new(200)
                .insert_header("x-nd-authorization", renewed.as_str())
                .set_body_json(body);
            if let Some(total) = total {
                r = r.insert_header("x-total-count", total.to_string().as_str());
            }
            r
        };
        match (request.method.as_str(), path.as_str()) {
            ("GET", "/api/library") => {
                let libraries: Value = serde_json::from_str(&fixture("library.json")).unwrap();
                let n = libraries.as_array().unwrap().len();
                reply(libraries, Some(n))
            }
            ("GET", "/api/album") => {
                let start: usize = query["_start"].parse().unwrap();
                let end: usize = query["_end"].parse().unwrap();
                assert_eq!(query.get("_sort").map(String::as_str), Some("id"));
                let library: Option<u64> = query.get("library_id").map(|l| l.parse().unwrap());
                let matching: Vec<Value> = state
                    .albums
                    .iter()
                    .filter(|a| state.ignore_filter || library == a["libraryId"].as_u64())
                    .cloned()
                    .collect();
                let page: Vec<Value> = matching
                    .iter()
                    .skip(start)
                    .take(end.saturating_sub(start).min(PAGE_CAP))
                    .cloned()
                    .collect();
                let mut total = matching.len();
                if state.growing_total && start > 0 {
                    total += 1;
                }
                reply(Value::Array(page), (!state.no_total).then_some(total))
            }
            _ => ResponseTemplate::new(404),
        }
    }
}

async fn fake(state: State) -> (MockServer, Arc<Mutex<State>>) {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(state));
    Mock::given(wiremock::matchers::any())
        .respond_with(Fake(Arc::clone(&state)))
        .mount(&server)
        .await;
    (server, state)
}

fn standard() -> State {
    State {
        is_admin: true,
        albums: serde_json::from_str(&fixture("albums_library_1.json")).unwrap(),
        kind: "navidrome".to_owned(),
        version: "0.64.2 (3f1c2b7)".to_owned(),
        ..State::default()
    }
}

fn client(server: &MockServer, password: &str) -> ServiceClient {
    ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(&server.uri()).unwrap(),
            auth: AUTH,
            credential: LoginCredential::encode("starr", &SecretString::from(password.to_owned())),
        },
    )
    .unwrap()
}

#[tokio::test]
async fn probe_libraries_and_albums_with_one_login_and_only_reads() {
    let (server, state) = fake(standard()).await;
    let client = client(&server, PASSWORD);
    let navidrome = Navidrome::new(client.clone());
    let report = navidrome.probe().await.unwrap();
    assert_eq!(report.version, Version::new(0, 64, 2, 0));
    assert_eq!(navidrome.server_id().await.unwrap(), None);

    let libraries = navidrome.libraries().await.unwrap();
    assert!(libraries.complete);
    let got: Vec<_> = libraries
        .items
        .iter()
        .map(|l| {
            (
                l.id.as_str(),
                l.grant_id.as_deref(),
                l.name.as_str(),
                l.content,
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("1", Some("1"), "Music Library", LibraryContent::Music),
            ("2", Some("2"), "Audiobooks", LibraryContent::Music),
        ]
    );

    // A new adapter on the shared client (the next job) reuses the token.
    let albums = Navidrome::new(client).items("1").await.unwrap();
    assert!(albums.complete, "paged by what arrived, total matched");
    let got: Vec<_> = albums
        .items
        .iter()
        .map(|a| {
            (
                a.remote_id.as_str(),
                a.kind,
                a.title.as_str(),
                a.year,
                a.units.clone(),
                a.provider_ids.mb_release_group.map(|u| u.to_string()),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (
                "0Fq7nT2sYp8Vw3Ld5Hk9Bz",
                ItemKind::Album,
                "OK Computer",
                Some(1997),
                UnitCounts::Tracks(12),
                Some("b1392450-e666-3926-a536-22c65f834433".to_owned())
            ),
            (
                "1Gr8oU3tZq9Wx4Me6Il0Ca",
                ItemKind::Album,
                "Kid A",
                Some(2000),
                UnitCounts::Tracks(7),
                Some("1b022e01-4da6-387b-8658-8678046e4cef".to_owned())
            ),
            // No MBID tags: listed, never matched by name.
            (
                "2Hs9pV4uAr0Xy5Nf7Jm1Db",
                ItemKind::Album,
                "Untagged Rip",
                None,
                UnitCounts::Tracks(10),
                None
            ),
            // "In Rainbows" has only missing files: counted, not listed.
        ]
    );
    assert_eq!(
        albums.items[0]
            .provider_ids
            .mb_artist
            .map(|u| u.to_string()),
        Some("a74b1b7f-71a5-4011-9441-d0b5e4122711".to_owned())
    );

    let state = state.lock().unwrap();
    assert_eq!(state.logins, 1, "one login for the whole run");
    assert!(
        state
            .seen
            .iter()
            .all(|r| r.starts_with("GET ") || r == "POST /auth/login"),
        "only reads besides the login: {:?}",
        state.seen
    );
    let album_pages = state.seen.iter().filter(|r| *r == "GET /api/album").count();
    assert_eq!(album_pages, 2, "4 albums at 2 per page");
}

#[tokio::test]
async fn a_refused_token_is_dropped_and_the_new_login_keeps_to_the_login_budget() {
    let (server, state) = fake(standard()).await;
    let client = client(&server, PASSWORD);
    let navidrome = Navidrome::new(client.clone());
    navidrome.libraries().await.unwrap();
    // The service forgets the token (restart, admin password changed).
    state.lock().unwrap().token = Some("other".to_owned());
    // The re-login falls into the 20 s login budget: no second login.
    let err = navidrome.libraries().await.unwrap_err();
    assert!(matches!(err, AdapterError::Throttled { .. }), "{err:?}");
    assert_eq!(state.lock().unwrap().logins, 1);
}

#[tokio::test]
async fn a_wrong_password_or_a_plain_account_is_refused() {
    let (server, _) = fake(standard()).await;
    let err = Navidrome::new(client(&server, "wrong"))
        .probe()
        .await
        .unwrap_err();
    assert_eq!(err, AdapterError::CredentialsInvalid);

    let (server, _) = fake(State {
        is_admin: false,
        ..standard()
    })
    .await;
    let err = Navidrome::new(client(&server, PASSWORD))
        .libraries()
        .await
        .unwrap_err();
    assert!(matches!(err, AdapterError::Rejected(_)), "{err:?}");
}

#[tokio::test]
async fn another_product_or_version_is_refused() {
    let (server, state) = fake(State {
        kind: "gonic".to_owned(),
        ..standard()
    })
    .await;
    let err = Navidrome::new(client(&server, PASSWORD))
        .probe()
        .await
        .unwrap_err();
    assert!(matches!(err, AdapterError::Unknown(_)), "{err:?}");
    assert_eq!(state.lock().unwrap().logins, 0, "no login for a stranger");

    let (server, _) = fake(State {
        version: "0.57.0 (abc)".to_owned(),
        ..standard()
    })
    .await;
    let err = Navidrome::new(client(&server, PASSWORD))
        .probe()
        .await
        .unwrap_err();
    assert!(
        matches!(err, AdapterError::VersionUnsupported { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn listings_without_proof_are_incomplete() {
    for (name, state) in [
        (
            "no total",
            State {
                no_total: true,
                ..standard()
            },
        ),
        (
            "total changed while paging",
            State {
                growing_total: true,
                ..standard()
            },
        ),
        (
            "filter ignored",
            State {
                ignore_filter: true,
                albums: {
                    let mut albums: Vec<Value> =
                        serde_json::from_str(&fixture("albums_library_1.json")).unwrap();
                    albums[1]["libraryId"] = Value::from(2);
                    albums
                },
                ..standard()
            },
        ),
    ] {
        let (server, _) = fake(state).await;
        let listing = Navidrome::new(client(&server, PASSWORD))
            .items("1")
            .await
            .unwrap();
        assert!(!listing.complete, "{name}");
        assert!(
            listing.items.iter().all(|a| a.title != "Kid A") || name != "filter ignored",
            "an album of another library is never filed under this one"
        );
    }
    let (server, _) = fake(standard()).await;
    let err = Navidrome::new(client(&server, PASSWORD))
        .items("music")
        .await
        .unwrap_err();
    assert!(matches!(err, AdapterError::Rejected(_)), "{err:?}");
}
