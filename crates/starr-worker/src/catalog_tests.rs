#![allow(clippy::unwrap_used)]

use secrecy::SecretString;
use serde_json::json;
use starr_core::adapters::Probe;
use starr_core::catalog::{Catalog, CatalogRef, MediaKind};
use starr_core::jobs::ItemOutcome;
use starr_db::services::{NewServiceInstance, StoredCredential};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::catalog::{AnyCatalog, CATALOG_RECIPIENT_META_KEY, CatalogAccess, CatalogOpenError};
use crate::image_cache::{ImageCache, ImageFormat};
use crate::secrets::{CredentialKey, ResealReport};
use crate::services_tests::TempDir;
use starr_core::adapters::SecretClass;
use starr_secrets::CatalogTokenKey;

const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.worker-test";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/tmdb/fixtures/v3/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// Sealed with the worker's key, as every credential was before ADR 0003
/// amendment 1.
fn seal(key: &CredentialKey, value: &str) -> StoredCredential {
    let sealer = starr_secrets::CredentialSealer::parse(key.recipient()).unwrap();
    let (recipient, ciphertext) = sealer
        .seal(&SecretString::from(value.to_owned()))
        .unwrap()
        .into_parts();
    StoredCredential {
        ciphertext,
        recipient,
        class: SecretClass::ServiceCredential,
    }
}

/// Sealed with the web key, as catalog tokens are now.
fn seal_catalog(access: &CatalogAccess, value: &str) -> StoredCredential {
    let (recipient, ciphertext) = access
        .sealer()
        .seal(&SecretString::from(value.to_owned()))
        .unwrap()
        .into_parts();
    StoredCredential {
        ciphertext,
        recipient,
        class: SecretClass::CatalogReadToken,
    }
}

/// A worker as in single-process mode: worker key plus the web key.
fn single_process_jobs() -> SystemJobs {
    SystemJobs::new(CredentialKey::ephemeral())
        .with_catalog(Arc::new(CatalogAccess::new(CatalogTokenKey::generate())))
}

async fn insert(
    db: &Database,
    adapter_type: &str,
    url: &str,
    credential: &StoredCredential,
) -> i64 {
    db.create_service(
        &NewServiceInstance {
            adapter_type,
            name: &format!("{adapter_type}-{}", uuid::Uuid::new_v4()),
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

/// Add an instance sealed the way the API seals it: by the adapter's
/// secret class.
async fn add(db: &Database, jobs: &SystemJobs, adapter_type: &str, url: &str) -> i64 {
    let credential = match crate::adapters::descriptor(adapter_type)
        .unwrap()
        .secret_class
    {
        SecretClass::ServiceCredential => seal(&jobs.credentials, TOKEN),
        SecretClass::CatalogReadToken => seal_catalog(jobs.catalog.as_deref().unwrap(), TOKEN),
    };
    insert(db, adapter_type, url, &credential).await
}

fn ctx(db: &Database, payload: serde_json::Value) -> JobContext {
    JobContext {
        db: db.clone(),
        payload,
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn tmdb_check_uses_the_bearer_token() {
    let db = Database::open_in_memory().await.unwrap();
    let server = MockServer::start().await;
    Mock::given(path("/3/authentication"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("authentication.json")))
        .expect(1)
        .mount(&server)
        .await;
    let jobs = single_process_jobs();
    let id = add(&db, &jobs, "tmdb", &server.uri()).await;
    let report = jobs
        .run(
            JobKind::ServicesCheck,
            ctx(&db, json!({ "instanceId": id })),
        )
        .await
        .unwrap();
    assert!(
        matches!(report.expectation(), starr_core::jobs::Expectation::Met(_)),
        "{report:?}"
    );
    let service = db.get_service(id).await.unwrap().unwrap();
    assert_eq!(service.detected_version.as_deref(), Some("3.0.0.0"));
    assert_eq!(service.capabilities, vec!["metadata".to_owned()]);
}

fn stashdb_fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/stashdb/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// Spec 0007 Q14: every probe checks the key's roles; a key that could
/// write fails the check, a read-only one passes.
#[tokio::test]
async fn stashdb_checks_refuse_a_key_that_could_write() {
    let db = Database::open_in_memory().await.unwrap();
    let jobs = single_process_jobs();
    for (file, ok) in [("probe_read.json", true), ("probe_edit.json", false)] {
        let server = MockServer::start().await;
        Mock::given(path("/graphql"))
            .and(header("apikey", TOKEN))
            .respond_with(ResponseTemplate::new(200).set_body_string(stashdb_fixture(file)))
            .mount(&server)
            .await;
        let id = add(&db, &jobs, "stashdb", &server.uri()).await;
        jobs.run(
            JobKind::ServicesCheck,
            ctx(&db, json!({ "instanceId": id })),
        )
        .await
        .unwrap();
        let service = db.get_service(id).await.unwrap().unwrap();
        if ok {
            assert_eq!(service.last_error, None, "{file}");
            assert_eq!(service.detected_version.as_deref(), Some("0.10.3.0"));
        } else {
            assert_eq!(service.last_error.as_deref(), Some("rejected"), "{file}");
        }
    }
}

/// `MusicBrainz` takes no credential: the API seals an empty value of the
/// catalog class, and nothing but Starr's agent is ever sent.
#[tokio::test]
async fn musicbrainz_checks_searches_and_lists_without_a_credential() {
    let db = Database::open_in_memory().await.unwrap();
    let mb = MockServer::start().await;
    let lb = MockServer::start().await;
    let agent = starr_adapter_kit::USER_AGENT;
    Mock::given(path(
        "/ws/2/artist/a74b1b7f-71a5-4011-9441-d0b5e4122711",
    ))
    .and(header("user-agent", agent))
    .respond_with(ResponseTemplate::new(200).set_body_string(
        r#"{"id":"a74b1b7f-71a5-4011-9441-d0b5e4122711","name":"Radiohead","sort-name":"Radiohead"}"#,
    ))
    .expect(1)
    .mount(&mb)
    .await;
    Mock::given(path("/ws/2/release-group"))
        .and(header("user-agent", agent))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"count":1,"offset":0,"release-groups":[{"id":"b1392450-e666-3926-a536-22c65f834433","title":"OK Computer","primary-type":"Album"}]}"#,
        ))
        .expect(1)
        .mount(&mb)
        .await;
    Mock::given(path("/1/stats/sitewide/release-groups"))
        .and(header("user-agent", agent))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&lb)
        .await;
    let access = CatalogAccess::new(CatalogTokenKey::generate())
        .with_listenbrainz_base(&url::Url::parse(&lb.uri()).unwrap());
    let jobs = SystemJobs::new(CredentialKey::ephemeral()).with_catalog(Arc::new(access));
    let credential = seal_catalog(jobs.catalog.as_deref().unwrap(), "");
    let id = insert(&db, "musicbrainz", &mb.uri(), &credential).await;
    let report = jobs
        .run(
            JobKind::ServicesCheck,
            ctx(&db, json!({ "instanceId": id })),
        )
        .await
        .unwrap();
    assert!(
        matches!(report.expectation(), starr_core::jobs::Expectation::Met(_)),
        "{report:?}"
    );
    let service = db.get_service(id).await.unwrap().unwrap();
    assert_eq!(service.detected_version.as_deref(), Some("2.0.0.0"));

    let catalog = jobs.catalog(&db, id).await.unwrap();
    assert_eq!(catalog.descriptor().type_id, "musicbrainz");
    let found = catalog
        .search(&starr_core::catalog::SearchQuery {
            text: "ok computer",
            kinds: &[MediaKind::Album],
            page: 1,
            language: "en-US",
        })
        .await
        .unwrap();
    assert_eq!(found.items[0].title, "OK Computer");
    let relevance = starr_core::catalog::Relevance::music_default();
    let lane = catalog
        .list(&starr_core::catalog::ListQuery {
            kind: MediaKind::Album,
            list: starr_core::catalog::ListKind::Trending,
            page: 1,
            language: "en-US",
            relevance: &relevance,
            filter: None,
        })
        .await
        .unwrap();
    assert_eq!(lane.items.len(), 0);
    for server in [&mb, &lb] {
        server.verify().await;
        for request in server.received_requests().await.unwrap() {
            assert!(request.headers.get("authorization").is_none());
        }
    }
}

/// `catalog.refresh-lists` (spec 0008 slice 2): one `MusicBrainz` search
/// per fixed genre, stored in the catalog cache for a day, only while
/// music is on with a `MusicBrainz` catalog; the web side then reads them
/// without a request.
#[tokio::test]
async fn refresh_lists_fetches_every_genre_lane_into_the_cache() {
    let db = Database::open_in_memory().await.unwrap();
    let mb = MockServer::start().await;
    let lb = MockServer::start().await;
    // Each lane is ranked by one ListenBrainz popularity lookup.
    Mock::given(path("/1/popularity/release-group"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{"release_group_mbid":"b1392450-e666-3926-a536-22c65f834433","total_listen_count":5,"total_user_count":1}]"#,
        ))
        .mount(&lb)
        .await;
    Mock::given(path("/ws/2/release-group"))
        .and(header("user-agent", starr_adapter_kit::USER_AGENT))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"count":1,"offset":0,"release-groups":[{"id":"b1392450-e666-3926-a536-22c65f834433","title":"OK Computer","primary-type":"Album"}]}"#,
        ))
        .mount(&mb)
        .await;
    let access = Arc::new(
        CatalogAccess::new(CatalogTokenKey::generate())
            .with_listenbrainz_base(&url::Url::parse(&lb.uri()).unwrap()),
    );
    let jobs = SystemJobs::new(CredentialKey::ephemeral()).with_catalog(Arc::clone(&access));
    let credential = seal_catalog(&access, "");
    let id = insert(&db, "musicbrainz", &mb.uri(), &credential).await;

    // Music off, no catalog: nothing to do.
    assert!(
        !jobs
            .has_work(JobKind::CatalogRefreshLists, &db)
            .await
            .unwrap()
    );
    let report = jobs
        .run(JobKind::CatalogRefreshLists, ctx(&db, json!({})))
        .await
        .unwrap();
    assert_eq!(
        report.expectation(),
        &starr_core::jobs::Expectation::NotApplicable
    );
    assert_eq!(mb.received_requests().await.unwrap().len(), 0);

    db.add_role(
        &starr_db::categories::NewRole {
            category: starr_core::members::Category::Music,
            kind: starr_core::categories::RoleKind::Catalog,
            service_instance_id: id,
            offered: &[starr_core::adapters::Capability::Metadata],
        },
        Utc::now(),
    )
    .await
    .unwrap();
    db.set_category_enabled(starr_core::members::Category::Music, true, Utc::now())
        .await
        .unwrap();
    assert!(
        jobs.has_work(JobKind::CatalogRefreshLists, &db)
            .await
            .unwrap()
    );
    let started = std::time::Instant::now();
    let report = jobs
        .run(JobKind::CatalogRefreshLists, ctx(&db, json!({})))
        .await
        .unwrap();
    assert!(
        matches!(report.expectation(), starr_core::jobs::Expectation::Met(_)),
        "{report:?}"
    );
    let genres = starr_adapter_musicbrainz::GENRES.len();
    // Within the budget (ADR 0013: 1 request/s in one process): twelve
    // requests take at least eleven seconds.
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(report.items().len(), genres);
    assert_eq!(mb.received_requests().await.unwrap().len(), genres);
    // The web side reads every lane from the cache, sending nothing.
    let AnyCatalog::MusicBrainz(web) = access.catalog(&db, id).await.unwrap() else {
        panic!("not MusicBrainz");
    };
    for (genre, _) in starr_adapter_musicbrainz::GENRES {
        let lane = web.cached_genre_lane(genre).await.unwrap();
        assert_eq!(lane.items[0].title, "OK Computer", "{genre}");
    }
    assert_eq!(mb.received_requests().await.unwrap().len(), genres);
    // A day, in the instance's cache rows.
    let (rows,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM catalog_cache WHERE service_instance_id = ? AND request LIKE 'ws/2/release-group?query=tag%'",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(usize::try_from(rows).unwrap(), genres);
}

#[tokio::test]
async fn catalog_responses_are_cached_in_the_database() {
    let db = Database::open_in_memory().await.unwrap();
    let server = MockServer::start().await;
    Mock::given(path("/3/movie/27205"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=3600")
                .set_body_string(fixture("movie_27205.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/3/authentication"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("authentication.json")))
        .mount(&server)
        .await;
    let jobs = single_process_jobs();
    let id = add(&db, &jobs, "tmdb", &server.uri()).await;
    let item = CatalogRef {
        kind: MediaKind::Movie,
        id: "27205".into(),
    };
    let first = jobs
        .catalog(&db, id)
        .await
        .unwrap()
        .details(&item, "en-US")
        .await
        .unwrap();
    // A fresh adapter (e.g. the next job) is served from the cache.
    let catalog = jobs.catalog(&db, id).await.unwrap();
    let second = catalog.details(&item, "en-US").await.unwrap();
    assert_eq!(first, second);
    catalog.probe().await.unwrap();
    server.verify().await;

    // Expired rows are pruned by housekeeping.
    let later = Utc::now() + chrono::TimeDelta::hours(2);
    assert_eq!(db.catalog_cache_prune(later, 100).await.unwrap(), 1);
}

#[tokio::test]
async fn catalog_refuses_other_services_and_missing_ones() {
    let db = Database::open_in_memory().await.unwrap();
    let jobs = single_process_jobs();
    let radarr = add(&db, &jobs, "radarr", "http://radarr:7878").await;
    assert!(matches!(
        jobs.catalog(&db, radarr).await,
        Err(CatalogOpenError::NotACatalog { .. })
    ));
    assert!(matches!(
        jobs.catalog(&db, radarr + 1).await,
        Err(CatalogOpenError::Missing(_))
    ));
    // Sealed for another key: unreadable, no request.
    let other = single_process_jobs();
    let tmdb = add(&db, &other, "tmdb", "http://127.0.0.1:9").await;
    assert!(matches!(
        jobs.catalog(&db, tmdb).await,
        Err(CatalogOpenError::Unavailable {
            code: "credentials_unreadable",
            ..
        })
    ));
}

#[tokio::test]
async fn housekeeping_evicts_images_over_the_limit_and_reports_it() {
    let db = Database::open_in_memory().await.unwrap();
    let dir = TempDir::new("housekeeping-images");
    let cache = ImageCache::new(dir.0.clone());
    cache
        .store("tmdb", "w92", "abc", ImageFormat::Jpeg, b"x")
        .unwrap();
    let jobs = single_process_jobs().with_image_cache(cache, 1024 * 1024);
    let report = jobs
        .run(JobKind::Housekeeping, ctx(&db, json!({})))
        .await
        .unwrap();
    // Far under the limit: nothing evicted, but the step ran.
    assert_eq!(report.counter("images_evicted"), Some(0));
    assert_eq!(report.counter("catalog_cache_rows_deleted"), Some(0));
}

/// Spec 0007 Q13: adult images not used for 30 days go; while the adult
/// category is off, every adult image and cached adult catalog answer goes.
#[tokio::test]
async fn housekeeping_keeps_adult_caches_short_lived() {
    let db = Database::open_in_memory().await.unwrap();
    let dir = TempDir::new("housekeeping-adult");
    let cache = ImageCache::new(dir.0.clone());
    let poster = cache
        .store("tmdb", "w92", "abc", ImageFormat::Jpeg, b"x")
        .unwrap();
    let scene = cache
        .adult()
        .store("stashdb", "300", "abc", ImageFormat::Jpeg, b"x")
        .unwrap();
    let jobs = single_process_jobs().with_image_cache(cache.clone(), 1024 * 1024);
    let stashdb = add(&db, &jobs, "stashdb", "http://127.0.0.1:9").await;
    let tmdb = add(&db, &jobs, "tmdb", "http://127.0.0.1:9").await;
    let now = chrono::Utc::now();
    let hour = std::time::Duration::from_hours(1);
    db.catalog_cache_put(stashdb, "adult:1", b"x", now, hour)
        .await
        .unwrap();
    db.catalog_cache_put(tmdb, "3/movie/1", b"x", now, hour)
        .await
        .unwrap();

    // On: recent adult files and rows stay.
    db.enable_with_statement(starr_core::members::Category::Adult, "root", now)
        .await
        .unwrap();
    let report = jobs
        .run(JobKind::Housekeeping, ctx(&db, json!({})))
        .await
        .unwrap();
    assert_eq!(report.counter("private_images_deleted"), Some(0));
    assert!(scene.exists());
    assert!(
        db.catalog_cache_get(stashdb, "adult:1", now)
            .await
            .unwrap()
            .is_some()
    );

    // Off: they go, nothing else does.
    db.set_category_enabled(starr_core::members::Category::Adult, false, now)
        .await
        .unwrap();
    let report = jobs
        .run(JobKind::Housekeeping, ctx(&db, json!({})))
        .await
        .unwrap();
    assert_eq!(report.counter("private_images_deleted"), Some(1));
    assert_eq!(report.counter("private_cache_rows_deleted"), Some(1));
    assert!(!scene.exists() && poster.exists());
    assert!(
        db.catalog_cache_get(stashdb, "adult:1", now)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db.catalog_cache_get(tmdb, "3/movie/1", now)
            .await
            .unwrap()
            .is_some()
    );
}

/// Spec 0007 Q13: the private category's in-app notifications go after 90
/// days, everything else after the general 180.
#[tokio::test]
async fn housekeeping_deletes_private_notifications_after_ninety_days() {
    let db = Database::open_in_memory().await.unwrap();
    let now = chrono::Utc::now();
    for (category, days) in [
        ("adult", 91),
        ("adult", 89),
        ("movies", 91),
        ("movies", 181),
    ] {
        sqlx::query(
            "INSERT INTO notification (member_id, kind, payload, dedup_key, created_at)
             VALUES (NULL, 'approval_pending', ?1, ?2, ?3)",
        )
        .bind(json!({ "category": category, "title": "Placeholder" }).to_string())
        .bind(format!("{category}:{days}"))
        .bind(
            (now - chrono::TimeDelta::days(days))
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string(),
        )
        .execute(db.pool())
        .await
        .unwrap();
    }
    let report = single_process_jobs()
        .run(JobKind::Housekeeping, ctx(&db, json!({})))
        .await
        .unwrap();
    assert_eq!(report.counter("private_notifications_deleted"), Some(1));
    assert_eq!(report.counter("notifications_deleted"), Some(1));
    let left: Vec<(String,)> =
        sqlx::query_as("SELECT dedup_key FROM notification ORDER BY dedup_key")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        left,
        vec![("adult:89".to_owned(),), ("movies:91".to_owned(),)]
    );
}

/// A Radarr row relabelled as TMDB (e.g. by a compromised web process) is
/// never re-sealed to the web key.
#[tokio::test]
async fn a_relabelled_service_key_is_not_moved_to_the_web_key() {
    let db = Database::open_in_memory().await.unwrap();
    let jobs = single_process_jobs();
    jobs.catalog.as_deref().unwrap().publish(&db).await.unwrap();
    let radarr_key = "0123456789abcdef0123456789abcdef";
    let forged = insert(
        &db,
        "tmdb",
        "http://127.0.0.1:9",
        &seal(&jobs.credentials, radarr_key),
    )
    .await;
    let report = jobs.credentials.reseal_catalog_tokens(&db).await.unwrap();
    assert_eq!(report.resealed, 0);
    assert_eq!(report.unreadable, 1);
    let conn = db.service_connection(forged).await.unwrap().unwrap();
    assert_eq!(conn.credential.class, SecretClass::ServiceCredential);
    assert_eq!(conn.credential.recipient, jobs.credentials.recipient());
}

/// ADR 0003 amendment 1: the web side may open catalog tokens, never a
/// service credential, even for a TMDB row still sealed with the worker key.
#[tokio::test]
async fn catalog_access_opens_catalog_tokens_only() {
    let db = Database::open_in_memory().await.unwrap();
    let jobs = single_process_jobs();
    let web = jobs.catalog.as_deref().unwrap();
    let radarr = add(&db, &jobs, "radarr", "http://radarr:7878").await;
    assert!(matches!(
        web.catalog(&db, radarr).await,
        Err(CatalogOpenError::NotACatalog { .. })
    ));
    // A Radarr key forged into a catalog row: the web key cannot open it.
    let forged = StoredCredential {
        class: SecretClass::CatalogReadToken,
        ..seal(&jobs.credentials, "radarr-key")
    };
    let tmdb_forged = insert(&db, "tmdb", "http://127.0.0.1:9", &forged).await;
    assert!(matches!(
        web.catalog(&db, tmdb_forged).await,
        Err(CatalogOpenError::Unavailable {
            code: "credentials_unreadable",
            ..
        })
    ));
    // Not re-sealed yet: refused by class, before any decryption.
    let legacy = insert(
        &db,
        "tmdb",
        "http://127.0.0.1:9",
        &seal(&jobs.credentials, TOKEN),
    )
    .await;
    let err = web.catalog(&db, legacy).await.unwrap_err();
    assert!(err.to_string().contains("not a catalog token"), "{err}");
}

#[tokio::test]
async fn a_worker_without_the_web_key_reports_catalog_checks_as_unavailable() {
    let db = Database::open_in_memory().await.unwrap();
    let with_key = single_process_jobs();
    let id = add(&db, &with_key, "tmdb", "http://127.0.0.1:9").await;
    let split_worker = SystemJobs::new(CredentialKey::ephemeral());
    let report = split_worker
        .run(
            JobKind::ServicesCheck,
            ctx(&db, json!({ "instanceId": id })),
        )
        .await
        .unwrap();
    assert_eq!(
        report.items()[0].code.as_deref(),
        Some("catalog_key_unavailable")
    );
    assert!(matches!(
        split_worker.catalog(&db, id).await,
        Err(CatalogOpenError::Unavailable {
            code: "catalog_key_unavailable",
            ..
        })
    ));
}

/// Existing TMDB instances (sealed with secret.key) are re-sealed to the
/// web key by the worker; Radarr stays with the worker key; a token the
/// worker key cannot open is marked unreadable.
#[tokio::test]
async fn existing_tmdb_tokens_are_resealed_to_the_web_key() {
    let db = Database::open_in_memory().await.unwrap();
    let server = MockServer::start().await;
    Mock::given(path("/3/authentication"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("authentication.json")))
        .mount(&server)
        .await;
    let jobs = single_process_jobs();
    let legacy = insert(&db, "tmdb", &server.uri(), &seal(&jobs.credentials, TOKEN)).await;
    let radarr = add(&db, &jobs, "radarr", "http://radarr:7878").await;
    let lost = insert(
        &db,
        "tmdb",
        &server.uri(),
        &seal(&CredentialKey::ephemeral(), TOKEN),
    )
    .await;

    // Before the web side published its recipient: nothing changes.
    let report = jobs.credentials.reseal_catalog_tokens(&db).await.unwrap();
    assert_eq!(
        report,
        ResealReport {
            resealed: 0,
            unreadable: 0,
            waiting: 2
        }
    );
    let web = jobs.catalog.as_deref().unwrap();
    web.publish(&db).await.unwrap();
    assert_eq!(
        db.meta_get(CATALOG_RECIPIENT_META_KEY)
            .await
            .unwrap()
            .as_deref(),
        Some(web.recipient())
    );

    jobs.on_lease_acquired(&db).await.unwrap();
    let conn = db.service_connection(legacy).await.unwrap().unwrap();
    assert_eq!(conn.credential.class, SecretClass::CatalogReadToken);
    assert_eq!(conn.credential.recipient, web.recipient());
    let radarr_conn = db.service_connection(radarr).await.unwrap().unwrap();
    assert_eq!(radarr_conn.credential.class, SecretClass::ServiceCredential);
    assert_eq!(
        radarr_conn.credential.recipient,
        jobs.credentials.recipient()
    );
    let lost_service = db.get_service(lost).await.unwrap().unwrap();
    assert_eq!(
        lost_service.last_error.as_deref(),
        Some("credentials_unreadable")
    );
    // The re-sealed token works from the web side.
    web.catalog(&db, legacy)
        .await
        .unwrap()
        .probe()
        .await
        .unwrap();
    // Idempotent: nothing left to do (the lost one stays marked).
    let again = jobs.credentials.reseal_catalog_tokens(&db).await.unwrap();
    assert_eq!(again.resealed, 0);
    // The worker key's own publish never marks catalog tokens.
    jobs.credentials.publish(&db).await.unwrap();
    assert_eq!(
        db.get_service(legacy).await.unwrap().unwrap().last_error,
        None
    );
}
/// A 503 from `MusicBrainz` during `catalog.refresh-lists` (seen live on
/// 2026-11-07): the lane fails as throttled, the client pauses every
/// caller, the remaining lanes are skipped without a request, and lanes
/// cached before stay readable until the next run retries.
#[tokio::test]
async fn refresh_lists_stops_at_a_503_and_keeps_the_cached_lanes() {
    let db = Database::open_in_memory().await.unwrap();
    let mb = MockServer::start().await;
    let lb = MockServer::start().await;
    // Each lane is ranked by one ListenBrainz popularity lookup.
    Mock::given(path("/1/popularity/release-group"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{"release_group_mbid":"b1392450-e666-3926-a536-22c65f834433","total_listen_count":5,"total_user_count":1}]"#,
        ))
        .mount(&lb)
        .await;
    Mock::given(path("/ws/2/release-group"))
        .and(wiremock::matchers::query_param_contains(
            "query",
            "tag:\"jazz\"",
        ))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .mount(&mb)
        .await;
    Mock::given(path("/ws/2/release-group"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"count":1,"offset":0,"release-groups":[{"id":"b1392450-e666-3926-a536-22c65f834433","title":"OK Computer","primary-type":"Album"}]}"#,
        ))
        .with_priority(2)
        .mount(&mb)
        .await;
    let access = Arc::new(
        CatalogAccess::new(CatalogTokenKey::generate())
            .with_listenbrainz_base(&url::Url::parse(&lb.uri()).unwrap()),
    );
    let jobs = SystemJobs::new(CredentialKey::ephemeral()).with_catalog(Arc::clone(&access));
    let credential = seal_catalog(&access, "");
    let id = insert(&db, "musicbrainz", &mb.uri(), &credential).await;
    db.add_role(
        &starr_db::categories::NewRole {
            category: starr_core::members::Category::Music,
            kind: starr_core::categories::RoleKind::Catalog,
            service_instance_id: id,
            offered: &[starr_core::adapters::Capability::Metadata],
        },
        Utc::now(),
    )
    .await
    .unwrap();
    db.set_category_enabled(starr_core::members::Category::Music, true, Utc::now())
        .await
        .unwrap();

    let report = jobs
        .run(JobKind::CatalogRefreshLists, ctx(&db, json!({})))
        .await
        .unwrap();
    assert!(
        matches!(
            report.expectation(),
            starr_core::jobs::Expectation::Unmet(_)
        ),
        "{report:?}"
    );
    let outcomes: Vec<(ItemOutcome, Option<&str>)> = report
        .items()
        .iter()
        .map(|i| (i.outcome, i.code.as_deref()))
        .collect();
    // indie, electronic, then jazz (third in the fixed list) answers 503.
    assert_eq!(
        outcomes[..3],
        [
            (ItemOutcome::Succeeded, None),
            (ItemOutcome::Succeeded, None),
            (ItemOutcome::Failed, Some("throttled")),
        ]
    );
    assert!(
        outcomes[3..]
            .iter()
            .all(|o| *o == (ItemOutcome::Skipped, Some("throttled")))
    );
    assert_eq!(outcomes.len(), starr_adapter_musicbrainz::GENRES.len());
    // No request after the 503.
    assert_eq!(mb.received_requests().await.unwrap().len(), 3);
    let AnyCatalog::MusicBrainz(web) = access.catalog(&db, id).await.unwrap() else {
        panic!("not MusicBrainz");
    };
    assert!(web.cached_genre_lane("indie").await.is_some());
    assert!(web.cached_genre_lane("jazz").await.is_none());
}
