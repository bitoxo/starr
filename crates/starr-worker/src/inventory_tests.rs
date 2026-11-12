#![allow(clippy::unwrap_used)]

use secrecy::SecretString;
use serde_json::{Value, json};
use starr_core::jobs::{Expectation, ItemOutcome};
use starr_db::services::{NewServiceInstance, StoredCredential};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::secrets::CredentialKey;

const KEY: &str = "jellyfin-key-0123456789";
const AUTH: &str = "MediaBrowser Token=\"jellyfin-key-0123456789\"";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/jellyfin/fixtures/12.3.0/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// Serves the fixture items by type, all in one page with a total.
struct Items;

impl Respond for Items {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let episodes = request
            .url
            .query_pairs()
            .any(|(k, v)| k == "IncludeItemTypes" && v == "Episode");
        let library = request
            .url
            .query_pairs()
            .find(|(k, _)| k == "ParentId")
            .map_or_default(|(_, v)| v.into_owned());
        let albums = request
            .url
            .query_pairs()
            .any(|(k, v)| k == "IncludeItemTypes" && v == "MusicAlbum")
            && request
                .url
                .query_pairs()
                .any(|(k, v)| k == "Fields" && v.split(',').any(|f| f == "ChildCount"));
        // Everything lives in the "Shows" library, albums in "Music";
        // "Movies" is empty.
        let items: Value = if library == "7e64e319657a9516ec78490da03edccb" {
            if albums {
                serde_json::from_str(&fixture("derived_items_music_albums.json")).unwrap()
            } else {
                json!([])
            }
        } else if library == "a656b907eb3a73532e40e44b968d0225" && !albums {
            serde_json::from_str(&fixture(if episodes {
                "items_episodes.json"
            } else {
                "items_movies_series.json"
            }))
            .unwrap()
        } else {
            json!([])
        };
        let total = items.as_array().map_or(0, Vec::len);
        ResponseTemplate::new(200)
            .set_body_json(json!({ "Items": items, "TotalRecordCount": total }))
    }
}

async fn fake_jellyfin() -> MockServer {
    let server = MockServer::start().await;
    for (route, file) in [
        ("/System/Info", "system_info.json"),
        ("/Users", "users.json"),
        ("/Library/VirtualFolders", "virtual_folders.json"),
    ] {
        Mock::given(path(route))
            .and(header("authorization", AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture(file)))
            .mount(&server)
            .await;
    }
    Mock::given(path("/Items"))
        .and(header("authorization", AUTH))
        .respond_with(Items)
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    server
}

fn seal(key: &CredentialKey, value: &str) -> StoredCredential {
    let sealer = starr_secrets::CredentialSealer::parse(key.recipient()).unwrap();
    let (recipient, ciphertext) = sealer
        .seal(&SecretString::from(value.to_owned()))
        .unwrap()
        .into_parts();
    StoredCredential {
        ciphertext,
        recipient,
        class: starr_core::adapters::SecretClass::ServiceCredential,
    }
}

/// Let the movies category watch in instance `id`.
async fn watch_in(db: &Database, id: i64) {
    db.add_role(
        &starr_db::categories::NewRole {
            category: starr_core::members::Category::Movies,
            kind: starr_core::categories::RoleKind::Consume,
            service_instance_id: id,
            offered: &[starr_core::adapters::Capability::Libraries],
        },
        Utc::now(),
    )
    .await
    .unwrap();
}

async fn add(db: &Database, jobs: &SystemJobs, adapter_type: &str, url: &str, key: &str) -> i64 {
    db.create_service(
        &NewServiceInstance {
            adapter_type,
            name: &format!("{adapter_type}-{url}"),
            base_url: url,
            public_url: None,
            credential: &seal(&jobs.credentials, key),
        },
        Utc::now(),
    )
    .await
    .unwrap()
    .id
}

fn ctx(db: &Database) -> JobContext {
    JobContext {
        db: db.clone(),
        payload: json!({}),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn inventory_sync_stores_matched_items_of_consume_instances() {
    let db = Database::open_in_memory().await.unwrap();
    let server = fake_jellyfin().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, &jobs, "jellyfin", &server.uri(), KEY).await;
    watch_in(&db, id).await;
    // A Jellyfin no category watches in: never listed.
    add(&db, &jobs, "jellyfin", "http://unassigned.invalid", KEY).await;
    assert!(jobs.has_work(JobKind::InventorySync, &db).await.unwrap());

    let report = jobs.run(JobKind::InventorySync, ctx(&db)).await.unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    assert_eq!(report.items().len(), 1);
    assert_eq!(report.counter("appeared"), Some(3));
    assert_eq!(report.counter("unmatched"), Some(2));

    let rows = db.inventory(id).await.unwrap();
    let keys: Vec<_> = rows.iter().map(|r| r.natural_key.as_str()).collect();
    assert_eq!(
        keys,
        ["movie:tmdb:603", "movie:tmdb:693134", "series:tvdb:81189"]
    );
    let bb = &rows[2];
    assert_eq!(bb.seasons, [(0, 1), (1, 4), (2, 1)].into());
    assert_eq!(
        bb.server_id.as_deref(),
        Some("5f2b8c0e1a3d4b6f9e7c2a1d0b3e4f5a")
    );
    assert!(bb.present_since.is_some());

    // The probe of the registry entry works over the same client.
    let report = jobs
        .run(
            JobKind::ServicesCheck,
            JobContext {
                payload: json!({ "instanceId": id }),
                ..ctx(&db)
            },
        )
        .await
        .unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    let service = db.get_service(id).await.unwrap().unwrap();
    assert_eq!(
        service.capabilities,
        vec!["libraries".to_owned(), "accounts".to_owned()]
    );
}

#[tokio::test]
async fn a_failing_instance_is_reported_and_changes_nothing() {
    let db = Database::open_in_memory().await.unwrap();
    let server = fake_jellyfin().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, &jobs, "jellyfin", &server.uri(), "wrong-key").await;
    watch_in(&db, id).await;

    let report = jobs.run(JobKind::InventorySync, ctx(&db)).await.unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Unmet(_)),
        "{report:?}"
    );
    let item = &report.items()[0];
    assert_eq!(item.outcome, ItemOutcome::Failed);
    assert_eq!(item.code.as_deref(), Some("credentials_invalid"));
    assert_eq!(db.inventory(id).await.unwrap(), vec![]);
}

#[tokio::test]
async fn without_consume_instances_the_sync_has_nothing_to_do() {
    let db = Database::open_in_memory().await.unwrap();
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    add(&db, &jobs, "jellyfin", "http://unassigned.invalid", KEY).await;
    assert!(!jobs.has_work(JobKind::InventorySync, &db).await.unwrap());
    let report = jobs.run(JobKind::InventorySync, ctx(&db)).await.unwrap();
    assert_eq!(report.expectation(), &Expectation::NotApplicable);
}

/// Let `category` watch in instance `id`.
async fn watch_for(db: &Database, category: starr_core::members::Category, id: i64) {
    db.add_role(
        &starr_db::categories::NewRole {
            category,
            kind: starr_core::categories::RoleKind::Consume,
            service_instance_id: id,
            offered: &[starr_core::adapters::Capability::Libraries],
        },
        Utc::now(),
    )
    .await
    .unwrap();
}

const OK_COMPUTER: &str = "album:mbrg:b1392450-e666-3926-a536-22c65f834433";

#[tokio::test]
async fn jellyfin_music_libraries_are_read_only_for_the_music_category() {
    let db = Database::open_in_memory().await.unwrap();
    let server = fake_jellyfin().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, &jobs, "jellyfin", &server.uri(), KEY).await;
    watch_in(&db, id).await;
    jobs.run(JobKind::InventorySync, ctx(&db)).await.unwrap();
    assert!(
        db.inventory(id)
            .await
            .unwrap()
            .iter()
            .all(|r| r.kind != "album"),
        "movies alone never read the music library"
    );

    watch_for(&db, starr_core::members::Category::Music, id).await;
    let report = jobs.run(JobKind::InventorySync, ctx(&db)).await.unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    // The untagged album is unmatched (never by name), besides the two
    // unmatched videos.
    assert_eq!(report.counter("unmatched"), Some(3));
    let rows = db.inventory(id).await.unwrap();
    let album = rows.iter().find(|r| r.natural_key == OK_COMPUTER).unwrap();
    assert_eq!(album.kind, "album");
    assert_eq!(album.tracks, Some(12));
    assert_eq!(album.remote_id, "8a1b2c3d4e5f60718293a4b5c6d7e8f9");
    assert_eq!(
        album.library_ids,
        Some(vec!["7e64e319657a9516ec78490da03edccb".to_owned()])
    );
}

fn navidrome_fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/navidrome/fixtures/0.64.2/derived/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// A Navidrome that knows the admin `starr` and answers its token.
async fn fake_navidrome() -> MockServer {
    use wiremock::matchers::{body_json, method};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/ping"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(navidrome_fixture("ping_without_login.json")),
        )
        .mount(&server)
        .await;
    let mut login: Value = serde_json::from_str(&navidrome_fixture("login.json")).unwrap();
    login["token"] = json!("nd-token");
    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .and(body_json(
            json!({ "username": "starr", "password": "nd-pass" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(login))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/library"))
        .and(header("x-nd-authorization", "Bearer nd-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-total-count", "2")
                .set_body_string(navidrome_fixture("library.json")),
        )
        .mount(&server)
        .await;
    let albums: Value = serde_json::from_str(&navidrome_fixture("albums_library_1.json")).unwrap();
    Mock::given(method("GET"))
        .and(path("/api/album"))
        .and(wiremock::matchers::query_param("library_id", "1"))
        .and(header("x-nd-authorization", "Bearer nd-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-total-count", "4")
                .set_body_json(albums),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/album"))
        .and(wiremock::matchers::query_param("library_id", "2"))
        .and(header("x-nd-authorization", "Bearer nd-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-total-count", "0")
                .set_body_json(json!([])),
        )
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn navidrome_albums_and_libraries_are_read_with_one_login() {
    let db = Database::open_in_memory().await.unwrap();
    let server = fake_navidrome().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let login = starr_adapter_kit::LoginCredential::encode(
        "starr",
        &SecretString::from("nd-pass".to_owned()),
    );
    let id = add(
        &db,
        &jobs,
        "navidrome",
        &server.uri(),
        secrecy::ExposeSecret::expose_secret(&login),
    )
    .await;
    watch_for(&db, starr_core::members::Category::Music, id).await;

    for _ in 0..2 {
        let report = jobs.run(JobKind::InventorySync, ctx(&db)).await.unwrap();
        assert!(
            matches!(report.expectation(), Expectation::Met(_)),
            "{report:?}"
        );
    }
    let rows = db.inventory(id).await.unwrap();
    let keys: Vec<_> = rows
        .iter()
        .map(|r| (r.natural_key.as_str(), r.tracks))
        .collect();
    assert_eq!(
        keys,
        [
            ("album:mbrg:1b022e01-4da6-387b-8658-8678046e4cef", Some(7)),
            (OK_COMPUTER, Some(12)),
        ],
        "the untagged and the missing album are not stored"
    );
    assert_eq!(rows[1].server_id, None);
    // Navidrome has accounts (spec 0006 slice 7): `accounts.scan` keeps its
    // library list for the admin's mapping, the sync leaves it alone.
    assert_eq!(db.service_libraries(id).await.unwrap(), vec![]);
    // Only reads besides the one login (two runs share the token).
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|r| r.method.as_str() == "GET" || r.url.path() == "/auth/login"),
        "{requests:?}"
    );
}
