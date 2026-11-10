//! Jellyfin against the generic probe and library contracts. Fixtures in
//! `fixtures/<version>/` are built from the source shapes (jellyfin
//! `SystemInfo`, `UserDto`, `VirtualFolderInfo`, `BaseItemDto`), not
//! recorded from a live server yet (`capabilities.md`, Jellyfin).
#![allow(clippy::unwrap_used)]

use starr_adapter_jellyfin::{AUTH, DESCRIPTOR, Jellyfin};
use starr_adapter_kit::contract::library::{self, LibraryContract, run_library_contract};
use starr_adapter_kit::contract::{Fixture, ProbeContract, RecordedResponse, run_probe_contract};
use starr_core::library::{
    ItemKind, LibraryContent, MediaItem, MediaLibrary, ProviderIds, SeasonPresence, UnitCounts,
};

const LIBRARY_VERSION: &str = "12.3.0";
const ADMIN: &str = "c4b2a0e8f6d4c2b0a8e6f4d2c0b8a6e4";

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

/// Jellyfin answers a wrong or missing key with an empty 401.
fn unauthorized() -> RecordedResponse {
    RecordedResponse {
        status: 401,
        body: String::new(),
    }
}

#[tokio::test]
async fn jellyfin_passes_the_probe_contract() {
    run_probe_contract(&ProbeContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        probe_method: "GET",
        probe_path: "/System/Info",
        supported: vec![info("10.11.11"), info("12.3.0"), info("12.4.0")],
        unsupported: info("10.10.7"),
        unauthorized: unauthorized(),
        build: Jellyfin::new,
    })
    .await;
}

fn movie(id: &str, title: &str, year: Option<u32>, ids: ProviderIds) -> MediaItem {
    MediaItem {
        remote_id: id.to_owned(),
        kind: ItemKind::Movie,
        provider_ids: ids,
        title: title.to_owned(),
        year,
        units: UnitCounts::Whole,
    }
}

/// What `items` returns for the 12.3.0 fixtures.
fn expected_items() -> Vec<MediaItem> {
    vec![
        movie(
            "0f1e2d3c4b5a69788796a5b4c3d2e1f0",
            "The Matrix",
            Some(1999),
            ProviderIds {
                tmdb: Some(603),
                tvdb: None,
                imdb: Some("tt0133093".into()),
                ..ProviderIds::default()
            },
        ),
        MediaItem {
            remote_id: "1a2b3c4d5e6f708192a3b4c5d6e7f809".into(),
            kind: ItemKind::Series,
            provider_ids: ProviderIds {
                tmdb: Some(1396),
                tvdb: Some(81189),
                imdb: Some("tt0903747".into()),
                ..ProviderIds::default()
            },
            title: "Breaking Bad".into(),
            year: Some(2008),
            // Season 1: episodes 1, 2 and the double episode 3–4;
            // season 2: one file, one virtual (missing) episode.
            units: UnitCounts::Seasons(vec![
                SeasonPresence {
                    number: 0,
                    episodes: 1,
                },
                SeasonPresence {
                    number: 1,
                    episodes: 4,
                },
                SeasonPresence {
                    number: 2,
                    episodes: 1,
                },
            ]),
        },
        movie(
            "2b3c4d5e6f708192a3b4c5d6e7f8091a",
            "Home Recording",
            None,
            ProviderIds::default(),
        ),
        movie(
            "3c4d5e6f708192a3b4c5d6e7f8091a2b",
            "Dune: Part Two",
            Some(2024),
            ProviderIds {
                tmdb: Some(693_134),
                tvdb: None,
                imdb: Some("tt15239678".into()),
                ..ProviderIds::default()
            },
        ),
        MediaItem {
            remote_id: "4d5e6f708192a3b4c5d6e7f8091a2b3c".into(),
            kind: ItemKind::Series,
            provider_ids: ProviderIds::default(),
            title: "Fresh Show".into(),
            year: Some(2026),
            units: UnitCounts::Seasons(vec![]),
        },
    ]
}

#[tokio::test]
async fn jellyfin_passes_the_library_contract() {
    let emby_info = info("12.3.0")
        .body
        .replace("\"ProductName\": \"Jellyfin Server\",", "");
    assert!(!emby_info.contains("ProductName"));
    run_library_contract(&LibraryContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        build: Jellyfin::new,
        info: library::Fixture {
            path: "/System/Info".into(),
            body: read(LIBRARY_VERSION, "system_info.json"),
        },
        expected_server_id: "5f2b8c0e1a3d4b6f9e7c2a1d0b3e4f5a".into(),
        other_product_info: emby_info,
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
                id: "f137a2dd21bbc1b99aa5c0f6bf02a805".into(),
                grant_id: Some("f137a2dd21bbc1b99aa5c0f6bf02a805".into()),
                name: "Movies".into(),
                content: LibraryContent::Movies,
            },
            MediaLibrary {
                id: "a656b907eb3a73532e40e44b968d0225".into(),
                grant_id: Some("a656b907eb3a73532e40e44b968d0225".into()),
                name: "Shows".into(),
                content: LibraryContent::Series,
            },
            MediaLibrary {
                id: "7e64e319657a9516ec78490da03edccb".into(),
                grant_id: Some("7e64e319657a9516ec78490da03edccb".into()),
                name: "Music".into(),
                content: LibraryContent::Music,
            },
            MediaLibrary {
                id: "1b2c3d4e5f60718293a4b5c6d7e8f901".into(),
                grant_id: Some("1b2c3d4e5f60718293a4b5c6d7e8f901".into()),
                name: "Home videos".into(),
                content: LibraryContent::Other,
            },
        ],
        library_id: "a656b907eb3a73532e40e44b968d0225".into(),
        items_path: "/Items".into(),
        user_query: Some(("UserId", ADMIN.into())),
        primary_items: read(LIBRARY_VERSION, "items_movies_series.json"),
        episodes: read(LIBRARY_VERSION, "items_episodes.json"),
        expected_items: expected_items(),
        unauthorized: unauthorized(),
    })
    .await;
}

#[test]
fn descriptor_declares_libraries_accounts_and_a_tested_range() {
    use starr_core::adapters::{Capability, Version};
    assert_eq!(DESCRIPTOR.type_id, "jellyfin");
    assert_eq!(
        DESCRIPTOR.capabilities,
        &[Capability::Libraries, Capability::Accounts]
    );
    assert!(
        DESCRIPTOR
            .tested_versions
            .contains(&Version::new(12, 4, 0, 0))
    );
    assert!(
        !DESCRIPTOR
            .tested_versions
            .contains(&Version::new(12, 5, 0, 0))
    );
}
