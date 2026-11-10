//! Removing requests (spec 0004): cancel, revoke, hide, stops and
//! removal from the library, with two members sharing one acquisition.

use super::*;

const STARR_MONITORED: SourceChange = SourceChange {
    added: false,
    monitored: true,
};

fn observed(state: AcquisitionState) -> Observed {
    Observed {
        state,
        progress: None,
        message: None,
        stage: None,
    }
}

impl Fixture {
    async fn acquisition(&self) -> AcquisitionRecord {
        let mut all = self.acquisitions().await;
        assert_eq!(all.len(), 1, "{all:?}");
        all.remove(0)
    }

    async fn cancel(&self, member: i64, id: i64) -> CancelOutcome {
        self.db.cancel_request(member, id, MIA, t0()).await.unwrap()
    }

    async fn revoke(&self, id: i64, member: i64, reason: &str) -> RevokeOutcome {
        self.db
            .revoke_requests(
                &[DecisionTarget {
                    request_id: id,
                    member_id: member,
                }],
                reason,
                ADMIN,
                t0(),
            )
            .await
            .unwrap()
    }

    /// Mia and Noah share the movie's acquisition, both approved.
    async fn shared_movie(&self) -> (i64, i64) {
        let mia = self
            .request_movie(self.mia, RequestMode::Auto, MIA)
            .await
            .unwrap();
        let noah = self
            .request_movie(self.noah, RequestMode::Auto, NOAH)
            .await
            .unwrap();
        (mia.request_id, noah.request_id)
    }

    async fn submit(&self, change: SourceChange) -> i64 {
        let id = self.acquisition().await.id;
        assert!(
            self.db
                .record_submitted(id, Some("1"), change, SYSTEM, t0())
                .await
                .unwrap()
        );
        id
    }

    /// A consume service for movies that lists the movie as gone.
    async fn movie_gone_from_jellyfin(&self) {
        let jellyfin = instance(&self.db, "jellyfin").await;
        sqlx::query(
            "INSERT INTO service_role (category, kind, service_instance_id, created_at, updated_at)
             VALUES ('movies', 'consume', ?1, ?2, ?2)",
        )
        .bind(jellyfin)
        .bind(ts(t0()))
        .execute(self.db.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO inventory (service_instance_id, natural_key, kind, tmdb_id, remote_id,
                 title, last_seen, missing_runs, gone_since)
             VALUES (?1, 'movie:tmdb:603', 'movie', 603, 'x', 'The Matrix', ?2, 2, ?2)",
        )
        .bind(jellyfin)
        .bind(ts(t0()))
        .execute(self.db.pool())
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn one_member_cancelling_leaves_the_shared_acquisition_alone() {
    let f = fixture().await;
    let (mia, noah) = f.shared_movie().await;
    assert_eq!(
        f.cancel(f.mia, mia).await,
        CancelOutcome::Cancelled {
            state: RequestState::Cancelled,
            stop: false
        }
    );
    let acquisition = f.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Pending);
    assert_eq!(acquisition.requests, 1);
    assert_eq!(f.state(noah).await, RequestState::Approved);
    // Not Noah's to cancel, and nothing left for Mia.
    assert_eq!(f.cancel(f.noah, mia).await, CancelOutcome::NotFound);
    assert_eq!(f.cancel(f.mia, mia).await, CancelOutcome::NotCancellable);

    // Both cancelled before the submission: cancelled, nothing to stop.
    assert_eq!(
        f.cancel(f.noah, noah).await,
        CancelOutcome::Cancelled {
            state: RequestState::Cancelled,
            stop: false
        }
    );
    let acquisition = f.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Cancelled);
    assert!(!acquisition.stop_pending);
    assert_eq!(f.db.due_submissions(t0()).await.unwrap().len(), 0);
    assert!(!f.db.has_due_submissions(t0()).await.unwrap());

    // The item keeps its link and the history says who did it.
    let request = f.db.get_request(mia).await.unwrap().unwrap();
    assert_eq!(request.items[0].decision, Decision::Cancelled);
    assert_eq!(request.items[0].ended_at, Some(t0()));
    assert!(request.items[0].acquisition.is_some());
    let events = f.db.request_events(mia).await.unwrap();
    let last = events.last().unwrap();
    assert_eq!(
        (last.trigger, last.actor.as_deref(), last.to_state),
        (EventTrigger::Member, Some("mia"), RequestState::Cancelled)
    );
}

#[tokio::test]
async fn cancelling_a_held_acquisition_takes_it_out_of_the_resume_list() {
    let f = fixture().await;
    f.db.set_writes_paused(f.radarr, Some("root"), t0())
        .await
        .unwrap();
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    assert_eq!(f.db.held_at(f.radarr, 5).await.unwrap().total, 1);
    f.cancel(f.mia, mia.request_id).await;
    let held = f.db.held_at(f.radarr, 5).await.unwrap();
    assert_eq!((held.total, held.stops), (0, 0));
    assert_eq!(f.acquisition().await.state, AcquisitionState::Cancelled);
}

#[tokio::test]
async fn cancelling_after_submission_stops_only_what_starr_turned_on() {
    let f = fixture().await;
    // Starr switched the monitoring on: the last cancel queues one stop.
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.submit(STARR_MONITORED).await;
    assert_eq!(
        f.cancel(f.mia, mia.request_id).await,
        CancelOutcome::Cancelled {
            state: RequestState::Cancelled,
            stop: true
        }
    );
    let acquisition = f.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Submitted);
    assert!(acquisition.stop_pending && acquisition.unwanted);
    assert!(f.db.has_due_submissions(t0()).await.unwrap());
    let stops = f.db.due_stops(t0()).await.unwrap();
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0].acquisition_id, id);
    assert_eq!(stops[0].tmdb_id, Some(603));

    // Once the stop went out, a searching acquisition is cancelled.
    assert!(!f.db.record_stopped(id, t0()).await.unwrap());
    let acquisition = f.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Cancelled);
    assert!(!acquisition.stop_pending);
    assert_eq!(acquisition.stopped_at, Some(t0()));
    assert_eq!(f.db.due_stops(t0()).await.unwrap().len(), 0);
}

#[tokio::test]
async fn a_title_monitored_before_starr_is_let_go_without_a_write() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    // Already monitored in Radarr (D14): Starr changed nothing there.
    f.submit(SourceChange::default()).await;
    assert_eq!(
        f.cancel(f.mia, mia.request_id).await,
        CancelOutcome::Cancelled {
            state: RequestState::Cancelled,
            stop: false
        }
    );
    let acquisition = f.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Cancelled);
    assert!(!acquisition.stop_pending);
}

#[tokio::test]
async fn members_cannot_cancel_a_running_download_but_pending_seasons_go() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.submit(STARR_MONITORED).await;
    f.db.record_observed(id, &observed(AcquisitionState::Downloading), SYSTEM, t0())
        .await
        .unwrap();
    assert_eq!(
        f.cancel(f.mia, mia.request_id).await,
        CancelOutcome::NotCancellable
    );
    assert_eq!(f.state(mia.request_id).await, RequestState::InProgress);

    // Pending seasons are withdrawn by the same cancel.
    let noah = f
        .request_seasons(f.noah, RequestMode::Approval, &[1, 2])
        .await
        .unwrap();
    let CancelOutcome::Cancelled { state, stop } = f.cancel(f.noah, noah.request_id).await else {
        panic!("expected cancelled");
    };
    assert_eq!((state, stop), (RequestState::Cancelled, false));
}

#[tokio::test]
async fn admins_revoke_in_bulk_with_a_reason_and_the_member_is_told() {
    let f = fixture().await;
    let (mia, noah) = f.shared_movie().await;
    let seasons = f
        .request_seasons(f.mia, RequestMode::Approval, &[1])
        .await
        .unwrap();
    let id = f.submit(STARR_MONITORED).await;
    f.db.record_observed(id, &observed(AcquisitionState::Downloading), SYSTEM, t0())
        .await
        .unwrap();

    // A reason is required.
    assert!(
        f.db.revoke_requests(
            &[DecisionTarget {
                request_id: mia,
                member_id: f.mia
            }],
            "  ",
            ADMIN,
            t0()
        )
        .await
        .is_err()
    );
    // The owner must match (lesson 4).
    assert!(matches!(
        f.db.revoke_requests(
            &[DecisionTarget {
                request_id: mia,
                member_id: f.noah
            }],
            "no",
            ADMIN,
            t0()
        )
        .await,
        Err(DbError::OwnerMismatch)
    ));

    let outcome =
        f.db.revoke_requests(
            &[
                DecisionTarget {
                    request_id: mia,
                    member_id: f.mia,
                },
                DecisionTarget {
                    request_id: seasons.request_id,
                    member_id: f.mia,
                },
            ],
            "not on this server",
            ADMIN,
            t0(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.revoked, vec![mia, seasons.request_id]);
    // Noah still wants the movie: no stop.
    assert!(!outcome.stop);
    let request = f.db.get_request(mia).await.unwrap().unwrap();
    assert_eq!(request.state, RequestState::Revoked);
    assert_eq!(request.revoke_reason.as_deref(), Some("not on this server"));
    assert_eq!(request.items[0].decision, Decision::Revoked);
    assert_eq!(f.state(seasons.request_id).await, RequestState::Revoked);
    assert_eq!(f.state(noah).await, RequestState::InProgress);

    let notes =
        f.db.list_notifications(crate::notifications::Stream::Member(f.mia), None, 50)
            .await
            .unwrap();
    let revoked: Vec<_> = notes
        .iter()
        .filter(|n| n.kind == starr_core::notifications::NotificationKind::RequestRevoked)
        .collect();
    assert_eq!(revoked.len(), 2);
    assert_eq!(
        revoked[0].payload.reason.as_deref(),
        Some("not on this server")
    );

    // Revoking again finds nothing open.
    assert_eq!(f.revoke(mia, f.mia, "again").await.skipped, vec![mia]);

    // Noah's revoke leaves the download alone but stops the searching.
    let outcome = f.revoke(noah, f.noah, "duplicate").await;
    assert!(outcome.stop);
    let acquisition = f.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Downloading);
    assert!(acquisition.unwanted && acquisition.stop_pending);
    // After the stop the download runs on, unwanted, until it goes away.
    f.db.record_stopped(acquisition.id, t0()).await.unwrap();
    let acquisition = f.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Downloading);
    assert!(acquisition.unwanted && !acquisition.stop_pending);
    let change =
        f.db.record_observed(
            acquisition.id,
            &observed(AcquisitionState::Submitted),
            SYSTEM,
            t0(),
        )
        .await
        .unwrap();
    assert_eq!(
        change,
        Some((AcquisitionState::Downloading, AcquisitionState::Cancelled))
    );
}

#[tokio::test]
async fn after_a_revoke_the_next_request_waits_for_approval() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    f.revoke(mia.request_id, f.mia, "no").await;
    let again = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    assert_eq!(again.state, RequestState::PendingApproval);
    // Noah was not revoked: his mode applies.
    let noah = f
        .request_movie(f.noah, RequestMode::Auto, NOAH)
        .await
        .unwrap();
    assert_eq!(noah.state, RequestState::Approved);
}

#[tokio::test]
async fn after_an_own_cancel_the_normal_mode_applies_again() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    f.cancel(f.mia, mia.request_id).await;
    let again = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    assert_eq!(again.state, RequestState::Approved);
    assert!(again.submit);
    // The cancelled acquisition starts over.
    let acquisition = f.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Pending);
    assert_eq!(acquisition.requests, 1);
}

#[tokio::test]
async fn attaching_before_the_stop_went_out_takes_the_stop_back() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    f.submit(STARR_MONITORED).await;
    f.cancel(f.mia, mia.request_id).await;
    assert!(f.acquisition().await.stop_pending);
    let noah = f
        .request_movie(f.noah, RequestMode::Auto, NOAH)
        .await
        .unwrap();
    // Nobody wanted it, so Noah's mode decides (auto); it joins the
    // running acquisition and nothing is stopped now.
    assert_eq!(noah.state, RequestState::Submitted);
    let acquisition = f.acquisition().await;
    assert!(!acquisition.stop_pending && !acquisition.unwanted);
    assert_eq!(f.db.due_stops(t0()).await.unwrap().len(), 0);
}

#[tokio::test]
async fn an_approval_racing_the_stop_brings_the_monitoring_back() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.submit(STARR_MONITORED).await;
    f.cancel(f.mia, mia.request_id).await;
    // The stop is on its way when Noah attaches.
    f.request_movie(f.noah, RequestMode::Auto, NOAH)
        .await
        .unwrap();
    assert!(f.db.record_stopped(id, t0()).await.unwrap());
    let acquisition = f.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Pending);
    assert_eq!(f.db.due_submissions(t0()).await.unwrap().len(), 1);
}

#[tokio::test]
async fn paused_writes_hold_stops_too() {
    let f = fixture().await;
    let mia = f
        .request_seasons(f.mia, RequestMode::Auto, &[1, 2])
        .await
        .unwrap();
    for id in f.acquisitions().await.into_iter().map(|a| a.id) {
        f.db.record_submitted(id, Some("1"), STARR_MONITORED, SYSTEM, t0())
            .await
            .unwrap();
    }
    f.db.set_writes_paused(f.sonarr, Some("root"), t0())
        .await
        .unwrap();
    f.cancel(f.mia, mia.request_id).await;
    assert_eq!(f.db.due_stops(t0()).await.unwrap().len(), 0);
    assert!(!f.db.has_due_submissions(t0()).await.unwrap());
    let held = f.db.held_at(f.sonarr, 5).await.unwrap();
    assert_eq!((held.total, held.stops), (0, 2));
    assert_eq!(held.stop_preview[0].title, "Game of Thrones");
    f.db.set_writes_paused(f.sonarr, None, t0()).await.unwrap();
    assert_eq!(f.db.due_stops(t0()).await.unwrap().len(), 2);
    assert_eq!(f.db.held_at(f.sonarr, 5).await.unwrap().stops, 0);
}

#[tokio::test]
async fn a_failed_stop_retries_or_gives_up_readably() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.submit(STARR_MONITORED).await;
    f.cancel(f.mia, mia.request_id).await;
    let later = t0() + TimeDelta::minutes(15);
    f.db.record_stop_failure(id, "transient", "timeout", Some(later), t0())
        .await
        .unwrap();
    assert_eq!(f.db.due_stops(t0()).await.unwrap().len(), 0);
    assert_eq!(f.db.due_stops(later).await.unwrap().len(), 1);
    f.db.record_stop_failure(id, "rejected", "no", None, t0())
        .await
        .unwrap();
    let acquisition = f.acquisition().await;
    assert!(!acquisition.stop_pending);
    assert_eq!(acquisition.last_error.as_deref(), Some("no"));
}

#[tokio::test]
async fn only_finished_requests_can_be_hidden_and_only_from_their_member() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = mia.request_id;
    assert_eq!(
        f.db.hide_request(f.mia, id, true, t0()).await.unwrap(),
        HideOutcome::NotFinished
    );
    f.cancel(f.mia, id).await;
    assert_eq!(
        f.db.hide_request(f.noah, id, true, t0()).await.unwrap(),
        HideOutcome::NotFound
    );
    assert_eq!(
        f.db.hide_request(f.mia, id, true, t0()).await.unwrap(),
        HideOutcome::Done
    );
    let own = |without_hidden| RequestFilter {
        member_id: Some(f.mia),
        without_hidden,
        ..RequestFilter::default()
    };
    assert_eq!(f.db.list_requests(own(true)).await.unwrap().len(), 0);
    // Still there for "show hidden" and for admins.
    assert_eq!(f.db.list_requests(own(false)).await.unwrap().len(), 1);
    let all = f.db.list_requests(RequestFilter::default()).await.unwrap();
    assert_eq!(all[0].hidden_at, Some(t0()));
    f.db.hide_request(f.mia, id, false, t0()).await.unwrap();
    assert_eq!(f.db.list_requests(own(true)).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_title_gone_from_the_library_is_removed_silently_and_new_again() {
    let f = fixture().await;
    let (mia, noah) = f.shared_movie().await;
    let id = f.submit(STARR_MONITORED).await;
    let available = Observed {
        state: AcquisitionState::Available,
        progress: Some(1.0),
        message: None,
        stage: None,
    };
    f.db.record_observed(id, &available, SYSTEM, t0() - TimeDelta::days(1))
        .await
        .unwrap();
    assert_eq!(f.db.removal_candidates(t0()).await.unwrap().len(), 0);
    f.movie_gone_from_jellyfin().await;
    let candidates = f.db.removal_candidates(t0()).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].acquisition_id, id);
    assert!(f.db.has_removal_candidates(t0()).await.unwrap());

    let before =
        f.db.list_notifications(crate::notifications::Stream::Member(f.mia), None, 50)
            .await
            .unwrap()
            .len();
    let sync = Actor {
        trigger: EventTrigger::System,
        name: "acquisitions.sync",
    };
    assert!(f.db.record_removed(id, sync, t0()).await.unwrap());
    assert!(!f.db.record_removed(id, sync, t0()).await.unwrap());
    for request in [mia, noah] {
        assert_eq!(f.state(request).await, RequestState::Removed);
    }
    assert_eq!(f.acquisition().await.state, AcquisitionState::Cancelled);
    // Silent: no notification for the members.
    assert_eq!(
        f.db.list_notifications(crate::notifications::Stream::Member(f.mia), None, 50)
            .await
            .unwrap()
            .len(),
        before
    );
    let facts = f.db.unit_facts(f.movie, f.mia).await.unwrap();
    assert_eq!(facts[0].acquisition, Some(AcquisitionState::Cancelled));
    assert!(!facts[0].open_requests);
    assert_eq!(f.db.removal_candidates(t0()).await.unwrap().len(), 0);

    // Requestable again with the normal mode.
    let again = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    assert_eq!(again.state, RequestState::Approved);
    assert_eq!(f.acquisition().await.state, AcquisitionState::Pending);
}

#[tokio::test]
async fn a_write_that_lands_after_the_cancel_queues_a_stop() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.acquisition().await.id;
    assert!(f.db.submission_pending(id).await.unwrap());
    // The worker took it; the member cancels while the write is on its way.
    f.cancel(f.mia, mia.request_id).await;
    assert!(!f.db.submission_pending(id).await.unwrap());
    assert!(
        !f.db
            .record_submitted(id, Some("1"), STARR_MONITORED, SYSTEM, t0())
            .await
            .unwrap()
    );
    let acquisition = f.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Submitted);
    assert!(acquisition.stop_pending);
    assert_eq!(f.db.due_stops(t0()).await.unwrap().len(), 1);

    // Starr changed nothing there: it stays cancelled.
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.acquisition().await.id;
    f.cancel(f.mia, mia.request_id).await;
    f.db.record_submitted(id, Some("1"), SourceChange::default(), SYSTEM, t0())
        .await
        .unwrap();
    assert_eq!(f.acquisition().await.state, AcquisitionState::Cancelled);
}

#[tokio::test]
async fn a_pending_stop_is_dropped_once_the_files_are_there() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.submit(STARR_MONITORED).await;
    f.db.record_observed(id, &observed(AcquisitionState::Downloading), SYSTEM, t0())
        .await
        .unwrap();
    f.db.set_writes_paused(f.radarr, Some("root"), t0())
        .await
        .unwrap();
    f.revoke(mia.request_id, f.mia, "no").await;
    assert!(f.acquisition().await.stop_pending);
    let available = Observed {
        state: AcquisitionState::Available,
        progress: Some(1.0),
        message: None,
        stage: None,
    };
    f.db.record_observed(id, &available, SYSTEM, t0())
        .await
        .unwrap();
    f.db.set_writes_paused(f.radarr, None, t0()).await.unwrap();
    assert!(!f.acquisition().await.stop_pending);
    assert_eq!(f.db.due_stops(t0()).await.unwrap().len(), 0);
    assert!(!f.db.stop_pending(id, t0()).await.unwrap());
}

#[tokio::test]
async fn a_stop_on_an_acquisition_that_failed_meanwhile_keeps_the_reason() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.submit(STARR_MONITORED).await;
    f.cancel(f.mia, mia.request_id).await;
    sqlx::query("UPDATE acquisition SET state = 'failed', last_error = 'source removed'")
        .execute(f.db.pool())
        .await
        .unwrap();
    f.db.record_stopped(id, t0()).await.unwrap();
    let acquisition = f.acquisition().await;
    assert_eq!(acquisition.state, AcquisitionState::Failed);
    assert_eq!(acquisition.last_error.as_deref(), Some("source removed"));
}

#[tokio::test]
async fn an_unwanted_running_acquisition_does_not_approve_new_requests() {
    let f = fixture().await;
    let mia = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    // Monitored before Starr: revoking keeps the download, unwanted.
    let id = f.submit(SourceChange::default()).await;
    f.db.record_observed(id, &observed(AcquisitionState::Downloading), SYSTEM, t0())
        .await
        .unwrap();
    f.revoke(mia.request_id, f.mia, "no").await;
    assert!(f.acquisition().await.unwanted);
    let noah = f
        .request_movie(f.noah, RequestMode::Approval, NOAH)
        .await
        .unwrap();
    assert_eq!(noah.state, RequestState::PendingApproval);
}

#[tokio::test]
async fn a_title_the_source_still_has_files_for_is_looked_at_again_later() {
    let f = fixture().await;
    f.request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    let id = f.submit(STARR_MONITORED).await;
    let available = Observed {
        state: AcquisitionState::Available,
        progress: Some(1.0),
        message: None,
        stage: None,
    };
    f.db.record_observed(id, &available, SYSTEM, t0() - TimeDelta::days(1))
        .await
        .unwrap();
    f.movie_gone_from_jellyfin().await;
    assert_eq!(f.db.removal_candidates(t0()).await.unwrap().len(), 1);
    f.db.record_removal_kept(id, t0()).await.unwrap();
    assert!(!f.db.has_removal_candidates(t0()).await.unwrap());
    let later = t0() + REMOVAL_RECHECK;
    assert_eq!(f.db.removal_candidates(later).await.unwrap().len(), 1);
}

#[tokio::test]
async fn after_removal_a_revoked_member_starts_fresh() {
    let f = fixture().await;
    let (mia, _) = f.shared_movie().await;
    let id = f.submit(STARR_MONITORED).await;
    f.revoke(mia, f.mia, "no").await;
    let later = t0() + TimeDelta::hours(1);
    let available = Observed {
        state: AcquisitionState::Available,
        progress: Some(1.0),
        message: None,
        stage: None,
    };
    f.db.record_observed(id, &available, SYSTEM, t0())
        .await
        .unwrap();
    f.db.record_removed(id, SYSTEM, later).await.unwrap();
    let again = f
        .request_movie(f.mia, RequestMode::Auto, MIA)
        .await
        .unwrap();
    assert_eq!(again.state, RequestState::Approved);
}
