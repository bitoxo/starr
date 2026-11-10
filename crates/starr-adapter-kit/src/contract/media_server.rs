//! A stateful fake Jellyfin or Emby for account writes, seeded from the
//! adapter's **recorded** account fixtures (`users.json`,
//! `system_info.json`, `virtual_folders.json`). It behaves like the real
//! services where Starr depends on it (`docs/concept/capabilities.md`):
//! - every request needs the right credential (401 otherwise);
//! - `POST /Users/{id}/Policy` replaces the whole policy: Jellyfin refuses
//!   a partial one (400 without `AuthenticationProviderId` /
//!   `PasswordResetProviderId`), Emby accepts it and resets every missing
//!   field to its default (the partial-policy trap);
//! - `POST /Users/New` refuses a taken name in any case (400), unless
//!   duplicates are allowed (Emby's concurrent-create twins); Jellyfin
//!   takes the password with the create, Emby ignores it and creates the
//!   account active without a password;
//! - passwords: Jellyfin `POST /Users/Password?userId=`, Emby
//!   `POST /Users/{id}/Password`, body `{NewPw}`.
//!
//! Every request is kept (wiremock), so tests assert the exact requests a
//! write sent, and that a read sent no write at all.

use std::sync::{Arc, Mutex, PoisonError};

use secrecy::SecretString;
use serde_json::{Map, Value, json};
use url::Url;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::auth_header;
use crate::{AuthScheme, Endpoint, ServiceClient};
use starr_core::adapters::AdapterDescriptor;

/// Which service the fake plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeFlavor {
    Jellyfin,
    Emby,
}

/// The recorded fixtures the fake starts from.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub system_info: String,
    pub users: String,
    pub virtual_folders: String,
}

impl Recorded {
    /// Read `<dir>/{system_info,users,virtual_folders}.json`.
    ///
    /// # Panics
    /// If a file is missing (test code).
    #[must_use]
    pub fn from_dir(dir: &str) -> Self {
        let read = |name: &str| {
            let path = format!("{dir}/{name}");
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
        };
        Self {
            system_info: read("system_info.json"),
            users: read("users.json"),
            virtual_folders: read("virtual_folders.json"),
        }
    }
}

/// One request the fake received, other than `GET`.
#[derive(Debug, Clone, PartialEq)]
pub struct Write {
    pub method: String,
    /// Path and query, e.g. `/Users/Password?userId=ab12`.
    pub target: String,
    pub body: Value,
}

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // one switch per simulated failure
struct State {
    users: Vec<Value>,
    /// A default policy for new accounts and for Emby's reset.
    default_policy: Map<String, Value>,
    next: u64,
    allow_duplicates: bool,
    ignore_policy_writes: bool,
    fail_policy_writes: bool,
    /// Policy writes still answered before they fail (`None`: no limit).
    policy_writes_left: Option<u32>,
    /// Answer deletes with 204 but keep the account.
    ignore_deletes: bool,
}

#[derive(Clone)]
struct Responder {
    flavor: FakeFlavor,
    auth: (&'static str, String),
    recorded: Arc<Recorded>,
    state: Arc<Mutex<State>>,
}

fn json_reply(status: u16, body: &Value) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .insert_header("content-type", "application/json")
        .set_body_string(body.to_string())
}

fn text(status: u16, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_string(body)
}

fn name_of(user: &Value) -> String {
    user["Name"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_lowercase()
}

impl Responder {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn not_found() -> ResponseTemplate {
        // Jellyfin 12.3: 404 "User not found" on get, 400 on policy and
        // password; Emby: 404 on get and password, 500 on policy.
        text(404, "User not found")
    }

    fn user_index(state: &State, id: &str) -> Option<usize> {
        state
            .users
            .iter()
            .position(|u| u["Id"].as_str() == Some(id))
    }

    fn policy(&self, id: &str, body: Value) -> ResponseTemplate {
        let mut state = self.lock();
        let Some(index) = Self::user_index(&state, id) else {
            return match self.flavor {
                FakeFlavor::Jellyfin => text(400, "Error processing request."),
                FakeFlavor::Emby => {
                    text(500, "Object reference not set to an instance of an object.")
                }
            };
        };
        if state.fail_policy_writes {
            return text(503, "unavailable");
        }
        if let Some(left) = state.policy_writes_left.as_mut() {
            if *left == 0 {
                return text(503, "unavailable");
            }
            *left -= 1;
        }
        let Value::Object(given) = body else {
            return text(400, "Error processing request.");
        };
        let policy = match self.flavor {
            FakeFlavor::Jellyfin => {
                if !given.contains_key("AuthenticationProviderId")
                    || !given.contains_key("PasswordResetProviderId")
                {
                    return text(400, "Error processing request.");
                }
                given
            }
            // A partial policy resets every missing field.
            FakeFlavor::Emby => {
                let mut policy = state.default_policy.clone();
                policy.extend(given);
                policy
            }
        };
        if !state.ignore_policy_writes {
            state.users[index]["Policy"] = Value::Object(policy);
        }
        ResponseTemplate::new(204)
    }

    fn password(&self, id: &str, body: &Value) -> ResponseTemplate {
        let mut state = self.lock();
        let Some(index) = Self::user_index(&state, id) else {
            return match self.flavor {
                FakeFlavor::Jellyfin => text(400, "Error processing request."),
                FakeFlavor::Emby => Self::not_found(),
            };
        };
        let Some(password) = body["NewPw"].as_str() else {
            return text(400, "Error processing request.");
        };
        state.users[index]["HasPassword"] = Value::Bool(!password.is_empty());
        ResponseTemplate::new(204)
    }

    /// `DELETE /Users/{id}`: both answer 204 (Jellyfin) or 204/200 (Emby);
    /// an unknown ID is 404.
    fn delete(&self, id: &str) -> ResponseTemplate {
        let mut state = self.lock();
        let Some(index) = Self::user_index(&state, id) else {
            return Self::not_found();
        };
        if !state.ignore_deletes {
            state.users.remove(index);
        }
        ResponseTemplate::new(204)
    }

    fn create(&self, body: &Value) -> ResponseTemplate {
        let mut state = self.lock();
        let Some(name) = body["Name"]
            .as_str()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        else {
            return text(400, "Name is required");
        };
        let wanted = name.to_lowercase();
        if !state.allow_duplicates && state.users.iter().any(|u| name_of(u) == wanted) {
            return text(
                400,
                &format!("A user with the name '{name}' already exists."),
            );
        }
        state.next += 1;
        let id = format!("{:032x}", 0xfa4e_0000_u64 + state.next);
        let has_password = match self.flavor {
            FakeFlavor::Jellyfin => body["Password"].as_str().is_some_and(|p| !p.is_empty()),
            // Emby ignores the password on create.
            FakeFlavor::Emby => false,
        };
        let mut policy = state.default_policy.clone();
        if self.flavor == FakeFlavor::Jellyfin {
            // API-created users are hidden in 12.3 (recorded).
            policy.insert("IsHidden".to_owned(), Value::Bool(true));
        }
        let user = json!({
            "Name": name,
            "Id": id,
            "HasPassword": has_password,
            "DateCreated": format!("2026-11-03T10:00:{:02}.0000000Z", state.next % 60),
            "Policy": policy,
        });
        state.users.push(user.clone());
        json_reply(200, &user)
    }

    fn get(&self, path: &str) -> ResponseTemplate {
        match path {
            "/System/Info" => text(200, &self.recorded.system_info),
            "/Library/VirtualFolders" => text(200, &self.recorded.virtual_folders),
            "/Users" => json_reply(200, &Value::Array(self.lock().users.clone())),
            _ => match path.strip_prefix("/Users/") {
                Some(id) if !id.contains('/') => {
                    let state = self.lock();
                    match Self::user_index(&state, id) {
                        Some(i) => json_reply(200, &state.users[i]),
                        None => Self::not_found(),
                    }
                }
                _ => text(404, "not found"),
            },
        }
    }
}

impl Respond for Responder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let authorized = request
            .headers
            .get(self.auth.0)
            .and_then(|v| v.to_str().ok())
            == Some(self.auth.1.as_str());
        if !authorized {
            return text(401, "");
        }
        let path = request.url.path();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        match request.method.as_str() {
            "GET" => self.get(path),
            "POST" => {
                if path == "/Users/New" {
                    return self.create(&body);
                }
                if path == "/Users/Password" && self.flavor == FakeFlavor::Jellyfin {
                    let id = request
                        .url
                        .query_pairs()
                        .find(|(k, _)| k.eq_ignore_ascii_case("userId"))
                        .map(|(_, v)| v.into_owned())
                        .unwrap_or_default();
                    return self.password(&id, &body);
                }
                let rest = path.strip_prefix("/Users/").unwrap_or_default();
                match rest.split_once('/') {
                    Some((id, "Policy")) => self.policy(id, body),
                    Some((id, "Password")) if self.flavor == FakeFlavor::Emby => {
                        self.password(id, &body)
                    }
                    _ => text(404, "not found"),
                }
            }
            "DELETE" => match path.strip_prefix("/Users/") {
                Some(id) if !id.contains('/') => self.delete(id),
                _ => text(404, "not found"),
            },
            _ => text(405, "method not allowed"),
        }
    }
}

/// The fake service.
pub struct MediaServerFake {
    pub server: MockServer,
    flavor: FakeFlavor,
    auth: AuthScheme,
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for MediaServerFake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaServerFake")
            .field("uri", &self.server.uri())
            .field("flavor", &self.flavor)
            .finish_non_exhaustive()
    }
}

impl MediaServerFake {
    /// Start a fake answering only `key` under `auth`. New accounts get the
    /// policy of the recorded account named `template` (a plain friend
    /// created through the API) with every library and enabled.
    ///
    /// # Panics
    /// If the fixtures do not parse or lack `template` (test code).
    pub async fn start(
        flavor: FakeFlavor,
        auth: AuthScheme,
        key: &str,
        recorded: Recorded,
        template: &str,
    ) -> Self {
        let users: Vec<Value> =
            serde_json::from_str(&recorded.users).unwrap_or_else(|e| panic!("users.json: {e}"));
        let mut default_policy = users
            .iter()
            .find(|u| u["Name"] == template)
            .and_then(|u| u["Policy"].as_object().cloned())
            .unwrap_or_else(|| panic!("no recorded account {template:?}"));
        default_policy.insert("IsDisabled".to_owned(), Value::Bool(false));
        default_policy.insert("EnableAllFolders".to_owned(), Value::Bool(true));
        default_policy.insert("EnabledFolders".to_owned(), json!([]));
        let state = Arc::new(Mutex::new(State {
            users,
            default_policy,
            next: 0,
            allow_duplicates: false,
            ignore_policy_writes: false,
            fail_policy_writes: false,
            policy_writes_left: None,
            ignore_deletes: false,
        }));
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(Responder {
                flavor,
                auth: auth_header(auth, key),
                recorded: Arc::new(recorded),
                state: Arc::clone(&state),
            })
            .mount(&server)
            .await;
        Self {
            server,
            flavor,
            auth,
            state,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A client for the fake with `key`.
    ///
    /// # Panics
    /// If the client cannot be built (test code).
    #[must_use]
    pub fn client(&self, descriptor: &AdapterDescriptor, key: &str) -> ServiceClient {
        let base_url = Url::parse(&self.server.uri()).unwrap_or_else(|e| panic!("{e}"));
        ServiceClient::new(
            descriptor,
            Endpoint {
                base_url,
                auth: self.auth,
                credential: SecretString::from(key.to_owned()),
            },
        )
        .unwrap_or_else(|e| panic!("{e}"))
    }

    /// The current user objects.
    #[must_use]
    pub fn users(&self) -> Vec<Value> {
        self.lock().users.clone()
    }

    /// The current user with this name (exact), if any.
    #[must_use]
    pub fn user(&self, name: &str) -> Option<Value> {
        self.lock()
            .users
            .iter()
            .find(|u| u["Name"] == name)
            .cloned()
    }

    /// Change one user in place (e.g. a field an admin set in the service).
    ///
    /// # Panics
    /// If there is no such user (test code).
    pub fn edit_user(&self, name: &str, edit: impl FnOnce(&mut Value)) {
        let mut state = self.lock();
        let user = state
            .users
            .iter_mut()
            .find(|u| u["Name"] == name)
            .unwrap_or_else(|| panic!("no user {name:?}"));
        edit(user);
    }

    /// Add a user (e.g. a twin created concurrently).
    pub fn add_user(&self, user: Value) {
        self.lock().users.push(user);
    }

    /// Let `POST /Users/New` create duplicate names (Emby twins).
    pub fn allow_duplicates(&self) {
        self.lock().allow_duplicates = true;
    }

    /// Answer policy writes with 204 but keep the old policy.
    pub fn ignore_policy_writes(&self) {
        self.lock().ignore_policy_writes = true;
    }

    /// Answer policy writes with 503.
    pub fn fail_policy_writes(&self) {
        self.lock().fail_policy_writes = true;
    }

    /// Accept policy writes again after [`Self::fail_policy_writes`].
    pub fn allow_policy_writes(&self) {
        let mut state = self.lock();
        state.fail_policy_writes = false;
        state.policy_writes_left = None;
    }

    /// Accept the next `n` policy writes, answer every later one with 503.
    pub fn fail_policy_writes_after(&self, n: u32) {
        self.lock().policy_writes_left = Some(n);
    }

    /// Answer deletes with 204 but keep the account.
    pub fn ignore_deletes(&self) {
        self.lock().ignore_deletes = true;
    }

    /// Every request other than `GET`, in order.
    ///
    /// # Panics
    /// If request recording is off (never, in this fake).
    pub async fn writes(&self) -> Vec<Write> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method.as_str() != "GET")
            .map(|r| Write {
                method: r.method.to_string(),
                target: match r.url.query() {
                    Some(q) => format!("{}?{q}", r.url.path()),
                    None => r.url.path().to_owned(),
                },
                body: serde_json::from_slice(&r.body).unwrap_or(Value::Null),
            })
            .collect()
    }

    /// How many requests reached the fake.
    pub async fn request_count(&self) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .len()
    }
}
