//! A stateful fake of the Whisparr v3 API surface Starr uses, for the scene
//! request tests (spec 0007 slice 4). Responses follow the adapter's
//! fixtures (`crates/adapters/whisparr/fixtures/3.1.0.2116`, derived from
//! Whisparr's source, placeholders only). It records every write so tests
//! can say exactly what Starr sent; a write Starr must never send
//! (performers, studios, deletes, anything else) lands in `forbidden`.
//! Nothing real is contacted.
#![allow(dead_code, clippy::needless_pass_by_value)]

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};
use wiremock::matchers::{any, header};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::media::put;
use super::services::{API_KEY, Env};
use super::{TestResponse, post};

/// A second scene of the `StashDB` fixtures (in Performer P's and Studio
/// S's scene lists).
pub const SCENE_B: &str = "5ce0e000-0000-4000-8000-00000000000b";

fn read(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/whisparr/fixtures/3.1.0.2116/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn fixture(name: &str) -> Value {
    serde_json::from_str(&read(name)).unwrap()
}

#[derive(Debug, Default)]
pub struct WhisparrState {
    /// The library: scene resources as Whisparr lists them.
    pub scenes: Vec<Value>,
    /// Scenes Whisparr's own metadata knows (`StashDB` UUID, title).
    pub metadata: Vec<(String, String)>,
    /// Queue records per Whisparr scene id.
    pub queue: Vec<(i64, Value)>,
    /// Every write: method, path and body.
    pub writes: Vec<(String, String, Value)>,
    /// Writes Starr must never send (method and path).
    pub forbidden: Vec<String>,
    /// Answer adds with 500, echoing a title and a studio (as a failing
    /// Whisparr might).
    pub fail_adds: bool,
    /// Answer library reads with 404 (a wrong address behind a proxy).
    pub lost: bool,
    next_id: i64,
}

impl WhisparrState {
    /// The library entry of scene `uuid`.
    pub fn scene(&mut self, uuid: &str) -> &mut Value {
        self.scenes
            .iter_mut()
            .find(|s| s["foreignId"] == uuid)
            .unwrap()
    }

    /// Scene `uuid` is in Whisparr already, monitored or not, with or
    /// without its file (added by someone else).
    pub fn has_scene(&mut self, uuid: &str, title: &str, monitored: bool, has_file: bool) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        self.scenes.push(resource(
            id,
            uuid,
            title,
            monitored,
            1,
            "/data/private",
            has_file,
        ));
        id
    }

    pub fn writes_to(&self, method: &str, path_prefix: &str) -> Vec<Value> {
        self.writes
            .iter()
            .filter(|(m, p, _)| m == method && p.starts_with(path_prefix))
            .map(|(_, _, b)| b.clone())
            .collect()
    }
}

fn resource(
    id: i64,
    uuid: &str,
    title: &str,
    monitored: bool,
    profile: i64,
    root: &str,
    has_file: bool,
) -> Value {
    json!({
        "id": id,
        "title": title,
        "foreignId": uuid,
        "stashId": uuid,
        "itemType": "scene",
        "monitored": monitored,
        "qualityProfileId": profile,
        "rootFolderPath": root,
        "path": format!("{root}/Studio S/{title}"),
        "hasFile": has_file,
        "studioTitle": "Studio S",
        "tags": [],
    })
}

#[derive(Clone)]
pub struct FakeWhisparr(pub Arc<Mutex<WhisparrState>>);

impl FakeWhisparr {
    pub fn state(&self) -> MutexGuard<'_, WhisparrState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn start(&self) -> MockServer {
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

fn ok(status: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(status)
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

impl Respond for FakeWhisparr {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let method = request.method.to_string();
        let path = request.url.path().to_owned();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let mut state = self.state();
        match (method.as_str(), path.as_str()) {
            ("GET", "/api/v3/system/status") => ok(200, fixture("system_status.json")),
            ("GET", "/api/v3/qualityprofile") => ok(200, fixture("qualityprofile.json")),
            ("GET", "/api/v3/rootfolder") => ok(200, fixture("rootfolder.json")),
            ("GET", "/api/v3/movie") if state.lost => ResponseTemplate::new(404),
            ("GET", "/api/v3/movie") => {
                let wanted = query(request, "stashId").unwrap_or_default();
                let found: Vec<Value> = state
                    .scenes
                    .iter()
                    .filter(|s| s["foreignId"] == wanted.as_str())
                    .cloned()
                    .collect();
                ok(200, Value::Array(found))
            }
            ("GET", "/api/v3/lookup/scene") => {
                let term = query(request, "term").unwrap_or_default();
                let uuid = term.strip_prefix("stash:").unwrap_or_default();
                let found: Vec<Value> = state
                    .metadata
                    .iter()
                    .filter(|(id, _)| id == uuid)
                    .map(|(id, title)| {
                        json!({ "foreignId": id, "movie": { "foreignId": id, "itemType": "scene", "title": title } })
                    })
                    .collect();
                ok(200, Value::Array(found))
            }
            ("GET", "/api/v3/queue/details") => {
                let id: i64 = query(request, "movieId")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(-1);
                let records: Vec<Value> = state
                    .queue
                    .iter()
                    .filter(|(scene, _)| *scene == id)
                    .map(|(_, r)| r.clone())
                    .collect();
                ok(200, Value::Array(records))
            }
            ("POST", "/api/v3/movie") => {
                state.writes.push((method, path, body.clone()));
                if state.fail_adds {
                    return ResponseTemplate::new(500)
                        .set_body_string("Scene A of Studio S failed");
                }
                let uuid = body["foreignId"].as_str().unwrap_or_default().to_owned();
                if state.scenes.iter().any(|s| s["foreignId"] == uuid.as_str()) {
                    return ok(400, fixture("movie_add_duplicate_400.json"));
                }
                state.next_id += 1;
                let created = resource(
                    state.next_id,
                    &uuid,
                    body["title"].as_str().unwrap_or_default(),
                    body["monitored"].as_bool().unwrap_or(false),
                    body["qualityProfileId"].as_i64().unwrap_or(0),
                    body["rootFolderPath"].as_str().unwrap_or_default(),
                    false,
                );
                state.scenes.push(created.clone());
                ok(201, created)
            }
            ("PUT", p) if p.starts_with("/api/v3/movie/") => {
                state.writes.push((method, path.clone(), body.clone()));
                let id: i64 = p
                    .rsplit('/')
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(-1);
                let Some(scene) = state.scenes.iter_mut().find(|s| s["id"] == id) else {
                    return ResponseTemplate::new(404);
                };
                scene["monitored"] = body["monitored"].clone();
                ok(202, scene.clone())
            }
            ("POST", "/api/v3/command") => {
                state.writes.push((method, path, body));
                ok(201, json!({ "id": 1, "status": "queued" }))
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

/// A fake Whisparr with an empty library whose metadata knows `metadata`
/// (`StashDB` UUID, title).
pub async fn fake_whisparr(metadata: &[(&str, &str)]) -> (FakeWhisparr, MockServer) {
    let fake = FakeWhisparr(Arc::new(Mutex::new(WhisparrState {
        metadata: metadata
            .iter()
            .map(|(id, title)| ((*id).to_owned(), (*title).to_owned()))
            .collect(),
        next_id: 10,
        ..WhisparrState::default()
    })));
    let server = fake.start().await;
    (fake, server)
}

/// Connect `server` as the adult source with quality profile 3 and
/// `/data/private`; returns the service id.
pub async fn connect_whisparr(env: &Env, server: &MockServer) -> i64 {
    let res: TestResponse = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "whisparr", "name": "Source W", "baseUrl": server.uri(), "apiKey": API_KEY }),
        ))
        .await;
    assert_eq!(res.status, axum::http::StatusCode::ACCEPTED, "{}", res.raw);
    let id = res.body["service"]["id"].as_i64().unwrap();
    env.drain().await;
    let res = env
        .send(put(
            "/api/v1/categories/adult/source",
            json!({ "serviceId": id }),
        ))
        .await;
    assert_eq!(res.status, axum::http::StatusCode::OK, "{}", res.raw);
    env.send(post(
        &format!("/api/v1/services/{id}/source-lists/refresh"),
        json!({}),
    ))
    .await;
    env.drain().await;
    let res = env
        .send(put(
            "/api/v1/categories/adult/source/settings",
            json!({ "qualityProfileId": 3, "rootFolderPath": "/data/private" }),
        ))
        .await;
    assert_eq!(res.status, axum::http::StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["requestsReady"], true, "{}", res.raw);
    id
}

/// The `StashDB` fake answers `findScene` for `uuid` with Scene A's
/// fixture renamed (placeholders only).
pub async fn mount_scene(stashdb: &MockServer, uuid: &str, title: &str) {
    let body = super::stashdb::fixture("find_scene.json")
        .replace(super::stashdb::SCENE_A, uuid)
        .replace("Scene A", title);
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::body_string_contains("query FindScene"))
        .and(wiremock::matchers::body_string_contains(uuid))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .with_priority(1)
        .mount(stashdb)
        .await;
}
