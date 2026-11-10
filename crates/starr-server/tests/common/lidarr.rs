//! A stateful fake of the Lidarr 3.1.6 API surface Starr uses, built from
//! the recorded responses (`crates/adapters/lidarr/fixtures/3.1.6.5079`).
//! It records every write, so tests can say exactly what Starr sent; a
//! write Starr must never send (artist updates, deletes, album edits)
//! lands in `forbidden`. Nothing real is contacted.
#![allow(clippy::needless_pass_by_value)]

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};
use wiremock::matchers::{any, header};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::services::API_KEY;

pub const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
pub const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";
/// In the `MusicBrainz` fixture discography; Lidarr's metadata for it is
/// OK Computer's with this id and title.
pub const MOON_SHAPED_POOL: &str = "c4d5e6f7-0a1b-4c2d-8e3f-4a5b6c7d8e9f";
/// The id the fake gives Radiohead when Starr creates the artist.
pub const ARTIST_ID: i64 = 7;

fn read(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/lidarr/fixtures/3.1.6.5079/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn recorded(name: &str) -> Value {
    serde_json::from_str(&read(&format!("recorded/{name}"))).unwrap()
}

/// Lidarr's metadata (`GET /album/lookup`) for release group `id`.
fn lookup(id: &str) -> Option<Value> {
    let mut found: Value = serde_json::from_str(&read("album_lookup_ok_computer.json")).unwrap();
    match id {
        OK_COMPUTER => {}
        MOON_SHAPED_POOL => {
            found[0]["foreignAlbumId"] = json!(MOON_SHAPED_POOL);
            found[0]["title"] = json!("A Moon Shaped Pool");
        }
        _ => return None,
    }
    Some(found)
}

#[derive(Debug, Default)]
pub struct LidarrState {
    pub artist: Option<Value>,
    pub albums: Vec<Value>,
    pub queue: Vec<Value>,
    /// Every write: method, path and body.
    pub writes: Vec<(String, String, Value)>,
    /// Writes Starr must never send (method and path).
    pub forbidden: Vec<String>,
    next_album_id: i64,
}

impl LidarrState {
    /// The album with release group `id`.
    pub fn album(&mut self, id: &str) -> &mut Value {
        self.albums
            .iter_mut()
            .find(|a| a["foreignAlbumId"] == id)
            .unwrap()
    }

    /// Lidarr has every track of album `id` on disk (`tracks` of them).
    pub fn complete(&mut self, id: &str, tracks: u32) {
        self.album(id)["statistics"] =
            json!({ "trackFileCount": tracks, "trackCount": tracks, "totalTrackCount": tracks });
    }

    /// Radiohead is in Lidarr (monitored, Starr's profiles aside) with
    /// album `id`, monitored or not, and `files` of 12 tracks on disk.
    pub fn radiohead_with(&mut self, id: &str, monitored: bool, files: u32) {
        let mut artist = lookup(OK_COMPUTER).unwrap()[0]["artist"].clone();
        artist["id"] = json!(ARTIST_ID);
        artist["monitored"] = json!(true);
        artist["monitorNewItems"] = json!("all");
        artist["qualityProfileId"] = json!(1);
        artist["metadataProfileId"] = json!(1);
        artist["path"] = json!("/music/Radiohead");
        artist["rootFolderPath"] = json!("/music");
        self.artist = Some(artist);
        self.next_album_id += 1;
        let tracks = if monitored || files > 0 { 12 } else { 0 };
        self.albums.push(json!({
            "id": self.next_album_id, "artistId": ARTIST_ID, "foreignAlbumId": id,
            "title": "Album", "monitored": monitored,
            "statistics": { "trackFileCount": files, "trackCount": tracks, "totalTrackCount": 12 }
        }));
    }

    pub fn writes_to(&self, method: &str, path: &str) -> Vec<Value> {
        self.writes
            .iter()
            .filter(|(m, p, _)| m == method && p == path)
            .map(|(_, _, b)| b.clone())
            .collect()
    }
}

#[derive(Clone)]
pub struct FakeLidarr(pub Arc<Mutex<LidarrState>>);

impl FakeLidarr {
    pub fn state(&self) -> MutexGuard<'_, LidarrState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub async fn start(&self) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(header("X-Api-Key", API_KEY))
            .respond_with(self.clone())
            .mount(&server)
            .await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        server
    }
}

fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "application/json")
        .set_body_string(body.to_string())
}

fn query(request: &Request, name: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

impl Respond for FakeLidarr {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let method = request.method.to_string();
        let path = request.url.path().to_owned();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let mut state = self.state();
        match (method.as_str(), path.as_str()) {
            ("GET", "/api/v1/system/status") => ok(recorded("system_status.json")),
            ("GET", "/api/v1/qualityprofile") => ok(recorded("qualityprofile.json")),
            ("GET", "/api/v1/metadataprofile") => ok(recorded("metadataprofile.json")),
            ("GET", "/api/v1/rootfolder") => ok(recorded("rootfolder.json")),
            ("GET", "/api/v1/artist") => {
                let wanted = query(request, "mbId");
                match &state.artist {
                    Some(a) if wanted.as_deref() == a["foreignArtistId"].as_str() => ok(json!([a])),
                    _ => ok(json!([])),
                }
            }
            ("GET", "/api/v1/album") => {
                let artist = query(request, "artistId");
                if artist == Some(ARTIST_ID.to_string()) && state.artist.is_some() {
                    ok(Value::Array(state.albums.clone()))
                } else {
                    ok(json!([]))
                }
            }
            ("GET", "/api/v1/album/lookup") => {
                let term = query(request, "term").unwrap_or_default();
                let id = term.strip_prefix("lidarr:").unwrap_or_default();
                lookup(id).map_or_else(|| ok(json!([])), ok)
            }
            ("GET", "/api/v1/queue/details") => ok(Value::Array(state.queue.clone())),
            ("POST", "/api/v1/album") => {
                state.writes.push((method, path, body.clone()));
                if state.artist.is_none() {
                    let mut artist = body["artist"].clone();
                    artist["id"] = json!(ARTIST_ID);
                    artist["path"] = json!("/music/Radiohead");
                    if let Some(a) = artist.as_object_mut() {
                        a.remove("addOptions");
                    }
                    state.artist = Some(artist);
                }
                state.next_album_id += 1;
                let mut album = body;
                album["id"] = json!(state.next_album_id);
                album["artistId"] = json!(ARTIST_ID);
                album["statistics"] =
                    json!({ "trackFileCount": 0, "trackCount": 12, "totalTrackCount": 12 });
                if let Some(a) = album.as_object_mut() {
                    a.remove("artist");
                    a.remove("addOptions");
                }
                state.albums.push(album.clone());
                ResponseTemplate::new(201).set_body_json(album)
            }
            ("PUT", "/api/v1/album/monitor") => {
                state.writes.push((method, path, body.clone()));
                let monitored = body["monitored"].as_bool().unwrap_or(false);
                for id in body["albumIds"].as_array().into_iter().flatten() {
                    if let Some(album) = state.albums.iter_mut().find(|a| a["id"] == *id) {
                        album["monitored"] = json!(monitored);
                        if monitored && album["statistics"]["trackCount"] == 0 {
                            album["statistics"]["trackCount"] = json!(12);
                        }
                    }
                }
                ResponseTemplate::new(202).set_body_json(json!([]))
            }
            ("POST", "/api/v1/command") => {
                state.writes.push((method, path, body));
                ResponseTemplate::new(201).set_body_json(json!({ "id": 1, "status": "queued" }))
            }
            _ => {
                if method != "GET" {
                    state.forbidden.push(format!("{method} {path}"));
                }
                ResponseTemplate::new(404)
            }
        }
    }
}

/// A fake Lidarr without any artist.
pub async fn fake_lidarr() -> (FakeLidarr, MockServer) {
    let fake = FakeLidarr(Arc::new(Mutex::new(LidarrState {
        next_album_id: 100,
        ..LidarrState::default()
    })));
    let server = fake.start().await;
    (fake, server)
}
