#![allow(clippy::unwrap_used)]

use chrono::TimeDelta;
use serde_json::json;
use starr_core::categories::MinimumAvailability;
use starr_core::members::{Category, Entitlement, RequestMode};

use super::*;
use crate::members::NewMember;
use crate::requests::{
    Actor, DecisionTarget, EventTrigger, NewRequest, RequestFollow, SourceChange, SourceTarget,
    Unit,
};
use crate::services::NewServiceInstance;
use crate::test_support::t0;

const MEMBER: Actor<'static> = Actor {
    trigger: EventTrigger::Member,
    name: "mia",
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
    source: SourceTarget,
    movie: i64,
}

fn sealed() -> StoredCredential {
    StoredCredential {
        ciphertext: b"c".to_vec(),
        recipient: "age1x".to_owned(),
        class: SecretClass::ServiceCredential,
    }
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

async fn fixture() -> Fixture {
    let db = Database::open_in_memory().await.unwrap();
    let mia = member(&db, "mia").await;
    let noah = member(&db, "noah").await;
    let radarr = db
        .create_service(
            &NewServiceInstance {
                adapter_type: "radarr",
                name: "radarr",
                base_url: "http://x",
                public_url: None,
                credential: &sealed(),
            },
            t0(),
        )
        .await
        .unwrap()
        .id;
    let (movie,): (i64,) = sqlx::query_as(
        "INSERT INTO catalog_item (kind, tmdb_id, title, year, created_at, updated_at)
         VALUES ('movie', 603, 'The Matrix', 1999, ?1, ?1) RETURNING id",
    )
    .bind(ts(t0()))
    .fetch_one(db.pool())
    .await
    .unwrap();
    Fixture {
        db,
        mia,
        noah,
        source: SourceTarget {
            category: Category::Movies,
            service_instance_id: radarr,
            quality_profile_id: 4,
            root_folder_path: "/movies".into(),
            minimum_availability: Some(MinimumAvailability::Released),
            metadata_profile_id: None,
        },
        movie,
    }
}

impl Fixture {
    async fn request(&self, member: i64, mode: RequestMode) -> i64 {
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
                    source: &self.source,
                },
                MEMBER,
                t0(),
            )
            .await
            .unwrap()
            .request_id
    }

    async fn approve(&self, request_id: i64, member_id: i64) {
        self.db
            .approve_requests(
                &[DecisionTarget {
                    request_id,
                    member_id,
                }],
                std::slice::from_ref(&self.source),
                ADMIN,
                t0(),
            )
            .await
            .unwrap();
    }

    async fn kinds(&self, stream: Stream) -> Vec<NotificationKind> {
        self.db
            .list_notifications(stream, None, 50)
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.kind)
            .collect()
    }

    async fn acquisition(&self) -> i64 {
        let (id,): (i64,) = sqlx::query_as("SELECT id FROM acquisition")
            .fetch_one(self.db.pool())
            .await
            .unwrap();
        id
    }

    async fn channel(&self, kind: ChannelKind, name: &str, settings: Value) -> i64 {
        let secret = sealed();
        self.db
            .create_channel(
                &NewChannel {
                    kind,
                    name,
                    enabled: true,
                    settings: &settings,
                    secret: Some(&secret),
                    admin_events: &NotificationKind::ADMIN,
                },
                t0(),
            )
            .await
            .unwrap()
            .id
    }

    async fn deliveries(&self) -> Vec<(i64, String)> {
        sqlx::query_as("SELECT channel_id, state FROM notification_delivery ORDER BY id")
            .fetch_all(self.db.pool())
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn pending_request_notifies_admins_and_decisions_notify_only_the_owner() {
    let f = fixture().await;
    let id = f.request(f.mia, RequestMode::Approval).await;
    let admin =
        f.db.list_notifications(Stream::Admin, None, 50)
            .await
            .unwrap();
    assert_eq!(admin.len(), 1);
    assert_eq!(admin[0].kind, NotificationKind::ApprovalPending);
    assert_eq!(admin[0].payload.member.as_deref(), Some("mia"));
    assert_eq!(admin[0].payload.title, "The Matrix");
    assert_eq!(admin[0].payload.request_id, Some(id));
    // Creating it is the member's own doing.
    assert_eq!(f.kinds(Stream::Member(f.mia)).await, vec![]);

    f.approve(id, f.mia).await;
    let mine =
        f.db.list_notifications(Stream::Member(f.mia), None, 50)
            .await
            .unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].kind, NotificationKind::RequestApproved);
    // A member's payload never names a member.
    assert_eq!(mine[0].payload.member, None);
    assert_eq!(f.kinds(Stream::Member(f.noah)).await, vec![]);
    assert_eq!(
        f.db.unread_notifications(Stream::Member(f.mia))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        f.db.unread_notifications(Stream::Member(f.noah))
            .await
            .unwrap(),
        0
    );

    // Noah cannot mark Mia's notification read, not even by id.
    let changed =
        f.db.mark_notifications_read(Stream::Member(f.noah), Some(&[mine[0].id]), t0())
            .await
            .unwrap();
    assert_eq!(changed, 0);
    let changed =
        f.db.mark_notifications_read(Stream::Member(f.mia), None, t0())
            .await
            .unwrap();
    assert_eq!(changed, 1);
    assert_eq!(
        f.db.unread_notifications(Stream::Member(f.mia))
            .await
            .unwrap(),
        0
    );
    // The admin stream is untouched by members.
    assert_eq!(f.db.unread_notifications(Stream::Admin).await.unwrap(), 1);
}

#[tokio::test]
async fn automatic_and_attached_approvals_are_silent() {
    let f = fixture().await;
    f.request(f.noah, RequestMode::Auto).await;
    // Mia attaches to Noah's running acquisition: approved right away.
    f.request(f.mia, RequestMode::Approval).await;
    assert_eq!(f.kinds(Stream::Admin).await, vec![]);
    assert_eq!(f.kinds(Stream::Member(f.noah)).await, vec![]);
    assert_eq!(f.kinds(Stream::Member(f.mia)).await, vec![]);
}

#[tokio::test]
async fn rejection_reason_reaches_the_member_once() {
    let f = fixture().await;
    let id = f.request(f.mia, RequestMode::Approval).await;
    f.db.reject_requests(
        &[DecisionTarget {
            request_id: id,
            member_id: f.mia,
        }],
        "not in 4K",
        ADMIN,
        t0(),
    )
    .await
    .unwrap();
    let mine =
        f.db.list_notifications(Stream::Member(f.mia), None, 50)
            .await
            .unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].kind, NotificationKind::RequestRejected);
    assert_eq!(mine[0].payload.reason.as_deref(), Some("not in 4K"));

    // The same transition reported again (a retried transaction, a state
    // flapping back) does not notify twice.
    let mut conn = f.db.pool().acquire().await.unwrap();
    request_transition(
        &mut conn,
        id,
        Some(RequestState::PendingApproval),
        RequestState::Rejected,
        t0(),
    )
    .await
    .unwrap();
    drop(conn);
    assert_eq!(f.kinds(Stream::Member(f.mia)).await.len(), 1);
}

#[tokio::test]
async fn failed_acquisition_notifies_admins_once_and_every_member() {
    let f = fixture().await;
    let mia = f.request(f.mia, RequestMode::Auto).await;
    let noah = f.request(f.noah, RequestMode::Auto).await;
    let acq = f.acquisition().await;
    f.db.record_submit_failure(
        acq,
        "rejected",
        "quality profile missing",
        None,
        SYSTEM,
        t0(),
    )
    .await
    .unwrap();
    let admin =
        f.db.list_notifications(Stream::Admin, None, 50)
            .await
            .unwrap();
    assert_eq!(admin.len(), 1);
    assert_eq!(admin[0].kind, NotificationKind::AcquisitionFailed);
    assert_eq!(admin[0].payload.acquisition_id, Some(acq));
    assert_eq!(
        admin[0].payload.reason.as_deref(),
        Some("quality profile missing")
    );
    for (member, request) in [(f.mia, mia), (f.noah, noah)] {
        let mine =
            f.db.list_notifications(Stream::Member(member), None, 50)
                .await
                .unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].kind, NotificationKind::RequestFailed);
        assert_eq!(mine[0].payload.request_id, Some(request));
        // Technical reasons stay with the admins.
        assert_eq!(mine[0].payload.reason, None);
    }
}

#[tokio::test]
async fn a_retried_submission_failure_does_not_notify() {
    let f = fixture().await;
    f.request(f.mia, RequestMode::Auto).await;
    let acq = f.acquisition().await;
    let retry = Some(t0() + TimeDelta::minutes(5));
    f.db.record_submit_failure(acq, "transient", "timeout", retry, SYSTEM, t0())
        .await
        .unwrap();
    assert_eq!(f.kinds(Stream::Admin).await, vec![]);
    assert_eq!(f.kinds(Stream::Member(f.mia)).await, vec![]);
}

#[tokio::test]
async fn every_member_kind_can_be_opted_in_by_email() {
    let f = fixture().await;
    f.db.set_email_preferences(f.mia, &NotificationKind::MEMBER)
        .await
        .unwrap();
    let mut got = f.db.email_preferences(f.mia).await.unwrap();
    got.sort();
    let mut want = NotificationKind::MEMBER.to_vec();
    want.sort();
    assert_eq!(got, want);
}

#[tokio::test]
async fn an_account_waiting_for_the_password_notifies_once_per_generation() {
    let f = fixture().await;
    assert!(
        f.db.notify_account_activate(f.mia, 3, "Jellyfin", 0, t0())
            .await
            .unwrap()
    );
    assert!(
        !f.db
            .notify_account_activate(f.mia, 3, "Jellyfin", 0, t0())
            .await
            .unwrap()
    );
    assert!(
        f.db.notify_account_activate(f.mia, 3, "Jellyfin", 1, t0())
            .await
            .unwrap()
    );
    assert_eq!(
        f.kinds(Stream::Member(f.mia)).await,
        vec![
            NotificationKind::AccountActivate,
            NotificationKind::AccountActivate
        ]
    );
    assert_eq!(f.kinds(Stream::Member(f.noah)).await, vec![]);
}

#[tokio::test]
async fn deliveries_follow_channels_and_opt_in() {
    let f = fixture().await;
    let email = f
        .channel(ChannelKind::Email, "mail", json!({ "host": "smtp" }))
        .await;
    let discord = f.channel(ChannelKind::Discord, "ops", json!({})).await;
    // Mia opted in for approvals by email; Noah did not.
    f.db.set_email_preferences(f.mia, &[NotificationKind::RequestApproved])
        .await
        .unwrap();
    assert_eq!(
        f.db.email_preferences(f.mia).await.unwrap(),
        vec![NotificationKind::RequestApproved]
    );
    let mia = f.request(f.mia, RequestMode::Approval).await;
    // Admin stream: Discord takes it; email has no admin address.
    assert_eq!(f.deliveries().await, vec![(discord, "pending".to_owned())]);
    f.approve(mia, f.mia).await;
    assert_eq!(f.deliveries().await.len(), 2);
    assert_eq!(f.deliveries().await[1], (email, "pending".to_owned()));

    let due = f.db.due_deliveries(t0(), 10).await.unwrap();
    assert_eq!(due.len(), 2);
    let to_mia = due.iter().find(|d| d.channel.id == email).unwrap();
    assert_eq!(to_mia.member_email.as_deref(), Some("mia@example.com"));
    assert_eq!(to_mia.notification.kind, NotificationKind::RequestApproved);
    assert!(to_mia.channel.secret.is_some());

    // A transient failure retries later; delivered is final.
    let later = t0() + TimeDelta::minutes(1);
    f.db.record_delivery_failure(to_mia.id, "transient", "timeout", Some(later), t0())
        .await
        .unwrap();
    assert_eq!(f.db.due_deliveries(t0(), 10).await.unwrap().len(), 1);
    assert!(f.db.has_due_deliveries(later).await.unwrap());
    f.db.record_delivered(to_mia.id, later).await.unwrap();
    let to_ops = due.iter().find(|d| d.channel.id == discord).unwrap();
    f.db.record_delivery_failure(to_ops.id, "rejected", "HTTP 404", None, t0())
        .await
        .unwrap();
    assert!(!f.db.has_due_deliveries(later).await.unwrap());
    let states: Vec<String> = f.deliveries().await.into_iter().map(|(_, s)| s).collect();
    assert_eq!(states, vec!["failed", "delivered"]);
}

#[tokio::test]
async fn disabling_a_channel_drops_its_pending_deliveries() {
    let f = fixture().await;
    let discord = f.channel(ChannelKind::Discord, "ops", json!({})).await;
    f.request(f.mia, RequestMode::Approval).await;
    f.db.update_channel(
        discord,
        &ChannelUpdate {
            enabled: Some(false),
            ..ChannelUpdate::default()
        },
        t0(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(f.deliveries().await, vec![(discord, "failed".to_owned())]);
    assert!(!f.db.has_due_deliveries(t0()).await.unwrap());
}

#[tokio::test]
async fn channels_have_unique_names_and_one_email_server() {
    let f = fixture().await;
    let email = f.channel(ChannelKind::Email, "mail", json!({})).await;
    let secret = sealed();
    let again =
        f.db.create_channel(
            &NewChannel {
                kind: ChannelKind::Email,
                name: "mail2",
                enabled: true,
                settings: &json!({}),
                secret: None,
                admin_events: &[],
            },
            t0(),
        )
        .await;
    assert!(matches!(again, Err(DbError::ChannelTaken)));
    let dup =
        f.db.create_channel(
            &NewChannel {
                kind: ChannelKind::Webhook,
                name: "MAIL",
                enabled: true,
                settings: &json!({}),
                secret: Some(&secret),
                admin_events: &[],
            },
            t0(),
        )
        .await;
    assert!(matches!(dup, Err(DbError::ChannelTaken)));

    // The password can be removed (SMTP without login); member kinds are
    // never admin events.
    let updated =
        f.db.update_channel(
            email,
            &ChannelUpdate {
                secret: Some(None),
                admin_events: Some(&[
                    NotificationKind::RequestApproved,
                    NotificationKind::AcquisitionFailed,
                ]),
                ..ChannelUpdate::default()
            },
            t0(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!updated.has_secret);
    assert_eq!(
        updated.admin_events,
        vec![NotificationKind::AcquisitionFailed]
    );
    assert!(f.db.email_channel_enabled().await.unwrap());
    assert!(f.db.delete_channel(email).await.unwrap());
    assert!(!f.db.email_channel_enabled().await.unwrap());
    assert!(
        f.db.update_channel(email, &ChannelUpdate::default(), t0())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn admin_email_needs_an_address_and_old_notifications_go() {
    let f = fixture().await;
    let email = f
        .channel(
            ChannelKind::Email,
            "mail",
            json!({ "adminAddress": "ops@example.com" }),
        )
        .await;
    f.request(f.mia, RequestMode::Approval).await;
    assert_eq!(f.deliveries().await, vec![(email, "pending".to_owned())]);
    let due = f.db.due_deliveries(t0(), 10).await.unwrap();
    assert_eq!(due[0].member_email, None);
    f.db.record_channel_result(email, Some(("transient", "timeout")), t0())
        .await
        .unwrap();
    let channel = f.db.get_channel(email).await.unwrap().unwrap();
    assert_eq!(channel.last_ok, Some(false));
    assert_eq!(channel.last_error.as_deref(), Some("timeout"));

    let deleted =
        f.db.delete_notifications_before(t0() + TimeDelta::days(1))
            .await
            .unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(f.deliveries().await, vec![]);
}
#[tokio::test]
async fn removing_a_source_reports_only_newly_failed_acquisitions() {
    let f = fixture().await;
    let (other,): (i64,) = sqlx::query_as(
        "INSERT INTO catalog_item (kind, tmdb_id, title, year, created_at, updated_at)
         VALUES ('movie', 604, 'The Matrix Reloaded', 2003, ?1, ?1) RETURNING id",
    )
    .bind(ts(t0()))
    .fetch_one(f.db.pool())
    .await
    .unwrap();
    f.request(f.mia, RequestMode::Auto).await;
    f.db.create_request(
        &NewRequest {
            member_id: f.noah,
            catalog_item_id: other,
            follow: RequestFollow::No,
            units: &[Unit {
                catalog_item_id: other,
                season_number: None,
            }],
            mode: RequestMode::Auto,
            source: &f.source,
        },
        MEMBER,
        t0(),
    )
    .await
    .unwrap();
    let (failed,): (i64,) = sqlx::query_as("SELECT id FROM acquisition WHERE catalog_item_id = ?")
        .bind(other)
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    f.db.record_submit_failure(failed, "rejected", "no root folder", None, SYSTEM, t0())
        .await
        .unwrap();
    assert_eq!(
        f.kinds(Stream::Admin).await,
        vec![NotificationKind::AcquisitionFailed]
    );

    assert!(
        f.db.delete_service(f.source.service_instance_id, t0())
            .await
            .unwrap()
    );
    // The pending one fails now and is reported; the one that failed
    // before was reported then and is not again.
    let admin =
        f.db.list_notifications(Stream::Admin, None, 50)
            .await
            .unwrap();
    assert_eq!(admin.len(), 2);
    assert_eq!(admin[0].payload.title, "The Matrix");
    assert!(
        admin[0]
            .payload
            .reason
            .as_deref()
            .unwrap()
            .contains("was removed")
    );
}

#[tokio::test]
async fn suspended_members_get_no_email_planned() {
    let f = fixture().await;
    f.channel(ChannelKind::Email, "mail", json!({})).await;
    f.db.set_email_preferences(f.mia, &[NotificationKind::RequestApproved])
        .await
        .unwrap();
    let id = f.request(f.mia, RequestMode::Approval).await;
    assert!(f.db.suspend_member(f.mia).await.unwrap());
    f.approve(id, f.mia).await;
    // In-app still records it; no email delivery that could only fail.
    assert_eq!(
        f.kinds(Stream::Member(f.mia)).await,
        vec![NotificationKind::RequestApproved]
    );
    assert_eq!(f.deliveries().await, vec![]);
}
#[tokio::test]
async fn a_season_found_by_a_follow_tells_the_member_once() {
    let f = fixture().await;
    let (series,): (i64,) = sqlx::query_as(
        "INSERT INTO catalog_item (kind, tmdb_id, tvdb_id, title, year, created_at, updated_at)
         VALUES ('series', 1399, 121361, 'Dark', 2017, ?1, ?1) RETURNING id",
    )
    .bind(ts(t0()))
    .fetch_one(f.db.pool())
    .await
    .unwrap();
    let (season,): (i64,) = sqlx::query_as(
        "INSERT INTO catalog_item (kind, parent_id, season_number, title, created_at, updated_at)
         VALUES ('season', ?1, 3, 'Season 3', ?2, ?2) RETURNING id",
    )
    .bind(series)
    .bind(ts(t0()))
    .fetch_one(f.db.pool())
    .await
    .unwrap();
    let (follow, _) =
        f.db.follow(
            f.mia,
            series,
            Category::Series,
            crate::follows::Cutoff::Seasons(2),
            t0(),
        )
        .await
        .unwrap();
    let source = SourceTarget {
        category: Category::Series,
        minimum_availability: None,
        metadata_profile_id: None,
        ..f.source.clone()
    };
    let email = f.channel(ChannelKind::Email, "mail", json!({})).await;
    f.db.set_email_preferences(f.mia, &[NotificationKind::FollowRequested])
        .await
        .unwrap();
    let created =
        f.db.create_request(
            &NewRequest {
                member_id: f.mia,
                catalog_item_id: series,
                follow: RequestFollow::Found {
                    follow_id: follow.id,
                    cutoff: crate::follows::Cutoff::Seasons(3),
                },
                units: &[Unit {
                    catalog_item_id: season,
                    season_number: Some(3),
                }],
                mode: RequestMode::Auto,
                source: &source,
            },
            SYSTEM,
            t0(),
        )
        .await
        .unwrap();

    let mine =
        f.db.list_notifications(Stream::Member(f.mia), None, 50)
            .await
            .unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].kind, NotificationKind::FollowRequested);
    assert_eq!(mine[0].payload.title, "Dark");
    assert_eq!(mine[0].payload.seasons, vec![3]);
    assert_eq!(mine[0].payload.request_id, Some(created.request_id));
    // Opted in: also by email.
    assert_eq!(f.deliveries().await, vec![(email, "pending".to_owned())]);
    // Reported again (a retried transaction): still one.
    let mut conn = f.db.pool().acquire().await.unwrap();
    found_by_follow(&mut conn, created.request_id, t0())
        .await
        .unwrap();
    drop(conn);
    assert_eq!(f.kinds(Stream::Member(f.mia)).await.len(), 1);
    // A request the member made by hand says nothing.
    f.request(f.noah, RequestMode::Auto).await;
    assert_eq!(f.kinds(Stream::Member(f.noah)).await, vec![]);
}

#[tokio::test]
async fn a_blocked_import_notifies_admins_once_per_acquisition() {
    use starr_core::requests::{AcquisitionState, Observed};

    let f = fixture().await;
    let all = f
        .channel(ChannelKind::Webhook, "everything", json!({}))
        .await;
    let quiet = f.channel(ChannelKind::Discord, "quiet", json!({})).await;
    f.db.update_channel(
        quiet,
        &ChannelUpdate {
            admin_events: Some(&[NotificationKind::AcquisitionFailed]),
            ..ChannelUpdate::default()
        },
        t0(),
    )
    .await
    .unwrap();
    f.request(f.mia, RequestMode::Auto).await;
    let acq = f.acquisition().await;
    f.db.record_submitted(acq, Some("1"), SourceChange::default(), SYSTEM, t0())
        .await
        .unwrap();
    let observed = |progress, message: Option<&str>| Observed {
        state: AcquisitionState::Downloading,
        progress: Some(progress),
        message: message.map(str::to_owned),
        stage: None,
    };
    // Still loading with a warning: nothing yet.
    f.db.record_observed(acq, &observed(0.5, Some("Stalled")), SYSTEM, t0())
        .await
        .unwrap();
    assert_eq!(f.kinds(Stream::Admin).await, vec![]);

    let blocked = observed(1.0, Some("No files found are eligible for import"));
    for _ in 0..3 {
        f.db.record_observed(acq, &blocked, SYSTEM, t0())
            .await
            .unwrap();
    }
    let admin =
        f.db.list_notifications(Stream::Admin, None, 50)
            .await
            .unwrap();
    assert_eq!(admin.len(), 1, "once per acquisition");
    assert_eq!(admin[0].kind, NotificationKind::ImportBlocked);
    assert_eq!(admin[0].payload.acquisition_id, Some(acq));
    assert_eq!(admin[0].payload.title, "The Matrix");
    assert_eq!(
        admin[0].payload.reason.as_deref(),
        Some("No files found are eligible for import")
    );
    // Only the channel that chose the event carries it; members hear nothing.
    assert_eq!(f.deliveries().await, vec![(all, "pending".to_owned())]);
    assert_eq!(f.kinds(Stream::Member(f.mia)).await, vec![]);
}

/// Spec 0007 Q13: the private category's notifications (member and admin
/// stream, read or not) go after their own retention; others stay.
#[tokio::test]
async fn a_categorys_old_notifications_go_on_their_own() {
    let f = fixture().await;
    let email = f
        .channel(
            ChannelKind::Email,
            "mail",
            json!({ "adminAddress": "ops@example.com" }),
        )
        .await;
    let insert = |member: Option<i64>, kind: &'static str, category: &'static str, days: i64| {
        let db = &f.db;
        async move {
            let (id,): (i64,) = sqlx::query_as(
                "INSERT INTO notification (member_id, kind, payload, dedup_key, created_at, read_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5) RETURNING id",
            )
            .bind(member)
            .bind(kind)
            .bind(json!({ "category": category, "title": "Placeholder" }).to_string())
            .bind(format!("{kind}:{category}:{days}"))
            .bind(ts(t0() + TimeDelta::days(days)))
            .fetch_one(db.pool())
            .await
            .unwrap();
            id
        }
    };
    insert(Some(f.mia), "request_available", "adult", 0).await;
    let old_admin = insert(None, "approval_pending", "adult", 0).await;
    let recent = insert(Some(f.mia), "request_available", "adult", 2).await;
    let movies = insert(Some(f.mia), "request_available", "movies", 0).await;
    sqlx::query(
        "INSERT INTO notification_delivery (notification_id, channel_id, state, created_at, updated_at)
         VALUES (?1, ?2, 'pending', ?3, ?3)",
    )
    .bind(old_admin)
    .bind(email)
    .bind(ts(t0()))
    .execute(f.db.pool())
    .await
    .unwrap();

    let deleted =
        f.db.delete_category_notifications_before(Category::Adult, t0() + TimeDelta::days(1))
            .await
            .unwrap();
    assert_eq!(deleted, 2);
    let left: Vec<(i64,)> = sqlx::query_as("SELECT id FROM notification ORDER BY id")
        .fetch_all(f.db.pool())
        .await
        .unwrap();
    assert_eq!(left, vec![(recent,), (movies,)]);
    // Their deliveries go with them.
    assert_eq!(f.deliveries().await, vec![]);
}

#[test]
fn music_titles_name_artist_and_albums() {
    assert_eq!(
        album_title("Radiohead", &[("OK Computer".to_owned(), Some(1997))]),
        ("Radiohead – OK Computer".to_owned(), Some(1997))
    );
    assert_eq!(
        album_title(
            "Radiohead",
            &[
                ("OK Computer".to_owned(), Some(1997)),
                ("Kid A".to_owned(), Some(2000))
            ]
        ),
        ("Radiohead – OK Computer, Kid A".to_owned(), None)
    );
    assert_eq!(
        album_title("Radiohead", &[]),
        ("Radiohead".to_owned(), None)
    );
}
