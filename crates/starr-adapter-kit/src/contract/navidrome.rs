//! A stateful fake Navidrome for account reads and writes, seeded from the
//! Navidrome adapter's **derived** fixtures (`users.json`, `library.json`,
//! `login.json`, `ping_without_login.json`: hand-written from Navidrome's
//! source, not recorded). It behaves like Navidrome where Starr depends on
//! it (`docs/concept/capabilities.md`, "Navidrome"):
//! - `POST /auth/login` checks a user's name (any case) and password and
//!   answers a token; every native API request needs the current token
//!   of the latest login (401 otherwise), and every answer renews it
//!   (sliding JWT);
//! - `PUT /api/user/{id}` replaces the user: a body without `userName`
//!   empties `userName` and `name` and resets `createdAt` (the partial-PUT
//!   trap); a `password` in the body sets the password and signs the user
//!   out; an unknown ID answers 400 `ra.validation.unique`;
//! - `POST /api/user` refuses a taken name in any case (400), unless a
//!   concurrent twin is simulated ([`NavidromeFake::twin_on_create`]); new
//!   users get the libraries marked `defaultNewUsers`;
//! - `PUT /api/user/{id}/library` refuses admins, an empty list and
//!   unknown or duplicate IDs (400, nothing changed), an unknown user 404;
//! - there is no disable flag.
//!
//! Passwords are kept in the fake only to answer logins
//! ([`NavidromeFake::login_works`]); they are never part of a user object.
//! Every request is kept (wiremock), so tests assert the exact writes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use secrecy::SecretString;
use serde_json::{Map, Value, json};
use url::Url;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::media_server::Write;
use crate::{AuthScheme, Endpoint, LoginCredential, ServiceClient};
use starr_core::adapters::AdapterDescriptor;

/// The fixtures the fake starts from.
#[derive(Debug, Clone)]
pub struct Derived {
    pub users: String,
    pub libraries: String,
    pub login: String,
    pub ping: String,
}

impl Derived {
    /// Read `users.json`, `library.json`, `login.json` and
    /// `ping_without_login.json` from `dir`.
    ///
    /// # Panics
    /// If a file is missing (test code).
    #[must_use]
    pub fn from_dir(dir: &str) -> Self {
        let read = |name: &str| {
            std::fs::read_to_string(format!("{dir}/{name}"))
                .unwrap_or_else(|e| panic!("{dir}/{name}: {e}"))
        };
        Self {
            users: read("users.json"),
            libraries: read("library.json"),
            login: read("login.json"),
            ping: read("ping_without_login.json"),
        }
    }
}

#[allow(clippy::struct_excessive_bools)] // independent fake modes
#[derive(Debug, Default)]
struct State {
    users: Vec<Value>,
    libraries: Vec<Value>,
    /// Library grants per user ID (admins: every library).
    grants: HashMap<String, Vec<u64>>,
    /// Passwords per user ID.
    passwords: HashMap<String, String>,
    /// Users signed out by a password change, per user ID.
    sign_outs: HashMap<String, u32>,
    token: Option<String>,
    logins: usize,
    next: u32,
    /// `POST /api/user` also creates an older account of the same name.
    twin_on_create: bool,
    /// `PUT /api/user/{id}/library` answers 500.
    fail_library_writes: bool,
    /// `PUT /api/user/{id}` empties the name whatever the body (a broken
    /// server), so the read-back must notice.
    break_user_writes: bool,
    /// User objects come without `libraries`.
    users_without_libraries: bool,
}

fn json_reply(status: u16, body: &Value) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(body)
}

fn unique_error() -> ResponseTemplate {
    json_reply(
        400,
        &json!({ "errors": { "userName": "ra.validation.unique" } }),
    )
}

impl State {
    fn index(&self, id: &str) -> Option<usize> {
        self.users.iter().position(|u| u["id"] == id)
    }

    fn is_admin(&self, id: &str) -> bool {
        self.index(id)
            .is_some_and(|i| self.users[i]["isAdmin"] == Value::Bool(true))
    }

    fn library_ids(&self) -> Vec<u64> {
        self.libraries
            .iter()
            .filter_map(|l| l["id"].as_u64())
            .collect()
    }

    fn libraries_of(&self, id: &str) -> Vec<Value> {
        let ids: Vec<u64> = if self.is_admin(id) {
            self.library_ids()
        } else {
            self.grants.get(id).cloned().unwrap_or_default()
        };
        self.libraries
            .iter()
            .filter(|l| l["id"].as_u64().is_some_and(|i| ids.contains(&i)))
            .cloned()
            .collect()
    }

    /// A user as the API shows it.
    fn shown(&self, i: usize) -> Value {
        let mut user = self.users[i].clone();
        if let Value::Object(map) = &mut user {
            map.remove("libraries");
            if !self.users_without_libraries {
                let id = map["id"].as_str().unwrap_or_default().to_owned();
                map.insert("libraries".to_owned(), Value::Array(self.libraries_of(&id)));
            }
        }
        user
    }

    fn created_at(&mut self) -> String {
        self.next += 1;
        format!(
            "2026-11-03T10:{:02}:{:02}.5Z",
            self.next / 60,
            self.next % 60
        )
    }

    fn login(&mut self, body: &Value, login_reply: &str) -> ResponseTemplate {
        self.logins += 1;
        let name = body["username"].as_str().unwrap_or_default().to_lowercase();
        let password = body["password"].as_str().unwrap_or_default();
        let found = self.users.iter().find(|u| {
            u["userName"].as_str().unwrap_or_default().to_lowercase() == name
                && !name.is_empty()
                && u["id"]
                    .as_str()
                    .and_then(|id| self.passwords.get(id))
                    .is_some_and(|p| p == password)
        });
        let Some(user) = found.cloned() else {
            return json_reply(401, &json!({ "error": "Invalid username or password" }));
        };
        let token = format!("token-{}", self.logins);
        self.token = Some(token.clone());
        let mut reply: Value = serde_json::from_str(login_reply).unwrap_or_else(|e| panic!("{e}"));
        reply["token"] = Value::String(token);
        reply["isAdmin"] = user["isAdmin"].clone();
        reply["id"] = user["id"].clone();
        reply["username"] = user["userName"].clone();
        json_reply(200, &reply)
    }

    fn create(&mut self, body: &Value) -> ResponseTemplate {
        let name = body["userName"].as_str().unwrap_or_default().to_owned();
        let taken = |s: &Self| {
            s.users.iter().any(|u| {
                u["userName"]
                    .as_str()
                    .is_some_and(|n| n.to_lowercase() == name.to_lowercase())
            })
        };
        if taken(self) {
            return unique_error();
        }
        let defaults: Vec<u64> = self
            .libraries
            .iter()
            .filter(|l| l["defaultNewUsers"] == Value::Bool(true))
            .filter_map(|l| l["id"].as_u64())
            .collect();
        if self.twin_on_create {
            // Another create of the same name got in between the check and
            // the insert (Navidrome checks uniqueness in app code only).
            self.twin_on_create = false;
            let created = self.created_at();
            let twin = format!("twin-{}", self.next);
            self.users.push(json!({
                "id": twin, "userName": name.to_uppercase(), "name": name, "email": "",
                "isAdmin": false, "lastLoginAt": null, "lastAccessAt": null,
                "createdAt": created, "updatedAt": created,
            }));
            self.grants.insert(twin.clone(), defaults.clone());
            self.passwords
                .insert(twin, "the twin's own password".to_owned());
        }
        let created = self.created_at();
        let id = format!("nd-{}", self.next);
        let user = json!({
            "id": id, "userName": name, "name": body["name"], "email": "",
            "isAdmin": body["isAdmin"] == Value::Bool(true), "lastLoginAt": null,
            "lastAccessAt": null, "createdAt": created, "updatedAt": created,
        });
        self.users.push(user);
        self.grants.insert(id.clone(), defaults);
        if let Some(password) = body["password"].as_str() {
            self.passwords.insert(id.clone(), password.to_owned());
        }
        let i = self.users.len() - 1;
        json_reply(200, &self.shown(i))
    }

    fn update(&mut self, id: &str, body: &Value) -> ResponseTemplate {
        let Some(i) = self.index(id) else {
            return unique_error();
        };
        let broken = self.break_user_writes;
        let user = &mut self.users[i];
        match body["userName"].as_str() {
            Some(name) if !broken => {
                user["userName"] = Value::String(name.to_owned());
                user["name"] = body.get("name").cloned().unwrap_or(Value::Null);
                user["email"] = body.get("email").cloned().unwrap_or(json!(""));
                user["isAdmin"] = body.get("isAdmin").cloned().unwrap_or(json!(false));
            }
            // The trap: a partial body empties the names and resets the
            // creation time.
            _ => {
                user["userName"] = json!("");
                user["name"] = json!("");
                user["createdAt"] = json!("0001-01-01T00:00:00Z");
            }
        }
        if let Some(password) = body["password"].as_str().filter(|p| !p.is_empty()) {
            self.passwords.insert(id.to_owned(), password.to_owned());
            *self.sign_outs.entry(id.to_owned()).or_default() += 1;
        }
        json_reply(200, &json!({ "id": id }))
    }

    fn set_libraries(&mut self, id: &str, body: &Value) -> ResponseTemplate {
        if self.index(id).is_none() {
            return json_reply(404, &json!({ "error": "not found" }));
        }
        if self.fail_library_writes {
            return json_reply(500, &json!({ "error": "internal" }));
        }
        if self.is_admin(id) {
            return json_reply(400, &json!({ "error": "cannot set libraries of an admin" }));
        }
        let Some(ids) = body["libraryIds"].as_array() else {
            return json_reply(400, &json!({ "error": "libraryIds required" }));
        };
        let ids: Vec<u64> = ids.iter().filter_map(Value::as_u64).collect();
        let known = self.library_ids();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        if ids.is_empty() || unique.len() != ids.len() || ids.iter().any(|i| !known.contains(i)) {
            return json_reply(400, &json!({ "error": "invalid library list" }));
        }
        self.grants.insert(id.to_owned(), ids);
        json_reply(200, &Value::Array(self.libraries_of(id)))
    }
}

struct Responder {
    derived: Arc<Derived>,
    state: Arc<Mutex<State>>,
}

impl Respond for Responder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let path = request.url.path().to_owned();
        let method = request.method.as_str();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        match (method, path.as_str()) {
            ("GET", "/rest/ping") => {
                let ping: Value =
                    serde_json::from_str(&self.derived.ping).unwrap_or_else(|e| panic!("{e}"));
                return json_reply(200, &ping);
            }
            ("POST", "/auth/login") => return state.login(&body, &self.derived.login),
            _ => {}
        }
        let sent = request
            .headers
            .get("x-nd-authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        // Tokens of the latest login stay valid (a JWT until it expires);
        // an older login's are refused (as after Navidrome restarts with a
        // new secret, or the admin's password changed).
        let Some(token) = sent.as_deref().and_then(|s| s.strip_prefix("Bearer ")) else {
            return ResponseTemplate::new(401).set_body_string("Unauthorized");
        };
        let current = state.token.clone().unwrap_or_default();
        if current.is_empty() || !(token == current || token.starts_with(&format!("{current}-"))) {
            return ResponseTemplate::new(401).set_body_string("Unauthorized");
        }
        let renewed = format!("{token}-r");
        let reply = match (method, path.as_str()) {
            ("GET", "/api/library") => json_reply(200, &Value::Array(state.libraries.clone()))
                .insert_header("x-total-count", state.libraries.len().to_string().as_str()),
            ("GET", "/api/user") => {
                let users: Vec<Value> = (0..state.users.len()).map(|i| state.shown(i)).collect();
                json_reply(200, &Value::Array(users))
                    .insert_header("x-total-count", state.users.len().to_string().as_str())
            }
            ("POST", "/api/user") => state.create(&body),
            _ => match path.strip_prefix("/api/user/") {
                Some(rest) => {
                    let (id, library) = match rest.strip_suffix("/library") {
                        Some(id) => (id.to_owned(), true),
                        None => (rest.to_owned(), false),
                    };
                    match (method, library) {
                        ("GET", false) => match state.index(&id) {
                            Some(i) => json_reply(200, &state.shown(i)),
                            None => json_reply(404, &json!({ "error": "not found" })),
                        },
                        ("PUT", false) => state.update(&id, &body),
                        ("DELETE", false) => match state.index(&id) {
                            Some(i) => {
                                state.users.remove(i);
                                state.grants.remove(&id);
                                state.passwords.remove(&id);
                                json_reply(200, &json!({}))
                            }
                            None => json_reply(404, &json!({ "error": "not found" })),
                        },
                        ("GET", true) => match state.index(&id) {
                            Some(_) => json_reply(200, &Value::Array(state.libraries_of(&id))),
                            None => json_reply(404, &json!({ "error": "not found" })),
                        },
                        ("PUT", true) => state.set_libraries(&id, &body),
                        _ => ResponseTemplate::new(405),
                    }
                }
                None => ResponseTemplate::new(404),
            },
        };
        reply.insert_header("x-nd-authorization", renewed.as_str())
    }
}

/// The fake service.
pub struct NavidromeFake {
    pub server: MockServer,
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for NavidromeFake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NavidromeFake")
            .field("uri", &self.server.uri())
            .finish_non_exhaustive()
    }
}

impl NavidromeFake {
    /// Start a fake whose users are the derived `users.json`; `passwords`
    /// names the password of each user that can log in (by `userName`),
    /// e.g. Starr's own admin account.
    ///
    /// # Panics
    /// If the fixtures do not parse (test code).
    pub async fn start(derived: Derived, passwords: &[(&str, &str)]) -> Self {
        let users: Vec<Value> =
            serde_json::from_str(&derived.users).unwrap_or_else(|e| panic!("users.json: {e}"));
        let libraries: Vec<Value> = serde_json::from_str(&derived.libraries)
            .unwrap_or_else(|e| panic!("library.json: {e}"));
        let mut state = State {
            libraries,
            ..State::default()
        };
        for user in &users {
            let id = user["id"].as_str().unwrap_or_default().to_owned();
            let grants = user["libraries"]
                .as_array()
                .map_or_default(|ls| ls.iter().filter_map(|l| l["id"].as_u64()).collect());
            state.grants.insert(id.clone(), grants);
            if let Some((_, password)) = passwords.iter().find(|(n, _)| user["userName"] == *n) {
                state.passwords.insert(id, (*password).to_owned());
            }
        }
        state.users = users;
        let state = Arc::new(Mutex::new(state));
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(Responder {
                derived: Arc::new(derived),
                state: Arc::clone(&state),
            })
            .mount(&server)
            .await;
        Self { server, state }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A client for the fake that logs in as `username` with `password`.
    ///
    /// # Panics
    /// If the client cannot be built (test code).
    #[must_use]
    pub fn client(
        &self,
        descriptor: &AdapterDescriptor,
        auth: AuthScheme,
        username: &str,
        password: &str,
    ) -> ServiceClient {
        let base_url = Url::parse(&self.server.uri()).unwrap_or_else(|e| panic!("{e}"));
        ServiceClient::new(
            descriptor,
            Endpoint {
                base_url,
                auth,
                credential: LoginCredential::encode(
                    username,
                    &SecretString::from(password.to_owned()),
                ),
            },
        )
        .unwrap_or_else(|e| panic!("{e}"))
    }

    /// The current user objects (without libraries).
    #[must_use]
    pub fn users(&self) -> Vec<Value> {
        self.lock().users.clone()
    }

    /// The user with this `userName` (any case), if any.
    #[must_use]
    pub fn user(&self, name: &str) -> Option<Value> {
        self.lock()
            .users
            .iter()
            .find(|u| {
                u["userName"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .cloned()
    }

    /// The library IDs granted to the user with this ID.
    #[must_use]
    pub fn grants(&self, id: &str) -> Vec<u64> {
        self.lock().grants.get(id).cloned().unwrap_or_default()
    }

    /// Whether `name` (any case) logs in with `password` now.
    #[must_use]
    pub fn login_works(&self, name: &str, password: &str) -> bool {
        let state = self.lock();
        state.users.iter().any(|u| {
            u["userName"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name) && !n.is_empty())
                && u["id"]
                    .as_str()
                    .and_then(|id| state.passwords.get(id))
                    .is_some_and(|p| p == password)
        })
    }

    /// How often a password change signed out the user with this ID.
    #[must_use]
    pub fn sign_outs(&self, id: &str) -> u32 {
        self.lock().sign_outs.get(id).copied().unwrap_or(0)
    }

    /// Change one user in place (by `userName`, exact).
    ///
    /// # Panics
    /// If there is no such user (test code).
    pub fn edit_user(&self, name: &str, edit: impl FnOnce(&mut Map<String, Value>)) {
        let mut state = self.lock();
        let user = state
            .users
            .iter_mut()
            .find(|u| u["userName"] == name)
            .and_then(Value::as_object_mut)
            .unwrap_or_else(|| panic!("no user {name:?}"));
        edit(user);
    }

    /// The next create also makes an older twin of the same name.
    pub fn twin_on_create(&self) {
        self.lock().twin_on_create = true;
    }

    /// Answer library writes with 500.
    pub fn fail_library_writes(&self) {
        self.lock().fail_library_writes = true;
    }

    /// Empty the name on every user write (a broken server).
    pub fn break_user_writes(&self) {
        self.lock().break_user_writes = true;
    }

    /// Show users without their `libraries`.
    pub fn users_without_libraries(&self) {
        self.lock().users_without_libraries = true;
    }

    /// How many logins reached the fake.
    #[must_use]
    pub fn logins(&self) -> usize {
        self.lock().logins
    }

    /// Every request other than `GET` and the login, in order.
    pub async fn writes(&self) -> Vec<Write> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method.as_str() != "GET" && r.url.path() != "/auth/login")
            .map(|r| Write {
                method: r.method.to_string(),
                target: r.url.path().to_owned(),
                body: serde_json::from_slice(&r.body).unwrap_or(Value::Null),
            })
            .collect()
    }
}
