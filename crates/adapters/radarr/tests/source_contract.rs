//! Radarr against the generic `Source` contract. Fixtures under
//! `fixtures/<version>/` are built from the source shapes (not recorded from
//! a live instance yet, see `docs/concept/capabilities.md`).
#![allow(clippy::unwrap_used)]

use starr_adapter_kit::contract::RecordedResponse;
use starr_adapter_kit::contract::source::{Fixture, SourceContract, run_source_contract};
use starr_adapter_radarr::{AUTH, DESCRIPTOR, Radarr};
use starr_core::source::{
    AddRequest, Files, MonitorScope, NaturalKey, QualityProfile, QueueEntry, QueueState, RootFolder,
};

/// The `/api/v3` surface Starr uses is identical in v5 and v6 (checked in
/// source: `MovieController.AllMovie`, `QueueDetailsController.GetQueue`);
/// the shapes are kept once, under the newest version.
const SHAPES: &str = "6.4.4.10685";

fn read(version: &str, file: &str) -> String {
    let path = format!("{}/fixtures/{version}/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn get(path: &'static str, query: Option<(&'static str, &str)>, file: &str) -> Fixture {
    Fixture {
        path,
        query: query.map(|(k, v)| (k, v.to_owned())),
        body: read(SHAPES, file),
    }
}

fn status(version: &str) -> Fixture {
    Fixture {
        path: "/api/v3/system/status",
        query: None,
        body: read(version, "system_status.json"),
    }
}

fn contract(supported: &str) -> SourceContract<Radarr> {
    SourceContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        build: Radarr::new,
        status_supported: status(supported),
        status_unsupported: status("7.0.0.10815"),
        library_path: "/api/v3/movie",
        key_field: "tmdbId",
        key: NaturalKey::TmdbMovie(603),
        existing: read(SHAPES, "movie.json"),
        metadata: get(
            "/api/v3/movie/lookup/tmdb",
            Some(("tmdbId", "603")),
            "movie_lookup_tmdb.json",
        ),
        add: AddRequest {
            key: NaturalKey::TmdbMovie(603),
            quality_profile_id: 4,
            root_folder_path: "/movies".into(),
            scope: MonitorScope::Item,
            search: true,
            minimum_availability: None,
            metadata_profile_id: None,
        },
        required_add_fields: &[
            "tmdbId",
            "title",
            "qualityProfileId",
            "rootFolderPath",
            "monitored",
            "minimumAvailability",
            "addOptions",
        ],
        // `MovieExistsValidator` → `ValidationException` → 400 with the
        // validation failures as body.
        duplicate: RecordedResponse {
            status: 400,
            body: read(SHAPES, "movie_add_duplicate_400.json"),
        },
        extend_scope: MonitorScope::Item,
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
                    path: "/movies/".into(),
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
        progress_fixtures: vec![get(
            "/api/v3/queue/details",
            Some(("movieId", "1")),
            "queue_details.json",
        )],
        expected_files: Files::Movie { has_file: false },
        expected_queue: vec![
            QueueEntry {
                unit: None,
                size_bytes: 8_000_000_000,
                size_left_bytes: 2_000_000_000,
                state: QueueState::Downloading,
                message: None,
            },
            QueueEntry {
                unit: None,
                size_bytes: 4_000_000_000,
                size_left_bytes: 0,
                state: QueueState::Warning,
                message: Some("Not an upgrade for existing movie file(s)".into()),
            },
        ],
    }
}

#[tokio::test]
async fn radarr_passes_the_source_contract_on_v6() {
    run_source_contract(&contract("6.4.4.10685")).await;
}

#[tokio::test]
async fn radarr_passes_the_source_contract_on_v5() {
    run_source_contract(&contract("5.28.0.10274")).await;
}
