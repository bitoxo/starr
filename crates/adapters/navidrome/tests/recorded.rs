//! Navidrome against its own answers, recorded read-only from the
//! maintainer's Navidrome 0.64.2 on 2026-11-06 and sanitised
//! (`fixtures/0.64.2/recorded/`, README there; status and the headers that
//! matter in `responses.json`). A local replay, never a real Navidrome: it
//! answers the native API only with the token of the login, and every
//! answer renews it as recorded (`X-ND-Authorization`).
#![allow(clippy::unwrap_used)]

use secrecy::SecretString;
use serde_json::Value;
use starr_adapter_kit::{Endpoint, LoginCredential, ServiceClient};
use starr_adapter_navidrome::{AUTH, DESCRIPTOR, Navidrome};
use starr_core::accounts::{Accounts, LibraryGrant};
use starr_core::adapters::{Probe, Version};
use starr_core::library::{Library, LibraryContent, UnitCounts};
use url::Url;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const RECORDED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/0.64.2/recorded");
const TOKEN: &str = "replay-token";

fn recorded(name: &str) -> String {
    std::fs::read_to_string(format!("{RECORDED}/{name}.json")).unwrap()
}

fn recorded_json(name: &str) -> Value {
    serde_json::from_str(&recorded(name)).unwrap()
}

/// The recorded status and `X-Total-Count` of an answer.
fn response(name: &str) -> (u16, Option<String>) {
    let all = recorded_json("responses");
    let r = &all[name];
    (
        u16::try_from(r["status"].as_u64().unwrap()).unwrap(),
        r["headers"]["X-Total-Count"].as_str().map(str::to_owned),
    )
}

struct Replay;

impl Replay {
    fn answer(name: &str) -> ResponseTemplate {
        let (status, total) = response(name);
        let renews = recorded_json("responses")[name]["renewsTokenHeader"] == Value::Bool(true);
        let mut template = ResponseTemplate::new(status)
            .insert_header("content-type", "application/json")
            .set_body_string(recorded(name));
        if let Some(total) = total {
            template = template.insert_header("x-total-count", total.as_str());
        }
        if renews {
            template = template.insert_header("x-nd-authorization", TOKEN);
        }
        template
    }
}

impl Respond for Replay {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let path = request.url.path();
        let query: std::collections::HashMap<String, String> =
            request.url.query_pairs().into_owned().collect();
        match (request.method.as_str(), path) {
            ("GET", "/rest/ping") => return Self::answer("rest_ping_no_creds"),
            ("POST", "/auth/login") => {
                // The recorded reply with a usable token in place of the
                // redacted one.
                let mut reply = recorded_json("auth_login");
                reply["token"] = Value::String(TOKEN.to_owned());
                return ResponseTemplate::new(200).set_body_json(reply);
            }
            _ => {}
        }
        let bearer = format!("Bearer {TOKEN}");
        if request
            .headers
            .get("x-nd-authorization")
            .and_then(|v| v.to_str().ok())
            != Some(bearer.as_str())
        {
            return ResponseTemplate::new(401);
        }
        match (request.method.as_str(), path) {
            ("GET", "/api/library") => Self::answer("library_all"),
            ("GET", "/api/user") => Self::answer("user_all"),
            ("GET", "/api/album") => match (
                query.get("library_id").map(String::as_str),
                query.get("_start").map(String::as_str),
            ) {
                (Some("999"), _) => Self::answer("album_page_lib999"),
                // Only the first page was recorded.
                (Some("1"), Some("0")) => Self::answer("album_page_lib1"),
                _ => ResponseTemplate::new(200)
                    .insert_header(
                        "x-total-count",
                        response("album_page_lib1").1.unwrap().as_str(),
                    )
                    .set_body_string("[]"),
            },
            ("GET", p) if p.starts_with("/api/user/") && p.ends_with("/library") => {
                Self::answer("user_one_member_library")
            }
            ("GET", p) if p.starts_with("/api/user/") => Self::answer("user_one_member"),
            _ => ResponseTemplate::new(404),
        }
    }
}

async fn replay() -> (MockServer, Navidrome) {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(Replay)
        .mount(&server)
        .await;
    let client = ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(&server.uri()).unwrap(),
            auth: AUTH,
            credential: LoginCredential::encode("admin1", &SecretString::from("x".to_owned())),
        },
    )
    .unwrap();
    (server, Navidrome::new(client))
}

#[tokio::test]
async fn the_recorded_navidrome_probes_and_lists_its_library() {
    let (server, navidrome) = replay().await;
    assert_eq!(
        navidrome.probe().await.unwrap().version,
        Version::new(0, 64, 2, 0)
    );
    let libraries = navidrome.libraries().await.unwrap();
    assert!(libraries.complete);
    let got: Vec<_> = libraries
        .items
        .iter()
        .map(|l| (l.id.as_str(), l.name.as_str(), l.content))
        .collect();
    assert_eq!(got, [("1", "Music Library", LibraryContent::Music)]);
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|r| r.method.as_str() == "GET" || r.url.path() == "/auth/login")
    );
}

/// Recorded: no album of 0.64.2 carries an `mbz*` key (the maintainer's
/// files have no `MusicBrainz` tags), so every album is listed without
/// MBIDs, never matched by name. The recorded first page says 5517 albums;
/// the replay has no second page, so the listing is not complete.
#[tokio::test]
async fn recorded_albums_without_mbids_are_listed_without_provider_ids() {
    let (_server, navidrome) = replay().await;
    let page = recorded_json("album_page_lib1");
    for album in page.as_array().unwrap() {
        let keys: Vec<&String> = album.as_object().unwrap().keys().collect();
        assert!(keys.iter().all(|k| !k.starts_with("mbz")), "{keys:?}");
    }
    assert_eq!(page, recorded_json("album_page_nofilter"), "one library");
    let albums = navidrome.items("1").await.unwrap();
    assert!(!albums.complete);
    assert_eq!(albums.items.len(), 5);
    for album in &albums.items {
        assert_eq!(album.provider_ids.mb_release_group, None);
        assert_eq!(album.provider_ids.mb_artist, None);
        assert!(album.year.is_some());
        assert!(matches!(album.units, UnitCounts::Tracks(n) if n > 0));
    }
    assert_eq!(albums.items[0].title, "Album 1");

    // An unknown library: `[]` with `X-Total-Count: 0`, complete and empty.
    let none = navidrome.items("999").await.unwrap();
    assert!(none.complete);
    assert_eq!(none.items, Vec::new());
}

#[tokio::test]
async fn recorded_users_read_as_accounts() {
    let (_server, navidrome) = replay().await;
    let listing = navidrome.accounts().await.unwrap();
    assert!(listing.complete);
    let got: Vec<_> = listing
        .items
        .iter()
        .map(|a| (a.username.as_str(), a.privileged, a.libraries.clone()))
        .collect();
    let member = |name| (name, false, LibraryGrant::Only(vec!["1".to_owned()]));
    assert_eq!(
        got,
        [
            member("member1"),
            member("member2"),
            member("member3"),
            member("member4"),
            ("admin1", true, LibraryGrant::All),
        ]
    );
    let one = navidrome.account("recUser000000000000002").await.unwrap();
    assert_eq!(one, listing.items[0]);
}
