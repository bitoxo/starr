//! The stateful Whisparr v3 fake shared by the contract and privacy tests
//! (see `contract.rs` for what it models). The library, profiles, root
//! folders and status are **recorded** from Whisparr 3.6.5.1886
//! (`fixtures/3.6.5.1886/recorded/`, sanitised); what a read-only recording
//! cannot show (a scene not in the library, add answers, a queue with
//! entries) is derived from Whisparr's source (`fixtures/3.1.0.2116/`).
//! Placeholders only.
#![allow(
    dead_code,
    unused_imports,
    clippy::unwrap_used,
    clippy::struct_excessive_bools
)]

pub use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
pub use std::time::Duration;

pub use secrecy::SecretString;
pub use serde_json::{Map, Value, json};
pub use starr_adapter_kit::contract::{self, ProbeContract, RecordedResponse, run_probe_contract};
pub use starr_adapter_kit::{Endpoint, ServiceClient};
pub use starr_adapter_whisparr::{AUTH, DESCRIPTOR, Whisparr, client_options};
pub use starr_core::adapters::{AdapterError, Listing, Probe};
pub use starr_core::source::{
    AddOutcome, AddRequest, Deviation, Files, Lookup, MonitorScope, NaturalKey, QualityProfile,
    QueueEntry, QueueState, RootFolder, Source,
};
pub use tracing::Level;
pub use url::Url;
pub use uuid::Uuid;
pub use wiremock::matchers::{header, method, path, path_regex, query_param};
pub use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// The recorded Whisparr (inside the tested range).
pub const SUPPORTED: &str = "3.6.5.1886";
/// The tag the derived fixtures were read from.
pub const DERIVED: &str = "3.1.0.2116";
pub const UNSUPPORTED: &str = "2.2.0.231";
pub const KEY: &str = "contract-test-key-0123456789abcdef";

/// In the fake's library (id 29, unmonitored, no file): the recorded
/// `movie_by_stashid_unmonitored.json`.
pub const SCENE_A: Uuid = Uuid::from_u128(0x5ce0_0001_0000_4000_8000_0000_0000_0001);
pub const SCENE_A_ID: i64 = 29;
/// The recorded quality profile of Scene A ("1080p"); the other is 5 ("4K").
pub const PROFILE: i64 = 4;
/// Not in the library; Whisparr's metadata knows it (`lookup_scene_b.json`).
pub const SCENE_B: Uuid = Uuid::from_u128(0x5ce0_b000_0000_4000_8000_0000_0000_000b);
/// The id the fake gives Scene B on add.
pub const SCENE_B_ID: i64 = 12;
/// Neither in the library nor in Whisparr's metadata.
pub const SCENE_C: Uuid = Uuid::from_u128(0x5ce0_c000_0000_4000_8000_0000_0000_000c);
/// What the fake stores instead of the requested profile when mangling.
pub const MANGLED_PROFILE: i64 = 999;
/// Names that must never reach logs or errors (fixture placeholders).
pub const PRIVATE_MARKERS: [&str; 10] = [
    "5ce00001-0000-4000-8000-000000000001",
    "5ce0b000-0000-4000-8000-00000000000b",
    "9e7f0001-0000-4000-8000-000000000001",
    "9e7f0002-0000-4000-8000-000000000002",
    "57d10001-0000-4000-8000-000000000001",
    "9e7f0000-0000-4000-8000-0000000000aa",
    "57d10000-0000-4000-8000-0000000000bb",
    "Scene A",
    "Scene B",
    "Studio S",
];

pub fn read(relative: &str) -> String {
    let path = format!("{}/fixtures/{relative}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

pub fn derived(file: &str) -> String {
    read(&format!("{DERIVED}/{file}"))
}

/// A response recorded from Whisparr 3.6.5.1886 (sanitised).
pub fn recorded(file: &str) -> String {
    read(&format!("{SUPPORTED}/recorded/{file}"))
}

pub fn recorded_json(file: &str) -> Value {
    serde_json::from_str(&recorded(file)).unwrap()
}

pub fn derived_json(file: &str) -> Value {
    serde_json::from_str(&derived(file)).unwrap()
}

pub fn status_body(version: &str) -> String {
    if version == SUPPORTED {
        recorded("system_status.json")
    } else {
        read(&format!("{version}/system_status.json"))
    }
}

pub fn scene_a() -> Value {
    recorded_json("movie_by_stashid_unmonitored.json")[0].clone()
}

pub fn add_request(key: Uuid) -> AddRequest {
    AddRequest {
        key: NaturalKey::StashScene(key),
        quality_profile_id: PROFILE,
        root_folder_path: "/data/private".into(),
        scope: MonitorScope::Item,
        search: true,
        minimum_availability: None,
        metadata_profile_id: None,
    }
}

// --- the fake ---------------------------------------------------------------

#[derive(Debug, Default)]
pub struct State {
    pub scenes: Vec<Value>,
    /// `GET /movie?stashId=` pretends the scene is absent although it
    /// exists (someone else added it between our lookup and our add).
    pub hidden: bool,
    /// Store adds with a wrong quality profile.
    pub mangle: bool,
    /// Store adds unmonitored, as Whisparr does for a scene of an excluded
    /// studio when an exclusion tag is configured.
    pub store_unmonitored: bool,
    /// Reject every add as an excluded studio without a tag (nothing
    /// stored).
    pub reject_adds: bool,
    /// Answer this many commands with 503 before accepting them.
    pub failing_commands: u32,
    /// Answer `PUT /movie/{id}` with 500 (nothing stored).
    pub failing_puts: bool,
    /// `GET /movie?stashId=` ignores the filter and lists the whole
    /// library (a future 3.x build that drops the parameter).
    pub ignore_stash_filter: bool,
    /// Answer an add with this id instead of the stored one.
    pub reply_id: Option<i64>,
    pub posts: Vec<Value>,
    pub puts: Vec<Value>,
    pub commands: Vec<Value>,
    /// Writes Starr must never send: method and path.
    pub forbidden: Vec<String>,
}

impl State {
    pub fn writes(&self) -> usize {
        self.posts.len() + self.puts.len() + self.commands.len() + self.forbidden.len()
    }

    fn scene_by_foreign_id(&self, foreign_id: &str) -> Option<&Value> {
        self.scenes
            .iter()
            .find(|s| s["foreignId"].as_str() == Some(foreign_id))
    }

    fn scene_mut(&mut self, id: i64) -> Option<&mut Map<String, Value>> {
        self.scenes
            .iter_mut()
            .filter_map(Value::as_object_mut)
            .find(|s| s.get("id").and_then(Value::as_i64) == Some(id))
    }
}

pub type Shared = Arc<Mutex<State>>;

pub fn lock(state: &Shared) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn json_response(status: u16, body: &Value) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .insert_header("content-type", "application/json")
        .set_body_string(body.to_string())
}

pub fn query(request: &Request, name: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

/// `GET /movie?stashId=`: `FindByForeignId`, at most one scene.
pub struct ByStashId(Shared);

impl Respond for ByStashId {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let state = lock(&self.0);
        let wanted = query(request, "stashId").unwrap_or_default();
        if state.ignore_stash_filter {
            return json_response(200, &Value::Array(state.scenes.clone()));
        }
        match state.scene_by_foreign_id(&wanted) {
            Some(scene) if !state.hidden => json_response(200, &json!([scene])),
            _ => json_response(200, &json!([])),
        }
    }
}

/// `GET /lookup/scene?term=`: Whisparr's metadata knows Scene B only.
pub struct LookupScene;

impl Respond for LookupScene {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let term = query(request, "term").unwrap_or_default();
        if term == format!("stash:{SCENE_B}") {
            json_response(200, &derived_json("lookup_scene_b.json"))
        } else {
            json_response(200, &recorded_json("lookup_scene_unknown.json"))
        }
    }
}

/// `POST /movie`: `MovieController` validators, then what `AddMovieService`
/// stores (metadata from its own lookup; path, profile, monitoring, root
/// folder, tags and add options from the body).
pub struct Add(Shared);

impl Respond for Add {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(&request.body) else {
            return json_response(400, &json!([{ "errorMessage": "not a JSON object" }]));
        };
        let mut state = lock(&self.0);
        state.posts.push(Value::Object(body.clone()));
        if state.reject_adds {
            return json_response(400, &derived_json("movie_add_excluded_400.json"));
        }
        let text = |f: &str| body.get(f).and_then(Value::as_str).unwrap_or("").trim();
        let mut failures = Vec::new();
        // `PostValidator`: a title when there is no TMDB ID; a foreign ID;
        // a root folder that exists; a quality profile that exists.
        if text("title").is_empty() {
            failures.push("'Title' must not be empty.");
        }
        if text("foreignId").is_empty() {
            failures.push("'Foreign Id' must not be empty.");
        }
        // The recorded root folder and quality profiles.
        if text("rootFolderPath") != "/data/private" {
            failures.push("Root folder path does not exist");
        }
        if ![Some(4), Some(5)].contains(&body.get("qualityProfileId").and_then(Value::as_i64)) {
            failures.push("Quality Profile does not exist");
        }
        if !failures.is_empty() {
            let errors: Vec<Value> = failures
                .iter()
                .map(|m| json!({ "errorMessage": m }))
                .collect();
            return json_response(400, &Value::Array(errors));
        }
        let foreign_id = text("foreignId").to_owned();
        if state.scene_by_foreign_id(&foreign_id).is_some() {
            state.hidden = false;
            return json_response(400, &derived_json("movie_add_duplicate_400.json"));
        }
        // Whisparr's metadata for the scene (here: only Scene B exists).
        if foreign_id != SCENE_B.to_string() {
            return json_response(
                400,
                &json!([{ "propertyName": "StashDB", "errorMessage": "A movie with this ID was not found." }]),
            );
        }
        let mut stored = derived_json("lookup_scene_b.json")[0]["movie"].clone();
        let object = stored.as_object_mut().unwrap();
        let monitored = body.get("monitored").cloned().unwrap_or(json!(true));
        object.insert("id".into(), json!(SCENE_B_ID));
        object.insert(
            "monitored".into(),
            if state.store_unmonitored {
                json!(false)
            } else {
                monitored
            },
        );
        object.insert(
            "qualityProfileId".into(),
            if state.mangle {
                json!(MANGLED_PROFILE)
            } else {
                body["qualityProfileId"].clone()
            },
        );
        object.insert("rootFolderPath".into(), body["rootFolderPath"].clone());
        object.insert(
            "path".into(),
            json!(format!("{}/Studio S/Scene B", text("rootFolderPath"))),
        );
        object.insert(
            "tags".into(),
            body.get("tags").cloned().unwrap_or(json!([])),
        );
        object.insert("addOptions".into(), body["addOptions"].clone());
        object.insert("hasFile".into(), json!(false));
        state.scenes.push(stored.clone());
        // Slow enough that an unserialized second add would overlap.
        if let Some(id) = state.reply_id {
            stored["id"] = json!(id);
        }
        json_response(201, &stored).set_delay(Duration::from_millis(150))
    }
}

/// `PUT /movie/{id}`: `Movie.ApplyChanges` takes path, profile, monitoring,
/// root folder, tags and add options; everything else stays as stored.
pub struct Update(Shared);

impl Respond for Update {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let Ok(body) = serde_json::from_slice::<Value>(&request.body) else {
            return ResponseTemplate::new(400);
        };
        let id: i64 = request
            .url
            .path()
            .rsplit('/')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(-1);
        let mut state = lock(&self.0);
        state.puts.push(body.clone());
        if state.failing_puts {
            return ResponseTemplate::new(500).set_body_string("Scene A of Studio S failed");
        }
        let Some(scene) = state.scene_mut(id) else {
            return ResponseTemplate::new(404);
        };
        for field in [
            "path",
            "qualityProfileId",
            "monitored",
            "rootFolderPath",
            "tags",
            "addOptions",
        ] {
            if let Some(value) = body.get(field) {
                scene.insert(field.into(), value.clone());
            }
        }
        json_response(202, &body)
    }
}

pub struct Command(Shared);

impl Respond for Command {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body = serde_json::from_slice::<Value>(&request.body).unwrap_or(Value::Null);
        let mut state = lock(&self.0);
        if state.failing_commands > 0 {
            state.failing_commands -= 1;
            return ResponseTemplate::new(503);
        }
        state.commands.push(body);
        json_response(201, &json!({ "id": 1, "status": "queued" }))
    }
}

/// `GET /queue/details?movieId=`: Scene A's queue (derived: every recorded
/// queue was empty), `[]` for any other id (as recorded).
pub struct Queue;

impl Respond for Queue {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if query(request, "movieId").as_deref() == Some(&SCENE_A_ID.to_string()) {
            json_response(200, &derived_json("queue_details.json"))
        } else {
            json_response(200, &json!([]))
        }
    }
}

/// Every write the routes above do not take.
pub struct Forbidden(Shared);

impl Respond for Forbidden {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if request.method != wiremock::http::Method::GET {
            lock(&self.0)
                .forbidden
                .push(format!("{} {}", request.method, request.url.path()));
        }
        ResponseTemplate::new(404)
    }
}

pub struct Fake {
    pub server: MockServer,
    pub state: Shared,
}

impl Fake {
    pub fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    pub fn adapter(&self) -> Whisparr {
        Whisparr::new(client(&self.server.uri()))
    }
}

/// The client as Starr builds it for Whisparr: private content.
pub fn client(base: &str) -> ServiceClient {
    ServiceClient::with_options(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(base).unwrap(),
            auth: AUTH,
            credential: SecretString::from(KEY.to_owned()),
        },
        client_options(),
    )
    .unwrap()
}

pub async fn fake_with(scenes: Vec<Value>, version: &str) -> Fake {
    let server = MockServer::start().await;
    let state: Shared = Arc::new(Mutex::new(State {
        scenes,
        ..State::default()
    }));
    let auth = || header("X-Api-Key", KEY);
    let get = |route: &'static str| Mock::given(method("GET")).and(path(route)).and(auth());
    get("/api/v3/system/status")
        .respond_with(ResponseTemplate::new(200).set_body_string(status_body(version)))
        .mount(&server)
        .await;
    for (route, file) in [
        ("/api/v3/qualityprofile", "qualityprofile.json"),
        ("/api/v3/rootfolder", "rootfolder.json"),
    ] {
        get(route)
            .respond_with(ResponseTemplate::new(200).set_body_string(recorded(file)))
            .mount(&server)
            .await;
    }
    get("/api/v3/movie")
        .and(wiremock::matchers::query_param_contains("stashId", "-"))
        .respond_with(ByStashId(Arc::clone(&state)))
        .mount(&server)
        .await;
    get("/api/v3/lookup/scene")
        .respond_with(LookupScene)
        .mount(&server)
        .await;
    get("/api/v3/queue/details")
        .respond_with(Queue)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v3/movie"))
        .and(header("content-type", "application/json"))
        .and(auth())
        .respond_with(Add(Arc::clone(&state)))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/api/v3/movie/\d+$"))
        .and(header("content-type", "application/json"))
        .and(auth())
        .respond_with(Update(Arc::clone(&state)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v3/command"))
        .and(auth())
        .respond_with(Command(Arc::clone(&state)))
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::any())
        .respond_with(Forbidden(Arc::clone(&state)))
        .with_priority(10)
        .mount(&server)
        .await;
    Fake { server, state }
}

/// Scene A in the library, Whisparr inside the tested range.
pub async fn whisparr() -> Fake {
    fake_with(vec![scene_a()], SUPPORTED).await
}

/// The resource without its `monitored` flag, to compare everything else.
pub fn without_monitored(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("monitored");
    }
    value
}
