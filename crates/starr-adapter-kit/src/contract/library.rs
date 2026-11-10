//! Generic contract test suite for [`Library`] adapters (ADR 0009, lessons
//! 8, 10, 12).
//!
//! The adapter's fixtures are served by a fake that is stricter than the
//! real service: it answers only requests with the right credential, rejects
//! item queries that are not recursive, not scoped to the library, without
//! the library user or (for movies and series) without provider IDs, and it
//! returns at most [`PAGE_CAP`] items per page whatever `Limit` asks for, so
//! the adapter has to page by what it received. Modes make it drop the
//! total, change the total between pages, or fail.
//!
//! Known limit (documented in `capabilities.md`): offset paging cannot
//! detect one removal plus one addition between two pages (the count still
//! matches). The inventory planner's "two complete runs" rule covers it.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use secrecy::SecretString;
use serde_json::{Value, json};
use starr_core::adapters::{AdapterDescriptor, AdapterError};
use starr_core::library::{Library, MediaItem, MediaLibrary};
use url::Url;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::RecordedResponse;
use crate::{AuthScheme, Endpoint, ServiceClient};

const KEY: &str = "contract-test-key-0123456789abcdef";
/// Most items the fake returns per page.
pub const PAGE_CAP: usize = 2;

/// A `GET` endpoint the adapter reads, with a recorded body.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// Path relative to the base URL, e.g. `/System/Info`.
    pub path: String,
    pub body: String,
}

/// Everything the suite needs to know about one `Library` adapter.
#[derive(Debug)]
pub struct LibraryContract<A> {
    pub descriptor: &'static AdapterDescriptor,
    pub auth: AuthScheme,
    pub build: fn(ServiceClient) -> A,
    /// System info of a supported version (server ID source).
    pub info: Fixture,
    pub expected_server_id: String,
    /// What the *other* product answers at the same path (Jellyfin for an
    /// Emby adapter and vice versa): the adapter must refuse it.
    pub other_product_info: String,
    pub users: Fixture,
    pub folders: Fixture,
    pub expected_libraries: Vec<MediaLibrary>,
    pub library_id: String,
    /// Item query path for the library user, e.g. `/Items`.
    pub items_path: String,
    /// Query parameter carrying the library user, if the path does not.
    pub user_query: Option<(&'static str, String)>,
    /// JSON array of the library's movies and series.
    pub primary_items: String,
    /// JSON array of the library's episodes.
    pub episodes: String,
    /// What `items` must return for the fixtures (in fixture order).
    pub expected_items: Vec<MediaItem>,
    /// What the service answers to a wrong credential.
    pub unauthorized: RecordedResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    /// Pages carry no `TotalRecordCount`.
    NoTotal,
    /// The total grows by one after the first page.
    TotalChanges,
    /// Pages after the first answer with this status.
    FailAfterFirst(u16),
}

struct Items {
    primary: Vec<Value>,
    episodes: Vec<Value>,
    library_id: String,
    user_query: Option<(&'static str, String)>,
    mode: Arc<Mutex<Mode>>,
}

/// Query parameter by name, ignoring case (both services bind that way).
fn param(request: &Request, name: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.into_owned())
}

fn bad_request(reason: &str) -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_string(format!("contract fake: {reason}"))
}

impl Respond for Items {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mode = *self.mode.lock().unwrap_or_else(PoisonError::into_inner);
        if param(request, "ParentId").as_deref() != Some(self.library_id.as_str()) {
            return bad_request("ParentId must be the library");
        }
        if !param(request, "Recursive").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return bad_request("Recursive=true is required");
        }
        if let Some((name, value)) = &self.user_query
            && param(request, name).as_deref() != Some(value.as_str())
        {
            return bad_request("the library user is required");
        }
        let types = param(request, "IncludeItemTypes").unwrap_or_default();
        let types: Vec<&str> = types.split(',').map(str::trim).collect();
        let source = if types == ["Episode"] {
            &self.episodes
        } else {
            let mut sorted = types.clone();
            sorted.sort_unstable();
            if sorted != ["Movie", "Series"] {
                return bad_request("IncludeItemTypes must be Movie,Series or Episode");
            }
            let fields = param(request, "Fields").unwrap_or_default();
            if !fields.split(',').any(|f| f.trim() == "ProviderIds") {
                return bad_request("Fields must include ProviderIds");
            }
            &self.primary
        };
        let start: usize = param(request, "StartIndex")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let limit: usize = param(request, "Limit")
            .and_then(|v| v.parse().ok())
            .unwrap_or(usize::MAX);
        if start > 0
            && let Mode::FailAfterFirst(status) = mode
        {
            return ResponseTemplate::new(status)
                .insert_header("retry-after", "3")
                .set_body_string("failing on purpose");
        }
        let page: Vec<Value> = source
            .iter()
            .skip(start)
            .take(limit.min(PAGE_CAP))
            .cloned()
            .collect();
        let mut total = source.len();
        if mode == Mode::TotalChanges && start > 0 {
            total += 1;
        }
        let body = if mode == Mode::NoTotal {
            json!({ "Items": page })
        } else {
            json!({ "Items": page, "TotalRecordCount": total, "StartIndex": start })
        };
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_string(body.to_string())
    }
}

fn json_body(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "application/json")
        .set_body_string(body)
}

fn array(name: &str, body: &str) -> Vec<Value> {
    match serde_json::from_str(body) {
        Ok(Value::Array(items)) => items,
        other => panic!("{name} must be a JSON array, got {other:?}"),
    }
}

struct Fake {
    server: MockServer,
    mode: Arc<Mutex<Mode>>,
}

impl Fake {
    fn set(&self, mode: Mode) {
        *self.mode.lock().unwrap_or_else(PoisonError::into_inner) = mode;
    }
}

impl<A: Library> LibraryContract<A> {
    fn auth_header(&self) -> (&'static str, String) {
        super::auth_header(self.auth, KEY)
    }

    async fn fake(&self, info_body: &str) -> Fake {
        let server = MockServer::start().await;
        let (name, value) = self.auth_header();
        let mode = Arc::new(Mutex::new(Mode::Normal));
        for (route, body) in [
            (&self.info.path, info_body),
            (&self.users.path, self.users.body.as_str()),
            (&self.folders.path, self.folders.body.as_str()),
        ] {
            Mock::given(method("GET"))
                .and(path(route.as_str()))
                .and(header(name, value.as_str()))
                .respond_with(json_body(body))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path(self.items_path.as_str()))
            .and(header(name, value.as_str()))
            .respond_with(Items {
                primary: array("primary_items", &self.primary_items),
                episodes: array("episodes", &self.episodes),
                library_id: self.library_id.clone(),
                user_query: self.user_query.clone(),
                mode: Arc::clone(&mode),
            })
            .mount(&server)
            .await;
        // Anything else, or without the right key, is what the service
        // answers to a wrong credential.
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(self.unauthorized.status)
                    .set_body_string(&self.unauthorized.body),
            )
            .mount(&server)
            .await;
        Fake { server, mode }
    }

    fn adapter(&self, server: &MockServer, key: &str) -> A {
        let base_url = match Url::parse(&server.uri()) {
            Ok(url) => url,
            Err(err) => panic!("invalid fake URL: {err}"),
        };
        let client = match ServiceClient::new(
            self.descriptor,
            Endpoint {
                base_url,
                auth: self.auth,
                credential: SecretString::from(key.to_owned()),
            },
        ) {
            Ok(client) => client,
            Err(err) => panic!("cannot build client: {err}"),
        };
        (self.build)(client)
    }
}

/// Run every library contract case. Panics with a description on failure.
pub async fn run_library_contract<A: Library>(c: &LibraryContract<A>) {
    assert!(
        array("primary_items", &c.primary_items).len() > PAGE_CAP,
        "fixtures need more than {PAGE_CAP} items so paging is exercised"
    );
    lists_libraries_and_server_id(c).await;
    items_are_complete_across_capped_pages(c).await;
    missing_total_is_incomplete(c).await;
    changing_total_is_incomplete(c).await;
    errors_follow_the_taxonomy(c).await;
    wrong_credential_is_credentials_invalid(c).await;
    other_product_is_refused(c).await;
}

async fn lists_libraries_and_server_id<A: Library>(c: &LibraryContract<A>) {
    let fake = c.fake(&c.info.body).await;
    let adapter = c.adapter(&fake.server, KEY);
    let libraries = adapter.libraries().await;
    match libraries {
        Ok(listing) => {
            assert!(listing.complete, "library listing must be complete");
            assert_eq!(listing.items, c.expected_libraries);
        }
        Err(err) => panic!("libraries failed: {err:?}"),
    }
    assert_eq!(
        adapter.server_id().await,
        Ok(Some(c.expected_server_id.clone()))
    );
}

async fn items_are_complete_across_capped_pages<A: Library>(c: &LibraryContract<A>) {
    let fake = c.fake(&c.info.body).await;
    let result = c.adapter(&fake.server, KEY).items(&c.library_id).await;
    match result {
        Ok(listing) => {
            assert!(listing.complete, "a full listing must be complete");
            assert_eq!(listing.items, c.expected_items);
        }
        Err(err) => panic!("items failed: {err:?}"),
    }
}

async fn missing_total_is_incomplete<A: Library>(c: &LibraryContract<A>) {
    let fake = c.fake(&c.info.body).await;
    fake.set(Mode::NoTotal);
    match c.adapter(&fake.server, KEY).items(&c.library_id).await {
        Ok(listing) => assert!(
            !listing.complete,
            "a listing without total count must be incomplete"
        ),
        Err(err) => panic!("items failed: {err:?}"),
    }
}

async fn changing_total_is_incomplete<A: Library>(c: &LibraryContract<A>) {
    let fake = c.fake(&c.info.body).await;
    fake.set(Mode::TotalChanges);
    match c.adapter(&fake.server, KEY).items(&c.library_id).await {
        Ok(listing) => assert!(
            !listing.complete,
            "a total that changes while paging must make the listing incomplete"
        ),
        Err(err) => panic!("items failed: {err:?}"),
    }
}

async fn errors_follow_the_taxonomy<A: Library>(c: &LibraryContract<A>) {
    for (status, expected) in [
        (401, "credentials_invalid"),
        (429, "throttled"),
        (500, "transient"),
        (503, "transient"),
    ] {
        let fake = c.fake(&c.info.body).await;
        fake.set(Mode::FailAfterFirst(status));
        let result = c.adapter(&fake.server, KEY).items(&c.library_id).await;
        match result {
            Err(err) => {
                assert_eq!(err.code(), expected, "HTTP {status}: {err:?}");
                if status == 429 {
                    assert_eq!(
                        err,
                        AdapterError::Throttled {
                            retry_after: Some(Duration::from_secs(3))
                        }
                    );
                }
            }
            // A failed page never yields a (partial) listing.
            Ok(listing) => panic!("HTTP {status} on page 2 returned {listing:?}"),
        }
    }
}

async fn wrong_credential_is_credentials_invalid<A: Library>(c: &LibraryContract<A>) {
    let fake = c.fake(&c.info.body).await;
    let adapter = c.adapter(&fake.server, "wrong-key-fedcba9876543210");
    assert_eq!(
        adapter.libraries().await.map(|l| l.items),
        Err(AdapterError::CredentialsInvalid)
    );
    assert_eq!(
        adapter.items(&c.library_id).await.map(|l| l.items),
        Err(AdapterError::CredentialsInvalid)
    );
}

async fn other_product_is_refused<A: Library>(c: &LibraryContract<A>) {
    let fake = c.fake(&c.other_product_info).await;
    let adapter = c.adapter(&fake.server, KEY);
    let probe = adapter.probe().await;
    assert!(
        matches!(probe, Err(AdapterError::Unknown(_))),
        "probe against the other product: {probe:?}"
    );
    let server = adapter.server_id().await;
    assert!(
        matches!(server, Err(AdapterError::Unknown(_))),
        "server id from the other product: {server:?}"
    );
}
