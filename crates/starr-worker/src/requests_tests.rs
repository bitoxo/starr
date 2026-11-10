#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use secrecy::SecretString;
use serde_json::{Value, json};
use starr_core::categories::MinimumAvailability;
use starr_core::jobs::{Expectation, ItemOutcome};
use starr_core::members::{Category, Entitlement, RequestMode};
use starr_core::requests::{AcquisitionState, RequestState};
use starr_db::members::NewMember;
use starr_db::requests::{Actor, EventTrigger, NewRequest, RequestFollow, SourceTarget, Unit};
use starr_db::services::{NewServiceInstance, StoredCredential};
use tokio_util::sync::CancellationToken;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::secrets::CredentialKey;

const KEY: &str = "servarr-api-key-0123456789abcdef";
const MEMBER: Actor<'static> = Actor {
    trigger: EventTrigger::Member,
    name: "mia",
};

fn fixture(adapter: &str, version: &str, name: &str) -> Value {
    let path = format!(
        "{}/../adapters/{adapter}/fixtures/{version}/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn radarr(name: &str) -> Value {
    fixture("radarr", "6.4.4.10685", name)
}

fn sonarr(name: &str) -> Value {
    fixture("sonarr", "4.0.20.3014", name)
}

/// What a fake Radarr or Sonarr holds and was asked.
#[derive(Default)]
struct State {
    /// The title in the library (movie or series resource).
    title: Option<Value>,
    /// Bodies of POST/PUT writes, by path.
    writes: Vec<(String, String, Value)>,
    /// Commands (searches) posted.
    commands: Vec<Value>,
    queue: Value,
    episodes: Value,
    /// Answer every library request with this status.
    fail_with: Option<u16>,
    /// Delay the answers to library reads (the lookup) by this much.
    lookup_delay: Option<std::time::Duration>,
}

/// A stateful fake of the `/api/v3` surface the source adapters use,
/// built from the recorded shapes.
#[derive(Clone)]
struct FakeServarr {
    kind: &'static str,
    state: Arc<Mutex<State>>,
}

impl FakeServarr {
    fn new(kind: &'static str) -> Self {
        Self {
            kind,
            state: Arc::new(Mutex::new(State {
                queue: json!([]),
                episodes: json!([]),
                ..State::default()
            })),
        }
    }

    fn with(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap());
    }

    fn writes(&self) -> Vec<(String, String, Value)> {
        self.state.lock().unwrap().writes.clone()
    }

    async fn start(&self) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(self.clone())
            .mount(&server)
            .await;
        server
    }
}

impl Respond for FakeServarr {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if request
            .headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            != Some(KEY)
        {
            return ResponseTemplate::new(401);
        }
        let path = request.url.path().to_owned();
        let method = request.method.to_string();
        let ok = |v: Value| ResponseTemplate::new(200).set_body_json(v);
        let (library, id) = match self.kind {
            "radarr" => ("/api/v3/movie", 1),
            _ => ("/api/v3/series", 7),
        };
        if path == "/api/v3/system/status" {
            return ok(match self.kind {
                "radarr" => radarr("system_status.json"),
                _ => sonarr("system_status.json"),
            });
        }
        let mut state = self.state.lock().unwrap();
        if let Some(status) = state.fail_with {
            return ResponseTemplate::new(status).set_body_string("[{\"errorMessage\": \"nope\"}]");
        }
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        match (method.as_str(), path.as_str()) {
            ("GET", p) if p == library => {
                let listing = ok(json!(state.title.iter().cloned().collect::<Vec<_>>()));
                match state.lookup_delay {
                    Some(delay) => listing.set_delay(delay),
                    None => listing,
                }
            }
            ("GET", "/api/v3/movie/lookup/tmdb") => ok(radarr("movie_lookup_tmdb.json")),
            ("GET", "/api/v3/series/lookup") => ok(sonarr("series_lookup.json")),
            ("POST", p) if p == library => {
                state.writes.push((method, path, body.clone()));
                let mut created = body;
                created["id"] = json!(id);
                state.title = Some(created.clone());
                ok(created)
            }
            ("GET", p) if p == format!("{library}/{id}") => match &state.title {
                Some(t) => ok(t.clone()),
                None => ResponseTemplate::new(404),
            },
            ("PUT", p) if p == format!("{library}/{id}") => {
                state.writes.push((method, path, body.clone()));
                state.title = Some(body.clone());
                ok(body)
            }
            ("POST", "/api/v3/command") => {
                state.commands.push(body);
                ok(json!({ "id": 1 }))
            }
            ("GET", "/api/v3/queue/details") => ok(state.queue.clone()),
            ("GET", "/api/v3/episode") => ok(state.episodes.clone()),
            _ => ResponseTemplate::new(404),
        }
    }
}

fn seal(key: &CredentialKey, value: &str) -> StoredCredential {
    let sealer = starr_secrets::CredentialSealer::parse(key.recipient()).unwrap();
    let (recipient, ciphertext) = sealer
        .seal(&SecretString::from(value.to_owned()))
        .unwrap()
        .into_parts();
    StoredCredential {
        ciphertext,
        recipient,
        class: starr_core::adapters::SecretClass::ServiceCredential,
    }
}

struct Env {
    db: Database,
    jobs: SystemJobs,
    member: i64,
    _keys: std::path::PathBuf,
}

async fn env() -> Env {
    let db = Database::open_in_memory().await.unwrap();
    let dir = std::env::temp_dir().join(format!(
        "starr-requests-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let key = CredentialKey::init(&db, &dir.join("secret.key"))
        .await
        .unwrap();
    let member = db
        .create_member(
            &NewMember {
                username: "mia",
                email: "mia@example.com",
                display_name: "Mia",
                password_hash: None,
            },
            &[] as &[Entitlement],
            Utc::now(),
        )
        .await
        .unwrap();
    Env {
        db,
        jobs: SystemJobs::new(key),
        member,
        _keys: dir,
    }
}

impl Env {
    async fn instance(&self, adapter_type: &str, url: &str) -> i64 {
        self.db
            .create_service(
                &NewServiceInstance {
                    adapter_type,
                    name: adapter_type,
                    base_url: url,
                    public_url: None,
                    credential: &seal(&self.jobs.credentials, KEY),
                },
                Utc::now(),
            )
            .await
            .unwrap()
            .id
    }

    async fn item(&self, sql: &str) -> i64 {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let (id,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(sql.to_owned()))
            .bind(now)
            .fetch_one(self.db.pool())
            .await
            .unwrap();
        // Natural keys live in catalog_key (ADR 0012); copy the columns.
        self.db.copy_column_keys().await.unwrap();
        id
    }

    /// Mia requests The Matrix in auto mode at Radarr `radarr`.
    async fn request_matrix(&self, radarr: i64) -> i64 {
        let movie = self
            .item(
                "INSERT INTO catalog_item (kind, tmdb_id, title, created_at, updated_at)
                 VALUES ('movie', 603, 'The Matrix', ?1, ?1) RETURNING id",
            )
            .await;
        let source = SourceTarget {
            category: Category::Movies,
            service_instance_id: radarr,
            quality_profile_id: 4,
            root_folder_path: "/movies".into(),
            minimum_availability: Some(MinimumAvailability::InCinemas),
            metadata_profile_id: None,
        };
        self.db
            .create_request(
                &NewRequest {
                    member_id: self.member,
                    catalog_item_id: movie,
                    follow: RequestFollow::No,
                    units: &[Unit {
                        catalog_item_id: movie,
                        season_number: None,
                    }],
                    mode: RequestMode::Auto,
                    source: &source,
                },
                MEMBER,
                Utc::now(),
            )
            .await
            .unwrap()
            .request_id
    }

    /// Mia requests Breaking Bad season 2 in auto mode at Sonarr `sonarr`.
    async fn request_breaking_bad(&self, sonarr: i64) -> i64 {
        let series = self
            .item(
                "INSERT INTO catalog_item (kind, tmdb_id, tvdb_id, title, created_at, updated_at)
                 VALUES ('series', 1396, 81189, 'Breaking Bad', ?1, ?1) RETURNING id",
            )
            .await;
        let season = self
            .item(&format!(
                "INSERT INTO catalog_item (kind, parent_id, season_number, title, created_at,
                     updated_at)
                 VALUES ('season', {series}, 2, 'Season 2', ?1, ?1) RETURNING id"
            ))
            .await;
        let source = SourceTarget {
            category: Category::Series,
            service_instance_id: sonarr,
            quality_profile_id: 6,
            root_folder_path: "/tv".into(),
            minimum_availability: None,
            metadata_profile_id: None,
        };
        self.db
            .create_request(
                &NewRequest {
                    member_id: self.member,
                    catalog_item_id: series,
                    follow: RequestFollow::No,
                    units: &[Unit {
                        catalog_item_id: season,
                        season_number: Some(2),
                    }],
                    mode: RequestMode::Auto,
                    source: &source,
                },
                MEMBER,
                Utc::now(),
            )
            .await
            .unwrap()
            .request_id
    }

    fn ctx(&self) -> JobContext {
        JobContext {
            db: self.db.clone(),
            payload: json!({}),
            cancel: CancellationToken::new(),
        }
    }

    async fn state(&self, request: i64) -> RequestState {
        self.db.get_request(request).await.unwrap().unwrap().state
    }

    async fn acquisition(&self) -> starr_db::requests::AcquisitionRecord {
        self.db
            .list_acquisitions(None, None)
            .await
            .unwrap()
            .pop()
            .unwrap()
    }
}

#[tokio::test]
async fn a_new_movie_is_added_with_the_category_settings_and_read_back() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let server = fake.start().await;
    let radarr = env.instance("radarr", &server.uri()).await;
    let request = env.request_matrix(radarr).await;
    assert!(
        env.jobs
            .has_work(JobKind::RequestsSubmit, &env.db)
            .await
            .unwrap()
    );

    let report = env
        .jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    assert_eq!(
        report.expectation(),
        &Expectation::Met("submitted 1 of 1".to_owned())
    );
    let writes = fake.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    let (method, path, body) = &writes[0];
    assert_eq!((method.as_str(), path.as_str()), ("POST", "/api/v3/movie"));
    assert_eq!(body["qualityProfileId"], 4);
    assert_eq!(body["rootFolderPath"], "/movies");
    assert_eq!(body["minimumAvailability"], "inCinemas");
    assert_eq!(body["addOptions"]["searchForMovie"], true);

    let acquisition = env.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Submitted);
    assert_eq!(acquisition.submit_attempts, 1);
    assert_eq!(env.state(request).await, RequestState::Submitted);
    assert!(
        !env.jobs
            .has_work(JobKind::RequestsSubmit, &env.db)
            .await
            .unwrap()
    );

    // A second run has nothing to do and writes nothing (idempotent).
    let again = env
        .jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    assert_eq!(again.expectation(), &Expectation::NotApplicable);
    assert_eq!(fake.writes().len(), 1);
}

#[tokio::test]
async fn an_existing_series_only_gets_the_season_monitored() {
    let env = env().await;
    let fake = FakeServarr::new("sonarr");
    fake.with(|s| s.title = Some(sonarr("series.json")));
    let server = fake.start().await;
    let sonarr_id = env.instance("sonarr", &server.uri()).await;
    let request = env.request_breaking_bad(sonarr_id).await;

    let report = env
        .jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    assert_eq!(report.failed(), 0, "{report:?}");
    let writes = fake.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    let (method, path, body) = &writes[0];
    assert_eq!(
        (method.as_str(), path.as_str()),
        ("PUT", "/api/v3/series/7")
    );
    // Profile and folder stay Sonarr's (D14); season 1 stays monitored.
    assert_eq!(body["qualityProfileId"], 4);
    assert_eq!(body["rootFolderPath"], "/tv/");
    let monitored: Vec<u64> = body["seasons"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["monitored"] == true)
        .map(|s| s["seasonNumber"].as_u64().unwrap())
        .collect();
    assert_eq!(monitored, vec![1, 2]);
    let commands = fake.state.lock().unwrap().commands.clone();
    assert_eq!(
        commands,
        vec![json!({ "name": "SeasonSearch", "seriesId": 7, "seasonNumber": 2 })]
    );
    assert_eq!(env.state(request).await, RequestState::Submitted);
    assert_eq!(
        env.db
            .external_ref(env.acquisition().await.title_item_id, sonarr_id)
            .await
            .unwrap()
            .unwrap()
            .remote_id,
        "7"
    );

    // Sync: season 2 has 1 of 3 aired episodes and a download running.
    fake.with(|s| {
        s.episodes = sonarr("episode.json");
        s.queue = sonarr("queue_details.json");
    });
    assert!(
        env.jobs
            .has_work(JobKind::AcquisitionsSync, &env.db)
            .await
            .unwrap()
    );
    let report = env
        .jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(
        report.expectation(),
        &Expectation::Met("synced 1 of 1".to_owned())
    );
    let acquisition = env.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::PartiallyAvailable);
    assert!(acquisition.progress.is_some());
    assert_eq!(env.state(request).await, RequestState::PartiallyAvailable);

    // With Jellyfin watching, availability comes from its inventory: all
    // three aired episodes there means available (D15).
    let jellyfin = env.instance("jellyfin", "http://jellyfin").await;
    sqlx::query(
        "INSERT INTO service_role (category, kind, service_instance_id, created_at, updated_at)
         VALUES ('series', 'consume', ?1, '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z')",
    )
    .bind(jellyfin)
    .execute(env.db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO inventory (service_instance_id, natural_key, kind, tvdb_id, remote_id, title,
             seasons, present_since, last_seen)
         VALUES (?1, 'series:tvdb:81189', 'series', 81189, 'x', 'Breaking Bad', '{\"2\": 3}',
             '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z')",
    )
    .bind(jellyfin)
    .execute(env.db.pool())
    .await
    .unwrap();
    let report = env
        .jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(report.counter("became_available"), Some(1));
    assert_eq!(env.acquisition().await.state, AcquisitionState::Available);
    assert_eq!(env.state(request).await, RequestState::Available);
    assert!(
        !env.jobs
            .has_work(JobKind::AcquisitionsSync, &env.db)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn transient_failures_are_retried_later_permanent_ones_fail_with_the_reason() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    fake.with(|s| s.fail_with = Some(503));
    let server = fake.start().await;
    let radarr = env.instance("radarr", &server.uri()).await;
    let request = env.request_matrix(radarr).await;

    let report = env
        .jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Unmet(m) if m.contains("1 will be retried"))
    );
    assert_eq!(report.items()[0].outcome, ItemOutcome::Failed);
    let pending = env.acquisition().await;
    assert_eq!(pending.state, AcquisitionState::Pending);
    assert_eq!(pending.error_code.as_deref(), Some("transient"));
    assert!(pending.next_attempt_at.is_some());
    // Not due yet: neither scheduled nor picked up.
    assert!(
        !env.jobs
            .has_work(JobKind::RequestsSubmit, &env.db)
            .await
            .unwrap()
    );
    assert_eq!(env.state(request).await, RequestState::Approved);

    // Radarr refuses for good: failed, with Radarr's reason.
    sqlx::query("UPDATE acquisition SET next_attempt_at = NULL")
        .execute(env.db.pool())
        .await
        .unwrap();
    fake.with(|s| s.fail_with = Some(400));
    env.jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    let failed = env.acquisition().await;
    assert_eq!(failed.state, AcquisitionState::Failed);
    assert_eq!(failed.error_code.as_deref(), Some("rejected"));
    assert!(
        failed.last_error.as_deref().unwrap().contains("nope"),
        "{failed:?}"
    );
    assert_eq!(env.state(request).await, RequestState::Failed);
}

#[tokio::test]
async fn sync_follows_the_queue_and_notices_a_title_leaving_the_source() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let server = fake.start().await;
    let radarr_id = env.instance("radarr", &server.uri()).await;
    let request = env.request_matrix(radarr_id).await;
    env.jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();

    fake.with(|s| s.queue = radarr("queue_details.json"));
    env.jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    let downloading = env.acquisition().await;
    assert_eq!(downloading.state, AcquisitionState::Downloading);
    assert_eq!(downloading.progress, Some(0.833_333_333_333_333_4));
    assert_eq!(
        downloading.source_message.as_deref(),
        Some("Not an upgrade for existing movie file(s)")
    );
    assert_eq!(env.state(request).await, RequestState::InProgress);

    // Someone deletes the movie in Radarr.
    fake.with(|s| s.title = None);
    let report = env
        .jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(report.failed(), 1);
    let failed = env.acquisition().await;
    assert_eq!(failed.state, AcquisitionState::Failed);
    assert_eq!(
        failed.last_error.as_deref(),
        Some("the title is no longer in radarr")
    );
    assert_eq!(env.state(request).await, RequestState::Failed);
}

/// The source's queue holds the finished download with "import blocked":
/// the admins hear about it once, however often the sync sees it.
#[tokio::test]
async fn a_blocked_import_notifies_the_admins_once() {
    use starr_core::notifications::NotificationKind;
    use starr_db::notifications::Stream;

    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let server = fake.start().await;
    let radarr_id = env.instance("radarr", &server.uri()).await;
    env.request_matrix(radarr_id).await;
    env.jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    let admin = async || {
        env.db
            .list_notifications(Stream::Admin, None, 50)
            .await
            .unwrap()
    };

    // One release still loading, one blocked: the title is still coming.
    let queue = radarr("queue_details.json");
    fake.with(|s| s.queue = queue.clone());
    env.jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(admin().await, vec![]);

    // Only the blocked, finished release is left.
    let blocked = json!([queue[1].clone()]);
    assert_eq!(blocked[0]["trackedDownloadState"], "importBlocked");
    fake.with(|s| s.queue = blocked);
    for _ in 0..2 {
        env.jobs
            .run(JobKind::AcquisitionsSync, env.ctx())
            .await
            .unwrap();
    }
    let notifications = admin().await;
    assert_eq!(notifications.len(), 1, "{notifications:?}");
    let n = &notifications[0];
    assert_eq!(n.kind, NotificationKind::ImportBlocked);
    assert_eq!(n.payload.title, "The Matrix");
    assert_eq!(
        n.payload.reason.as_deref(),
        Some("Not an upgrade for existing movie file(s)")
    );
    assert_eq!(n.payload.acquisition_id, Some(env.acquisition().await.id));
    // The member's request keeps going; members get no notification.
    assert_eq!(
        env.db
            .list_notifications(Stream::Member(env.member), None, 50)
            .await
            .unwrap(),
        vec![]
    );
}

#[tokio::test]
async fn two_members_seasons_of_one_series_are_added_in_one_write() {
    let env = env().await;
    let fake = FakeServarr::new("sonarr");
    let server = fake.start().await;
    let sonarr_id = env.instance("sonarr", &server.uri()).await;
    let noah = env
        .db
        .create_member(
            &NewMember {
                username: "noah",
                email: "noah@example.com",
                display_name: "Noah",
                password_hash: None,
            },
            &[] as &[Entitlement],
            Utc::now(),
        )
        .await
        .unwrap();
    let series = env
        .item(
            "INSERT INTO catalog_item (kind, tmdb_id, tvdb_id, title, created_at, updated_at)
             VALUES ('series', 1396, 81189, 'Breaking Bad', ?1, ?1) RETURNING id",
        )
        .await;
    let source = SourceTarget {
        category: Category::Series,
        service_instance_id: sonarr_id,
        quality_profile_id: 6,
        root_folder_path: "/tv".into(),
        minimum_availability: None,
        metadata_profile_id: None,
    };
    let mut requests = Vec::new();
    for (member, number) in [(env.member, 1), (noah, 2)] {
        let season = env
            .item(&format!(
                "INSERT INTO catalog_item (kind, parent_id, season_number, title, created_at,
                     updated_at)
                 VALUES ('season', {series}, {number}, 'Season', ?1, ?1) RETURNING id"
            ))
            .await;
        let created = env
            .db
            .create_request(
                &NewRequest {
                    member_id: member,
                    catalog_item_id: series,
                    follow: RequestFollow::No,
                    units: &[Unit {
                        catalog_item_id: season,
                        season_number: Some(number),
                    }],
                    mode: RequestMode::Auto,
                    source: &source,
                },
                MEMBER,
                Utc::now(),
            )
            .await
            .unwrap();
        requests.push(created.request_id);
    }

    let report = env
        .jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    assert_eq!(
        report.expectation(),
        &Expectation::Met("submitted 2 of 2".to_owned())
    );
    // One add for both members' seasons, never two adds of one series.
    let writes = fake.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    let (method, path, body) = &writes[0];
    assert_eq!((method.as_str(), path.as_str()), ("POST", "/api/v3/series"));
    let monitored: Vec<u64> = body["seasons"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["monitored"] == true)
        .map(|s| s["seasonNumber"].as_u64().unwrap())
        .collect();
    assert_eq!(monitored, vec![1, 2]);
    for request in requests {
        assert_eq!(env.state(request).await, RequestState::Submitted);
    }
}

#[tokio::test]
async fn units_whose_source_vanished_are_reported_not_dropped() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let server = fake.start().await;
    let radarr = env.instance("radarr", &server.uri()).await;
    env.request_matrix(radarr).await;
    env.jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    // The instance goes away: its acquisition fails and leaves the sync.
    env.db.delete_service(radarr, Utc::now()).await.unwrap();
    let report = env
        .jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(report.expectation(), &Expectation::NotApplicable);
    assert_eq!(env.acquisition().await.state, AcquisitionState::Failed);
}

/// POST and PUT requests the fake source received.
async fn source_writes(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| matches!(r.method.as_str(), "POST" | "PUT"))
        .map(|r| format!("{} {}", r.method, r.url.path()))
        .collect()
}

#[tokio::test]
async fn paused_writes_hold_the_acquisition_until_resumed() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let server = fake.start().await;
    let radarr = env.instance("radarr", &server.uri()).await;
    env.db
        .set_writes_paused(radarr, Some("root"), Utc::now())
        .await
        .unwrap();
    let request = env.request_matrix(radarr).await;
    assert!(
        !env.jobs
            .has_work(JobKind::RequestsSubmit, &env.db)
            .await
            .unwrap(),
        "nothing is due while held"
    );

    let report = env
        .jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    assert_eq!(report.expectation(), &Expectation::NotApplicable);
    assert_eq!(source_writes(&server).await, Vec::<String>::new());
    let held = env.acquisition().await;
    assert_eq!(held.state, AcquisitionState::Pending);
    assert!(held.held);
    assert_eq!(held.submit_attempts, 0);
    assert_eq!(env.state(request).await, RequestState::Approved);

    env.db
        .set_writes_paused(radarr, None, Utc::now())
        .await
        .unwrap();
    let report = env
        .jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    assert_eq!(
        report.expectation(),
        &Expectation::Met("submitted 1 of 1".to_owned())
    );
    assert_eq!(
        source_writes(&server).await,
        vec!["POST /api/v3/movie".to_owned()]
    );
    assert_eq!(env.state(request).await, RequestState::Submitted);
}

#[tokio::test]
async fn a_pause_while_the_lookup_runs_stops_the_write() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    fake.with(|s| s.lookup_delay = Some(std::time::Duration::from_millis(300)));
    let server = fake.start().await;
    let radarr = env.instance("radarr", &server.uri()).await;
    env.request_matrix(radarr).await;
    let db = env.db.clone();
    let pause = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        db.set_writes_paused(radarr, Some("root"), Utc::now())
            .await
            .unwrap();
    });
    env.jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    pause.await.unwrap();
    assert_eq!(source_writes(&server).await, Vec::<String>::new());
    let held = env.acquisition().await;
    assert!(held.held);
    assert_eq!(held.submit_attempts, 0);
}

#[tokio::test]
async fn a_pause_during_the_run_stops_the_write_and_sync_keeps_reading() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let server = fake.start().await;
    let radarr = env.instance("radarr", &server.uri()).await;
    env.request_matrix(radarr).await;
    // Loaded as due, then paused before its title is written.
    let due = env.db.due_submissions(Utc::now()).await.unwrap();
    env.db
        .set_writes_paused(radarr, Some("root"), Utc::now())
        .await
        .unwrap();
    let mut tally = requests::Tally::default();
    requests::submit_title(&env.ctx(), env.jobs.openers(), &due, &mut tally)
        .await
        .unwrap();
    assert_eq!(source_writes(&server).await, Vec::<String>::new());
    assert!(env.acquisition().await.held);

    // Submitted before the pause: `acquisitions.sync` only reads and goes on.
    env.db
        .set_writes_paused(radarr, None, Utc::now())
        .await
        .unwrap();
    env.jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    env.db
        .set_writes_paused(radarr, Some("root"), Utc::now())
        .await
        .unwrap();
    let before = source_writes(&server).await.len();
    let report = env
        .jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(report.counter("synced"), Some(1), "{report:?}");
    assert_eq!(source_writes(&server).await.len(), before);
}

/// A fake Jellyfin whose movie library holds The Matrix once `has_matrix`
/// is set; built from the recorded shapes.
#[derive(Clone, Default)]
struct FakeJellyfin {
    has_matrix: Arc<Mutex<bool>>,
}

const JELLYFIN_MOVIES: &str = "f137a2dd21bbc1b99aa5c0f6bf02a805";

fn jellyfin(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/jellyfin/fixtures/12.3.0/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

impl Respond for FakeJellyfin {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let file = match request.url.path() {
            "/System/Info" => "system_info.json",
            "/Users" => "users.json",
            "/Library/VirtualFolders" => "virtual_folders.json",
            "/Items" => {
                let movies = request
                    .url
                    .query_pairs()
                    .any(|(k, v)| k == "ParentId" && v == JELLYFIN_MOVIES);
                let items: Vec<Value> = if movies && *self.has_matrix.lock().unwrap() {
                    let all: Vec<Value> =
                        serde_json::from_str(&jellyfin("items_movies_series.json")).unwrap();
                    all.into_iter()
                        .filter(|i| i["Name"] == "The Matrix")
                        .collect()
                } else {
                    Vec::new()
                };
                let total = items.len();
                return ResponseTemplate::new(200)
                    .set_body_json(json!({ "Items": items, "TotalRecordCount": total }));
            }
            _ => return ResponseTemplate::new(404),
        };
        ResponseTemplate::new(200).set_body_string(jellyfin(file))
    }
}

/// Pending jobs of `kind`: their `run_after`.
async fn pending(db: &Database, kind: JobKind) -> Vec<chrono::DateTime<Utc>> {
    let rows: Vec<(chrono::DateTime<Utc>,)> =
        sqlx::query_as("SELECT run_after FROM job WHERE kind = ? AND lease_owner IS NULL")
            .bind(kind.name())
            .fetch_all(db.pool())
            .await
            .unwrap();
    rows.into_iter().map(|(at,)| at).collect()
}

/// Claim and run the pending job of `kind` the way the worker does, so the
/// run is recorded.
async fn run_pending(env: &Env, kind: JobKind) {
    let later = Utc::now() + chrono::TimeDelta::hours(2);
    let job = env
        .db
        .claim_job(
            "test",
            &[kind.name()],
            later,
            std::time::Duration::from_mins(1),
        )
        .await
        .unwrap()
        .unwrap();
    env.db.start_run(&job, Utc::now()).await.unwrap();
    env.jobs.run(kind, env.ctx()).await.unwrap();
    env.db.complete_job(job.id, "test").await.unwrap();
}

/// A fake Jellyfin the movies category watches in (keep the server alive).
async fn watch_movies_in_jellyfin(env: &Env) -> (FakeJellyfin, MockServer) {
    let fake = FakeJellyfin::default();
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(fake.clone())
        .mount(&server)
        .await;
    let id = env.instance("jellyfin", &server.uri()).await;
    env.db
        .add_role(
            &starr_db::categories::NewRole {
                category: Category::Movies,
                kind: starr_core::categories::RoleKind::Consume,
                service_instance_id: id,
                offered: &[starr_core::adapters::Capability::Libraries],
            },
            Utc::now(),
        )
        .await
        .unwrap();
    (fake, server)
}

/// Maintainer's acceptance test: Radarr imported the movie long ago, but
/// Starr showed "Downloading 100%" until the hourly inventory run.
#[tokio::test]
async fn imported_but_not_in_jellyfin_waits_for_the_library_and_refreshes_it_early() {
    use chrono::TimeDelta;
    use starr_core::requests::DownloadStage;

    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let server = fake.start().await;
    let radarr_id = env.instance("radarr", &server.uri()).await;
    let (jellyfin_fake, _jellyfin_server) = watch_movies_in_jellyfin(&env).await;
    let request = env.request_matrix(radarr_id).await;
    env.jobs
        .run(JobKind::RequestsSubmit, env.ctx())
        .await
        .unwrap();
    // Imported: the file is there, the queue is empty.
    fake.with(|s| {
        if let Some(title) = s.title.as_mut() {
            title["hasFile"] = json!(true);
        }
    });

    let started = Utc::now();
    let report = env
        .jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(report.counter("arriving"), Some(1), "{report:?}");
    let arriving = env.acquisition().await;
    assert_eq!(arriving.state, AcquisitionState::Downloading);
    assert_eq!(arriving.stage, Some(DownloadStage::Arriving));
    assert_eq!(arriving.library_name.as_deref(), Some("jellyfin"));
    assert_eq!(env.state(request).await, RequestState::InProgress);
    // No inventory run yet: one is due right away.
    let due = pending(&env.db, JobKind::InventorySync).await;
    assert_eq!(due.len(), 1);
    assert!(due[0] <= Utc::now() && due[0] >= started - TimeDelta::seconds(1));

    // It runs; Jellyfin has not scanned the file yet.
    run_pending(&env, JobKind::InventorySync).await;
    let ran = Utc::now();
    env.jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(env.acquisition().await.stage, Some(DownloadStage::Arriving));
    // Asked again, but not before the minimum interval after that run.
    let due = pending(&env.db, JobKind::InventorySync).await;
    assert_eq!(due.len(), 1, "at most one pending");
    assert!(due[0] >= ran + TimeDelta::minutes(4), "{due:?}");
    env.jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(pending(&env.db, JobKind::InventorySync).await, due);

    // Arriving for longer than the window: Jellyfin may never list it, the
    // hourly schedule takes over instead of a walk every five minutes.
    sqlx::query(
        "UPDATE acquisition SET download_stage_since = ?;
         DELETE FROM job WHERE kind = 'inventory.sync';",
    )
    .bind(
        (Utc::now() - TimeDelta::hours(2))
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string(),
    )
    .execute(env.db.pool())
    .await
    .unwrap();
    let report = env
        .jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(report.counter("arriving"), Some(1));
    assert_eq!(
        pending(&env.db, JobKind::InventorySync).await,
        Vec::<chrono::DateTime<Utc>>::new()
    );

    // Jellyfin has it now: the next runs make it available and tell Mia.
    *jellyfin_fake.has_matrix.lock().unwrap() = true;
    env.jobs
        .run(JobKind::InventorySync, env.ctx())
        .await
        .unwrap();
    let report = env
        .jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(report.counter("became_available"), Some(1));
    assert_eq!(report.counter("arriving"), Some(0));
    let available = env.acquisition().await;
    assert_eq!(available.state, AcquisitionState::Available);
    assert_eq!(available.stage, None);
    assert_eq!(env.state(request).await, RequestState::Available);
    let notifications = env
        .db
        .list_notifications(
            starr_db::notifications::Stream::Member(env.member),
            None,
            10,
        )
        .await
        .unwrap();
    assert_eq!(
        notifications[0].kind,
        starr_core::notifications::NotificationKind::RequestAvailable
    );
}

#[path = "removal_tests.rs"]
mod removal;
