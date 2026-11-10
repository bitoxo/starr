//! Removing requests against fake Radarr/Sonarr (spec 0004): what goes out
//! to the source when nobody wants a title any more, and what never does.

use starr_db::requests::CancelOutcome;

use super::*;

const ADMIN: Actor<'static> = Actor {
    trigger: EventTrigger::Admin,
    name: "root",
};

impl Env {
    async fn cancel(&self, request: i64) -> CancelOutcome {
        self.db
            .cancel_request(self.member, request, MEMBER, Utc::now())
            .await
            .unwrap()
    }

    async fn submit(&self) -> starr_core::jobs::Report {
        self.jobs
            .run(JobKind::RequestsSubmit, self.ctx())
            .await
            .unwrap()
    }

    async fn second_member(&self) -> i64 {
        self.db
            .create_member(
                &NewMember {
                    username: "noah",
                    email: "noah@example.com",
                    display_name: "Noah",
                    password_hash: None,
                },
                &[] as &[Entitlement],
                Utc::now(),
            )
            .await
            .unwrap()
    }
}

fn puts(fake: &FakeServarr) -> Vec<Value> {
    fake.writes()
        .into_iter()
        .filter(|(method, _, _)| method == "PUT")
        .map(|(_, _, body)| body)
        .collect()
}

fn monitored_seasons(body: &Value) -> Vec<u64> {
    body["seasons"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["monitored"] == true)
        .map(|s| s["seasonNumber"].as_u64().unwrap())
        .collect()
}

#[tokio::test]
async fn cancelling_a_held_request_sends_nothing_at_all() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let server = fake.start().await;
    let radarr = env.instance("radarr", &server.uri()).await;
    env.db
        .set_writes_paused(radarr, Some("root"), Utc::now())
        .await
        .unwrap();
    let request = env.request_matrix(radarr).await;
    assert!(matches!(
        env.cancel(request).await,
        CancelOutcome::Cancelled { stop: false, .. }
    ));
    env.db
        .set_writes_paused(radarr, None, Utc::now())
        .await
        .unwrap();
    assert!(
        !env.jobs
            .has_work(JobKind::RequestsSubmit, &env.db)
            .await
            .unwrap()
    );
    env.submit().await;
    assert_eq!(fake.writes().len(), 0);
    assert_eq!(fake.state.lock().unwrap().commands.len(), 0);
    assert_eq!(env.acquisition().await.state, AcquisitionState::Cancelled);
}

#[tokio::test]
async fn the_last_cancel_after_submission_unmonitors_once_and_reads_back() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let server = fake.start().await;
    let radarr = env.instance("radarr", &server.uri()).await;
    let request = env.request_matrix(radarr).await;
    env.submit().await;
    assert_eq!(fake.writes().len(), 1, "Starr added the movie");

    assert!(matches!(
        env.cancel(request).await,
        CancelOutcome::Cancelled { stop: true, .. }
    ));
    assert!(
        env.jobs
            .has_work(JobKind::RequestsSubmit, &env.db)
            .await
            .unwrap()
    );
    let report = env.submit().await;
    assert_eq!(report.counter("stopped"), Some(1), "{report:?}");
    let puts = puts(&fake);
    assert_eq!(puts.len(), 1, "exactly one unmonitor");
    assert_eq!(puts[0]["monitored"], false);
    // Only the flag changed; profile and folder are Radarr's as before.
    assert_eq!(puts[0]["qualityProfileId"], 4);
    assert_eq!(puts[0]["rootFolderPath"], "/movies");
    assert_eq!(
        fake.state.lock().unwrap().title.as_ref().unwrap()["monitored"],
        false
    );
    let acquisition = env.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Cancelled);
    assert!(acquisition.stopped_at.is_some());

    // Nothing more goes out.
    env.submit().await;
    assert_eq!(fake.writes().len(), 2);
}

#[tokio::test]
async fn a_title_monitored_before_starr_is_never_unmonitored() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let mut monitored = radarr("movie.json");
    monitored["monitored"] = json!(true);
    fake.with(|s| s.title = Some(monitored));
    let server = fake.start().await;
    let radarr = env.instance("radarr", &server.uri()).await;
    let request = env.request_matrix(radarr).await;
    env.submit().await;
    assert!(
        fake.writes().is_empty(),
        "already monitored: nothing written"
    );

    assert!(matches!(
        env.cancel(request).await,
        CancelOutcome::Cancelled { stop: false, .. }
    ));
    env.submit().await;
    assert!(fake.writes().is_empty(), "never unmonitored");
    assert_eq!(env.acquisition().await.state, AcquisitionState::Cancelled);
}

#[tokio::test]
async fn sonarr_unmonitors_only_the_seasons_nobody_else_wants() {
    let env = env().await;
    let fake = FakeServarr::new("sonarr");
    // Season 1 was monitored before Starr; 2 and 3 were not.
    fake.with(|s| s.title = Some(sonarr("series.json")));
    let server = fake.start().await;
    let sonarr_id = env.instance("sonarr", &server.uri()).await;
    let series = env
        .item(
            "INSERT INTO catalog_item (kind, tmdb_id, tvdb_id, title, created_at, updated_at)
             VALUES ('series', 1396, 81189, 'Breaking Bad', ?1, ?1) RETURNING id",
        )
        .await;
    let mut seasons = Vec::new();
    for n in [2, 3] {
        seasons.push(
            env.item(&format!(
                "INSERT INTO catalog_item (kind, parent_id, season_number, title, created_at,
                     updated_at)
                 VALUES ('season', {series}, {n}, 'Season {n}', ?1, ?1) RETURNING id"
            ))
            .await,
        );
    }
    let source = SourceTarget {
        category: Category::Series,
        service_instance_id: sonarr_id,
        quality_profile_id: 6,
        root_folder_path: "/tv".into(),
        minimum_availability: None,
        metadata_profile_id: None,
    };
    let unit = |i: usize, n: u32| Unit {
        catalog_item_id: seasons[i],
        season_number: Some(n),
    };
    let request = |member: i64, units: Vec<Unit>| {
        let source = source.clone();
        let db = env.db.clone();
        async move {
            db.create_request(
                &NewRequest {
                    member_id: member,
                    catalog_item_id: series,
                    follow: RequestFollow::No,
                    units: &units,
                    mode: RequestMode::Auto,
                    source: &source,
                },
                MEMBER,
                Utc::now(),
            )
            .await
            .unwrap()
            .request_id
        }
    };
    let mia = request(env.member, vec![unit(0, 2), unit(1, 3)]).await;
    let noah = env.second_member().await;
    request(noah, vec![unit(1, 3)]).await;
    env.submit().await;
    let after_submit = puts(&fake);
    assert_eq!(after_submit.len(), 1);
    assert_eq!(monitored_seasons(&after_submit[0]), vec![1, 2, 3]);

    // Mia cancels both seasons; Noah still wants season 3.
    assert!(matches!(
        env.cancel(mia).await,
        CancelOutcome::Cancelled { stop: true, .. }
    ));
    env.submit().await;
    let all = puts(&fake);
    assert_eq!(all.len(), 2, "one stop for the title");
    assert_eq!(monitored_seasons(&all[1]), vec![1, 3]);
    assert_eq!(all[1]["monitored"], true, "the series stays monitored");
    assert!(
        fake.state.lock().unwrap().commands.len() <= 2,
        "a stop searches nothing"
    );
}

#[tokio::test]
async fn paused_writes_hold_the_stop_until_resumed() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let server = fake.start().await;
    let radarr = env.instance("radarr", &server.uri()).await;
    let request = env.request_matrix(radarr).await;
    env.submit().await;
    env.db
        .set_writes_paused(radarr, Some("root"), Utc::now())
        .await
        .unwrap();
    env.cancel(request).await;

    let report = env.submit().await;
    assert!(puts(&fake).is_empty(), "held while paused");
    assert_eq!(report.expectation(), &Expectation::NotApplicable);
    let held = env.db.held_at(radarr, 5).await.unwrap();
    assert_eq!(held.stops, 1);
    assert_eq!(held.stop_preview[0].title, "The Matrix");

    env.db
        .set_writes_paused(radarr, None, Utc::now())
        .await
        .unwrap();
    env.submit().await;
    let sent = puts(&fake);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["monitored"], false);
    assert_eq!(env.db.held_at(radarr, 5).await.unwrap().stops, 0);
}

#[tokio::test]
async fn revoking_a_download_stops_the_search_and_leaves_the_download() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let server = fake.start().await;
    let radarr = env.instance("radarr", &server.uri()).await;
    let request = env.request_matrix(radarr).await;
    env.submit().await;
    fake.with(|s| s.queue = super::radarr("queue_details.json"));
    env.jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(env.acquisition().await.state, AcquisitionState::Downloading);

    let outcome = env
        .db
        .revoke_requests(
            &[starr_db::requests::DecisionTarget {
                request_id: request,
                member_id: env.member,
            }],
            "not on this server",
            ADMIN,
            Utc::now(),
        )
        .await
        .unwrap();
    assert!(outcome.stop);
    env.submit().await;
    let writes = fake.writes();
    // Added once, unmonitored once; no DELETE, no queue change.
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert_eq!(writes[1].0, "PUT");
    let acquisition = env.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Downloading);
    assert!(acquisition.unwanted);

    // The download goes away without a file: the acquisition lets go.
    fake.with(|s| s.queue = json!([]));
    env.jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(env.acquisition().await.state, AcquisitionState::Cancelled);
    assert_eq!(fake.writes().len(), 2);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one removal story
async fn a_title_gone_from_jellyfin_without_a_file_is_removed_and_requestable() {
    let env = env().await;
    let fake = FakeServarr::new("radarr");
    let server = fake.start().await;
    let radarr = env.instance("radarr", &server.uri()).await;
    let request = env.request_matrix(radarr).await;
    env.submit().await;
    let jellyfin = env.instance("jellyfin", "http://jellyfin").await;
    sqlx::query(
        "INSERT INTO service_role (category, kind, service_instance_id, created_at, updated_at)
         VALUES ('movies', 'consume', ?1, '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z')",
    )
    .bind(jellyfin)
    .execute(env.db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO inventory (service_instance_id, natural_key, kind, tmdb_id, remote_id, title,
             present_since, last_seen)
         VALUES (?1, 'movie:tmdb:603', 'movie', 603, 'x', 'The Matrix',
             '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z')",
    )
    .bind(jellyfin)
    .execute(env.db.pool())
    .await
    .unwrap();
    fake.with(|s| {
        if let Some(title) = s.title.as_mut() {
            title["hasFile"] = json!(true);
        }
    });
    env.jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(env.state(request).await, RequestState::Available);
    assert!(
        !env.jobs
            .has_work(JobKind::AcquisitionsSync, &env.db)
            .await
            .unwrap()
    );

    // Jellyfin reports it gone (two complete runs), Radarr still has the
    // file: kept.
    sqlx::query(
        "UPDATE inventory SET present_since = NULL, gone_since = '2026-11-02T00:00:00.000Z',
             missing_runs = 2",
    )
    .execute(env.db.pool())
    .await
    .unwrap();
    let long_ago = async || {
        sqlx::query("UPDATE acquisition SET synced_at = '2026-01-01T00:00:00.000Z'")
            .execute(env.db.pool())
            .await
            .unwrap();
    };
    long_ago().await;
    assert!(
        env.jobs
            .has_work(JobKind::AcquisitionsSync, &env.db)
            .await
            .unwrap()
    );
    let report = env
        .jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(report.counter("removed"), Some(0));
    assert_eq!(env.state(request).await, RequestState::Available);
    // Kept: not asked again before the recheck interval.
    assert!(
        !env.jobs
            .has_work(JobKind::AcquisitionsSync, &env.db)
            .await
            .unwrap()
    );
    long_ago().await;

    // The file is gone too: removed, silently, and new again.
    fake.with(|s| {
        if let Some(title) = s.title.as_mut() {
            title["hasFile"] = json!(false);
        }
    });
    let report = env
        .jobs
        .run(JobKind::AcquisitionsSync, env.ctx())
        .await
        .unwrap();
    assert_eq!(report.counter("removed"), Some(1), "{report:?}");
    assert_eq!(env.state(request).await, RequestState::Removed);
    assert_eq!(env.acquisition().await.state, AcquisitionState::Cancelled);
    let notifications = env
        .db
        .list_notifications(
            starr_db::notifications::Stream::Member(env.member),
            None,
            50,
        )
        .await
        .unwrap();
    assert!(
        notifications
            .iter()
            .all(|n| n.kind != starr_core::notifications::NotificationKind::RequestFailed),
        "{notifications:?}"
    );
    assert!(
        fake.writes().len() == 1,
        "removal writes nothing to the source"
    );
    assert!(
        !env.jobs
            .has_work(JobKind::AcquisitionsSync, &env.db)
            .await
            .unwrap()
    );
}
