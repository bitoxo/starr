#![allow(clippy::unwrap_used)]

use chrono::TimeDelta;
use starr_core::adapters::SecretClass;
use starr_core::members::{Category, Entitlement};

use super::*;
use crate::members::NewMember;
use crate::services::{NewServiceInstance, StoredCredential};
use crate::test_support::{keys_from_columns, t0, write_as_newer_release};

const MIA: Actor<'static> = Actor {
    trigger: EventTrigger::Member,
    name: "mia",
};
const NOAH: Actor<'static> = Actor {
    trigger: EventTrigger::Member,
    name: "noah",
};
const ADMIN: Actor<'static> = Actor {
    trigger: EventTrigger::Admin,
    name: "root",
};
const SYSTEM: Actor<'static> = Actor {
    trigger: EventTrigger::System,
    name: "requests.submit",
};

struct Fixture {
    db: Database,
    mia: i64,
    noah: i64,
    radarr: i64,
    sonarr: i64,
    movie: i64,
    series: i64,
    /// Season items by number 1, 2.
    seasons: Vec<i64>,
}

async fn member(db: &Database, name: &str) -> i64 {
    db.create_member(
        &NewMember {
            username: name,
            email: &format!("{name}@example.com"),
            display_name: name,
            password_hash: None,
        },
        &[] as &[Entitlement],
        t0(),
    )
    .await
    .unwrap()
}

async fn instance(db: &Database, adapter_type: &str) -> i64 {
    db.create_service(
        &NewServiceInstance {
            adapter_type,
            name: adapter_type,
            base_url: "http://x",
            public_url: None,
            credential: &StoredCredential {
                ciphertext: b"c".to_vec(),
                recipient: "age1x".to_owned(),
                class: SecretClass::ServiceCredential,
            },
        },
        t0(),
    )
    .await
    .unwrap()
    .id
}

async fn item(db: &Database, sql: &str) -> i64 {
    let (id,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(sql.to_owned()))
        .bind(ts(t0()))
        .fetch_one(db.pool())
        .await
        .unwrap();
    id
}

async fn fixture() -> Fixture {
    let db = Database::open_in_memory().await.unwrap();
    let mia = member(&db, "mia").await;
    let noah = member(&db, "noah").await;
    let radarr = instance(&db, "radarr").await;
    let sonarr = instance(&db, "sonarr").await;
    let movie = item(
        &db,
        "INSERT INTO catalog_item (kind, tmdb_id, imdb_id, title, year, created_at, updated_at)
         VALUES ('movie', 603, 'tt0133093', 'The Matrix', 1999, ?1, ?1) RETURNING id",
    )
    .await;
    let series = item(
        &db,
        "INSERT INTO catalog_item (kind, tmdb_id, tvdb_id, title, created_at, updated_at)
         VALUES ('series', 1399, 121361, 'Game of Thrones', ?1, ?1) RETURNING id",
    )
    .await;
    let mut seasons = Vec::new();
    for n in [1, 2] {
        seasons.push(
            item(
                &db,
                &format!(
                    "INSERT INTO catalog_item (kind, parent_id, season_number, title, metadata,
                         metadata_source, metadata_fetched_at, metadata_expires_at,
                         created_at, updated_at)
                     VALUES ('season', {series}, {n}, 'Season {n}', '{{\"episodeCount\": 10}}',
                         'tmdb', ?1, ?1, ?1, ?1) RETURNING id"
                ),
            )
            .await,
        );
    }
    keys_from_columns(&db).await;
    Fixture {
        db,
        mia,
        noah,
        radarr,
        sonarr,
        movie,
        series,
        seasons,
    }
}

impl Fixture {
    fn movies(&self) -> SourceTarget {
        SourceTarget {
            category: Category::Movies,
            service_instance_id: self.radarr,
            quality_profile_id: 4,
            root_folder_path: "/movies".into(),
            minimum_availability: Some(MinimumAvailability::InCinemas),
            metadata_profile_id: None,
        }
    }

    fn series_source(&self) -> SourceTarget {
        SourceTarget {
            category: Category::Series,
            service_instance_id: self.sonarr,
            quality_profile_id: 6,
            root_folder_path: "/tv".into(),
            minimum_availability: None,
            metadata_profile_id: None,
        }
    }

    async fn request_movie(
        &self,
        member: i64,
        mode: RequestMode,
        actor: Actor<'_>,
    ) -> Result<Created, DbError> {
        let source = self.movies();
        self.db
            .create_request(
                &NewRequest {
                    member_id: member,
                    catalog_item_id: self.movie,
                    follow: RequestFollow::No,
                    units: &[Unit {
                        catalog_item_id: self.movie,
                        season_number: None,
                    }],
                    mode,
                    source: &source,
                },
                actor,
                t0(),
            )
            .await
    }

    async fn request_seasons(
        &self,
        member: i64,
        mode: RequestMode,
        numbers: &[u32],
    ) -> Result<Created, DbError> {
        let source = self.series_source();
        let units: Vec<Unit> = numbers
            .iter()
            .map(|n| Unit {
                catalog_item_id: self.seasons[*n as usize - 1],
                season_number: Some(*n),
            })
            .collect();
        self.db
            .create_request(
                &NewRequest {
                    member_id: member,
                    catalog_item_id: self.series,
                    follow: RequestFollow::Start {
                        cutoff: crate::follows::Cutoff::Seasons(2),
                    },
                    units: &units,
                    mode,
                    source: &source,
                },
                MIA,
                t0(),
            )
            .await
    }

    async fn state(&self, id: i64) -> RequestState {
        self.db.get_request(id).await.unwrap().unwrap().state
    }

    async fn acquisitions(&self) -> Vec<AcquisitionRecord> {
        self.db.list_acquisitions(None, None).await.unwrap()
    }

    async fn approve(&self, id: i64, member: i64) -> DecisionOutcome {
        self.db
            .approve_requests(
                &[DecisionTarget {
                    request_id: id,
                    member_id: member,
                }],
                &[self.movies(), self.series_source()],
                ADMIN,
                t0(),
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn two_members_share_one_acquisition_and_attach_counts_as_approved() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    assert_eq!(mia.state, RequestState::PendingApproval);
    assert!(!mia.submit);
    // Noah asks too while Mia's request waits: his own mode decides.
    let noah = f
        .request_movie(f.noah, RequestMode::Approval, NOAH)
        .await
        .unwrap();
    assert_eq!(noah.state, RequestState::PendingApproval);
    assert_eq!(f.acquisitions().await.len(), 0);

    let outcome = f.approve(mia.request_id, f.mia).await;
    assert_eq!(outcome.decided, vec![mia.request_id]);
    assert!(outcome.submit);
    assert_eq!(f.state(mia.request_id).await, RequestState::Approved);
    // Noah's request is still his own decision.
    assert_eq!(
        f.state(noah.request_id).await,
        RequestState::PendingApproval
    );
    let acquisitions = f.acquisitions().await;
    assert_eq!(acquisitions.len(), 1);
    assert_eq!(acquisitions[0].state, AcquisitionState::Pending);
    assert_eq!(acquisitions[0].requests, 1);

    // Approving Noah's links him to the same acquisition: no second one.
    f.approve(noah.request_id, f.noah).await;
    let acquisitions = f.acquisitions().await;
    assert_eq!(acquisitions.len(), 1);
    assert_eq!(acquisitions[0].requests, 2);

    // A third member attaching to the running acquisition is approved
    // right away, even in approval mode (D10).
    let lea = member(&f.db, "lea").await;
    let attached = f
        .request_movie(lea, RequestMode::Approval, MIA)
        .await
        .unwrap();
    assert_eq!(attached.state, RequestState::Approved);
    // The shared acquisition still waits for submission.
    assert!(attached.submit);
    let events = f.db.request_events(attached.request_id).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].from_state, None);
    assert_eq!(events[0].to_state, RequestState::Approved);
    assert!(events[0].detail.as_deref().unwrap().contains("attached"));
}

#[tokio::test]
async fn one_open_request_per_member_and_unit() {
    let f = fixture().await;
    f.request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    assert!(matches!(
        f.request_movie(f.mia, RequestMode::Approval, MIA).await,
        Err(DbError::RequestOpen)
    ));
    // The other member is not blocked by Mia's request (owner in the rule).
    f.request_movie(f.noah, RequestMode::Approval, NOAH)
        .await
        .unwrap();
    // Season 1 requested; asking for seasons 1 and 2 again is refused as a
    // whole, season 2 alone is fine.
    f.request_seasons(f.mia, RequestMode::Approval, &[1])
        .await
        .unwrap();
    assert!(matches!(
        f.request_seasons(f.mia, RequestMode::Approval, &[1, 2])
            .await,
        Err(DbError::RequestOpen)
    ));
    f.request_seasons(f.mia, RequestMode::Approval, &[2])
        .await
        .unwrap();
    // After a withdrawal the unit can be requested again.
    let mine =
        f.db.list_requests(RequestFilter {
            member_id: Some(f.mia),
            category: Some(Category::Movies),
            ..RequestFilter::default()
        })
        .await
        .unwrap();
    f.db.withdraw_request(f.mia, mine[0].id, MIA, t0())
        .await
        .unwrap();
    f.request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
}

#[tokio::test]
async fn auto_mode_approves_and_creates_the_acquisition_with_the_settings() {
    let f = fixture().await;
    let created = f
        .request_seasons(f.mia, RequestMode::Auto, &[1, 2])
        .await
        .unwrap();
    assert_eq!(created.state, RequestState::Approved);
    assert!(created.submit);
    let due = f.db.due_submissions(t0()).await.unwrap();
    assert_eq!(due.len(), 2);
    assert_eq!(due[0].title_item_id, f.series);
    assert_eq!(due[0].tvdb_id, Some(121_361));
    assert_eq!(due[0].season_number, Some(1));
    assert_eq!(due[0].quality_profile_id, 6);
    assert_eq!(due[0].root_folder_path, "/tv");
    let request = f.db.get_request(created.request_id).await.unwrap().unwrap();
    assert!(request.follow);
    assert_eq!(request.items.len(), 2);
    assert!(
        request
            .items
            .iter()
            .all(|i| i.decision == Decision::Approved && i.decided_at.is_some())
    );
}

#[tokio::test]
async fn rejected_stays_rejected_even_when_the_title_arrives() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    let outcome =
        f.db.reject_requests(
            &[DecisionTarget {
                request_id: mia.request_id,
                member_id: f.mia,
            }],
            "  Not in our library plan  ",
            ADMIN,
            t0(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.decided, vec![mia.request_id]);
    let rejected = f.db.get_request(mia.request_id).await.unwrap().unwrap();
    assert_eq!(rejected.state, RequestState::Rejected);
    assert_eq!(
        rejected.reject_reason.as_deref(),
        Some("Not in our library plan")
    );

    // Noah gets it through auto mode; it becomes available.
    f.request_movie(f.noah, RequestMode::Auto, NOAH)
        .await
        .unwrap();
    let acquisition = f.acquisitions().await[0].id;
    f.db.record_submitted(
        acquisition,
        Some("1"),
        SourceChange::default(),
        SYSTEM,
        t0(),
    )
    .await
    .unwrap();
    f.db.record_observed(
        acquisition,
        &Observed {
            state: AcquisitionState::Available,
            progress: Some(1.0),
            message: None,
            stage: None,
        },
        SYSTEM,
        t0(),
    )
    .await
    .unwrap();
    assert_eq!(f.state(mia.request_id).await, RequestState::Rejected);
    // Mia cannot request what is available now.
    assert!(matches!(
        f.request_movie(f.mia, RequestMode::Auto, MIA).await,
        Err(DbError::AlreadyAvailable)
    ));
    let events = f.db.request_events(mia.request_id).await.unwrap();
    let last = events.last().unwrap();
    assert_eq!(last.trigger, EventTrigger::Admin);
    assert_eq!(last.actor.as_deref(), Some("root"));
    assert_eq!(last.detail.as_deref(), Some("Not in our library plan"));
}

#[tokio::test]
async fn decisions_need_the_right_owner_and_pending_items() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    let noah = f
        .request_movie(f.noah, RequestMode::Approval, NOAH)
        .await
        .unwrap();
    // Naming Noah as the owner of Mia's request fails the whole call.
    let wrong =
        f.db.approve_requests(
            &[
                DecisionTarget {
                    request_id: noah.request_id,
                    member_id: f.noah,
                },
                DecisionTarget {
                    request_id: mia.request_id,
                    member_id: f.noah,
                },
            ],
            &[f.movies()],
            ADMIN,
            t0(),
        )
        .await;
    assert!(matches!(wrong, Err(DbError::OwnerMismatch)));
    assert_eq!(
        f.state(noah.request_id).await,
        RequestState::PendingApproval
    );
    assert_eq!(f.acquisitions().await.len(), 0);

    // Without a source for the category nothing is approved either.
    let missing =
        f.db.approve_requests(
            &[DecisionTarget {
                request_id: mia.request_id,
                member_id: f.mia,
            }],
            &[f.series_source()],
            ADMIN,
            t0(),
        )
        .await;
    assert!(matches!(missing, Err(DbError::CategorySourceMissing)));

    // A second approval skips what is decided already; unknown ids too.
    f.approve(mia.request_id, f.mia).await;
    let again =
        f.db.approve_requests(
            &[
                DecisionTarget {
                    request_id: mia.request_id,
                    member_id: f.mia,
                },
                DecisionTarget {
                    request_id: 999,
                    member_id: f.mia,
                },
            ],
            &[f.movies()],
            ADMIN,
            t0(),
        )
        .await
        .unwrap();
    assert_eq!(again.decided, Vec::<i64>::new());
    assert_eq!(again.skipped, vec![mia.request_id, 999]);
    assert!(matches!(
        f.db.reject_requests(
            &[DecisionTarget {
                request_id: noah.request_id,
                member_id: f.noah
            }],
            "   ",
            ADMIN,
            t0()
        )
        .await,
        Err(DbError::InvalidData(_))
    ));
}

#[tokio::test]
async fn withdraw_only_own_and_only_pending() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    // Noah cannot see or withdraw Mia's request.
    assert_eq!(
        f.db.withdraw_request(f.noah, mia.request_id, NOAH, t0())
            .await
            .unwrap(),
        WithdrawOutcome::NotFound
    );
    assert_eq!(f.state(mia.request_id).await, RequestState::PendingApproval);
    assert_eq!(
        f.db.withdraw_request(f.mia, mia.request_id, MIA, t0())
            .await
            .unwrap(),
        WithdrawOutcome::Withdrawn(RequestState::Cancelled)
    );
    assert_eq!(
        f.db.withdraw_request(f.mia, mia.request_id, MIA, t0())
            .await
            .unwrap(),
        WithdrawOutcome::NotPending
    );

    // After approval there is nothing to withdraw (D11).
    let noah = f
        .request_movie(f.noah, RequestMode::Auto, NOAH)
        .await
        .unwrap();
    assert_eq!(
        f.db.withdraw_request(f.noah, noah.request_id, NOAH, t0())
            .await
            .unwrap(),
        WithdrawOutcome::NotPending
    );
}

#[tokio::test]
async fn mixed_requests_withdraw_the_pending_part_only() {
    let f = fixture().await;
    // Noah's season 1 runs; Mia asks for seasons 1 and 2 in approval mode.
    f.request_seasons(f.noah, RequestMode::Auto, &[1])
        .await
        .unwrap();
    let mia = f
        .request_seasons(f.mia, RequestMode::Approval, &[1, 2])
        .await
        .unwrap();
    let request = f.db.get_request(mia.request_id).await.unwrap().unwrap();
    assert_eq!(request.items[0].decision, Decision::Approved);
    assert_eq!(request.items[1].decision, Decision::PendingApproval);
    assert_eq!(request.state, RequestState::PendingApproval);
    let needs =
        f.db.list_requests(RequestFilter {
            needs_decision: true,
            oldest_first: true,
            ..RequestFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(
        needs.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![mia.request_id]
    );
    assert_eq!(needs[0].member_username, "mia");

    assert_eq!(
        f.db.withdraw_request(f.mia, mia.request_id, MIA, t0())
            .await
            .unwrap(),
        WithdrawOutcome::Withdrawn(RequestState::Approved)
    );
    let request = f.db.get_request(mia.request_id).await.unwrap().unwrap();
    assert_eq!(request.items[1].decision, Decision::Cancelled);
}

#[tokio::test]
async fn submission_failures_retry_then_fail_and_can_be_retried() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.acquisitions().await[0].id;
    let later = t0() + TimeDelta::minutes(1);
    assert!(
        f.db.record_submit_failure(id, "transient", "timeout", Some(later), SYSTEM, t0())
            .await
            .unwrap()
    );
    // Not due until the retry time; the request still looks approved.
    assert!(!f.db.has_due_submissions(t0()).await.unwrap());
    assert!(f.db.has_due_submissions(later).await.unwrap());
    assert_eq!(f.state(mia.request_id).await, RequestState::Approved);
    let pending = &f.acquisitions().await[0];
    assert_eq!(pending.submit_attempts, 1);
    assert_eq!(pending.last_error.as_deref(), Some("timeout"));

    assert!(
        f.db.record_submit_failure(id, "rejected", "Path not allowed", None, SYSTEM, later)
            .await
            .unwrap()
    );
    let failed = &f.acquisitions().await[0];
    assert_eq!(failed.state, AcquisitionState::Failed);
    assert_eq!(failed.error_code.as_deref(), Some("rejected"));
    assert_eq!(f.state(mia.request_id).await, RequestState::Failed);
    assert!(!f.db.has_due_submissions(later).await.unwrap());

    // Noah asking now decides by his mode and restarts the acquisition.
    let noah = f
        .request_movie(f.noah, RequestMode::Approval, NOAH)
        .await
        .unwrap();
    assert_eq!(noah.state, RequestState::PendingApproval);
    assert_eq!(
        f.acquisitions().await[0].state,
        AcquisitionState::Failed,
        "nothing restarts before the approval"
    );
    assert!(f.approve(noah.request_id, f.noah).await.submit);
    let restarted = &f.acquisitions().await[0];
    assert_eq!(restarted.state, AcquisitionState::Pending);
    assert_eq!(restarted.submit_attempts, 0);
    assert_eq!(restarted.last_error, None);
    assert_eq!(f.state(mia.request_id).await, RequestState::Approved);

    // The admin retry starts a failed one over too.
    f.db.record_submit_failure(id, "rejected", "again", None, SYSTEM, later)
        .await
        .unwrap();
    assert_eq!(
        f.db.retry_acquisition(id, None, ADMIN, later)
            .await
            .unwrap(),
        RetryOutcome::Retried(id)
    );
    assert_eq!(
        f.db.retry_acquisition(id, None, ADMIN, later)
            .await
            .unwrap(),
        RetryOutcome::NotFailed
    );
    assert_eq!(f.state(noah.request_id).await, RequestState::Approved);
}

#[tokio::test]
async fn progress_is_state_and_only_state_changes_are_events() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.acquisitions().await[0].id;
    f.db.record_submitted(id, Some("42"), SourceChange::default(), SYSTEM, t0())
        .await
        .unwrap();
    assert_eq!(f.state(mia.request_id).await, RequestState::Submitted);
    let before = f.db.request_events(mia.request_id).await.unwrap().len();
    for step in 0..10u32 {
        let change =
            f.db.record_observed(
                id,
                &Observed {
                    state: AcquisitionState::Downloading,
                    progress: Some(f64::from(step) / 10.0),
                    message: None,
                    stage: None,
                },
                SYSTEM,
                t0(),
            )
            .await
            .unwrap();
        assert_eq!(
            change.is_some(),
            step == 0,
            "only the first step changes the state"
        );
    }
    let events = f.db.request_events(mia.request_id).await.unwrap();
    assert_eq!(events.len(), before + 1);
    assert_eq!(events.last().unwrap().to_state, RequestState::InProgress);
    let request = f.db.get_request(mia.request_id).await.unwrap().unwrap();
    let brief = request.items[0].acquisition.as_ref().unwrap();
    assert_eq!(brief.progress, Some(0.9));

    // A pending acquisition never jumps to available by observation.
    let noah = f
        .request_seasons(f.noah, RequestMode::Auto, &[1])
        .await
        .unwrap();
    let season_acq = f
        .acquisitions()
        .await
        .into_iter()
        .find(|a| a.season_number == Some(1))
        .unwrap();
    let observed = Observed {
        state: AcquisitionState::Available,
        progress: Some(1.0),
        message: None,
        stage: None,
    };
    assert_eq!(
        f.db.record_observed(season_acq.id, &observed, SYSTEM, t0())
            .await
            .unwrap(),
        None
    );
    assert_eq!(f.state(noah.request_id).await, RequestState::Approved);
    assert_eq!(
        f.db.synced_acquisitions()
            .await
            .unwrap()
            .iter()
            .map(|u| u.acquisition_id)
            .collect::<Vec<_>>(),
        vec![id]
    );
    // The title leaving the source fails it with the reason.
    assert!(
        f.db.fail_acquisition(id, "not_found", "not in Radarr anymore", SYSTEM, t0())
            .await
            .unwrap()
    );
    assert_eq!(f.state(mia.request_id).await, RequestState::Failed);
}

#[tokio::test]
async fn the_download_stage_is_live_state_shown_only_while_downloading() {
    let f = fixture().await;
    let jellyfin = instance(&f.db, "jellyfin").await;
    let emby = instance(&f.db, "emby").await;
    for id in [jellyfin, emby] {
        sqlx::query(
            "INSERT INTO service_role (category, kind, service_instance_id, created_at, updated_at)
             VALUES ('movies', 'consume', ?1, ?2, ?2)",
        )
        .bind(id)
        .bind(ts(t0()))
        .execute(f.db.pool())
        .await
        .unwrap();
    }
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.acquisitions().await[0].id;
    f.db.record_submitted(id, Some("42"), SourceChange::default(), SYSTEM, t0())
        .await
        .unwrap();
    let observe = |state, stage| Observed {
        state,
        progress: Some(1.0),
        message: None,
        stage,
    };
    f.db.record_observed(
        id,
        &observe(AcquisitionState::Downloading, Some(DownloadStage::Arriving)),
        SYSTEM,
        t0(),
    )
    .await
    .unwrap();
    let acquisition = &f.acquisitions().await[0];
    assert_eq!(acquisition.stage, Some(DownloadStage::Arriving));
    // Every consume service of the category, by name.
    assert_eq!(acquisition.library_name.as_deref(), Some("emby / jellyfin"));
    assert_eq!(
        f.db.library_name(Category::Movies)
            .await
            .unwrap()
            .as_deref(),
        Some("emby / jellyfin")
    );
    assert_eq!(f.db.library_name(Category::Series).await.unwrap(), None);
    let request = f.db.get_request(mia.request_id).await.unwrap().unwrap();
    let brief = request.items[0].acquisition.as_ref().unwrap();
    assert_eq!(brief.stage, Some(DownloadStage::Arriving));
    assert_eq!(brief.library_name.as_deref(), Some("emby / jellyfin"));
    let facts = f.db.unit_facts(f.movie, f.mia).await.unwrap();
    assert_eq!(facts[0].stage, Some(DownloadStage::Arriving));
    // The sync knows since when it is arriving; observing it again keeps that.
    let since = |units: Vec<SyncUnit>| units[0].arriving_since;
    assert_eq!(since(f.db.synced_acquisitions().await.unwrap()), Some(t0()));
    f.db.record_observed(
        id,
        &observe(AcquisitionState::Downloading, Some(DownloadStage::Arriving)),
        SYSTEM,
        t0() + TimeDelta::hours(1),
    )
    .await
    .unwrap();
    assert_eq!(since(f.db.synced_acquisitions().await.unwrap()), Some(t0()));
    // Importing is no arriving; arriving again starts over.
    for (stage, at) in [(DownloadStage::Importing, 2), (DownloadStage::Arriving, 3)] {
        f.db.record_observed(
            id,
            &observe(AcquisitionState::Downloading, Some(stage)),
            SYSTEM,
            t0() + TimeDelta::hours(at),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        since(f.db.synced_acquisitions().await.unwrap()),
        Some(t0() + TimeDelta::hours(3))
    );

    // Available: the stage is gone, even though the column is not reset.
    f.db.record_observed(
        id,
        &observe(AcquisitionState::Available, Some(DownloadStage::Arriving)),
        SYSTEM,
        t0(),
    )
    .await
    .unwrap();
    assert_eq!(f.acquisitions().await[0].stage, None);
    let request = f.db.get_request(mia.request_id).await.unwrap().unwrap();
    assert_eq!(request.items[0].acquisition.as_ref().unwrap().stage, None);
    assert_eq!(
        f.db.unit_facts(f.movie, f.mia).await.unwrap()[0].stage,
        None
    );
}

#[tokio::test]
async fn open_requests_at_a_source_survive_a_switch() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    assert_eq!(
        f.db.open_requests_at(f.radarr, Category::Movies)
            .await
            .unwrap(),
        vec![mia.request_id]
    );
    assert_eq!(
        f.db.open_requests_at(f.sonarr, Category::Movies)
            .await
            .unwrap(),
        Vec::<i64>::new()
    );
}

#[tokio::test]
async fn presence_and_unit_facts() {
    let f = fixture().await;
    assert_eq!(
        f.db.title_presence(Category::Series, ItemKind::Series, Some(1399), None, None)
            .await
            .unwrap(),
        TitlePresence::NoService
    );
    let jellyfin = instance(&f.db, "jellyfin").await;
    sqlx::query(
        "INSERT INTO service_role (category, kind, service_instance_id, created_at, updated_at)
         VALUES ('series', 'consume', ?1, ?2, ?2)",
    )
    .bind(jellyfin)
    .bind(ts(t0()))
    .execute(f.db.pool())
    .await
    .unwrap();
    assert_eq!(
        f.db.title_presence(Category::Series, ItemKind::Series, Some(1399), None, None)
            .await
            .unwrap(),
        TitlePresence::Absent
    );
    sqlx::query(
        "INSERT INTO inventory (service_instance_id, natural_key, kind, tvdb_id, remote_id,
             title, seasons, present_since, last_seen)
         VALUES (?1, 'series:tvdb:121361', 'series', 121361, 'x', 'GoT', '{\"1\": 10, \"2\": 3}',
             ?2, ?2)",
    )
    .bind(jellyfin)
    .bind(ts(t0()))
    .execute(f.db.pool())
    .await
    .unwrap();
    let presence =
        f.db.title_presence(
            Category::Series,
            ItemKind::Series,
            Some(1399),
            Some(121_361),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        presence.unit(Some(1), None),
        ConsumePresence::Present { episodes: 10 }
    );
    assert_eq!(presence.unit(Some(3), None), ConsumePresence::Absent);

    f.request_seasons(f.noah, RequestMode::Approval, &[2])
        .await
        .unwrap();
    let facts = f.db.unit_facts(f.series, f.mia).await.unwrap();
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[0].expected_episodes, Some(10));
    assert!(!facts[0].open_requests);
    // Noah's request shows as "requested", not as Mia's.
    assert!(facts[1].open_requests);
    assert!(!facts[1].mine);
    assert!(f.db.unit_facts(f.series, f.noah).await.unwrap()[1].mine);
    let movie = f.db.unit_facts(f.movie, f.mia).await.unwrap();
    assert_eq!(movie.len(), 1);
    assert_eq!(movie[0].season_number, None);
}

/// Lesson 24: intent never cascades into inventory.
#[tokio::test]
async fn request_tables_have_no_foreign_key_into_inventory() {
    let db = Database::open_in_memory().await.unwrap();
    for table in ["request", "request_item", "request_event", "acquisition"] {
        let targets: Vec<(String,)> =
            sqlx::query_as("SELECT \"table\" FROM pragma_foreign_key_list(?)")
                .bind(table)
                .fetch_all(db.pool())
                .await
                .unwrap();
        assert!(!targets.is_empty(), "{table}");
        assert!(targets.iter().all(|(t,)| t != "inventory"), "{table}");
    }
    let columns: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM pragma_table_info('acquisition')")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert!(columns.iter().all(|(c,)| !c.contains("member")));
}

#[tokio::test]
async fn auto_mode_restarts_a_failed_acquisition_and_tells_the_others_without_names() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.acquisitions().await[0].id;
    f.db.record_submit_failure(id, "rejected", "Path not allowed", None, SYSTEM, t0())
        .await
        .unwrap();
    assert_eq!(f.state(mia.request_id).await, RequestState::Failed);

    let noah = f
        .request_movie(f.noah, RequestMode::Auto, NOAH)
        .await
        .unwrap();
    assert_eq!(noah.state, RequestState::Approved);
    assert!(noah.submit);
    assert_eq!(f.acquisitions().await[0].state, AcquisitionState::Pending);
    assert_eq!(f.state(mia.request_id).await, RequestState::Approved);
    let last =
        f.db.request_events(mia.request_id)
            .await
            .unwrap()
            .pop()
            .unwrap();
    assert_eq!(last.trigger, EventTrigger::System);
    assert_ne!(last.actor.as_deref(), Some("noah"));
}

#[tokio::test]
async fn removing_the_source_fails_open_acquisitions_and_a_retry_moves_them() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let noah = f
        .request_movie(f.noah, RequestMode::Auto, NOAH)
        .await
        .unwrap();
    let id = f.acquisitions().await[0].id;
    f.db.record_submitted(id, Some("1"), SourceChange::default(), SYSTEM, t0())
        .await
        .unwrap();

    assert!(f.db.delete_service(f.radarr, t0()).await.unwrap());
    let orphan = &f.acquisitions().await[0];
    assert_eq!(orphan.service_instance_id, None);
    assert_eq!(orphan.state, AcquisitionState::Failed);
    assert_eq!(orphan.error_code.as_deref(), Some(CODE_SOURCE_REMOVED));
    for request in [mia.request_id, noah.request_id] {
        assert_eq!(f.state(request).await, RequestState::Failed);
    }
    assert_eq!(f.db.synced_acquisitions().await.unwrap().len(), 0);
    // Nowhere to go without a source.
    assert_eq!(
        f.db.retry_acquisition(id, None, ADMIN, t0()).await.unwrap(),
        RetryOutcome::NoSource
    );

    // A new Radarr where Lea requested meanwhile: the retry joins hers.
    let radarr = instance(&f.db, "radarr-new").await;
    let source = SourceTarget {
        service_instance_id: radarr,
        ..f.movies()
    };
    let lea = member(&f.db, "lea").await;
    let leas =
        f.db.create_request(
            &NewRequest {
                member_id: lea,
                catalog_item_id: f.movie,
                follow: RequestFollow::No,
                units: &[Unit {
                    catalog_item_id: f.movie,
                    season_number: None,
                }],
                mode: RequestMode::Auto,
                source: &source,
            },
            MIA,
            t0(),
        )
        .await
        .unwrap();
    let RetryOutcome::Retried(target) =
        f.db.retry_acquisition(id, Some(&source), ADMIN, t0())
            .await
            .unwrap()
    else {
        panic!("not retried");
    };
    assert_ne!(target, id);
    let acquisitions = f.acquisitions().await;
    assert_eq!(acquisitions.len(), 1);
    assert_eq!(acquisitions[0].service_instance_id, Some(radarr));
    assert_eq!(acquisitions[0].requests, 3);
    for request in [mia.request_id, noah.request_id, leas.request_id] {
        assert_eq!(f.state(request).await, RequestState::Approved);
    }
    // Mia can withdraw nothing, but the unit is not stuck: the items are
    // live again and due for submission at the new source.
    let due = f.db.due_submissions(t0()).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].service_instance_id, radarr);
}

/// D10 (maintainer 2026-11-02): asking again after a rejection always goes
/// back to an admin, even with a running acquisition or auto mode; the
/// other member's request is untouched.
#[tokio::test]
async fn after_a_rejection_an_admin_decides_again() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    f.db.reject_requests(
        &[DecisionTarget {
            request_id: mia.request_id,
            member_id: f.mia,
        }],
        "Not now",
        ADMIN,
        t0(),
    )
    .await
    .unwrap();
    // Noah gets it automatically; the acquisition runs.
    let noah = f
        .request_movie(f.noah, RequestMode::Auto, NOAH)
        .await
        .unwrap();
    let acquisition = f.acquisitions().await[0].id;
    f.db.record_submitted(
        acquisition,
        Some("1"),
        SourceChange::default(),
        SYSTEM,
        t0(),
    )
    .await
    .unwrap();
    let noah_before = f.db.request_events(noah.request_id).await.unwrap().len();

    // Mia asks again, even in auto mode: waits for an admin, not attached.
    let again = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    assert_eq!(again.state, RequestState::PendingApproval);
    assert!(!again.submit);
    let request = f.db.get_request(again.request_id).await.unwrap().unwrap();
    assert_eq!(request.items[0].decision, Decision::PendingApproval);
    assert_eq!(request.items[0].acquisition, None);
    let needs =
        f.db.list_requests(RequestFilter {
            needs_decision: true,
            ..RequestFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(
        needs.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![again.request_id]
    );
    // The old one stays rejected; Noah's request is untouched.
    assert_eq!(f.state(mia.request_id).await, RequestState::Rejected);
    assert_eq!(f.state(noah.request_id).await, RequestState::Submitted);
    assert_eq!(
        f.db.request_events(noah.request_id).await.unwrap().len(),
        noah_before
    );
    assert_eq!(f.acquisitions().await[0].requests, 1);

    // Approved now, it joins the running acquisition.
    f.approve(again.request_id, f.mia).await;
    assert_eq!(f.state(again.request_id).await, RequestState::Submitted);
    assert_eq!(f.acquisitions().await[0].requests, 2);
}

#[tokio::test]
async fn paused_writes_hold_pending_acquisitions_until_resumed() {
    let f = fixture().await;
    f.db.set_writes_paused(f.radarr, Some("root"), t0())
        .await
        .unwrap()
        .unwrap();
    // Approvals still work: the request is approved, its acquisition held.
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    assert_eq!(mia.state, RequestState::Approved);
    let held = &f.acquisitions().await[0];
    assert_eq!(held.state, AcquisitionState::Pending);
    assert!(held.held);
    assert_eq!(f.db.due_submissions(t0()).await.unwrap().len(), 0);
    assert!(!f.db.has_due_submissions(t0()).await.unwrap());

    // Another instance's writes go on.
    f.request_seasons(f.mia, RequestMode::Auto, &[1])
        .await
        .unwrap();
    let due = f.db.due_submissions(t0()).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].service_instance_id, f.sonarr);

    // A retry of a failed one on the paused instance is held again.
    let id = held.id;
    f.db.record_submit_failure(id, "rejected", "no", None, SYSTEM, t0())
        .await
        .unwrap();
    assert_eq!(
        f.db.retry_acquisition(id, None, ADMIN, t0()).await.unwrap(),
        RetryOutcome::Retried(id)
    );
    let retried = f.db.list_acquisitions(Some(id), None).await.unwrap();
    assert!(retried[0].held);
    assert_eq!(f.db.due_submissions(t0()).await.unwrap().len(), 1);

    f.db.set_writes_paused(f.radarr, None, t0())
        .await
        .unwrap()
        .unwrap();
    assert!(!f.db.list_acquisitions(Some(id), None).await.unwrap()[0].held);
    let due = f.db.due_submissions(t0()).await.unwrap();
    assert_eq!(due.len(), 2);
    assert!(due.iter().any(|u| u.acquisition_id == id));
}

/// Status journey (maintainer, 2026-11-02): title cards load only the
/// member's open requests, the detail view only one title's, and the
/// history of many requests in one go.
#[tokio::test]
async fn open_and_title_filters_and_batched_history() {
    let f = fixture().await;
    let rejected = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    f.db.reject_requests(
        &[DecisionTarget {
            request_id: rejected.request_id,
            member_id: f.mia,
        }],
        "No",
        ADMIN,
        t0(),
    )
    .await
    .unwrap();
    let open = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    let ids = |list: Vec<RequestRecord>| list.into_iter().map(|r| r.id).collect::<Vec<_>>();
    let mine = RequestFilter {
        member_id: Some(f.mia),
        ..RequestFilter::default()
    };
    assert_eq!(
        ids(f.db.list_requests(mine).await.unwrap()),
        vec![open.request_id, rejected.request_id]
    );
    assert_eq!(
        ids(f
            .db
            .list_requests(RequestFilter { open: true, ..mine })
            .await
            .unwrap()),
        vec![open.request_id]
    );
    assert_eq!(
        ids(f
            .db
            .list_requests(RequestFilter {
                catalog_item_id: Some(f.series),
                ..mine
            })
            .await
            .unwrap()),
        Vec::<i64>::new()
    );
    assert_eq!(
        f.db.list_requests(RequestFilter {
            catalog_item_id: Some(f.movie),
            ..mine
        })
        .await
        .unwrap()
        .len(),
        2
    );
    let history =
        f.db.requests_events(&[open.request_id, rejected.request_id, 999])
            .await
            .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[&rejected.request_id].len(), 2);
    assert_eq!(
        history[&open.request_id],
        f.db.request_events(open.request_id).await.unwrap()
    );
}

#[tokio::test]
async fn held_lists_what_resuming_sends_out_oldest_first() {
    let f = fixture().await;
    // Nothing is held while writes go out.
    f.request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let held = f.db.held_at(f.radarr, 5).await.unwrap();
    assert_eq!((held.total, held.preview.len()), (0, 0));

    for instance in [f.radarr, f.sonarr] {
        f.db.set_writes_paused(instance, Some("root"), t0())
            .await
            .unwrap();
    }
    f.request_seasons(f.noah, RequestMode::Auto, &[2, 1])
        .await
        .unwrap();
    let movies = f.db.held_at(f.radarr, 5).await.unwrap();
    assert_eq!(movies.total, 1);
    assert_eq!(
        movies.preview,
        vec![HeldTitle {
            title: "The Matrix".to_owned(),
            year: Some(1999),
            season: None,
            album: None,
            category: Category::Movies,
        }]
    );
    let series = f.db.held_at(f.sonarr, 1).await.unwrap();
    assert_eq!(series.total, 2, "the total counts beyond the preview");
    assert_eq!(series.preview.len(), 1);
    assert_eq!(series.preview[0].title, "Game of Thrones");
    assert!(series.preview[0].season.is_some());

    // Submitted ones are not held any more.
    let id = f
        .acquisitions()
        .await
        .into_iter()
        .find(|a| a.service_instance_id == Some(f.radarr))
        .unwrap()
        .id;
    f.db.record_submitted(id, Some("1"), SourceChange::default(), SYSTEM, t0())
        .await
        .unwrap();
    assert_eq!(f.db.held_at(f.radarr, 5).await.unwrap().total, 0);
    assert_eq!(f.db.held_at(999, 5).await.unwrap().total, 0);
}

#[path = "removal_tests.rs"]
mod removal;

/// After a rollback, rows a newer release wrote with states this binary
/// does not know are left out of lists instead of failing them (ADR 0012).
#[tokio::test]
async fn lists_skip_states_written_by_a_newer_release() {
    let f = fixture().await;
    let movie = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let series = f
        .request_seasons(f.noah, RequestMode::Auto, &[1, 2])
        .await
        .unwrap();
    let season_2 = f.seasons[1];
    write_as_newer_release(
        &f.db,
        &format!(
            "UPDATE request SET state = 'teleported' WHERE id = {movie};
             UPDATE request_item SET decision = 'paused', acquisition_id = NULL
                 WHERE catalog_item_id = {season_2};
             UPDATE acquisition SET state = 'shipping', download_stage = 'unpacking'
                 WHERE catalog_item_id = {season_2};
             INSERT INTO request_event (request_id, at, from_state, to_state, trigger)
                 VALUES ({series}, '2026-11-01T12:00:00.000Z', 'approved', 'teleported', 'robot');",
            movie = movie.request_id,
            series = series.request_id,
        ),
    )
    .await;

    let requests = f.db.list_requests(RequestFilter::default()).await.unwrap();
    assert_eq!(
        requests.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![series.request_id]
    );
    // The item with the unknown decision is left out, the other stays.
    assert_eq!(requests[0].items.len(), 1);
    assert_eq!(requests[0].items[0].season_number, Some(1));
    assert_eq!(f.db.get_request(movie.request_id).await.unwrap(), None);
    // Only the event this binary can read is left.
    let events = f.db.request_events(series.request_id).await.unwrap();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].to_state, RequestState::Approved);
    let acquisitions = f.acquisitions().await;
    assert_eq!(acquisitions.len(), 2, "{acquisitions:?}");
    assert!(acquisitions.iter().all(|a| a.catalog_item_id != season_2));
    let facts = f.db.unit_facts(f.series, f.noah).await.unwrap();
    assert_eq!(facts.len(), 1);
    assert!(f.db.synced_acquisitions().await.is_ok());
}

#[tokio::test]
async fn the_metadata_profile_travels_with_the_acquisition() {
    let f = fixture().await;
    let source = SourceTarget {
        metadata_profile_id: Some(7),
        ..f.movies()
    };
    f.db.create_request(
        &NewRequest {
            member_id: f.mia,
            catalog_item_id: f.movie,
            follow: RequestFollow::No,
            units: &[Unit {
                catalog_item_id: f.movie,
                season_number: None,
            }],
            mode: RequestMode::Auto,
            source: &source,
        },
        MIA,
        t0(),
    )
    .await
    .unwrap();
    let due = f.db.due_submissions(t0()).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].metadata_profile_id, Some(7));
}
/// Scenes (spec 0007 slice 4): a scene is its own unit like a movie, keyed
/// by its `StashDB` UUID; the media servers' adult libraries are never
/// asked (Q5 A: Whisparr's files), and an available scene is removed only
/// after Whisparr showed no file on two checks six hours apart (R4).
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn scenes_are_units_keyed_by_stashdb_and_removed_after_two_missing_files() {
    let f = fixture().await;
    let whisparr = instance(&f.db, "whisparr").await;
    let scene_uuid = "5ce0e000-0000-4000-8000-00000000000a";
    let scene = item(
        &f.db,
        "INSERT INTO catalog_item (kind, title, created_at, updated_at)
         VALUES ('scene', 'Scene A', ?1, ?1) RETURNING id",
    )
    .await;
    sqlx::query(
        "INSERT INTO catalog_key (catalog_item_id, scheme, value) VALUES (?, 'stashdb_scene', ?)",
    )
    .bind(scene)
    .bind(scene_uuid)
    .execute(f.db.pool())
    .await
    .unwrap();
    // An adult consume role does not make the media servers the evidence.
    let emby = instance(&f.db, "emby").await;
    sqlx::query(
        "INSERT INTO service_role (category, kind, service_instance_id, created_at, updated_at)
         VALUES ('adult', 'consume', ?1, ?2, ?2)",
    )
    .bind(emby)
    .bind(ts(t0()))
    .execute(f.db.pool())
    .await
    .unwrap();
    assert_eq!(
        f.db.title_presence(Category::Adult, ItemKind::Scene, None, None, None)
            .await
            .unwrap(),
        TitlePresence::NoService
    );
    let source = SourceTarget {
        category: Category::Adult,
        service_instance_id: whisparr,
        quality_profile_id: 3,
        root_folder_path: "/private".into(),
        minimum_availability: None,
        metadata_profile_id: None,
    };
    for member in [f.mia, f.noah] {
        f.db.create_request(
            &NewRequest {
                member_id: member,
                catalog_item_id: scene,
                follow: RequestFollow::No,
                units: &[Unit {
                    catalog_item_id: scene,
                    season_number: None,
                }],
                mode: RequestMode::Auto,
                source: &source,
            },
            MIA,
            t0(),
        )
        .await
        .unwrap();
    }
    let facts = f.db.unit_facts(scene, f.mia).await.unwrap();
    assert_eq!(facts.len(), 1, "the scene is its own unit");
    assert!(facts[0].mine && facts[0].open_requests);
    let due = f.db.due_submissions(t0()).await.unwrap();
    assert_eq!(due.len(), 1, "one shared acquisition");
    let uuid = Uuid::parse_str(scene_uuid).unwrap();
    assert_eq!(due[0].stash_scene, Some(uuid));
    assert_eq!(due[0].category, Category::Adult);
    let id = due[0].acquisition_id;
    let mia_request =
        f.db.list_requests(RequestFilter {
            member_id: Some(f.mia),
            ..RequestFilter::default()
        })
        .await
        .unwrap()
        .remove(0);
    assert_eq!(mia_request.stash_scene, Some(uuid));
    assert_eq!(mia_request.kind, ItemKind::Scene);

    f.db.record_submitted(id, Some("12"), SourceChange::default(), SYSTEM, t0())
        .await
        .unwrap();
    let available = Observed {
        state: AcquisitionState::Available,
        progress: Some(1.0),
        message: None,
        stage: None,
    };
    f.db.record_observed(id, &available, SYSTEM, t0())
        .await
        .unwrap();
    // Not looked at again before six hours.
    assert_eq!(
        f.db.scene_file_checks(t0() + TimeDelta::hours(1))
            .await
            .unwrap()
            .len(),
        0
    );
    let later = t0() + TimeDelta::hours(7);
    let checks = f.db.scene_file_checks(later).await.unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].stash_scene, Some(uuid));
    // A file there: kept, and the next check six hours on.
    assert_eq!(
        f.db.record_scene_file(id, true, SYSTEM, later)
            .await
            .unwrap(),
        SceneFile::Kept
    );
    assert_eq!(f.db.scene_file_checks(later).await.unwrap().len(), 0);
    // No file once: noted only; a file found again clears it.
    let t1 = later + TimeDelta::hours(7);
    assert_eq!(
        f.db.record_scene_file(id, false, SYSTEM, t1).await.unwrap(),
        SceneFile::Missing
    );
    let t2 = t1 + TimeDelta::hours(7);
    assert_eq!(
        f.db.record_scene_file(id, true, SYSTEM, t2).await.unwrap(),
        SceneFile::Kept
    );
    assert_eq!(
        f.db.record_scene_file(id, false, SYSTEM, t2).await.unwrap(),
        SceneFile::Missing
    );
    assert_eq!(f.state(mia_request.id).await, RequestState::Available);
    // No file on the next check as well: removed for both members.
    let t3 = t2 + TimeDelta::hours(7);
    assert_eq!(
        f.db.record_scene_file(id, false, SYSTEM, t3).await.unwrap(),
        SceneFile::Removed
    );
    assert_eq!(f.state(mia_request.id).await, RequestState::Removed);
    assert_eq!(
        f.db.record_scene_file(id, false, SYSTEM, t3).await.unwrap(),
        SceneFile::NotAvailable
    );
    let facts = f.db.unit_facts(scene, f.noah).await.unwrap();
    assert!(!facts[0].open_requests, "requestable again");
}

/// Spec 0008 F-4: an approval can be undone while nothing went to the
/// source for it: the request waits again, its held acquisition is
/// cancelled and the member's "approved" notification is taken back.
#[tokio::test]
async fn undo_approval_while_held_puts_the_request_back() {
    let f = fixture().await;
    f.db.set_writes_paused(f.radarr, Some("root"), t0())
        .await
        .unwrap()
        .unwrap();
    let mia = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    let target = DecisionTarget {
        request_id: mia.request_id,
        member_id: f.mia,
    };
    f.approve(mia.request_id, f.mia).await;
    assert!(
        f.db.approvals_undoable(&[mia.request_id], t0())
            .await
            .unwrap()
    );
    let approved: (i64,) =
        sqlx::query_as("SELECT count(*) FROM notification WHERE kind = 'request_approved'")
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    assert_eq!(approved.0, 1);

    let outcome = f.db.undo_approvals(&[target], ADMIN, t0()).await.unwrap();
    assert_eq!(outcome.decided, vec![mia.request_id]);
    assert_eq!(f.state(mia.request_id).await, RequestState::PendingApproval);
    let request = f.db.get_request(mia.request_id).await.unwrap().unwrap();
    assert_eq!(request.items[0].decision, Decision::PendingApproval);
    assert!(request.items[0].acquisition.is_none());
    assert_eq!(f.acquisitions().await[0].state, AcquisitionState::Cancelled);
    let approved: (i64,) =
        sqlx::query_as("SELECT count(*) FROM notification WHERE kind = 'request_approved'")
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    assert_eq!(approved.0, 0, "the member's notification is taken back");

    // It can be approved again (and notified again).
    f.approve(mia.request_id, f.mia).await;
    assert_eq!(f.state(mia.request_id).await, RequestState::Approved);
}

/// Spec 0003 D11 / 0008 F-4: once the submission is under way (the source
/// takes writes), undo is refused and nothing changes.
#[tokio::test]
async fn undo_approval_is_refused_once_it_goes_to_the_source() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    f.approve(mia.request_id, f.mia).await;
    assert!(
        !f.db
            .approvals_undoable(&[mia.request_id], t0())
            .await
            .unwrap()
    );
    let err =
        f.db.undo_approvals(
            &[DecisionTarget {
                request_id: mia.request_id,
                member_id: f.mia,
            }],
            ADMIN,
            t0(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::UndoTooLate));
    assert_eq!(f.state(mia.request_id).await, RequestState::Approved);
    assert_eq!(f.acquisitions().await[0].state, AcquisitionState::Pending);
}

/// Joining an acquisition an earlier approval of someone else wanted
/// sends nothing new: undo only unlinks this request.
#[tokio::test]
async fn undo_approval_of_a_shared_acquisition_leaves_it_running() {
    let f = fixture().await;
    let noah = f
        .request_movie(f.noah, RequestMode::Approval, NOAH)
        .await
        .unwrap();
    let mia = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    f.approve(noah.request_id, f.noah).await;
    f.approve(mia.request_id, f.mia).await;
    assert_eq!(f.acquisitions().await[0].requests, 2);
    f.db.undo_approvals(
        &[DecisionTarget {
            request_id: mia.request_id,
            member_id: f.mia,
        }],
        ADMIN,
        t0(),
    )
    .await
    .unwrap();
    assert_eq!(f.state(mia.request_id).await, RequestState::PendingApproval);
    assert_eq!(f.state(noah.request_id).await, RequestState::Approved);
    let shared = &f.acquisitions().await[0];
    assert_eq!(shared.state, AcquisitionState::Pending);
    assert_eq!(shared.requests, 1);
}

/// Only fresh approvals: an hour later the undo is gone, and the owner
/// must match.
#[tokio::test]
async fn undo_approval_needs_a_fresh_approval_and_the_owner() {
    let f = fixture().await;
    f.db.set_writes_paused(f.radarr, Some("root"), t0())
        .await
        .unwrap()
        .unwrap();
    let mia = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    f.approve(mia.request_id, f.mia).await;
    let wrong = DecisionTarget {
        request_id: mia.request_id,
        member_id: f.noah,
    };
    assert!(matches!(
        f.db.undo_approvals(&[wrong], ADMIN, t0())
            .await
            .unwrap_err(),
        DbError::OwnerMismatch
    ));
    let later = t0() + UNDO_APPROVAL_WINDOW + TimeDelta::seconds(1);
    let right = DecisionTarget {
        request_id: mia.request_id,
        member_id: f.mia,
    };
    assert!(
        !f.db
            .approvals_undoable(&[mia.request_id], later)
            .await
            .unwrap()
    );
    assert!(matches!(
        f.db.undo_approvals(&[right], ADMIN, later)
            .await
            .unwrap_err(),
        DbError::UndoTooLate
    ));
}

/// Two members' requests for one title approved together at a live
/// source: the title goes out, so neither approval can be undone (the
/// other request of the same batch does not count as "wanted anyway").
#[tokio::test]
async fn undo_approval_of_a_batch_sharing_one_unit_is_refused() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    let noah = f
        .request_movie(f.noah, RequestMode::Approval, NOAH)
        .await
        .unwrap();
    let both = [
        DecisionTarget {
            request_id: mia.request_id,
            member_id: f.mia,
        },
        DecisionTarget {
            request_id: noah.request_id,
            member_id: f.noah,
        },
    ];
    f.db.approve_requests(&both, &[f.movies()], ADMIN, t0())
        .await
        .unwrap();
    assert!(
        !f.db
            .approvals_undoable(&[mia.request_id, noah.request_id], t0())
            .await
            .unwrap()
    );
    assert!(matches!(
        f.db.undo_approvals(&both, ADMIN, t0()).await.unwrap_err(),
        DbError::UndoTooLate
    ));
}

/// An approval that joined files already there told the member "ready":
/// not undone. Neither is an automatic approval (the member's mode).
#[tokio::test]
async fn undo_approval_is_refused_for_ready_titles_and_automatic_approvals() {
    let f = fixture().await;
    f.db.set_writes_paused(f.radarr, Some("root"), t0())
        .await
        .unwrap()
        .unwrap();
    let mia = f
        .request_movie(f.mia, RequestMode::Approval, MIA)
        .await
        .unwrap();
    let noah = f
        .request_movie(f.noah, RequestMode::Auto, NOAH)
        .await
        .unwrap();
    assert!(
        !f.db
            .approvals_undoable(&[noah.request_id], t0())
            .await
            .unwrap()
    );

    let id = f.acquisitions().await[0].id;
    sqlx::query("UPDATE acquisition SET state = 'available' WHERE id = ?")
        .bind(id)
        .execute(f.db.pool())
        .await
        .unwrap();
    f.approve(mia.request_id, f.mia).await;
    assert!(
        !f.db
            .approvals_undoable(&[mia.request_id], t0())
            .await
            .unwrap()
    );
}
