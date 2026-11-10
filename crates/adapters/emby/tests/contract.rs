//! Emby against the generic probe and library contracts. Fixtures in
//! `fixtures/<version>/` are built from Emby's REST reference shapes
//! (`SystemInfo`, `UserDto`, `VirtualFolderInfo`, `BaseItemDto`), not
//! recorded from a live server yet (`capabilities.md`, Emby).
#![allow(clippy::unwrap_used)]

use starr_adapter_emby::{AUTH, DESCRIPTOR, Emby};
use starr_adapter_kit::contract::library::{self, LibraryContract, run_library_contract};
use starr_adapter_kit::contract::{Fixture, ProbeContract, RecordedResponse, run_probe_contract};
use starr_core::library::{
    ItemKind, LibraryContent, MediaItem, MediaLibrary, ProviderIds, SeasonPresence, UnitCounts,
};

const LIBRARY_VERSION: &str = "4.10.0.40";

fn read(version: &str, file: &str) -> String {
    let path = format!("{}/fixtures/{version}/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn info(version: &str) -> Fixture {
    Fixture {
        version: version.parse().unwrap(),
        body: read(version, "system_info.json"),
    }
}

/// Emby's answer to a wrong key [V 10-02].
fn unauthorized() -> RecordedResponse {
    RecordedResponse {
        status: 401,
        body: "Access token is invalid or expired.".into(),
    }
}

#[tokio::test]
async fn emby_passes_the_probe_contract() {
    run_probe_contract(&ProbeContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        probe_method: "GET",
        probe_path: "/System/Info",
        supported: vec![info("4.10.0.40"), info("4.10.1.0")],
        unsupported: info("4.9.1.90"),
        unauthorized: unauthorized(),
        build: Emby::new,
    })
    .await;
}

fn series(id: &str, title: &str, year: u32, ids: ProviderIds, seasons: &[(u32, u32)]) -> MediaItem {
    MediaItem {
        remote_id: id.to_owned(),
        kind: ItemKind::Series,
        provider_ids: ids,
        title: title.to_owned(),
        year: Some(year),
        units: UnitCounts::Seasons(
            seasons
                .iter()
                .map(|&(number, episodes)| SeasonPresence { number, episodes })
                .collect(),
        ),
    }
}

#[tokio::test]
async fn emby_passes_the_library_contract() {
    // What a Jellyfin server answers at the same path.
    let jellyfin_info = read(LIBRARY_VERSION, "system_info.json").replace(
        "\"ServerName\": \"lab-emby\",",
        "\"ServerName\": \"lab-emby\", \"ProductName\": \"Jellyfin Server\",",
    );
    assert!(jellyfin_info.contains("ProductName"));
    run_library_contract(&LibraryContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        build: Emby::new,
        info: library::Fixture {
            path: "/System/Info".into(),
            body: read(LIBRARY_VERSION, "system_info.json"),
        },
        expected_server_id: "8c1f0e2d3b4a59687f6e5d4c3b2a1908".into(),
        other_product_info: jellyfin_info,
        users: library::Fixture {
            path: "/Users".into(),
            body: read(LIBRARY_VERSION, "users.json"),
        },
        folders: library::Fixture {
            path: "/Library/VirtualFolders".into(),
            body: read(LIBRARY_VERSION, "virtual_folders.json"),
        },
        expected_libraries: vec![
            MediaLibrary {
                id: "3".into(),
                // Emby grants access by the library `Guid`.
                grant_id: Some("0c41907140d802bb58430fed7e2cd79e".into()),
                name: "Movies".into(),
                content: LibraryContent::Movies,
            },
            MediaLibrary {
                id: "6".into(),
                grant_id: Some("a2b3c4d5e6f708192a3b4c5d6e7f8091".into()),
                name: "TV".into(),
                content: LibraryContent::Series,
            },
            MediaLibrary {
                id: "9".into(),
                grant_id: Some("b3c4d5e6f708192a3b4c5d6e7f8091a2".into()),
                name: "Audiobooks".into(),
                content: LibraryContent::Books,
            },
        ],
        library_id: "6".into(),
        // The admin's ID is in the path; Emby's `/Items` without a user is
        // not verified.
        items_path: "/Users/1/Items".into(),
        user_query: None,
        primary_items: read(LIBRARY_VERSION, "items_movies_series.json"),
        episodes: read(LIBRARY_VERSION, "items_episodes.json"),
        expected_items: vec![
            MediaItem {
                remote_id: "40".into(),
                kind: ItemKind::Movie,
                provider_ids: ProviderIds {
                    tmdb: Some(603),
                    tvdb: None,
                    imdb: Some("tt0133093".into()),
                    ..ProviderIds::default()
                },
                title: "The Matrix".into(),
                year: Some(1999),
                units: UnitCounts::Whole,
            },
            series(
                "51",
                "Breaking Bad",
                2008,
                ProviderIds {
                    tmdb: Some(1396),
                    tvdb: Some(81189),
                    imdb: Some("tt0903747".into()),
                    ..ProviderIds::default()
                },
                &[(1, 2), (2, 1)],
            ),
            MediaItem {
                remote_id: "60".into(),
                kind: ItemKind::Movie,
                provider_ids: ProviderIds::default(),
                title: "Unmatched Film".into(),
                year: Some(2020),
                units: UnitCounts::Whole,
            },
            series(
                "61",
                "Severance",
                2022,
                ProviderIds {
                    tmdb: Some(95396),
                    tvdb: Some(371_980),
                    imdb: None,
                    ..ProviderIds::default()
                },
                &[(1, 3)],
            ),
        ],
        unauthorized: unauthorized(),
    })
    .await;
}

#[test]
fn descriptor_declares_libraries_accounts_and_a_tested_range() {
    use starr_core::adapters::{Capability, Version};
    assert_eq!(DESCRIPTOR.type_id, "emby");
    assert_eq!(
        DESCRIPTOR.capabilities,
        &[Capability::Libraries, Capability::Accounts]
    );
    assert!(
        DESCRIPTOR
            .tested_versions
            .contains(&Version::new(4, 10, 1, 0))
    );
    assert!(
        !DESCRIPTOR
            .tested_versions
            .contains(&Version::new(4, 9, 1, 90))
    );
}
