#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicU32, Ordering};

use secrecy::SecretString;
use serde_json::json;
use starr_core::jobs::{Expectation, ItemOutcome};
use starr_db::services::{NewServiceInstance, StoredCredential};
use starr_secrets::{SecretKey, ServiceCredentials};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::secrets::{CredentialKey, RECIPIENT_META_KEY, SecretsInitError};

const KEY: &str = "radarr-key-0123456789";

fn fixture() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adapters/radarr/fixtures/6.4.4.10685/system_status.json"
    ))
    .unwrap()
}

async fn fake_radarr() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/api/v3/system/status"))
        .and(header("X-Api-Key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture()))
        .mount(&server)
        .await;
    Mock::given(path("/api/v3/system/status"))
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

async fn add(db: &Database, name: &str, url: &str, credential: &StoredCredential) -> i64 {
    db.create_service(
        &NewServiceInstance {
            adapter_type: "radarr",
            name,
            base_url: url,
            public_url: None,
            credential,
        },
        Utc::now(),
    )
    .await
    .unwrap()
    .id
}

fn ctx(db: &Database, payload: serde_json::Value) -> JobContext {
    JobContext {
        db: db.clone(),
        payload,
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn successful_check_stores_version_and_capabilities() {
    let db = Database::open_in_memory().await.unwrap();
    let server = fake_radarr().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, "Movies", &server.uri(), &seal(&jobs.credentials, KEY)).await;

    let report = jobs
        .run(
            JobKind::ServicesCheck,
            ctx(&db, json!({ "instanceId": id })),
        )
        .await
        .unwrap();

    assert!(matches!(report.expectation(), Expectation::Met(_)));
    assert_eq!(report.items().len(), 1);
    assert_eq!(report.items()[0].outcome, ItemOutcome::Succeeded);
    let s = db.get_service(id).await.unwrap().unwrap();
    assert_eq!(s.detected_version.as_deref(), Some("6.4.4.10685"));
    assert_eq!(s.capabilities, vec!["requests"]);
    assert!(s.last_check_at.is_some());
    assert_eq!(s.last_error, None);
}

#[tokio::test]
async fn sonarr_instances_are_checked_with_the_sonarr_probe() {
    let db = Database::open_in_memory().await.unwrap();
    let server = MockServer::start().await;
    let status = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adapters/sonarr/fixtures/4.0.20.3014/system_status.json"
    ))
    .unwrap();
    Mock::given(path("/api/v3/system/status"))
        .and(header("X-Api-Key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_string(status))
        .mount(&server)
        .await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = db
        .create_service(
            &NewServiceInstance {
                adapter_type: "sonarr",
                name: "Series",
                base_url: &server.uri(),
                public_url: None,
                credential: &seal(&jobs.credentials, KEY),
            },
            Utc::now(),
        )
        .await
        .unwrap()
        .id;

    let report = jobs
        .run(
            JobKind::ServicesCheck,
            ctx(&db, json!({ "instanceId": id })),
        )
        .await
        .unwrap();

    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    let s = db.get_service(id).await.unwrap().unwrap();
    assert_eq!(s.detected_version.as_deref(), Some("4.0.20.3014"));
    assert_eq!(s.capabilities, vec!["requests"]);
    assert_eq!(s.last_error, None);
}

#[tokio::test]
async fn failed_checks_store_the_taxonomy_code_and_mark_the_report_unmet() {
    let db = Database::open_in_memory().await.unwrap();
    let server = fake_radarr().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let wrong = add(
        &db,
        "Wrong key",
        &server.uri(),
        &seal(&jobs.credentials, "nope"),
    )
    .await;
    // Sealed for a key this worker does not hold.
    let other = CredentialKey::ephemeral();
    let foreign = add(&db, "Foreign", &server.uri(), &seal(&other, KEY)).await;
    let ok = add(&db, "Good", &server.uri(), &seal(&jobs.credentials, KEY)).await;

    // Empty payload checks every instance.
    let report = jobs
        .run(JobKind::ServicesCheck, ctx(&db, json!({})))
        .await
        .unwrap();

    assert!(
        matches!(report.expectation(), Expectation::Unmet(_)),
        "{report:?}"
    );
    assert_eq!(report.failed(), 2);
    let code = |id| {
        let db = db.clone();
        async move { db.get_service(id).await.unwrap().unwrap().last_error }
    };
    assert_eq!(code(wrong).await.as_deref(), Some("credentials_invalid"));
    assert_eq!(
        code(foreign).await.as_deref(),
        Some("credentials_unreadable")
    );
    assert_eq!(code(ok).await, None);
    // The detail never contains the credential.
    let printed = format!("{report:?}");
    assert!(!printed.contains(KEY), "{printed}");
}

#[tokio::test]
async fn removed_instance_is_skipped_and_empty_runs_are_not_applicable() {
    let db = Database::open_in_memory().await.unwrap();
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let report = jobs
        .run(
            JobKind::ServicesCheck,
            ctx(&db, json!({ "instanceId": 42 })),
        )
        .await
        .unwrap();
    assert_eq!(report.items()[0].outcome, ItemOutcome::Skipped);
    assert_eq!(report.expectation(), &Expectation::NotApplicable);

    let report = jobs
        .run(JobKind::ServicesCheck, ctx(&db, json!({})))
        .await
        .unwrap();
    assert_eq!(report.items(), &[]);
    assert!(
        jobs.run(
            JobKind::ServicesCheck,
            ctx(&db, json!({ "instanceId": "x" }))
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn one_client_per_instance_is_reused_across_checks() {
    let db = Database::open_in_memory().await.unwrap();
    let server = MockServer::start().await;
    let calls = std::sync::Arc::new(AtomicU32::new(0));
    let counter = calls.clone();
    Mock::given(path("/api/v3/system/status"))
        .respond_with(move |_: &wiremock::Request| {
            counter.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(429).insert_header("retry-after", "60")
        })
        .mount(&server)
        .await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, "Movies", &server.uri(), &seal(&jobs.credentials, KEY)).await;
    for _ in 0..3 {
        jobs.run(
            JobKind::ServicesCheck,
            ctx(&db, json!({ "instanceId": id })),
        )
        .await
        .unwrap();
    }
    // The 429 gate lives in the shared client: only the first check calls out.
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let s = db.get_service(id).await.unwrap().unwrap();
    assert_eq!(s.last_error.as_deref(), Some("throttled"));
}

/// Review finding: a credential changed while a check runs must still be
/// checked. The pending-job dedup hands the update the running job, so that
/// job re-probes the new connection instead of reporting the stale result.
#[tokio::test]
async fn a_credential_changed_during_the_check_is_checked_again() {
    let db = Database::open_in_memory().await.unwrap();
    let server = MockServer::start().await;
    Mock::given(path("/api/v3/system/status"))
        .and(header("X-Api-Key", KEY))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fixture())
                .set_delay(std::time::Duration::from_millis(400)),
        )
        .mount(&server)
        .await;
    Mock::given(path("/api/v3/system/status"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let jobs = std::sync::Arc::new(SystemJobs::new(CredentialKey::ephemeral()));
    let id = add(&db, "Movies", &server.uri(), &seal(&jobs.credentials, KEY)).await;

    let running = {
        let (jobs, db) = (jobs.clone(), db.clone());
        tokio::spawn(async move {
            jobs.run(
                JobKind::ServicesCheck,
                ctx(&db, json!({ "instanceId": id })),
            )
            .await
            .unwrap()
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    db.update_service(
        id,
        &starr_db::services::ServiceUpdate {
            credential: Some(&seal(&jobs.credentials, "a-new-wrong-key")),
            ..Default::default()
        },
        Utc::now(),
    )
    .await
    .unwrap()
    .unwrap();

    let report = running.await.unwrap();
    assert_eq!(report.items()[0].outcome, ItemOutcome::Failed, "{report:?}");
    assert_eq!(
        report.items()[0].code.as_deref(),
        Some("credentials_invalid")
    );
    let s = db.get_service(id).await.unwrap().unwrap();
    assert_eq!(s.last_error.as_deref(), Some("credentials_invalid"));
    assert!(s.last_check_at.is_some());
}

/// A fresh directory under the system temp dir, removed on drop.
pub(crate) struct TempDir(pub(crate) std::path::PathBuf);

impl TempDir {
    pub(crate) fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("starr-worker-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn key_init_publishes_the_recipient() {
    let dir = TempDir::new("key-init");
    let path = dir.0.join("secret.key");
    let db = Database::open_in_memory().await.unwrap();

    let key = CredentialKey::init(&db, &path).await.unwrap();
    assert_eq!(
        db.meta_get(RECIPIENT_META_KEY).await.unwrap().as_deref(),
        Some(key.recipient())
    );
    let id = add(&db, "Mine", "http://radarr", &seal(&key, KEY)).await;
    // Restarting with the same file keeps everything readable.
    let again = CredentialKey::init(&db, &path).await.unwrap();
    assert_eq!(again.recipient(), key.recipient());
    assert_eq!(db.get_service(id).await.unwrap().unwrap().last_error, None);
}

#[tokio::test]
async fn a_lost_key_file_starts_with_a_new_key_and_flags_sealed_services() {
    let dir = TempDir::new("key-lost");
    let path = dir.0.join("secret.key");
    let db = Database::open_in_memory().await.unwrap();
    let old = CredentialKey::init(&db, &path).await.unwrap();
    let id = add(&db, "Movies", "http://radarr", &seal(&old, KEY)).await;

    std::fs::remove_file(&path).unwrap();
    let new = CredentialKey::init(&db, &path).await.unwrap();

    assert_ne!(new.recipient(), old.recipient());
    assert!(path.exists(), "a new key file is written");
    assert_eq!(
        db.meta_get(RECIPIENT_META_KEY).await.unwrap().as_deref(),
        Some(new.recipient())
    );
    let s = db.get_service(id).await.unwrap().unwrap();
    assert_eq!(s.last_error.as_deref(), Some("credentials_unreadable"));
    assert!(s.last_check_at.is_some());
}

#[tokio::test]
async fn a_replaced_key_file_flags_only_services_sealed_for_the_old_key() {
    let dir = TempDir::new("key-mismatch");
    let path = dir.0.join("secret.key");
    let db = Database::open_in_memory().await.unwrap();
    let old = CredentialKey::init(&db, &path).await.unwrap();
    let stale = add(&db, "Old", "http://radarr", &seal(&old, KEY)).await;

    // The operator put a different (valid) key file in place.
    std::fs::remove_file(&path).unwrap();
    let other = SecretKey::<ServiceCredentials>::load_or_create(&path)
        .unwrap()
        .0;
    let key = CredentialKey::init(&db, &path).await.unwrap();
    assert_eq!(key.recipient(), other.recipient(), "the file's key is used");
    let fresh = add(&db, "New", "http://radarr", &seal(&key, KEY)).await;
    // Another restart flags nothing new.
    CredentialKey::init(&db, &path).await.unwrap();

    let error = |id| {
        let db = db.clone();
        async move { db.get_service(id).await.unwrap().unwrap().last_error }
    };
    assert_eq!(
        error(stale).await.as_deref(),
        Some("credentials_unreadable")
    );
    assert_eq!(error(fresh).await, None);
}

#[tokio::test]
async fn restoring_the_original_key_clears_the_flag() {
    let dir = TempDir::new("key-restore");
    let path = dir.0.join("secret.key");
    let backup = dir.0.join("secret.key.bak");
    let db = Database::open_in_memory().await.unwrap();
    let original = CredentialKey::init(&db, &path).await.unwrap();
    let id = add(&db, "Movies", "http://radarr", &seal(&original, KEY)).await;
    std::fs::rename(&path, &backup).unwrap();

    // One start without the key flags the service ...
    CredentialKey::init(&db, &path).await.unwrap();
    let flagged = db.get_service(id).await.unwrap().unwrap();
    assert_eq!(
        flagged.last_error.as_deref(),
        Some("credentials_unreadable")
    );

    // ... restoring the backup and restarting clears it again.
    std::fs::rename(&backup, &path).unwrap();
    let restored = CredentialKey::init(&db, &path).await.unwrap();
    assert_eq!(restored.recipient(), original.recipient());
    let s = db.get_service(id).await.unwrap().unwrap();
    assert_eq!(s.last_error, None);
    assert_eq!(s.last_check_at, None, "shown as not checked yet");
}

#[cfg(unix)]
#[tokio::test]
async fn a_key_file_writable_by_others_refuses_to_start() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("key-writable");
    let path = dir.0.join("secret.key");
    let db = Database::open_in_memory().await.unwrap();
    CredentialKey::init(&db, &path).await.unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o620)).unwrap();
    let err = CredentialKey::init(&db, &path).await.unwrap_err();
    assert!(matches!(err, SecretsInitError::Key(_)), "{err}");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    CredentialKey::init(&db, &path).await.unwrap();
}

#[tokio::test]
async fn a_malformed_key_file_still_refuses_to_start() {
    let dir = TempDir::new("key-malformed");
    let path = dir.0.join("secret.key");
    let db = Database::open_in_memory().await.unwrap();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&path).unwrap(), b"not a key").unwrap();
    let err = CredentialKey::init(&db, &path).await.unwrap_err();
    assert!(matches!(err, SecretsInitError::Key(_)), "{err}");
}
fn radarr_fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/radarr/fixtures/6.4.4.10685/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[tokio::test]
async fn source_lists_are_read_and_stored_and_failures_recorded() {
    let db = Database::open_in_memory().await.unwrap();
    let server = MockServer::start().await;
    for (route, file) in [
        ("/api/v3/system/status", "system_status.json"),
        ("/api/v3/qualityprofile", "qualityprofile.json"),
        ("/api/v3/rootfolder", "rootfolder.json"),
    ] {
        Mock::given(path(route))
            .and(header("X-Api-Key", KEY))
            .respond_with(ResponseTemplate::new(200).set_body_string(radarr_fixture(file)))
            .mount(&server)
            .await;
    }
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, "Radarr", &server.uri(), &seal(&jobs.credentials, KEY)).await;
    let report = jobs
        .run(JobKind::SourcesLists, ctx(&db, json!({ "instanceId": id })))
        .await
        .unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    let lists = db.source_lists(id).await.unwrap().unwrap();
    assert_eq!(
        lists
            .quality_profiles
            .iter()
            .map(|p| (p.id, p.name.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "Any"), (4, "HD-1080p")]
    );
    assert_eq!(lists.root_folders.len(), 2);
    assert!(lists.fetched_at.is_some());
    assert_eq!(lists.error, None);
    assert_eq!(report.counter("settings_cleared"), Some(0));

    // Settings the server does not offer (another Radarr behind a changed
    // address, spec 0008 ADM-01) are cleared; the others stay.
    let role = db
        .add_role(
            &starr_db::categories::NewRole {
                category: starr_core::members::Category::Movies,
                kind: starr_core::categories::RoleKind::Source,
                service_instance_id: id,
                offered: &[starr_core::adapters::Capability::Requests],
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let chosen = starr_core::categories::SourceSettings {
        quality_profile_id: Some(9),
        root_folder_path: Some("/movies/".into()),
        minimum_availability: None,
        metadata_profile_id: None,
    };
    db.set_source_settings(role.id, &chosen, chrono::Utc::now())
        .await
        .unwrap();
    let report = jobs
        .run(JobKind::SourcesLists, ctx(&db, json!({ "instanceId": id })))
        .await
        .unwrap();
    assert_eq!(report.counter("settings_cleared"), Some(1), "{report:?}");
    let kept = db.get_role(role.id).await.unwrap().unwrap();
    assert_eq!(
        kept.source_settings().unwrap(),
        starr_core::categories::SourceSettings {
            quality_profile_id: None,
            ..chosen
        }
    );

    // A wrong key: the error is stored, the earlier lists stay.
    let wrong = add(
        &db,
        "Radarr 2",
        &server.uri(),
        &seal(&jobs.credentials, "nope"),
    )
    .await;
    let report = jobs
        .run(
            JobKind::SourcesLists,
            ctx(&db, json!({ "instanceId": wrong })),
        )
        .await
        .unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Unmet(_)),
        "{report:?}"
    );
    let failed = db.source_lists(wrong).await.unwrap().unwrap();
    assert!(failed.error.is_some());
    assert_eq!(failed.quality_profiles, vec![]);
}

#[tokio::test]
async fn lidarr_lists_include_its_metadata_profiles() {
    let db = Database::open_in_memory().await.unwrap();
    let server = MockServer::start().await;
    for (route, file) in [
        ("/api/v1/qualityprofile", "qualityprofile.json"),
        ("/api/v1/rootfolder", "rootfolder.json"),
        ("/api/v1/metadataprofile", "metadataprofile.json"),
    ] {
        let body = std::fs::read_to_string(format!(
            "{}/../adapters/lidarr/fixtures/3.1.6.5079/recorded/{file}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        Mock::given(path(route))
            .and(header("X-Api-Key", KEY))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
    }
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = db
        .create_service(
            &NewServiceInstance {
                adapter_type: "lidarr",
                name: "Lidarr",
                base_url: &server.uri(),
                public_url: None,
                credential: &seal(&jobs.credentials, KEY),
            },
            Utc::now(),
        )
        .await
        .unwrap()
        .id;
    let report = jobs
        .run(JobKind::SourcesLists, ctx(&db, json!({ "instanceId": id })))
        .await
        .unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    let lists = db.source_lists(id).await.unwrap().unwrap();
    assert_eq!(lists.quality_profiles.len(), 4);
    assert_eq!(lists.root_folders.len(), 1);
    assert_eq!(
        lists
            .metadata_profiles
            .iter()
            .map(|p| (p.id, p.admits_albums_and_eps()))
            .collect::<Vec<_>>(),
        // Lidarr's shipped profiles: "Standard" drops EPs, "None" drops all.
        vec![(1, false), (2, false)]
    );
    // Reading lists writes nothing.
    let writes = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method != wiremock::http::Method::GET)
        .count();
    assert_eq!(writes, 0);
}

fn whisparr_fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/whisparr/fixtures/3.1.0.2116/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// Spec 0007 slice 3: Whisparr's lists are read like Radarr's, nothing is
/// written, and a failure is stored without Whisparr's words or the path
/// (private content, ADR 0014).
#[tokio::test]
async fn whisparr_lists_are_read_and_its_failures_quote_nothing() {
    let db = Database::open_in_memory().await.unwrap();
    let server = MockServer::start().await;
    for (route, file) in [
        ("/api/v3/qualityprofile", "qualityprofile.json"),
        ("/api/v3/rootfolder", "rootfolder.json"),
    ] {
        Mock::given(path(route))
            .and(header("X-Api-Key", KEY))
            .respond_with(ResponseTemplate::new(200).set_body_string(whisparr_fixture(file)))
            .mount(&server)
            .await;
    }
    let broken = MockServer::start().await;
    Mock::given(path("/api/v3/qualityprofile"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string(r#"[{"errorMessage": "Scene A by Studio S"}]"#),
        )
        .mount(&broken)
        .await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let mut ids = Vec::new();
    for (name, url) in [("Whisparr", server.uri()), ("Whisparr 2", broken.uri())] {
        let id = db
            .create_service(
                &NewServiceInstance {
                    adapter_type: "whisparr",
                    name,
                    base_url: &url,
                    public_url: None,
                    credential: &seal(&jobs.credentials, KEY),
                },
                Utc::now(),
            )
            .await
            .unwrap()
            .id;
        ids.push(id);
    }
    let report = jobs
        .run(
            JobKind::SourcesLists,
            ctx(&db, json!({ "instanceId": ids[0] })),
        )
        .await
        .unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    let lists = db.source_lists(ids[0]).await.unwrap().unwrap();
    assert_eq!(
        lists
            .quality_profiles
            .iter()
            .map(|p| (p.id, p.name.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "Any"), (3, "HD-1080p")]
    );
    assert_eq!(lists.root_folders[0].path, "/data/private");
    assert_eq!(lists.metadata_profiles, vec![]);

    let report = jobs
        .run(
            JobKind::SourcesLists,
            ctx(&db, json!({ "instanceId": ids[1] })),
        )
        .await
        .unwrap();
    let failed = db.source_lists(ids[1]).await.unwrap().unwrap();
    let stored = format!("{:?} {report:?}", failed.error);
    assert!(failed.error.is_some(), "{stored}");
    for marker in ["Scene A", "Studio S", "qualityprofile"] {
        assert!(!stored.contains(marker), "{marker:?} in {stored}");
    }
    for server in [&server, &broken] {
        let writes = server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.method != wiremock::http::Method::GET)
            .count();
        assert_eq!(writes, 0);
    }
}
