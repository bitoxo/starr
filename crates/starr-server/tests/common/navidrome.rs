//! A stateful fake of the Navidrome surface Starr reads (spec 0006 slice
//! 5), shaped like the adapter's hand-written fixtures
//! (`crates/adapters/navidrome/fixtures/0.64.2/derived`, from Navidrome's
//! source, not recorded). It knows one admin, answers the native API only
//! with the current token and records every request, so tests can say that
//! Starr only reads. Nothing real is contacted.
#![allow(clippy::needless_pass_by_value)]

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

pub const ND_USER: &str = "starr";
pub const ND_PASSWORD: &str = "navidrome-admin-pass";

fn read(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/navidrome/fixtures/0.64.2/derived/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[derive(Debug, Default)]
pub struct NavidromeState {
    /// Albums of library 1 as `GET /api/album` lists them.
    pub albums: Vec<Value>,
    pub token: Option<String>,
    pub logins: usize,
    /// Every request: method and path.
    pub requests: Vec<String>,
}

impl NavidromeState {
    /// Navidrome scanned album `release_group` with `songs` tracks, tagged
    /// with its `MusicBrainz` release group (`None`: untagged files).
    pub fn scan(&mut self, id: &str, name: &str, release_group: Option<&str>, songs: u32) {
        self.albums.retain(|a| a["id"] != id);
        let mut album = json!({
            "id": id, "libraryId": 1, "name": name, "albumArtist": "Radiohead",
            "maxYear": 1997, "songCount": songs, "missing": false,
            "mbzAlbumArtistId": "a74b1b7f-71a5-4011-9441-d0b5e4122711"
        });
        if let Some(rg) = release_group {
            album["mbzReleaseGroupId"] = json!(rg);
        }
        self.albums.push(album);
    }

    /// The album's files are gone (Navidrome flags it missing).
    pub fn lose(&mut self, id: &str) {
        for album in &mut self.albums {
            if album["id"] == id {
                album["missing"] = json!(true);
            }
        }
    }
}

#[derive(Clone)]
pub struct FakeNavidrome(pub Arc<Mutex<NavidromeState>>);

impl FakeNavidrome {
    pub fn state(&self) -> MutexGuard<'_, NavidromeState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn ok(body: Value, total: Option<usize>) -> ResponseTemplate {
    let mut r = ResponseTemplate::new(200).set_body_json(body);
    if let Some(total) = total {
        r = r.insert_header("x-total-count", total.to_string().as_str());
    }
    r
}

impl Respond for FakeNavidrome {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.state();
        let path = request.url.path().to_owned();
        state.requests.push(format!("{} {path}", request.method));
        match (request.method.as_str(), path.as_str()) {
            ("GET", "/rest/ping") => {
                return ResponseTemplate::new(200).set_body_string(read("ping_without_login.json"));
            }
            ("POST", "/auth/login") => {
                let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
                state.logins += 1;
                if body["username"] != ND_USER || body["password"] != ND_PASSWORD {
                    return ResponseTemplate::new(401);
                }
                let token = format!("nd-{}", state.logins);
                state.token = Some(token.clone());
                let mut login: Value = serde_json::from_str(&read("login.json")).unwrap();
                login["token"] = json!(token);
                return ok(login, None);
            }
            _ => {}
        }
        let sent = request
            .headers
            .get("x-nd-authorization")
            .and_then(|v| v.to_str().ok());
        if state.token.is_none()
            || sent
                != state
                    .token
                    .as_ref()
                    .map(|t| format!("Bearer {t}"))
                    .as_deref()
        {
            return ResponseTemplate::new(401);
        }
        match path.as_str() {
            "/api/library" => {
                let libraries: Value = serde_json::from_str(&read("library.json")).unwrap();
                let libraries = Value::Array(libraries.as_array().unwrap()[..1].to_vec());
                ok(libraries, Some(1))
            }
            "/api/album" => {
                let library = request
                    .url
                    .query_pairs()
                    .find(|(k, _)| k == "library_id")
                    .map(|(_, v)| v.into_owned());
                let albums = if library.as_deref() == Some("1") {
                    state.albums.clone()
                } else {
                    Vec::new()
                };
                let total = albums.len();
                ok(Value::Array(albums), Some(total))
            }
            _ => ResponseTemplate::new(404),
        }
    }
}

/// A Navidrome with one library and no albums yet.
pub async fn fake_navidrome() -> (FakeNavidrome, MockServer) {
    let fake = FakeNavidrome(Arc::new(Mutex::new(NavidromeState::default())));
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(fake.clone())
        .mount(&server)
        .await;
    (fake, server)
}
