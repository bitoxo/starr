//! Generic contract test suite for [`Source`] adapters (ADR 0009, lessons
//! 8–10, 12).
//!
//! The adapter's fixtures are served by a small **stateful** fake of a
//! Servarr-style library (collection at `library_path`, items at
//! `library_path/{id}`, filtered by the natural key as a query parameter).
//! The fake is stricter than the real service: it answers only requests
//! with the right credential, rejects adds that lack required fields or name
//! another key, and counts every write so each case can assert exactly what
//! was sent.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use secrecy::SecretString;
use serde_json::{Value, json};
use starr_core::adapters::{AdapterDescriptor, AdapterError, Listing};
use starr_core::source::{
    AddOutcome, AddRequest, Deviation, Files, Lookup, MonitorScope, NaturalKey, QualityProfile,
    QueueEntry, RootFolder, Source, UnitId,
};
use url::Url;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::RecordedResponse;
use crate::{AuthScheme, Endpoint, ServiceClient};

const KEY: &str = "contract-test-key-0123456789abcdef";
/// The quality profile id the fake stores instead of the requested one in
/// the read-back mismatch case.
const MANGLED_PROFILE: i64 = 999;

/// A `GET` endpoint the adapter reads, with a recorded body.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// Path relative to the base URL, e.g. `/api/v3/qualityprofile`.
    pub path: &'static str,
    /// Required query parameter, if any (`("movieId", "1")`).
    pub query: Option<(&'static str, String)>,
    pub body: String,
}

/// Everything the suite needs to know about one `Source` adapter.
#[derive(Debug)]
pub struct SourceContract<A> {
    pub descriptor: &'static AdapterDescriptor,
    pub auth: AuthScheme,
    pub build: fn(ServiceClient) -> A,
    /// `system/status` of a supported version (writes allowed).
    pub status_supported: Fixture,
    /// `system/status` of a version outside the tested range.
    pub status_unsupported: Fixture,
    /// Library collection path, e.g. `/api/v3/movie`.
    pub library_path: &'static str,
    /// Query parameter and JSON field holding the natural key (`tmdbId`).
    pub key_field: &'static str,
    pub key: NaturalKey,
    /// The item as it is in the library before Starr touches it (JSON
    /// object with `id`), monitored only partly so that
    /// `extend_scope` has something to add.
    pub existing: String,
    /// The service's metadata lookup for `key` (body for the add).
    pub metadata: Fixture,
    pub add: AddRequest,
    /// Fields an add must carry; the fake rejects adds without them.
    pub required_add_fields: &'static [&'static str],
    /// What the service answers to an add of an item it already has.
    pub duplicate: RecordedResponse,
    /// Also the scope of `stop_monitoring`: what extending turned on,
    /// stopping turns off again, and nothing else.
    pub extend_scope: MonitorScope,
    pub profiles: (Fixture, Vec<QualityProfile>),
    pub root_folders: (Fixture, Vec<RootFolder>),
    /// Endpoints `progress` reads besides the library (queue, episodes).
    pub progress_fixtures: Vec<Fixture>,
    pub expected_files: Files,
    pub expected_queue: Vec<QueueEntry>,
}

#[derive(Debug, Default)]
struct State {
    item: Option<Value>,
    /// The library filter pretends the item is absent although it exists
    /// (someone else added it between our lookup and our add).
    hidden: bool,
    /// Store adds with a wrong profile and no monitored seasons.
    mangle: bool,
    /// Reject every add with a validation error (nothing stored).
    reject_adds: bool,
    /// Answer this many commands with 503 before accepting them.
    failing_commands: u32,
    posts: Vec<Value>,
    puts: Vec<Value>,
    commands: Vec<Value>,
}

type Shared = Arc<Mutex<State>>;

fn lock(state: &Shared) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn json_response(status: u16, body: &Value) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .insert_header("content-type", "application/json")
        .set_body_string(body.to_string())
}

fn key_value(key: NaturalKey) -> u32 {
    match key {
        NaturalKey::TmdbMovie(id) | NaturalKey::TvdbSeries(id) => id,
        // Test harness (feature `contract`): a music key here is a mistake in
        // the calling test, not a runtime case.
        NaturalKey::MbArtist(_) | NaturalKey::StashScene(_) => {
            panic!("this suite fakes Radarr and Sonarr, keyed by numbers")
        }
    }
}

struct LibraryFilter(Shared);

impl Respond for LibraryFilter {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let state = lock(&self.0);
        match (&state.item, state.hidden) {
            (Some(item), false) => json_response(200, &json!([item])),
            _ => json_response(200, &json!([])),
        }
    }
}

struct ItemById(Shared);

impl Respond for ItemById {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        match &lock(&self.0).item {
            Some(item) => json_response(200, item),
            None => ResponseTemplate::new(404),
        }
    }
}

struct Add {
    state: Shared,
    id: i64,
    key_field: &'static str,
    key: u32,
    required: &'static [&'static str],
    duplicate: RecordedResponse,
}

impl Respond for Add {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let Ok(Value::Object(mut body)) = serde_json::from_slice::<Value>(&request.body) else {
            return json_response(
                400,
                &json!([{ "errorMessage": "body is not a JSON object" }]),
            );
        };
        let mut state = lock(&self.state);
        state.posts.push(Value::Object(body.clone()));
        if state.reject_adds {
            return json_response(
                400,
                &json!([{ "propertyName": "RootFolderPath", "errorMessage": "Root folder path does not exist" }]),
            );
        }
        let missing: Vec<_> = self
            .required
            .iter()
            .filter(|f| body.get(**f).is_none_or(Value::is_null))
            .collect();
        if !missing.is_empty() {
            return json_response(
                400,
                &json!([{ "errorMessage": format!("missing {missing:?}") }]),
            );
        }
        if body.get(self.key_field).and_then(Value::as_u64) != Some(u64::from(self.key)) {
            return json_response(400, &json!([{ "errorMessage": "wrong key" }]));
        }
        if state.item.is_some() {
            state.hidden = false;
            return ResponseTemplate::new(self.duplicate.status)
                .insert_header("content-type", "application/json")
                .set_body_string(&self.duplicate.body);
        }
        body.insert("id".into(), json!(self.id));
        // The reply echoes the request; what is stored may differ (lesson 9).
        let reply = Value::Object(body.clone());
        if state.mangle {
            body.insert("qualityProfileId".into(), json!(MANGLED_PROFILE));
            if let Some(seasons) = body.get_mut("seasons").and_then(Value::as_array_mut) {
                for season in seasons.iter_mut().filter_map(Value::as_object_mut) {
                    season.insert("monitored".into(), json!(false));
                }
            }
        }
        state.item = Some(Value::Object(body));
        // Slow enough that an unserialized second add would overlap.
        json_response(201, &reply).set_delay(Duration::from_millis(150))
    }
}

struct Update(Shared);

impl Respond for Update {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let Ok(body) = serde_json::from_slice::<Value>(&request.body) else {
            return ResponseTemplate::new(400);
        };
        let mut state = lock(&self.0);
        state.puts.push(body.clone());
        state.item = Some(body.clone());
        json_response(202, &body)
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
        json_response(201, &json!({ "id": 1, "status": "queued" }))
    }
}

/// A running fake plus its state.
struct Fake {
    server: MockServer,
    state: Shared,
}

impl<A: Source> SourceContract<A> {
    /// Header name and value that carry the right credential.
    fn auth_header(&self) -> (&'static str, String) {
        super::auth_header(self.auth, KEY)
    }

    fn auth_matcher(&self) -> wiremock::matchers::HeaderExactMatcher {
        let (name, value) = self.auth_header();
        header(name, value.as_str())
    }

    fn existing(&self) -> Value {
        match serde_json::from_str(&self.existing) {
            Ok(value @ Value::Object(_)) => value,
            other => panic!("`existing` must be a JSON object: {other:?}"),
        }
    }

    fn existing_id(&self) -> i64 {
        self.existing()
            .get("id")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("`existing` has no numeric id"))
    }

    fn adapter(&self, client: ServiceClient) -> A {
        (self.build)(client)
    }

    fn client(&self, server: &MockServer) -> ServiceClient {
        let base_url = Url::parse(&server.uri()).unwrap_or_else(|e| panic!("{e}"));
        ServiceClient::new(
            self.descriptor,
            Endpoint {
                base_url,
                auth: self.auth,
                credential: SecretString::from(KEY.to_owned()),
            },
        )
        .unwrap_or_else(|e| panic!("cannot build client: {e}"))
    }

    async fn mount_get(&self, server: &MockServer, fixture: &Fixture) {
        let mut mock = Mock::given(method("GET"))
            .and(path(fixture.path))
            .and(self.auth_matcher());
        if let Some((name, value)) = &fixture.query {
            mock = mock.and(query_param(*name, value.as_str()));
        }
        mock.respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(&fixture.body),
        )
        .mount(server)
        .await;
    }

    /// A fake with `item` in the library (or none) and `status` as version.
    async fn fake(&self, item: Option<Value>, status: &Fixture) -> Fake {
        let server = MockServer::start().await;
        let state: Shared = Arc::new(Mutex::new(State {
            item,
            ..State::default()
        }));
        let key = key_value(self.key).to_string();
        let item_path = format!("{}/{}", self.library_path, self.existing_id());
        let auth = || self.auth_matcher();

        self.mount_get(&server, status).await;
        self.mount_get(&server, &self.metadata).await;
        self.mount_get(&server, &self.profiles.0).await;
        self.mount_get(&server, &self.root_folders.0).await;
        for fixture in &self.progress_fixtures {
            self.mount_get(&server, fixture).await;
        }
        Mock::given(method("GET"))
            .and(path(self.library_path))
            .and(query_param(self.key_field, key.as_str()))
            .and(auth())
            .respond_with(LibraryFilter(Arc::clone(&state)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(item_path.as_str()))
            .and(auth())
            .respond_with(ItemById(Arc::clone(&state)))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(self.library_path))
            .and(header("content-type", "application/json"))
            .and(auth())
            .respond_with(Add {
                state: Arc::clone(&state),
                id: self.existing_id(),
                key_field: self.key_field,
                key: key_value(self.key),
                required: self.required_add_fields,
                duplicate: self.duplicate.clone(),
            })
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path(item_path.as_str()))
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
        Fake { server, state }
    }
}

/// Run every source contract case. Panics with a description on failure.
pub async fn run_source_contract<A: Source>(c: &SourceContract<A>) {
    lists_profiles_and_root_folders(c).await;
    lookup_absent_and_present(c).await;
    add_writes_once_and_reads_back(c).await;
    add_reports_read_back_mismatch(c).await;
    concurrent_adds_post_once(c).await;
    duplicate_answer_means_already_exists(c).await;
    add_of_an_existing_item_changes_nothing(c).await;
    extend_monitoring_only_adds(c).await;
    stop_monitoring_turns_off_only_the_scope(c).await;
    a_failed_search_is_repeated_on_retry(c).await;
    a_rejected_add_reports_the_reason(c).await;
    progress_maps_queue_and_files(c).await;
    absent_items_are_not_found(c).await;
    failures_follow_the_taxonomy(c).await;
    unsupported_version_is_read_only(c).await;
}

async fn lists_profiles_and_root_folders<A: Source>(c: &SourceContract<A>) {
    let fake = c.fake(None, &c.status_supported).await;
    let adapter = c.adapter(c.client(&fake.server));
    assert_eq!(
        adapter.quality_profiles().await,
        Ok(Listing {
            items: c.profiles.1.clone(),
            complete: true
        })
    );
    assert_eq!(
        adapter.root_folders().await,
        Ok(Listing {
            items: c.root_folders.1.clone(),
            complete: true
        })
    );
}

async fn lookup_absent_and_present<A: Source>(c: &SourceContract<A>) {
    let fake = c.fake(None, &c.status_supported).await;
    let adapter = c.adapter(c.client(&fake.server));
    assert_eq!(adapter.lookup(c.key).await, Ok(Lookup::NotInLibrary));

    let fake = c.fake(Some(c.existing()), &c.status_supported).await;
    let adapter = c.adapter(c.client(&fake.server));
    match adapter.lookup(c.key).await {
        Ok(Lookup::InLibrary(item)) => {
            assert_eq!(item.key, c.key);
            assert_eq!(item.remote_id, c.existing_id());
        }
        other => panic!("expected the existing item, got {other:?}"),
    }
}

async fn add_writes_once_and_reads_back<A: Source>(c: &SourceContract<A>) {
    let fake = c.fake(None, &c.status_supported).await;
    let adapter = c.adapter(c.client(&fake.server));
    let outcome = adapter.add(&c.add).await;
    match &outcome {
        Ok(AddOutcome::Added(report)) => {
            assert!(report.written);
            assert_eq!(report.deviations, vec![], "exact add must not deviate");
            assert_eq!(report.item.key, c.key);
            assert_eq!(report.item.quality_profile_id, c.add.quality_profile_id);
        }
        other => panic!("expected Added, got {other:?}"),
    }
    let state = lock(&fake.state);
    assert_eq!(state.posts.len(), 1, "exactly one add");
    let post = &state.posts[0];
    assert_eq!(post["qualityProfileId"], json!(c.add.quality_profile_id));
    assert_eq!(post["rootFolderPath"], json!(c.add.root_folder_path));
    assert_eq!(post["monitored"], json!(true));
    if let MonitorScope::Units(wanted) = &c.add.scope {
        for season in post["seasons"].as_array().into_iter().flatten() {
            let number = season["seasonNumber"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok());
            let expected = number.is_some_and(|n| wanted.contains(&UnitId::Season(n)));
            assert_eq!(season["monitored"], json!(expected), "season {number:?}");
        }
    }
}

async fn add_reports_read_back_mismatch<A: Source>(c: &SourceContract<A>) {
    let fake = c.fake(None, &c.status_supported).await;
    lock(&fake.state).mangle = true;
    let adapter = c.adapter(c.client(&fake.server));
    let Ok(AddOutcome::Added(report)) = adapter.add(&c.add).await else {
        panic!("expected Added");
    };
    assert!(
        report.deviations.contains(&Deviation::QualityProfile {
            expected: c.add.quality_profile_id,
            found: MANGLED_PROFILE
        }),
        "{:?}",
        report.deviations
    );
    if let MonitorScope::Units(wanted) = &c.add.scope {
        for &unit in wanted {
            assert!(
                report
                    .deviations
                    .contains(&Deviation::UnitNotMonitored(unit)),
                "{unit}: {:?}",
                report.deviations
            );
        }
    }
}

async fn concurrent_adds_post_once<A: Source>(c: &SourceContract<A>) {
    let fake = c.fake(None, &c.status_supported).await;
    let client = c.client(&fake.server);
    // Two jobs, two adapter instances, one service instance.
    let (first, second) = (c.adapter(client.clone()), c.adapter(client));
    let (a, b) = tokio::join!(first.add(&c.add), second.add(&c.add));
    let added = [&a, &b]
        .iter()
        .filter(|r| matches!(r, Ok(AddOutcome::Added(_))))
        .count();
    let existed = [&a, &b]
        .iter()
        .filter(|r| matches!(r, Ok(AddOutcome::AlreadyExists(_))))
        .count();
    assert_eq!((added, existed), (1, 1), "{a:?} / {b:?}");
    assert_eq!(lock(&fake.state).posts.len(), 1, "adds must be serialized");
}

async fn duplicate_answer_means_already_exists<A: Source>(c: &SourceContract<A>) {
    // Someone else added it between our lookup and our add.
    let fake = c.fake(Some(c.existing()), &c.status_supported).await;
    lock(&fake.state).hidden = true;
    let adapter = c.adapter(c.client(&fake.server));
    match adapter.add(&c.add).await {
        Ok(AddOutcome::AlreadyExists(item)) => assert_eq!(item.remote_id, c.existing_id()),
        other => panic!(
            "expected AlreadyExists after HTTP {}, got {other:?}",
            c.duplicate.status
        ),
    }
    let state = lock(&fake.state);
    assert_eq!(state.posts.len(), 1);
    assert!(state.puts.is_empty(), "an existing item is left untouched");
}

async fn add_of_an_existing_item_changes_nothing<A: Source>(c: &SourceContract<A>) {
    let fake = c.fake(Some(c.existing()), &c.status_supported).await;
    let adapter = c.adapter(c.client(&fake.server));
    assert!(matches!(
        adapter.add(&c.add).await,
        Ok(AddOutcome::AlreadyExists(_))
    ));
    let state = lock(&fake.state);
    assert!(state.posts.is_empty() && state.puts.is_empty() && state.commands.is_empty());
}

/// The resource with every `monitored` flag (item and seasons) cleared, to
/// compare everything else.
fn without_monitoring(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.remove("monitored");
        if let Some(seasons) = object.get_mut("seasons").and_then(Value::as_array_mut) {
            for season in seasons.iter_mut().filter_map(Value::as_object_mut) {
                season.remove("monitored");
            }
        }
    }
    value
}

fn monitored_flags(value: &Value) -> Vec<(Option<u64>, bool)> {
    let mut flags = vec![(None, value["monitored"] == json!(true))];
    for season in value["seasons"].as_array().into_iter().flatten() {
        flags.push((
            season["seasonNumber"].as_u64(),
            season["monitored"] == json!(true),
        ));
    }
    flags
}

async fn extend_monitoring_only_adds<A: Source>(c: &SourceContract<A>) {
    let before = c.existing();
    let fake = c.fake(Some(before.clone()), &c.status_supported).await;
    let adapter = c.adapter(c.client(&fake.server));
    let report = adapter
        .extend_monitoring(c.key, &c.extend_scope, true)
        .await
        .unwrap_or_else(|e| panic!("extend failed: {e}"));
    assert!(report.written);
    assert_eq!(report.deviations, vec![]);
    {
        let state = lock(&fake.state);
        assert_eq!(state.puts.len(), 1, "one read-modify-write");
        let put = &state.puts[0];
        assert_eq!(
            without_monitoring(put.clone()),
            without_monitoring(before.clone()),
            "only monitored flags may change"
        );
        for ((season, was), (_, now)) in monitored_flags(&before)
            .into_iter()
            .zip(monitored_flags(put))
        {
            assert!(!was || now, "season {season:?} was unmonitored");
        }
        assert!(!state.commands.is_empty(), "search was asked for");
    }
    // Asking again writes nothing.
    let again = adapter
        .extend_monitoring(c.key, &c.extend_scope, true)
        .await
        .unwrap_or_else(|e| panic!("second extend failed: {e}"));
    assert!(!again.written);
    assert_eq!(lock(&fake.state).puts.len(), 1);
}

async fn stop_monitoring_turns_off_only_the_scope<A: Source>(c: &SourceContract<A>) {
    let before = c.existing();
    let fake = c.fake(Some(before.clone()), &c.status_supported).await;
    let adapter = c.adapter(c.client(&fake.server));
    adapter
        .extend_monitoring(c.key, &c.extend_scope, false)
        .await
        .unwrap_or_else(|e| panic!("extend failed: {e}"));
    let report = adapter
        .stop_monitoring(c.key, &c.extend_scope)
        .await
        .unwrap_or_else(|e| panic!("stop failed: {e}"));
    assert!(report.written);
    assert_eq!(report.deviations, vec![], "exact stop must not deviate");
    {
        let state = lock(&fake.state);
        assert_eq!(state.puts.len(), 2, "one read-modify-write for the stop");
        assert_eq!(
            state.puts[1], before,
            "stopping must turn off exactly what extending turned on"
        );
        assert!(state.posts.is_empty(), "nothing added");
        assert!(state.commands.is_empty(), "a stop searches nothing");
    }
    // Stopping again writes nothing.
    let again = adapter
        .stop_monitoring(c.key, &c.extend_scope)
        .await
        .unwrap_or_else(|e| panic!("second stop failed: {e}"));
    assert!(!again.written);
    assert_eq!(again.deviations, vec![]);
    assert_eq!(lock(&fake.state).puts.len(), 2);
}

async fn progress_maps_queue_and_files<A: Source>(c: &SourceContract<A>) {
    let fake = c.fake(Some(c.existing()), &c.status_supported).await;
    let adapter = c.adapter(c.client(&fake.server));
    let progress = adapter
        .progress(c.key)
        .await
        .unwrap_or_else(|e| panic!("progress failed: {e}"));
    assert_eq!(progress.item.remote_id, c.existing_id());
    assert_eq!(progress.files, c.expected_files);
    assert_eq!(
        progress.queue,
        Listing {
            items: c.expected_queue.clone(),
            complete: true
        }
    );
}

async fn absent_items_are_not_found<A: Source>(c: &SourceContract<A>) {
    let fake = c.fake(None, &c.status_supported).await;
    let adapter = c.adapter(c.client(&fake.server));
    assert!(matches!(
        adapter.progress(c.key).await,
        Err(AdapterError::NotFound(_))
    ));
    assert!(matches!(
        adapter
            .extend_monitoring(c.key, &c.extend_scope, false)
            .await,
        Err(AdapterError::NotFound(_))
    ));
    assert!(matches!(
        adapter.stop_monitoring(c.key, &c.extend_scope).await,
        Err(AdapterError::NotFound(_))
    ));
    assert_eq!(lock(&fake.state).puts, Vec::<Value>::new());
}

async fn failures_follow_the_taxonomy<A: Source>(c: &SourceContract<A>) {
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
        let adapter = c.adapter(c.client(&server));
        let result = adapter.lookup(c.key).await;
        assert!(
            result.as_ref().err().is_some_and(expected),
            "lookup: {result:?}"
        );
    }

    // A server error on the add itself stays transient: it is not mistaken
    // for "already exists".
    let fake = c.fake(None, &c.status_supported).await;
    Mock::given(method("POST"))
        .and(path(c.library_path))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(1)
        .mount(&fake.server)
        .await;
    let adapter = c.adapter(c.client(&fake.server));
    let result = adapter.add(&c.add).await;
    assert!(
        matches!(result, Err(AdapterError::Transient(_))),
        "{result:?}"
    );
}

async fn unsupported_version_is_read_only<A: Source>(c: &SourceContract<A>) {
    let fake = c.fake(Some(c.existing()), &c.status_unsupported).await;
    let adapter = c.adapter(c.client(&fake.server));
    assert!(matches!(
        adapter.lookup(c.key).await,
        Ok(Lookup::InLibrary(_))
    ));
    assert!(adapter.progress(c.key).await.is_ok(), "reads keep working");
    assert!(matches!(
        adapter
            .extend_monitoring(c.key, &c.extend_scope, true)
            .await,
        Err(AdapterError::VersionUnsupported { .. })
    ));
    assert!(matches!(
        adapter.stop_monitoring(c.key, &c.extend_scope).await,
        Err(AdapterError::VersionUnsupported { .. })
    ));
    let fake_empty = c.fake(None, &c.status_unsupported).await;
    let adapter_empty = c.adapter(c.client(&fake_empty.server));
    assert!(matches!(
        adapter_empty.add(&c.add).await,
        Err(AdapterError::VersionUnsupported { .. })
    ));
    for state in [lock(&fake.state), lock(&fake_empty.state)] {
        assert!(state.posts.is_empty() && state.puts.is_empty() && state.commands.is_empty());
    }
}

async fn a_failed_search_is_repeated_on_retry<A: Source>(c: &SourceContract<A>) {
    let fake = c.fake(Some(c.existing()), &c.status_supported).await;
    lock(&fake.state).failing_commands = 1;
    let adapter = c.adapter(c.client(&fake.server));
    let first = adapter
        .extend_monitoring(c.key, &c.extend_scope, true)
        .await;
    assert!(
        matches!(first, Err(AdapterError::Transient(_))),
        "{first:?}"
    );
    // The job retries: the monitoring is already there, the search is not.
    let retry = adapter
        .extend_monitoring(c.key, &c.extend_scope, true)
        .await
        .unwrap_or_else(|e| panic!("retry failed: {e}"));
    assert!(!retry.written);
    let state = lock(&fake.state);
    assert_eq!(state.puts.len(), 1);
    assert!(!state.commands.is_empty(), "the search was lost");
}

async fn a_rejected_add_reports_the_reason<A: Source>(c: &SourceContract<A>) {
    let fake = c.fake(None, &c.status_supported).await;
    lock(&fake.state).reject_adds = true;
    let adapter = c.adapter(c.client(&fake.server));
    match adapter.add(&c.add).await {
        Err(AdapterError::Rejected(reason)) => {
            assert!(
                reason.contains("Root folder path does not exist"),
                "{reason}"
            );
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    assert_eq!(
        lock(&fake.state).posts.len(),
        1,
        "no retry of a rejected add"
    );
}
