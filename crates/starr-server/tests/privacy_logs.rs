//! Spec 0007 acceptance "logs and audit", in its own test binary: `tracing`
//! caches callsite interest process-wide, so a thread-local capture is only
//! reliable when no other test runs in parallel. An adult request goes
//! through retry → submit (the source fails, echoing the title) → revoke
//! and no title, name or reason reaches any log line at any level; the
//! private area (lanes, search, scene, performer and studio pages, images)
//! is browsed against the `StashDB` fake, which also fails once echoing a
//! title, and neither a name nor a `StashDB` id is logged. Scene requests
//! (slice 4) go through submit (Whisparr fails echoing the title), approve,
//! submit and revoke with the stop, with the same result for logs, audit
//! and run reports. Performer and studio follows (slice 5) go through
//! following and a `follows.check` over the weekly cap (requests, the
//! hint, `follow_capped`): no name, title or `StashDB` UUID in logs, audit, run reports or
//! job payloads.
#![allow(
    clippy::unwrap_used,
    clippy::used_underscore_binding,
    clippy::too_many_lines
)]

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use common::privacy::{MARKERS, privacy};
use common::stashdb::{IMAGE, PERFORMER_P, SCENE_A, STUDIO_S, mount_entity_scenes};
use common::whisparr::{SCENE_B, connect_whisparr, fake_whisparr, mount_scene};
use common::{get, post};
use serde_json::json;
use wiremock::matchers::{body_string_contains, path};
use wiremock::{Mock, ResponseTemplate};

/// A scene whose details the fake fails on once the requests are done,
/// echoing a title in its error.
const SCENE_C: &str = "5ce0e000-0000-4000-8000-00000000000c";

/// Scenes the followed performer and studio publish after the follow date.
const FOLLOWED: [&str; 3] = [
    "5ce0e000-0000-4000-8000-000000000301",
    "5ce0e000-0000-4000-8000-000000000302",
    "5ce0e000-0000-4000-8000-000000000303",
];

#[derive(Clone)]
struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn adult_titles_never_reach_the_logs() {
    let capture = Capture(Arc::default());
    let writer = capture.clone();
    // Every level of every target, except the statement texts `sqlx`
    // logs: they hold this test's seeded literals (Starr binds values, and
    // `sqlx` never logs bound values).
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("trace,sqlx::query=off"))
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    // Global, so the blocking threads (image cache IO) and the database's
    // threads are captured too; this binary holds this one test.
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let mut p = privacy().await;
    // The title has a TMDB id, so the Radarr fake is asked, and fails. (A
    // source echoing titles is covered where it matters: private-content
    // clients of the adapter kit, `redaction.rs`.)
    sqlx::query(
        "INSERT INTO catalog_key (catalog_item_id, scheme, value)
         SELECT catalog_item_id, 'tmdb_movie', '4242' FROM acquisition WHERE id = ?",
    )
    .bind(p.acquisition)
    .execute(p.pool())
    .await
    .unwrap();
    Mock::given(path("/api/v3/movie"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&p.setup._servers[1])
        .await;

    p.set_unlocked(&p.ops, true).await;
    let res = p
        .ops(post(
            &format!("/api/v1/admin/acquisitions/{}/retry", p.acquisition),
            json!({}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    p.setup.env.drain().await;
    let res = p
        .ops(post(
            "/api/v1/admin/requests/revoke",
            json!({
                "requests": [{ "id": p.ada_request, "memberId": p.ada.id }],
                "reason": "Reason R: Scene A",
            }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    p.setup.env.drain().await;

    // Scene requests (spec 0007 slice 4): Ada requests Scene A, Ops
    // approves, Whisparr fails the add echoing the title; Scene B goes
    // out, is revoked with a reason naming it and is unmonitored again.
    let (whisparr, _whisparr_server) =
        fake_whisparr(&[(SCENE_A, "Scene A"), (SCENE_B, "Scene B")]).await;
    connect_whisparr(&p.setup.env, &_whisparr_server).await;
    mount_scene(&p.stashdb, SCENE_B, "Scene B").await;
    whisparr.state().fail_adds = true;
    p.set_unlocked(&p.ada.cookie, true).await;
    let mut scene_requests = Vec::new();
    for scene in [SCENE_A, SCENE_B] {
        let res = p
            .as_member(
                &p.ada,
                post("/api/v1/private/requests", json!({ "scenes": [scene] })),
            )
            .await;
        assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
        let id = res.body["requests"][0]["id"].as_i64().unwrap();
        scene_requests.push(id);
        let res = p
            .ops(post(
                "/api/v1/admin/approvals/approve",
                json!({ "requests": [{ "id": id, "memberId": p.ada.id }] }),
            ))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        p.setup.env.drain().await;
        whisparr.state().fail_adds = false;
    }
    assert_eq!(whisparr.state().writes_to("POST", "/api/v3/movie").len(), 2);
    let res = p
        .ops(post(
            "/api/v1/admin/requests/revoke",
            json!({
                "requests": [{ "id": scene_requests[1], "memberId": p.ada.id }],
                "reason": "Reason R: Scene B",
            }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    p.setup.env.drain().await;
    assert_eq!(whisparr.state().writes_to("PUT", "/api/v3/movie/").len(), 1);
    let res = p
        .as_member(&p.ada, get("/api/v1/private/requests?hidden=true"))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);

    // The private area, as Ada browses it.
    Mock::given(body_string_contains(SCENE_C))
        .respond_with(ResponseTemplate::new(500).set_body_string("Scene A by Studio S failed"))
        .with_priority(1)
        .mount(&p.stashdb)
        .await;
    Mock::given(body_string_contains("Studio S broken"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(common::stashdb::fixture("error_other.json")),
        )
        .with_priority(1)
        .mount(&p.stashdb)
        .await;
    p.set_unlocked(&p.ada.cookie, true).await;
    for path in [
        "/api/v1/private".to_owned(),
        "/api/v1/private/discover/new".to_owned(),
        "/api/v1/private/discover/trending".to_owned(),
        format!("/api/v1/private/scenes/{SCENE_A}"),
        format!("/api/v1/private/scenes/{SCENE_C}"),
        format!("/api/v1/private/performers/{PERFORMER_P}"),
        format!("/api/v1/private/studios/{STUDIO_S}"),
        format!("/api/v1/images/stashdb/600/{IMAGE}"),
    ] {
        let res = p.as_member(&p.ada, get(&path)).await;
        assert!(
            res.status.is_success() || res.status.is_server_error(),
            "{path}: {}",
            res.raw
        );
    }
    for query in ["Performer P", "Studio S broken"] {
        let res = p
            .as_member(
                &p.ada,
                post("/api/v1/private/search", json!({ "query": query })),
            )
            .await;
        assert!(
            res.status.is_success() || res.status.is_server_error(),
            "{query}: {}",
            res.raw
        );
    }

    // Follows (slice 5): Ada follows Studio S besides Performer P, the
    // weekly cap is 2 and three scenes are new; the worker reads StashDB.
    let access = Arc::clone(&p.setup.env.app.state.catalog);
    p.setup.env.worker_catalog(access);
    // The pages browsed above cached the old scene lists.
    sqlx::query("DELETE FROM catalog_cache")
        .execute(p.pool())
        .await
        .unwrap();
    mount_entity_scenes(
        &p.stashdb,
        &[
            (FOLLOWED[0], "Scene Followed 1", Some("2026-11-02")),
            (FOLLOWED[1], "Scene Followed 2", Some("2026-11-03")),
            (FOLLOWED[2], "Scene Followed 3", Some("2026-11-04")),
        ],
    )
    .await;
    let res = p
        .as_member(
            &p.ada,
            post(
                "/api/v1/private/follows",
                json!({ "kind": "studio", "id": STUDIO_S }),
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let categories = p.admin(get("/api/v1/categories")).await;
    let role = categories
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == "adult")
        .unwrap()["catalogs"][0]["id"]
        .as_i64()
        .unwrap();
    let res = p
        .admin(common::media::put(
            &format!("/api/v1/categories/adult/roles/{role}/follow-cap"),
            json!({ "perWeek": 2 }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = p
        .admin(post("/api/v1/jobs/follows.check/trigger", json!({})))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    p.setup.env.drain().await;
    let (by_follow,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM request WHERE follow_id IS NOT NULL")
            .fetch_one(p.pool())
            .await
            .unwrap();
    let reports: Vec<(Option<String>,)> =
        sqlx::query_as("SELECT report FROM run WHERE kind = 'follows.check'")
            .fetch_all(p.pool())
            .await
            .unwrap();
    assert_eq!(
        by_follow, 2,
        "two for the studio follow, at the cap: {reports:?}"
    );
    let res = p.as_member(&p.ada, get("/api/v1/private/follows")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let payloads: Vec<(String, String)> = sqlx::query_as("SELECT kind, payload FROM job")
        .fetch_all(p.pool())
        .await
        .unwrap();
    for (kind, payload) in &payloads {
        for marker in MARKERS.iter().chain(&FOLLOWED).chain(&["Scene Followed"]) {
            assert!(
                !payload.contains(marker),
                "{kind} payload names {marker:?}: {payload}"
            );
        }
    }

    // Retention (slice 6): the adult image cache's file names are StashDB
    // image ids. A hit that cannot refresh a file's time, the purge on
    // switch-off and the housekeeping runs that cannot delete it log no
    // path; once they can, the files go.
    let undeletable = retention_cannot_delete(&mut p).await;

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    if undeletable {
        assert!(
            logs.contains("cannot delete cached image")
                && logs.contains("cannot refresh cached image time"),
            "the failing purge was not logged: {logs}"
        );
    }
    assert!(
        !logs.contains(&IMAGE.replace('-', "")),
        "the logs name an adult image file: {logs}"
    );
    for marker in FOLLOWED.iter().chain(&["Scene Followed"]) {
        assert!(!logs.contains(marker), "the logs name {marker:?}: {logs}");
    }
    assert!(
        logs.contains("submission failed") && logs.contains("[adult]"),
        "the failing submission was not logged: {logs}"
    );
    for marker in MARKERS {
        assert!(!logs.contains(marker), "the logs name {marker:?}: {logs}");
    }
    assert!(!logs.contains("Scene B"), "the logs name Scene B: {logs}");
    for id in [SCENE_A, SCENE_B, SCENE_C, PERFORMER_P, STUDIO_S, IMAGE] {
        assert!(
            !logs.contains(id),
            "the logs name a StashDB id {id}: {logs}"
        );
    }
    assert!(
        logs.contains("catalog call failed"),
        "the failing catalog calls were not logged: {logs}"
    );
    let rows: Vec<(String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT action, target, details FROM audit_log")
            .fetch_all(p.pool())
            .await
            .unwrap();
    for (action, target, details) in &rows {
        let text = format!("{action} {target:?} {details:?}");
        for marker in MARKERS
            .iter()
            .chain(&["Scene B", SCENE_A, SCENE_B, "Scene Followed"])
            .chain(&FOLLOWED)
        {
            assert!(!text.contains(marker), "audit names {marker:?}: {text}");
        }
    }
    assert!(
        rows.iter()
            .filter(|(a, _, _)| a == "request.create")
            .count()
            >= 2,
        "the scene requests were audited: {rows:?}"
    );
    // Run reports name private units by id and keep no source words.
    let runs: Vec<(String, Option<String>)> = sqlx::query_as("SELECT kind, report FROM run")
        .fetch_all(p.pool())
        .await
        .unwrap();
    for (kind, report) in &runs {
        let text = report.clone().unwrap_or_default();
        for marker in MARKERS
            .iter()
            .chain(&["Scene B", SCENE_A, SCENE_B, "Scene Followed"])
            .chain(&FOLLOWED)
        {
            assert!(
                !text.contains(marker),
                "{kind} report names {marker:?}: {text}"
            );
        }
    }
    assert!(
        runs.iter().any(|(k, r)| k == "requests.submit"
            && r.as_deref()
                .is_some_and(|r| r.contains("[adult] acquisition #"))),
        "{runs:?}"
    );
    assert!(
        runs.iter()
            .any(|(k, r)| k == "follows.check"
                && r.as_deref().is_some_and(|r| r.contains("[adult] follow #")
                    && r.contains("1 scenes over the weekly cap"))),
        "{runs:?}"
    );
    assert!(
        rows.iter()
            .any(|(a, t, _)| a == "follow.create" && t.is_none()),
        "the follow was audited without a name: {rows:?}"
    );
}

/// Makes the cached adult images undeletable, serves one (its time is too
/// old to keep, so the refresh fails), switches adult off and runs
/// housekeeping; then makes them deletable again and runs housekeeping
/// once more, which removes them. Returns whether the files really were
/// undeletable (not when the tests run as root).
async fn retention_cannot_delete(p: &mut common::privacy::Privacy) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let adult_dir = p.images.0.join("adult");
    let adult_files = |p: &common::privacy::Privacy| -> Vec<std::path::PathBuf> {
        p.images
            .files()
            .into_iter()
            .filter(|f| f.starts_with(&adult_dir))
            .collect()
    };
    let files = adult_files(p);
    assert!(!files.is_empty(), "the browsed image was not cached");
    let set_mode = |path: &std::path::Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    for file in &files {
        std::fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_hours(48))
            .unwrap();
        set_mode(file, 0o444);
        set_mode(file.parent().unwrap(), 0o555);
    }
    let res = p
        .as_member(&p.ada, get(&format!("/api/v1/images/stashdb/600/{IMAGE}")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(p.switch_adult(false).await.status, StatusCode::OK);
    p.setup
        .env
        .worker_images(starr_worker::image_cache::ImageCache::new(
            p.images.0.clone(),
        ));
    p.setup.env.drain().await;
    let undeletable = files.iter().all(|f| f.exists());

    for file in &files {
        set_mode(file.parent().unwrap(), 0o755);
        set_mode(file, 0o644);
    }
    let res = p
        .admin(post("/api/v1/jobs/system.housekeeping/trigger", json!({})))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    p.setup.env.drain().await;
    let left = adult_files(p);
    assert!(
        left.is_empty(),
        "adult images left after the purge: {left:?}"
    );
    undeletable
}
