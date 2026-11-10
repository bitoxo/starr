//! Generic contract test suite for the read half of [`Accounts`] (ADR 0009,
//! ADR 0011, lessons 8, 10, 12).
//!
//! The adapter's recorded fixtures are served by a fake that answers only
//! `GET` requests with the right credential. Every case also asserts that
//! the adapter sent **no other method** to the service: reading accounts
//! must never write (spec 0005 slice 1).

use secrecy::SecretString;
use starr_core::accounts::{Accounts, RemoteAccount};
use starr_core::adapters::{AdapterDescriptor, AdapterError};
use url::Url;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::RecordedResponse;
use crate::{AuthScheme, Endpoint, ServiceClient};

const KEY: &str = "contract-test-key-0123456789abcdef";

/// Everything the suite needs to know about one `Accounts` adapter.
#[derive(Debug)]
pub struct AccountsContract<A> {
    pub descriptor: &'static AdapterDescriptor,
    pub auth: AuthScheme,
    pub build: fn(ServiceClient) -> A,
    /// Recorded body of `GET /Users`.
    pub users: String,
    /// What `accounts` must return for it (complete, in fixture order).
    pub expected: Vec<RemoteAccount>,
    /// A remote ID from the listing and the recorded `GET /Users/{id}` body.
    pub one: (String, String),
    /// What the service answers for an unknown ID.
    pub unknown: RecordedResponse,
    /// What the service answers to a wrong credential.
    pub unauthorized: RecordedResponse,
}

fn json_body(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "application/json")
        .set_body_string(body)
}

impl<A: Accounts> AccountsContract<A> {
    async fn fake(&self, users: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        let (name, value) = super::auth_header(self.auth, KEY);
        Mock::given(method("GET"))
            .and(path("/Users"))
            .and(header(name, value.as_str()))
            .respond_with(users)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/Users/{}", self.one.0)))
            .and(header(name, value.as_str()))
            .respond_with(json_body(&self.one.1))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Users/0123456789abcdef0123456789abcdef"))
            .and(header(name, value.as_str()))
            .respond_with(
                ResponseTemplate::new(self.unknown.status).set_body_string(&self.unknown.body),
            )
            .mount(&server)
            .await;
        // Behind an auth proxy: a redirect, which must never read as "no
        // such account".
        Mock::given(method("GET"))
            .and(path("/Users/fedcba9876543210fedcba9876543210"))
            .and(header(name, value.as_str()))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", "https://sso.example.com/login"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(self.unauthorized.status)
                    .set_body_string(&self.unauthorized.body),
            )
            .mount(&server)
            .await;
        server
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

/// Panics unless every request the fake received was a `GET`.
async fn only_reads(server: &MockServer, case: &str) {
    let requests = server.received_requests().await.unwrap_or_default();
    assert!(!requests.is_empty(), "{case}: no request reached the fake");
    let writes: Vec<String> = requests
        .iter()
        .filter(|r| r.method.as_str() != "GET")
        .map(|r| format!("{} {}", r.method, r.url.path()))
        .collect();
    assert!(
        writes.is_empty(),
        "{case}: non-GET requests sent: {writes:?}"
    );
}

/// Run every accounts contract case. Panics with a description on failure.
pub async fn run_accounts_contract<A: Accounts>(c: &AccountsContract<A>) {
    assert!(
        c.expected.iter().any(|a| a.privileged) && c.expected.iter().any(|a| !a.privileged),
        "fixtures need privileged and unprivileged accounts"
    );
    assert!(
        c.expected.iter().any(|a| !a.enabled),
        "fixtures need a disabled account"
    );
    lists_every_account(c).await;
    reads_one_account(c).await;
    unknown_id_is_not_found(c).await;
    a_redirect_is_not_not_found(c).await;
    server_error_is_transient(c).await;
    wrong_credential_is_credentials_invalid(c).await;
}

async fn lists_every_account<A: Accounts>(c: &AccountsContract<A>) {
    let server = c.fake(json_body(&c.users)).await;
    match c.adapter(&server, KEY).accounts().await {
        Ok(listing) => {
            assert!(listing.complete, "the recorded listing must be complete");
            assert_eq!(listing.items, c.expected);
        }
        Err(err) => panic!("accounts failed: {err:?}"),
    }
    only_reads(&server, "accounts").await;
}

async fn reads_one_account<A: Accounts>(c: &AccountsContract<A>) {
    let server = c.fake(json_body(&c.users)).await;
    let Some(expected) = c.expected.iter().find(|a| a.remote_id == c.one.0) else {
        panic!("{} is not in the expected listing", c.one.0);
    };
    match c.adapter(&server, KEY).account(&c.one.0).await {
        Ok(account) => assert_eq!(&account, expected, "account({})", c.one.0),
        Err(err) => panic!("account({}) failed: {err:?}", c.one.0),
    }
    only_reads(&server, "account").await;
}

async fn unknown_id_is_not_found<A: Accounts>(c: &AccountsContract<A>) {
    let server = c.fake(json_body(&c.users)).await;
    let result = c
        .adapter(&server, KEY)
        .account("0123456789abcdef0123456789abcdef")
        .await;
    assert!(
        matches!(result, Err(AdapterError::NotFound(_))),
        "unknown account: {result:?}"
    );
    only_reads(&server, "unknown account").await;
}

async fn server_error_is_transient<A: Accounts>(c: &AccountsContract<A>) {
    let server = c
        .fake(ResponseTemplate::new(503).set_body_string("down"))
        .await;
    let result = c.adapter(&server, KEY).accounts().await;
    assert!(
        matches!(result, Err(AdapterError::Transient(_))),
        "HTTP 503: {result:?}"
    );
    only_reads(&server, "server error").await;
}

async fn wrong_credential_is_credentials_invalid<A: Accounts>(c: &AccountsContract<A>) {
    let server = c.fake(json_body(&c.users)).await;
    let adapter = c.adapter(&server, "wrong-key-fedcba9876543210");
    assert_eq!(
        adapter.accounts().await.map(|l| l.items),
        Err(AdapterError::CredentialsInvalid)
    );
    assert_eq!(
        adapter.account(&c.one.0).await,
        Err(AdapterError::CredentialsInvalid)
    );
    only_reads(&server, "wrong credential").await;
}

async fn a_redirect_is_not_not_found<A: Accounts>(c: &AccountsContract<A>) {
    let server = c.fake(json_body(&c.users)).await;
    let result = c
        .adapter(&server, KEY)
        .account("fedcba9876543210fedcba9876543210")
        .await;
    assert!(
        matches!(result, Err(AdapterError::Rejected(_))),
        "redirect: {result:?}"
    );
    only_reads(&server, "redirect").await;
}
