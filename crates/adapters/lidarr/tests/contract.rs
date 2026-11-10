//! Lidarr against the generic probe contract and its own `Source` contract.
//!
//! Reads come from `fixtures/3.1.6.5079/recorded/`: read-only GETs against
//! a real Lidarr 3.1.6.5079 with Coldplay in the library (see the README
//! there). What cannot be recorded read-only — write replies, the duplicate
//! add, lookups of an artist Lidarr does not have, a queue with entries —
//! is in `fixtures/3.1.6.5079/*.json`, derived from Lidarr's source and the
//! recorded shapes (README there). The fake below is a small **stateful**
//! Lidarr: an artist with its albums, `POST /album` that creates the
//! embedded artist when it is new, `PUT /album/monitor` and commands. It is
//! stricter than the real service: it answers only requests with the right
//! key, rejects adds without the fields Lidarr validates, and records every
//! write — including any write to the artist itself, which Starr must never
//! send.
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::struct_excessive_bools
)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use secrecy::SecretString;
use serde_json::{Value, json};
use starr_adapter_kit::contract::{self, ProbeContract, RecordedResponse, run_probe_contract};
use starr_adapter_kit::{Endpoint, ServiceClient};
use starr_adapter_lidarr::{AUTH, DESCRIPTOR, Lidarr};
use starr_core::adapters::{AdapterError, Listing, Probe, Version};
use starr_core::source::{
    AddOutcome, AddRequest, Deviation, FileState, Files, Lookup, MetadataProfile, MonitorScope,
    NaturalKey, QualityProfile, QueueEntry, QueueState, RootFolder, Source, UnitId, UnitProgress,
};
use url::Url;
use uuid::Uuid;
use wiremock::matchers::{header, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const SUPPORTED: &str = "3.1.6.5079";
const UNSUPPORTED: &str = "2.14.5.4836";
const KEY: &str = "contract-test-key-0123456789abcdef";

// In the recorded library: Coldplay (Lidarr id 1, unmonitored) with the
// albums kept in `recorded/albums.json`, all unmonitored.
const COLDPLAY: Uuid = Uuid::from_u128(0xcc19_7bad_dc9c_440d_a5b5_d52b_a2e1_4234);
const COLDPLAY_ID: i64 = 1;
const MYLO_XYLOTO: Uuid = Uuid::from_u128(0x61b7_b6cd_86a6_4728_9478_b58c_93bc_a8fd);
const GHOST_STORIES: Uuid = Uuid::from_u128(0x9c5a_a9d2_068b_4626_b352_ae6f_4f4e_0b15);
const MUSIC_OF_THE_SPHERES: Uuid = Uuid::from_u128(0xc169_f440_90bf_4a0b_aa6a_c830_72bd_7eed);
/// A Coldplay album Lidarr does not list (trimmed from the recording),
/// looked up through the derived `album_lookup_parachutes.json`.
const PARACHUTES: Uuid = Uuid::from_u128(0x1dc4_c347_a1db_32aa_b14f_bc9c_c507_b843);

// Not in Lidarr: added through the derived lookups.
const RADIOHEAD: Uuid = Uuid::from_u128(0xa74b_1b7f_71a5_4011_9441_d0b5_e412_2711);
const OK_COMPUTER: Uuid = Uuid::from_u128(0xb139_2450_e666_3926_a536_22c6_5f83_4433);
const KID_A: Uuid = Uuid::from_u128(0x1b02_2e01_4da6_387b_8658_8678_046e_4cef);
const IN_RAINBOWS: Uuid = Uuid::from_u128(0x6e33_5887_60ba_38f0_95af_fae7_7743_36bf);
/// The id the fake gives an artist created by an add.
const NEW_ARTIST_ID: i64 = 2;
/// What the fake stores instead of the requested profile when mangling.
const MANGLED_PROFILE: i64 = 999;

fn read_file(relative: &str) -> String {
    let path = format!("{}/fixtures/{relative}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// A response recorded from Lidarr 3.1.6.5079.
fn recorded(file: &str) -> String {
    read_file(&format!("{SUPPORTED}/recorded/{file}"))
}

fn recorded_json(file: &str) -> Value {
    serde_json::from_str(&recorded(file)).unwrap()
}

/// A response derived from Lidarr's source and the recorded shapes.
fn derived(file: &str) -> String {
    read_file(&format!("{SUPPORTED}/{file}"))
}

fn status_body(version: &str) -> String {
    if version == SUPPORTED {
        recorded("system_status.json")
    } else {
        read_file(&format!("{version}/system_status.json"))
    }
}

fn album_unit(id: Uuid) -> UnitId {
    UnitId::ReleaseGroup(id)
}

fn albums(ids: &[Uuid]) -> MonitorScope {
    MonitorScope::Units(ids.iter().copied().map(album_unit).collect())
}

fn coldplay() -> NaturalKey {
    NaturalKey::MbArtist(COLDPLAY)
}

fn radiohead() -> NaturalKey {
    NaturalKey::MbArtist(RADIOHEAD)
}

/// An add of a new artist (Radiohead) with two albums.
fn add_request() -> AddRequest {
    AddRequest {
        key: radiohead(),
        quality_profile_id: 2,
        root_folder_path: "/music".into(),
        scope: albums(&[OK_COMPUTER, KID_A]),
        search: true,
        minimum_availability: None,
        metadata_profile_id: Some(1),
    }
}

/// An add of an album of the artist the recorded Lidarr has.
fn coldplay_add_request() -> AddRequest {
    AddRequest {
        key: coldplay(),
        scope: albums(&[MYLO_XYLOTO]),
        ..add_request()
    }
}

// --- the fake ---------------------------------------------------------------

#[derive(Debug, Default)]
struct State {
    artist: Option<Value>,
    albums: Vec<Value>,
    /// `GET /queue/details?artistId=` of the artist.
    queue: Vec<Value>,
    /// `GET /artist?mbId=` pretends the artist is absent although it
    /// exists (someone else added it between our lookup and our add).
    hidden: bool,
    /// Store adds with a wrong profile and the album unmonitored.
    mangle: bool,
    /// Reject every add with a validation error (nothing stored).
    reject_adds: bool,
    /// A new artist's refresh lists the rest of its discography
    /// (unmonitored) before Starr's next add. `AddAlbumService` adds the
    /// artist without a refresh, but one may run in between.
    refresh_on_create: bool,
    /// Answer this many commands with 503 before accepting them.
    failing_commands: u32,
    next_album_id: i64,
    album_posts: Vec<Value>,
    monitor_puts: Vec<Value>,
    commands: Vec<Value>,
    /// Writes Starr must never send: to the artist, album updates,
    /// deletes. Method and path.
    forbidden: Vec<String>,
}

impl State {
    fn writes(&self) -> usize {
        self.album_posts.len()
            + self.monitor_puts.len()
            + self.commands.len()
            + self.forbidden.len()
    }

    fn album_mut(&mut self, id: i64) -> Option<&mut serde_json::Map<String, Value>> {
        self.albums
            .iter_mut()
            .filter_map(Value::as_object_mut)
            .find(|a| a.get("id").and_then(Value::as_i64) == Some(id))
    }

    fn has_album(&self, release_group: &str) -> bool {
        self.albums
            .iter()
            .any(|a| a["foreignAlbumId"].as_str() == Some(release_group))
    }

    /// The stored artist if `field` of it equals the query parameter `name`.
    fn artist_matching(&self, request: &Request, name: &str, field: &str) -> Option<&Value> {
        let wanted = query(request, name)?;
        self.artist.as_ref().filter(|artist| match &artist[field] {
            Value::String(s) => *s == wanted,
            Value::Number(n) => n.to_string() == wanted,
            _ => false,
        })
    }

    fn album_mut_by_release_group(
        &mut self,
        release_group: Uuid,
    ) -> &mut serde_json::Map<String, Value> {
        let rg = release_group.to_string();
        self.albums
            .iter_mut()
            .filter_map(Value::as_object_mut)
            .find(|a| a.get("foreignAlbumId").and_then(Value::as_str) == Some(rg.as_str()))
            .unwrap()
    }

    fn artist_mut(&mut self) -> &mut Value {
        self.artist.as_mut().unwrap()
    }
}

fn query(request: &Request, name: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

type Shared = Arc<Mutex<State>>;

fn lock(state: &Shared) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn json_response(status: u16, body: &Value) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .insert_header("content-type", "application/json")
        .set_body_string(body.to_string())
}

/// `GET /artist?mbId=`: the artist with that `foreignArtistId`, or the
/// recorded answer for an unknown MBID (`[]`).
struct ArtistByMbid(Shared);

impl Respond for ArtistByMbid {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let state = lock(&self.0);
        match state.artist_matching(request, "mbId", "foreignArtistId") {
            Some(artist) if !state.hidden => json_response(200, &json!([artist])),
            _ => json_response(200, &recorded_json("artist_unknown.json")),
        }
    }
}

/// `GET /album?artistId=`: the artist's albums, `[]` for another id.
struct AlbumsOfArtist(Shared);

impl Respond for AlbumsOfArtist {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let state = lock(&self.0);
        match state.artist_matching(request, "artistId", "id") {
            Some(_) => json_response(200, &Value::Array(state.albums.clone())),
            None => json_response(200, &json!([])),
        }
    }
}

/// `GET /queue/details?artistId=`: the artist's queue, `[]` for another id.
struct QueueOfArtist(Shared);

impl Respond for QueueOfArtist {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let state = lock(&self.0);
        match state.artist_matching(request, "artistId", "id") {
            Some(_) => json_response(200, &Value::Array(state.queue.clone())),
            None => json_response(200, &json!([])),
        }
    }
}

fn missing_fields(body: &Value) -> Vec<&'static str> {
    let artist = &body["artist"];
    let mut missing = Vec::new();
    if body["foreignAlbumId"].as_str().is_none_or(str::is_empty) {
        missing.push("foreignAlbumId");
    }
    if !artist.is_object() {
        missing.push("artist");
        return missing;
    }
    if artist["foreignArtistId"].as_str().is_none_or(str::is_empty) {
        missing.push("artist.foreignArtistId");
    }
    for field in ["qualityProfileId", "metadataProfileId"] {
        if artist[field].as_i64().is_none_or(|id| id <= 0) {
            missing.push(field);
        }
    }
    if artist["rootFolderPath"].as_str().is_none() && artist["path"].as_str().is_none() {
        missing.push("artist.rootFolderPath");
    }
    missing
}

/// `POST /album` as `AddAlbumService` does it: the embedded artist is
/// created when Lidarr does not have it (its refresh then brings in the
/// rest of the discography, unmonitored under `monitor: none`), else the
/// existing artist is used.
struct AddAlbum(Shared);

impl Respond for AddAlbum {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let Ok(body) = serde_json::from_slice::<Value>(&request.body) else {
            return json_response(400, &json!([{ "errorMessage": "body is not JSON" }]));
        };
        let mut state = lock(&self.0);
        state.album_posts.push(body.clone());
        if state.reject_adds {
            return json_response(
                400,
                &json!([{ "propertyName": "Artist.RootFolderPath", "errorMessage": "Root folder path '/music' does not exist" }]),
            );
        }
        let missing = missing_fields(&body);
        if !missing.is_empty() {
            return json_response(
                400,
                &json!([{ "errorMessage": format!("missing {missing:?}") }]),
            );
        }
        let release_group = body["foreignAlbumId"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if state.has_album(&release_group) {
            state.hidden = false;
            return ResponseTemplate::new(400)
                .insert_header("content-type", "application/json")
                .set_body_string(derived("album_add_duplicate_400.json"));
        }
        if state.artist.is_none() {
            let mut artist = body["artist"].clone();
            artist["id"] = json!(NEW_ARTIST_ID);
            let name = artist["artistName"].as_str().unwrap_or_default().to_owned();
            artist["path"] = json!(format!("/music/{name}"));
            artist.as_object_mut().map(|a| a.remove("addOptions"));
            if state.mangle {
                artist["qualityProfileId"] = json!(MANGLED_PROFILE);
            }
            state.artist = Some(artist);
            // A refresh after the add: the rest of the discography,
            // unmonitored.
            let others = if state.refresh_on_create {
                vec![OK_COMPUTER, KID_A, IN_RAINBOWS]
            } else {
                Vec::new()
            };
            for other in others {
                let other = other.to_string();
                if other != release_group && !state.has_album(&other) {
                    state.next_album_id += 1;
                    let id = state.next_album_id;
                    state.albums.push(json!({
                        "id": id, "artistId": NEW_ARTIST_ID, "foreignAlbumId": other,
                        "title": "Other", "monitored": false,
                        "statistics": { "trackFileCount": 0, "trackCount": 0, "totalTrackCount": 10 }
                    }));
                }
            }
        }
        state.next_album_id += 1;
        let id = state.next_album_id;
        let mut album = body;
        album["id"] = json!(id);
        album["artistId"] = state
            .artist
            .as_ref()
            .map_or(Value::Null, |a| a["id"].clone());
        album["statistics"] =
            json!({ "trackFileCount": 0, "trackCount": 10, "totalTrackCount": 10 });
        let reply = album.clone();
        if let Some(a) = album.as_object_mut() {
            a.remove("artist");
            a.remove("addOptions");
        }
        if state.mangle {
            album["monitored"] = json!(false);
        }
        state.albums.push(album);
        // Slow enough that an unserialized second add would overlap.
        json_response(201, &reply).set_delay(Duration::from_millis(150))
    }
}

struct SetMonitored(Shared);

impl Respond for SetMonitored {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let Ok(body) = serde_json::from_slice::<Value>(&request.body) else {
            return ResponseTemplate::new(400);
        };
        let mut state = lock(&self.0);
        state.monitor_puts.push(body.clone());
        let monitored = body["monitored"].as_bool().unwrap_or(false);
        for id in body["albumIds"].as_array().into_iter().flatten() {
            if let Some(album) = id.as_i64().and_then(|id| state.album_mut(id)) {
                album.insert("monitored".into(), json!(monitored));
            }
        }
        json_response(202, &json!([]))
    }
}

struct Command(Shared);

impl Respond for Command {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body = serde_json::from_slice::<Value>(&request.body).unwrap_or(Value::Null);
        let mut state = lock(&self.0);
        if state.failing_commands > 0 {
            state.failing_commands -= 1;
            return ResponseTemplate::new(503);
        }
        state.commands.push(body);
        json_response(
            201,
            &json!({ "id": 1, "name": "AlbumSearch", "status": "queued" }),
        )
    }
}

struct Forbidden(Shared);

impl Respond for Forbidden {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        lock(&self.0)
            .forbidden
            .push(format!("{} {}", request.method, request.url.path()));
        ResponseTemplate::new(405)
    }
}

struct Fake {
    server: MockServer,
    state: Shared,
}

fn auth() -> wiremock::matchers::HeaderExactMatcher {
    header("X-Api-Key", KEY)
}

async fn mount_get(server: &MockServer, at: &str, query: Option<(&str, &str)>, body: String) {
    let mut mock = Mock::given(method("GET")).and(path(at)).and(auth());
    if let Some((name, value)) = query {
        mock = mock.and(query_param(name, value));
    }
    mock.respond_with(
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_string(body),
    )
    .mount(server)
    .await;
}

/// A fake Lidarr with Coldplay in the library exactly as recorded
/// (`recorded/artist.json`, `recorded/albums.json`, empty queue) or with no
/// artist, reporting `version`.
async fn start_fake(existing: bool, version: &str) -> Fake {
    let server = MockServer::start().await;
    let artist = recorded_json("artist.json")
        .as_array()
        .and_then(|found| found.first().cloned());
    let state: Shared = Arc::new(Mutex::new(State {
        artist: if existing { artist } else { None },
        albums: if existing {
            recorded_json("albums.json")
                .as_array()
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        },
        queue: recorded_json("queue_details.json")
            .as_array()
            .cloned()
            .unwrap_or_default(),
        next_album_id: 200,
        ..State::default()
    }));
    mount_get(&server, "/api/v1/system/status", None, status_body(version)).await;
    for (at, file) in [
        ("/api/v1/qualityprofile", "qualityprofile.json"),
        ("/api/v1/metadataprofile", "metadataprofile.json"),
        ("/api/v1/rootfolder", "rootfolder.json"),
    ] {
        mount_get(&server, at, None, recorded(file)).await;
    }
    for (rg, body) in [
        (MYLO_XYLOTO, recorded("album_lookup_existing.json")),
        (PARACHUTES, derived("album_lookup_parachutes.json")),
        (OK_COMPUTER, derived("album_lookup_ok_computer.json")),
        (KID_A, derived("album_lookup_kid_a.json")),
        (IN_RAINBOWS, derived("album_lookup_in_rainbows.json")),
    ] {
        let term = format!("lidarr:{rg}");
        mount_get(&server, "/api/v1/album/lookup", Some(("term", &term)), body).await;
    }
    Mock::given(method("GET"))
        .and(path("/api/v1/queue/details"))
        .and(auth())
        .respond_with(QueueOfArtist(Arc::clone(&state)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/artist"))
        .and(auth())
        .respond_with(ArtistByMbid(Arc::clone(&state)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/album"))
        .and(auth())
        .respond_with(AlbumsOfArtist(Arc::clone(&state)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/album"))
        .and(header("content-type", "application/json"))
        .and(auth())
        .respond_with(AddAlbum(Arc::clone(&state)))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/album/monitor"))
        .and(header("content-type", "application/json"))
        .and(auth())
        .respond_with(SetMonitored(Arc::clone(&state)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/command"))
        .and(auth())
        .respond_with(Command(Arc::clone(&state)))
        .mount(&server)
        .await;
    for (verb, at) in [
        ("POST", r"^/api/v1/artist"),
        ("PUT", r"^/api/v1/artist"),
        ("PUT", r"^/api/v1/album/\d+$"),
        ("DELETE", r"^/"),
    ] {
        Mock::given(method(verb))
            .and(path_regex(at))
            .respond_with(Forbidden(Arc::clone(&state)))
            .mount(&server)
            .await;
    }
    Fake { server, state }
}

fn client(server: &MockServer) -> ServiceClient {
    ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(&server.uri()).unwrap(),
            auth: AUTH,
            credential: SecretString::from(KEY.to_owned()),
        },
    )
    .unwrap()
}

fn adapter(fake: &Fake) -> Lidarr {
    Lidarr::new(client(&fake.server))
}

fn monitored_albums(fake: &Fake) -> BTreeSet<String> {
    lock(&fake.state)
        .albums
        .iter()
        .filter(|a| a["monitored"] == json!(true))
        .filter_map(|a| a["foreignAlbumId"].as_str().map(str::to_owned))
        .collect()
}

// --- probe ------------------------------------------------------------------

fn status_fixture(version: &str) -> contract::Fixture {
    contract::Fixture {
        version: version.parse().unwrap(),
        body: status_body(version),
    }
}

#[tokio::test]
async fn lidarr_passes_the_probe_contract() {
    run_probe_contract(&ProbeContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        probe_method: "GET",
        probe_path: "/api/v1/system/status",
        supported: vec![status_fixture(SUPPORTED)],
        unsupported: status_fixture(UNSUPPORTED),
        // Same `ApiKeyAuthenticationHandler` as Radarr and Sonarr: empty 401.
        unauthorized: RecordedResponse {
            status: 401,
            body: String::new(),
        },
        build: Lidarr::new,
    })
    .await;
}

#[tokio::test]
async fn another_arr_app_at_the_url_is_not_accepted_as_lidarr() {
    let server = MockServer::start().await;
    let body = recorded("system_status.json")
        .replace("\"appName\": \"Lidarr\"", "\"appName\": \"Sonarr\"");
    Mock::given(path("/api/v1/system/status"))
        .and(auth())
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;
    let result = Lidarr::new(client(&server)).probe().await;
    assert!(
        matches!(&result, Err(AdapterError::Unknown(msg)) if msg.contains("Sonarr")),
        "{result:?}"
    );
}

#[test]
fn descriptor_declares_requests_and_a_tested_range() {
    assert_eq!(DESCRIPTOR.type_id, "lidarr");
    let range = DESCRIPTOR.tested_versions;
    assert!(range.contains(&Version::new(3, 0, 0, 0)));
    assert!(range.contains(&Version::new(3, 2, 0, 5144)));
    assert!(!range.contains(&Version::new(2, 14, 5, 4836)));
    assert!(!range.contains(&Version::new(4, 0, 0, 0)));
}

// --- lists and lookup -------------------------------------------------------

#[tokio::test]
async fn lists_profiles_folders_and_metadata_profiles() {
    let fake = start_fake(false, SUPPORTED).await;
    let lidarr = adapter(&fake);
    assert_eq!(
        lidarr.quality_profiles().await,
        Ok(Listing {
            items: vec![
                QualityProfile {
                    id: 1,
                    name: "Any".into()
                },
                QualityProfile {
                    id: 2,
                    name: "Lossless".into()
                },
                QualityProfile {
                    id: 3,
                    name: "Standard".into()
                },
                QualityProfile {
                    id: 4,
                    name: "FLAC bevorzugt".into()
                },
            ],
            complete: true
        })
    );
    assert_eq!(
        lidarr.root_folders().await,
        Ok(Listing {
            items: vec![RootFolder {
                id: 1,
                path: "/music".into(),
                accessible: true,
                free_space_bytes: Some(2_888_925_659_136),
            }],
            complete: true
        })
    );
    assert_eq!(lock(&fake.state).writes(), 0);
}

#[tokio::test]
async fn lidarrs_default_metadata_profiles_both_trigger_the_q6_warning() {
    let fake = start_fake(false, SUPPORTED).await;
    let profiles = adapter(&fake).metadata_profiles().await.unwrap();
    assert!(profiles.complete);
    // As shipped: "Standard" allows the primary type Album only (no EP),
    // "None" allows nothing.
    assert_eq!(
        profiles.items,
        vec![
            MetadataProfile {
                id: 1,
                name: "Standard".into(),
                primary_types: vec!["Album".into()],
                secondary_types: vec!["Studio".into()],
            },
            MetadataProfile {
                id: 2,
                name: "None".into(),
                primary_types: vec![],
                secondary_types: vec![],
            },
        ]
    );
    // Spec 0006 Q6 A: Starr warns for a profile without Album, EP and
    // Studio, so for both of Lidarr's defaults.
    assert!(profiles.items.iter().all(|p| !p.admits_albums_and_eps()));
    assert_eq!(lock(&fake.state).writes(), 0);
}

#[tokio::test]
async fn lookup_reads_the_artist_and_its_albums() {
    // An unknown MBID comes back as `[]` (recorded).
    let fake_absent = start_fake(false, SUPPORTED).await;
    assert_eq!(
        adapter(&fake_absent).lookup(coldplay()).await,
        Ok(Lookup::NotInLibrary)
    );

    // Coldplay as recorded: unmonitored, every album unmonitored. The
    // resource has no `mbId`; the artist is matched by `foreignArtistId`.
    let fake = start_fake(true, SUPPORTED).await;
    let Ok(Lookup::InLibrary(item)) = adapter(&fake).lookup(coldplay()).await else {
        panic!("expected the artist");
    };
    assert_eq!(item.remote_id, COLDPLAY_ID);
    assert_eq!(item.key, coldplay());
    assert_eq!(item.title, "Coldplay");
    assert!(!item.monitored);
    assert_eq!(item.quality_profile_id, 4);
    assert_eq!(item.root_folder_path.as_deref(), Some("/music"));
    let units: Vec<(UnitId, bool)> = item.units.iter().map(|u| (u.unit, u.monitored)).collect();
    assert_eq!(
        units,
        vec![
            (album_unit(MYLO_XYLOTO), false),
            (album_unit(GHOST_STORIES), false),
            (album_unit(MUSIC_OF_THE_SPHERES), false),
        ]
    );
    assert_eq!(lock(&fake.state).writes(), 0);
}

#[tokio::test]
async fn an_answer_for_another_artist_is_not_taken_for_the_requested_one() {
    // Should a Lidarr ignore the `mbId` filter, it lists other artists.
    let server = MockServer::start().await;
    mount_get(&server, "/api/v1/artist", None, recorded("artist.json")).await;
    assert_eq!(
        Lidarr::new(client(&server)).lookup(radiohead()).await,
        Ok(Lookup::NotInLibrary)
    );
}

// --- add --------------------------------------------------------------------

#[tokio::test]
async fn add_creates_the_artist_monitoring_only_the_requested_albums() {
    let fake = start_fake(false, SUPPORTED).await;
    let outcome = adapter(&fake).add(&add_request()).await;
    let Ok(AddOutcome::Added(report)) = outcome else {
        panic!("expected Added, got {outcome:?}");
    };
    assert!(report.written);
    assert_eq!(report.deviations, vec![], "exact add must not deviate");
    assert_eq!(report.item.remote_id, NEW_ARTIST_ID);
    assert!(report.item.unit_monitored(album_unit(OK_COMPUTER)));
    assert!(report.item.unit_monitored(album_unit(KID_A)));
    assert_eq!(report.item.units.len(), 2);

    let state = lock(&fake.state);
    assert_eq!(state.album_posts.len(), 2, "one add per album");
    assert!(state.monitor_puts.is_empty() && state.commands.is_empty());
    assert!(state.forbidden.is_empty(), "{:?}", state.forbidden);
    // The first add creates the artist with the category's settings.
    let first = &state.album_posts[0];
    assert_eq!(first["monitored"], json!(true));
    assert_eq!(first["addOptions"], json!({ "searchForNewAlbum": true }));
    let artist = &first["artist"];
    assert_eq!(artist["foreignArtistId"], json!(RADIOHEAD.to_string()));
    assert_eq!(artist["qualityProfileId"], json!(2));
    assert_eq!(artist["metadataProfileId"], json!(1));
    assert_eq!(artist["rootFolderPath"], json!("/music"));
    assert_eq!(artist["monitored"], json!(true));
    assert_eq!(artist["monitorNewItems"], json!("none"));
    assert_eq!(
        artist["addOptions"],
        json!({
            "monitor": "none",
            // Should Lidarr apply the add options after its first scan, it
            // monitors exactly the requested albums, not none of them.
            "albumsToMonitor": [KID_A.to_string(), OK_COMPUTER.to_string()],
            "searchForMissingAlbums": false
        })
    );
    // The second one uses the artist that exists now, as Lidarr has it.
    let second = &state.album_posts[1];
    assert_eq!(second["artist"]["id"], json!(NEW_ARTIST_ID));
    assert_eq!(second["artist"]["monitorNewItems"], json!("none"));
    drop(state);
    assert_eq!(
        monitored_albums(&fake),
        [OK_COMPUTER.to_string(), KID_A.to_string()].into()
    );
}

#[tokio::test]
async fn an_album_the_refresh_brought_in_is_monitored_not_added_twice() {
    let fake = start_fake(false, SUPPORTED).await;
    lock(&fake.state).refresh_on_create = true;
    let outcome = adapter(&fake).add(&add_request()).await;
    let Ok(AddOutcome::Added(report)) = outcome else {
        panic!("expected Added, got {outcome:?}");
    };
    assert_eq!(report.deviations, vec![]);
    assert!(!report.item.unit_monitored(album_unit(IN_RAINBOWS)));
    let state = lock(&fake.state);
    assert_eq!(state.album_posts.len(), 1, "only the first album is posted");
    assert_eq!(state.monitor_puts.len(), 1, "Kid A was there: switched on");
    assert_eq!(state.monitor_puts[0]["monitored"], json!(true));
    assert_eq!(
        state.commands,
        vec![json!({ "name": "AlbumSearch", "albumIds": [state.monitor_puts[0]["albumIds"][0]] })]
    );
    assert!(state.forbidden.is_empty(), "{:?}", state.forbidden);
}

#[tokio::test]
async fn add_reports_a_read_back_mismatch() {
    let fake = start_fake(false, SUPPORTED).await;
    lock(&fake.state).mangle = true;
    let Ok(AddOutcome::Added(report)) = adapter(&fake).add(&add_request()).await else {
        panic!("expected Added");
    };
    assert!(report.deviations.contains(&Deviation::QualityProfile {
        expected: 2,
        found: MANGLED_PROFILE
    }));
    for album in [OK_COMPUTER, KID_A] {
        assert!(
            report
                .deviations
                .contains(&Deviation::UnitNotMonitored(album_unit(album))),
            "{:?}",
            report.deviations
        );
    }
}

#[tokio::test]
async fn concurrent_adds_of_one_artist_are_serialized() {
    let fake = start_fake(false, SUPPORTED).await;
    let shared = client(&fake.server);
    let (first, second) = (Lidarr::new(shared.clone()), Lidarr::new(shared));
    let request = add_request();
    let (a, b) = tokio::join!(first.add(&request), second.add(&request));
    let added = [&a, &b]
        .iter()
        .filter(|r| matches!(r, Ok(AddOutcome::Added(_))))
        .count();
    let existed = [&a, &b]
        .iter()
        .filter(|r| matches!(r, Ok(AddOutcome::AlreadyExists(_))))
        .count();
    assert_eq!((added, existed), (1, 1), "{a:?} / {b:?}");
    assert_eq!(lock(&fake.state).album_posts.len(), 2, "two albums, once");
}

#[tokio::test]
async fn an_artist_added_meanwhile_is_left_untouched() {
    let fake = start_fake(true, SUPPORTED).await;
    lock(&fake.state).hidden = true;
    let outcome = adapter(&fake).add(&coldplay_add_request()).await;
    let Ok(AddOutcome::AlreadyExists(item)) = outcome else {
        panic!("expected AlreadyExists, got {outcome:?}");
    };
    assert_eq!(item.remote_id, COLDPLAY_ID);
    let state = lock(&fake.state);
    assert_eq!(state.album_posts.len(), 1, "the one rejected add");
    assert!(state.monitor_puts.is_empty() && state.forbidden.is_empty());
}

#[tokio::test]
async fn add_of_an_artist_in_lidarr_writes_nothing() {
    let fake = start_fake(true, SUPPORTED).await;
    assert!(matches!(
        adapter(&fake).add(&coldplay_add_request()).await,
        Ok(AddOutcome::AlreadyExists(_))
    ));
    assert_eq!(lock(&fake.state).writes(), 0);
}

#[tokio::test]
async fn add_without_a_metadata_profile_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let request = AddRequest {
        metadata_profile_id: None,
        ..add_request()
    };
    let result = Lidarr::new(client(&server)).add(&request).await;
    assert!(
        matches!(result, Err(AdapterError::Rejected(_))),
        "{result:?}"
    );
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        0
    );
}

#[tokio::test]
async fn a_rejected_add_reports_lidarrs_reason() {
    let fake = start_fake(false, SUPPORTED).await;
    lock(&fake.state).reject_adds = true;
    match adapter(&fake).add(&add_request()).await {
        Err(AdapterError::Rejected(reason)) => {
            assert!(
                reason.contains("Root folder path '/music' does not exist"),
                "{reason}"
            );
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    assert_eq!(
        lock(&fake.state).album_posts.len(),
        1,
        "no retry of a rejected add"
    );
}

#[tokio::test]
async fn a_server_error_on_the_add_stays_transient() {
    let fake = start_fake(false, SUPPORTED).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/album"))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(1)
        .mount(&fake.server)
        .await;
    let result = adapter(&fake).add(&add_request()).await;
    assert!(
        matches!(result, Err(AdapterError::Transient(_))),
        "{result:?}"
    );
}

// --- extend and stop --------------------------------------------------------

/// Coldplay as recorded, but monitored: searches only go out for a
/// monitored artist.
async fn start_monitored_coldplay() -> Fake {
    let fake = start_fake(true, SUPPORTED).await;
    lock(&fake.state).artist_mut()["monitored"] = json!(true);
    fake
}

#[tokio::test]
async fn extend_monitors_known_albums_and_adds_missing_ones_under_the_artist() {
    let fake = start_monitored_coldplay().await;
    let artist_as_read = lock(&fake.state).artist.clone();
    let lidarr = adapter(&fake);
    // Ghost Stories is in Lidarr but unmonitored, Parachutes is not there.
    let scope = albums(&[GHOST_STORIES, PARACHUTES]);
    let report = lidarr
        .extend_monitoring(coldplay(), &scope, true)
        .await
        .unwrap();
    assert!(report.written);
    assert_eq!(report.deviations, vec![]);
    {
        let state = lock(&fake.state);
        assert!(state.forbidden.is_empty(), "{:?}", state.forbidden);
        assert_eq!(
            state.monitor_puts,
            vec![json!({ "albumIds": [2], "monitored": true })]
        );
        assert_eq!(state.album_posts.len(), 1);
        // The existing artist goes along exactly as Lidarr has it.
        assert_eq!(
            Some(&state.album_posts[0]["artist"]),
            artist_as_read.as_ref()
        );
        assert_eq!(
            state.album_posts[0]["foreignAlbumId"],
            json!(PARACHUTES.to_string())
        );
        assert_eq!(
            state.album_posts[0]["addOptions"],
            json!({ "searchForNewAlbum": true })
        );
        // Ghost Stories is searched by command; Parachutes by its own add.
        assert_eq!(
            state.commands,
            vec![json!({ "name": "AlbumSearch", "albumIds": [2] })]
        );
    }
    assert_eq!(
        monitored_albums(&fake),
        [GHOST_STORIES.to_string(), PARACHUTES.to_string()].into()
    );

    // Asking again writes nothing but searches again (a lost search on a
    // retry must not stay lost).
    let again = lidarr
        .extend_monitoring(coldplay(), &scope, true)
        .await
        .unwrap();
    assert!(!again.written);
    assert_eq!(again.deviations, vec![]);
    let state = lock(&fake.state);
    assert_eq!((state.monitor_puts.len(), state.album_posts.len()), (1, 1));
    assert_eq!(state.commands.len(), 2);
    assert_eq!(
        state.commands[1]["albumIds"].as_array().map(Vec::len),
        Some(2)
    );
}

/// An album Lidarr monitors with every track on disk is already there
/// (spec 0006 acceptance: zero Lidarr writes); one switched on that has
/// all its files gets exactly the monitor write, no search.
#[tokio::test]
async fn complete_albums_get_no_search() {
    let fake = start_monitored_coldplay().await;
    {
        let mut state = lock(&fake.state);
        for (rg, monitored) in [(MYLO_XYLOTO, true), (GHOST_STORIES, false)] {
            let album = state.album_mut_by_release_group(rg);
            album.insert("monitored".into(), json!(monitored));
            album.insert(
                "statistics".into(),
                json!({ "trackFileCount": 14, "trackCount": 14, "totalTrackCount": 28 }),
            );
        }
    }
    let lidarr = adapter(&fake);
    let report = lidarr
        .extend_monitoring(coldplay(), &albums(&[MYLO_XYLOTO]), true)
        .await
        .unwrap();
    assert!(!report.written);
    assert_eq!(lock(&fake.state).writes(), 0);

    let report = lidarr
        .extend_monitoring(coldplay(), &albums(&[GHOST_STORIES]), true)
        .await
        .unwrap();
    assert!(report.written);
    let state = lock(&fake.state);
    assert_eq!(
        state.monitor_puts,
        vec![json!({ "albumIds": [2], "monitored": true })]
    );
    assert!(state.commands.is_empty(), "{:?}", state.commands);
}

#[tokio::test]
async fn a_failed_search_is_repeated_on_retry() {
    let fake = start_monitored_coldplay().await;
    lock(&fake.state).failing_commands = 1;
    let lidarr = adapter(&fake);
    let scope = albums(&[GHOST_STORIES]);
    let first = lidarr.extend_monitoring(coldplay(), &scope, true).await;
    assert!(
        matches!(first, Err(AdapterError::Transient(_))),
        "{first:?}"
    );
    let retry = lidarr
        .extend_monitoring(coldplay(), &scope, true)
        .await
        .unwrap();
    assert!(!retry.written);
    let state = lock(&fake.state);
    assert_eq!(state.monitor_puts.len(), 1);
    assert_eq!(state.commands.len(), 1);
}

#[tokio::test]
async fn an_unmonitored_artist_is_reported_not_switched_on() {
    // Coldplay is unmonitored in the recording.
    let fake = start_fake(true, SUPPORTED).await;
    let report = adapter(&fake)
        .extend_monitoring(coldplay(), &albums(&[GHOST_STORIES]), true)
        .await
        .unwrap();
    assert!(report.deviations.contains(&Deviation::ItemNotMonitored));
    let state = lock(&fake.state);
    assert!(state.forbidden.is_empty(), "the artist is never written");
    assert!(
        state.commands.is_empty(),
        "nothing searched for an unmonitored artist"
    );
}

#[tokio::test]
async fn stop_unmonitors_exactly_the_albums_and_never_the_artist() {
    let fake = start_monitored_coldplay().await;
    let lidarr = adapter(&fake);
    let scope = albums(&[GHOST_STORIES, PARACHUTES]);
    lidarr
        .extend_monitoring(coldplay(), &scope, false)
        .await
        .unwrap();
    // Mylo Xyloto was monitored before and stays so.
    lock(&fake.state).album_mut_by_release_group(MYLO_XYLOTO)["monitored"] = json!(true);
    let artist_before = lock(&fake.state).artist.clone();
    let report = lidarr.stop_monitoring(coldplay(), &scope).await.unwrap();
    assert!(report.written);
    assert_eq!(report.deviations, vec![], "exact stop must not deviate");
    assert!(report.item.monitored, "the artist stays monitored (Q13 A)");
    {
        let state = lock(&fake.state);
        assert_eq!(state.monitor_puts.len(), 2);
        let stop = &state.monitor_puts[1];
        assert_eq!(stop["monitored"], json!(false));
        let mut ids: Vec<i64> = stop["albumIds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_i64)
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![2, 201]);
        assert!(state.commands.is_empty(), "a stop searches nothing");
        assert!(state.forbidden.is_empty(), "{:?}", state.forbidden);
        assert_eq!(state.artist, artist_before);
    }
    assert_eq!(monitored_albums(&fake), [MYLO_XYLOTO.to_string()].into());
    // Stopping again writes nothing.
    let again = lidarr.stop_monitoring(coldplay(), &scope).await.unwrap();
    assert!(!again.written);
    assert_eq!(lock(&fake.state).monitor_puts.len(), 2);
}

#[tokio::test]
async fn absent_artists_are_not_found_and_get_no_writes() {
    let fake = start_fake(false, SUPPORTED).await;
    let lidarr = adapter(&fake);
    let scope = albums(&[GHOST_STORIES]);
    assert!(matches!(
        lidarr.progress(coldplay()).await,
        Err(AdapterError::NotFound(_))
    ));
    assert!(matches!(
        lidarr.extend_monitoring(coldplay(), &scope, true).await,
        Err(AdapterError::NotFound(_))
    ));
    assert!(matches!(
        lidarr.stop_monitoring(coldplay(), &scope).await,
        Err(AdapterError::NotFound(_))
    ));
    assert_eq!(lock(&fake.state).writes(), 0);
}

// --- progress ---------------------------------------------------------------

#[tokio::test]
async fn progress_of_the_recorded_artist_has_no_files_and_an_empty_queue() {
    let fake = start_fake(true, SUPPORTED).await;
    let progress = adapter(&fake).progress(coldplay()).await.unwrap();
    assert_eq!(progress.item.remote_id, COLDPLAY_ID);
    // Unmonitored albums report `trackCount` 0 (recorded), so nothing is
    // expected of them.
    let unit = |rg| UnitProgress {
        unit: album_unit(rg),
        monitored: false,
        expected: 0,
        expected_with_file: 0,
        with_file: 0,
    };
    assert_eq!(
        progress.files,
        Files::Units(Listing {
            items: vec![
                unit(MYLO_XYLOTO),
                unit(GHOST_STORIES),
                unit(MUSIC_OF_THE_SPHERES)
            ],
            complete: true,
        })
    );
    assert_eq!(progress.files.state(), FileState::None);
    assert_eq!(
        progress.queue,
        Listing {
            items: vec![],
            complete: true
        }
    );
    assert_eq!(lock(&fake.state).writes(), 0);
}

#[tokio::test]
async fn progress_counts_tracks_per_album_and_maps_the_queue() {
    let fake = start_fake(true, SUPPORTED).await;
    {
        // Not recordable read-only: files on disk and downloads in the
        // queue. Statistics as `AlbumStatisticsResource` reports them, the
        // queue from the derived `queue_details.json`.
        let mut state = lock(&fake.state);
        let mylo = state.album_mut_by_release_group(MYLO_XYLOTO);
        mylo.insert("monitored".into(), json!(true));
        mylo.insert(
            "statistics".into(),
            json!({ "trackFileCount": 14, "trackCount": 14, "totalTrackCount": 28, "sizeOnDisk": 480_000_000, "percentOfTracks": 100.0 }),
        );
        state.album_mut_by_release_group(GHOST_STORIES).insert(
            "statistics".into(),
            json!({ "trackFileCount": 3, "trackCount": 3, "totalTrackCount": 15, "sizeOnDisk": 90_000_000, "percentOfTracks": 100.0 }),
        );
        state.queue = serde_json::from_str::<Value>(&derived("queue_details.json"))
            .unwrap()
            .as_array()
            .cloned()
            .unwrap();
    }
    let progress = adapter(&fake).progress(coldplay()).await.unwrap();
    assert_eq!(progress.item.remote_id, COLDPLAY_ID);
    let expected = Files::Units(Listing {
        items: vec![
            UnitProgress {
                unit: album_unit(MYLO_XYLOTO),
                monitored: true,
                expected: 14,
                expected_with_file: 14,
                with_file: 14,
            },
            UnitProgress {
                unit: album_unit(GHOST_STORIES),
                monitored: false,
                expected: 3,
                expected_with_file: 3,
                with_file: 3,
            },
            UnitProgress {
                unit: album_unit(MUSIC_OF_THE_SPHERES),
                monitored: false,
                expected: 0,
                expected_with_file: 0,
                with_file: 0,
            },
        ],
        complete: true,
    });
    assert_eq!(progress.files, expected);
    // Only the monitored album counts: Mylo Xyloto is complete.
    assert_eq!(progress.files.state(), FileState::Complete);
    {
        // A second monitored album with 5 of 17 tracks: partial.
        let mut state = lock(&fake.state);
        let spheres = state.album_mut_by_release_group(MUSIC_OF_THE_SPHERES);
        spheres.insert("monitored".into(), json!(true));
        spheres.insert(
            "statistics".into(),
            json!({ "trackFileCount": 5, "trackCount": 17, "totalTrackCount": 17, "sizeOnDisk": 150_000_000, "percentOfTracks": 29.4 }),
        );
    }
    let partial = adapter(&fake).progress(coldplay()).await.unwrap();
    let Files::Units(units) = &partial.files else {
        panic!("expected units");
    };
    assert_eq!(
        units.items[2],
        UnitProgress {
            unit: album_unit(MUSIC_OF_THE_SPHERES),
            monitored: true,
            expected: 17,
            expected_with_file: 5,
            with_file: 5,
        }
    );
    assert_eq!(partial.files.state(), FileState::Partial);
    assert_eq!(
        progress.queue,
        Listing {
            items: vec![
                QueueEntry {
                    unit: Some(album_unit(GHOST_STORIES)),
                    size_bytes: 400_000_000,
                    size_left_bytes: 100_000_000,
                    state: QueueState::Downloading,
                    message: None,
                },
                QueueEntry {
                    unit: Some(album_unit(GHOST_STORIES)),
                    size_bytes: 410_000_000,
                    size_left_bytes: 0,
                    state: QueueState::Warning,
                    message: Some("Album match is not close enough: 71.4% vs 80% [Tracks]".into()),
                },
                QueueEntry {
                    unit: None,
                    size_bytes: 50_000_000,
                    size_left_bytes: 50_000_000,
                    state: QueueState::Queued,
                    message: None,
                },
            ],
            complete: true
        }
    );
    assert_eq!(lock(&fake.state).writes(), 0);
}

// --- taxonomy and versions ----------------------------------------------------

#[tokio::test]
async fn failures_follow_the_taxonomy() {
    type Expect = fn(&AdapterError) -> bool;
    let cases: [(ResponseTemplate, Expect); 4] = [
        (ResponseTemplate::new(401), |e| {
            *e == AdapterError::CredentialsInvalid
        }),
        (
            ResponseTemplate::new(429).insert_header("retry-after", "5"),
            |e| {
                *e == AdapterError::Throttled {
                    retry_after: Some(Duration::from_secs(5)),
                }
            },
        ),
        (ResponseTemplate::new(500), |e| {
            matches!(e, AdapterError::Transient(_))
        }),
        (ResponseTemplate::new(503), |e| {
            matches!(e, AdapterError::Transient(_))
        }),
    ];
    for (response, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(response)
            .mount(&server)
            .await;
        let result = Lidarr::new(client(&server)).lookup(coldplay()).await;
        assert!(result.as_ref().err().is_some_and(expected), "{result:?}");
    }
}

#[tokio::test]
async fn outside_the_tested_range_reads_work_and_writes_do_not() {
    let fake = start_fake(true, UNSUPPORTED).await;
    let lidarr = adapter(&fake);
    let scope = albums(&[GHOST_STORIES]);
    assert!(matches!(
        lidarr.lookup(coldplay()).await,
        Ok(Lookup::InLibrary(_))
    ));
    assert!(
        lidarr.progress(coldplay()).await.is_ok(),
        "reads keep working"
    );
    assert!(lidarr.metadata_profiles().await.is_ok());
    assert!(matches!(
        lidarr.extend_monitoring(coldplay(), &scope, true).await,
        Err(AdapterError::VersionUnsupported { .. })
    ));
    assert!(matches!(
        lidarr.stop_monitoring(coldplay(), &scope).await,
        Err(AdapterError::VersionUnsupported { .. })
    ));
    let empty = start_fake(false, UNSUPPORTED).await;
    assert!(matches!(
        adapter(&empty).add(&add_request()).await,
        Err(AdapterError::VersionUnsupported { .. })
    ));
    assert_eq!(lock(&fake.state).writes(), 0);
    assert_eq!(lock(&empty.state).writes(), 0);
}

#[tokio::test]
async fn movies_series_and_whole_item_scopes_are_refused_without_a_request() {
    let server = MockServer::start().await;
    let lidarr = Lidarr::new(client(&server));
    let movie = lidarr.lookup(NaturalKey::TmdbMovie(603)).await;
    assert!(matches!(movie, Err(AdapterError::Rejected(_))), "{movie:?}");
    let series = lidarr.progress(NaturalKey::TvdbSeries(81189)).await;
    assert!(
        matches!(series, Err(AdapterError::Rejected(_))),
        "{series:?}"
    );
    let whole = lidarr
        .extend_monitoring(coldplay(), &MonitorScope::Item, false)
        .await;
    assert!(matches!(whole, Err(AdapterError::Rejected(_))), "{whole:?}");
    let seasons = lidarr
        .stop_monitoring(coldplay(), &MonitorScope::seasons([1]))
        .await;
    assert!(
        matches!(seasons, Err(AdapterError::Rejected(_))),
        "{seasons:?}"
    );
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        0
    );
}
