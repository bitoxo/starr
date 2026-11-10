//! Generic contract test suite for [`Probe`] adapters (ADR 0009, lesson 10).
//!
//! Every adapter runs [`run_probe_contract`] from its tests with its own
//! recorded fixtures (one success body per tested service version). The
//! suite replays them through a `wiremock` fake that is stricter than the
//! real service: it answers only the probe request with the right
//! credential, and every case asserts the exact taxonomy outcome.
//!
//! Panics on violation: this is test code, compiled only with the
//! `contract` feature from adapter dev-dependencies.

pub mod account_writes;
pub mod accounts;
pub mod library;
pub mod media_server;
pub mod navidrome;
pub mod source;

use std::time::Duration;

use secrecy::SecretString;
use starr_core::adapters::{AdapterDescriptor, AdapterError, Probe, Version};
use url::Url;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::{AuthScheme, Endpoint, ServiceClient};

const KEY: &str = "contract-test-key-0123456789abcdef";
const WRONG_KEY: &str = "wrong-key-fedcba9876543210";

/// A recorded success response of the probe endpoint.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// The version this response reports.
    pub version: Version,
    pub body: String,
}

/// A recorded non-success response (status and body).
#[derive(Debug, Clone)]
pub struct RecordedResponse {
    pub status: u16,
    pub body: String,
}

/// Everything the suite needs to know about one adapter.
#[derive(Debug)]
pub struct ProbeContract<A> {
    pub descriptor: &'static AdapterDescriptor,
    pub auth: AuthScheme,
    /// HTTP method and path of the probe request, relative to the base URL
    /// (e.g. `GET`, `/api/v3/system/status`).
    pub probe_method: &'static str,
    pub probe_path: &'static str,
    /// One recorded success per tested service version (non-empty).
    pub supported: Vec<Fixture>,
    /// A recorded success from a version outside the tested range.
    pub unsupported: Fixture,
    /// What the service answers to a wrong credential.
    pub unauthorized: RecordedResponse,
    /// Constructs the adapter around the client Starr hands it.
    pub build: fn(ServiceClient) -> A,
}

/// Header name and value that carry `key` under `auth`.
pub(crate) fn auth_header(auth: AuthScheme, key: &str) -> (&'static str, String) {
    match auth {
        AuthScheme::ApiKeyHeader(name) => (name, key.to_owned()),
        AuthScheme::Bearer => ("Authorization", format!("Bearer {key}")),
        AuthScheme::MediaBrowserToken => ("Authorization", format!("MediaBrowser Token=\"{key}\"")),
        AuthScheme::None | AuthScheme::Session(_) => {
            panic!("the contract suites need an adapter with a fixed credential header")
        }
    }
}

fn client<A>(contract: &ProbeContract<A>, base: &str, key: &str) -> ServiceClient {
    let base_url = match Url::parse(base) {
        Ok(url) => url,
        Err(err) => panic!("invalid base URL {base}: {err}"),
    };
    match ServiceClient::new(
        contract.descriptor,
        Endpoint {
            base_url,
            auth: contract.auth,
            credential: SecretString::from(key.to_owned()),
        },
    ) {
        Ok(client) => client,
        Err(err) => panic!("cannot build client: {err}"),
    }
}

impl<A: Probe> ProbeContract<A> {
    fn auth_header(&self) -> (&'static str, String) {
        auth_header(self.auth, KEY)
    }

    /// The probe request with the right credential, under `prefix`.
    fn probe_mock(&self, prefix: &str, response: ResponseTemplate) -> Mock {
        let (name, value) = self.auth_header();
        Mock::given(method(self.probe_method))
            .and(path(format!("{prefix}{}", self.probe_path)))
            .and(header(name, value.as_str()))
            .respond_with(response)
    }

    async fn probe_with(
        &self,
        server: &MockServer,
        prefix: &str,
        key: &str,
    ) -> Result<Version, AdapterError> {
        let adapter = (self.build)(client(self, &format!("{}{prefix}", server.uri()), key));
        assert_eq!(
            adapter.descriptor(),
            self.descriptor,
            "adapter reports a different descriptor"
        );
        adapter.probe().await.map(|report| report.version)
    }
}

fn json(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "application/json")
        .set_body_string(body)
}

/// Run every probe contract case. Panics with a description on failure.
pub async fn run_probe_contract<A: Probe>(contract: &ProbeContract<A>) {
    assert!(
        !contract.supported.is_empty(),
        "contract needs at least one supported fixture"
    );
    for fixture in &contract.supported {
        assert!(
            contract
                .descriptor
                .tested_versions
                .contains(&fixture.version),
            "fixture {} is outside the tested range; record one inside it",
            fixture.version
        );
    }
    assert!(
        !contract
            .descriptor
            .tested_versions
            .contains(&contract.unsupported.version),
        "the unsupported fixture {} is inside the tested range",
        contract.unsupported.version
    );

    ok_for_every_supported_version(contract).await;
    ok_under_a_url_base(contract).await;
    wrong_credential_is_credentials_invalid(contract).await;
    throttling_is_reported_and_shared(contract).await;
    server_error_is_transient(contract).await;
    unreachable_is_transient(contract).await;
    unsupported_version_is_reported(contract).await;
    garbage_body_is_unknown(contract).await;
}

async fn ok_for_every_supported_version<A: Probe>(c: &ProbeContract<A>) {
    for fixture in &c.supported {
        let server = MockServer::start().await;
        c.probe_mock("", json(&fixture.body))
            .expect(1)
            .mount(&server)
            .await;
        let result = c.probe_with(&server, "", KEY).await;
        assert_eq!(result, Ok(fixture.version), "fixture {}", fixture.version);
        server.verify().await;
    }
}

async fn ok_under_a_url_base<A: Probe>(c: &ProbeContract<A>) {
    let fixture = &c.supported[0];
    let server = MockServer::start().await;
    c.probe_mock("/media/svc", json(&fixture.body))
        .expect(1)
        .mount(&server)
        .await;
    // Trailing slash or not, the URL base is kept.
    let result = c.probe_with(&server, "/media/svc/", KEY).await;
    assert_eq!(result, Ok(fixture.version), "with trailing slash");
    server.reset().await;
    c.probe_mock("/media/svc", json(&fixture.body))
        .expect(1)
        .mount(&server)
        .await;
    let result = c.probe_with(&server, "/media/svc", KEY).await;
    assert_eq!(result, Ok(fixture.version), "without trailing slash");
    server.verify().await;
}

async fn wrong_credential_is_credentials_invalid<A: Probe>(c: &ProbeContract<A>) {
    let server = MockServer::start().await;
    c.probe_mock("", json(&c.supported[0].body))
        .expect(0)
        .mount(&server)
        .await;
    // Anything without the right key gets the recorded rejection, once:
    // a probe never retries or falls back to a login (lesson 11).
    Mock::given(method(c.probe_method))
        .respond_with(
            ResponseTemplate::new(c.unauthorized.status).set_body_string(&c.unauthorized.body),
        )
        .expect(1)
        .mount(&server)
        .await;
    let result = c.probe_with(&server, "", WRONG_KEY).await;
    assert_eq!(result, Err(AdapterError::CredentialsInvalid));
    server.verify().await;
}

async fn throttling_is_reported_and_shared<A: Probe>(c: &ProbeContract<A>) {
    let server = MockServer::start().await;
    c.probe_mock(
        "",
        ResponseTemplate::new(429)
            .insert_header("retry-after", "7")
            .set_body_string("Too Many Requests"),
    )
    .expect(1)
    .mount(&server)
    .await;
    let client = client(c, &server.uri(), KEY);
    let first = (c.build)(client.clone()).probe().await;
    assert_eq!(
        first,
        Err(AdapterError::Throttled {
            retry_after: Some(Duration::from_secs(7))
        })
    );
    // A second adapter on the same instance client does not hit the service.
    let second = (c.build)(client).probe().await;
    match second {
        Err(AdapterError::Throttled {
            retry_after: Some(left),
        }) => assert!(left <= Duration::from_secs(7), "{left:?}"),
        other => panic!("expected Throttled without a request, got {other:?}"),
    }
    server.verify().await;
}

async fn server_error_is_transient<A: Probe>(c: &ProbeContract<A>) {
    for status in [500, 502, 503] {
        let server = MockServer::start().await;
        c.probe_mock(
            "",
            ResponseTemplate::new(status).set_body_string("upstream down"),
        )
        .expect(1)
        .mount(&server)
        .await;
        let result = c.probe_with(&server, "", KEY).await;
        assert!(
            matches!(result, Err(AdapterError::Transient(_))),
            "HTTP {status}: {result:?}"
        );
        server.verify().await;
    }
}

async fn unreachable_is_transient<A: Probe>(c: &ProbeContract<A>) {
    // wiremock pools its servers, so a dropped MockServer keeps listening:
    // take a free port from the OS and close it again instead.
    let port = match std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()) {
        Ok(addr) => addr.port(),
        Err(err) => panic!("cannot reserve a port: {err}"),
    };
    let uri = format!("http://127.0.0.1:{port}");
    let adapter = (c.build)(client(c, &uri, KEY));
    let result = adapter.probe().await;
    assert!(
        matches!(result, Err(AdapterError::Transient(_))),
        "{result:?}"
    );
}

async fn unsupported_version_is_reported<A: Probe>(c: &ProbeContract<A>) {
    let server = MockServer::start().await;
    c.probe_mock("", json(&c.unsupported.body))
        .expect(1)
        .mount(&server)
        .await;
    let result = c.probe_with(&server, "", KEY).await;
    assert_eq!(
        result,
        Err(AdapterError::VersionUnsupported {
            found: c.unsupported.version,
            tested: c.descriptor.tested_versions,
        })
    );
    server.verify().await;
}

async fn garbage_body_is_unknown<A: Probe>(c: &ProbeContract<A>) {
    for body in [
        "<html><body>Please log in</body></html>",
        "{\"unexpected\": true}",
        "",
    ] {
        let server = MockServer::start().await;
        c.probe_mock("", json(body)).expect(1).mount(&server).await;
        let result = c.probe_with(&server, "", KEY).await;
        assert!(
            matches!(result, Err(AdapterError::Unknown(_))),
            "body {body:?}: {result:?}"
        );
        server.verify().await;
    }
}
