//! Radarr against the generic probe contract, replaying fixtures per
//! Radarr version from `fixtures/<version>/` (lesson 10).
#![allow(clippy::unwrap_used)]

use secrecy::SecretString;
use starr_adapter_kit::contract::{Fixture, ProbeContract, RecordedResponse, run_probe_contract};
use starr_adapter_kit::{Endpoint, ServiceClient};
use starr_adapter_radarr::{AUTH, DESCRIPTOR, Radarr};
use starr_core::adapters::{AdapterError, Probe, Version};
use url::Url;
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(version: &str) -> Fixture {
    let file = format!(
        "{}/fixtures/{version}/system_status.json",
        env!("CARGO_MANIFEST_DIR")
    );
    Fixture {
        version: version.parse().unwrap(),
        body: std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file}: {e}")),
    }
}

#[tokio::test]
async fn radarr_passes_the_probe_contract() {
    run_probe_contract(&ProbeContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        probe_method: "GET",
        probe_path: "/api/v3/system/status",
        supported: vec![fixture("5.28.0.10274"), fixture("6.4.4.10685")],
        unsupported: fixture("7.0.0.10815"),
        // Radarr answers a wrong or missing key with an empty 401.
        unauthorized: RecordedResponse {
            status: 401,
            body: String::new(),
        },
        build: Radarr::new,
    })
    .await;
}

#[tokio::test]
async fn another_arr_app_at_the_url_is_not_accepted_as_radarr() {
    let server = MockServer::start().await;
    let body = fixture("6.4.4.10685")
        .body
        .replace("\"appName\": \"Radarr\"", "\"appName\": \"Sonarr\"");
    Mock::given(path("/api/v3/system/status"))
        .and(header("X-Api-Key", "k"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;
    let client = ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(&server.uri()).unwrap(),
            auth: AUTH,
            credential: SecretString::from("k".to_owned()),
        },
    )
    .unwrap();
    let result = Radarr::new(client).probe().await;
    assert!(
        matches!(&result, Err(AdapterError::Unknown(msg)) if msg.contains("Sonarr")),
        "{result:?}"
    );
}

#[test]
fn descriptor_declares_requests_and_a_tested_range() {
    assert_eq!(DESCRIPTOR.type_id, "radarr");
    assert!(
        DESCRIPTOR
            .tested_versions
            .contains(&Version::new(5, 0, 0, 0))
    );
    assert!(
        !DESCRIPTOR
            .tested_versions
            .contains(&Version::new(4, 7, 5, 7809))
    );
}
