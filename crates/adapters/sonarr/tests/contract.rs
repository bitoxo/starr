//! Sonarr against the generic probe and `Source` contracts. Fixtures under
//! `fixtures/<version>/` are built from the source shapes (not recorded from
//! a live instance yet, see `docs/concept/capabilities.md`).
#![allow(clippy::unwrap_used)]

use secrecy::SecretString;
use starr_adapter_kit::contract::source::{Fixture, SourceContract, run_source_contract};
use starr_adapter_kit::contract::{self, ProbeContract, RecordedResponse, run_probe_contract};
use starr_adapter_kit::{Endpoint, ServiceClient};
use starr_adapter_sonarr::{AUTH, DESCRIPTOR, Sonarr};
use starr_core::adapters::{AdapterError, Listing, Probe, Version};
use starr_core::source::{
    AddRequest, Files, MonitorScope, NaturalKey, QualityProfile, QueueEntry, QueueState,
    RootFolder, Source, UnitId, UnitProgress,
};
use url::Url;
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SUPPORTED: &str = "4.0.20.3014";
const UNSUPPORTED: &str = "5.0.0.2907";

fn read(version: &str, file: &str) -> String {
    let path = format!("{}/fixtures/{version}/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn status_fixture(version: &str) -> contract::Fixture {
    contract::Fixture {
        version: version.parse().unwrap(),
        body: read(version, "system_status.json"),
    }
}

fn get(path: &'static str, query: Option<(&'static str, &str)>, file: &str) -> Fixture {
    Fixture {
        path,
        query: query.map(|(k, v)| (k, v.to_owned())),
        body: read(SUPPORTED, file),
    }
}

fn status(version: &str) -> Fixture {
    Fixture {
        path: "/api/v3/system/status",
        query: None,
        body: read(version, "system_status.json"),
    }
}

/// `counts`: all episodes (not part of the progress), aired, aired with a
/// file, with a file.
fn season(number: u32, monitored: bool, counts: [u32; 4]) -> UnitProgress {
    let [_episodes, aired, aired_with_file, with_file] = counts;
    UnitProgress {
        unit: UnitId::Season(number),
        monitored,
        expected: aired,
        expected_with_file: aired_with_file,
        with_file,
    }
}

#[allow(clippy::too_many_lines)] // one declarative fixture table
fn source_contract() -> SourceContract<Sonarr> {
    let key = NaturalKey::TvdbSeries(81189);
    SourceContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        build: Sonarr::new,
        status_supported: status(SUPPORTED),
        status_unsupported: status(UNSUPPORTED),
        library_path: "/api/v3/series",
        key_field: "tvdbId",
        key,
        existing: read(SUPPORTED, "series.json"),
        metadata: get(
            "/api/v3/series/lookup",
            Some(("term", "tvdb:81189")),
            "series_lookup.json",
        ),
        add: AddRequest {
            key,
            quality_profile_id: 4,
            root_folder_path: "/tv".into(),
            scope: MonitorScope::seasons([1, 2]),
            search: true,
            minimum_availability: None,
            metadata_profile_id: None,
        },
        required_add_fields: &[
            "tvdbId",
            "title",
            "qualityProfileId",
            "rootFolderPath",
            "monitored",
            "seasonFolder",
            "monitorNewItems",
            "seasons",
            "addOptions",
        ],
        // Two adds racing past `SeriesExistsValidator` hit the unique index
        // on `Series.TvdbId`; the error pipeline turns the SQLite constraint
        // failure of a POST into 409.
        duplicate: RecordedResponse {
            status: 409,
            body: read(SUPPORTED, "series_add_conflict_409.json"),
        },
        extend_scope: MonitorScope::seasons([2]),
        profiles: (
            get("/api/v3/qualityprofile", None, "qualityprofile.json"),
            vec![
                QualityProfile {
                    id: 1,
                    name: "Any".into(),
                },
                QualityProfile {
                    id: 4,
                    name: "HD-1080p".into(),
                },
            ],
        ),
        root_folders: (
            get("/api/v3/rootfolder", None, "rootfolder.json"),
            vec![
                RootFolder {
                    id: 1,
                    path: "/tv/".into(),
                    accessible: true,
                    free_space_bytes: Some(1_200_000_000_000),
                },
                RootFolder {
                    id: 2,
                    path: "/offline/".into(),
                    accessible: false,
                    free_space_bytes: None,
                },
            ],
        ),
        progress_fixtures: vec![
            get("/api/v3/episode", Some(("seriesId", "7")), "episode.json"),
            get(
                "/api/v3/queue/details",
                Some(("seriesId", "7")),
                "queue_details.json",
            ),
        ],
        // Specials (season 0) aired without files, season 1 complete,
        // season 2 partial, season 3 not aired yet.
        expected_files: Files::Units(Listing {
            items: vec![
                season(0, false, [2, 2, 0, 0]),
                season(1, true, [3, 3, 3, 3]),
                season(2, false, [3, 3, 1, 1]),
                season(3, false, [2, 0, 0, 0]),
            ],
            complete: true,
        }),
        expected_queue: vec![
            QueueEntry {
                unit: Some(UnitId::Season(2)),
                size_bytes: 2_000_000_000,
                size_left_bytes: 500_000_000,
                state: QueueState::Downloading,
                message: None,
            },
            QueueEntry {
                unit: Some(UnitId::Season(2)),
                size_bytes: 2_100_000_000,
                size_left_bytes: 0,
                state: QueueState::Warning,
                message: Some(
                    "Found matching series via grab history, but release was matched to series by ID. Automatic import is not possible.".into(),
                ),
            },
        ],
    }
}

fn client(server: &MockServer) -> ServiceClient {
    ServiceClient::new(
        &DESCRIPTOR,
        Endpoint {
            base_url: Url::parse(&server.uri()).unwrap(),
            auth: AUTH,
            credential: SecretString::from("k".to_owned()),
        },
    )
    .unwrap()
}

#[tokio::test]
async fn sonarr_passes_the_probe_contract() {
    run_probe_contract(&ProbeContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        probe_method: "GET",
        probe_path: "/api/v3/system/status",
        supported: vec![status_fixture(SUPPORTED)],
        unsupported: status_fixture(UNSUPPORTED),
        // Same `ApiKeyAuthenticationHandler` as Radarr: empty 401.
        unauthorized: RecordedResponse {
            status: 401,
            body: String::new(),
        },
        build: Sonarr::new,
    })
    .await;
}

#[tokio::test]
async fn sonarr_passes_the_source_contract() {
    run_source_contract(&source_contract()).await;
}

#[test]
fn specials_do_not_count_for_the_series_state() {
    use starr_core::source::FileState;
    let Files::Units(mut listing) = source_contract().expected_files else {
        panic!("series have seasons");
    };
    // As mapped from the fixture only season 1 is monitored, and complete.
    assert_eq!(Files::Units(listing.clone()).state(), FileState::Complete);
    // Monitored specials without files still do not hold the series back.
    listing.items[0].monitored = true;
    assert_eq!(Files::Units(listing.clone()).state(), FileState::Complete);
    // Monitoring the partial season 2 makes the series partial.
    listing.items[2].monitored = true;
    assert_eq!(Files::Units(listing).state(), FileState::Partial);
}

#[tokio::test]
async fn another_arr_app_at_the_url_is_not_accepted_as_sonarr() {
    let server = MockServer::start().await;
    let body = read(SUPPORTED, "system_status.json")
        .replace("\"appName\": \"Sonarr\"", "\"appName\": \"Radarr\"");
    Mock::given(path("/api/v3/system/status"))
        .and(header("X-Api-Key", "k"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;
    let result = Sonarr::new(client(&server)).probe().await;
    assert!(
        matches!(&result, Err(AdapterError::Unknown(msg)) if msg.contains("Radarr")),
        "{result:?}"
    );
}

#[tokio::test]
async fn movies_and_whole_item_scopes_are_refused_without_a_request() {
    let server = MockServer::start().await;
    let adapter = Sonarr::new(client(&server));
    let movie = adapter.lookup(NaturalKey::TmdbMovie(603)).await;
    assert!(matches!(movie, Err(AdapterError::Rejected(_))), "{movie:?}");
    let whole = adapter
        .extend_monitoring(NaturalKey::TvdbSeries(81189), &MonitorScope::Item, false)
        .await;
    assert!(matches!(whole, Err(AdapterError::Rejected(_))), "{whole:?}");
    let none = adapter
        .extend_monitoring(
            NaturalKey::TvdbSeries(81189),
            &MonitorScope::Units(std::collections::BTreeSet::new()),
            false,
        )
        .await;
    assert!(matches!(none, Err(AdapterError::Rejected(_))), "{none:?}");
    // Music keys and album units are not Sonarr's (ADR 0013).
    let artist = adapter
        .lookup(NaturalKey::MbArtist(uuid::Uuid::nil()))
        .await;
    assert!(
        matches!(artist, Err(AdapterError::Rejected(_))),
        "{artist:?}"
    );
    let album = adapter
        .extend_monitoring(
            NaturalKey::TvdbSeries(81189),
            &MonitorScope::Units(
                [UnitId::Season(1), UnitId::ReleaseGroup(uuid::Uuid::nil())].into(),
            ),
            false,
        )
        .await;
    assert!(matches!(album, Err(AdapterError::Rejected(_))), "{album:?}");
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        0
    );
}

#[test]
fn descriptor_declares_requests_and_a_tested_range() {
    assert_eq!(DESCRIPTOR.type_id, "sonarr");
    assert!(
        DESCRIPTOR
            .tested_versions
            .contains(&Version::new(4, 0, 4, 0))
    );
    // `addOptions.monitor: "skip"` does not exist before 4.0.4.
    assert!(
        !DESCRIPTOR
            .tested_versions
            .contains(&Version::new(4, 0, 2, 1183))
    );
    assert!(
        !DESCRIPTOR
            .tested_versions
            .contains(&Version::new(5, 0, 0, 0))
    );
}
